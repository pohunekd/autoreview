//! Reads JSON `null` list entries as the default value, as Go decodes them into a slice.

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

/// Replaces each `None` entry with `T::default()`, as Go reads a `null` slice element.
#[must_use]
pub fn zero_entries<T: Default>(entries: Vec<Option<T>>) -> Vec<T> {
    entries.into_iter().map(Option::unwrap_or_default).collect()
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    #[derive(Debug, Default, PartialEq, Deserialize)]
    #[serde(default)]
    struct Probe {
        #[serde(deserialize_with = "super::deserialize")]
        items: Vec<String>,
    }

    #[test]
    fn null_entries_read_as_default() {
        let from_entries: Probe = serde_json::from_str(r#"{"items": ["a", null, "b"]}"#)
            .expect("null entries deserialize");
        assert_eq!(
            from_entries.items,
            vec!["a".to_owned(), String::new(), "b".to_owned()]
        );

        let from_null: Probe =
            serde_json::from_str(r#"{"items": null}"#).expect("a null list deserializes");
        assert_eq!(from_null, Probe::default());

        let refused = serde_json::from_str::<Probe>(r#"{"items": "a"}"#);
        assert!(refused.is_err(), "a value that is not a list is refused");
    }
}
