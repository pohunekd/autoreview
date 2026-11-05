//! Debounce, retry and dispatch decisions for review requests, and the persisted schedule record.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeSet;

use autoreview_go_compat::go_time::format_rfc3339_seconds;
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime, UtcOffset, macros::datetime};

use crate::{Error, diagnostic::failure_explanation};

/// Characters of the head SHA that a notification shows, as Go's `%.12s`.
const NOTIFICATION_HEAD_CHARS: usize = 12;

/// The pull request and head that a review request names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Target {
    /// The repository that owns the pull request.
    pub repository: String,
    /// The pull request number.
    pub number: i64,
    /// The head commit SHA of the pull request.
    pub head_sha: String,
    /// The review request identifier.
    pub request_id: i64,
}

impl Target {
    /// Returns the `repository#number` key shared by every head of one pull request.
    pub fn key(&self) -> String {
        format!("{}#{}", self.repository, self.number)
    }

    /// Returns the key, head SHA and review request ID that name one review.
    pub fn identity(&self) -> String {
        format!("{}@{}@{}", self.key(), self.head_sha, self.request_id)
    }
}

/// Timing of the debounce window and of the automatic retries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Policy {
    /// The debounce delay before a new head is dispatched.
    pub debounce: Duration,
    /// The delays between automatic retries.
    pub retry_delays: Vec<Duration>,
}

/// The persisted schedule entry for one review identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Record {
    /// The workflow IDs that currently block dispatch.
    #[serde(
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "autoreview_go_compat::null_default::deserialize"
    )]
    pub blocking_workflows: Vec<String>,
    /// The review identity this record tracks.
    pub identity: String,
    /// The current phase of the review.
    pub phase: String,
    /// Numbers the runs of this identity and keeps their workflow IDs unique.
    pub attempt: i64,
    /// Counts only the automatic retries after a failure.
    pub retries: i64,
    /// The workflow ID of the current run.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub workflow_id: String,
    /// The time after which the record is eligible for dispatch.
    #[serde(with = "autoreview_go_compat::go_time")]
    pub due_at: OffsetDateTime,
    /// The error code of the last failure.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub error_code: String,
}

impl Default for Record {
    fn default() -> Self {
        Self {
            blocking_workflows: Vec::new(),
            identity: String::new(),
            phase: String::new(),
            attempt: 0,
            retries: 0,
            workflow_id: String::new(),
            due_at: go_zero_time(),
            error_code: String::new(),
        }
    }
}

/// Go's zero `time.Time`, which Go writes for a record without a due time.
fn go_zero_time() -> OffsetDateTime {
    datetime!(0001-01-01 0:00 UTC)
}

/// A workflow run as reported by the run history.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Run {
    /// The pull request and head this run belongs to.
    pub target: Target,
    /// The workflow run identifier.
    pub workflow_id: String,
    /// The attempt number of the run.
    pub attempt: i64,
    /// The workflow status.
    pub status: String,
    /// The workflow result.
    pub result: String,
    /// The failure text of the run.
    pub failure: String,
    /// Whether the run was released.
    pub released: bool,
    /// Whether workflow cleanup is still pending.
    pub cleanup_pending: bool,
}

impl Run {
    /// Reports whether the run still holds its pull request.
    pub fn active(&self) -> bool {
        (self.status != "completed" && !self.released) || self.cleanup_pending
    }
}

/// The outcome of one scheduling pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    /// The updated schedule record.
    pub record: Record,
    /// Whether the scheduler should dispatch a new run.
    pub dispatch: bool,
    /// Whether the scheduler should notify about a failure.
    pub notify: bool,
}

/// Applies durable completion evidence to a record after its workflows were cleaned up.
pub fn reconcile_completed(mut record: Record, workflow_ids: &BTreeSet<String>) -> Record {
    if !record.workflow_id.is_empty() && workflow_ids.contains(&record.workflow_id) {
        record.phase = "completed".to_owned();
        record.error_code = String::new();
    }
    let mut retained = Vec::new();
    for workflow_id in record.blocking_workflows {
        if !workflow_ids.contains(&workflow_id) {
            retained.push(workflow_id);
        }
    }
    record.blocking_workflows = retained;
    record
}

