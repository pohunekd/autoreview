//! Review statistics: the current state and a daily history, read from the store only.
//!
//! One review row is one attempt: a retry is a new row, so `started` and `failed` count
//! attempts. The counts come from the `reviews`, `review_steps` and `review_step_runs` tables,
//! the rollup of the pruned reviews and of the stops, and the schedule records of the
//! discovery state; nothing asks GitHub or pohunek.

// Rust guideline compliant 2026-06-26

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use autoreview_go_compat::go_time::{format_rfc3339_seconds, parse_rfc3339};
use autoreview_schedule::phase;
use autoreview_schedule::scheduler::{Record, superseded};
use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime, UtcOffset};
use tokio_util::sync::CancellationToken;

use crate::discovery::poll::live;
use crate::engine::{Clock, Engine};
use crate::error::Error;
use crate::integrations::Integrations;
use crate::model::review::APPROVAL_KIND_APPROVE;
use crate::model::{Decision, Outcome, ReviewOutcome, Verification};
use crate::publish::plan::PublicationPlan;
use crate::review::input::ArtifactOutcome;
use crate::store::rollup::{
    COUNTER_BLOCKED_ALONE, COUNTER_CHECKED, COUNTER_DEMOTED, COUNTER_DISAGREED,
    COUNTER_FIRST_PUBLICATION, COUNTER_MULTI_REVIEWER, DIMENSION_DAY, DIMENSION_DISAGREEMENT,
    DIMENSION_DISAGREEMENT_REVIEWER, DIMENSION_FAILURE, DIMENSION_REPOSITORY, DIMENSION_REQUEST,
    DIMENSION_REQUEST_ID, DIMENSION_REVIEWER, DIMENSION_RULE, DIMENSION_STEP, DIMENSION_VERIFIER,
    PIPELINE_DIMENSIONS,
};
use crate::store::{
    EndedStepRun, RESULT_FAILURE, RESULT_SUCCESS, REVIEW_AWAITING, REVIEW_COMPLETED, REVIEW_QUEUED,
    REVIEW_RUNNING, Rollup, RollupCount, RollupSample, StatsSnapshot, StepOutcome, StepRun,
    StoredReview, VerifierSteps,
};
use crate::verify::{VERDICT_CONFIRMED, VERDICT_NOT_BLOCKING, VERDICT_REFUTED};

/// Version of the statistics document; a changed field gets a new version.
pub const STATS_SCHEMA_VERSION: i64 = 1;

/// Most UTC days one history covers, so one document holds a bounded number of days.
pub const STATS_RANGE_MAX_DAYS: i64 = 366;

/// Error code of a `since` or `until` that is not a valid range of UTC days.
pub const STATS_RANGE_INVALID: &str = "stats_range_invalid";

/// Error code of a range longer than `STATS_RANGE_MAX_DAYS`.
pub const STATS_RANGE_TOO_LONG: &str = "stats_range_too_long";

/// Byte length of a UTC day written as `YYYY-MM-DD`.
const DAY_TEXT_LEN: usize = 10;

/// Byte offsets of the two hyphens in `YYYY-MM-DD`.
const DAY_HYPHENS: [usize; 2] = [4, 7];

/// Percentile of the median publication time.
const MEDIAN_PERCENT: usize = 50;

/// Percentile of the slow publication time.
const P90_PERCENT: usize = 90;

/// Counter of the `failure` dimension, the field of `FailedCount`.
const COUNTER_FAILED: &str = "failed";

/// A range of whole UTC days, both ends included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatsRange {
    /// First day of the range.
    pub(crate) since: Date,
    /// Last day of the range.
    pub(crate) until: Date,
    /// First instant of the range, midnight UTC of `since`.
    pub(crate) start: OffsetDateTime,
    /// First instant after the range, midnight UTC after `until`.
    pub(crate) end: OffsetDateTime,
}

impl StatsRange {
    /// Reads the range of `since` and `until`; `until` is `today` when absent.
    ///
    /// Returns `None` without `since`: the document then holds the current state only.
    ///
    /// # Errors
    ///
    /// Returns `stats_range_invalid` for a day that is not `YYYY-MM-DD`, for `since` or
    /// `until` after `today`, for `until` without `since` and for `until` before `since`, and
    /// `stats_range_too_long` for a range of more than `STATS_RANGE_MAX_DAYS` days.
    pub fn parse(
        since: Option<&str>,
        until: Option<&str>,
        today: Date,
    ) -> Result<Option<Self>, Error> {
        let Some(since) = since else {
            return match until {
                Some(_) => Err(range_invalid("until needs since")),
                None => Ok(None),
            };
        };
        let since = parse_day(since)?;
        if since > today {
            return Err(range_invalid("since is after today"));
        }
        let until = match until {
            Some(text) => parse_day(text)?,
            None => today,
        };
        if until > today {
            return Err(range_invalid("until is after today"));
        }
        if until < since {
            return Err(range_invalid("until is before since"));
        }
        if (until - since).whole_days() >= STATS_RANGE_MAX_DAYS {
            return Err(Error::workflow(STATS_RANGE_TOO_LONG, true, None));
        }
        let Some(after) = until.next_day() else {
            return Err(range_invalid("until is the last representable day"));
        };
        Ok(Some(Self {
            since,
            until,
            start: since.midnight().assume_utc(),
            end: after.midnight().assume_utc(),
        }))
    }

    /// Reports whether `day` lies inside the range.
    fn contains(&self, day: Date) -> bool {
        self.since <= day && day <= self.until
    }
}

/// Reads one UTC day written as `YYYY-MM-DD`, digits and hyphens only.
///
/// # Errors
///
/// Returns `stats_range_invalid` for other text and for a day that does not exist.
pub fn parse_day(text: &str) -> Result<Date, Error> {
    let bytes = text.as_bytes();
    let shaped = bytes.len() == DAY_TEXT_LEN
        && bytes.iter().enumerate().all(|(index, byte)| {
            if DAY_HYPHENS.contains(&index) {
                *byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        });
    if !shaped {
        return Err(range_invalid("a day is not YYYY-MM-DD"));
    }
    let format = time::macros::format_description!("[year]-[month]-[day]");
    Date::parse(text, &format).map_err(|_| range_invalid("a day does not exist"))
}

/// Returns the `stats_range_invalid` error with its reason.
fn range_invalid(reason: &str) -> Error {
    Error::workflow(STATS_RANGE_INVALID, true, Some(reason.to_owned().into()))
}

/// The statistics document that `autoreview stats --json` prints and `review_stats` returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StatsDocument {
    /// Version of this document's fields, `STATS_SCHEMA_VERSION`.
    pub schema_version: i64,
    /// When the document was computed, RFC 3339 in UTC without a fraction.
    pub generated_at: String,
    /// The queued and running reviews and the waits without a decision.
    pub current: CurrentState,
    /// The daily counts of the range; `null` without `since`.
    pub history: Option<History>,
}

/// Counts of the queued and running reviews and the waits without a decision, by status.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct StateCounts {
    /// Reviews dispatched that no worker runs yet.
    pub queued: i64,
    /// Reviews that a worker runs.
    pub running: i64,
    /// Clean reviews that wait for an approval decision or its deadline.
    pub awaiting: i64,
}

/// The current state: totals, then per repository and per review host.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CurrentState {
    /// Counts over every queued, running and undecided awaiting review.
    #[serde(flatten)]
    pub total: StateCounts,
    /// Pull requests whose schedule record is `failed`: attempts used up, or a failure that is
    /// not retried.
    pub stopped: i64,
    /// Counts per repository, sorted by name.
    pub by_repository: Vec<Keyed<StateCounts>>,
    /// Counts per review host chosen at dispatch, sorted by name.
    pub by_host: Vec<Keyed<StateCounts>>,
    /// Stopped pull requests per repository, sorted by name; the schedule records no host.
    pub stopped_by_repository: Vec<Keyed<StoppedCount>>,
}

