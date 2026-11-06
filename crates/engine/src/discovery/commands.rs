//! Discovery: the commands that retry, resolve and show the reviews of a pull request.
//!
//! Mirrors `cmd/autoreview/discovery.go:507-537, 581-650`.

// Rust guideline compliant 2026-06-26

use autoreview_schedule::scheduler::Record;
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use super::poll::live;
use crate::engine::{Clock, Engine};
use crate::error::Error;
use crate::integrations::Integrations;
use crate::model::ids::WORKFLOW_ID_PREFIX;
use crate::model::{
    AwaitRecord, Decision, PublishedRecord, ResolveRequest, ResolveResult, RetryRequest,
    RetryResult, head_prefix_valid, repository_valid, sha_valid,
};
use crate::store::{ReviewFilter, StoredReview};

/// Schedule phase of a review identity whose attempt is queued or running.
const PHASE_RUNNING: &str = "running";

/// Error code of a command whose repository, number or head is invalid.
const REVIEW_IDENTITY_INVALID: &str = "review_identity_invalid";

/// Error code of a retry whose poll queued no attempt for the pull request.
const REVIEW_RETRY_NOT_DISPATCHED: &str = "review_retry_not_dispatched";

/// Error code of a resolve that finds no review waiting for approval.
const REVIEW_NOT_AWAITING_APPROVAL: &str = "review_not_awaiting_approval";

/// Everything the engine knows about one pull request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PullRequestReviewStatus {
    /// The `repository#number` key of the pull request.
    pub(crate) key: String,
    /// Schedule record of the pull request, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) schedule: Option<Record>,
    /// Last published review of the pull request, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) published: Option<PublishedRecord>,
    /// Review that waits for approval, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) awaiting: Option<AwaitRecord>,
    /// Stored reviews of the pull request, newest first.
    pub(crate) reviews: Vec<StoredReview>,
}

impl<I: Integrations, C: Clock> Engine<I, C> {
    /// Starts a new attempt of a stopped review through one locked poll.
    ///
    /// The head may be a prefix, as a stop notification shows it. It must match the current
    /// head.
    ///
    /// # Errors
    ///
    /// Returns `review_identity_invalid` for an invalid request, the error of the lock, of the
    /// poll or of the store, and `review_retry_not_dispatched` when no attempt is running.
    pub async fn retry(
        &self,
        cancel: &CancellationToken,
        request: &RetryRequest,
    ) -> Result<RetryResult, Error> {
        if !repository_valid(&request.repository)
            || request.number <= 0
            || !head_prefix_valid(&request.head_sha)
        {
            return Err(Error::workflow(REVIEW_IDENTITY_INVALID, true, None));
        }
        let key = format!("{}#{}", request.repository, request.number);
        let record = self
            .with_discovery(|| async {
                self.poll_pass(cancel, Some(request)).await?;
                live(cancel)?;
                let state = self.store.load_discovery()?;
                Ok(state.schedule.get(&key).cloned())
            })
            .await?;
        let Some(record) = record.filter(|record| record.phase == PHASE_RUNNING) else {
            return Err(Error::workflow(REVIEW_RETRY_NOT_DISPATCHED, true, None));
        };
        // The identity is repository#number@head@request; the head is its second field.
        let parts: Vec<&str> = record.identity.splitn(3, '@').collect();
        let [_, head, _] = parts.as_slice() else {
            return Err(Error::workflow(REVIEW_RETRY_NOT_DISPATCHED, true, None));
        };
        if !sha_valid(head) {
            return Err(Error::workflow(REVIEW_RETRY_NOT_DISPATCHED, true, None));
        }
        Ok(RetryResult {
            ok: true,
            workflow_id: record.workflow_id.clone(),
            head_sha: (*head).to_owned(),
            attempt: record.attempt,
        })
    }

    /// Finds the review that waits for approval at the given head prefix.
    ///
    /// It only reads, so it never waits for a running poll.
    ///
    /// # Errors
    ///
    /// Returns `review_identity_invalid` for an invalid request, the error of the store, and
    /// `review_not_awaiting_approval` when no review matches.
    pub fn resolve(
        &self,
        cancel: &CancellationToken,
        request: &ResolveRequest,
    ) -> Result<ResolveResult, Error> {
        if !repository_valid(&request.repository)
            || request.number <= 0
            || !head_prefix_valid(&request.head_prefix)
        {
            return Err(Error::workflow(REVIEW_IDENTITY_INVALID, true, None));
        }
        live(cancel)?;
        let state = self.store.load_discovery()?;
        let key = format!("{}#{}", request.repository, request.number);
        if let Some(record) = state.awaiting.get(&key)
            && record.head_sha.starts_with(&request.head_prefix)
        {
            return Ok(ResolveResult {
                ok: true,
                workflow_id: record.workflow_id.clone(),
                head_sha: record.head_sha.clone(),
                findings: 0,
            });
        }
        // A repeated approval after publication resolves to the published review. Its
        // approval reports the publication.
        if let Some(record) = state.published.get(&key)
            && record.decision == Decision::Approve
            && record.head_sha.starts_with(&request.head_prefix)
            && record.workflow_id.starts_with(WORKFLOW_ID_PREFIX)
        {
            live(cancel)?;
            if self.store.get_review(&record.workflow_id)?.is_some() {
                return Ok(ResolveResult {
                    ok: true,
                    workflow_id: record.workflow_id.clone(),
                    head_sha: record.head_sha.clone(),
                    findings: 0,
                });
            }
        }
        Err(Error::workflow(REVIEW_NOT_AWAITING_APPROVAL, true, None))
    }

    /// Returns the reviews that wait for an approval, ordered by their pull request key.
    ///
    /// # Errors
    ///
    /// Returns the error of the store.
    pub fn list_awaiting(&self, cancel: &CancellationToken) -> Result<Vec<AwaitRecord>, Error> {
        live(cancel)?;
        let state = self.store.load_discovery()?;
        let mut records: Vec<AwaitRecord> = state.awaiting.into_values().collect();
        records.sort_by_cached_key(AwaitRecord::key);
        Ok(records)
    }

    /// Returns everything the engine knows about one pull request.
    ///
    /// The repository and number are not validated; the caller validates them.
    ///
    /// # Errors
    ///
    /// Returns the error of the store.
    pub fn pull_request_status(
        &self,
        cancel: &CancellationToken,
        repository: &str,
        number: i64,
    ) -> Result<PullRequestReviewStatus, Error> {
        let key = format!("{repository}#{number}");
        live(cancel)?;
        let mut state = self.store.load_discovery()?;
        let schedule = state.schedule.remove(&key);
        let published = state.published.remove(&key);
        let awaiting = state.awaiting.remove(&key);
        live(cancel)?;
        let reviews = self.store.list_reviews(
            &ReviewFilter::ReviewKey(key.clone()),
            self.config.review_list_limit,
        )?;
        Ok(PullRequestReviewStatus {
            key,
            schedule,
            published,
            awaiting,
            reviews,
        })
    }
}