/// Decides whether the review identity of `target` dispatches, waits, retries or completes.
///
/// Runs under the discovery object's exclusive lock. A missing result for a
/// dispatched run is uncertain, so the function never starts a duplicate run
/// for it.
///
/// # Errors
///
/// Returns `Error::TimeOutOfRange` when adding a delay to an instant leaves
/// the range that `time` can represent.
pub fn plan(
    previous: Record,
    target: &Target,
    runs: &[Run],
    now: OffsetDateTime,
    policy: &Policy,
) -> Result<Decision, Error> {
    let now = now.to_offset(UtcOffset::UTC);
    let target_key = target.key();
    let target_identity = target.identity();
    let fresh = previous.identity != target_identity || previous.phase == "withdrawn";
    let mut r = if fresh {
        let mut blocking = previous.blocking_workflows.clone();
        if previous.phase == "running" && !previous.workflow_id.is_empty() {
            blocking.push(previous.workflow_id.clone());
        }
        Record {
            identity: target_identity.clone(),
            phase: "waiting".to_owned(),
            due_at: due_after(now, policy.debounce)?,
            blocking_workflows: blocking,
            attempt: 0,
            retries: 0,
            workflow_id: String::new(),
            error_code: String::new(),
        }
    } else {
        previous
    };
    let mut blockers = BTreeSet::new();
    for id in &r.blocking_workflows {
        blockers.insert(id.clone());
    }
    let mut blocked = false;
    let mut latest: Option<&Run> = None;
    for run in runs {
        if run.target.key() != target_key {
            continue;
        }
        blocked = blocked || run.active();
        if run.active() {
            blockers.insert(run.workflow_id.clone());
        } else {
            blockers.remove(&run.workflow_id);
        }
        if run.target.identity() == target_identity {
            let should_replace = match latest {
                None => true,
                Some(current) => run.attempt > current.attempt,
            };
            if should_replace {
                latest = Some(run);
            }
        }
    }
    r.blocking_workflows = blockers.into_iter().collect();
    blocked = blocked || !r.blocking_workflows.is_empty();
    if let Some(latest_run) = latest {
        // The dispatch is durable but may not yet be visible in the run history.
        if !fresh && r.phase == "running" && r.attempt > latest_run.attempt {
            return Ok(Decision {
                record: r,
                dispatch: false,
                notify: false,
            });
        }
        if latest_run.active() {
            r.phase = "running".to_owned();
            r.workflow_id = latest_run.workflow_id.clone();
            r.attempt = latest_run.attempt;
            return Ok(Decision {
                record: r,
                dispatch: false,
                notify: false,
            });
        }
        if latest_run.result == "success" || latest_run.released {
            r.phase = "completed".to_owned();
            r.workflow_id = latest_run.workflow_id.clone();
            r.attempt = latest_run.attempt;
            return Ok(Decision {
                record: r,
                dispatch: false,
                notify: false,
            });
        }
        // Each terminal result is handled once; later polls keep its retry deadline.
        if fresh || r.workflow_id != latest_run.workflow_id || r.phase == "running" {
            r.workflow_id = latest_run.workflow_id.clone();
            r.attempt = latest_run.attempt;
            let code = failure_code(&latest_run.failure);
            // A superseded run left nothing behind; the head is reviewed anew.
            if superseded(&code) {
                r.phase = "waiting".to_owned();
                r.attempt = latest_run.attempt.wrapping_add(1);
                r.error_code = String::new();
                r.due_at = due_after(now, policy.debounce)?;
                return Ok(Decision {
                    record: r,
                    dispatch: false,
                    notify: false,
                });
            }
            r = after_failure(r, &code, now, policy)?;
            return Ok(Decision {
                record: r,
                dispatch: false,
                notify: true,
            });
        }
    } else if r.phase == "running" {
        r.error_code = "workflow_state_unknown".to_owned();
        return Ok(Decision {
            record: r,
            dispatch: false,
            notify: false,
        });
    }
    if blocked || (r.phase != "waiting" && r.phase != "retry_wait") || now < r.due_at {
        return Ok(Decision {
            record: r,
            dispatch: false,
            notify: false,
        });
    }
    r.phase = "running".to_owned();
    Ok(Decision {
        record: r,
        dispatch: true,
        notify: false,
    })
}

