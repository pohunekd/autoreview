//! Review statistics: the current state and a daily history, read from the store only.
//!
//! One review row is one attempt: a retry is a new row, so `started` and `failed` count
//! attempts. Every count comes from the `reviews` table; nothing asks GitHub or pohunek.

// Rust guideline compliant 2026-06-26

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use autoreview_go_compat::go_time::{format_rfc3339_seconds, parse_rfc3339};
use autoreview_schedule::scheduler::superseded;
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
    REVIEW_RUNNING, StoredReview,
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
    /// Counts per repository, sorted by name.
    pub by_repository: Vec<Keyed<StateCounts>>,
    /// Counts per review host chosen at dispatch, sorted by name.
    pub by_host: Vec<Keyed<StateCounts>>,
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
enum Ending {
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
enum Published {
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

/// Builds the document from the review rows that `Store::stats_reviews` returns.
///
/// The current state counts every queued and running row and every awaiting row without an
/// approval decision. With `range`, an attempt counts as started on the UTC day of
/// `created_at` and as completed on the UTC day of `completed_at`, each only inside the range.
///
/// # Errors
///
/// Returns `store_state_invalid` for a timestamp or an outcome that does not decode, and for
/// a completed row without `completed_at`.
pub fn compute(
    reviews: &[StoredReview],
    range: Option<&StatsRange>,
    generated_at: OffsetDateTime,
    pruned_before: OffsetDateTime,
) -> Result<StatsDocument, Error> {
    let current = current_state(reviews);
    let history = match range {
        Some(range) => Some(history(reviews, range, pruned_before)?),
        None => None,
    };
    Ok(StatsDocument {
        schema_version: STATS_SCHEMA_VERSION,
        generated_at: format_rfc3339_seconds(generated_at),
        current,
        history,
    })
}

/// Counts the queued, running and undecided awaiting rows, in total, per repository and per
/// host.
///
/// An awaiting row counts only while it waits for a decision, as `autoreview awaiting` lists
/// it; a decided wait that `serve` has not continued yet counts nowhere.
fn current_state(reviews: &[StoredReview]) -> CurrentState {
    let mut total = StateCounts::default();
    let mut repositories: BTreeMap<String, StateCounts> = BTreeMap::new();
    let mut hosts: BTreeMap<String, StateCounts> = BTreeMap::new();
    for review in reviews {
        if review.status == REVIEW_COMPLETED
            || (review.status == REVIEW_AWAITING && !review.awaits_decision())
        {
            continue;
        }
        let count = |counts: &mut StateCounts| match review.status.as_str() {
            REVIEW_QUEUED => counts.queued += 1,
            REVIEW_RUNNING => counts.running += 1,
            REVIEW_AWAITING => counts.awaiting += 1,
            _ => {}
        };
        count(&mut total);
        let input = &review.input;
        count(
            repositories
                .entry(input.pull_request.repository.clone())
                .or_default(),
        );
        count(hosts.entry(input.route.host.clone()).or_default());
    }
    CurrentState {
        total,
        by_repository: keyed(repositories, |counts| counts),
        by_host: keyed(hosts, |counts| counts),
    }
}

/// Counts the starts and completions inside `range`.
fn history(
    reviews: &[StoredReview],
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
        if review.completed_at.is_empty() {
            return Err(state_invalid(review, "is completed without completed_at"));
        }
        let completed = stored_instant(review, "completed_at", &review.completed_at)?;
        if !range.contains(completed.date()) {
            continue;
        }
        let elapsed = (completed - created).whole_seconds();
        let ending = ending(review, (elapsed >= 0).then_some(elapsed))?;
        tally.add(review, completed.date(), |bucket| bucket.complete(ending));
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
            "  time to publication: median {}, p90 {}",
            seconds_text(totals.publication_median_seconds),
            seconds_text(totals.publication_p90_seconds)
        )?;
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

impl<I: Integrations, C: Clock> Engine<I, C> {
    /// Returns the statistics document of the store; `since` and `until` are UTC days.
    ///
    /// Reads the store only. `until` is the current UTC day when absent.
    ///
    /// # Errors
    ///
    /// Returns the errors of `StatsRange::parse`, `Store::stats_reviews` and `compute`.
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
        let reviews = self.store.stats_reviews(bounds)?;
        compute(&reviews, range.as_ref(), now, self.prune_cutoff())
    }
}

#[cfg(test)]
mod tests {
    use autoreview_policy::Reviewer;
    use serde_json::{Value, json};
    use time::macros::{date, datetime};

