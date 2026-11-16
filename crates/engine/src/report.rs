//! The review report: timing, decisions and finding outcomes of a range, from the store only.
//!
//! The review counts come from the statistics history of the range; the finding counts come
//! from the outcome rows of the findings published in it, with the outcome each has now.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeMap;
use std::fmt;

use autoreview_go_compat::go_time::format_rfc3339_seconds;
use serde::Serialize;
use time::UtcOffset;
use tokio_util::sync::CancellationToken;

use crate::discovery::poll::live;
use crate::engine::{Clock, Engine};
use crate::error::Error;
use crate::integrations::Integrations;
use crate::model::repository_valid;
use crate::stats::{
    Counters, Keyed, STATS_RANGE_INVALID, StatsRange, first_publications, history, keyed,
    median_and_p90, seconds_text, store_invalid,
};
use crate::store::outcome_checks::{OUTCOME_OPEN_AT_CLOSE, OUTCOME_OPEN_AT_MERGE};
use crate::store::rollup::{
    COUNTER_COMPLETED, COUNTER_STARTED, DIMENSION_DAY, DIMENSION_PIPELINE, DIMENSION_REPOSITORY,
    PIPELINE_DIMENSIONS,
};
use crate::store::{
    FindingOutcome, OUTCOME_ANSWERED, OUTCOME_FIXED, OUTCOME_OPEN, Rollup, RollupCount,
    RollupSample, SnapshotReads,
};

/// Version of the report document; a changed field gets a new version.
pub const REPORT_SCHEMA_VERSION: i64 = 2;

/// Error code of a `--pipeline` that no rule of the policy runs.
pub const PIPELINE_NOT_CONFIGURED: &str = "pipeline_not_configured";

/// Error code of a range that starts before the store's retention of the data it needs.
pub const REPORT_RANGE_PRUNED: &str = "report_range_pruned";

/// Error code of a malformed repository name.
const REPOSITORY_NOT_ALLOWED: &str = "repository_not_allowed";

/// Outcome of a blocking finding that the verifier refuted.
const OUTCOME_REFUTED: &str = "refuted";

/// Outcome of a blocking finding that the verifier found not blocking.
const OUTCOME_NOT_BLOCKING: &str = "not_blocking";

/// What `review_report` reports on: a range of UTC days and optional filters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReportRequest {
    /// First UTC day, `YYYY-MM-DD`.
    pub since: String,
    /// Last UTC day, `YYYY-MM-DD`; today (UTC) when absent.
    pub until: Option<String>,
    /// Repository as lowercase `owner/name`; every repository when absent.
    pub repository: Option<String>,
    /// Name of a policy pipeline; every pipeline when absent.
    pub pipeline: Option<String>,
}

/// The report document that `autoreview report --json` prints.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReportDocument {
    /// Version of this document's fields, `REPORT_SCHEMA_VERSION`.
    pub schema_version: i64,
    /// When the document was computed, RFC 3339 in UTC without a fraction.
    pub generated_at: String,
    /// First UTC day of the range, `YYYY-MM-DD`.
    pub since: String,
    /// Last UTC day of the range, `YYYY-MM-DD`.
    pub until: String,
    /// The repository counted; `null` for every repository.
    pub repository: Option<String>,
    /// The pipeline counted; `null` for every pipeline.
    pub pipeline: Option<String>,
    /// Timing and decisions of the reviews completed in the range.
    pub reviews: ReviewMetrics,
    /// Outcomes of the findings published in the range.
    pub findings: FindingMetrics,
}

/// Timing and decisions of the review attempts completed in the range.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ReviewMetrics {
    /// Attempts that published a review on GitHub.
    pub published: i64,
    /// Approval waits that ended at their deadline without a publication.
    pub approval_expired: i64,
    /// Attempts that reached a decision: `published` plus `approval_expired`.
    pub decided: i64,
    /// Published approvals of reviews that never waited for an approval.
    pub approved_without_human: i64,
    /// Published approvals of waits that the `approve` command approved.
    pub approved_after_wait: i64,
    /// Published reviews that requested changes.
    pub changes_requested: i64,
    /// Published reviews that commented.
    pub commented: i64,
    /// `approved_without_human` in percent of `decided`, one decimal.
    pub approved_without_human_percent: Option<f64>,
    /// `approved_after_wait` plus `approval_expired` in percent of `decided`, one decimal.
    pub waited_for_human_percent: Option<f64>,
    /// `changes_requested` in percent of `decided`, one decimal.
    pub changes_requested_percent: Option<f64>,
    /// `commented` in percent of `decided`, one decimal.
    pub commented_percent: Option<f64>,
    /// Median seconds from the creation of a published attempt to its completion.
    pub attempt_to_publication_median_seconds: Option<i64>,
    /// 90th percentile of the same seconds, nearest rank.
    pub attempt_to_publication_p90_seconds: Option<i64>,
    /// Median seconds from the review request of a published attempt to its completion.
    ///
    /// Only attempts with a known request time count; a label trigger has none.
    pub request_to_publication_median_seconds: Option<i64>,
    /// 90th percentile of the same seconds, nearest rank.
    pub request_to_publication_p90_seconds: Option<i64>,
    /// Published attempts with the results of two or more reviewers.
    pub multi_reviewer: i64,
    /// Those where one reviewer made a blocking claim and another none.
    pub disagreed: i64,
    /// `disagreed` in percent of `multi_reviewer`, one decimal.
    pub disagreed_percent: Option<f64>,
    /// The verifier's verdicts and the disagreement per reviewer ID, sorted by it.
    pub by_reviewer: Vec<Keyed<ReviewerCounts>>,
}

