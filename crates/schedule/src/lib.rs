//! Review scheduling for the Rust port: debounce, retry policy and persisted schedule records.

// Rust guideline compliant 2026-06-26

pub mod diagnostic;
pub mod metadata;
pub mod scheduler;

/// Errors from schedule time arithmetic and review session metadata.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Adding a delay to an instant leaves the range that `time` can represent.
    #[error("schedule time is outside the representable range")]
    TimeOutOfRange,
    /// The companion group name is not one that pohunek-companion would show.
    #[error("invalid review session group")]
    InvalidSessionGroup,
    /// The repository, number, URL or head SHA of a review session is malformed.
    #[error("invalid review session identity")]
    InvalidSessionIdentity,
}