    use super::{StatsRange, compute};
    use crate::error_code;
    use crate::store::StoredReview;

    /// The counter names in the order of the arrays that `counters` takes.
    const COUNTER_NAMES: [&str; 10] = [
        "started",
        "completed",
        "published",
        "approved_without_human",
        "approved_after_wait",
        "changes_requested",
        "commented",
        "superseded",
        "approval_expired",
        "failed",
    ];

    /// Returns a queued review of `repository` under the rule `acme/*` on `host-a`, by `opus`.
    fn review(id: &str, repository: &str, created_at: &str) -> StoredReview {
        let mut review = StoredReview {
            workflow_id: id.to_owned(),
            key: format!("{repository}#1"),
            status: "queued".to_owned(),
            created_at: created_at.to_owned(),
            updated_at: created_at.to_owned(),
            ..StoredReview::default()
        };
        review.input.pull_request.repository = repository.to_owned();
        review.input.rule.match_glob = "acme/*".to_owned();
        review.input.route.host = "host-a".to_owned();
        review.input.reviewers = reviewers(&["opus"]);
        review
    }

    /// Returns reviewers with the IDs `ids`.
    fn reviewers(ids: &[&str]) -> Vec<Reviewer> {
        ids.iter()
            .map(|id| Reviewer {
                id: (*id).to_owned(),
                ..Reviewer::default()
            })
            .collect()
    }

    /// Completes `review` at `completed_at` with `result`, the failure code and the outcome.
    fn done(
        mut review: StoredReview,
        completed_at: &str,
        result: &str,
        failure: &str,
        outcome: &str,
    ) -> StoredReview {
        review.status = "completed".to_owned();
        review.result = result.to_owned();
        review.failure = failure.to_owned();
        review.outcome = outcome.to_owned();
        review.completed_at = completed_at.to_owned();
        review
    }

    /// Returns the stored outcome text of a publication with `decision`.
    fn published(decision: &str) -> String {
        format!(
            "{{\"outcome\":\"published\",\"publication\":{{\"decision\":\"{decision}\",\
             \"github_review_id\":1,\"review_url\":\"https://example.invalid/r/1\",\
             \"reused\":false}}}}"
        )
    }

    /// Returns one counters object: its key (empty for the totals), ten counts, two percentiles.
    fn counters(key: &str, counts: [i64; 10], median: Option<i64>, p90: Option<i64>) -> Value {
        let mut value = json!({
            "publication_median_seconds": median,
            "publication_p90_seconds": p90,
        });
        for (name, count) in COUNTER_NAMES.into_iter().zip(counts) {
            value[name] = json!(count);
        }
        if !key.is_empty() {
            value["key"] = json!(key);
        }
        value
    }

    /// Returns one current-state entry.
    fn state(key: &str, queued: i64, running: i64, awaiting: i64) -> Value {
        json!({ "key": key, "queued": queued, "running": running, "awaiting": awaiting })
    }

