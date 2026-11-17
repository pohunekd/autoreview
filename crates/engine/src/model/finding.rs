//! Findings and the validated output of one reviewer session.

// Rust guideline compliant 2026-06-26

use serde::{Deserialize, Serialize};

use super::review::PriorVerdict;

/// Reports whether a flag is unset; the JSON omits an unset flag.
pub(crate) fn is_false(value: &bool) -> bool {
    !*value
}

/// Finding reported by a reviewer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct Finding {
    /// Identifier of the finding.
    pub id: String,
    /// Identifier of the source.
    pub source_id: String,
    /// Identifier of the reviewer.
    pub reviewer_id: String,
    /// Label of the reviewer.
    pub reviewer_label: String,
    /// Severity of the finding.
    pub severity: String,
    /// Confidence of the finding.
    pub confidence: String,
    /// File path of the finding.
    pub path: String,
    /// Line of the finding.
    pub line: i64,
    /// Title of the finding.
    pub title: String,
    /// Body of the finding.
    pub body: String,
    /// Whether the finding is anchored.
    pub anchored: bool,
    /// Marks a note that is published without asking for changes.
    ///
    /// The zero value blocks, so a finding without the flag keeps blocking.
    #[serde(skip_serializing_if = "is_false")]
    pub non_blocking: bool,
    /// Verdict of the finding.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub verdict: String,
    /// Slug that groups findings of one kind; empty when the reviewer gave none.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub category: String,
    /// The `skip` glob of the repository review rules that turned the finding into a note.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub skipped_by: String,
}

autoreview_go_compat::object_only_serde!(Finding);

/// Longest category slug, in characters.
const CATEGORY_MAX_CHARACTERS: usize = 40;

/// Reports whether `value` is a category slug: words of `a-z` and `0-9` joined by single hyphens.
pub(crate) fn category_valid(value: &str) -> bool {
    value.len() <= CATEGORY_MAX_CHARACTERS
        && value.split('-').all(|word| {
            !word.is_empty()
                && word
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

/// Output of one reviewer session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct ReviewerResult {
    /// Identifier of the reviewer.
    pub reviewer_id: String,
    /// Summary of the review.
    pub summary: String,
    /// Findings of the review.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    pub findings: Vec<Finding>,
    /// Tests of the review.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    pub tests: Vec<String>,
    /// Verdicts on prior findings.
    #[serde(
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "autoreview_go_compat::null_default::deserialize"
    )]
    pub prior: Vec<PriorVerdict>,
}

autoreview_go_compat::object_only_serde!(ReviewerResult);

/// Summary of one reviewer in the artifact.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct ReviewerSummary {
    /// Identifier of the reviewer.
    pub reviewer_id: String,
    /// Label of the reviewer.
    pub label: String,
    /// Summary of the review.
    pub summary: String,
}

autoreview_go_compat::object_only_serde!(ReviewerSummary);