/// The verifier's verdicts on one reviewer's blocking findings, and its lone blocking claims.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ReviewerCounts {
    /// Findings that a finished verification gave a verdict.
    pub verifier_checked: i64,
    /// Checked findings that the verifier found not blocking or refuted.
    pub verifier_demoted: i64,
    /// `verifier_demoted` in percent of `verifier_checked`, one decimal.
    pub verifier_demoted_percent: Option<f64>,
    /// Disagreeing attempts in which this reviewer made a blocking claim.
    pub blocked_alone: i64,
}

impl ReviewerCounts {
    /// Returns the counts with `verifier_demoted_percent`.
    fn finish(mut self) -> Self {
        self.verifier_demoted_percent = percent(self.verifier_demoted, self.verifier_checked);
        self
    }
}

/// Outcome counts of the findings published in the range: totals and breakdowns.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct FindingMetrics {
    /// Counts over every finding.
    pub total: OutcomeCounts,
    /// Counts per reviewer ID, the first reviewer of a merged finding, sorted by it.
    pub by_reviewer: Vec<Keyed<OutcomeCounts>>,
    /// Counts per severity, sorted by it.
    pub by_severity: Vec<Keyed<OutcomeCounts>>,
    /// Counts per repository, sorted by name.
    pub by_repository: Vec<Keyed<OutcomeCounts>>,
}

/// Counts of findings by the outcome each has now.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct OutcomeCounts {
    /// Findings published, whatever their outcome.
    pub findings: i64,
    /// Findings that held the merge back when they were published.
    pub blocking: i64,
    /// Findings that nothing has settled yet.
    pub open: i64,
    /// Findings that a later review settled as fixed.
    pub fixed: i64,
    /// Findings answered without an upheld check.
    pub answered: i64,
    /// Blocking findings that the verifier refuted.
    pub refuted: i64,
    /// Blocking findings that the verifier found not blocking.
    pub not_blocking: i64,
    /// Findings still open when their pull request was merged.
    pub open_at_merge: i64,
    /// Findings still open when their pull request was closed unmerged.
    pub open_at_close: i64,
    /// `fixed` in percent of `fixed` plus `open_at_merge`, one decimal.
    pub fixed_percent: Option<f64>,
}

impl OutcomeCounts {
    /// Counts one finding; an outcome this version does not know counts in `findings` only.
    fn add(&mut self, outcome: &FindingOutcome) {
        self.findings += 1;
        self.blocking += i64::from(outcome.blocking);
        let counter = match outcome.outcome.as_str() {
            OUTCOME_OPEN => &mut self.open,
            OUTCOME_FIXED => &mut self.fixed,
            OUTCOME_ANSWERED => &mut self.answered,
            OUTCOME_REFUTED => &mut self.refuted,
            OUTCOME_NOT_BLOCKING => &mut self.not_blocking,
            OUTCOME_OPEN_AT_MERGE => &mut self.open_at_merge,
            OUTCOME_OPEN_AT_CLOSE => &mut self.open_at_close,
            _ => return,
        };
        *counter += 1;
    }

    /// Returns the counts with `fixed_percent`.
    fn finish(mut self) -> Self {
        self.fixed_percent = percent(self.fixed, self.fixed + self.open_at_merge);
        self
    }
}

/// Returns `part` in percent of `whole`, rounded half up to one decimal; `None` for no whole.
fn percent(part: i64, whole: i64) -> Option<f64> {
    if whole <= 0 {
        return None;
    }
    let tenths = (part * 2000 + whole) / (whole * 2);
    // A count of findings or reviews is far below 2^53, so the conversion is exact.
    Some(tenths as f64 / 10.0)
}