    /// The fixed store of the table test: one row per case that the counters distinguish.
    fn rows() -> Vec<StoredReview> {
        let widget = "acme/widget";
        let gadget = "acme/gadget";
        let mut running = review("r1", gadget, "2026-10-20T00:00:00Z");
        running.status = "running".to_owned();
        running.input.route.host = "host-b".to_owned();
        running.input.rule.match_glob = gadget.to_owned();
        let mut awaiting = review("a1", widget, "2026-11-01T10:00:00Z");
        awaiting.status = "awaiting".to_owned();
        let mut decided = review("a2", widget, "2026-10-30T10:00:00Z");
        decided.status = "awaiting".to_owned();
        decided.approval = "approve".to_owned();
        let mut both = review("p1", widget, "2026-11-01T00:00:00Z");
        both.input.reviewers = reviewers(&["opus", "sonnet"]);
        let mut waited = review("p2", widget, "2026-11-01T01:00:00Z");
        waited.approval = "approve".to_owned();
        let mut before = review("p3", gadget, "2026-10-31T23:59:00Z");
        before.input.rule.match_glob = gadget.to_owned();
        let mut twice = review("p4", gadget, "2026-11-02T00:00:00Z");
        twice.input.rule.match_glob = gadget.to_owned();
        twice.input.reviewers = reviewers(&["opus", "opus"]);
        let mut replaced = review("s2", widget, "2026-11-01T03:00:00Z");
        replaced.approval = "superseded".to_owned();
        let mut expired = review("e1", widget, "2026-11-01T04:00:00Z");
        expired.approval = "expired".to_owned();
        let mut failed = review("f1", gadget, "2026-11-01T05:00:00Z");
        failed.input.reviewers = reviewers(&["codex"]);
        let success = "success";
        let failure = "failure";
        vec![
            review("q1", widget, "2026-11-02T09:00:00Z"),
            running,
            awaiting,
            // A decided wait that serve has not continued yet; not in the current state.
            decided,
            // 100 seconds; an approval that never waited.
            done(
                both,
                "2026-11-01T00:01:40Z",
                success,
                "",
                &published("APPROVE"),
            ),
            // One day; approved through the approve command.
            done(
                waited,
                "2026-11-02T01:00:00Z",
                success,
                "",
                &published("APPROVE"),
            ),
            // Started before the range; 90.5 seconds count as 90.
            done(
                before,
                "2026-11-01T00:00:30.5Z",
                success,
                "",
                &published("REQUEST_CHANGES"),
            ),
            done(
                twice,
                "2026-11-02T00:05:00Z",
                success,
                "",
                &published("COMMENT"),
            ),
            // A decision this engine does not know, completed before it started.
            done(
                review("u1", widget, "2026-11-02T10:00:00Z"),
                "2026-11-02T09:59:00Z",
                success,
                "",
                &published("DISMISS"),
            ),
            // An outcome this engine does not know.
            done(
                review("u2", widget, "2026-11-02T11:00:00Z"),
                "2026-11-02T11:00:00Z",
                success,
                "",
                "{\"outcome\":\"abandoned\"}",
            ),
            done(
                review("s1", widget, "2026-11-01T02:00:00Z"),
                "2026-11-01T02:10:00Z",
                failure,
                "head_changed",
                "",
            ),
            done(
                replaced,
                "2026-11-02T03:00:00Z",
                success,
                "",
                "{\"outcome\":\"superseded\"}",
            ),
            done(
                expired,
                "2026-11-02T04:00:00Z",
                success,
                "",
                "{\"outcome\":\"approval_expired\"}",
            ),
            done(
                failed,
                "2026-11-01T05:00:05Z",
                failure,
                "pohunek_session_start_failed",
                "",
            ),
            // Completed after the range.
            done(
                review("l1", widget, "2026-11-02T23:00:00Z"),
                "2026-11-03T00:00:01Z",
                failure,
                "github_request_failed",
                "",
            ),
        ]
    }

