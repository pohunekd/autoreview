//! The review report: timing, decisions and finding outcomes of a range, from the store only.
//!
//! The review counts come from the statistics history of the range; the finding counts come
//! from the outcome rows of the findings published in it, with the outcome each has now.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeMap;

use serde::Serialize;

use crate::stats::{Counters, Keyed, keyed};
use crate::store::outcome_checks::{OUTCOME_OPEN_AT_CLOSE, OUTCOME_OPEN_AT_MERGE};
use crate::store::{FindingOutcome, OUTCOME_ANSWERED, OUTCOME_FIXED, OUTCOME_OPEN};

/// Outcome of a blocking finding that the verifier refuted.
const OUTCOME_REFUTED: &str = "refuted";

/// Outcome of a blocking finding that the verifier found not blocking.
const OUTCOME_NOT_BLOCKING: &str = "not_blocking";

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
    ///
    /// It starts at the attempt, not at the review request, which the store does not keep.
    pub attempt_to_publication_median_seconds: Option<i64>,
    /// 90th percentile of the same seconds, nearest rank.
    pub attempt_to_publication_p90_seconds: Option<i64>,
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
pub fn review_metrics(counters: &Counters) -> ReviewMetrics {
    let decided = counters.published + counters.approval_expired;
    let waited = counters.approved_after_wait + counters.approval_expired;
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
        let cases = [
            (
                "one sixth is 16.7, one third 33.3",
                thirds,
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
                },
            ),
            (
                "6.25 rounds up to 6.3",
                sixteenths,
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
            ("nothing decided", nothing, ReviewMetrics::default()),
        ];
        for (name, counters, want) in cases {
            assert_eq!(review_metrics(&counters), want, "{name}");
        }
    }
}
