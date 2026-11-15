//! Review statistics: the current state and a daily history, read from the store only.
//!
//! One review row is one attempt: a retry is a new row, so `started` and `failed` count
//! attempts. The counts come from the `reviews` and `review_step_runs` tables and the schedule
//! records of the discovery state; nothing asks GitHub or pohunek.

// Rust guideline compliant 2026-06-26

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use autoreview_go_compat::go_time::{format_rfc3339_seconds, parse_rfc3339};
use autoreview_schedule::phase;
use autoreview_schedule::scheduler::{Record, superseded};
use serde::Serialize;
use time::{Date, OffsetDateTime, UtcOffset};
use tokio_util::sync::CancellationToken;

use crate::discovery::poll::live;
use crate::engine::{Clock, Engine};
use crate::error::Error;
use crate::integrations::Integrations;
use crate::model::review::APPROVAL_KIND_APPROVE;
use crate::model::{Decision, Outcome, ReviewOutcome};
use crate::store::{
    RESULT_FAILURE, RESULT_SUCCESS, REVIEW_AWAITING, REVIEW_COMPLETED, REVIEW_QUEUED,
    REVIEW_RUNNING, StepOutcome, StepRun, StoredReview,
};

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
fn parse_day(text: &str) -> Result<Date, Error> {
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

/// Counts of review attempts that started or completed in one bucket.
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

/// The counts of one bucket while the rows are read, with the publication seconds.
#[derive(Debug, Default)]
struct Tally {
    /// The counts; the two percentiles are filled by `finish`.
    counters: Counters,
    /// Seconds to publication of every published attempt with a valid duration.
    seconds: Vec<i64>,
}

impl Tally {
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
        self.seconds.sort_unstable();
        self.counters.publication_median_seconds = nearest_rank(&self.seconds, MEDIAN_PERCENT);
        self.counters.publication_p90_seconds = nearest_rank(&self.seconds, P90_PERCENT);
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
    let cause = format!("review {} {reason}", review.workflow_id);
    Error::workflow("store_state_invalid", true, Some(cause.into()))
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

/// The buckets of a history while the rows are read.
#[derive(Debug, Default)]
struct HistoryTally {
    /// The whole range.
    totals: Tally,
    /// One bucket per UTC day of the range.
    days: BTreeMap<Date, Tally>,
    /// One bucket per repository.
    repositories: BTreeMap<String, Tally>,
    /// One bucket per rule `match`.
    rules: BTreeMap<String, Tally>,
    /// One bucket per reviewer ID.
    reviewers: BTreeMap<String, Tally>,
    /// The `failed` attempts per stored error code.
    failures: BTreeMap<String, i64>,
}

impl HistoryTally {
    /// Applies `count` to the total, the day, and the breakdowns of `review`.
    fn add(&mut self, review: &StoredReview, day: Date, count: impl Fn(&mut Tally)) {
        let input = &review.input;
        count(&mut self.totals);
        count(self.days.entry(day).or_default());
        count(
            self.repositories
                .entry(input.pull_request.repository.clone())
                .or_default(),
        );
        count(self.rules.entry(input.rule.match_glob.clone()).or_default());
        let reviewers: BTreeSet<&str> = input.reviewers.iter().map(|r| r.id.as_str()).collect();
        for reviewer in reviewers {
            count(self.reviewers.entry(reviewer.to_owned()).or_default());
        }
    }
}

/// Builds the document from review rows, step runs and schedule records.
///
/// The rows, the step runs and the schedule records are those of one `Store::stats_snapshot`.
/// The current state counts every queued and running row, every awaiting row without an
/// approval decision, and every schedule record in phase `failed`. With `range`, an attempt
/// counts as started on the UTC day of `created_at` and as completed on the UTC day of
/// `completed_at`, each only inside the range, and every step run in `steps` counts; the
/// snapshot holds only the runs that ended inside the range.
///
/// # Errors
///
/// Returns `store_state_invalid` for a timestamp or an outcome that does not decode, for a
/// completed row without `completed_at`, and for a schedule key without `#`.
pub fn compute(
    reviews: &[StoredReview],
    steps: &[StepRun],
    schedule: &BTreeMap<String, Record>,
    range: Option<&StatsRange>,
    generated_at: OffsetDateTime,
    pruned_before: OffsetDateTime,
) -> Result<StatsDocument, Error> {
    let current = current_state(reviews, schedule)?;
    let history = match range {
        Some(range) => Some(history(reviews, steps, range, pruned_before)?),
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

/// Counts the starts and completions inside `range`, and the step runs of `steps`.
fn history(
    reviews: &[StoredReview],
    steps: &[StepRun],
    range: &StatsRange,
    pruned_before: OffsetDateTime,
) -> Result<History, Error> {
    let mut tally = HistoryTally::default();
    let mut day = range.since;
    while day <= range.until {
        tally.days.insert(day, Tally::default());
        let Some(next) = day.next_day() else {
            break;
        };
        day = next;
    }
    for review in reviews {
        let created = stored_instant(review, "created_at", &review.created_at)?;
        if range.contains(created.date()) {
            tally.add(review, created.date(), Tally::start);
        }
        if review.status != REVIEW_COMPLETED {
            continue;
        }
        let completed = completed_instant(review)?;
        if !range.contains(completed.date()) {
            continue;
        }
        let ending = ending(review, elapsed_seconds(created, completed))?;
        tally.add(review, completed.date(), |bucket| bucket.complete(ending));
        if ending == Ending::Failed {
            *tally.failures.entry(review.failure.clone()).or_default() += 1;
        }
    }
    let mut step_tallies: BTreeMap<&str, StepTally> = BTreeMap::new();
    for run in steps {
        step_tallies.entry(run.step.as_str()).or_default().add(run);
    }
    Ok(History {
        since: range.since.to_string(),
        until: range.until.to_string(),
        pruned_before: format_rfc3339_seconds(pruned_before),
        totals: tally.totals.finish(),
        days: keyed(tally.days, Tally::finish),
        by_repository: keyed(tally.repositories, Tally::finish),
        by_rule: keyed(tally.rules, Tally::finish),
        by_reviewer: keyed(tally.reviewers, Tally::finish),
        failed_by_code: keyed(tally.failures, |failed| FailedCount { failed }),
        steps: keyed(step_tallies, StepTally::finish),
    })
}

/// Turns a sorted map into breakdown entries, finishing each value with `finish`.
fn keyed<K: ToString, V, T>(map: BTreeMap<K, V>, finish: impl Fn(V) -> T) -> Vec<Keyed<T>> {
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
fn seconds_text(seconds: Option<i64>) -> String {
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
        let (reviews, discovery, steps) = self.store.stats_snapshot(bounds)?;
        compute(
            &reviews,
            &steps,
            &discovery.schedule,
            range.as_ref(),
            now,
            self.prune_cutoff(),
        )
    }
}
