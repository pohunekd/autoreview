//! A running review: its slots, stored steps, notifications and reviewer sessions.
//!
//! Mirrors `cmd/autoreview/review.go:34-36, 129-142, 153-428`.

// Rust guideline compliant 2026-06-26

use std::error::Error as StdError;
use std::future::Future;
use std::time::Duration;

use autoreview_policy::{Event, Reviewer};
use autoreview_schedule::diagnostic::diagnostic;
use autoreview_schedule::scheduler::superseded;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

use super::input::{NOTIFY_STEP_PREFIX, SLOT_STATE_PREFIX, SlotState, still_wanted};
use crate::engine::{Clock, Engine, StepRetryPolicy, retry_step};
use crate::error::{BoxError, Error, WORKFLOW_FAILED, error_code, pohunek_error_code};
use crate::integrations::{Integrations, SessionRequest};
use crate::messages::{
    SessionCleanupFailure, approved_review_failed_message, orphaned_session_message,
    session_cleanup_failed_message, superseded_message,
};
use crate::model::review::milliseconds;
use crate::model::{Discussion, PullRequest, ReviewInput, RunLimits};
use crate::store::RESULT_FAILURE;

/// Marks a review interrupted by an engine shutdown.
///
/// The review keeps its status and resumes from its stored steps on the next start.
#[derive(Debug, thiserror::Error)]
#[error("review interrupted by shutdown")]
pub(crate) struct Shutdown;

/// Marks a failure of a reviewer session itself.
///
/// The session did not start, could not be read, or gave no usable output in time.
#[derive(Debug, thiserror::Error)]
#[error("reviewer session failed")]
pub(crate) struct SessionFault {
    /// The error of the failed session read, kept as the diagnostic of the fault.
    #[source]
    source: Option<Error>,
}

/// Returns the error of a review interrupted by an engine shutdown.
///
/// Its code is `workflow_failed` and it is not permanent, as Go's plain `errShutdown`.
pub(crate) fn shutdown_error() -> Error {
    Error::workflow(WORKFLOW_FAILED, false, Some(Box::new(Shutdown)))
}

/// Reports whether `err` or an error in its chain is a [`Shutdown`].
pub(crate) fn is_shutdown(err: &Error) -> bool {
    chain_has::<Shutdown>(err)
}

/// Returns a permanent session failure with `code`, marked as a [`SessionFault`].
pub(crate) fn session_fault(code: &str) -> Error {
    Error::workflow(code, true, Some(Box::new(SessionFault { source: None })))
}

/// Returns a permanent session failure with the code of `err`, keeping `err` as its source.
///
/// Only the source differs from [`session_fault`]: the code and the permanent flag are the same.
pub(crate) fn session_fault_from(err: Error) -> Error {
    let code = error_code(&err).to_owned();
    Error::workflow(
        code,
        true,
        Some(Box::new(SessionFault { source: Some(err) })),
    )
}

/// Reports whether `err` or an error in its chain is a [`SessionFault`].
pub(crate) fn is_session_fault(err: &Error) -> bool {
    chain_has::<SessionFault>(err)
}

/// Reports whether an error of type `T` is in the source chain of `err`.
fn chain_has<T: StdError + 'static>(err: &Error) -> bool {
    let mut current: Option<&dyn StdError> = Some(err);
    while let Some(error) = current {
        if error.is::<T>() {
            return true;
        }
        current = error.source();
    }
    false
}

/// Returns the code, permanent flag and cause of the first workflow failure in the chain.
fn first_workflow(err: &Error) -> Option<(&str, bool, Option<&BoxError>)> {
    let mut current: Option<&dyn StdError> = Some(err);
    while let Some(error) = current {
        if let Some(Error::Workflow {
            code,
            permanent,
            cause,
        }) = error.downcast_ref::<Error>()
        {
            return Some((code.as_str(), *permanent, cause.as_ref()));
        }
        current = error.source();
    }
    None
}

/// Converts a Go duration to a wait; a negative one, which Go waits zero for, is zero.
pub(super) fn wait_of(value: time::Duration) -> Duration {
    Duration::try_from(value).unwrap_or(Duration::ZERO)
}

/// A persisted deadline, written as Go writes a `time.Time`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
struct Deadline(#[serde(with = "autoreview_go_compat::go_time")] OffsetDateTime);

/// Names one slot of a review: a reviewer by its position, or the verifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SlotId {
    /// The reviewer at this position of `Review::slots`.
    Reviewer(usize),
    /// The verifier slot.
    Verifier,
}

