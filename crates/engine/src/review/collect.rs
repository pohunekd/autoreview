//! Collecting the reviewer results.
//!
//! The sessions run in parallel until each slot has a result.
//!
//! Mirrors `cmd/autoreview/review.go:430-548`.

// Rust guideline compliant 2026-06-26

use std::time::Instant;

use autoreview_policy::Event;

use super::input::STATE_DEADLINE;
use super::sessions::{
    Review, ReviewerSlot, SlotId, StepFailure, is_shutdown, session_fault, session_fault_from,
    wait_of,
};
use crate::engine::Clock;
use crate::error::{Error, WORKFLOW_FAILED, error_code};
use crate::integrations::Integrations;
use crate::messages::started_message;
use crate::model::review::milliseconds;
use crate::protocol::validate::reviewer_output_retryable;
use crate::protocol::{go_utf8_lossy, parse_reviewer_output};
use crate::store::{ReviewStep, StepOutcome};

/// Step name of the read of a reviewer session's output in the retry log.
const STEP_SESSION_OUTPUT: &str = "session_output";

/// Reports whether the reviewer slot `slot` holds its checked result.
fn reviewer_done(slot: &ReviewerSlot) -> bool {
    slot.state.result.is_some()
}

impl<I: Integrations, C: Clock> Review<'_, I, C> {
    /// Runs every reviewer slot to a result and records the run of the reviewers step.
    ///
    /// The succeeded run is recorded with the last stored result, a failed run when the review
    /// completes as failed. A review whose reviewers all had a stored result when it resumed
    /// records nothing: that run ended before.
    pub(crate) async fn collect(&mut self) -> Result<(), StepFailure> {
        let started = self.step_started();
        let ended = self.slots.iter().all(reviewer_done);
        let collected = self.collect_results(started).await;
        collected.map_err(|err| {
            if ended {
                StepFailure::without_run(err)
            } else {
                self.step_failure(ReviewStep::Reviewers, started, err)
            }
        })
    }

    /// Runs every reviewer slot to a result; the run started at `started`.
    ///
    /// It announces the start once the sessions are up.
    async fn collect_results(&mut self, started: Instant) -> Result<(), Error> {
        let ids: Vec<SlotId> = (0..self.slots.len()).map(SlotId::Reviewer).collect();
        self.start_slots(&ids, reviewer_done).await?;
        let message = started_message(&self.input);
        self.notify("started", Event::Started, &message).await?;
        let recorded = Some((ReviewStep::Reviewers, started));
        self.await_slots(
            &ids,
            STATE_DEADLINE,
            recorded,
            reviewer_done,
            Self::parse_reviewer,
        )
        .await
    }

    /// Checks the complete output of a reviewer and stores the result in `slot`.
    ///
    /// The bytes become text as Go's `encoding/json` reads them: one U+FFFD per invalid byte.
    fn parse_reviewer(&self, slot: &mut ReviewerSlot, output: &[u8]) -> Result<(), Error> {
        let raw = go_utf8_lossy(output);
        let result = parse_reviewer_output(
            &self.engine.config,
            &self.input,
            &self.discussion,
            &slot.reviewer,
            &raw,
        )?;
        slot.state.result = Some(result);
        Ok(())
    }

    /// Removes the sessions of finished slots and starts a session for each other slot.
    ///
    /// A restart between storing a result and removing its session leaves the session open.
    pub(crate) async fn start_slots<D>(&mut self, ids: &[SlotId], done: D) -> Result<(), Error>
    where
        D: Fn(&ReviewerSlot) -> bool,
    {
        let mut finished: Vec<SlotId> = Vec::new();
        for id in ids {
            if let Some(slot) = self.slot(*id)
                && done(slot)
                && slot.state.session.is_some()
            {
                finished.push(*id);
            }
        }
        self.remove_sessions(&finished).await?;
        for id in ids {
            let Some(slot) = self.slot(*id) else {
                return Err(session_fault(WORKFLOW_FAILED));
            };
            if done(slot) || slot.state.session.is_some() {
                continue;
            }
            self.start_session(*id).await?;
        }
        Ok(())
    }

    /// Polls the slots `ids` until each one is done.
    ///
    /// It fails when the deadline under `deadline_key` passes or the head moves.
    ///
    /// `parse` checks a complete output into the slot. A slot whose output is unusable gets a
    /// fresh session up to the run limit.
    ///
    /// With `recorded`, the step and its start, the result that makes every slot done is
    /// stored with the succeeded run of that step.
    pub(crate) async fn await_slots<D, P>(
        &mut self,
        ids: &[SlotId],
        deadline_key: &str,
        recorded: Option<(ReviewStep, Instant)>,
        done: D,
        parse: P,
    ) -> Result<(), Error>
    where
        D: Fn(&ReviewerSlot) -> bool,
        P: Fn(&Self, &mut ReviewerSlot, &[u8]) -> Result<(), Error>,
    {
        let limits = self.limits().clone();
        let deadline = self.deadline(deadline_key)?;
        loop {
            self.current_pull_request().await?;
            let mut pending = false;
            for id in ids {
                let Some(slot) = self.slot(*id) else {
                    return Err(session_fault(WORKFLOW_FAILED));
                };
                if done(slot) {
                    continue;
                }
                let mut updated = slot.clone();
                let Some(session) = updated.state.session.clone() else {
                    return Err(session_fault(WORKFLOW_FAILED));
                };
                let protocol = updated.reviewer.protocol.clone();
                let output = self
                    .fresh(STEP_SESSION_OUTPUT, || {
                        let integration = &*self.engine.integration;
                        let cancel = &self.cancel;
                        let session = session.clone();
                        let protocol = protocol.clone();
                        async move { integration.session_output(cancel, &session, protocol).await }
                    })
                    .await;
                let output = match output {
                    Ok(output) => output,
                    Err(err) => {
                        if is_shutdown(&err) {
                            return Err(err);
                        }
                        return Err(session_fault_from(err));
                    }
                };
                let Some(bytes) = output else {
                    pending = true;
                    continue;
                };
                match parse(&*self, &mut updated, &bytes) {
                    Ok(()) => {
                        match self.slot_mut(*id) {
                            Some(slot) => slot.clone_from(&updated),
                            None => return Err(session_fault(WORKFLOW_FAILED)),
                        }
                        let complete = ids.iter().all(|id| self.slot(*id).is_some_and(&done));
                        match recorded {
                            Some((step, started)) if complete => {
                                let run = self.step_run(step, started, StepOutcome::Succeeded);
                                self.save_slot_recorded(&updated, &run)?;
                            }
                            _ => self.save_slot(&updated)?,
                        }
                        self.remove_sessions(&[*id]).await?;
                    }
                    Err(err) => {
                        if reviewer_output_retryable(&err)
                            && updated.state.run < limits.reviewer_run_attempts
                        {
                            tracing::warn!(
                                review_id = %self.input.pull_request.review_id(),
                                reviewer = %updated.reviewer.id,
                                run = updated.state.run,
                                error_code = %error_code(&err),
                                error = %err,
                                "retrying reviewer after unusable output"
                            );
                            self.count_retry();
                            self.remove_sessions(&[*id]).await?;
                            self.sleep(wait_of(milliseconds(limits.reviewer_run_retry_ms)))
                                .await?;
                            let saved = match self.slot_mut(*id) {
                                Some(slot) => {
                                    slot.state.run += 1;
                                    slot.state.start = 0;
                                    slot.clone()
                                }
                                None => return Err(session_fault(WORKFLOW_FAILED)),
                            };
                            self.save_slot(&saved)?;
                            self.start_session(*id).await?;
                            pending = true;
                        } else {
                            tracing::warn!(
                                review_id = %self.input.pull_request.review_id(),
                                reviewer = %updated.reviewer.id,
                                run = updated.state.run,
                                error_code = %error_code(&err),
                                error = %err,
                                "rejecting reviewer output"
                            );
                            return Err(session_fault(error_code(&err)));
                        }
                    }
                }
            }
            if !pending {
                return Ok(());
            }
            if self.engine.clock.wall_now() >= deadline {
                return Err(session_fault("review_output_timeout"));
            }
            self.sleep(wait_of(milliseconds(limits.output_poll_ms)))
                .await?;
        }
    }
}
