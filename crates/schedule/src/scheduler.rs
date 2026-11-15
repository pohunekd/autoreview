//! Debounce, retry and dispatch decisions for review requests, and the persisted schedule record.

// Rust guideline compliant 2026-06-26

use std::collections::{BTreeMap, BTreeSet};

use autoreview_go_compat::go_time::format_rfc3339_seconds;
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime, UtcOffset, macros::datetime};

use crate::{Error, diagnostic::failure_explanation, phase};

/// Characters of the head SHA that a notification shows: the first twelve.
const NOTIFICATION_HEAD_CHARS: usize = 12;

/// Failure code of a session start whose fetched pull request head is another commit.
pub const HEAD_FETCH_MISMATCH: &str = "pohunek_head_fetch_mismatch";

/// Error code of a review start that the author limit of its rule holds or skips.
pub const AUTHOR_LIMIT_REACHED: &str = "author_limit_reached";

/// Failure code of a reviewer run that its model account's rate or usage limit ended.
pub const REVIEWER_USAGE_LIMITED: &str = "reviewer_usage_limited";

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
    /// The delays between automatic retries of a review that a usage limit ended.
    pub usage_limit_delays: Vec<Duration>,
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
    /// The [`REVIEWER_USAGE_LIMITED`] runs of the current streak of runs that spend no retry.
    ///
    /// The record keeps the count, so pruned runs never shorten the streak.
    #[serde(skip_serializing_if = "is_zero")]
    pub usage_limited: i64,
    /// The [`HEAD_FETCH_MISMATCH`] runs of the current streak of runs that spend no retry.
    #[serde(skip_serializing_if = "is_zero")]
    pub fetch_mismatches: i64,
    /// `usage_limited` before the failure of `handled_workflow` was handled.
    #[serde(skip_serializing_if = "is_zero")]
    pub handled_usage_limited: i64,
    /// `fetch_mismatches` before the failure of `handled_workflow` was handled.
    #[serde(skip_serializing_if = "is_zero")]
    pub handled_fetch_mismatches: i64,
    /// Marks a dispatch that continues a review the author limit let through.
    ///
    /// A host wait keeps it, so the dispatch after the wait is not held as a new start.
    #[serde(skip_serializing_if = "is_false")]
    pub continues: bool,
    /// Marks a dispatch of a pull request whose author limit skip was notified.
    ///
    /// A host wait keeps it, so a skip after the wait sends no second notice.
    #[serde(skip_serializing_if = "is_false")]
    pub skip_notified: bool,
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
            usage_limited: 0,
            fetch_mismatches: 0,
            handled_usage_limited: 0,
            handled_fetch_mismatches: 0,
            continues: false,
            skip_notified: false,
        }
    }
}

/// Reports whether the author limit skip of the pull request of `record` was notified.
///
/// The record carries [`AUTHOR_LIMIT_REACHED`], or waits for a host after a dispatch that was so
/// notified. A withdrawn record proves nothing: a pull request that asks again is notified again.
fn skip_was_notified(record: &Record) -> bool {
    record.phase != phase::WITHDRAWN
        && (record.error_code == AUTHOR_LIMIT_REACHED
            || (record.awaits_host && record.skip_notified))
}

/// Reports whether a flag is unset; an unset flag is not written.
fn is_false(value: &bool) -> bool {
    !*value
}

/// Reports whether a count is zero; a zero count is not written.
fn is_zero(value: &i64) -> bool {
    *value == 0
}