/// The stopped pull requests of one repository.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct StoppedCount {
    /// Pull requests whose schedule record is `failed`: attempts used up, or a failure that is
    /// not retried.
    pub stopped: i64,
}

/// The failed attempts of one error code.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FailedCount {
    /// Attempts counted as `failed` that stored this code.
    pub failed: i64,
}

/// One entry of a breakdown: its name, then its counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Keyed<T> {
    /// Repository, host, rule `match`, reviewer ID or UTC day, as the list names it.
    pub key: String,
    /// The counts of this entry.
    #[serde(flatten)]
    pub counts: T,
}

/// The history of a range: totals, one entry per day, and breakdowns over the range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct History {
    /// First UTC day of the range, `YYYY-MM-DD`.
    pub since: String,
    /// Last UTC day of the range, `YYYY-MM-DD`.
    pub until: String,
    /// Completed reviews last updated before this instant may already be pruned.
    pub pruned_before: String,
    /// Counts over the whole range.
    pub totals: Counters,
    /// Counts per UTC day, every day of the range in order.
    pub days: Vec<Keyed<Counters>>,
    /// Counts per repository over the range, sorted by name.
    pub by_repository: Vec<Keyed<Counters>>,
    /// Counts per rule `match` over the range, sorted by it.
    pub by_rule: Vec<Keyed<Counters>>,
    /// Counts per reviewer ID over the range; a review counts once for each of its reviewers.
    pub by_reviewer: Vec<Keyed<Counters>>,
    /// The `failed` attempts of the range per stored error code, sorted by code.
    pub failed_by_code: Vec<Keyed<FailedCount>>,
    /// The runs of each review step that ended in the range, sorted by step name.
    pub steps: Vec<Keyed<StepCounts>>,
}

/// Counts of the runs of one review step that ended in the range.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct StepCounts {
    /// Runs of the step, whatever their outcome.
    pub runs: i64,
    /// Runs that finished the work of the step.
    pub succeeded: i64,
    /// Runs stopped because a newer state of the pull request replaced the review.
    pub superseded: i64,
    /// Runs that failed for any other reason.
    pub failed: i64,
    /// Repeated attempts inside the runs: retried calls, session starts and reviewer runs.
    pub retries: i64,
    /// Median milliseconds of the succeeded runs.
    pub duration_median_ms: Option<i64>,
    /// 90th percentile of the same milliseconds, nearest rank.
    pub duration_p90_ms: Option<i64>,
}

/// Counts of review attempts that started or completed, and of stops, in one bucket.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Counters {
    /// Attempts created in the bucket.
    pub started: i64,
    /// Attempts completed in the bucket, whatever their result.
    pub completed: i64,
    /// Completed attempts that published a review on GitHub.
    pub published: i64,
    /// Published approvals of reviews that never waited for an approval.
    pub approved_without_human: i64,
    /// Published approvals of waits that the `approve` command approved.
    pub approved_after_wait: i64,
    /// Published reviews that requested changes.
    pub changes_requested: i64,
    /// Published reviews that commented.
    pub commented: i64,
    /// Attempts that a newer state of the pull request replaced.
    pub superseded: i64,
    /// Approval waits that ended at their deadline without a publication.
    pub approval_expired: i64,
    /// Attempts that failed for any other reason; a retry is a new attempt.
    pub failed: i64,
    /// Published attempts that handed their notes to the author instead of approving.
    pub notes_to_author: i64,
    /// Pull requests whose schedule record entered `failed`; no rule or reviewer has one.
    pub stopped: i64,
    /// Median seconds from creation to completion of the published attempts.
    pub publication_median_seconds: Option<i64>,
    /// 90th percentile of the same seconds, nearest rank.
    pub publication_p90_seconds: Option<i64>,
}

/// How one completed attempt ended, as its row records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ending {
    /// Published a review on GitHub.
    Published {
        /// Decision of the publication.
        decision: Published,
        /// Seconds from creation to completion; `None` when the clock went back.
        seconds: Option<i64>,
    },
    /// Replaced by a newer state of the pull request.
    Superseded,
    /// The approval wait reached its deadline.
    Expired,
    /// Failed with a code that is not a superseding one.
    Failed,
    /// An outcome this engine does not know, as an older engine stored it.
    Unknown,
}

/// The decision of a published attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Published {
    /// Approved without an approval wait.
    ApprovedWithoutHuman,
    /// Approved after the `approve` command decided its wait.
    ApprovedAfterWait,
    /// Requested changes.
    ChangesRequested,
    /// Commented.
    Commented,
    /// A decision this engine does not know.
    Other,
}

impl Counters {
    /// Returns each counter with its field name, the rollup's counter, without the percentiles.
    fn named(&self) -> [(&'static str, i64); 12] {
        [
            ("started", self.started),
            ("completed", self.completed),
            ("published", self.published),
            ("approved_without_human", self.approved_without_human),
            ("approved_after_wait", self.approved_after_wait),
            ("changes_requested", self.changes_requested),
            ("commented", self.commented),
            ("superseded", self.superseded),
            ("approval_expired", self.approval_expired),
            ("failed", self.failed),
            ("notes_to_author", self.notes_to_author),
            ("stopped", self.stopped),
        ]
    }

    /// Returns the counter whose field name is `name`, or `None` for another name.
    fn named_mut(&mut self, name: &str) -> Option<&mut i64> {
        match name {
            "started" => Some(&mut self.started),
            "completed" => Some(&mut self.completed),
            "published" => Some(&mut self.published),
            "approved_without_human" => Some(&mut self.approved_without_human),
            "approved_after_wait" => Some(&mut self.approved_after_wait),
            "changes_requested" => Some(&mut self.changes_requested),
            "commented" => Some(&mut self.commented),
            "superseded" => Some(&mut self.superseded),
            "approval_expired" => Some(&mut self.approval_expired),
            "failed" => Some(&mut self.failed),
            "notes_to_author" => Some(&mut self.notes_to_author),
            "stopped" => Some(&mut self.stopped),
            _ => None,
        }
    }
}

impl StepCounts {
    /// Returns each counter with its field name, the rollup's counter, without the percentiles.
    fn named(&self) -> [(&'static str, i64); 5] {
        [
            ("runs", self.runs),
            ("succeeded", self.succeeded),
            ("superseded", self.superseded),
            ("failed", self.failed),
            ("retries", self.retries),
        ]
    }

    /// Returns the counter whose field name is `name`, or `None` for another name.
    fn named_mut(&mut self, name: &str) -> Option<&mut i64> {
        match name {
            "runs" => Some(&mut self.runs),
            "succeeded" => Some(&mut self.succeeded),
            "superseded" => Some(&mut self.superseded),
            "failed" => Some(&mut self.failed),
            "retries" => Some(&mut self.retries),
            _ => None,
        }
    }

    /// Reports whether the rollup counter `name` is a step counter.
    fn knows(name: &str) -> bool {
        Self::default().named_mut(name).is_some()
    }
}

/// The counts of one bucket while the rows are read, with the publication seconds.
#[derive(Debug, Default)]
struct Tally {
    /// The counts; the two percentiles are filled by `finish`.
    counters: Counters,
    /// Seconds to publication of every published attempt with a valid duration.
    seconds: Vec<i64>,
}

impl Tally {
    /// Adds the counts and the seconds of `other`.
    fn merge(&mut self, other: &Self) {
        for (name, value) in other.counters.named() {
            if let Some(counter) = self.counters.named_mut(name) {
                *counter += value;
            }
        }
        self.seconds.extend_from_slice(&other.seconds);
    }

