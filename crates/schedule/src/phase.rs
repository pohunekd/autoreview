//! The phases of a schedule record, as stored in the discovery state.

// Rust guideline compliant 2026-06-26

/// Schedule phase of a review identity that waits for its debounce.
pub const WAITING: &str = "waiting";

/// Schedule phase of a review that waits for its next retry.
pub const RETRY_WAIT: &str = "retry_wait";

/// Schedule phase of a review identity whose attempt is queued or running.
pub const RUNNING: &str = "running";

/// Schedule phase of a review identity whose last run ended.
pub const COMPLETED: &str = "completed";

/// Schedule phase of a review identity that gets no automatic attempt.
pub const FAILED: &str = "failed";

/// Schedule phase of a review identity that left every trigger.
pub const WITHDRAWN: &str = "withdrawn";
