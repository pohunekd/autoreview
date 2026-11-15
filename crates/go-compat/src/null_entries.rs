//! Reads JSON `null` list entries as the default value, which stored lists may contain.

// Rust guideline compliant 2026-06-26

use serde::{Deserialize, Deserializer};

/// Deserializes a list, reading a `null` list as empty and each `null` entry as `T::default()`.
///
/// # Errors
///
/// Returns an error when the value is not a list or an entry fails to deserialize.
pub fn deserialize<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    let entries = Option::<Vec<Option<T>>>::deserialize(deserializer)?;
    Ok(zero_entries(entries.unwrap_or_default()))
}

/// Replaces each `None` entry with `T::default()`, so a stored `null` element reads as default.
#[must_use]
pub fn zero_entries<T: Default>(entries: Vec<Option<T>>) -> Vec<T> {
    entries.into_iter().map(Option::unwrap_or_default).collect()
}