/// Returns the timing and decision metrics of one bucket of the statistics history.
///
/// `request_seconds` are the seconds from the review request to publication of the bucket,
/// `multi_reviewer` and `disagreed` its attempts with two or more reviewers and those that
/// disagreed, and `reviewers` the counts per reviewer ID, whose shares this fills.
pub fn review_metrics(
    counters: &Counters,
    request_seconds: Vec<i64>,
    (multi_reviewer, disagreed): (i64, i64),
    reviewers: BTreeMap<String, ReviewerCounts>,
) -> ReviewMetrics {
    let decided = counters.published + counters.approval_expired;
    let waited = counters.approved_after_wait + counters.approval_expired;
    let (request_median, request_p90) = median_and_p90(request_seconds);
    ReviewMetrics {
        published: counters.published,
        approval_expired: counters.approval_expired,
        decided,
        approved_without_human: counters.approved_without_human,
        approved_after_wait: counters.approved_after_wait,
        changes_requested: counters.changes_requested,
        commented: counters.commented,
        approved_without_human_percent: percent(counters.approved_without_human, decided),
        waited_for_human_percent: percent(waited, decided),
        changes_requested_percent: percent(counters.changes_requested, decided),
        commented_percent: percent(counters.commented, decided),
        attempt_to_publication_median_seconds: counters.publication_median_seconds,
        attempt_to_publication_p90_seconds: counters.publication_p90_seconds,
        request_to_publication_median_seconds: request_median,
        request_to_publication_p90_seconds: request_p90,
        multi_reviewer,
        disagreed,
        disagreed_percent: percent(disagreed, multi_reviewer),
        by_reviewer: keyed(reviewers, ReviewerCounts::finish),
    }
}

/// Counts the outcomes of `repository` and `pipeline`, or of every one when `None`.
pub fn finding_metrics(
    outcomes: &[FindingOutcome],
    repository: Option<&str>,
    pipeline: Option<&str>,
) -> FindingMetrics {
    let mut total = OutcomeCounts::default();
    let mut reviewers: BTreeMap<String, OutcomeCounts> = BTreeMap::new();
    let mut severities: BTreeMap<String, OutcomeCounts> = BTreeMap::new();
    let mut repositories: BTreeMap<String, OutcomeCounts> = BTreeMap::new();
    let selected = outcomes.iter().filter(|outcome| {
        repository.is_none_or(|name| outcome.repository == name)
            && pipeline.is_none_or(|name| outcome.pipeline == name)
    });
    for outcome in selected {
        total.add(outcome);
        for (map, key) in [
            (&mut reviewers, &outcome.reviewer_id),
            (&mut severities, &outcome.severity),
            (&mut repositories, &outcome.repository),
        ] {
            map.entry(key.clone()).or_default().add(outcome);
        }
    }
    FindingMetrics {
        total: total.finish(),
        by_reviewer: keyed(reviewers, OutcomeCounts::finish),
        by_severity: keyed(severities, OutcomeCounts::finish),
        by_repository: keyed(repositories, OutcomeCounts::finish),
    }
}

/// Column headers of the findings table, in the order of `OutcomeCounts::cells`.
const COLUMNS: [&str; 7] = [
    "findings",
    "blocking",
    "open",
    "fixed",
    "open_at_merge",
    "fixed%",
    "demoted",
];

/// Header of the label column of the findings table.
const LABEL_HEADER: &str = "Findings";

impl OutcomeCounts {
    /// Returns the cells of one table row; `demoted` is `refuted` plus `not_blocking`.
    fn cells(&self) -> [String; 7] {
        [
            self.findings.to_string(),
            self.blocking.to_string(),
            self.open.to_string(),
            self.fixed.to_string(),
            self.open_at_merge.to_string(),
            percent_text(self.fixed_percent),
            (self.refuted + self.not_blocking).to_string(),
        ]
    }
}

/// Returns a percentage as text with one decimal, or `none` without a whole.
fn percent_text(value: Option<f64>) -> String {
    value.map_or_else(|| "none".to_owned(), |value| format!("{value:.1}%"))
}

