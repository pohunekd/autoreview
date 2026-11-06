//! Discovery: the poll that plans every candidate and queues the reviews that are due.
//!
//! Mirrors `cmd/autoreview/discovery.go:204-505`.

// Rust guideline compliant 2026-06-26

use std::collections::{BTreeMap, BTreeSet};

use autoreview_policy::Event;
use autoreview_schedule::scheduler::{Record, Run};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

use super::PHASE_RETRY_WAIT;
use crate::engine::{Clock, Engine, retry_step};
use crate::error::{Error, error_code};
use crate::integrations::Integrations;
use crate::messages::{go_elapsed_nanos, handoff_reminder_message};
use crate::model::{Candidate, RetryRequest};
use crate::store::{DiscoveryState, RESULT_SUCCESS, REVIEW_COMPLETED};

/// Schedule phase of a review identity that waits for its debounce.
const PHASE_WAITING: &str = "waiting";

/// Schedule phase of a review identity whose last run ended.
const PHASE_COMPLETED: &str = "completed";

/// Schedule phase of a review identity that gets no automatic attempt.
const PHASE_FAILED: &str = "failed";

/// Schedule phase of a review identity that left every trigger.
const PHASE_WITHDRAWN: &str = "withdrawn";

/// Start value of the latest attempt, below every stored attempt.
const NO_ATTEMPT: i64 = -1;

/// Error code of a retry for a pull request that is not a candidate.
const REVIEW_REQUEST_NOT_ACTIVE: &str = "review_request_not_active";

/// Error code of a retry whose head is not the current head.
const HEAD_CHANGED: &str = "head_changed";

/// Error code of a retry while a run of the pull request is active.
const REVIEW_ALREADY_RUNNING: &str = "review_already_running";

/// Error code of a retry of a head whose review succeeded or was released.
const REVIEW_ALREADY_COMPLETED: &str = "review_already_completed";

/// Read error codes of a pull request that left the candidates after the search.
const SKIPPABLE_CANDIDATE_CODES: [&str; 4] = [
    "pull_request_not_open",
    "pull_request_draft",
    "review_request_not_active",
    "review_label_removed",
];

impl<I: Integrations, C: Clock> Engine<I, C> {
    /// Returns the waits that no longer match their pull request and drops ended waits.
    ///
    /// A pull request missing from search is read directly first, because search lags behind.
    ///
    /// # Errors
    ///
    /// Returns the read error of a pull request unless it is one that ends the candidate.
    #[expect(dead_code, reason = "called by S23b")]
    async fn supersede_awaiting(
        &self,
        cancel: &CancellationToken,
        state: &mut DiscoveryState,
        present: &BTreeMap<String, Candidate>,
        own: &[Run],
    ) -> Result<Vec<String>, Error> {
        let finished: BTreeSet<&str> = own
            .iter()
            .filter(|run| run.status == REVIEW_COMPLETED)
            .map(|run| run.workflow_id.as_str())
            .collect();
        let mut superseded = Vec::new();
        let awaiting = state.awaiting.clone();
        for (key, record) in &awaiting {
            if finished.contains(record.workflow_id.as_str()) {
                state.awaiting.remove(key);
                continue;
            }
            if let Some(candidate) = present.get(key) {
                if record.matches(&candidate.pull_request) {
                    continue;
                }
            } else {
                let read = retry_step(self.step_policy(), &self.clock, cancel, || {
                    self.integration.get_pull_request(
                        &record.repository,
                        record.number,
                        &record.trigger,
                    )
                })
                .await;
                match read {
                    Ok(pr) if record.matches(&pr) => continue,
                    Ok(_) => {}
                    Err(err) if !skippable_candidate_error(&err) => return Err(err),
                    Err(_) => {}
                }
            }
            superseded.push(record.workflow_id.clone());
            state.awaiting.remove(key);
        }
        Ok(superseded)
    }

