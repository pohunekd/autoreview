//! The approval wait of a clean review: it is recorded, released and then decided.
//!
//! Mirrors `cmd/autoreview/review.go:777-856`.

// Rust guideline compliant 2026-06-26

use autoreview_policy::Event;

use super::sessions::{Review, is_shutdown};
use crate::discovery::poll::live;
use crate::engine::Clock;
use crate::error::Error;
use crate::integrations::Integrations;
use crate::messages::{approval_expired_message, approval_requested_message, superseded_message};
use crate::model::review::{
    APPROVAL_KIND_APPROVE, APPROVAL_KIND_EXPIRED, APPROVAL_KIND_SUPERSEDED,
};
use crate::model::{Artifact, AwaitRecord, Outcome, ReviewOutcome};
use crate::publish::plan::PublicationPlan;
use crate::store::RESULT_SUCCESS;

/// Nanoseconds in one second, as Go's `time.Second`.
const NANOS_PER_SECOND: i64 = 1_000_000_000;

impl<I: Integrations, C: Clock> Review<'_, I, C> {
    /// Records the wait, releases the pull request and asks the user for an approval.
    ///
    /// The wait is recorded before the release, so discovery never sees the head without a
    /// review it keeps.
    ///
    /// # Errors
    ///
    /// Returns the shutdown error, or the error of the store or of the discovery record. A
    /// failed approval request completes the review as a failure and returns `Ok`.
    #[expect(dead_code, reason = "called by S28b")]
    pub(crate) async fn await_approval(&mut self, artifact: &Artifact) -> Result<(), Error> {
        let record = self.await_record();
        if let Err(err) = self.engine.mark_awaiting(&self.cancel, &record).await {
            return Err(self.interrupted(err));
        }
        live(&self.cancel)?;
        self.set_released(true)?;
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
            // Nobody would learn of the wait, so the review fails and is reported.
            live(&self.cancel)?;
            self.set_released(false)?;
            if let Err(clear) = self.engine.clear_awaiting(&self.cancel, &record).await {
                return Err(self.interrupted(clear));
            }
            return self.fail(err).await;
        }
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
            .start_awaiting(&self.workflow_id, deadline)
    }

    /// Continues a waiting review by the decision `approval` stored for it.
    ///
    /// An approval publishes the review, an expiry or a newer review ends the wait without a
    /// publication, and a wait without a decision is recorded again.
    ///
    /// # Errors
    ///
    /// Returns the shutdown error, or the error of the store, a notification or the discovery
    /// record.
    #[expect(dead_code, reason = "called by S28b")]
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