    /// Reports whether the rollup counter `name` counts in a bucket.
    fn knows(name: &str) -> bool {
        Counters::default().named_mut(name).is_some()
    }

    /// Adds `value` to the rollup counter `name`; a name that `knows` refuses adds nothing.
    fn add_named(&mut self, name: &str, value: i64) {
        if let Some(counter) = self.counters.named_mut(name) {
            *counter += value;
        }
    }

    /// Counts one attempt created in the bucket.
    fn start(&mut self) {
        self.counters.started += 1;
    }

    /// Counts one attempt completed in the bucket.
    fn complete(&mut self, ending: Ending) {
        let counters = &mut self.counters;
        counters.completed += 1;
        match ending {
            Ending::Published { decision, seconds } => {
                counters.published += 1;
                match decision {
                    Published::ApprovedWithoutHuman => counters.approved_without_human += 1,
                    Published::ApprovedAfterWait => counters.approved_after_wait += 1,
                    Published::ChangesRequested => counters.changes_requested += 1,
                    Published::Commented => counters.commented += 1,
                    Published::Other => {}
                }
                self.seconds.extend(seconds);
            }
            Ending::Superseded => counters.superseded += 1,
            Ending::Expired => counters.approval_expired += 1,
            Ending::Failed => counters.failed += 1,
            Ending::Unknown => {}
        }
    }

    /// Returns the counts with the median and the 90th percentile of the seconds.
    fn finish(mut self) -> Counters {
        let (median, p90) = median_and_p90(self.seconds);
        self.counters.publication_median_seconds = median;
        self.counters.publication_p90_seconds = p90;
        self.counters
    }
}

/// The counts of one step while the runs are read, with the durations of the succeeded runs.
#[derive(Debug, Default)]
struct StepTally {
    /// The counts; the two percentiles are filled by `finish`.
    counts: StepCounts,
    /// Milliseconds of every succeeded run.
    durations: Vec<i64>,
}

impl StepTally {
    /// Counts one run of the step.
    fn add(&mut self, run: &StepRun) {
        let counts = &mut self.counts;
        counts.runs += 1;
        counts.retries += run.retries;
        match run.outcome {
            StepOutcome::Succeeded => {
                counts.succeeded += 1;
                self.durations.push(run.duration_ms);
            }
            StepOutcome::Superseded => counts.superseded += 1,
            StepOutcome::Failed => counts.failed += 1,
        }
    }

    /// Adds the counts and the durations of `other`.
    fn merge(&mut self, other: Self) {
        for (name, value) in other.counts.named() {
            if let Some(counter) = self.counts.named_mut(name) {
                *counter += value;
            }
        }
        self.durations.extend(other.durations);
    }

    /// Returns the counts with the median and the 90th percentile of the durations.
    fn finish(mut self) -> StepCounts {
        self.durations.sort_unstable();
        self.counts.duration_median_ms = nearest_rank(&self.durations, MEDIAN_PERCENT);
        self.counts.duration_p90_ms = nearest_rank(&self.durations, P90_PERCENT);
        self.counts
    }
}

/// Returns the nearest-rank percentile of sorted `values`: the `ceil(p * n / 100)`-th smallest.
fn nearest_rank(values: &[i64], percent: usize) -> Option<i64> {
    let rank = (percent * values.len()).div_ceil(100);
    values.get(rank.checked_sub(1)?).copied()
}

/// Returns the median and the 90th percentile of `values` by nearest rank; `None` for none.
pub(crate) fn median_and_p90(mut values: Vec<i64>) -> (Option<i64>, Option<i64>) {
    values.sort_unstable();
    let median = nearest_rank(&values, MEDIAN_PERCENT);
    (median, nearest_rank(&values, P90_PERCENT))
}

/// Reads a stored timestamp as an instant in UTC.
fn stored_instant(
    review: &StoredReview,
    column: &str,
    text: &str,
) -> Result<OffsetDateTime, Error> {
    parse_rfc3339(text)
        .map(|instant| instant.to_offset(UtcOffset::UTC))
        .map_err(|err| state_invalid(review, &format!("{column}: {err}")))
}

/// Returns the `store_state_invalid` error of one review row.
fn state_invalid(review: &StoredReview, reason: &str) -> Error {
    store_invalid(format!("review {} {reason}", review.workflow_id))
}

/// Returns the `store_state_invalid` error with its cause.
pub(crate) fn store_invalid(cause: String) -> Error {
    Error::workflow("store_state_invalid", true, Some(cause.into()))
}

/// Reads the UTC day of a rollup row.
fn rollup_day(text: &str) -> Result<Date, Error> {
    parse_day(text).map_err(|_| store_invalid(format!("rollup day {text:?} is not a day")))
}

/// Reads the completion instant of a completed review.
fn completed_instant(review: &StoredReview) -> Result<OffsetDateTime, Error> {
    if review.completed_at.is_empty() {
        return Err(state_invalid(review, "is completed without completed_at"));
    }
    stored_instant(review, "completed_at", &review.completed_at)
}

/// Returns the whole seconds from `created` to `completed`; `None` when the clock went back.
fn elapsed_seconds(created: OffsetDateTime, completed: OffsetDateTime) -> Option<i64> {
    let elapsed = (completed - created).whole_seconds();
    (elapsed >= 0).then_some(elapsed)
}

/// Reads how a completed review ended, with its seconds from creation to completion.
///
/// # Errors
///
/// Returns `store_state_invalid` as `compute` does for the same row.
pub(crate) fn completed_ending(review: &StoredReview) -> Result<Ending, Error> {
    let created = stored_instant(review, "created_at", &review.created_at)?;
    let completed = completed_instant(review)?;
    ending(review, elapsed_seconds(created, completed))
}

/// Returns the whole seconds from the review request of `review` to its completion `end`.
///
/// `None` when the request time is unknown or later than `end` (clock skew).
fn request_seconds(review: &StoredReview, end: OffsetDateTime) -> Result<Option<i64>, Error> {
    let text = &review.input.pull_request.requested_at;
    if text.is_empty() {
        return Ok(None);
    }
    let requested = stored_instant(review, "requested_at", text)?;
    Ok(elapsed_seconds(requested, end))
}

/// Returns the `repository#number#request ID` key of the review request of `review`.
///
/// `None` without a request ID, as for a label trigger.
fn request_key(review: &StoredReview) -> Option<String> {
    let pull_request = &review.input.pull_request;
    let id = pull_request.review_request_id;
    (id != 0).then(|| format!("{}#{id}", pull_request.key()))
}

/// Returns the workflow IDs of the first published attempt of each review request.
///
/// The first completed earliest; a tie goes to the smaller workflow ID. A request in `pruned`
/// had its first publication pruned, so no attempt of `reviews` is its first.
///
/// # Errors
///
/// Returns `store_state_invalid` as `compute` does for the same rows.
pub(crate) fn first_publications(
    reviews: &[StoredReview],
    pruned: &BTreeSet<String>,
) -> Result<BTreeSet<String>, Error> {
    let mut firsts: BTreeMap<String, (OffsetDateTime, &str)> = BTreeMap::new();
    for review in reviews {
        let Some(key) = request_key(review).filter(|key| !pruned.contains(key)) else {
            continue;
        };
        if review.status != REVIEW_COMPLETED
            || !matches!(completed_ending(review)?, Ending::Published { .. })
        {
            continue;
        }
        let attempt = (completed_instant(review)?, review.workflow_id.as_str());
        let first = firsts.entry(key).or_insert(attempt);
        *first = (*first).min(attempt);
    }
    Ok(firsts.into_values().map(|(_, id)| id.to_owned()).collect())
}

/// Reads how a completed attempt ended from its result, failure, approval and outcome.
fn ending(review: &StoredReview, seconds: Option<i64>) -> Result<Ending, Error> {
    if review.result == RESULT_FAILURE {
        return Ok(if superseded(&review.failure) {
            Ending::Superseded
        } else {
            Ending::Failed
        });
    }
    if review.result != RESULT_SUCCESS || review.outcome.is_empty() {
        return Ok(Ending::Unknown);
    }
    let outcome: ReviewOutcome = serde_json::from_str(&review.outcome)
        .map_err(|err| state_invalid(review, &format!("outcome: {err}")))?;
    Ok(match (outcome.outcome, outcome.publication) {
        (Outcome::Published, Some(publication)) => Ending::Published {
            decision: match publication.decision {
                Decision::Approve if review.approval == APPROVAL_KIND_APPROVE => {
                    Published::ApprovedAfterWait
                }
                Decision::Approve if review.approval.is_empty() => Published::ApprovedWithoutHuman,
                Decision::RequestChanges => Published::ChangesRequested,
                Decision::Comment => Published::Commented,
                _ => Published::Other,
            },
            seconds,
        },
        (Outcome::Superseded, _) => Ending::Superseded,
        (Outcome::Expired, _) => Ending::Expired,
        _ => Ending::Unknown,
    })
}

/// The buckets of one UTC day while the rows and the rollup are read.
#[derive(Debug, Default)]
struct DayTally {
    /// The whole day.
    total: Tally,
    /// One bucket per repository.
    repositories: BTreeMap<String, Tally>,
    /// One bucket per rule `match`.
    rules: BTreeMap<String, Tally>,
    /// One bucket per reviewer ID.
    reviewers: BTreeMap<String, Tally>,
    /// The `failed` attempts per stored error code.
    failures: BTreeMap<String, i64>,
    /// One bucket per review step.
    steps: BTreeMap<String, StepTally>,
    /// Seconds from the review request to publication, per repository.
    requests: RequestSeconds,
    /// The verifier's verdicts, per repository and reviewer ID.
    checks: Checks,
    /// The reviewer disagreement of published attempts, per repository.
    disagreements: Disagreements,
}

/// Seconds from the review request to publication of published attempts, per repository.
pub(crate) type RequestSeconds = BTreeMap<String, Vec<i64>>;

/// The verifier's verdicts per repository and reviewer ID of the checked finding.
pub(crate) type Checks = BTreeMap<(String, String), CheckCounts>;

/// The verifier's verdicts on the findings of one reviewer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CheckCounts {
    /// Findings that a finished verification gave a verdict.
    pub(crate) checked: i64,
    /// Checked findings found not blocking or refuted.
    pub(crate) demoted: i64,
}

impl CheckCounts {
    /// Adds the counts of `other`.
    fn merge(&mut self, other: Self) {
        self.checked += other.checked;
        self.demoted += other.demoted;
    }
}

/// The reviewer disagreement of published attempts per repository.
pub(crate) type Disagreements = BTreeMap<String, Disagreement>;

/// The reviewer disagreement of the published attempts of one repository.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Disagreement {
    /// Published attempts with the results of two or more reviewers.
    pub(crate) multi_reviewer: i64,
    /// Those where one reviewer made a blocking claim and another none.
    pub(crate) disagreed: i64,
    /// Per reviewer ID, the disagreeing attempts in which it made a blocking claim.
    pub(crate) blocked_alone: BTreeMap<String, i64>,
}

