//! The approval wait of a clean review: it is recorded, released and then decided.
//!
//! Mirrors `cmd/autoreview/review.go:777-856, 893-948`, except that the wait is
//! stored before the approval request is sent (#143).

// Rust guideline compliant 2026-06-26

use autoreview_policy::Event;
use tokio_util::sync::CancellationToken;

use super::input::STEP_PLAN;
use super::sessions::{Review, is_shutdown};
use crate::discovery::poll::live;
use crate::engine::{Clock, Engine, STEP_ARTIFACT, STEP_PUBLICATION};
use crate::error::{Error, error_code};
use crate::integrations::Integrations;
use crate::messages::{approval_expired_message, approval_requested_message, superseded_message};
use crate::model::review::{
    APPROVAL_KIND_APPROVE, APPROVAL_KIND_EXPIRED, APPROVAL_KIND_SUPERSEDED,
};
use crate::model::{
    ApprovalResult, ApproveRequest, Artifact, AwaitRecord, Outcome, PublishResult, ReviewOutcome,
};
use crate::publish::plan::PublicationPlan;
use crate::store::{RESULT_SUCCESS, REVIEW_AWAITING};

/// Nanoseconds in one second, as Go's `time.Second`.
const NANOS_PER_SECOND: i64 = 1_000_000_000;

/// Error code of an approval whose review, artifact or plan is not stored.
const REVIEW_INCOMPLETE: &str = "review_incomplete";

/// Error code of an approval whose identity differs from the stored artifact.
const REVIEW_IDENTITY_INVALID: &str = "review_identity_invalid";

/// Error code of an approval whose plan does not wait for one.
const APPROVAL_NOT_REQUIRED: &str = "approval_not_required";

/// Error code of an approval of a review that does not wait yet.
const REVIEW_NOT_AWAITING_APPROVAL: &str = "review_not_awaiting_approval";

/// Error code of an approval that came after the deadline.
const APPROVAL_EXPIRED: &str = "approval_expired";

/// Error code of an approval of a review that a newer review replaced.
const REVIEW_SUPERSEDED: &str = "review_superseded";

/// Publication status of an approved review the engine has not published yet.
const PUBLICATION_PENDING: &str = "pending";

/// Publication status of an approved review that is already on GitHub.
const PUBLICATION_PUBLISHED: &str = "published";

impl<I: Integrations, C: Clock> Review<'_, I, C> {
    /// Records the wait, releases the pull request and asks the user for an approval.
    ///
    /// The wait is recorded before the release, so discovery never sees the head without a
    /// review it keeps. The review is stored as awaiting before the request is sent, so an
    /// approval that follows the request at once is accepted.
    ///
    /// # Errors
    ///
    /// Returns the shutdown error, or the error of the store or of the discovery record. A
    /// failed approval request is logged and leaves the review waiting; the function then
    /// returns `Ok`.
    pub(crate) async fn await_approval(&mut self, artifact: &Artifact) -> Result<(), Error> {
        let record = self.await_record();
        if let Err(err) = self.engine.mark_awaiting(&self.cancel, &record).await {
            return Err(self.interrupted(err));
        }
        live(&self.cancel)?;
        self.set_released(true)?;
        let nanos = self
            .input
            .approval_timeout_seconds
            .wrapping_mul(NANOS_PER_SECOND);
        let deadline = self
            .engine
            .clock
            .wall_now()
            .saturating_add(time::Duration::nanoseconds(nanos));
        live(&self.cancel)?;
        self.engine
            .store
            .start_awaiting(&self.workflow_id, deadline)?;
        let message = approval_requested_message(
            self.engine.config.excerpt_characters,
            &self.input,
            artifact,
        );
        if let Err(err) = self
            .notify("approval_requested", Event::ApprovalRequested, &message)
            .await
        {
            if is_shutdown(&err) {
                return Err(err);
            }
            // The review waits anyway: `autoreview awaiting` lists it, and the deadline ends it.
            tracing::error!(
                review_id = %self.input.pull_request.review_id(),
                error_code = %error_code(&err),
                "approval request failed"
            );
        }
        Ok(())
    }

    /// Continues a waiting review by the decision `approval` stored for it.
    ///
    /// An approval publishes the review, an expiry or a newer review ends the wait without a
    /// publication, and a wait without a decision is recorded again.
    ///
    /// # Errors
    ///
    /// Returns the shutdown error, the error of the store or of the discovery record, or
    /// `review_input_invalid` for an event without a level.
    pub(crate) async fn continue_after_approval(
        &mut self,
        approval: &str,
        artifact: Artifact,
        plan: PublicationPlan,
    ) -> Result<(), Error> {
        match approval {
            APPROVAL_KIND_APPROVE => {
                // Holding the pull request again shows a failed publication to the scheduler.
                // It also allows an explicit retry.
                self.approved = true;
                live(&self.cancel)?;
                self.set_released(false)?;
                let current = match self.current_pull_request().await {
                    Ok(current) => current,
                    Err(err) => {
                        if is_shutdown(&err) {
                            return Err(err);
                        }
                        let record = self.await_record();
                        if let Err(clear) = self.engine.clear_awaiting(&self.cancel, &record).await
                        {
                            return Err(self.interrupted(clear));
                        }
                        return self.fail(err).await;
                    }
                };
                self.publish(&current, &artifact, &plan).await
            }
            APPROVAL_KIND_EXPIRED => {
                self.notify(
                    "approval_expired",
                    Event::ApprovalExpired,
                    &approval_expired_message(&self.input),
                )
                .await?;
                self.settle_await(Outcome::Expired).await
            }
            APPROVAL_KIND_SUPERSEDED => {
                self.notify(
                    "superseded",
                    Event::Superseded,
                    &superseded_message(&self.input, "newer_review"),
                )
                .await?;
                self.settle_await(Outcome::Superseded).await
            }
            _ => {
                // A wait without a decision was interrupted before it was recorded.
                self.await_approval(&artifact).await
            }
        }
    }

    /// Returns the discovery record of the wait of this review.
    fn await_record(&self) -> AwaitRecord {
        let pr = &self.input.pull_request;
        AwaitRecord {
            repository: pr.repository.clone(),
            number: pr.number,
            head_sha: pr.head_sha.clone(),
            review_request_id: pr.review_request_id,
            trigger: self.input.rule.trigger.clone(),
            workflow_id: self.workflow_id.clone(),
        }
    }

    /// Removes the wait from discovery and completes a review that ended without a publication.
    async fn settle_await(&self, outcome: Outcome) -> Result<(), Error> {
        if let Err(err) = self
            .engine
            .clear_awaiting(&self.cancel, &self.await_record())
            .await
        {
            return Err(self.interrupted(err));
        }
        live(&self.cancel)?;
        self.engine.store.complete_review(
            &self.workflow_id,
            RESULT_SUCCESS,
            "",
            Some(&ReviewOutcome {
                outcome,
                publication: None,
            }),
        )
    }
}