    /// Reminds the user once of handed-off notes that the author left waiting.
    ///
    /// The reminder is due when the author did not request the review again within the policy's
    /// reminder time. The hand-off of a pull request that is no longer open ends. A failed read
    /// is retried on the next pass, so it holds up no review.
    ///
    /// # Errors
    ///
    /// Returns `configuration_invalid` without a policy, the read error when `cancel` is
    /// cancelled, or the error of a notification.
    #[expect(dead_code, reason = "called by S23b")]
    async fn remind_handoffs(
        &self,
        cancel: &CancellationToken,
        state: &mut DiscoveryState,
        present: &BTreeMap<String, Candidate>,
        now: OffsetDateTime,
    ) -> Result<(), Error> {
        let after = self.policy()?.handoff_reminder();
        if after.is_zero() {
            return Ok(());
        }
        let mut due = Vec::new();
        for (key, record) in &state.published {
            if !record.awaits_author || record.reminded || present.contains_key(key) {
                continue;
            }
            let is_due = u128::try_from(go_elapsed_nanos(now, record.published_at))
                .is_ok_and(|elapsed| elapsed >= after.as_nanos());
            if is_due {
                due.push(key.clone());
            }
        }
        for key in due {
            let Some(mut record) = state.published.get(&key).cloned() else {
                continue;
            };
            let read = retry_step(self.step_policy(), &self.clock, cancel, || {
                self.integration
                    .read_pull_request_state(&record.repository, record.number)
            })
            .await;
            match read {
                Err(err) if cancel.is_cancelled() => return Err(err),
                Err(err) => {
                    tracing::warn!(
                        repository = %record.repository,
                        pull_request = record.number,
                        error_code = error_code(&err),
                        "notes hand-off unreadable"
                    );
                    continue;
                }
                Ok(current) if !current.open => {
                    record.awaits_author = false;
                }
                Ok(current) if current.requested => {
                    // Search lags behind; the next pass reviews it.
                    continue;
                }
                Ok(current) => {
                    let level = self.level(Event::HandoffReminder)?;
                    let message = handoff_reminder_message(
                        self.config.excerpt_characters,
                        &record,
                        &current,
                        now,
                    );
                    self.notify_now(cancel, level, &message).await?;
                    record.reminded = true;
                }
            }
            state.published.insert(key, record);
        }
        Ok(())
    }

    /// Turns the failed latest attempt of a pull request head into a due retry.
    ///
    /// # Errors
    ///
    /// Returns `review_request_not_active`, `head_changed`, `review_already_running` or
    /// `review_already_completed`, all permanent.
    #[expect(dead_code, reason = "called by S23b")]
    fn apply_force(
        &self,
        state: &mut DiscoveryState,
        present: &BTreeMap<String, Candidate>,
        own: &[Run],
        force: &RetryRequest,
        now: OffsetDateTime,
    ) -> Result<(), Error> {
        let key = format!("{}#{}", force.repository, force.number);
        let Some(candidate) = present.get(&key) else {
            return Err(Error::workflow(REVIEW_REQUEST_NOT_ACTIVE, true, None));
        };
        let target = candidate.pull_request.target();
        if !target.head_sha.starts_with(&force.head_sha) {
            return Err(Error::workflow(HEAD_CHANGED, true, None));
        }
        // The run history decides, not the schedule record: the record may still say running
        // from a poll that saw the run before it ended.
        let mut forced = Record {
            identity: target.identity(),
            phase: PHASE_RETRY_WAIT.to_owned(),
            due_at: now,
            ..Record::default()
        };
        // The forced record names the latest attempt of this head, so the planner treats its
        // failure as handled. The schedule record keeps the attempt counter when the history
        // no longer holds that attempt, so a retry never reuses an earlier workflow ID.
        if let Some(previous) = state.schedule.get(&key)
            && previous.identity == forced.identity
        {
            forced.attempt = previous.attempt.wrapping_add(1);
            forced.workflow_id = previous.workflow_id.clone();
        }
        let mut latest_attempt = NO_ATTEMPT;
        for run in own {
            if run.target.key() != key {
                continue;
            }
            if run.active() {
                return Err(Error::workflow(REVIEW_ALREADY_RUNNING, true, None));
            }
            if run.target.identity() != forced.identity {
                continue;
            }
            if run.result == RESULT_SUCCESS || run.released {
                return Err(Error::workflow(REVIEW_ALREADY_COMPLETED, true, None));
            }
            if run.attempt > latest_attempt {
                latest_attempt = run.attempt;
                forced.workflow_id = run.workflow_id.clone();
                forced.attempt = forced.attempt.max(run.attempt.wrapping_add(1));
            }
        }
        state.schedule.insert(key, forced);
        Ok(())
    }

    /// Keeps the discovery state bounded.
    ///
    /// Records of pull requests that left every trigger are withdrawn, and dropped once they
    /// ended and block nothing. Published decisions older than the retention are dropped.
    #[expect(dead_code, reason = "called by S23b")]
    fn prune_discovery(
        &self,
        state: &mut DiscoveryState,
        present: &BTreeMap<String, Candidate>,
        now: OffsetDateTime,
    ) {
        state.schedule.retain(|key, record| {
            if present.contains_key(key) {
                return true;
            }
            match record.phase.as_str() {
                PHASE_WAITING | PHASE_RETRY_WAIT => {
                    record.phase = PHASE_WITHDRAWN.to_owned();
                    true
                }
                PHASE_WITHDRAWN | PHASE_COMPLETED | PHASE_FAILED => {
                    !record.blocking_workflows.is_empty()
                }
                _ => true,
            }
        });
        let retention = self.config.published_retention.as_nanos();
        state.published.retain(|_, record| {
            !u128::try_from(go_elapsed_nanos(now, record.published_at))
                .is_ok_and(|elapsed| elapsed > retention)
        });
    }
}

/// Reports a pull request that changed between the search and its detail read.
///
/// It is simply not a candidate in this poll (`github.go:507-515`).
fn skippable_candidate_error(err: &Error) -> bool {
    SKIPPABLE_CANDIDATE_CODES.contains(&error_code(err))
}