impl Disagreement {
    /// Adds the counts of `other`.
    fn merge(&mut self, other: Self) {
        self.multi_reviewer += other.multi_reviewer;
        self.disagreed += other.disagreed;
        for (reviewer, count) in other.blocked_alone {
            *self.blocked_alone.entry(reviewer).or_default() += count;
        }
    }
}

/// The part of a stored reviewer slot that the disagreement reads.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
struct StoredSlot {
    /// The checked result of the reviewer; a slot that never finished has none.
    result: Option<SlotResult>,
}

/// The findings of a reviewer's checked result, as far as the disagreement reads them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
struct SlotResult {
    /// The reviewer's findings before they are combined; a stored `null` is none.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    findings: Vec<SlotFinding>,
}

/// Whether one finding of a reviewer's result blocks.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
struct SlotFinding {
    /// Marks a note; an absent flag blocks.
    non_blocking: bool,
}

/// Returns the reviewers that blocked alone in one attempt, or `None` when fewer than two
/// of its reviewer slots in `slots` have a result.
///
/// A reviewer blocks with a finding that is not `non_blocking`. The attempt disagreed when
/// some reviewers block and the others do not; then each one that blocks blocked alone.
/// An attempt where all block or none does gives an empty list.
fn blocked_alone(slots: &BTreeMap<String, StoredSlot>) -> Option<Vec<&str>> {
    let blocks: Vec<(&str, bool)> = (slots.iter())
        .filter_map(|(reviewer, slot)| {
            let result = slot.result.as_ref()?;
            let blocks = result.findings.iter().any(|finding| !finding.non_blocking);
            Some((reviewer.as_str(), blocks))
        })
        .collect();
    if blocks.len() < 2 {
        return None;
    }
    if blocks.iter().all(|(_, blocks)| *blocks) {
        return Some(Vec::new());
    }
    let alone = blocks.into_iter().filter(|(_, blocks)| *blocks);
    Some(alone.map(|(reviewer, _)| reviewer).collect())
}

impl DayTally {
    /// Returns the breakdown map of a review `dimension`, or `None` for another dimension.
    fn breakdown(&mut self, dimension: &str) -> Option<&mut BTreeMap<String, Tally>> {
        match dimension {
            DIMENSION_REPOSITORY => Some(&mut self.repositories),
            DIMENSION_RULE => Some(&mut self.rules),
            DIMENSION_REVIEWER => Some(&mut self.reviewers),
            _ => None,
        }
    }
}

/// The buckets of a history per UTC day, while the rows and the rollup are read.
#[derive(Debug, Default)]
struct HistoryTally {
    /// One bucket per UTC day.
    days: BTreeMap<Date, DayTally>,
}

impl HistoryTally {
    /// Applies `count` to the day and its breakdowns of `review`.
    fn add(&mut self, review: &StoredReview, day: Date, count: impl Fn(&mut Tally)) {
        let input = &review.input;
        let bucket = self.days.entry(day).or_default();
        count(&mut bucket.total);
        count(
            bucket
                .repositories
                .entry(input.pull_request.repository.clone())
                .or_default(),
        );
        count(
            bucket
                .rules
                .entry(input.rule.match_glob.clone())
                .or_default(),
        );
        let reviewers: BTreeSet<&str> = input.reviewers.iter().map(|r| r.id.as_str()).collect();
        for reviewer in reviewers {
            count(bucket.reviewers.entry(reviewer.to_owned()).or_default());
        }
    }

