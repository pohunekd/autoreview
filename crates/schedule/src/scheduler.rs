//! Debounce, retry and dispatch decisions for review requests, and the persisted schedule record.

// Rust guideline compliant 2026-06-26

use std::collections::{BTreeMap, BTreeSet};

use autoreview_go_compat::go_time::format_rfc3339_seconds;
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime, UtcOffset, macros::datetime};

use crate::{Error, diagnostic::failure_explanation, phase};

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
#[serde(default, remote = "Self")]
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
    /// Marks a record that waits because no review host could start its review.
    #[serde(skip_serializing_if = "is_false")]
    pub awaits_host: bool,
    /// The failed run whose failure this record handled and notified last.
    ///
    /// A retarget keeps it, so a return to the base branch of that run notifies nothing.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub handled_workflow: String,
    /// The retry count before the failure of `handled_workflow` was handled.
    #[serde(skip_serializing_if = "is_zero")]
    pub handled_retries: i64,
}

autoreview_go_compat::object_only_serde!(Record);

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
            awaits_host: false,
            handled_workflow: String::new(),
            handled_retries: 0,
        }
    }
}

/// Reports whether a flag is unset; an unset flag is not written.
fn is_false(value: &bool) -> bool {
    !*value
}

/// Reports whether a count is zero; a zero count is not written.
fn is_zero(value: &i64) -> bool {
    *value == 0
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
        record.phase = phase::COMPLETED.to_owned();
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
    let fresh = previous.identity != target_identity || previous.phase == phase::WITHDRAWN;
    let mut r = if fresh {
        let mut blocking = previous.blocking_workflows.clone();
        if previous.phase == phase::RUNNING && !previous.workflow_id.is_empty() {
            blocking.push(previous.workflow_id.clone());
        }
        Record {
            identity: target_identity.clone(),
            phase: phase::WAITING.to_owned(),
            due_at: due_after(now, policy.debounce)?,
            blocking_workflows: blocking,
            attempt: 0,
            retries: 0,
            workflow_id: String::new(),
            error_code: String::new(),
            awaits_host: false,
            handled_workflow: String::new(),
            handled_retries: 0,
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
        if !fresh && r.phase == phase::RUNNING && r.attempt > latest_run.attempt {
            return Ok(Decision {
                record: r,
                dispatch: false,
                notify: false,
            });
        }
        if latest_run.active() {
            r.phase = phase::RUNNING.to_owned();
            r.workflow_id = latest_run.workflow_id.clone();
            r.attempt = latest_run.attempt;
            return Ok(Decision {
                record: r,
                dispatch: false,
                notify: false,
            });
        }
        if latest_run.result == "success" || latest_run.released {
            r.phase = phase::COMPLETED.to_owned();
            r.workflow_id = latest_run.workflow_id.clone();
            r.attempt = latest_run.attempt;
            return Ok(Decision {
                record: r,
                dispatch: false,
                notify: false,
            });
        }
        // Each terminal result is handled once; later polls keep its retry deadline.
        if fresh || r.workflow_id != latest_run.workflow_id || r.phase == phase::RUNNING {
            r.workflow_id = latest_run.workflow_id.clone();
            r.attempt = latest_run.attempt;
            let code = failure_code(&latest_run.failure);
            // A superseded run left nothing behind; the head is reviewed anew.
            if superseded(&code) {
                r.phase = phase::WAITING.to_owned();
                r.attempt = latest_run.attempt.wrapping_add(1);
                r.error_code = String::new();
                r.due_at = due_after(now, policy.debounce)?;
                return Ok(Decision {
                    record: r,
                    dispatch: false,
                    notify: false,
                });
            }
            // A failure handled before a retarget away and back gets the same state again, from
            // the same retry count, and no second notice. A running record follows a new run.
            let repeated = r.phase != phase::RUNNING
                && !r.handled_workflow.is_empty()
                && r.handled_workflow == latest_run.workflow_id;
            if repeated {
                r.retries = r.handled_retries;
            } else {
                r.handled_workflow = latest_run.workflow_id.clone();
                r.handled_retries = r.retries;
            }
            r = after_failure(r, &code, now, policy)?;
            return Ok(Decision {
                record: r,
                dispatch: false,
                notify: !repeated,
            });
        }
    } else if r.phase == phase::RUNNING {
        r.error_code = "workflow_state_unknown".to_owned();
        return Ok(Decision {
            record: r,
            dispatch: false,
            notify: false,
        });
    }
    if blocked || (r.phase != phase::WAITING && r.phase != phase::RETRY_WAIT) || now < r.due_at {
        return Ok(Decision {
            record: r,
            dispatch: false,
            notify: false,
        });
    }
    r.phase = phase::RUNNING.to_owned();
    Ok(Decision {
        record: r,
        dispatch: true,
        notify: false,
    })
}

/// What decides the review of a published head whose pull request targets another base branch.
#[derive(Debug, Clone, Copy)]
pub struct Retarget<'a> {
    /// The base branch that the pull request targets.
    pub base_branch: &'a str,
    /// The base branch of the stored input of each run of the head, by workflow ID.
    ///
    /// A run without an entry, whose stored review is gone, only raises the attempt.
    pub run_bases: &'a BTreeMap<String, String>,
    /// The workflow ID of the published review.
    pub published_workflow: &'a str,
}

