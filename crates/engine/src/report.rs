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
    median_and_p90, seconds_text,
};
use crate::store::outcome_checks::{OUTCOME_OPEN_AT_CLOSE, OUTCOME_OPEN_AT_MERGE};
use crate::store::{FindingOutcome, OUTCOME_ANSWERED, OUTCOME_FIXED, OUTCOME_OPEN, Rollup};

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
/// `request_seconds` are the seconds from the review request to publication of the bucket.
pub fn review_metrics(counters: &Counters, request_seconds: Vec<i64>) -> ReviewMetrics {
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

impl<I: Integrations, C: Clock> Engine<I, C> {
    /// Returns the report of `request`, read from the store only.
    ///
    /// Without a pipeline, the review counts are those of `autoreview stats` for the range,
    /// of the repository when one is named. A pipeline has no rollup, so its counts come from
    /// the kept rows, and its range must start after the review retention.
    ///
    /// # Errors
    ///
    /// Returns `repository_not_allowed` for a malformed repository, `pipeline_not_configured`
    /// for a pipeline that no rule runs, the errors of `StatsRange::parse`,
    /// `report_range_pruned` for a range that starts before the outcome retention (with a
    /// pipeline, also before the review retention), and the errors of the store reads.
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
        let review_cutoff = self.prune_cutoff();
        let pruned = range.start < self.outcome_prune_cutoff()
            || (pipeline.is_some() && range.start < review_cutoff);
        if pruned {
            return Err(Error::workflow(REPORT_RANGE_PRUNED, true, None));
        }
        live(cancel)?;
        let mut snapshot = self.store.stats_snapshot(Some((range.start, range.end)))?;
        // Before the pipeline filter: the first publication of a request may be another's.
        let firsts = first_publications(&snapshot.reviews, &snapshot.pruned_requests)?;
        if let Some(name) = pipeline {
            snapshot.reviews.retain(|review| {
                let input = &review.input;
                input.rule.pipeline.name == name
                    && repository.is_none_or(|repo| input.pull_request.repository == repo)
            });
            snapshot.rollup = Rollup::default();
        }
        let (history, mut requests) = history(&snapshot, &range, review_cutoff, &firsts)?;
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
            reviews: review_metrics(&counters, request_seconds),
            findings: finding_metrics(&outcomes, repository, pipeline),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{FindingMetrics, OutcomeCounts, ReviewMetrics, finding_metrics, review_metrics};
    use crate::stats::{Counters, Keyed};
    use crate::store::FindingOutcome;

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
    fn entry(key: &str, counts: OutcomeCounts) -> Keyed<OutcomeCounts> {
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
    fn review_metrics_share_the_decided_attempts_rounded_half_up() {
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
        // Four of the five publications have a request time, given unsorted.
        let cases = [
            (
                "one sixth is 16.7, one third 33.3",
                thirds,
                vec![3000, 600, 1500, 900],
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
                },
            ),
            (
                "6.25 rounds up to 6.3",
                sixteenths,
                Vec::new(),
                ReviewMetrics {
                    published: 16,
                    decided: 16,
                    approved_without_human: 1,
                    changes_requested: 15,
                    approved_without_human_percent: Some(6.3),
                    waited_for_human_percent: Some(0.0),
                    changes_requested_percent: Some(93.8),
                    commented_percent: Some(0.0),
                    ..ReviewMetrics::default()
                },
            ),
            (
                "nothing decided",
                nothing,
                Vec::new(),
                ReviewMetrics::default(),
            ),
        ];
        for (name, counters, request_seconds, want) in cases {
            assert_eq!(review_metrics(&counters, request_seconds), want, "{name}");
        }
    }
}
