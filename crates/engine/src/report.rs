//! The review report: timing, decisions and finding outcomes of a range, from the store only.
//!
//! The review counts come from the statistics history of the range; the finding counts come
//! from the outcome rows of the findings published in it, with the outcome each has now.

// Rust guideline compliant 2026-06-26

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
