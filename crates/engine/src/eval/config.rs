//! The `eval` section of the configuration: what an eval run needs besides the review settings.
//!
//! The loader of the configuration file validates the section, so a value here is already
//! checked against the review settings of the same file.

// Rust guideline compliant 2026-06-26

use std::time::Duration;

/// The validated `eval` section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalConfig {
    /// Name of the read-only credential that every eval request uses.
    pub credential: String,
    /// Prefix of the session names of eval reviewers.
    pub session_name_prefix: String,
    /// Prefix of the branches of eval reviewer sessions.
    pub session_branch_prefix: String,
    /// Most trials that run at once.
    pub max_concurrent_trials: usize,
    /// Most trials of one run; a larger run is refused before any session starts.
    pub max_trials: usize,
    /// Most sessions of one run, retries included.
    pub max_sessions: usize,
    /// Longest wall-clock time of one trial.
    pub trial_timeout: Duration,
}