/// One reviewer or the verifier, with its session request and its stored progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReviewerSlot {
    /// The reviewer the slot runs.
    pub(crate) reviewer: Reviewer,
    /// The session request; its run and start are set from the state.
    pub(crate) request: SessionRequest,
    /// The persisted progress of the slot.
    pub(crate) state: SlotState,
}

/// One running review of one pull request head.
#[expect(dead_code, reason = "called by S28")]
pub(crate) struct Review<'a, I, C> {
    /// The engine that runs the review.
    pub(crate) engine: &'a Engine<I, C>,
    /// Cancelled when the engine shuts down.
    pub(crate) cancel: CancellationToken,
    /// Workflow ID of the review attempt.
    pub(crate) workflow_id: String,
    /// The stored input the review decides by.
    pub(crate) input: ReviewInput,
    /// The reviewer slots, in the order of the input reviewers.
    pub(crate) slots: Vec<ReviewerSlot>,
    /// The verifier slot of a pipeline that has a verifier.
    pub(crate) verifier: Option<ReviewerSlot>,
    /// What the reviewers of a review with prior findings read.
    pub(crate) discussion: Discussion,
    /// Set once the user approved a waiting review.
    pub(crate) approved: bool,
}

impl<I: Integrations, C: Clock> Review<'_, I, C> {
    /// Returns the loop limits of the review, read from its input.
    pub(crate) fn limits(&self) -> &RunLimits {
        &self.input.limits
    }

    /// Returns the step retry policy of the review, read from its limits.
    fn retry_policy(&self) -> StepRetryPolicy {
        let limits = self.limits();
        StepRetryPolicy {
            total: wait_of(milliseconds(limits.step_retry_ms)),
            initial: wait_of(milliseconds(limits.step_retry_initial_ms)),
            max_interval: wait_of(milliseconds(limits.step_retry_max_interval_ms)),
        }
    }

    /// Turns an error into [`shutdown_error`] once the engine is shutting down.
    ///
    /// A shutdown is never recorded as a review failure.
    pub(crate) fn interrupted(&self, err: Error) -> Error {
        if self.cancel.is_cancelled() {
            shutdown_error()
        } else {
            err
        }
    }

    /// Runs `step` once per review with step retries and stores its output under `name`.
    ///
    /// A resumed review reads the stored output instead of repeating the side effect.
    pub(crate) async fn once<T, Fut>(
        &self,
        name: &str,
        step: impl FnMut() -> Fut,
    ) -> Result<T, Error>
    where
        T: Serialize + DeserializeOwned,
        Fut: Future<Output = Result<T, Error>>,
    {
        if let Some(value) = self.engine.store.load_step::<T>(&self.workflow_id, name)? {
            return Ok(value);
        }
        match retry_step(self.retry_policy(), &self.engine.clock, &self.cancel, step).await {
            Ok(value) => {
                self.engine
                    .store
                    .save_step(&self.workflow_id, name, &value)?;
                Ok(value)
            }
            Err(err) => Err(self.interrupted(err)),
        }
    }

    /// Runs `step` with step retries and never stores its output.
    ///
    /// It serves reads whose current answer matters on every resume.
    pub(crate) async fn fresh<T, Fut>(&self, step: impl FnMut() -> Fut) -> Result<T, Error>
    where
        Fut: Future<Output = Result<T, Error>>,
    {
        match retry_step(self.retry_policy(), &self.engine.clock, &self.cancel, step).await {
            Ok(value) => Ok(value),
            Err(err) => Err(self.interrupted(err)),
        }
    }

    /// Waits for `delay`, or fails with [`shutdown_error`] when the engine shuts down.
    pub(crate) async fn sleep(&self, delay: Duration) -> Result<(), Error> {
        match self
            .cancel
            .run_until_cancelled(self.engine.clock.sleep(delay))
            .await
        {
            Some(()) => Ok(()),
            None => Err(shutdown_error()),
        }
    }

    /// Sends `message` at the level of `event` once per review, under the step `name`.
    ///
    /// An event without a level fails with `review_input_invalid`.
    pub(crate) async fn notify(
        &self,
        name: &str,
        event: Event,
        message: &str,
    ) -> Result<(), Error> {
        let step_name = format!("{NOTIFY_STEP_PREFIX}{name}");
        self.once::<bool, _>(&step_name, || {
            let integration = &*self.engine.integration;
            async move {
                let Some(level) = self.input.level(event) else {
                    return Err(Error::workflow("review_input_invalid", true, None));
                };
                integration.notify(level, message).await?;
                Ok(true)
            }
        })
        .await?;
        Ok(())
    }

    /// Returns the slot that `id` names, or `None` when the review has no such slot.
    pub(crate) fn slot(&self, id: SlotId) -> Option<&ReviewerSlot> {
        match id {
            SlotId::Reviewer(index) => self.slots.get(index),
            SlotId::Verifier => self.verifier.as_ref(),
        }
    }

    /// Returns the slot that `id` names for a change, or `None` when there is no such slot.
    pub(crate) fn slot_mut(&mut self, id: SlotId) -> Option<&mut ReviewerSlot> {
        match id {
            SlotId::Reviewer(index) => self.slots.get_mut(index),
            SlotId::Verifier => self.verifier.as_mut(),
        }
    }

    /// Stores the progress of `slot` under `slot/<reviewer id>`.
    pub(crate) fn save_slot(&self, slot: &ReviewerSlot) -> Result<(), Error> {
        let name = format!("{SLOT_STATE_PREFIX}{}", slot.reviewer.id);
        self.engine
            .store
            .set_state(&self.workflow_id, &name, &slot.state)
    }

    /// Stores whether the review may still own open sessions.
    pub(crate) fn set_cleanup_pending(&self, pending: bool) -> Result<(), Error> {
        self.engine
            .store
            .set_review_flags(&self.workflow_id, None, Some(pending))
    }

    /// Stores whether the review released its claim on the pull request.
    pub(crate) fn set_released(&self, released: bool) -> Result<(), Error> {
        self.engine
            .store
            .set_review_flags(&self.workflow_id, Some(released), None)
    }

    /// Returns the IDs of the reviewer slots, then the verifier slot when there is one.
    pub(crate) fn all_slots(&self) -> Vec<SlotId> {
        let mut ids: Vec<SlotId> = (0..self.slots.len()).map(SlotId::Reviewer).collect();
        if self.verifier.is_some() {
            ids.push(SlotId::Verifier);
        }
        ids
    }

    /// Reports whether any slot still has an open session.
    pub(crate) fn sessions_open(&self) -> bool {
        self.slots
            .iter()
            .chain(self.verifier.iter())
            .any(|slot| slot.state.session.is_some())
    }

    /// Removes the sessions of the slots `ids` with bounded attempts.
    ///
    /// A session the host keeps is reported for manual removal and the review goes on: its
    /// output is stored, so the session holds nothing.
    pub(crate) async fn remove_sessions(&mut self, ids: &[SlotId]) -> Result<(), Error> {
        let limits = self.limits().clone();
        let mut failures: Vec<SessionCleanupFailure> = Vec::new();
        let mut names: Vec<String> = Vec::new();
        for id in ids {
            let slot = match self.slot_mut(*id) {
                Some(slot) => slot,
                None => continue,
            };
            let Some(session) = slot.state.session.clone() else {
                continue;
            };
            names.push(format!("{}-{}", slot.reviewer.id, slot.state.run));
            let mut code = String::new();
            for attempt in 1..=limits.session_cleanup_attempts {
                let integration = &*self.engine.integration;
                let remove = integration.remove_session_confirmed(&session).await;
                let remove = match remove {
                    Ok(()) => Ok(()),
                    Err(err) => Err(self.interrupted(err)),
                };
                match remove {
                    Ok(()) => {
                        code.clear();
                        break;
                    }
                    Err(err) => {
                        if is_shutdown(&err) {
                            return Err(err);
                        }
                        code = error_code(&err).to_owned();
                        if let Some(pohunek) = pohunek_error_code(&err) {
                            code.push('/');
                            code.push_str(pohunek);
                        }
                        tracing::warn!(
                            review_id = %self.input.pull_request.review_id(),
                            reviewer = %session.reviewer_id,
                            session_id = %session.session_id,
                            host = %session.host,
                            attempt = attempt,
                            error_code = %code.as_str(),
                            "reviewer session cleanup failed"
                        );
                        if attempt < limits.session_cleanup_attempts {
                            self.sleep(wait_of(milliseconds(limits.session_cleanup_retry_ms)))
                                .await?;
                        }
                    }
                }
            }
            if !code.is_empty() {
                failures.push(SessionCleanupFailure { session, code });
            }
            let slot = match self.slot_mut(*id) {
                Some(slot) => slot,
                None => continue,
            };
            slot.state.session = None;
            let saved = slot.clone();
            self.save_slot(&saved)?;
        }
        if !self.sessions_open() {
            self.set_cleanup_pending(false)?;
        }
        if failures.is_empty() {
            return Ok(());
        }
        names.sort();
        let message = session_cleanup_failed_message(
            &self.input.pull_request,
            &failures,
            limits.session_cleanup_attempts,
        );
        self.notify(
            &format!("cleanup/{}", names.join(",")),
            Event::CleanupManual,
            &message,
        )
        .await
    }

    /// Starts the session of slot `id`, retrying transient start failures inside the review.
    ///
    /// A slow host then does not cost a whole scheduler attempt. The integration finds a session
    /// started before a restart by its name.
    #[expect(dead_code, reason = "called by S26")]
    pub(crate) async fn start_session(&mut self, id: SlotId) -> Result<(), Error> {
        let limits = self.limits().clone();
        let Some(state) = self.slot(id).map(|slot| slot.state.clone()) else {
            return Err(session_fault(WORKFLOW_FAILED));
        };
        let Some(template) = self.slot(id).map(|slot| slot.request.clone()) else {
            return Err(session_fault(WORKFLOW_FAILED));
        };
        let mut request = template;
        request.run = state.run;
        let mut start = state.start.max(1);
        while start <= limits.session_start_attempts {
            let attempt = start;
            if let Some(slot) = self.slot_mut(id)
                && slot.state.start != attempt
            {
                slot.state.start = attempt;
                let saved = slot.clone();
                self.save_slot(&saved)?;
            }
            request.start = attempt;
            let integration = &*self.engine.integration;
            let started = integration.ensure_session(&request).await;
            let started = match started {
                Ok(session) => Ok(session),
                Err(err) => Err(self.interrupted(err)),
            };
            match started {
                Ok(session) => {
                    if let Some(slot) = self.slot_mut(id) {
                        slot.state.session = Some(session);
                        let saved = slot.clone();
                        self.save_slot(&saved)?;
                    }
                    self.set_cleanup_pending(true)?;
                    return self.remove_orphaned_sessions(&request, attempt - 1).await;
                }
                Err(err) => {
                    if is_shutdown(&err) {
                        return Err(err);
                    }
                    let (code, permanent) = match first_workflow(&err) {
                        Some((code, permanent, cause)) => {
                            let reviewer =
                                self.slot(id).map_or("", |slot| slot.reviewer.id.as_str());
                            tracing::warn!(
                                review_id = %self.input.pull_request.review_id(),
                                reviewer = %reviewer,
                                host = %request.route.host,
                                start_attempt = attempt,
                                error_code = %code,
                                diagnostic = %diagnostic(cause.map(|cause| &**cause)),
                                "reviewer session start failed"
                            );
                            (code.to_owned(), permanent)
                        }
                        None => (WORKFLOW_FAILED.to_owned(), true),
                    };
                    if permanent || attempt == limits.session_start_attempts {
                        self.remove_orphaned_sessions(&request, attempt).await?;
                        return Err(session_fault(&code));
                    }
                    self.sleep(wait_of(milliseconds(limits.session_start_retry_ms)))
                        .await?;
                }
            }
            start += 1;
        }
        Err(session_fault(WORKFLOW_FAILED))
    }

    /// Removes the sessions of the first `starts` start attempts of the run of `request`.
    ///
    /// These are sessions the host created after their start was given up.
    async fn remove_orphaned_sessions(
        &self,
        request: &SessionRequest,
        starts: i64,
    ) -> Result<(), Error> {
        for start in 1..=starts {
            let mut orphan = request.clone();
            orphan.start = start;
            self.remove_orphaned_session(&orphan).await?;
        }
        Ok(())
    }

    /// Removes a session the host created after the client gave up on its start.
    ///
    /// The start failure stays the cause; a session that may still run is reported for manual
    /// removal.
    async fn remove_orphaned_session(&self, request: &SessionRequest) -> Result<(), Error> {
        let integration = &*self.engine.integration;
        let found = integration.find_session(request).await;
        let session = match found {
            Ok(found) => found,
            Err(err) => {
                let err = self.interrupted(err);
                if is_shutdown(&err) {
                    return Err(err);
                }
                tracing::warn!(
                    review_id = %self.input.pull_request.review_id(),
                    reviewer = %request.reviewer.id,
                    error_code = %error_code(&err),
                    diagnostic = %diagnostic(Some(&err)),
                    "orphaned reviewer session cleanup failed"
                );
                return self
                    .notify(
                        &format!(
                            "orphan/{}/{}/{}",
                            request.reviewer.id, request.run, request.start
                        ),
                        Event::CleanupManual,
                        &orphaned_session_message(request, error_code(&err)),
                    )
                    .await;
            }
        };
        if let Some(session) = session {
            let removed = integration.remove_session_confirmed(&session).await;
            let removed = match removed {
                Ok(()) => Ok(()),
                Err(err) => Err(self.interrupted(err)),
            };
            let removed: Result<(), Error> = removed;
            if let Err(err) = removed {
                if is_shutdown(&err) {
                    return Err(err);
                }
                tracing::warn!(
                    review_id = %self.input.pull_request.review_id(),
                    reviewer = %request.reviewer.id,
                    error_code = %error_code(&err),
                    diagnostic = %diagnostic(Some(&err)),
                    "orphaned reviewer session cleanup failed"
                );
                return self
                    .notify(
                        &format!(
                            "orphan/{}/{}/{}",
                            request.reviewer.id, request.run, request.start
                        ),
                        Event::CleanupManual,
                        &orphaned_session_message(request, error_code(&err)),
                    )
                    .await;
            }
        }
        Ok(())
    }

    /// Reads the pull request again and fails with `head_changed` when its head moved.
    pub(crate) async fn current_pull_request(&self) -> Result<PullRequest, Error> {
        let repository = self.input.pull_request.repository.clone();
        let number = self.input.pull_request.number;
        let trigger = self.input.rule.trigger.clone();
        let current = self
            .fresh(|| {
                let integration = &*self.engine.integration;
                let repository = repository.clone();
                let trigger = trigger.clone();
                async move {
                    integration
                        .get_pull_request(&repository, number, &trigger)
                        .await
                }
            })
            .await?;
        still_wanted(&self.input.pull_request, &current)?;
        Ok(current)
    }

    /// Ends the review as a failure with the code of `cause`.
    ///
    /// A shutdown returns `cause` unchanged. A superseded review is reported quietly; any other
    /// cause is logged, and the scheduler retries or reports it.
    pub(crate) async fn fail(&mut self, cause: Error) -> Result<(), Error> {
        if is_shutdown(&cause) {
            return Err(cause);
        }
        let ids = self.all_slots();
        self.remove_sessions(&ids).await?;
        let code = error_code(&cause).to_owned();
        if superseded(&code) {
            self.notify(
                "superseded",
                Event::Superseded,
                &superseded_message(&self.input, &code),
            )
            .await?;
        } else {
            tracing::error!(
                review_id = %self.input.pull_request.review_id(),
                error_code = %code.as_str(),
                "review failed"
            );
        }
        if self.approved && !superseded(&code) {
            self.notify(
                "approved_failed",
                Event::ReviewStopped,
                &approved_review_failed_message(&self.input, &code),
            )
            .await?;
        }
        self.engine
            .store
            .complete_review(&self.workflow_id, RESULT_FAILURE, &code, None)
    }

    /// Returns the deadline stored under `key`, storing one on the first call.
    ///
    /// The first deadline is now plus the session timeout times the run attempts; the product
    /// wraps as Go's `time.Duration` does.
    #[expect(dead_code, reason = "called by S26")]
    pub(crate) fn deadline(&self, key: &str) -> Result<OffsetDateTime, Error> {
        if let Some(deadline) = self
            .engine
            .store
            .get_state::<Deadline>(&self.workflow_id, key)?
        {
            return Ok(deadline.0);
        }
        let limits = self.limits();
        let nanos = i64::try_from(milliseconds(limits.session_timeout_ms).whole_nanoseconds())
            .unwrap_or_default();
        let total = time::Duration::nanoseconds(nanos.wrapping_mul(limits.reviewer_run_attempts));
        let deadline = self.engine.clock.wall_now().saturating_add(total);
        self.engine
            .store
            .set_state(&self.workflow_id, key, &Deadline(deadline))?;
        Ok(deadline)
    }
}
