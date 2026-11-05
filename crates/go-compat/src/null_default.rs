//! Reads JSON `null` as the default value, for fields that Go writes as nil collections.

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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde::Deserialize;

    #[derive(Debug, Default, PartialEq, Deserialize)]
    #[serde(default)]
    struct Probe {
        #[serde(deserialize_with = "super::deserialize")]
        items: Vec<String>,
        #[serde(deserialize_with = "super::deserialize")]
        counts: BTreeMap<String, i64>,
    }

    #[test]
    fn null_default_reads_null_as_empty() {
        let from_null: Probe = serde_json::from_str(r#"{"items": null, "counts": null}"#)
            .expect("null fields deserialize");
        assert_eq!(from_null, Probe::default());

        let from_missing: Probe = serde_json::from_str("{}").expect("missing fields deserialize");
        assert_eq!(from_missing, Probe::default());

        let filled: Probe = serde_json::from_str(r#"{"items": ["a"], "counts": {"k": 1}}"#)
            .expect("values deserialize");
        assert_eq!(filled.items, vec!["a".to_owned()]);
        assert_eq!(filled.counts.get("k"), Some(&1));
    }
}