    /// Counts the starts and completions of `reviews` on the days of `range`, every day
    /// without one; a published attempt whose plan in `plans` handed the notes to the author
    /// also counts them, and one in `firsts` its seconds from the review request. A completed
    /// attempt also counts the verdicts of its verification in `verifier`, and a published
    /// one the disagreement of its reviewer slots in `slots`.
    fn add_reviews<'r>(
        &mut self,
        reviews: impl IntoIterator<Item = &'r StoredReview>,
        plans: &BTreeMap<String, String>,
        verifier: &VerifierSteps,
        slots: &BTreeMap<String, BTreeMap<String, String>>,
        range: Option<&StatsRange>,
        firsts: &BTreeSet<String>,
    ) -> Result<(), Error> {
        let inside = |day: Date| range.is_none_or(|range| range.contains(day));
        for review in reviews {
            let created = stored_instant(review, "created_at", &review.created_at)?;
            if inside(created.date()) {
                self.add(review, created.date(), Tally::start);
            }
            if review.status != REVIEW_COMPLETED {
                continue;
            }
            let completed = completed_instant(review)?;
            if !inside(completed.date()) {
                continue;
            }
            let ending = ending(review, elapsed_seconds(created, completed))?;
            self.add(review, completed.date(), |bucket| bucket.complete(ending));
            if ending == Ending::Failed {
                let bucket = self.days.entry(completed.date()).or_default();
                *bucket.failures.entry(review.failure.clone()).or_default() += 1;
            }
            if let Ending::Published { .. } = ending
                && firsts.contains(&review.workflow_id)
                && let Some(seconds) = request_seconds(review, completed)?
            {
                let bucket = self.days.entry(completed.date()).or_default();
                let repository = review.input.pull_request.repository.clone();
                bucket.requests.entry(repository).or_default().push(seconds);
            }
            let plan = match (ending, plans.get(&review.workflow_id)) {
                (Ending::Published { .. }, Some(text)) => serde_json::from_str(text)
                    .map_err(|err| state_invalid(review, &format!("plan: {err}")))?,
                _ => PublicationPlan::default(),
            };
            if plan.notes_to_author {
                let notes = |bucket: &mut Tally| bucket.counters.notes_to_author += 1;
                self.add(review, completed.date(), notes);
            }
            self.add_checks(review, completed.date(), verifier)?;
            if let (Ending::Published { .. }, Some(slots)) =
                (ending, slots.get(&review.workflow_id))
            {
                self.add_disagreement(review, completed.date(), slots)?;
            }
        }
        Ok(())
    }

    /// Counts the reviewer disagreement of the published `review` on `day` from its stored
    /// reviewer slots `slots`, by reviewer ID.
    fn add_disagreement(
        &mut self,
        review: &StoredReview,
        day: Date,
        slots: &BTreeMap<String, String>,
    ) -> Result<(), Error> {
        let mut decoded = BTreeMap::new();
        for (reviewer, text) in slots {
            let slot: StoredSlot = serde_json::from_str(text)
                .map_err(|err| state_invalid(review, &format!("slot {reviewer}: {err}")))?;
            decoded.insert(reviewer.clone(), slot);
        }
        let Some(alone) = blocked_alone(&decoded) else {
            return Ok(());
        };
        let mut counts = Disagreement {
            multi_reviewer: 1,
            disagreed: i64::from(!alone.is_empty()),
            blocked_alone: BTreeMap::new(),
        };
        for reviewer in alone {
            counts.blocked_alone.insert(reviewer.to_owned(), 1);
        }
        let repository = review.input.pull_request.repository.clone();
        let disagreements = &mut self.days.entry(day).or_default().disagreements;
        disagreements.entry(repository).or_default().merge(counts);
        Ok(())
    }

    /// Counts the verdicts of the verification of `review` on `day`, per reviewer of the
    /// checked finding as the combined artifact names it.
    ///
    /// A verification that did not finish checked nothing. A verdict on a finding that the
    /// combined artifact does not hold, or holds without a reviewer, counts nowhere.
    fn add_checks(
        &mut self,
        review: &StoredReview,
        day: Date,
        verifier: &VerifierSteps,
    ) -> Result<(), Error> {
        let Some(text) = verifier.verifications.get(&review.workflow_id) else {
            return Ok(());
        };
        let verification: Verification = serde_json::from_str(text)
            .map_err(|err| state_invalid(review, &format!("verification: {err}")))?;
        if !verification.error_code.is_empty() {
            return Ok(());
        }
        let combined: ArtifactOutcome = match verifier.combined.get(&review.workflow_id) {
            Some(text) => serde_json::from_str(text)
                .map_err(|err| state_invalid(review, &format!("combined artifact: {err}")))?,
            None => ArtifactOutcome::default(),
        };
        let checks = &mut self.days.entry(day).or_default().checks;
        for check in &verification.findings {
            let demoted = match check.verdict.as_str() {
                VERDICT_CONFIRMED => 0,
                VERDICT_NOT_BLOCKING | VERDICT_REFUTED => 1,
                _ => continue,
            };
            let findings = &combined.artifact.findings;
            let Some(finding) = findings.iter().find(|finding| finding.id == check.id) else {
                continue;
            };
            if finding.reviewer_id.is_empty() {
                continue;
            }
            let repository = review.input.pull_request.repository.clone();
            let counts = checks.entry((repository, finding.reviewer_id.clone()));
            counts.or_default().merge(CheckCounts {
                checked: 1,
                demoted,
            });
        }
        Ok(())
    }

    /// Counts each step run on the UTC day it ended; the snapshot of a range holds only the
    /// runs that ended inside it.
    fn add_runs(&mut self, runs: &[EndedStepRun]) -> Result<(), Error> {
        for ended in runs {
            let finished = parse_rfc3339(&ended.finished_at)
                .map(|instant| instant.to_offset(UtcOffset::UTC))
                .map_err(|err| {
                    store_invalid(format!(
                        "step run of review {} finished_at: {err}",
                        ended.workflow_id
                    ))
                })?;
            let bucket = self.days.entry(finished.date()).or_default();
            let step = bucket.steps.entry(ended.run.step.as_str().to_owned());
            step.or_default().add(&ended.run);
        }
        Ok(())
    }

    /// Adds the counts and samples of the rollup; a dimension or counter it does not know
    /// counts nowhere and adds no entry.
    fn add_rollup(&mut self, rollup: &Rollup) -> Result<(), Error> {
        for count in &rollup.counts {
            let bucket = self.days.entry(rollup_day(&count.day)?).or_default();
            let value = count.value;
            match count.dimension.as_str() {
                DIMENSION_DAY => {
                    bucket.total.add_named(&count.counter, value);
                }
                DIMENSION_FAILURE if count.counter == COUNTER_FAILED => {
                    *bucket.failures.entry(count.key.clone()).or_default() += value;
                }
                DIMENSION_STEP if StepCounts::knows(&count.counter) => {
                    let step = bucket.steps.entry(count.key.clone()).or_default();
                    if let Some(counter) = step.counts.named_mut(&count.counter) {
                        *counter += value;
                    }
                }
                DIMENSION_VERIFIER => {
                    let counts = match count.counter.as_str() {
                        COUNTER_CHECKED => CheckCounts {
                            checked: value,
                            demoted: 0,
                        },
                        COUNTER_DEMOTED => CheckCounts {
                            checked: 0,
                            demoted: value,
                        },
                        _ => continue,
                    };
                    let Some((repository, reviewer)) = count.key.split_once('#') else {
                        return Err(store_invalid(format!("rollup verifier key {}", count.key)));
                    };
                    let key = (repository.to_owned(), reviewer.to_owned());
                    bucket.checks.entry(key).or_default().merge(counts);
                }
                DIMENSION_DISAGREEMENT => {
                    let counts = match count.counter.as_str() {
                        COUNTER_MULTI_REVIEWER => Disagreement {
                            multi_reviewer: value,
                            ..Disagreement::default()
                        },
                        COUNTER_DISAGREED => Disagreement {
                            disagreed: value,
                            ..Disagreement::default()
                        },
                        _ => continue,
                    };
                    let disagreement = bucket.disagreements.entry(count.key.clone());
                    disagreement.or_default().merge(counts);
                }
                DIMENSION_DISAGREEMENT_REVIEWER if count.counter == COUNTER_BLOCKED_ALONE => {
                    let Some((repository, reviewer)) = count.key.split_once('#') else {
                        let key = &count.key;
                        return Err(store_invalid(format!("rollup disagreement key {key}")));
                    };
                    let disagreement = bucket.disagreements.entry(repository.to_owned());
                    let alone = &mut disagreement.or_default().blocked_alone;
                    *alone.entry(reviewer.to_owned()).or_default() += value;
                }
                dimension => {
                    if let (true, Some(map)) =
                        (Tally::knows(&count.counter), bucket.breakdown(dimension))
                    {
                        map.entry(count.key.clone())
                            .or_default()
                            .add_named(&count.counter, value);
                    }
                }
            }
        }
        for sample in &rollup.samples {
            let bucket = self.days.entry(rollup_day(&sample.day)?).or_default();
            let times = usize::try_from(sample.count)
                .map_err(|_| store_invalid(format!("rollup sample count {}", sample.count)))?;
            let values = std::iter::repeat_n(sample.sample, times);
            match sample.dimension.as_str() {
                DIMENSION_DAY => bucket.total.seconds.extend(values),
                DIMENSION_STEP => {
                    let step = bucket.steps.entry(sample.key.clone()).or_default();
                    step.durations.extend(values);
                }
                DIMENSION_REQUEST => {
                    let seconds = bucket.requests.entry(sample.key.clone()).or_default();
                    seconds.extend(values);
                }
                dimension => {
                    if let Some(map) = bucket.breakdown(dimension) {
                        map.entry(sample.key.clone())
                            .or_default()
                            .seconds
                            .extend(values);
                    }
                }
            }
        }
        Ok(())
    }

    /// Returns the counts and samples of every day, as the prune adds them to the rollup.
    fn into_rollup(self) -> Rollup {
        let mut rollup = Rollup::default();
        for (day, bucket) in self.days {
            let day = day.to_string();
            push_tally(&mut rollup, &day, DIMENSION_DAY, "", &bucket.total);
            for (dimension, map) in [
                (DIMENSION_REPOSITORY, &bucket.repositories),
                (DIMENSION_RULE, &bucket.rules),
                (DIMENSION_REVIEWER, &bucket.reviewers),
            ] {
                for (key, tally) in map {
                    push_tally(&mut rollup, &day, dimension, key, tally);
                }
            }
            for (code, failed) in &bucket.failures {
                push_count(
                    &mut rollup,
                    &day,
                    DIMENSION_FAILURE,
                    code,
                    COUNTER_FAILED,
                    *failed,
                );
            }
            for (step, tally) in &bucket.steps {
                for (name, value) in tally.counts.named() {
                    push_count(&mut rollup, &day, DIMENSION_STEP, step, name, value);
                }
                push_samples(&mut rollup, &day, DIMENSION_STEP, step, &tally.durations);
            }
            for (repository, seconds) in &bucket.requests {
                push_samples(&mut rollup, &day, DIMENSION_REQUEST, repository, seconds);
            }
            for ((repository, reviewer), counts) in &bucket.checks {
                let key = format!("{repository}#{reviewer}");
                for (name, value) in [
                    (COUNTER_CHECKED, counts.checked),
                    (COUNTER_DEMOTED, counts.demoted),
                ] {
                    push_count(&mut rollup, &day, DIMENSION_VERIFIER, &key, name, value);
                }
            }
            for (repository, counts) in &bucket.disagreements {
                for (name, value) in [
                    (COUNTER_MULTI_REVIEWER, counts.multi_reviewer),
                    (COUNTER_DISAGREED, counts.disagreed),
                ] {
                    let dimension = DIMENSION_DISAGREEMENT;
                    push_count(&mut rollup, &day, dimension, repository, name, value);
                }
                for (reviewer, alone) in &counts.blocked_alone {
                    let (key, name) = (format!("{repository}#{reviewer}"), COUNTER_BLOCKED_ALONE);
                    let dimension = DIMENSION_DISAGREEMENT_REVIEWER;
                    push_count(&mut rollup, &day, dimension, &key, name, *alone);
                }
            }
        }
        rollup
    }
}