/// Plans the review that a retarget of the published head of `target` asks for.
///
/// Returns `None` when a run of the head that was not superseded reviewed the current base
/// branch: [`plan_on_base`] then plans the head. Otherwise the ended runs of the head
/// count as no review; [`plan`] does not see them, and none of them stays a blocker. A record
/// that follows a running run of the head keeps that run's attempt, which [`plan`] matches
/// against the run history. Any other record gets an attempt above every run of the head,
/// skipping the attempt whose `workflow_id` is the published workflow. A record that starts
/// afresh keeps `handled_workflow` and `handled_retries`, so [`plan`] handles that failure
/// again without a notice when the pull request returns to the base branch of its run.
///
/// # Errors
///
/// Returns the error of [`plan`].
pub fn plan_retarget(
    previous: &Record,
    target: &Target,
    runs: &[Run],
    retarget: &Retarget<'_>,
    workflow_id: impl Fn(i64) -> String,
    now: OffsetDateTime,
    policy: &Policy,
) -> Result<Option<Decision>, Error> {
    let identity = target.identity();
    let mut ended = BTreeSet::new();
    for run in runs.iter().filter(|run| run.target.identity() == identity) {
        if !run.active() {
            ended.insert(run.workflow_id.as_str());
            // A superseded run reviewed nothing, so its base branch still needs a review.
            if superseded(&failure_code(&run.failure)) {
                continue;
            }
        }
        if retarget
            .run_bases
            .get(&run.workflow_id)
            .is_some_and(|base| base == retarget.base_branch)
        {
            return Ok(None);
        }
    }
    let attempt = free_attempt(runs, &identity, retarget.published_workflow, &workflow_id);
    let visible: Vec<Run> = runs
        .iter()
        .filter(|run| !ended.contains(run.workflow_id.as_str()))
        .cloned()
        .collect();
    let mut record = previous.clone();
    // `plan` drops a blocker when it sees its run ended; it does not see these runs.
    record
        .blocking_workflows
        .retain(|id| !ended.contains(id.as_str()));
    // A record that follows a run of the head starts afresh. The record of the retarget names
    // no workflow until it dispatches; it is kept, so its debounce runs once.
    let mut handled = None;
    if record.identity == identity && !record.workflow_id.is_empty() {
        handled = Some((record.handled_workflow, record.handled_retries));
        record = Record {
            blocking_workflows: record.blocking_workflows,
            ..Record::default()
        };
    }
    let mut decision = plan(record, target, &visible, now, policy)?;
    // The handled failure is never this record's workflow; it only stops a second notice.
    if let Some((workflow, retries)) = handled {
        decision.record.handled_workflow = workflow;
        decision.record.handled_retries = retries;
    }
    // A higher attempt on a running record would hide the end of its run from `plan`.
    let follows_run = !decision.dispatch
        && decision.record.phase == phase::RUNNING
        && visible
            .iter()
            .any(|run| run.active() && run.workflow_id == decision.record.workflow_id);
    if !follows_run {
        decision.record.attempt = decision.record.attempt.max(attempt);
    }
    Ok(Some(decision))
}