impl<I: Integrations, C: Clock> Engine<I, C> {
    /// Records the user's approval of a clean review that waits for it.
    ///
    /// The engine publishes the review on its next tick.
    ///
    /// # Errors
    ///
    /// Returns the error of the store, `review_incomplete` for a review without its artifact or
    /// plan, `review_identity_invalid`, `approval_not_required`, `review_not_awaiting_approval`,
    /// `approval_expired` or `review_superseded`.
    pub fn approve(
        &self,
        cancel: &CancellationToken,
        workflow_id: &str,
        request: &ApproveRequest,
    ) -> Result<ApprovalResult, Error> {
        live(cancel)?;
        let stored = self.store.get_review(workflow_id)?;
        let Some(stored) = stored else {
            return Err(Error::workflow(REVIEW_INCOMPLETE, true, None));
        };
        let artifact = live(cancel)
            .and_then(|()| self.store.load_step::<Artifact>(workflow_id, STEP_ARTIFACT))?;
        let plan = live(cancel).and_then(|()| {
            self.store
                .load_step::<PublicationPlan>(workflow_id, STEP_PLAN)
        })?;
        let (Some(artifact), Some(plan)) = (artifact, plan) else {
            return Err(Error::workflow(REVIEW_INCOMPLETE, true, None));
        };
        if request.repository != artifact.repository
            || request.number != artifact.number
            || request.head_sha != artifact.head_sha
        {
            return Err(Error::workflow(REVIEW_IDENTITY_INVALID, true, None));
        }
        if !plan.await_approval {
            return Err(Error::workflow(APPROVAL_NOT_REQUIRED, true, None));
        }
        let mut result = ApprovalResult {
            ok: true,
            review_id: workflow_id.to_owned(),
            publication_status: PUBLICATION_PENDING.to_owned(),
            github_review_id: 0,
            review_url: String::new(),
        };
        let publication = live(cancel).and_then(|()| {
            self.store
                .load_step::<PublishResult>(workflow_id, STEP_PUBLICATION)
        })?;
        if let Some(published) = publication {
            result.publication_status = PUBLICATION_PUBLISHED.to_owned();
            result.github_review_id = published.github_review_id;
            result.review_url = published.review_url;
            return Ok(result);
        }
        let mut decision = stored.approval;
        if decision.is_empty() {
            if stored.status != REVIEW_AWAITING {
                return Err(Error::workflow(REVIEW_NOT_AWAITING_APPROVAL, true, None));
            }
            live(cancel)?;
            decision = self
                .store
                .decide_approval(workflow_id, APPROVAL_KIND_APPROVE)?;
        }
        match decision.as_str() {
            APPROVAL_KIND_APPROVE => Ok(result),
            APPROVAL_KIND_EXPIRED => Err(Error::workflow(APPROVAL_EXPIRED, true, None)),
            _ => Err(Error::workflow(REVIEW_SUPERSEDED, true, None)),
        }
    }
}
