//! Helpers that read and write the persisted and published formats of autoreview.

// Rust guideline compliant 2026-06-26

pub mod go_time;
pub mod null_default;
pub mod null_entries;
pub mod object_only;

/// Errors from the stored time helpers.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The text is not RFC 3339 with an offset.
    #[error("cannot parse {input:?} as RFC 3339 time with an offset")]
    Parse {
        /// The rejected text.
        input: String,
        /// The failure reported by the `time` crate.
        #[source]
        source: time::error::Parse,
    },
    /// The year is outside the range that the stored time JSON form accepts.
    #[error("year {year} is outside 0..=9999, the years the stored time JSON form holds")]
    YearOutOfRange {
        /// The UTC year that was rejected.
        year: i32,
    },
}
