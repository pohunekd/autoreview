//! Review policy types: the rules, pipelines and reviewers a review input stores, glob matching and human review signals.

// Rust guideline compliant 2026-06-26

mod human_review;
mod types;

pub use human_review::{HumanReview, match_glob};
pub use types::{
    Checkout, EVENTS, Event, Level, OnClean, OnFindings, OnNotes, Pipeline, Policy, Protocol,
    Reviewer, Rule, Trigger, TriggerKind, VERIFIER_REVIEWER_ID, Verifier, WORKFLOW_EVENTS,
};

/// Errors from policy types and human review validation.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Human review needs on_clean approve.
    #[error("human_review needs on_clean approve")]
    HumanReviewNeedsApprove,

    /// Human review must set labels or deleted tests.
    #[error("human_review must set labels or deleted_tests")]
    HumanReviewEmpty,

    /// Human review lists too many entries.
    #[error("human_review lists at most {limit} labels and {limit} deleted_tests globs")]
    HumanReviewTooManyEntries {
        /// Maximum entries allowed.
        limit: usize,
    },

    /// Human review label is invalid or duplicated.
    #[error("human_review labels: {label:?} must be a valid label listed once")]
    HumanReviewLabel {
        /// The rejected label.
        label: String,
    },

    /// Human review glob is invalid or duplicated.
    #[error("human_review deleted_tests: {glob:?} must be a valid relative glob listed once")]
    HumanReviewGlob {
        /// The rejected glob.
        glob: String,
    },

    /// Pattern syntax error.
    #[error("syntax error in pattern")]
    BadPattern,
}
