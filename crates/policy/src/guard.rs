//! The text guard section of a repository rule.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeSet;

use autoreview_guard::FieldKind;
use serde::de::{self, Deserializer};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};

use crate::Error;

/// What a pull request gets when its guard score reaches the threshold.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum OnSuspect {
    /// A clean review of an approving rule waits for a human.
    #[serde(rename = "await")]
    Await,
    /// A review of an untrusted author that would request changes comments instead.
    #[serde(rename = "comment")]
    Comment,
    /// The decision stays as it is; only the result is recorded and notified.
    #[serde(rename = "log")]
    Log,
    /// The empty value of a stored guard without the key; parsing never produces it.
    #[default]
    #[serde(rename = "")]
    Unset,
    /// A value a newer engine stored that this one does not know, kept as stored.
    #[serde(untagged)]
    Other(String),
}

impl OnSuspect {
    /// Returns the policy file name of the outcome.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Await => "await",
            Self::Comment => "comment",
            Self::Log => "log",
            Self::Unset => "",
            Self::Other(value) => value,
        }
    }
}

/// The text guard of a rule: its fields, threshold and suspect outcome.
///
/// It is copied with the rule into each review workflow input. A stored guard without a key
/// reads it as empty, zero or unset, so rows keep decoding when a later release adds a key.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct RuleGuard {
    /// The pull request texts the guard scans; never empty in a parsed policy.
    #[serde(
        serialize_with = "serialize_fields",
        deserialize_with = "deserialize_fields"
    )]
    pub fields: BTreeSet<FieldKind>,
    /// The score from which a pull request is suspect, 1 to `u32::MAX` when parsed.
    pub threshold: u32,
    /// What a suspect pull request gets.
    pub on_suspect: OnSuspect,
}

autoreview_go_compat::object_only_serde!(RuleGuard);

/// The guard section as written in the policy file; an absent key is refused.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct FileGuard {
    /// Field codes; empty when absent.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    fields: Vec<String>,
    /// Threshold; zero when absent.
    threshold: i64,
    /// Outcome name; empty when absent.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    on_suspect: String,
}

/// Returns the field kind of a policy file code.
fn field_kind(code: &str) -> Option<FieldKind> {
    FieldKind::ALL
        .into_iter()
        .find(|kind| kind.as_str() == code)
}

/// Returns the codes of every field kind, comma separated, for error messages.
fn field_codes() -> String {
    FieldKind::ALL.map(FieldKind::as_str).join(", ")
}

/// Writes the fields as a list of their codes.
fn serialize_fields<S: Serializer>(
    fields: &BTreeSet<FieldKind>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_seq(fields.iter().map(|kind| kind.as_str()))
}

/// Reads the fields from their codes; null is empty, an unknown code fails.
///
/// An unknown code names a text this engine cannot scan. Dropping it would scan less than the
/// stored rule asks, so the row is refused instead.
fn deserialize_fields<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeSet<FieldKind>, D::Error> {
    let codes = Option::<Vec<String>>::deserialize(deserializer)?.unwrap_or_default();
    codes
        .iter()
        .map(|code| {
            field_kind(code)
                .ok_or_else(|| de::Error::custom(format!("unknown guard field {code:?}")))
        })
        .collect()
}

/// Validates the guard section of a rule; an absent key sets no guard.
///
/// # Errors
///
/// Returns a guard error when the key is null or a field is missing, unknown or out of range.
pub(crate) fn parse_guard(raw: Option<&Option<FileGuard>>) -> Result<Option<RuleGuard>, Error> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let Some(raw) = raw else {
        return Err(Error::GuardNull);
    };
    if raw.fields.is_empty() {
        return Err(Error::GuardFieldsEmpty);
    }
    let mut fields = BTreeSet::new();
    for code in &raw.fields {
        let kind = field_kind(code).ok_or_else(|| Error::GuardField {
            field: code.clone(),
            known: field_codes(),
        })?;
        if !fields.insert(kind) {
            return Err(Error::GuardFieldRepeated {
                field: code.clone(),
            });
        }
    }
    // Every score is at least zero, so zero would mark every pull request.
    let threshold = u32::try_from(raw.threshold)
        .ok()
        .filter(|threshold| *threshold >= 1)
        .ok_or(Error::GuardThreshold { max: u32::MAX })?;
    let on_suspect = match raw.on_suspect.as_str() {
        "await" => OnSuspect::Await,
        "comment" => OnSuspect::Comment,
        "log" => OnSuspect::Log,
        _ => return Err(Error::GuardOnSuspect),
    };
    Ok(Some(RuleGuard {
        fields,
        threshold,
        on_suspect,
    }))
}