impl fmt::Display for ReportDocument {
    /// Writes the short text table that `autoreview report` prints without `--json`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let every = |value: &Option<String>| value.clone().unwrap_or_else(|| "every".to_owned());
        writeln!(
            f,
            "Report {} to {} (UTC days), repository {}, pipeline {}",
            self.since,
            self.until,
            every(&self.repository),
            every(&self.pipeline)
        )?;
        let reviews = &self.reviews;
        writeln!(
            f,
            "Reviews: {} decided, {} published, {} approval expired",
            reviews.decided, reviews.published, reviews.approval_expired
        )?;
        writeln!(
            f,
            "  approved without a human {}, waited for a human {}",
            percent_text(reviews.approved_without_human_percent),
            percent_text(reviews.waited_for_human_percent)
        )?;
        writeln!(
            f,
            "  changes requested {}, commented {}",
            percent_text(reviews.changes_requested_percent),
            percent_text(reviews.commented_percent)
        )?;
        writeln!(
            f,
            "  attempt creation to publication: median {}, p90 {}",
            seconds_text(reviews.attempt_to_publication_median_seconds),
            seconds_text(reviews.attempt_to_publication_p90_seconds)
        )?;
        writeln!(
            f,
            "  review request to publication: median {}, p90 {}",
            seconds_text(reviews.request_to_publication_median_seconds),
            seconds_text(reviews.request_to_publication_p90_seconds)
        )?;
        writeln!(
            f,
            "  reviewers disagreed: {} of {} attempts with two or more reviewers, {}",
            reviews.disagreed,
            reviews.multi_reviewer,
            percent_text(reviews.disagreed_percent)
        )?;
        let reviewers = &reviews.by_reviewer;
        for entry in reviewers.iter().filter(|e| e.counts.verifier_checked > 0) {
            let counts = &entry.counts;
            writeln!(
                f,
                "  verifier on reviewer {}: {} checked, {} demoted, {}",
                entry.key,
                counts.verifier_checked,
                counts.verifier_demoted,
                percent_text(counts.verifier_demoted_percent)
            )?;
        }
        for entry in reviewers.iter().filter(|e| e.counts.blocked_alone > 0) {
            let (key, alone) = (&entry.key, entry.counts.blocked_alone);
            writeln!(f, "  reviewer {key} blocked alone: {alone}")?;
        }
        let findings = &self.findings;
        let mut rows = vec![("total".to_owned(), &findings.total)];
        for (kind, entries) in [
            ("reviewer", &findings.by_reviewer),
            ("severity", &findings.by_severity),
            ("repository", &findings.by_repository),
        ] {
            for entry in entries {
                rows.push((format!("{kind} {}", entry.key), &entry.counts));
            }
        }
        let labels = rows.iter().map(|(label, _)| label.chars().count());
        let width = labels.max().unwrap_or(0).max(LABEL_HEADER.len());
        write!(f, "{LABEL_HEADER:<width$}")?;
        for column in COLUMNS {
            write!(f, " {column}")?;
        }
        writeln!(f)?;
        for (label, counts) in rows {
            write!(f, "{label:<width$}")?;
            for (column, cell) in COLUMNS.iter().zip(counts.cells()) {
                write!(f, " {cell:>column_width$}", column_width = column.len())?;
            }
            writeln!(f)?;
        }
        Ok(())
    }
}

/// Returns the rollup of `pipeline`, of `repository` when one is named, under the dimensions
/// that the statistics history reads, or `None` when a day lacks the counts per pipeline.
///
/// The prune counts each review's `started` and `completed` under `day` and again under
/// `pipeline`; a release with the rollup but without the `pipeline` dimensions wrote only the
/// first, so a day whose two sums differ holds reviews without their pipeline. Reviews pruned
/// by a release without the rollup left no counts at all and are not detected. The
/// `pipeline` counts become counts of the day, which a pipeline's report reads; every other
/// entry is left out.
///
/// # Errors
///
/// Returns `store_state_invalid` for a key of a `pipeline` dimension without its pipeline.
fn pipeline_rollup(
    rollup: &Rollup,
    pipeline: &str,
    repository: Option<&str>,
) -> Result<Option<Rollup>, Error> {
    let mut unmatched: BTreeMap<(&str, &str), i64> = BTreeMap::new();
    for count in &rollup.counts {
        let sign = match count.dimension.as_str() {
            DIMENSION_DAY => 1,
            DIMENSION_PIPELINE => -1,
            _ => continue,
        };
        let counter = count.counter.as_str();
        if counter == COUNTER_STARTED || counter == COUNTER_COMPLETED {
            let day = count.day.as_str();
            *unmatched.entry((day, counter)).or_default() += sign * count.value;
        }
    }
    if unmatched.values().any(|value| *value != 0) {
        return Ok(None);
    }
    let mut picked = Rollup::default();
    for count in &rollup.counts {
        if let Some((dimension, key)) =
            plain_entry(&count.dimension, &count.key, pipeline, repository)?
        {
            picked.counts.push(RollupCount {
                dimension: dimension.to_owned(),
                key,
                ..count.clone()
            });
        }
    }
    for sample in &rollup.samples {
        if let Some((dimension, key)) =
            plain_entry(&sample.dimension, &sample.key, pipeline, repository)?
        {
            picked.samples.push(RollupSample {
                dimension: dimension.to_owned(),
                key,
                ..sample.clone()
            });
        }
    }
    Ok(Some(picked))
}