/// Adds one count to `rollup`; a zero adds nothing, so the rollup holds no empty entry.
fn push_count(rollup: &mut Rollup, day: &str, dimension: &str, key: &str, name: &str, value: i64) {
    if value != 0 {
        rollup.counts.push(RollupCount {
            day: day.to_owned(),
            dimension: dimension.to_owned(),
            key: key.to_owned(),
            counter: name.to_owned(),
            value,
        });
    }
}

/// Adds each distinct value of `values` with how often it occurs.
fn push_samples(rollup: &mut Rollup, day: &str, dimension: &str, key: &str, values: &[i64]) {
    let mut counted: BTreeMap<i64, i64> = BTreeMap::new();
    for value in values {
        *counted.entry(*value).or_default() += 1;
    }
    for (sample, count) in counted {
        rollup.samples.push(RollupSample {
            day: day.to_owned(),
            dimension: dimension.to_owned(),
            key: key.to_owned(),
            sample,
            count,
        });
    }
}

/// Adds the counters and the seconds of one bucket.
fn push_tally(rollup: &mut Rollup, day: &str, dimension: &str, key: &str, tally: &Tally) {
    for (name, value) in tally.counters.named() {
        push_count(rollup, day, dimension, key, name, value);
    }
    push_samples(rollup, day, dimension, key, &tally.seconds);
}

/// Returns the dimension and key per pipeline of an entry of `pipeline`, or `None` for a
/// dimension that `report --pipeline` does not read.
fn pipeline_entry(dimension: &str, key: &str, pipeline: &str) -> Option<(&'static str, String)> {
    let (_, per_pipeline) = (PIPELINE_DIMENSIONS.iter()).find(|(plain, _)| *plain == dimension)?;
    let key = match key.split_once('#') {
        Some((repository, reviewer)) => format!("{repository}#{pipeline}#{reviewer}"),
        None => format!("{key}#{pipeline}"),
    };
    Some((per_pipeline, key))
}

/// Returns the rollup of pruned reviews: their starts, completions, notes, verdicts,
/// disagreement and step runs, and per pipeline the counts that `report --pipeline` reads.
///
/// `runs` are the step runs of `reviews`, `plans` their stored plans by workflow ID,
/// `verifier` their verifier's steps, `slots` their reviewer slots, and `firsts` the first
/// publications of `first_publications`; each of `reviews` among them adds its request
/// seconds and its `request_id` count. The reviews of each pipeline are counted again alone,
/// and the entries of `PIPELINE_DIMENSIONS` are kept under their dimension per pipeline.
///
/// # Errors
///
/// Returns `store_state_invalid` as `compute` does for the same rows, for the plan or a
/// reviewer slot of a published attempt and for a verification or combined artifact that does
/// not decode.
pub(crate) fn rollup(
    reviews: &[StoredReview],
    runs: &[EndedStepRun],
    plans: &BTreeMap<String, String>,
    verifier: &VerifierSteps,
    slots: &BTreeMap<String, BTreeMap<String, String>>,
    firsts: &BTreeSet<String>,
) -> Result<Rollup, Error> {
    let mut tally = HistoryTally::default();
    tally.add_reviews(reviews, plans, verifier, slots, None, firsts)?;
    tally.add_runs(runs)?;
    let mut rollup = tally.into_rollup();
    let mut pipelines: BTreeMap<&str, Vec<&StoredReview>> = BTreeMap::new();
    for review in reviews {
        let pipeline = review.input.rule.pipeline.name.as_str();
        pipelines.entry(pipeline).or_default().push(review);
    }
    for (pipeline, reviews) in pipelines {
        let mut tally = HistoryTally::default();
        tally.add_reviews(reviews, plans, verifier, slots, None, firsts)?;
        let counted = tally.into_rollup();
        for count in counted.counts {
            let entry = pipeline_entry(&count.dimension, &count.key, pipeline);
            if let Some((dimension, key)) = entry {
                let dimension = dimension.to_owned();
                rollup.counts.push(RollupCount {
                    dimension,
                    key,
                    ..count
                });
            }
        }
        for sample in counted.samples {
            let entry = pipeline_entry(&sample.dimension, &sample.key, pipeline);
            if let Some((dimension, key)) = entry {
                let dimension = dimension.to_owned();
                rollup.samples.push(RollupSample {
                    dimension,
                    key,
                    ..sample
                });
            }
        }
    }
    for review in reviews {
        if firsts.contains(&review.workflow_id)
            && let Some(key) = request_key(review)
        {
            let day = completed_instant(review)?.date().to_string();
            let counter = COUNTER_FIRST_PUBLICATION;
            push_count(&mut rollup, &day, DIMENSION_REQUEST_ID, &key, counter, 1);
        }
    }
    Ok(rollup)
}