/// Records a failed run and schedules its retry when the failure is retryable and budget remains.
///
/// # Errors
///
/// Returns `Error::TimeOutOfRange` when adding a retry delay to the current
/// instant leaves the range that `time` can represent.
pub fn after_failure(
    mut r: Record,
    code: &str,
    now: OffsetDateTime,
    policy: &Policy,
) -> Result<Record, Error> {
    r.error_code = failure_code(code);
    let index = usize::try_from(r.retries).ok();
    let delay = index.and_then(|i| policy.retry_delays.get(i));
    if retryable(&r.error_code)
        && let Some(delay) = delay
    {
        r.phase = "retry_wait".to_owned();
        r.due_at = due_after(now, *delay)?;
        r.retries = r.retries.wrapping_add(1);
        r.attempt = r.attempt.wrapping_add(1);
        return Ok(r);
    }
    r.phase = "failed".to_owned();
    Ok(r)
}

/// Extracts the failure code from the failure text of a run.
///
/// Returns `workflow_failed` when the text has no leading lowercase identifier.
pub fn failure_code(failure: &str) -> String {
    let stripped = failure.strip_prefix("[500] ").unwrap_or(failure);
    let code = match stripped.split_once(':') {
        Some((before, _)) => before,
        None => stripped,
    };
    let trimmed = code.trim();
    if is_failure_code(trimmed) {
        trimmed.to_owned()
    } else {
        "workflow_failed".to_owned()
    }
}

