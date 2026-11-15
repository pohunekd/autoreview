//! Reads JSON `null` as the default value, for collections that earlier releases write as `null`.

// Rust guideline compliant 2026-06-26

use serde::{Deserialize, Deserializer};

/// Deserializes `T`, reading JSON `null` as `T::default()`.
///
/// # Errors
///
/// Returns an error when `T` fails to deserialize from a non-null value.
pub fn deserialize<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    let value = Option::<T>::deserialize(deserializer)?;
    Ok(value.unwrap_or_default())
}
