//! The author limit of a repository rule: the reviews one author may start in a window.

// Rust guideline compliant 2026-06-26

use std::time::Duration;

use serde::{Deserialize, Deserializer};

use crate::Error;

/// Largest number of reviews an author limit allows in its window.
pub(crate) const MAX_LIMIT_REVIEWS: u32 = 1000;

/// Longest window of an author limit, in hours.
pub(crate) const MAX_LIMIT_WINDOW_HOURS: u32 = 720;

/// What a review start over the author limit gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverLimit {
    /// The review starts when the window frees a slot.
    Wait,
    /// The review is not started, and the operator is notified.
    Skip,
}

impl OverLimit {
    /// Returns the policy file name of the outcome.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wait => "wait",
            Self::Skip => "skip",
        }
    }
}

/// How many reviews one pull request author may start under a rule in a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthorLimit {
    /// The most reviews one author may start in the window, 1 to 1000.
    pub reviews: u32,
    /// The length of the window in hours, 1 to 720.
    pub window_hours: u32,
    /// What a start over the bound gets.
    pub over: OverLimit,
}

impl AuthorLimit {
    /// Returns the length of the window.
    #[must_use]
    pub fn window(&self) -> Duration {
        Duration::from_hours(u64::from(self.window_hours))
    }
}

/// The author limit as written in the policy file; an absent field is refused.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct FileAuthorLimit {
    /// Reviews per window; zero when absent.
    reviews: i64,
    /// Window length in hours; zero when absent.
    window_hours: i64,
    /// Outcome name; empty when absent.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    over: String,
}

/// Reads a key that may be absent but not null: absent stays `None`, null is `Some(None)`.
///
/// # Errors
///
/// Returns the error of the deserializer.
pub(crate) fn present<'de, D>(deserializer: D) -> Result<Option<Option<FileAuthorLimit>>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::deserialize(deserializer).map(Some)
}

/// Validates the author limit of a rule; an absent key sets no limit.
///
/// # Errors
///
/// Returns an author limit error when the key is null or a field is missing or out of range.
pub(crate) fn parse_author_limit(
    raw: Option<&Option<FileAuthorLimit>>,
) -> Result<Option<AuthorLimit>, Error> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let Some(raw) = raw else {
        return Err(Error::AuthorLimitNull);
    };
    let reviews = u32::try_from(raw.reviews)
        .ok()
        .filter(|reviews| (1..=MAX_LIMIT_REVIEWS).contains(reviews))
        .ok_or(Error::AuthorLimitReviews {
            max: MAX_LIMIT_REVIEWS,
        })?;
    let window_hours = u32::try_from(raw.window_hours)
        .ok()
        .filter(|hours| (1..=MAX_LIMIT_WINDOW_HOURS).contains(hours))
        .ok_or(Error::AuthorLimitWindow {
            max: MAX_LIMIT_WINDOW_HOURS,
        })?;
    let over = match raw.over.as_str() {
        "wait" => OverLimit::Wait,
        "skip" => OverLimit::Skip,
        _ => return Err(Error::AuthorLimitOver),
    };
    Ok(Some(AuthorLimit {
        reviews,
        window_hours,
        over,
    }))
}