/// Plans a head on a base branch that one of its runs reviewed.
///
/// It plans when [`plan_retarget`] returned `None`. An ended run of the head that completed
/// another base branch, by the base branch of its stored input, does not complete this one:
/// [`plan`] does not see it. Once a run completed this base branch, or a run without a stored
/// base branch completed, as `apply_force` counts it, no ended run of another base branch is
/// seen. None of the hidden runs stays a blocker. A record that followed a hidden run is
/// completed like it, so [`plan`] handles the latest run of this base branch. A record
/// that waits or dispatches gets an attempt above every run of the head, skipping the attempt
/// whose `workflow_id` is the published workflow: a workflow ID names the head and the attempt,
/// not the base branch.
///
/// # Errors
///
/// Returns the error of [`plan`].
pub fn plan_on_base(
    previous: Record,
    target: &Target,
    runs: &[Run],
    retarget: &Retarget<'_>,
    workflow_id: impl Fn(i64) -> String,
    now: OffsetDateTime,
    policy: &Policy,
) -> Result<Decision, Error> {
    let identity = target.identity();
    let attempt = free_attempt(runs, &identity, retarget.published_workflow, &workflow_id);
    let ended: Vec<&Run> = runs
        .iter()
        .filter(|run| run.target.identity() == identity && !run.active())
        .collect();
    // A run without a stored base branch, or with an empty one, counts for this base branch.
    let elsewhere = |run: &Run| {
        retarget
            .run_bases
            .get(&run.workflow_id)
            .is_some_and(|base| !base.is_empty() && base != retarget.base_branch)
    };
    let completed = |run: &Run| run.result == "success" || run.released;
    // A head reviewed here is completed here; a later failure elsewhere does not reopen it.
    let reviewed_here = ended.iter().any(|run| completed(run) && !elsewhere(run));
    let other: BTreeSet<&str> = ended
        .iter()
        .filter(|run| elsewhere(run) && (reviewed_here || completed(run)))
        .map(|run| run.workflow_id.as_str())
        .collect();
    let visible: Vec<Run> = runs
        .iter()
        .filter(|run| !other.contains(run.workflow_id.as_str()))
        .cloned()
        .collect();
    let mut record = previous;
    record
        .blocking_workflows
        .retain(|id| !other.contains(id.as_str()));
    // A running record would wait for a run that `plan` does not see.
    if record.phase == phase::RUNNING && other.contains(record.workflow_id.as_str()) {
        record.phase = phase::COMPLETED.to_owned();
    }
    let mut decision = plan(record, target, &visible, now, policy)?;
    let waits = decision.dispatch
        || decision.record.phase == phase::WAITING
        || decision.record.phase == phase::RETRY_WAIT;
    if waits {
        decision.record.attempt = decision.record.attempt.max(attempt);
    }
    Ok(decision)
}

/// Returns the next attempt of `identity` whose workflow is not the `published` one.
///
/// The run of the published review may be pruned while its record is kept.
fn free_attempt(
    runs: &[Run],
    identity: &str,
    published: &str,
    workflow_id: &impl Fn(i64) -> String,
) -> i64 {
    let mut attempt = 0_i64;
    for run in runs.iter().filter(|run| run.target.identity() == identity) {
        attempt = attempt.max(run.attempt.wrapping_add(1));
    }
    if workflow_id(attempt) == published {
        attempt = attempt.wrapping_add(1);
    }
    attempt
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
    r.awaits_host = false;
    let index = usize::try_from(r.retries).ok();
    let delay = index.and_then(|i| policy.retry_delays.get(i));
    if retryable(&r.error_code)
        && let Some(delay) = delay
    {
        r.phase = phase::RETRY_WAIT.to_owned();
        r.due_at = due_after(now, *delay)?;
        r.retries = r.retries.wrapping_add(1);
        r.attempt = r.attempt.wrapping_add(1);
        return Ok(r);
    }
    r.phase = phase::FAILED.to_owned();
    Ok(r)
}