/// Builds the document from one `Store::stats_snapshot`.
///
/// The current state counts every queued and running row, every awaiting row without an
/// approval decision, and every schedule record in phase `failed`. With `range`, an attempt
/// counts as started on the UTC day of `created_at` and as completed on the UTC day of
/// `completed_at`, each only inside the range, and a step run on the UTC day it ended; the
/// rollup adds the same counts of the pruned reviews of the range's days, and its stops.
///
/// # Errors
///
/// Returns `store_state_invalid` for a timestamp, an outcome or a rollup day that does not
/// decode, for a completed row without `completed_at`, for the plan or a reviewer slot of a
/// published row, the verification or combined artifact of a completed row or a `verifier` or
/// `disagreement_reviewer` rollup key that does not decode, and for a schedule key without `#`.
pub fn compute(
    snapshot: &StatsSnapshot,
    range: Option<&StatsRange>,
    generated_at: OffsetDateTime,
    pruned_before: OffsetDateTime,
) -> Result<StatsDocument, Error> {
    let current = current_state(&snapshot.reviews, &snapshot.discovery.schedule)?;
    let history = match range {
        // The document has no request seconds, so no attempt counts as a first publication.
        Some(range) => Some(history(snapshot, range, pruned_before, &BTreeSet::new())?.0),
        None => None,
    };
    Ok(StatsDocument {
        schema_version: STATS_SCHEMA_VERSION,
        generated_at: format_rfc3339_seconds(generated_at),
        current,
        history,
    })
}

/// Counts the pull requests whose schedule record is in phase `failed`, per repository.
///
/// The key of a record is `repository#number`.
///
/// # Errors
///
/// Returns `store_state_invalid` for a key without `#`.
pub(crate) fn stopped(records: &BTreeMap<String, Record>) -> Result<BTreeMap<String, i64>, Error> {
    let mut repositories: BTreeMap<String, i64> = BTreeMap::new();
    for (key, record) in records {
        if record.phase != phase::FAILED {
            continue;
        }
        let Some((repository, _)) = key.rsplit_once('#') else {
            let cause = format!("schedule key {key:?} has no number");
            return Err(Error::workflow(
                "store_state_invalid",
                true,
                Some(cause.into()),
            ));
        };
        *repositories.entry(repository.to_owned()).or_default() += 1;
    }
    Ok(repositories)
}

/// Reports whether `review` counts in the current state.
///
/// A queued or running row counts. An awaiting row counts only while it waits for a decision,
/// as `autoreview awaiting` lists it; a decided wait that `serve` has not continued yet counts
/// nowhere.
pub(crate) fn counts_now(review: &StoredReview) -> bool {
    match review.status.as_str() {
        REVIEW_QUEUED | REVIEW_RUNNING => true,
        REVIEW_AWAITING => review.awaits_decision(),
        _ => false,
    }
}

impl StateCounts {
    /// Counts one review of the current state under its `status`.
    pub(crate) fn add(&mut self, status: &str) {
        match status {
            REVIEW_QUEUED => self.queued += 1,
            REVIEW_RUNNING => self.running += 1,
            REVIEW_AWAITING => self.awaiting += 1,
            _ => {}
        }
    }
}

/// Counts the current state of the rows and the schedule records.
///
/// The rows that `counts_now` takes count in total, per repository and per host; the stopped
/// pull requests of `schedule` count per repository.
fn current_state(
    reviews: &[StoredReview],
    schedule: &BTreeMap<String, Record>,
) -> Result<CurrentState, Error> {
    let mut total = StateCounts::default();
    let mut repositories: BTreeMap<String, StateCounts> = BTreeMap::new();
    let mut hosts: BTreeMap<String, StateCounts> = BTreeMap::new();
    for review in reviews.iter().filter(|review| counts_now(review)) {
        let input = &review.input;
        total.add(&review.status);
        repositories
            .entry(input.pull_request.repository.clone())
            .or_default()
            .add(&review.status);
        hosts
            .entry(input.route.host.clone())
            .or_default()
            .add(&review.status);
    }
    let stopped = stopped(schedule)?;
    Ok(CurrentState {
        total,
        stopped: stopped.values().sum(),
        by_repository: keyed(repositories, |counts| counts),
        by_host: keyed(hosts, |counts| counts),
        stopped_by_repository: keyed(stopped, |stopped| StoppedCount { stopped }),
    })
}

/// Counts the rows, the step runs and the rollup of the snapshot inside `range`.
///
/// Returns the history with the seconds from the review request to publication of the first
/// publications `firsts`, the verifier's verdicts and the reviewer disagreement.
pub(crate) fn history(
    snapshot: &StatsSnapshot,
    range: &StatsRange,
    pruned_before: OffsetDateTime,
    firsts: &BTreeSet<String>,
) -> Result<(History, RequestSeconds, Checks, Disagreements), Error> {
    let mut tally = HistoryTally::default();
    let (reviews, plans) = (&snapshot.reviews, &snapshot.plans);
    let (verifier, slots) = (&snapshot.verifier, &snapshot.slots);
    tally.add_reviews(reviews, plans, verifier, slots, Some(range), firsts)?;
    tally.add_runs(&snapshot.steps)?;
    tally.add_rollup(&snapshot.rollup)?;
    let mut totals = Tally::default();
    let mut days = BTreeMap::new();
    let mut repositories: BTreeMap<String, Tally> = BTreeMap::new();
    let mut rules: BTreeMap<String, Tally> = BTreeMap::new();
    let mut reviewers: BTreeMap<String, Tally> = BTreeMap::new();
    let mut failures: BTreeMap<String, i64> = BTreeMap::new();
    let mut steps: BTreeMap<String, StepTally> = BTreeMap::new();
    let mut requests = RequestSeconds::new();
    let mut checks = Checks::new();
    let mut disagreements = Disagreements::new();
    let mut day = range.since;
    while day <= range.until {
        let bucket = tally.days.remove(&day).unwrap_or_default();
        totals.merge(&bucket.total);
        for (target, source) in [
            (&mut repositories, bucket.repositories),
            (&mut rules, bucket.rules),
            (&mut reviewers, bucket.reviewers),
        ] {
            for (key, counted) in source {
                target.entry(key).or_default().merge(&counted);
            }
        }
        for (code, failed) in bucket.failures {
            *failures.entry(code).or_default() += failed;
        }
        for (step, counted) in bucket.steps {
            steps.entry(step).or_default().merge(counted);
        }
        for (repository, seconds) in bucket.requests {
            requests.entry(repository).or_default().extend(seconds);
        }
        for (key, counts) in bucket.checks {
            checks.entry(key).or_default().merge(counts);
        }
        for (repository, counts) in bucket.disagreements {
            disagreements.entry(repository).or_default().merge(counts);
        }
        days.insert(day, bucket.total);
        let Some(next) = day.next_day() else {
            break;
        };
        day = next;
    }
    let history = History {
        since: range.since.to_string(),
        until: range.until.to_string(),
        pruned_before: format_rfc3339_seconds(pruned_before),
        totals: totals.finish(),
        days: keyed(days, Tally::finish),
        by_repository: keyed(repositories, Tally::finish),
        by_rule: keyed(rules, Tally::finish),
        by_reviewer: keyed(reviewers, Tally::finish),
        failed_by_code: keyed(failures, |failed| FailedCount { failed }),
        steps: keyed(steps, StepTally::finish),
    };
    Ok((history, requests, checks, disagreements))
}