    /// The counters and percentiles of every bucket over the fixed store.
    #[test]
    fn review_stats_count_every_bucket_of_a_fixed_store() {
        let range = StatsRange::parse(
            Some("2026-11-01"),
            Some("2026-11-02"),
            date!(2026 - 11 - 10),
        )
        .expect("the range is valid")
        .expect("the range is set");
        let document = compute(
            &rows(),
            Some(&range),
            datetime!(2026-11-03 08:00:00.25 UTC),
            datetime!(2026-10-04 08:00:00 UTC),
        )
        .expect("the rows decode");
        let day = Some(86_400);
        let days = vec![
            counters(
                "2026-11-01",
                [7, 4, 2, 1, 0, 1, 0, 1, 0, 1],
                Some(90),
                Some(100),
            ),
            counters("2026-11-02", [5, 6, 3, 0, 1, 0, 1, 1, 1, 0], Some(300), day),
        ];
        let by_repository = vec![
            counters(
                "acme/gadget",
                [2, 3, 2, 0, 0, 1, 1, 0, 0, 1],
                Some(90),
                Some(300),
            ),
            counters(
                "acme/widget",
                [10, 7, 3, 1, 1, 0, 0, 2, 1, 0],
                Some(100),
                day,
            ),
        ];
        let by_rule = vec![
            counters("acme/*", [11, 8, 3, 1, 1, 0, 0, 2, 1, 1], Some(100), day),
            counters(
                "acme/gadget",
                [1, 2, 2, 0, 0, 1, 1, 0, 0, 0],
                Some(90),
                Some(300),
            ),
        ];
        let by_reviewer = vec![
            counters("codex", [1, 1, 0, 0, 0, 0, 0, 0, 0, 1], None, None),
            counters("opus", [11, 9, 5, 1, 1, 1, 1, 2, 1, 0], Some(100), day),
            counters(
                "sonnet",
                [1, 1, 1, 1, 0, 0, 0, 0, 0, 0],
                Some(100),
                Some(100),
            ),
        ];
        let expected = json!({
            "schema_version": 1,
            "generated_at": "2026-11-03T08:00:00Z",
            "current": {
                "queued": 1,
                "running": 1,
                "awaiting": 1,
                "by_repository": [state("acme/gadget", 0, 1, 0), state("acme/widget", 1, 0, 1)],
                "by_host": [state("host-a", 1, 0, 1), state("host-b", 0, 1, 0)],
            },
            "history": {
                "since": "2026-11-01",
                "until": "2026-11-02",
                "pruned_before": "2026-10-04T08:00:00Z",
                "totals": counters("", [12, 10, 5, 1, 1, 1, 1, 2, 1, 1], Some(100), day),
                "days": days,
                "by_repository": by_repository,
                "by_rule": by_rule,
                "by_reviewer": by_reviewer,
            },
        });
        let actual = serde_json::to_value(&document).expect("the document serializes");
        assert_eq!(actual, expected);
    }

    /// One range case: since, until, and the accepted (since, until) or the refusal code.
    type RangeCase = (
        Option<&'static str>,
        Option<&'static str>,
        Result<Option<(&'static str, &'static str)>, &'static str>,
    );

    /// Ranges of whole UTC days: `until` defaults to today, at most 366 days, strict days only.
    #[test]
    fn stats_range_takes_whole_utc_days_up_to_the_limit() {
        let today = date!(2026 - 11 - 10);
        let cases: [RangeCase; 17] = [
            (None, None, Ok(None)),
            (None, Some("2026-11-01"), Err("stats_range_invalid")),
            (
                Some("2026-11-01"),
                None,
                Ok(Some(("2026-11-01", "2026-11-10"))),
            ),
            (
                Some("2026-11-01"),
                Some("2026-11-01"),
                Ok(Some(("2026-11-01", "2026-11-01"))),
            ),
            (
                Some("2025-11-10"),
                None,
                Ok(Some(("2025-11-10", "2026-11-10"))),
            ),
            (Some("2025-11-09"), None, Err("stats_range_too_long")),
            (
                Some("2025-11-08"),
                Some("2026-11-09"),
                Err("stats_range_too_long"),
            ),
            (
                Some("2026-11-01"),
                Some("2027-11-02"),
                Err("stats_range_invalid"),
            ),
            (
                Some("2026-11-02"),
                Some("2026-11-01"),
                Err("stats_range_invalid"),
            ),
            (Some("2026-11-11"), None, Err("stats_range_invalid")),
            (
                Some("2026-11-01"),
                Some("2026-11-11"),
                Err("stats_range_invalid"),
            ),
            (
                Some("2026-11-11"),
                Some("2026-11-12"),
                Err("stats_range_invalid"),
            ),
            (Some("2026-02-30"), None, Err("stats_range_invalid")),
            (Some("2026-1-01"), None, Err("stats_range_invalid")),
            (Some("+2026-11-0"), None, Err("stats_range_invalid")),
            (
                Some("2026-11-01T00:00:00Z"),
                None,
                Err("stats_range_invalid"),
            ),
            (
                Some("2026-11-01"),
                Some("20261102"),
                Err("stats_range_invalid"),
            ),
        ];
        for (since, until, expected) in cases {
            let actual = match StatsRange::parse(since, until, today) {
                Ok(range) => {
                    Ok(range.map(|range| (range.since.to_string(), range.until.to_string())))
                }
                Err(error) => Err(error_code(&error).to_owned()),
            };
            let expected = expected
                .map(|range| range.map(|(from, to)| (from.to_owned(), to.to_owned())))
                .map_err(str::to_owned);
            assert_eq!(actual, expected, "{since:?} {until:?}");
        }
    }
}
