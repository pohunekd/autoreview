//! JSON log lines on stderr, so stdout carries only the command's JSON.

// Rust guideline compliant 2026-06-26

use autoreview_engine::BoxError;
use tracing::Level;

/// Lowest level that is logged, as Go's default `slog` handler logs `INFO` and above.
const LOG_LEVEL: Level = Level::INFO;

/// Installs the JSON subscriber that writes every log line to stderr.
///
/// # Errors
///
/// Returns the error of `tracing-subscriber` when a global subscriber is already set.
pub(crate) fn init() -> Result<(), BoxError> {
    tracing_subscriber::fmt()
        .json()
        .with_max_level(LOG_LEVEL)
        .with_writer(std::io::stderr)
        .try_init()
}