/// Turns a sorted map into breakdown entries, finishing each value with `finish`.
pub(crate) fn keyed<K: ToString, V, T>(
    map: BTreeMap<K, V>,
    finish: impl Fn(V) -> T,
) -> Vec<Keyed<T>> {
    map.into_iter()
        .map(|(key, value)| Keyed {
            key: key.to_string(),
            counts: finish(value),
        })
        .collect()
}

impl fmt::Display for StatsDocument {
    /// Writes the short summary that `autoreview stats` prints without `--json`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let current = &self.current;
        writeln!(f, "Current: {}", state_line(&current.total))?;
        for entry in &current.by_repository {
            writeln!(f, "  {}: {}", entry.key, state_line(&entry.counts))?;
        }
        for entry in &current.by_host {
            writeln!(f, "  host {}: {}", entry.key, state_line(&entry.counts))?;
        }
        writeln!(f, "Pull requests stopped: {}", current.stopped)?;
        let Some(history) = &self.history else {
            return writeln!(
                f,
                "History: none; pass --since YYYY-MM-DD for the daily counts."
            );
        };
        let totals = &history.totals;
        writeln!(
            f,
            "History {} to {} (UTC days):",
            history.since, history.until
        )?;
        writeln!(
            f,
            "  started {}, completed {}, published {}, superseded {}, approval expired {}, \
             failed {}",
            totals.started,
            totals.completed,
            totals.published,
            totals.superseded,
            totals.approval_expired,
            totals.failed
        )?;
        writeln!(
            f,
            "  published: approved without a human {}, approved after a wait {}, \
             changes requested {}, commented {}",
            totals.approved_without_human,
            totals.approved_after_wait,
            totals.changes_requested,
            totals.commented
        )?;
        writeln!(
            f,
            "  notes handed to the author {}, pull requests stopped {}",
            totals.notes_to_author, totals.stopped
        )?;
        if !history.failed_by_code.is_empty() {
            let codes: Vec<String> = history
                .failed_by_code
                .iter()
                .map(|entry| format!("{} {}", entry.key, entry.counts.failed))
                .collect();
            writeln!(f, "  failed by code: {}", codes.join(", "))?;
        }
        writeln!(
            f,
            "  time to publication: median {}, p90 {}",
            seconds_text(totals.publication_median_seconds),
            seconds_text(totals.publication_p90_seconds)
        )?;
        for entry in &history.steps {
            let counts = &entry.counts;
            writeln!(
                f,
                "  step {}: {} runs, {} failed, {} superseded, {} retries, median {}, p90 {}",
                entry.key,
                counts.runs,
                counts.failed,
                counts.superseded,
                counts.retries,
                milliseconds_text(counts.duration_median_ms),
                milliseconds_text(counts.duration_p90_ms)
            )?;
        }
        writeln!(
            f,
            "  completed reviews last updated before {} may be pruned",
            history.pruned_before
        )
    }
}

/// Returns the counts of the current state as one line.
fn state_line(counts: &StateCounts) -> String {
    format!(
        "{} queued, {} running, {} awaiting approval",
        counts.queued, counts.running, counts.awaiting
    )
}

/// Returns seconds as text, or `none` without a published review.
pub(crate) fn seconds_text(seconds: Option<i64>) -> String {
    seconds.map_or_else(|| "none".to_owned(), |value| format!("{value} s"))
}

/// Returns milliseconds as text, or `none` without a succeeded run.
fn milliseconds_text(milliseconds: Option<i64>) -> String {
    milliseconds.map_or_else(|| "none".to_owned(), |value| format!("{value} ms"))
}

impl<I: Integrations, C: Clock> Engine<I, C> {
    /// Returns the statistics document of the store; `since` and `until` are UTC days.
    ///
    /// Reads the store only. `until` is the current UTC day when absent.
    ///
    /// # Errors
    ///
    /// Returns the errors of `StatsRange::parse`, `Store::stats_snapshot` and `compute`.
    pub fn review_stats(
        &self,
        cancel: &CancellationToken,
        since: Option<&str>,
        until: Option<&str>,
    ) -> Result<StatsDocument, Error> {
        let now = self.clock.wall_now().to_offset(UtcOffset::UTC);
        let range = StatsRange::parse(since, until, now.date())?;
        live(cancel)?;
        let bounds = range.map(|range| (range.start, range.end));
        let snapshot = self.store.stats_snapshot(bounds)?;
        compute(&snapshot, range.as_ref(), now, self.prune_cutoff())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::{Value, json};

    use super::{StoredSlot, blocked_alone};

    /// A stored reviewer slot with a result of `findings`.
    fn slot(findings: Value) -> Value {
        json!({"run": 1, "result": {"reviewer_id": "x", "summary": "S", "findings": findings}})
    }

    #[test]
    fn blocked_alone_needs_two_reviewers_and_one_without_a_blocking_claim() {
        let blocking = || slot(json!([{"id": "C001", "severity": "high"}]));
        let note = || slot(json!([{"id": "C002", "severity": "low", "non_blocking": true}]));
        let unfinished = json!({"run": 2, "session": {"name": "s"}});
        let cases = [
            (
                "a blocking claim against none",
                vec![("opus", blocking()), ("sonnet", slot(json!([])))],
                Some(vec!["opus"]),
            ),
            (
                "notes only count as none",
                vec![("opus", note()), ("sonnet", blocking())],
                Some(vec!["sonnet"]),
            ),
            (
                "both block",
                vec![("opus", blocking()), ("sonnet", blocking())],
                Some(Vec::new()),
            ),
            (
                "neither blocks, one result stored with null findings",
                vec![("opus", slot(Value::Null)), ("sonnet", note())],
                Some(Vec::new()),
            ),
            ("one reviewer", vec![("opus", blocking())], None),
            (
                "a slot without a result is no reviewer",
                vec![("opus", blocking()), ("sonnet", unfinished)],
                None,
            ),
            (
                "two block against one",
                vec![
                    ("haiku", slot(json!([]))),
                    ("opus", blocking()),
                    ("sonnet", blocking()),
                ],
                Some(vec!["opus", "sonnet"]),
            ),
        ];
        for (name, slots, want) in cases {
            let slots: BTreeMap<String, StoredSlot> = (slots.into_iter())
                .map(|(id, value)| {
                    let slot = serde_json::from_value(value).expect("the slot decodes");
                    (id.to_owned(), slot)
                })
                .collect();
            assert_eq!(blocked_alone(&slots), want, "{name}");
        }
    }
}