/// The zero instant (0001-01-01 UTC) that a record without a due time stores.
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
    /// The login of the pull request author.
    pub author: String,
    /// The match pattern of the rule the run was dispatched under.
    pub rule: String,
    /// When the run was dispatched; `None` when its stored time does not parse.
    pub started_at: Option<OffsetDateTime>,
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
        // A new head of a pull request whose skip by the author limit was notified keeps the
        // code, so a skip of the new head sends no second notice.
        let skipped = skip_was_notified(&previous);
        Record {
            identity: target_identity.clone(),
            phase: phase::WAITING.to_owned(),
            due_at: due_after(now, policy.debounce)?,
            blocking_workflows: blocking,
            attempt: 0,
            retries: 0,
            workflow_id: String::new(),
            error_code: if skipped {
                AUTHOR_LIMIT_REACHED.to_owned()
            } else {
                String::new()
            },
            awaits_host: false,
            handled_workflow: String::new(),
            handled_retries: 0,
            usage_limited: 0,
            fetch_mismatches: 0,
            handled_usage_limited: 0,
            handled_fetch_mismatches: 0,
            continues: false,
            skip_notified: false,
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
            (r.usage_limited, r.fetch_mismatches) = (0, 0);
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
            // A failure handled before a retarget away and back gets the same state again, from
            // the same retry and streak counts, and no second notice. A running record follows a
            // new run. A superseded run is no failure, so it is never the handled one.
            let repeated = r.phase != phase::RUNNING
                && !r.handled_workflow.is_empty()
                && r.handled_workflow == latest_run.workflow_id;
            if !superseded(&code) {
                if repeated {
                    r.retries = r.handled_retries;
                    r.usage_limited = r.handled_usage_limited;
                    r.fetch_mismatches = r.handled_fetch_mismatches;
                } else {
                    r.handled_workflow = latest_run.workflow_id.clone();
                    r.handled_retries = r.retries;
                    r.handled_usage_limited = r.usage_limited;
                    r.handled_fetch_mismatches = r.fetch_mismatches;
                }
            }
            // The runs that spend no retry form one streak, counted on the record; any other
            // ending ends it.
            match code.as_str() {
                REVIEWER_USAGE_LIMITED => r.usage_limited = r.usage_limited.saturating_add(1),
                HEAD_FETCH_MISMATCH => r.fetch_mismatches = r.fetch_mismatches.saturating_add(1),
                _ => (r.usage_limited, r.fetch_mismatches) = (0, 0),
            }
            // A superseded run left nothing behind; the head is reviewed anew. So is a start
            // whose fetched head was another commit, until the retry budget is spent.
            let restarts = usize::try_from(r.fetch_mismatches).unwrap_or(usize::MAX);
            let again = superseded(&code)
                || (code == HEAD_FETCH_MISMATCH && restarts <= policy.retry_delays.len());
            if again {
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
            // A usage limit notifies on the first limited run of its streak and when it stops.
            let mut notify = !repeated;
            if code == REVIEWER_USAGE_LIMITED {
                r = after_usage_limit(r, now, policy)?;
                notify = notify && (r.usage_limited <= 1 || r.phase == phase::FAILED);
            } else {
                r = after_failure(r, &code, now, policy)?;
            }
            return Ok(Decision {
                record: r,
                dispatch: false,
                notify,
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
    if r.error_code == AUTHOR_LIMIT_REACHED {
        r.error_code = String::new();
    }
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
/// branch: [`plan_on_base`] then plans the head. Otherwise [`plan_unreviewed`] plans it.
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
    for run in runs.iter().filter(|run| run.target.identity() == identity) {
        // A superseded run reviewed nothing, so its base branch still needs a review.
        if !run.active() && superseded(&failure_code(&run.failure)) {
            continue;
        }
        if retarget
            .run_bases
            .get(&run.workflow_id)
            .is_some_and(|base| base == retarget.base_branch)
        {
            return Ok(None);
        }
    }
    plan_unreviewed(previous, target, runs, retarget, workflow_id, now, policy).map(Some)
}

/// Plans a head whose ended runs count as no review of the current base branch.
///
/// [`plan`] does not see the ended runs of the head, and none of them stays a blocker. A record
/// that follows a running run of the head keeps that run's attempt, which [`plan`] matches
/// against the run history. Any other record gets an attempt above every run of the head,
/// skipping the attempt whose `workflow_id` is the published workflow. A record that starts
/// afresh keeps `handled_workflow`, `handled_retries` and the streak counts with their
/// snapshots, so [`plan`] handles that failure again without a notice when the pull request
/// returns to the base branch of its run.
///
/// # Errors
///
/// Returns the error of [`plan`].
pub fn plan_unreviewed(
    previous: &Record,
    target: &Target,
    runs: &[Run],
    retarget: &Retarget<'_>,
    workflow_id: impl Fn(i64) -> String,
    now: OffsetDateTime,
    policy: &Policy,
) -> Result<Decision, Error> {
    let identity = target.identity();
    let ended: BTreeSet<&str> = runs
        .iter()
        .filter(|run| run.target.identity() == identity && !run.active())
        .map(|run| run.workflow_id.as_str())
        .collect();
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
    let mut kept = None;
    if record.identity == identity && !record.workflow_id.is_empty() {
        kept = Some(record.clone());
        record = Record {
            blocking_workflows: record.blocking_workflows,
            ..Record::default()
        };
    }
    let mut decision = plan(record, target, &visible, now, policy)?;
    // The handled failure is never this record's workflow; it only stops a second notice. The
    // streak counts stay, so a retarget neither resets nor repeats them.
    if let Some(kept) = kept {
        decision.record.handled_workflow = kept.handled_workflow;
        decision.record.handled_retries = kept.handled_retries;
        decision.record.handled_usage_limited = kept.handled_usage_limited;
        decision.record.handled_fetch_mismatches = kept.handled_fetch_mismatches;
        decision.record.usage_limited = kept.usage_limited;
        decision.record.fetch_mismatches = kept.fetch_mismatches;
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
    Ok(decision)
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

/// How many reviews one author may start in a window, and what a start over it gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthorBound {
    /// The most reviews the author may start in the window.
    pub reviews: usize,
    /// The length of the window.
    pub window: Duration,
    /// Skips a start over the bound instead of waiting for a free slot.
    pub skip: bool,
}

/// Holds a dispatch of a new start that the author's earlier `starts` leave no slot for.
///
/// Starts inside the window count; a start at the window's first instant has left it. Only a
/// dispatch from a `retry_wait` record of the same identity goes through over the bound, an
/// automatic retry or an operator `retry`, when it waits for no host or its delayed dispatch
/// was such a continuation; the dispatch keeps that mark in `continues`. Over the bound, a
/// `skip` bound fails the record with [`AUTHOR_LIMIT_REACHED`] and notifies, unless `previous`
/// proves that the skip of the pull request was notified; the dispatch keeps that proof in
/// `skip_notified`. Otherwise the record waits until a start leaves the window.
///
/// # Errors
///
/// Returns `Error::TimeOutOfRange` when the window leaves the range of `time`.
pub fn limit_author(
    mut decision: Decision,
    previous: &Record,
    starts: &[OffsetDateTime],
    bound: &AuthorBound,
    now: OffsetDateTime,
) -> Result<Decision, Error> {
    let continues = previous.identity == decision.record.identity
        && previous.phase == phase::RETRY_WAIT
        && (!previous.awaits_host || previous.continues);
    let notified = skip_was_notified(previous);
    if !decision.dispatch {
        return Ok(decision);
    }
    decision.record.continues = continues;
    decision.record.skip_notified = notified;
    if continues {
        return Ok(decision);
    }
    let opens = now.checked_sub(bound.window).ok_or(Error::TimeOutOfRange)?;
    let mut inside: Vec<OffsetDateTime> = starts.iter().copied().filter(|t| *t > opens).collect();
    if inside.len() < bound.reviews {
        return Ok(decision);
    }
    inside.sort();
    // A slot frees when the start that leaves `reviews - 1` inside the window leaves it.
    let leaving = (inside.len().checked_sub(bound.reviews)).and_then(|index| inside.get(index));
    let free_at = due_after(leaving.copied().unwrap_or(now), bound.window)?;
    decision.dispatch = false;
    decision.record.awaits_host = false;
    if bound.skip {
        decision.record.phase = phase::FAILED.to_owned();
        decision.record.error_code = AUTHOR_LIMIT_REACHED.to_owned();
        decision.notify = !notified;
    } else {
        decision.record.phase = phase::WAITING.to_owned();
        decision.record.due_at = free_at;
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

/// Records a run that a usage limit ended; `usage_limited` of `r` already counts it.
///
/// The review waits for the delay at that count in `usage_limit_delays` and keeps its retry
/// budget, so a later transient fault still gets every retry. With the delays spent it fails.
///
/// # Errors
///
/// Returns `Error::TimeOutOfRange` when adding the delay to the current
/// instant leaves the range that `time` can represent.
fn after_usage_limit(mut r: Record, now: OffsetDateTime, policy: &Policy) -> Result<Record, Error> {
    r.error_code = REVIEWER_USAGE_LIMITED.to_owned();
    r.awaits_host = false;
    let index = usize::try_from(r.usage_limited.saturating_sub(1)).ok();
    let delay = index.and_then(|i| policy.usage_limit_delays.get(i));
    let Some(delay) = delay else {
        r.phase = phase::FAILED.to_owned();
        return Ok(r);
    };
    r.phase = phase::RETRY_WAIT.to_owned();
    r.due_at = due_after(now, *delay)?;
    r.attempt = r.attempt.wrapping_add(1);
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
            | "pohunek_head_fetch_failed"
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

/// Returns the notice that the review of `target` stopped on a fetched head of another commit.
///
/// It names the expected head SHA and the `fetched` commit, or says that pohunek named none.
pub fn head_mismatch_notification(target: &Target, record: &Record, fetched: &str) -> String {
    let description = failure_explanation(&record.error_code);
    let head: String = target
        .head_sha
        .chars()
        .take(NOTIFICATION_HEAD_CHARS)
        .collect();
    let fetched = if fetched.is_empty() {
        "not reported"
    } else {
        fetched
    };
    format!(
        "Review for {} at {} stopped after attempt {}. {} Expected commit: {}. Fetched commit: \
         {}. No further automatic attempt is scheduled. Diagnostic: {}.",
        target.key(),
        head,
        record.attempt.wrapping_add(1),
        description,
        target.head_sha,
        fetched,
        record.error_code
    )
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
        AUTHOR_LIMIT_REACHED, AuthorBound, Decision, HEAD_FETCH_MISMATCH, Policy,
        REVIEWER_USAGE_LIMITED, Record, Retarget, Run, Target, after_failure,
        head_mismatch_notification, limit_author, notification, plan, plan_on_base, plan_retarget,
        reconcile_completed, retryable, wait_for_host,
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
                usage_limit_delays: vec![
                    Duration::minutes(15),
                    Duration::minutes(30),
                    Duration::minutes(60),
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

    /// Dispatches `record` and ends its run with `ending` (empty for a success).
    ///
    /// A new record is dispatched after its debounce. `host` makes the dispatch wait for a
    /// review host first, then the run ends with a head fetch mismatch.
    fn dispatch_and_end(
        record: Record,
        target: &Target,
        runs: &mut Vec<Run>,
        ending: &str,
        now: OffsetDateTime,
        policy: &Policy,
    ) -> Decision {
        let mut due = plan(record, target, runs, now, policy).expect("plan");
        if !due.dispatch {
            due = plan(due.record, target, runs, now + policy.debounce, policy).expect("plan");
        }
        assert!(due.dispatch, "not dispatched: {due:?}");
        let mut ending = ending;
        if ending == "host" {
            let waiting = wait_for_host(due.record, "pohunek_request_failed");
            due = plan(waiting, target, runs, now, policy).expect("plan");
            assert!(due.dispatch, "not dispatched after the host wait: {due:?}");
            ending = HEAD_FETCH_MISMATCH;
        }
        let mut record = due.record;
        record.workflow_id = format!("w{}", record.attempt);
        let result = if ending.is_empty() {
            "success"
        } else {
            "failure"
        };
        runs.push(Run {
            target: target.clone(),
            workflow_id: record.workflow_id.clone(),
            attempt: record.attempt,
            status: "completed".to_owned(),
            result: result.to_owned(),
            failure: ending.to_owned(),
            ..Run::default()
        });
        plan(record, target, runs, now, policy).expect("plan")
    }

    // A start whose fetched head is another commit is started again until the retry budget
    // is spent, then the head stops with a notice; any other ending but a usage limit resets
    // the count (#416).
    #[test]
    fn head_fetch_mismatch_restarts_until_the_retry_budget_then_stops() {
        const M: &str = HEAD_FETCH_MISMATCH;
        let wait = ("waiting", false);
        let stop = ("failed", true);
        // Each case: its name, the ending of each run, and the phase and notice after each.
        type Case<'a> = (&'a str, &'a [&'a str], &'a [(&'a str, bool)]);
        let cases: [Case<'_>; 7] = [
            (
                "stops after the retry budget",
                &[M, M, M, M],
                &[wait, wait, wait, stop],
            ),
            (
                "superseded then success",
                &[M, M, ""],
                &[wait, wait, ("completed", false)],
            ),
            (
                "fail then mismatch",
                &["github_request_failed", M, M, M, M],
                &[("retry_wait", true), wait, wait, wait, stop],
            ),
            (
                "a failure resets the count",
                &[M, M, "github_request_failed", M, M, M, M],
                &[wait, wait, ("retry_wait", true), wait, wait, wait, stop],
            ),
            (
                "a moved head resets the count and is not counted",
                &[M, M, "head_changed", "head_changed", M, M, M, M],
                &[wait, wait, wait, wait, wait, wait, wait, stop],
            ),
            (
                "another superseded code resets the count",
                &[M, M, M, "base_branch_changed", M, M, M, M],
                &[wait, wait, wait, wait, wait, wait, wait, stop],
            ),
            (
                "a host wait neither counts nor resets",
                &[M, "host", M, M],
                &[wait, wait, wait, stop],
            ),
        ];
        for (name, endings, want) in cases {
            let (target, mut now, policy) = fixture();
            let mut runs = Vec::new();
            let mut record = Record::default();
            for (ending, (phase, notify)) in endings.iter().zip(want.iter()) {
                now += Duration::hours(1);
                let decision = dispatch_and_end(record, &target, &mut runs, ending, now, &policy);
                assert!(
                    decision.record.phase == *phase && decision.notify == *notify,
                    "{name}: after {ending:?}: {decision:?}"
                );
                record = decision.record;
            }
            // A restart plans from the stored record again: no second notice, no new start.
            let again = plan(
                record.clone(),
                &target,
                &runs,
                now + Duration::days(1),
                &policy,
            )
            .expect("plan");
            assert!(!again.notify, "{name}: notified twice: {again:?}");
            if record.phase == "failed" {
                assert!(
                    !again.dispatch && again.record.error_code == M,
                    "{name}: the stopped head started again: {again:?}"
                );
                let failures = endings.iter().filter(|ending| retryable(ending)).count();
                assert_eq!(
                    usize::try_from(record.retries).ok(),
                    Some(failures),
                    "{name}: the mismatches spent the retries"
                );
            }
        }
    }

    /// Retargets the head of `record` from `main` to `dev` and back before it starts again.
    fn away_and_back(
        record: Record,
        target: &Target,
        runs: &[Run],
        now: OffsetDateTime,
        policy: &Policy,
    ) -> Decision {
        let bases: BTreeMap<String, String> = (runs.iter())
            .map(|run| (run.workflow_id.clone(), "main".to_owned()))
            .collect();
        let workflow_id = |attempt: i64| format!("w{attempt}");
        let retarget = |base_branch| Retarget {
            base_branch,
            run_bases: &bases,
            published_workflow: "published",
        };
        let (dev, main) = (retarget("dev"), retarget("main"));
        let away = plan_retarget(&record, target, runs, &dev, workflow_id, now, policy)
            .expect("plan")
            .expect("no run reviewed dev");
        plan_on_base(away.record, target, runs, &main, workflow_id, now, policy).expect("plan")
    }

    // A run that a usage limit ended waits on the usage limit delays and keeps the retry budget;
    // it notifies on the first limited run of its streak and when the delays are spent. A
    // fetch mismatch and a usage limit share one streak, so neither resets the other's count;
    // pruned runs and a retarget away and back keep both counts (#461).
    #[test]
    fn usage_limit_retries_on_its_own_delays_then_stops() {
        const U: &str = REVIEWER_USAGE_LIMITED;
        const G: &str = "github_request_failed";
        const M: &str = HEAD_FETCH_MISMATCH;
        const BACK: &str = "retarget away and back";
        let wait = |notify: bool, minutes: i64| ("retry_wait", notify, minutes);
        let stop = ("failed", true, 0);
        // Each case: its name, the ending of each run, the phase, notice and delay after each, and
        // the retries spent at the end.
        type Case<'a> = (&'a str, &'a [&'a str], &'a [(&'a str, bool, i64)], i64);
        let cases: [Case<'_>; 9] = [
            (
                "stops after its own delays",
                &[U, U, U, U],
                &[wait(true, 15), wait(false, 30), wait(false, 60), stop],
                0,
            ),
            (
                "success ends the wait",
                &[U, U, ""],
                &[wait(true, 15), wait(false, 30), ("completed", false, 0)],
                0,
            ),
            (
                "keeps the retry budget",
                &[G, U, G, U, U, G, G],
                &[
                    wait(true, 1),
                    wait(true, 15),
                    wait(true, 5),
                    wait(true, 15),
                    wait(false, 30),
                    wait(true, 15),
                    stop,
                ],
                3,
            ),
            (
                "a moved head resets the count",
                &[U, U, "head_changed", U],
                &[
                    wait(true, 15),
                    wait(false, 30),
                    ("waiting", false, 2),
                    wait(true, 15),
                ],
                0,
            ),
            (
                "a fetch mismatch keeps the usage count",
                &[U, M, U, M, U, M, U],
                &[
                    wait(true, 15),
                    ("waiting", false, 2),
                    wait(false, 30),
                    ("waiting", false, 2),
                    wait(false, 60),
                    ("waiting", false, 2),
                    stop,
                ],
                0,
            ),
            (
                "a usage limit keeps the mismatch count",
                &[M, U, M, U, M, U, M],
                &[
                    ("waiting", false, 2),
                    wait(true, 15),
                    ("waiting", false, 2),
                    wait(false, 30),
                    ("waiting", false, 2),
                    wait(false, 60),
                    stop,
                ],
                0,
            ),
            (
                "pruned runs keep the usage count",
                &[U, M, U, M, U, U],
                &[
                    wait(true, 15),
                    ("waiting", false, 2),
                    wait(false, 30),
                    ("waiting", false, 2),
                    wait(false, 60),
                    stop,
                ],
                0,
            ),
            (
                "pruned runs keep the mismatch count",
                &[M, M, M, M],
                &[
                    ("waiting", false, 2),
                    ("waiting", false, 2),
                    ("waiting", false, 2),
                    stop,
                ],
                0,
            ),
            (
                "a retarget away and back keeps the count",
                &[U, U, BACK, U, U],
                &[
                    wait(true, 15),
                    wait(false, 30),
                    wait(false, 30),
                    wait(false, 60),
                    stop,
                ],
                0,
            ),
        ];
        for (name, endings, want, retries) in cases {
            let (target, mut now, policy) = fixture();
            let mut runs = Vec::new();
            let mut record = Record::default();
            for (ending, (phase, notify, minutes)) in endings.iter().zip(want.iter()) {
                now += Duration::hours(1);
                // A pruned row keeps only the latest run, as if the store pruned the others.
                if name.starts_with("pruned") {
                    runs.drain(..runs.len().saturating_sub(1));
                }
                let decision = if *ending == BACK {
                    away_and_back(record, &target, &runs, now, &policy)
                } else {
                    dispatch_and_end(record, &target, &mut runs, ending, now, &policy)
                };
                let due = decision.record.due_at == now + Duration::minutes(*minutes);
                assert!(
                    decision.record.phase == *phase
                        && decision.notify == *notify
                        && (*minutes == 0 || due),
                    "{name}: after {ending:?}: {decision:?}"
                );
                record = decision.record;
            }
            // A restart plans from the stored record again: no second notice, no new start.
            let again = plan(record.clone(), &target, &runs, now, &policy).expect("plan");
            assert!(!again.notify, "{name}: notified twice: {again:?}");
            assert_eq!(record.retries, retries, "{name}: the retries spent");
        }
        assert!(!retryable(U), "the usage limit is not a transient fault");
        assert_ne!(failure_explanation(U), failure_explanation(""));
    }

    /// One row of the author limit table: what `limit_author` makes of a due dispatch.
    struct LimitCase {
        name: &'static str,
        previous: Record,
        skip: bool,
        /// The author's earlier starts, in hours before now.
        ago: &'static [i64],
        /// Dispatch, phase, notify, error code, and for a wait the hours until it is due.
        want: (bool, &'static str, bool, &'static str, i64),
    }

    // The author limit holds a new start over the bound until a start leaves the window, or
    // skips it with one notice; a retry of an admitted review is not held (#401).
    #[test]
    fn author_limit_holds_new_starts_over_the_bound() {
        let (target, now, policy) = fixture();
        let record = |phase: &str, attempt: i64, awaits_host: bool| Record {
            identity: target.identity(),
            phase: phase.to_owned(),
            attempt,
            awaits_host,
            due_at: now,
            ..Record::default()
        };
        let old_head = Record {
            identity: format!("{}@{}@0", target.key(), "b".repeat(40)),
            ..record(phase::FAILED, 0, false)
        };
        let skipped = Record {
            error_code: AUTHOR_LIMIT_REACHED.to_owned(),
            ..old_head.clone()
        };
        let route_failed = Record {
            error_code: "project_route_not_found".to_owned(),
            ..old_head.clone()
        };
        let carried = Record {
            error_code: AUTHOR_LIMIT_REACHED.to_owned(),
            ..record(phase::WAITING, 0, false)
        };
        let old_retry = Record {
            phase: phase::RETRY_WAIT.to_owned(),
            ..old_head.clone()
        };
        let retry_on_host = Record {
            continues: true,
            ..record(phase::RETRY_WAIT, 1, true)
        };
        let (run, wait, failed) = (phase::RUNNING, phase::WAITING, phase::FAILED);
        let code = AUTHOR_LIMIT_REACHED;
        #[rustfmt::skip]
        let cases = [
            LimitCase { name: "a first start under the bound",
                previous: record(wait, 0, false), skip: false, ago: &[1],
                want: (true, run, false, "", 0) },
            LimitCase { name: "at the bound it waits for the oldest start",
                previous: record(wait, 0, false), skip: false, ago: &[1, 5],
                want: (false, wait, false, "", 19) },
            LimitCase { name: "over the bound it waits for the start that frees a slot",
                previous: record(wait, 0, false), skip: false, ago: &[10, 1, 5],
                want: (false, wait, false, "", 19) },
            LimitCase { name: "starts outside the window do not count",
                previous: record(wait, 0, false), skip: false, ago: &[24, 30, 1],
                want: (true, run, false, "", 0) },
            LimitCase { name: "a start dated after now counts",
                previous: record(wait, 0, false), skip: false, ago: &[-1, 2],
                want: (false, wait, false, "", 22) },
            LimitCase { name: "a skip stops the head with a notice",
                previous: record(wait, 0, false), skip: true, ago: &[1, 2],
                want: (false, failed, true, code, 0) },
            LimitCase { name: "a skip after a skip of the pull request sends no notice",
                previous: skipped.clone(), skip: true, ago: &[1, 2],
                want: (false, failed, false, code, 0) },
            LimitCase { name: "a skip after a route failure notifies",
                previous: route_failed, skip: true, ago: &[1, 2],
                want: (false, failed, true, code, 0) },
            LimitCase { name: "an automatic retry is not held",
                previous: record(phase::RETRY_WAIT, 1, false), skip: true, ago: &[1, 2],
                want: (true, run, false, "", 0) },
            LimitCase { name: "a retry after a host wait is held",
                previous: record(phase::RETRY_WAIT, 0, true), skip: false, ago: &[1, 2],
                want: (false, wait, false, "", 22) },
            LimitCase { name: "a retry that waited for a host is not held",
                previous: retry_on_host.clone(), skip: false, ago: &[1, 2],
                want: (true, run, false, "", 0) },
            LimitCase { name: "a retry that waited for a host is not skipped",
                previous: retry_on_host, skip: true, ago: &[1, 2],
                want: (true, run, false, "", 0) },
            LimitCase { name: "a restart after a superseded run is held",
                previous: record(wait, 1, false), skip: false, ago: &[1, 2],
                want: (false, wait, false, "", 22) },
            LimitCase { name: "a retry wait of another head is held",
                previous: old_retry, skip: false, ago: &[1, 2],
                want: (false, wait, false, "", 22) },
            LimitCase { name: "a review after a retarget is held",
                previous: record(phase::COMPLETED, 1, false), skip: true, ago: &[1, 2],
                want: (false, failed, true, code, 0) },
            LimitCase { name: "a skip of a new head of a skipped pull request sends no notice",
                previous: carried.clone(), skip: true, ago: &[1, 2],
                want: (false, failed, false, code, 0) },
        ];
        let bound = |skip: bool| AuthorBound {
            reviews: 2,
            window: Duration::hours(24),
            skip,
        };
        for case in cases {
            let decision = Decision {
                record: record(phase::RUNNING, case.previous.attempt, false),
                dispatch: true,
                notify: false,
            };
            let starts: Vec<OffsetDateTime> = (case.ago.iter())
                .map(|hours| now - Duration::hours(*hours))
                .collect();
            let got = limit_author(decision, &case.previous, &starts, &bound(case.skip), now)
                .expect("limit");
            let (dispatch, phase, notify, error_code, due_in_hours) = case.want;
            let due = now + Duration::hours(due_in_hours);
            assert!(
                got.dispatch == dispatch
                    && got.record.phase == phase
                    && got.notify == notify
                    && got.record.error_code == error_code
                    && !got.record.awaits_host
                    && (phase != wait || got.record.due_at == due),
                "{}: {got:?}",
                case.name
            );
        }
        // A planned dispatch passes through when it is not a dispatch.
        let running = Decision {
            record: record(phase::RUNNING, 0, false),
            dispatch: false,
            notify: false,
        };
        let starts = [now, now];
        let got = limit_author(
            running.clone(),
            &Record::default(),
            &starts,
            &bound(true),
            now,
        );
        assert_eq!(got.expect("limit"), running);
        // The window slides: the held head is due when the oldest start leaves the window.
        let starts = [now - Duration::hours(23), now - Duration::hours(1)];
        let planned = plan(record(wait, 0, false), &target, &[], now, &policy).expect("plan");
        let held = limit_author(
            planned,
            &record(wait, 0, false),
            &starts,
            &bound(false),
            now,
        );
        let held = held.expect("limit").record;
        let early = plan(
            held.clone(),
            &target,
            &[],
            now + Duration::minutes(59),
            &policy,
        );
        assert!(
            !early.expect("plan").dispatch,
            "dispatched before a slot freed"
        );
        let later = now + Duration::hours(1);
        let due = plan(held.clone(), &target, &[], later, &policy).expect("plan");
        let admitted = limit_author(due, &held, &starts, &bound(false), later).expect("limit");
        assert!(
            admitted.dispatch,
            "the freed slot was not used: {admitted:?}"
        );
        // A new head of a skipped pull request carries the code through its debounce, also to a
        // head pushed during it or during a host wait, and a dispatch drops it. A withdrawn
        // record carries nothing, so a pull request that asks again is notified again (#423).
        let next = Target {
            head_sha: "c".repeat(40),
            ..target.clone()
        };
        let free = &starts[..1];
        let fresh = plan(skipped.clone(), &target, &[], now, &policy).expect("plan");
        let pushed = plan(fresh.record.clone(), &next, &[], now, &policy).expect("plan");
        let due = plan(fresh.record.clone(), &target, &[], later, &policy).expect("plan");
        let slot = limit_author(due.clone(), &fresh.record, free, &bound(true), later);
        let on_host = wait_for_host(slot.expect("limit").record, "pohunek_request_failed");
        let pushed_on_host = plan(on_host, &next, &[], later, &policy).expect("plan");
        let withdrawn = Record {
            phase: phase::WITHDRAWN.to_owned(),
            ..fresh.record.clone()
        };
        let asked = plan(withdrawn, &target, &[], now, &policy).expect("plan");
        assert!(
            fresh.record.error_code == code
                && pushed.record.error_code == code
                && due.dispatch
                && due.record.error_code.is_empty()
                && pushed_on_host.record.error_code == code
                && asked.record.error_code.is_empty(),
            "the skip was not carried: {pushed:?} {pushed_on_host:?} {asked:?}"
        );
        // A dispatch with a free slot that meets a host outage is judged again once the window
        // is full: a retry or an operator `retry` starts with no notice, a new start is held or
        // skipped, and a later head of a skipped pull request sends no second notice (#423). A
        // restart after a superseded continuation keeps a stale `continues` until its dispatch.
        let stale = Record {
            continues: true,
            ..record(wait, 1, false)
        };
        #[rustfmt::skip]
        let host_waits = [
            ("a retry that waits", record(phase::RETRY_WAIT, 1, false), false, (true, false)),
            ("a retry that skips", record(phase::RETRY_WAIT, 1, false), true, (true, false)),
            ("a new start", record(wait, 0, false), true, (false, true)),
            ("a restart with a stale continuation mark", stale, false, (false, false)),
            ("a later head of a skipped pull request", carried, true, (false, false)),
        ];
        for (name, previous, skip, (dispatch, notify)) in host_waits {
            let due = plan(previous.clone(), &target, &[], now, &policy).expect("plan");
            let first = limit_author(due, &previous, free, &bound(skip), now).expect("limit");
            let waiting = wait_for_host(first.record.clone(), "pohunek_request_failed");
            let back = plan(waiting.clone(), &target, &[], now, &policy).expect("plan");
            let second = limit_author(back, &waiting, &starts, &bound(skip), now).expect("limit");
            assert!(
                first.dispatch
                    && !first.notify
                    && second.dispatch == dispatch
                    && second.notify == notify,
                "{name}: {first:?} {second:?}"
            );
        }
        // A skip notice is the review_stopped text with the explanation of the code.
        let stopped = Record {
            error_code: AUTHOR_LIMIT_REACHED.to_owned(),
            ..record(failed, 0, false)
        };
        let text = notification(&target, &stopped);
        assert!(
            text.contains("as its `author_limit` allows") && text.ends_with(&format!("{code}.")),
            "{text}"
        );
    }

    // The mismatch count belongs to one head: a new head starts its own, a retarget keeps it,
    // and the stop notice names both commits (#416).
    #[test]
    fn head_fetch_mismatch_count_follows_the_head() {
        const M: &str = HEAD_FETCH_MISMATCH;
        let wait = ("waiting", false);
        let stop = ("failed", true);

        // A new head seen by discovery is a new identity and starts its own count.
        let (target, mut now, policy) = fixture();
        let mut runs = Vec::new();
        let mut record = Record::default();
        for _ in 0..3 {
            now += Duration::hours(1);
            record = dispatch_and_end(record, &target, &mut runs, M, now, &policy).record;
        }
        let mut moved = target.clone();
        moved.head_sha = "b".repeat(40);
        for want in [wait, wait, wait, stop] {
            now += Duration::hours(1);
            let decision = dispatch_and_end(record, &moved, &mut runs, M, now, &policy);
            assert!(
                (decision.record.phase.as_str(), decision.notify) == want,
                "new head: {decision:?}"
            );
            record = decision.record;
        }

        // A retarget while the head waits does not reset the count of the head.
        let (target, mut now, policy) = fixture();
        let mut runs = Vec::new();
        let mut record = Record::default();
        for _ in 0..3 {
            now += Duration::hours(1);
            record = dispatch_and_end(record, &target, &mut runs, M, now, &policy).record;
        }
        let mut run_bases: BTreeMap<String, String> = runs
            .iter()
            .map(|run| (run.workflow_id.clone(), "main".to_owned()))
            .collect();
        let workflow_id = |attempt: i64| format!("w{attempt}");
        let planned = {
            let retarget = Retarget {
                base_branch: "dev",
                run_bases: &run_bases,
                published_workflow: "published",
            };
            now += Duration::hours(1);
            let first = plan_retarget(
                &record,
                &target,
                &runs,
                &retarget,
                workflow_id,
                now,
                &policy,
            )
            .expect("plan")
            .expect("a mismatch on another base branch reviewed nothing here");
            now += policy.debounce;
            let waited = &first.record;
            plan_retarget(waited, &target, &runs, &retarget, workflow_id, now, &policy)
                .expect("plan")
                .expect("a mismatch on another base branch reviewed nothing here")
        };
        assert!(planned.dispatch, "retarget not dispatched: {planned:?}");
        let mut record = planned.record;
        record.workflow_id = format!("w{}", record.attempt);
        runs.push(Run {
            target: target.clone(),
            workflow_id: record.workflow_id.clone(),
            attempt: record.attempt,
            status: "completed".to_owned(),
            result: "failure".to_owned(),
            failure: M.to_owned(),
            ..Run::default()
        });
        run_bases.insert(record.workflow_id.clone(), "dev".to_owned());
        let retarget = Retarget {
            base_branch: "dev",
            run_bases: &run_bases,
            published_workflow: "published",
        };
        let on_dev = plan_retarget(
            &record,
            &target,
            &runs,
            &retarget,
            workflow_id,
            now,
            &policy,
        )
        .expect("plan");
        assert!(
            on_dev.is_none(),
            "the mismatch on dev was skipped: {on_dev:?}"
        );
        let decision = plan_on_base(record, &target, &runs, &retarget, workflow_id, now, &policy)
            .expect("plan");
        assert!(
            decision.record.phase == "failed" && decision.notify,
            "the retargeted head did not stop: {decision:?}"
        );
        let fetched = "c".repeat(40);
        let text = head_mismatch_notification(&target, &decision.record, &fetched);
        assert!(
            text.contains(&target.head_sha) && text.contains(&fetched) && text.contains(M),
            "{text}"
        );
        let unknown = head_mismatch_notification(&target, &decision.record, "");
        assert!(
            unknown.contains("Fetched commit: not reported."),
            "{unknown}"
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
}
