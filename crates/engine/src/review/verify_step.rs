//! The verification step: the verifier checks the blocking claims of a review.
//!
//! Mirrors `cmd/autoreview/review.go:545-593`.

// Rust guideline compliant 2026-06-26

use super::input::{STATE_VERIFIER_DEADLINE, STEP_VERIFICATION};
use super::sessions::{Review, ReviewerSlot, SlotId, StepFailure, is_session_fault};
use crate::discovery::poll::live;
use crate::engine::Clock;
use crate::error::{Error, WORKFLOW_FAILED, error_code};
use crate::integrations::Integrations;
use crate::model::Verification;
use crate::protocol::go_utf8_lossy;
use crate::store::{ReviewStep, StepOutcome, StepRun};
use crate::verify::{
    VERDICT_CONFIRMED, VERDICT_NOT_BLOCKING, VERDICT_REFUTED, VERDICT_REOPENED, VERDICT_UPHELD,
    VerificationClaims, build_verifier_request, parse_verifier_output,
};

/// Reports whether the verifier slot `slot` holds its checked result.
fn verifier_done(slot: &ReviewerSlot) -> bool {
    slot.state.verification.is_some()
}

/// Counts the checks of `verification`, findings and prior alike, with the verdict `verdict`.
fn verdict_count(verification: &Verification, verdict: &str) -> usize {
    verification
        .findings
        .iter()
        .filter(|check| check.verdict == verdict)
        .count()
        + verification
            .prior
            .iter()
            .filter(|check| check.verdict == verdict)
            .count()
}

/// Returns `failure` with the run, if it records one, lasting `duration_ms`.
fn measured_at(failure: StepFailure, duration_ms: i64) -> StepFailure {
    let run = failure.run.map(|run| StepRun { duration_ms, ..run });
    StepFailure { run, ..failure }
}

impl<I: Integrations, C: Clock> Review<'_, I, C> {
    /// Runs the verifier on `claims` once per review, stores the verification and records the
    /// run of the verifier step with it.
    ///
    /// A verifier session that fails leaves every claim as the reviewers made it, so the review
    /// still publishes; its run is recorded as failed. An error of the review itself, such as a
    /// newer head, stops it, and its run is recorded when the review completes as failed.
    ///
    /// # Errors
    ///
    /// Returns the error of the verifier request, of the store, of the review, such as
    /// `head_changed`, and `workflow_failed` when the review has no verifier slot.
    pub(crate) async fn verify(
        &mut self,
        claims: &VerificationClaims,
    ) -> Result<Verification, StepFailure> {
        let loaded = live(&self.cancel)
            .and_then(|()| {
                self.engine
                    .store
                    .load_step::<Verification>(&self.workflow_id, STEP_VERIFICATION)
            })
            .map_err(StepFailure::without_run)?;
        if let Some(stored) = loaded {
            return Ok(stored);
        }
        let prompt = build_verifier_request(&self.input, claims, &self.discussion)
            .map_err(StepFailure::without_run)?;
        let Some(slot) = self.verifier.as_mut() else {
            let error = Error::workflow(WORKFLOW_FAILED, true, None);
            return Err(StepFailure::without_run(error));
        };
        slot.request.prompt = prompt;
        let reviewer_label = slot.reviewer.label.clone();
        let ids = [SlotId::Verifier];
        let started = self.step_started();
        let run = match self.start_slots(&ids, verifier_done).await {
            Ok(()) => {
                self.await_slots(
                    &ids,
                    STATE_VERIFIER_DEADLINE,
                    None,
                    verifier_done,
                    |review: &Self, slot: &mut ReviewerSlot, output: &[u8]| {
                        review.parse_verifier(claims, slot, output)
                    },
                )
                .await
            }
            Err(err) => Err(err),
        };
        let checked = run.and_then(|()| {
            self.verifier
                .as_ref()
                .and_then(|slot| slot.state.verification.clone())
                .ok_or_else(|| Error::workflow(WORKFLOW_FAILED, true, None))
        });
        let mut verification = Verification {
            reviewer_label,
            findings: Vec::new(),
            prior: Vec::new(),
            error_code: String::new(),
        };
        let step = ReviewStep::Verifier;
        let recorded = match checked {
            Ok(result) => {
                verification.findings = result.findings;
                verification.prior = result.prior;
                tracing::info!(
                    review_id = %self.input.pull_request.review_id(),
                    confirmed = verdict_count(&verification, VERDICT_CONFIRMED),
                    not_blocking = verdict_count(&verification, VERDICT_NOT_BLOCKING),
                    refuted = verdict_count(&verification, VERDICT_REFUTED),
                    upheld = verdict_count(&verification, VERDICT_UPHELD),
                    reopened = verdict_count(&verification, VERDICT_REOPENED),
                    "review verified"
                );
                self.step_run(step, started, StepOutcome::Succeeded)
            }
            Err(err) if is_session_fault(&err) => {
                // Measured before the session cleanup, which is not part of the run.
                let failed = self.step_run(step, started, StepOutcome::Failed);
                if let Err(cleanup) = self.remove_sessions(&ids).await {
                    // A failed cleanup ends the step, and the run keeps its time before it.
                    let failure = self.step_failure(step, started, cleanup);
                    return Err(measured_at(failure, failed.duration_ms));
                }
                verification.error_code = error_code(&err).to_owned();
                tracing::warn!(
                    review_id = %self.input.pull_request.review_id(),
                    error_code = %verification.error_code.as_str(),
                    "verification did not finish, publishing the claims unverified"
                );
                failed
            }
            Err(err) => return Err(self.step_failure(step, started, err)),
        };
        let inserted = live(&self.cancel)
            .and_then(|()| {
                self.engine.store.save_step_recorded(
                    &self.workflow_id,
                    STEP_VERIFICATION,
                    &verification,
                    &recorded,
                )
            })
            // A failed write ends the step, and the run keeps the time measured with its result.
            .map_err(|err| {
                measured_at(self.step_failure(step, started, err), recorded.duration_ms)
            })?;
        if inserted {
            self.engine.count_step(&recorded);
        }
        Ok(verification)
    }

    /// Checks the complete output of the verifier and stores the result in `slot`.
    ///
    /// The bytes become text as Go's `encoding/json` reads them: one U+FFFD per invalid byte.
    fn parse_verifier(
        &self,
        claims: &VerificationClaims,
        slot: &mut ReviewerSlot,
        output: &[u8],
    ) -> Result<(), Error> {
        let raw = go_utf8_lossy(output);
        let result = parse_verifier_output(&self.engine.config, &self.input, claims, &raw)?;
        slot.state.verification = Some(result);
        Ok(())
    }
}