/// Returns the dimension and key that the history reads for an entry of a `pipeline`
/// dimension of `pipeline` and `repository`, or `None` for any other entry.
fn plain_entry(
    dimension: &str,
    key: &str,
    pipeline: &str,
    repository: Option<&str>,
) -> Result<Option<(&'static str, String)>, Error> {
    let Some((plain, _)) = (PIPELINE_DIMENSIONS.iter()).find(|(_, kept)| *kept == dimension) else {
        return Ok(None);
    };
    let mut parts = key.splitn(3, '#');
    let (Some(repo), Some(name)) = (parts.next(), parts.next()) else {
        return Err(store_invalid(format!("rollup {dimension} key {key}")));
    };
    if name != pipeline || repository.is_some_and(|wanted| wanted != repo) {
        return Ok(None);
    }
    Ok(Some(match (*plain, parts.next()) {
        (DIMENSION_REPOSITORY, _) => (DIMENSION_DAY, String::new()),
        (plain, Some(reviewer)) => (plain, format!("{repo}#{reviewer}")),
        (plain, None) => (plain, repo.to_owned()),
    }))
}

impl<I: Integrations, C: Clock> Engine<I, C> {
    /// Returns the report of `request`, read from the store only.
    ///
    /// Without a pipeline, the review counts are those of `autoreview stats` for the range,
    /// of the repository when one is named. A pipeline's counts come from its kept rows and
    /// its counts in the rollup.
    ///
    /// # Errors
    ///
    /// Returns `repository_not_allowed` for a malformed repository, `pipeline_not_configured`
    /// for a pipeline that no rule runs, the errors of `StatsRange::parse`,
    /// `report_range_pruned` for a range that starts before the outcome retention or, with a
    /// pipeline, that holds a day pruned without the counts per pipeline, and the errors of
    /// the store reads.
    pub fn review_report(
        &self,
        cancel: &CancellationToken,
        request: &ReportRequest,
    ) -> Result<ReportDocument, Error> {
        let repository = request.repository.as_deref();
        if repository.is_some_and(|name| !repository_valid(name)) {
            return Err(Error::workflow(REPOSITORY_NOT_ALLOWED, true, None));
        }
        let pipeline = request.pipeline.as_deref();
        if let Some(name) = pipeline {
            let policy = self.config.policy.as_ref();
            if !policy.is_some_and(|policy| policy.pipeline_names().contains(name)) {
                return Err(Error::workflow(PIPELINE_NOT_CONFIGURED, true, None));
            }
        }
        let now = self.clock.wall_now().to_offset(UtcOffset::UTC);
        let until = request.until.as_deref();
        let Some(range) = StatsRange::parse(Some(&request.since), until, now.date())? else {
            return Err(Error::workflow(STATS_RANGE_INVALID, true, None));
        };
        if range.start < self.outcome_prune_cutoff() {
            return Err(Error::workflow(REPORT_RANGE_PRUNED, true, None));
        }
        live(cancel)?;
        let bounds = Some((range.start, range.end));
        let mut snapshot = self.store.stats_snapshot(bounds, SnapshotReads::Report)?;
        // Before the pipeline filter: the first publication of a request may be another's.
        let firsts = first_publications(&snapshot.reviews, &snapshot.pruned_requests)?;
        if let Some(name) = pipeline {
            snapshot.reviews.retain(|review| {
                let input = &review.input;
                input.rule.pipeline.name == name
                    && repository.is_none_or(|repo| input.pull_request.repository == repo)
            });
            let Some(rollup) = pipeline_rollup(&snapshot.rollup, name, repository)? else {
                return Err(Error::workflow(REPORT_RANGE_PRUNED, true, None));
            };
            snapshot.rollup = rollup;
        }
        let (history, mut requests, checks, disagreements) =
            history(&snapshot, &range, self.prune_cutoff(), &firsts)?;
        let counters = match (pipeline, repository) {
            (None, Some(name)) => (history.by_repository.into_iter())
                .find(|entry| entry.key == name)
                .map(|entry| entry.counts)
                .unwrap_or_default(),
            _ => history.totals,
        };
        // Each attempt has one repository, so the repositories together are the total.
        let request_seconds = match repository {
            Some(name) => requests.remove(name).unwrap_or_default(),
            None => requests.into_values().flatten().collect(),
        };
        let mut reviewers: BTreeMap<String, ReviewerCounts> = BTreeMap::new();
        for ((checked_in, reviewer), counts) in checks {
            if repository.is_none_or(|name| checked_in == name) {
                let entry = reviewers.entry(reviewer).or_default();
                entry.verifier_checked += counts.checked;
                entry.verifier_demoted += counts.demoted;
            }
        }
        let (mut multi_reviewer, mut disagreed) = (0, 0);
        for (counted_in, counts) in disagreements {
            if repository.is_none_or(|name| counted_in == name) {
                multi_reviewer += counts.multi_reviewer;
                disagreed += counts.disagreed;
                for (reviewer, alone) in counts.blocked_alone {
                    reviewers.entry(reviewer).or_default().blocked_alone += alone;
                }
            }
        }
        live(cancel)?;
        // A second read: a publication between the two reads shows in one part only.
        let outcomes = self.store.outcomes_between(range.start, range.end)?;
        Ok(ReportDocument {
            schema_version: REPORT_SCHEMA_VERSION,
            generated_at: format_rfc3339_seconds(now),
            since: range.since.to_string(),
            until: range.until.to_string(),
            repository: request.repository.clone(),
            pipeline: request.pipeline.clone(),
            reviews: review_metrics(
                &counters,
                request_seconds,
                (multi_reviewer, disagreed),
                reviewers,
            ),
            findings: finding_metrics(&outcomes, repository, pipeline),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        FindingMetrics, OutcomeCounts, ReviewMetrics, ReviewerCounts, finding_metrics,
        pipeline_rollup, review_metrics,
    };
    use crate::error::error_code;
    use crate::stats::{Counters, Keyed};
    use crate::store::{FindingOutcome, Rollup, RollupCount, RollupSample};

    /// A row's repository, pipeline, reviewer, severity, blocking flag and outcome.
    type Row<'a> = (&'a str, &'a str, &'a str, &'a str, bool, &'a str);

    /// One published finding with the fields the report reads.
    fn finding(row: Row<'_>) -> FindingOutcome {
        let (repository, pipeline, reviewer, severity, blocking, outcome) = row;
        FindingOutcome {
            repository: repository.to_owned(),
            pipeline: pipeline.to_owned(),
            reviewer_id: reviewer.to_owned(),
            severity: severity.to_owned(),
            blocking,
            outcome: outcome.to_owned(),
            ..FindingOutcome::default()
        }
    }

    /// The counts in field order: findings, blocking, then the seven outcomes and the share.
    fn counts(values: [i64; 9], fixed_percent: Option<f64>) -> OutcomeCounts {
        OutcomeCounts {
            findings: values[0],
            blocking: values[1],
            open: values[2],
            fixed: values[3],
            answered: values[4],
            refuted: values[5],
            not_blocking: values[6],
            open_at_merge: values[7],
            open_at_close: values[8],
            fixed_percent,
        }
    }

    /// One breakdown entry.
    fn entry<T>(key: &str, counts: T) -> Keyed<T> {
        let key = key.to_owned();
        Keyed { key, counts }
    }

    #[test]
    fn finding_metrics_count_each_outcome_by_reviewer_severity_and_repository() {
        let (widgets, gadgets) = ("acme/widgets", "acme/gadgets");
        let (bot, lint) = ("review-bot", "lint-bot");
        // `dismissed` is an outcome this version does not know.
        let rows: Vec<FindingOutcome> = [
            (widgets, "default", bot, "major", true, "fixed"),
            (widgets, "default", bot, "major", true, "open_at_merge"),
            (widgets, "default", bot, "minor", false, "fixed"),
            (widgets, "default", lint, "major", false, "refuted"),
            (widgets, "default", lint, "major", false, "not_blocking"),
            (widgets, "deep", bot, "major", true, "open"),
            (gadgets, "default", lint, "minor", false, "answered"),
            (gadgets, "default", lint, "minor", false, "open_at_close"),
            (gadgets, "default", lint, "minor", true, "dismissed"),
        ]
        .into_iter()
        .map(finding)
        .collect();
        let every = FindingMetrics {
            total: counts([9, 4, 1, 2, 1, 1, 1, 1, 1], Some(66.7)),
            by_reviewer: vec![
                entry(lint, counts([5, 1, 0, 0, 1, 1, 1, 0, 1], None)),
                entry(bot, counts([4, 3, 1, 2, 0, 0, 0, 1, 0], Some(66.7))),
            ],
            by_severity: vec![
                entry("major", counts([5, 3, 1, 1, 0, 1, 1, 1, 0], Some(50.0))),
                entry("minor", counts([4, 1, 0, 1, 1, 0, 0, 0, 1], Some(100.0))),
            ],
            by_repository: vec![
                entry(gadgets, counts([3, 1, 0, 0, 1, 0, 0, 0, 1], None)),
                entry(widgets, counts([6, 3, 1, 2, 0, 1, 1, 1, 0], Some(66.7))),
            ],
        };
        let total = counts([5, 2, 0, 2, 0, 1, 1, 1, 0], Some(66.7));
        let filtered = FindingMetrics {
            total: total.clone(),
            by_reviewer: vec![
                entry(lint, counts([2, 0, 0, 0, 0, 1, 1, 0, 0], None)),
                entry(bot, counts([3, 2, 0, 2, 0, 0, 0, 1, 0], Some(66.7))),
            ],
            by_severity: vec![
                entry("major", counts([4, 2, 0, 1, 0, 1, 1, 1, 0], Some(50.0))),
                entry("minor", counts([1, 0, 0, 1, 0, 0, 0, 0, 0], Some(100.0))),
            ],
            by_repository: vec![entry(widgets, total)],
        };
        let empty = FindingMetrics::default();
        let cases = [
            ("every repository and pipeline", None, None, every),
            ("one of each", Some(widgets), Some("default"), filtered),
            ("a pipeline without findings", None, Some("missing"), empty),
        ];
        for (name, repository, pipeline, want) in cases {
            let got = finding_metrics(&rows, repository, pipeline);
            assert_eq!(got, want, "{name}");
        }
    }

    #[test]
    fn review_metrics_share_the_decided_attempts_checks_and_disagreements_rounded_half_up() {
        let thirds = Counters {
            published: 5,
            approved_without_human: 2,
            approved_after_wait: 1,
            changes_requested: 1,
            commented: 1,
            approval_expired: 1,
            publication_median_seconds: Some(750),
            publication_p90_seconds: Some(9000),
            ..Counters::default()
        };
        let sixteenths = Counters {
            published: 16,
            approved_without_human: 1,
            changes_requested: 15,
            ..Counters::default()
        };
        let nothing = Counters::default();
        // The counts per reviewer, with the share that `review_metrics` fills.
        let verdicts = |checked, demoted, verifier_demoted_percent, blocked_alone| ReviewerCounts {
            verifier_checked: checked,
            verifier_demoted: demoted,
            verifier_demoted_percent,
            blocked_alone,
        };
        let (lint, bot) = ("lint-bot".to_owned(), "review-bot".to_owned());
        // A reviewer with nothing checked has no share, also when it blocked alone.
        let thirds_checks = [
            (lint.clone(), verdicts(3, 1, None, 0)),
            (bot.clone(), verdicts(0, 0, None, 2)),
        ];
        let sixteenths_checks = [(bot.clone(), verdicts(16, 1, None, 1))];
        // Four of the five publications have a request time, given unsorted.
        let cases = [
            (
                "one sixth is 16.7, one third 33.3",
                thirds,
                vec![3000, 600, 1500, 900],
                (6, 2),
                BTreeMap::from(thirds_checks),
                ReviewMetrics {
                    published: 5,
                    approval_expired: 1,
                    decided: 6,
                    approved_without_human: 2,
                    approved_after_wait: 1,
                    changes_requested: 1,
                    commented: 1,
                    approved_without_human_percent: Some(33.3),
                    waited_for_human_percent: Some(33.3),
                    changes_requested_percent: Some(16.7),
                    commented_percent: Some(16.7),
                    attempt_to_publication_median_seconds: Some(750),
                    attempt_to_publication_p90_seconds: Some(9000),
                    request_to_publication_median_seconds: Some(900),
                    request_to_publication_p90_seconds: Some(3000),
                    multi_reviewer: 6,
                    disagreed: 2,
                    disagreed_percent: Some(33.3),
                    by_reviewer: vec![
                        entry(&lint, verdicts(3, 1, Some(33.3), 0)),
                        entry(&bot, verdicts(0, 0, None, 2)),
                    ],
                },
            ),
            (
                "6.25 rounds up to 6.3",
                sixteenths,
                Vec::new(),
                (16, 1),
                BTreeMap::from(sixteenths_checks),
                ReviewMetrics {
                    published: 16,
                    decided: 16,
                    approved_without_human: 1,
                    changes_requested: 15,
                    approved_without_human_percent: Some(6.3),
                    waited_for_human_percent: Some(0.0),
                    changes_requested_percent: Some(93.8),
                    commented_percent: Some(0.0),
                    multi_reviewer: 16,
                    disagreed: 1,
                    disagreed_percent: Some(6.3),
                    by_reviewer: vec![entry(&bot, verdicts(16, 1, Some(6.3), 1))],
                    ..ReviewMetrics::default()
                },
            ),
            (
                "nothing decided",
                nothing,
                Vec::new(),
                (0, 0),
                BTreeMap::new(),
                ReviewMetrics::default(),
            ),
        ];
        for (name, counters, request_seconds, attempts, reviewers, want) in cases {
            let got = review_metrics(&counters, request_seconds, attempts, reviewers);
            assert_eq!(got, want, "{name}");
        }
    }

    /// One rollup count: day, dimension, key, counter and value.
    type Count = (&'static str, &'static str, &'static str, &'static str, i64);
    /// One rollup sample: day, dimension, key, sample and count.
    type Sample = (&'static str, &'static str, &'static str, i64, i64);

    /// Returns the rollup of `counts` and `samples`.
    fn rollup(counts: &[Count], samples: &[Sample]) -> Rollup {
        let text = str::to_owned;
        Rollup {
            counts: (counts.iter())
                .map(|&(day, dimension, key, counter, value)| RollupCount {
                    day: text(day),
                    dimension: text(dimension),
                    key: text(key),
                    counter: text(counter),
                    value,
                })
                .collect(),
            samples: (samples.iter())
                .map(|&(day, dimension, key, sample, count)| RollupSample {
                    day: text(day),
                    dimension: text(dimension),
                    key: text(key),
                    sample,
                    count,
                })
                .collect(),
        }
    }

    #[test]
    fn pipeline_rollup_reads_one_pipeline_and_refuses_days_pruned_without_it() {
        let (day, before) = ("2026-11-14", "2026-11-13");
        // Three reviews of the repositories `o/w` and `o/g` in the pipelines `p` and `q`, and a
        // stop; reviewers `r` and `s`.
        let pruned: Vec<Count> = vec![
            (day, "day", "", "started", 3),
            (day, "day", "", "completed", 3),
            (day, "day", "", "stopped", 2),
            (day, "verifier", "o/w#r", "checked", 2),
            (day, "pipeline", "o/w#p", "started", 1),
            (day, "pipeline", "o/w#p", "completed", 1),
            (day, "pipeline", "o/w#q", "started", 1),
            (day, "pipeline", "o/w#q", "completed", 1),
            (day, "pipeline", "o/g#p", "started", 1),
            (day, "pipeline", "o/g#p", "completed", 1),
            (day, "pipeline", "o/g#p", "published", 1),
            (day, "pipeline_verifier", "o/w#p#r", "checked", 1),
            (day, "pipeline_disagreement", "o/g#p", "disagreed", 1),
            (
                day,
                "pipeline_disagreement_reviewer",
                "o/g#p#s",
                "blocked_alone",
                1,
            ),
        ];
        let samples: Vec<Sample> = vec![
            (day, "day", "", 600, 1),
            (day, "pipeline", "o/g#p", 600, 1),
            (day, "pipeline_request", "o/w#p", 900, 1),
        ];
        let with = |extra: &[Count]| rollup(&[pruned.as_slice(), extra].concat(), &samples);
        let read = |stored: &Rollup, pipeline: &str, repository: Option<&str>| {
            let got = pipeline_rollup(stored, pipeline, repository);
            got.map_err(|err| error_code(&err).to_owned())
        };
        let ok = |counts: &[Count], samples: &[Sample]| Ok(Some(rollup(counts, samples)));
        let (started, completed) = (
            (day, "day", "", "started", 1),
            (day, "day", "", "completed", 1),
        );
        let p = [
            started,
            completed,
            started,
            completed,
            (day, "day", "", "published", 1),
            (day, "verifier", "o/w#r", "checked", 1),
            (day, "disagreement", "o/g", "disagreed", 1),
            (day, "disagreement_reviewer", "o/g#s", "blocked_alone", 1),
        ];
        let p_samples = [(day, "day", "", 600, 1), (day, "request", "o/w", 900, 1)];
        let w = [started, completed, (day, "verifier", "o/w#r", "checked", 1)];
        let across = [
            (before, "day", "", "started", 1),
            (before, "pipeline", "o/w#q", "started", 1),
            completed,
            (day, "pipeline", "o/w#q", "completed", 1),
        ];
        let cancel = [across[0], (day, "pipeline", "o/w#q", "started", 1)];
        let broken = [(day, "pipeline_verifier", "o/w", "checked", 1)];
        let cases = [
            (
                "no rollup",
                read(&Rollup::default(), "p", None),
                ok(&[], &[]),
            ),
            (
                "pipeline p",
                read(&with(&[]), "p", None),
                ok(&p, &p_samples),
            ),
            (
                "p of o/w",
                read(&with(&[]), "p", Some("o/w")),
                ok(&w, &p_samples[1..]),
            ),
            (
                "pipeline q",
                read(&with(&[]), "q", None),
                ok(&[started, completed], &[]),
            ),
            (
                "unknown pipeline",
                read(&with(&[]), "x", None),
                ok(&[], &[]),
            ),
            (
                "stops only",
                read(&rollup(&pruned[2..3], &[]), "p", None),
                ok(&[], &[]),
            ),
            (
                "created the day before",
                read(&rollup(&across, &[]), "q", None),
                { ok(&[across[0], completed], &[]) },
            ),
            (
                "pruned without pipelines",
                read(&rollup(&pruned[..2], &[]), "p", None),
                Ok(None),
            ),
            (
                "one review without",
                read(&with(&[completed]), "p", None),
                Ok(None),
            ),
            (
                "two days cancel",
                read(&rollup(&cancel, &[]), "q", None),
                Ok(None),
            ),
            (
                "key without pipeline",
                read(&rollup(&broken, &[]), "p", None),
                { Err("store_state_invalid".to_owned()) },
            ),
        ];
        for (name, got, want) in cases {
            assert_eq!(got, want, "{name}");
        }
    }
}