/// Keeps a dispatched record due while no review host can start its review.
///
/// The record waits in `retry_wait` at its due time, so the next pass dispatches it again. The
/// attempt and the retry budget stay as they are, because no review started.
pub fn wait_for_host(mut r: Record, code: &str) -> Record {
    r.phase = phase::RETRY_WAIT.to_owned();
    r.error_code = failure_code(code);
    r.awaits_host = true;
    r
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
            | "pohunek_session_lost"
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
            | "base_branch_changed"
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
    if record.phase == phase::RETRY_WAIT {
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

/// Returns the notice that reviews wait for a review host.
///
/// It names the review of `target` as the first one that waits, the `hosts` whose outage it
/// announces, and the host error code.
pub fn host_wait_notification(target: &Target, record: &Record, hosts: &[&str]) -> String {
    let description = failure_explanation(&record.error_code);
    let head: String = target
        .head_sha
        .chars()
        .take(NOTIFICATION_HEAD_CHARS)
        .collect();
    format!(
        "Reviews wait for a review host: no review host could start the review for {} at {} \
         (failed: {}). {} Each waiting review starts on the first discovery pass after a host \
         answers and uses no automatic retry. Diagnostic: {}.",
        target.key(),
        head,
        hosts.join(", "),
        description,
        record.error_code
    )
}

/// Adds `delay` to `now`, failing instead of panicking when the result leaves the range of `time`.
fn due_after(now: OffsetDateTime, delay: Duration) -> Result<OffsetDateTime, Error> {
    now.checked_add(delay).ok_or(Error::TimeOutOfRange)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use time::{Duration, OffsetDateTime};

    use super::{
        Policy, Record, Retarget, Run, Target, after_failure, notification, plan, plan_on_base,
        plan_retarget, reconcile_completed, retryable,
    };
    use crate::diagnostic::{diagnostic, failure_explanation};
    use crate::phase;

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
    fn retarget_is_reviewed_once_per_base_branch_and_ended_runs_never_block() {
        let (target, now, policy) = fixture();
        let identity = target.identity();
        let workflow = |attempt: i64| format!("wf-{attempt}");
        let ended = |attempt: i64, failure: &str| Run {
            target: target.clone(),
            workflow_id: workflow(attempt),
            attempt,
            status: "completed".to_owned(),
            result: if failure.is_empty() {
                "success"
            } else {
                "failure"
            }
            .to_owned(),
            failure: failure.to_owned(),
            ..Run::default()
        };
        let running = |attempt: i64| Run {
            target: target.clone(),
            workflow_id: workflow(attempt),
            attempt,
            status: "running".to_owned(),
            ..Run::default()
        };
        // The record that discovery holds for the head when the row starts.
        let record = |phase: &str, attempt: i64, blocking: &[&str]| Record {
            identity: identity.clone(),
            phase: phase.to_owned(),
            attempt,
            workflow_id: workflow(attempt),
            blocking_workflows: blocking.iter().map(|id| (*id).to_owned()).collect(),
            ..Record::default()
        };
        let bases: BTreeMap<String, String> = [
            ("wf-0", "main"),
            ("wf-1", "release"),
            ("wf-2", "release"),
            ("wf-3", "next"),
        ]
        .into_iter()
        .map(|(id, base)| (id.to_owned(), base.to_owned()))
        .collect();
        let superseded = "base_branch_changed: the base branch changed";
        let label_removed = "review_label_removed: the review label was removed";
        let session_lost = "pohunek_session_lost: the reviewer session was lost";
        let wrapper_invalid = "review_wrapper_invalid: the reviewer wrapper is invalid";
        let lost = || vec![ended(0, ""), ended(1, session_lost)];
        let stopped = || vec![ended(0, ""), ended(1, wrapper_invalid)];
        // The retry wf-2 of `release` ends with `failure`, then wf-3 approves `next`.
        let approved_on_next = |failure: &str| {
            vec![
                ended(0, ""),
                ended(1, session_lost),
                ended(2, failure),
                ended(3, ""),
            ]
        };
        // The record that discovery writes on `next`, the base branch of the published wf-3.
        let on_next = Record {
            workflow_id: workflow(3),
            handled_workflow: workflow(2),
            handled_retries: 1,
            ..record(phase::COMPLETED, 0, &[])
        };
        // Each pass runs one debounce after the one before and plans from its record; it gives
        // (planned by `plan_retarget`, phase, attempt, retries, blockers, dispatch, notify).
        type Pass = (
            Vec<Run>,
            &'static str,
            (bool, &'static str, i64, i64, Vec<&'static str>, bool, bool),
        );
        // A row names the error code of its failed routes; without one every route resolves.
        type Row = (
            &'static str,
            Record,
            &'static str,
            Option<&'static str>,
            Vec<Pass>,
        );
        let rows: Vec<Row> = vec![
            (
                "plain retarget",
                record(phase::COMPLETED, 0, &[]),
                "wf-0",
                None,
                vec![
                    (
                        vec![ended(0, "")],
                        "release",
                        (true, phase::WAITING, 1, 0, vec![], false, false),
                    ),
                    (
                        vec![ended(0, "")],
                        "release",
                        (true, phase::RUNNING, 1, 0, vec![], true, false),
                    ),
                ],
            ),
            (
                "retarget review still running",
                record(phase::RUNNING, 1, &[]),
                "wf-0",
                None,
                vec![(
                    vec![ended(0, ""), running(1)],
                    "release",
                    (false, phase::RUNNING, 1, 0, vec!["wf-1"], false, false),
                )],
            ),
            (
                "retarget review superseded at the same head",
                record(phase::RUNNING, 1, &["wf-1"]),
                "wf-0",
                None,
                vec![
                    (
                        vec![ended(0, ""), ended(1, label_removed)],
                        "release",
                        (true, phase::WAITING, 2, 0, vec![], false, false),
                    ),
                    (
                        vec![ended(0, ""), ended(1, label_removed)],
                        "release",
                        (true, phase::RUNNING, 2, 0, vec![], true, false),
                    ),
                ],
            ),
            (
                "second retarget while the retarget review runs",
                record(phase::RUNNING, 1, &["wf-1"]),
                "wf-0",
                None,
                vec![
                    (
                        vec![ended(0, ""), running(1)],
                        "next",
                        (true, phase::RUNNING, 1, 0, vec!["wf-1"], false, false),
                    ),
                    (
                        vec![ended(0, ""), ended(1, superseded)],
                        "next",
                        (true, phase::WAITING, 2, 0, vec![], false, false),
                    ),
                    (
                        vec![ended(0, ""), ended(1, superseded)],
                        "next",
                        (true, phase::RUNNING, 2, 0, vec![], true, false),
                    ),
                ],
            ),
            (
                "back to the base branch of the running retarget review, which then fails",
                record(phase::RUNNING, 1, &["wf-1"]),
                "wf-0",
                None,
                vec![
                    (
                        vec![ended(0, ""), running(1)],
                        "next",
                        (true, phase::RUNNING, 1, 0, vec!["wf-1"], false, false),
                    ),
                    (
                        vec![ended(0, ""), running(1)],
                        "release",
                        (false, phase::RUNNING, 1, 0, vec!["wf-1"], false, false),
                    ),
                    (
                        vec![ended(0, ""), ended(1, session_lost)],
                        "release",
                        (false, phase::RETRY_WAIT, 2, 1, vec![], false, true),
                    ),
                ],
            ),
            (
                "failed retry, retargeted away and back within the debounce",
                Record {
                    retries: 1,
                    ..record(phase::RUNNING, 1, &["wf-1"])
                },
                "wf-0",
                None,
                vec![
                    (
                        lost(),
                        "release",
                        (false, phase::RETRY_WAIT, 2, 2, vec![], false, true),
                    ),
                    (
                        lost(),
                        "next",
                        (true, phase::WAITING, 2, 0, vec![], false, false),
                    ),
                    (
                        lost(),
                        "release",
                        (false, phase::RETRY_WAIT, 2, 2, vec![], false, false),
                    ),
                    (
                        lost(),
                        "release",
                        (false, phase::RETRY_WAIT, 2, 2, vec![], false, false),
                    ),
                    (
                        lost(),
                        "release",
                        (false, phase::RETRY_WAIT, 2, 2, vec![], false, false),
                    ),
                    (
                        lost(),
                        "release",
                        (false, phase::RUNNING, 2, 2, vec![], true, false),
                    ),
                    // The retry is a new run, so its failure is notified.
                    (
                        vec![ended(0, ""), ended(1, session_lost), ended(2, session_lost)],
                        "release",
                        (false, phase::RETRY_WAIT, 3, 3, vec![], false, true),
                    ),
                ],
            ),
            (
                "failed retarget review, back by way of the published base branch",
                // The record that discovery writes on the published base branch.
                Record {
                    handled_workflow: workflow(1),
                    ..record(phase::COMPLETED, 0, &[])
                },
                "wf-0",
                None,
                vec![(
                    lost(),
                    "release",
                    (false, phase::RETRY_WAIT, 2, 1, vec![], false, false),
                )],
            ),
            (
                "stopped review, approved on a third base branch, back",
                on_next.clone(),
                "wf-3",
                None,
                vec![
                    (
                        approved_on_next(wrapper_invalid),
                        "release",
                        (false, phase::FAILED, 2, 1, vec![], false, false),
                    ),
                    (
                        approved_on_next(wrapper_invalid),
                        "release",
                        (false, phase::FAILED, 2, 1, vec![], false, false),
                    ),
                ],
            ),
            (
                "retrying review, approved on a third base branch, back",
                on_next.clone(),
                "wf-3",
                None,
                vec![
                    (
                        approved_on_next(session_lost),
                        "release",
                        (false, phase::RETRY_WAIT, 4, 2, vec![], false, false),
                    ),
                    (
                        approved_on_next(session_lost),
                        "release",
                        (false, phase::RETRY_WAIT, 4, 2, vec![], false, false),
                    ),
                    (
                        approved_on_next(session_lost),
                        "release",
                        (false, phase::RETRY_WAIT, 4, 2, vec![], false, false),
                    ),
                    (
                        approved_on_next(session_lost),
                        "release",
                        (false, phase::RUNNING, 4, 2, vec![], true, false),
                    ),
                ],
            ),
            (
                "retry by hand after the approval on a third base branch",
                // The record that `retry` writes on `release` after the stopped review.
                Record {
                    due_at: now,
                    workflow_id: workflow(2),
                    handled_workflow: workflow(2),
                    ..record(phase::RETRY_WAIT, 4, &[])
                },
                "wf-3",
                None,
                vec![(
                    approved_on_next(wrapper_invalid),
                    "release",
                    (false, phase::RUNNING, 4, 0, vec![], true, false),
                )],
            ),
            (
                "review of a third base branch approved after the return",
                Record {
                    handled_workflow: workflow(2),
                    handled_retries: 1,
                    ..record(phase::RUNNING, 3, &["wf-3"])
                },
                "wf-3",
                None,
                vec![(
                    approved_on_next(wrapper_invalid),
                    "release",
                    (false, phase::FAILED, 2, 1, vec![], false, false),
                )],
            ),
            (
                "stopped review, retargeted away and back within the debounce",
                record(phase::RUNNING, 1, &["wf-1"]),
                "wf-0",
                None,
                vec![
                    (
                        stopped(),
                        "release",
                        (false, phase::FAILED, 1, 0, vec![], false, true),
                    ),
                    (
                        stopped(),
                        "next",
                        (true, phase::WAITING, 2, 0, vec![], false, false),
                    ),
                    (
                        stopped(),
                        "release",
                        (false, phase::FAILED, 1, 0, vec![], false, false),
                    ),
                    (
                        stopped(),
                        "release",
                        (false, phase::FAILED, 1, 0, vec![], false, false),
                    ),
                ],
            ),
            (
                "route failure on the new base branch stops the review once",
                record(phase::RUNNING, 1, &["wf-1"]),
                "wf-0",
                Some("project_route_not_found"),
                vec![
                    (
                        lost(),
                        "release",
                        (false, phase::RETRY_WAIT, 2, 1, vec![], false, true),
                    ),
                    (
                        lost(),
                        "next",
                        (true, phase::WAITING, 2, 0, vec![], false, false),
                    ),
                    (
                        lost(),
                        "next",
                        (true, phase::FAILED, 2, 0, vec![], true, true),
                    ),
                    (
                        lost(),
                        "next",
                        (true, phase::FAILED, 2, 0, vec![], false, false),
                    ),
                    (
                        lost(),
                        "next",
                        (true, phase::FAILED, 2, 0, vec![], false, false),
                    ),
                ],
            ),
            (
                "retryable route failure on the new base branch waits for its backoff",
                record(phase::RUNNING, 1, &["wf-1"]),
                "wf-0",
                Some("github_request_failed"),
                vec![
                    (
                        lost(),
                        "release",
                        (false, phase::RETRY_WAIT, 2, 1, vec![], false, true),
                    ),
                    (
                        lost(),
                        "next",
                        (true, phase::WAITING, 2, 0, vec![], false, false),
                    ),
                    (
                        lost(),
                        "next",
                        (true, phase::RETRY_WAIT, 3, 1, vec![], true, true),
                    ),
                    (
                        lost(),
                        "next",
                        (true, phase::RETRY_WAIT, 4, 2, vec![], true, true),
                    ),
                    (
                        lost(),
                        "next",
                        (true, phase::RETRY_WAIT, 4, 2, vec![], false, false),
                    ),
                ],
            ),
            (
                "back to a reviewed base branch",
                record(phase::COMPLETED, 1, &[]),
                "wf-1",
                None,
                // The record names wf-0, the run that reviewed `main`, and its attempt.
                vec![(
                    vec![ended(0, ""), ended(1, "")],
                    "main",
                    (false, phase::COMPLETED, 0, 0, vec![], false, false),
                )],
            ),
            (
                "back to a reviewed base branch after a retried review elsewhere",
                // The record that discovery writes on `release`, where wf-2 published.
                Record {
                    workflow_id: workflow(2),
                    handled_workflow: workflow(1),
                    ..record(phase::COMPLETED, 0, &[])
                },
                "wf-2",
                None,
                vec![(
                    vec![ended(0, ""), ended(1, session_lost), ended(2, "")],
                    "main",
                    (false, phase::COMPLETED, 0, 0, vec![], false, false),
                )],
            ),
            (
                "back to a reviewed base branch after a superseded review elsewhere",
                Record {
                    workflow_id: workflow(2),
                    ..record(phase::COMPLETED, 0, &[])
                },
                "wf-2",
                None,
                vec![(
                    vec![ended(0, ""), ended(1, superseded), ended(2, "")],
                    "main",
                    (false, phase::COMPLETED, 0, 0, vec![], false, false),
                )],
            ),
            (
                "back to a reviewed base branch after a review elsewhere retried by hand",
                Record {
                    workflow_id: workflow(2),
                    handled_workflow: workflow(1),
                    ..record(phase::COMPLETED, 0, &[])
                },
                "wf-2",
                None,
                vec![(
                    vec![ended(0, ""), ended(1, wrapper_invalid), ended(2, "")],
                    "main",
                    (false, phase::COMPLETED, 0, 0, vec![], false, false),
                )],
            ),
            (
                "back to a base branch whose run was superseded",
                record(phase::COMPLETED, 1, &[]),
                "wf-1",
                None,
                vec![(
                    vec![ended(0, superseded), ended(1, "")],
                    "main",
                    (true, phase::WAITING, 2, 0, vec![], false, false),
                )],
            ),
            (
                "runs pruned, published workflow kept",
                Record::default(),
                "wf-0",
                None,
                vec![(
                    vec![],
                    "release",
                    (true, phase::WAITING, 1, 0, vec![], false, false),
                )],
            ),
        ];
        for (name, mut previous, published, route_failure, passes) in rows {
            let mut at = now;
            for (index, (runs, base_branch, want)) in passes.into_iter().enumerate() {
                let retarget = Retarget {
                    base_branch,
                    run_bases: &bases,
                    published_workflow: published,
                };
                let planned =
                    plan_retarget(&previous, &target, &runs, &retarget, workflow, at, &policy)
                        .expect("plan_retarget");
                let retargeted = planned.is_some();
                let mut decision = match planned {
                    Some(decision) => decision,
                    None => {
                        plan_on_base(previous, &target, &runs, &retarget, workflow, at, &policy)
                            .expect("plan_on_base")
                    }
                };
                // As discovery does: a resolved route names the dispatched run, and a failed
                // route goes through the retry policy and is notified.
                if decision.dispatch {
                    match route_failure {
                        None => decision.record.workflow_id = workflow(decision.record.attempt),
                        Some(code) => {
                            decision.record = after_failure(decision.record, code, at, &policy)
                                .expect("after_failure");
                            decision.notify = true;
                        }
                    }
                }
                let record = &decision.record;
                assert_eq!(
                    (
                        retargeted,
                        record.phase.as_str(),
                        record.attempt,
                        record.retries,
                        record.blocking_workflows.clone(),
                        decision.dispatch,
                        decision.notify,
                    ),
                    (
                        want.0,
                        want.1,
                        want.2,
                        want.3,
                        want.4.iter().map(|id| (*id).to_owned()).collect::<Vec<_>>(),
                        want.5,
                        want.6,
                    ),
                    "{name}, pass {index}"
                );
                previous = decision.record;
                at += policy.debounce;
            }
        }
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