/// Checks whether `code` matches the failure code pattern.
fn is_failure_code(code: &str) -> bool {
    let mut bytes = code.bytes();
    matches!(bytes.next(), Some(b'a'..=b'z'))
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Reports whether a failure code names a transient fault that the scheduler retries.
pub fn retryable(code: &str) -> bool {
    matches!(
        code,
        "pohunek_session_start_failed"
            | "pohunek_request_failed"
            | "pohunek_request_timeout"
            | "pohunek_base_fetch_failed"
            | "pohunek_base_fetch_timeout"
            | "pohunek_base_fetch_dns_failed"
            | "pohunek_base_fetch_ref_locked"
            | "github_request_failed"
    )
}

/// Reports whether a failure code means the run was overtaken by a newer state of the pull request.
pub fn superseded(code: &str) -> bool {
    matches!(
        code,
        "head_changed"
            | "review_label_removed"
            | "review_request_changed"
            | "review_request_not_active"
            | "pull_request_not_open"
            | "pull_request_draft"
    )
}

/// Returns the published notification for a failed review of `target`.
///
/// A `retry_wait` record names the next attempt and its due time; any other phase
/// says that no automatic attempt follows.
pub fn notification(target: &Target, record: &Record) -> String {
    let description = failure_explanation(&record.error_code);
    let head: String = target
        .head_sha
        .chars()
        .take(NOTIFICATION_HEAD_CHARS)
        .collect();
    let next = record.attempt.wrapping_add(1);
    if record.phase == "retry_wait" {
        let due = format_rfc3339_seconds(record.due_at);
        format!(
            "Review for {} at {}: {} Next automatic attempt: {} at {}. Diagnostic: {}.",
            target.key(),
            head,
            description,
            next,
            due,
            record.error_code
        )
    } else {
        format!(
            "Review for {} at {} stopped after attempt {}. {} No further automatic attempt is scheduled. Fix the cause, then retry this PR. Diagnostic: {}.",
            target.key(),
            head,
            next,
            description,
            record.error_code
        )
    }
}

/// Adds `delay` to `now`, failing instead of panicking when the result leaves the range of `time`.
fn due_after(now: OffsetDateTime, delay: Duration) -> Result<OffsetDateTime, Error> {
    now.checked_add(delay).ok_or(Error::TimeOutOfRange)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use time::{Duration, OffsetDateTime};

    use super::{Policy, Record, Run, Target, notification, plan, reconcile_completed, retryable};
    use crate::diagnostic::{diagnostic, failure_explanation};

    fn fixture() -> (Target, OffsetDateTime, Policy) {
        (
            Target {
                repository: "owner/repo".to_owned(),
                number: 42,
                head_sha: "a".repeat(40),
                request_id: 0,
            },
            OffsetDateTime::from_unix_timestamp(1000).expect("valid timestamp"),
            Policy {
                debounce: Duration::minutes(2),
                retry_delays: vec![
                    Duration::minutes(1),
                    Duration::minutes(5),
                    Duration::minutes(15),
                ],
            },
        )
    }

    #[test]
    fn rapid_pushes_debounce_to_latest_head() {
        let (mut target, mut now, policy) = fixture();
        let mut record = Record::default();
        for head in ["a", "b", "c", "d"] {
            target.head_sha = head.repeat(40);
            let decision = plan(record, &target, &[], now, &policy).expect("plan");
            assert!(
                !decision.dispatch,
                "dispatched while commits are still arriving"
            );
            record = decision.record;
            now += Duration::minutes(1);
        }
        let quiet = plan(record.clone(), &target, &[], now, &policy).expect("plan");
        assert!(!quiet.dispatch, "dispatched before quiet period");
        let decision =
            plan(record, &target, &[], now + Duration::minutes(1), &policy).expect("plan");
        assert!(
            decision.dispatch && decision.record.identity == target.identity(),
            "latest head not dispatched: {decision:?}"
        );
    }

    #[test]
    fn superseding_review_waits_for_confirmed_completion() {
        let (target, now, policy) = fixture();
        let mut old = target.clone();
        old.head_sha = "b".repeat(40);
        let mut runs = vec![Run {
            target: old,
            workflow_id: "old".to_owned(),
            status: "running".to_owned(),
            ..Run::default()
        }];
        let decision = plan(Record::default(), &target, &runs, now, &policy).expect("plan");
        let decision = plan(
            decision.record,
            &target,
            &runs,
            now + Duration::hours(1),
            &policy,
        )
        .expect("plan");
        assert!(!decision.dispatch, "overlapped superseded review");
        runs[0].status = "completed".to_owned();
        runs[0].result = "success".to_owned();
        let decision = plan(
            decision.record,
            &target,
            &runs,
            now + Duration::hours(1),
            &policy,
        )
        .expect("plan");
        assert!(
            decision.dispatch,
            "replacement did not start after cleanup completed"
        );
    }

    #[test]
    fn transient_failure_retries_with_persistent_bounded_backoff() {
        let (target, now, policy) = fixture();
        let mut runs = vec![Run {
            target: target.clone(),
            workflow_id: "run0".to_owned(),
            status: "completed".to_owned(),
            result: "failure".to_owned(),
            failure: "[500] pohunek_session_start_failed".to_owned(),
            ..Run::default()
        }];
        let mut decision = plan(Record::default(), &target, &runs, now, &policy).expect("plan");
        assert!(
            decision.record.phase == "retry_wait"
                && decision.record.attempt == 1
                && decision.notify,
            "legacy failure not recovered: {decision:?}"
        );
        let budget = i64::try_from(policy.retry_delays.len()).expect("delay count fits i64");
        for attempt in 1..=budget {
            // Simulate a service restart between scheduling and dispatch.
            let encoded = serde_json::to_string(&decision.record).expect("record serializes");
            let restored: Record = serde_json::from_str(&encoded).expect("record deserializes");
            decision = plan(
                restored.clone(),
                &target,
                &runs,
                restored.due_at - Duration::seconds(1),
                &policy,
            )
            .expect("plan");
            assert!(
                !decision.dispatch && !decision.notify && decision.record.due_at == restored.due_at,
                "retry deadline was reset"
            );
            decision =
                plan(decision.record, &target, &runs, restored.due_at, &policy).expect("plan");
            assert!(
                decision.dispatch && decision.record.attempt == attempt,
                "retry missing: {decision:?}"
            );
            decision.record.workflow_id = "new-run".to_owned();
            let lagged = plan(
                decision.record.clone(),
                &target,
                &runs,
                restored.due_at + Duration::seconds(1),
                &policy,
            )
            .expect("plan");
            assert!(
                !lagged.dispatch && !lagged.notify && lagged.record.attempt == attempt,
                "admin visibility lag duplicated dispatch"
            );
            runs = vec![Run {
                target: target.clone(),
                workflow_id: "new-run".to_owned(),
                attempt,
                status: "completed".to_owned(),
                result: "failure".to_owned(),
                failure: "[500] pohunek_session_start_failed".to_owned(),
                ..Run::default()
            }];
            decision = plan(
                decision.record,
                &target,
                &runs,
                restored.due_at + Duration::seconds(1),
                &policy,
            )
            .expect("plan");
        }
        assert!(
            decision.record.phase == "failed" && !decision.dispatch,
            "retry budget exceeded: {decision:?}"
        );
    }

    #[test]
    fn terminal_failure_success_and_unknown_history_never_redispatch() {
        let (target, now, policy) = fixture();
        let runs = [
            Run {
                target: target.clone(),
                workflow_id: "run".to_owned(),
                status: "completed".to_owned(),
                result: "failure".to_owned(),
                failure: "[500] review_wrapper_invalid".to_owned(),
                ..Run::default()
            },
            Run {
                target: target.clone(),
                workflow_id: "run".to_owned(),
                status: "completed".to_owned(),
                result: "success".to_owned(),
                ..Run::default()
            },
            Run {
                target: target.clone(),
                workflow_id: "run".to_owned(),
                status: "suspended".to_owned(),
                released: true,
                ..Run::default()
            },
        ];
        for run in runs {
            let history = [run.clone()];
            let decision = plan(Record::default(), &target, &history, now, &policy).expect("plan");
            let decision = plan(
                decision.record,
                &target,
                &history,
                now + Duration::hours(1),
                &policy,
            )
            .expect("plan");
            assert!(!decision.dispatch, "unexpected dispatch: {run:?}");
        }
        let record = Record {
            identity: target.identity(),
            phase: "running".to_owned(),
            workflow_id: "unknown".to_owned(),
            ..Record::default()
        };
        let decision = plan(record, &target, &[], now + Duration::hours(1), &policy).expect("plan");
        assert!(
            !decision.dispatch && decision.record.error_code == "workflow_state_unknown",
            "uncertain run repeated: {decision:?}"
        );
    }

    #[test]
    fn new_assignment_and_independent_pr() {
        let (mut target, now, policy) = fixture();
        let mut other = target.clone();
        other.number += 1;
        let runs = vec![Run {
            target: other,
            status: "running".to_owned(),
            ..Run::default()
        }];
        let record = plan(Record::default(), &target, &runs, now, &policy)
            .expect("plan")
            .record;
        let decision = plan(
            record.clone(),
            &target,
            &runs,
            now + policy.debounce,
            &policy,
        )
        .expect("plan");
        assert!(decision.dispatch, "another PR blocked this PR");
        let mut record = record;
        record.phase = "completed".to_owned();
        target.request_id += 1;
        let decision = plan(record, &target, &[], now, &policy).expect("plan");
        assert!(
            decision.record.phase == "waiting",
            "new review request was ignored"
        );
    }

    #[test]
    fn missing_or_unclean_previous_run_blocks_new_head() {
        let (target, now, policy) = fixture();
        let mut old = target.clone();
        old.head_sha = "b".repeat(40);
        let record = Record {
            identity: old.identity(),
            phase: "running".to_owned(),
            workflow_id: "old".to_owned(),
            ..Record::default()
        };
        let decision = plan(record, &target, &[], now, &policy).expect("plan");
        let decision = plan(
            decision.record,
            &target,
            &[],
            now + Duration::hours(1),
            &policy,
        )
        .expect("plan");
        assert!(!decision.dispatch, "missing previous run allowed overlap");
        let mut runs = vec![Run {
            target: old,
            workflow_id: "old".to_owned(),
            status: "completed".to_owned(),
            result: "failure".to_owned(),
            cleanup_pending: true,
            ..Run::default()
        }];
        let decision = plan(
            decision.record,
            &target,
            &runs,
            now + Duration::hours(1),
            &policy,
        )
        .expect("plan");
        assert!(!decision.dispatch, "unconfirmed cleanup allowed overlap");
        runs[0].cleanup_pending = false;
        let decision = plan(
            decision.record,
            &target,
            &runs,
            now + Duration::hours(1),
            &policy,
        )
        .expect("plan");
        assert!(
            decision.dispatch,
            "confirmed cleanup did not release the pending head"
        );
    }

    #[test]
    fn completed_evidence_releases_expired_previous_workflow() {
        let (target, now, policy) = fixture();
        let mut old = target.clone();
        old.head_sha = "b".repeat(40);

        let previous = Record {
            identity: old.identity(),
            phase: "running".to_owned(),
            workflow_id: "old".to_owned(),
            ..Record::default()
        };
        let previous = reconcile_completed(previous, &BTreeSet::from(["old".to_owned()]));
        assert!(
            previous.phase == "completed",
            "completed workflow remained {}",
            previous.phase
        );

        let decision = plan(previous, &target, &[], now, &policy).expect("plan");
        let decision = plan(
            decision.record,
            &target,
            &[],
            now + policy.debounce,
            &policy,
        )
        .expect("plan");
        assert!(
            decision.dispatch,
            "new review remained blocked after run history expired: {decision:?}"
        );

        let stuck = Record {
            identity: target.identity(),
            phase: "waiting".to_owned(),
            blocking_workflows: vec!["old".to_owned(), "unknown".to_owned()],
            due_at: now,
            ..Record::default()
        };
        let stuck = reconcile_completed(stuck, &BTreeSet::from(["old".to_owned()]));
        assert!(
            stuck.blocking_workflows.len() == 1 && stuck.blocking_workflows[0] == "unknown",
            "unexpected blockers after reconciliation: {:?}",
            stuck.blocking_workflows
        );
        assert!(
            !plan(stuck.clone(), &target, &[], now, &policy)
                .expect("plan")
                .dispatch,
            "unconfirmed workflow no longer blocked dispatch"
        );
        let stuck = reconcile_completed(stuck, &BTreeSet::from(["unknown".to_owned()]));
        assert!(
            plan(stuck, &target, &[], now, &policy)
                .expect("plan")
                .dispatch,
            "confirmed stale blockers did not release dispatch"
        );
        let empty = reconcile_completed(
            Record {
                phase: "waiting".to_owned(),
                ..Record::default()
            },
            &BTreeSet::from([String::new()]),
        );
        assert!(
            empty.phase == "waiting",
            "empty workflow id was treated as completion evidence"
        );
    }

    #[test]
    fn withdrawn_pending_request_can_be_observed_again() {
        let (target, now, policy) = fixture();
        let record = Record {
            identity: target.identity(),
            phase: "withdrawn".to_owned(),
            ..Record::default()
        };
        let decision = plan(record, &target, &[], now, &policy).expect("plan");
        assert!(
            decision.record.phase == "waiting" && !decision.dispatch,
            "unexpected reactivation: {decision:?}"
        );
    }

    // A superseded run left nothing behind, so the same head is reviewed again
    // once its trigger asks for it, under a new attempt.
    #[test]
    fn superseded_run_is_dispatched_again_without_a_notification() {
        let (target, now, policy) = fixture();
        let runs = vec![Run {
            target: target.clone(),
            workflow_id: "run0".to_owned(),
            status: "completed".to_owned(),
            result: "failure".to_owned(),
            failure: "[500] review_label_removed".to_owned(),
            ..Run::default()
        }];
        let decision = plan(Record::default(), &target, &runs, now, &policy).expect("plan");
        assert!(
            !decision.notify
                && !decision.dispatch
                && decision.record.phase == "waiting"
                && decision.record.attempt == 1,
            "superseded run handling: {decision:?}"
        );
        let decision = plan(
            decision.record,
            &target,
            &runs,
            now + policy.debounce,
            &policy,
        )
        .expect("plan");
        assert!(
            decision.dispatch && decision.record.attempt == 1,
            "the head was not reviewed again: {decision:?}"
        );
    }

    // Reviewing a head again after a supersede never spends the retry budget.
    #[test]
    fn superseded_redispatch_keeps_the_retry_budget() {
        let (target, now, policy) = fixture();
        let mut record = Record::default();
        for attempt in 0..3 {
            let runs = vec![Run {
                target: target.clone(),
                workflow_id: "superseded".to_owned(),
                attempt,
                status: "completed".to_owned(),
                result: "failure".to_owned(),
                failure: "[500] review_label_removed".to_owned(),
                ..Run::default()
            }];
            record.workflow_id = String::new();
            record = plan(record, &target, &runs, now, &policy)
                .expect("plan")
                .record;
        }
        let failed = vec![Run {
            target: target.clone(),
            workflow_id: "failed".to_owned(),
            attempt: record.attempt,
            status: "completed".to_owned(),
            result: "failure".to_owned(),
            failure: "[500] github_request_failed".to_owned(),
            ..Run::default()
        }];
        record.phase = "running".to_owned();
        let decision = plan(record, &target, &failed, now, &policy).expect("plan");
        assert!(
            decision.record.phase == "retry_wait" && decision.record.retries == 1,
            "supersedes consumed the retry budget: {:?}",
            decision.record
        );
    }

    #[test]
    fn diagnostics_do_not_expose_raw_errors() {
        let disk = std::io::Error::other("secret-token: No space left on device");
        assert!(
            diagnostic(Some(&disk)) == "disk_full",
            "disk failure not classified"
        );
        let raw = std::io::Error::other("token=secret prompt=private");
        let got = diagnostic(Some(&raw));
        assert!(
            got == "unclassified_cli_failure",
            "raw detail escaped: {got}"
        );
    }

    #[test]
    fn request_timeout_is_retryable_and_explained() {
        assert!(
            retryable("pohunek_request_timeout"),
            "a Pohunek request timeout must be retried"
        );
        assert!(
            failure_explanation("pohunek_request_timeout").contains("did not answer within"),
            "request timeout lacks a specific explanation"
        );
        let err = std::io::Error::other("exit status 1: pohunek error code request_timeout");
        let got = diagnostic(Some(&err));
        assert!(got == "timeout", "diagnostic = {got}");
    }

    #[test]
    fn failure_notifications_explain_cause_and_next_step() {
        let (target, now, _) = fixture();
        for phase in ["retry_wait", "failed"] {
            let message = notification(
                &target,
                &Record {
                    phase: phase.to_owned(),
                    error_code: "pohunek_base_fetch_timeout".to_owned(),
                    attempt: 2,
                    due_at: now,
                    ..Record::default()
                },
            );
            for want in [
                target.key().as_str(),
                "Git timed out",
                "keyring",
                "Diagnostic: pohunek_base_fetch_timeout",
            ] {
                assert!(
                    message.contains(want),
                    "notification lacks {want:?}: {message}"
                );
            }
            assert!(
                phase != "retry_wait" || message.contains("Next automatic attempt: 3 at"),
                "{message}"
            );
            assert!(
                phase != "failed" || message.contains("No further automatic attempt"),
                "{message}"
            );
        }
    }
}
