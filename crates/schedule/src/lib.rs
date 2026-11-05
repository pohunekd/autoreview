//! Review scheduling for the Rust port: debounce, retry policy and persisted schedule records.

// Rust guideline compliant 2026-06-26

pub mod scheduler;

/// Errors from scheduling time arithmetic.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Adding a delay to an instant leaves the range that `time` can represent.
    #[error("schedule time is outside the representable range")]
    TimeOutOfRange,
}
