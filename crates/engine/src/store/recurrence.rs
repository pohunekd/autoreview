//! The recurrence decision: which categories of fixed findings are due a guidance suggestion.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// One finding that a suggestion counted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuggestedFinding {
    /// Pull request number of the finding.
    pub number: i64,
    /// Key of the finding.
    pub finding_key: String,
    /// Body of the finding, as the reviewer wrote it.
    pub body: String,
}

/// A guidance rule that recurring findings of one category suggest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// Repository as `owner/name`.
    pub repository: String,
    /// Category the findings share.
    pub category: String,
    /// The fixed findings that the suggestion counted, oldest fix first.
    pub findings: Vec<SuggestedFinding>,
}

/// A fixed finding with a category, as the count reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FixedFinding {
    /// Repository as `owner/name`.
    pub(super) repository: String,
    /// Category slug of the finding.
    pub(super) category: String,
    /// When the finding was settled as fixed.
    pub(super) fixed_at: OffsetDateTime,
    /// The finding as a suggestion lists it.
    pub(super) finding: SuggestedFinding,
}

/// Returns the suggestions that `fixed` makes due, in repository and category order.
///
/// A finding counts when it was fixed at or after `window_start` and after the last
/// suggestion of its repository and category. A category is due when `threshold` findings count.
pub(super) fn due_suggestions(
    fixed: &[FixedFinding],
    last: &BTreeMap<(String, String), OffsetDateTime>,
    threshold: usize,
    window_start: OffsetDateTime,
) -> Vec<Suggestion> {
    let mut counted: BTreeMap<(&str, &str), Vec<SuggestedFinding>> = BTreeMap::new();
    for item in fixed {
        let key = (item.repository.clone(), item.category.clone());
        let after_last = last.get(&key).is_none_or(|time| item.fixed_at > *time);
        if item.fixed_at >= window_start && after_last {
            counted
                .entry((item.repository.as_str(), item.category.as_str()))
                .or_default()
                .push(item.finding.clone());
        }
    }
    counted
        .into_iter()
        .filter(|(_, findings)| findings.len() >= threshold)
        .map(|((repository, category), findings)| Suggestion {
            repository: repository.to_owned(),
            category: category.to_owned(),
            findings,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use time::macros::datetime;
    use time::{Duration, OffsetDateTime};

    use super::{FixedFinding, SuggestedFinding, due_suggestions};

    #[test]
    fn recurrence_is_decided_per_repository_and_category_within_the_window() {
        let start = datetime!(2026-01-10 00:00 UTC);
        let day = |offset: i64| start + Duration::days(offset);
        let fixed = |repository: &str, category: &str, at: OffsetDateTime| FixedFinding {
            repository: repository.to_owned(),
            category: category.to_owned(),
            fixed_at: at,
            finding: SuggestedFinding {
                number: 1,
                finding_key: format!("{}:{category}", at.unix_timestamp()),
                body: String::new(),
            },
        };
        let widgets = "acme/widgets";
        let gadgets = "acme/gadgets";
        let rows = |repository: &str, category: &str, days: &[i64]| -> Vec<FixedFinding> {
            days.iter()
                .map(|offset| fixed(repository, category, day(*offset)))
                .collect()
        };
        let last = |days: i64| {
            BTreeMap::from([((widgets.to_owned(), "missing-test".to_owned()), day(days))])
        };
        // (case, rows, last suggestions, categories due with their counted findings)
        let cases = [
            (
                "below the threshold",
                rows(widgets, "missing-test", &[1, 2]),
                BTreeMap::new(),
                vec![],
            ),
            (
                "at the threshold",
                rows(widgets, "missing-test", &[1, 2, 3]),
                BTreeMap::new(),
                vec![("acme/widgets", "missing-test", 3)],
            ),
            (
                "one fix before the window",
                rows(widgets, "missing-test", &[-1, 1, 2]),
                BTreeMap::new(),
                vec![],
            ),
            (
                "fixes before the last suggestion",
                rows(widgets, "missing-test", &[1, 2, 3, 5]),
                last(4),
                vec![],
            ),
            (
                "a fresh set after the last suggestion",
                rows(widgets, "missing-test", &[1, 2, 5, 6, 7]),
                last(4),
                vec![("acme/widgets", "missing-test", 3)],
            ),
            (
                "two categories",
                [
                    rows(widgets, "missing-test", &[1, 2, 3]),
                    rows(widgets, "naming", &[1, 2]),
                ]
                .concat(),
                BTreeMap::new(),
                vec![("acme/widgets", "missing-test", 3)],
            ),
            (
                "two repositories do not add up",
                [
                    rows(widgets, "missing-test", &[1, 2]),
                    rows(gadgets, "missing-test", &[3, 4]),
                ]
                .concat(),
                BTreeMap::new(),
                vec![],
            ),
        ];
        for (name, rows, last, want) in cases {
            let due = due_suggestions(&rows, &last, 3, start);
            let got: Vec<(&str, &str, usize)> = due
                .iter()
                .map(|s| (s.repository.as_str(), s.category.as_str(), s.findings.len()))
                .collect();
            assert_eq!(got, want, "{name}");
        }
    }
}
