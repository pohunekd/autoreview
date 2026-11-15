//! The text guard section of a repository rule.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeSet;

use autoreview_guard::FieldKind;
use serde::de::Deserializer;
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
    ///
    /// The guard counts as suspect, see [`RuleGuard`].
    #[default]
    #[serde(rename = "")]
    Unset,
    /// A value a newer engine stored that this one does not know, kept as stored.
    ///
    /// The guard counts as suspect, see [`RuleGuard`].
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

/// One field of a guard: a text kind the guard scans, or an unknown stored code.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GuardField {
    /// A kind of pull request text the guard scans.
    Kind(FieldKind),
    /// A code a newer engine stored that this one does not know, kept as stored.
    ///
    /// Parsing a policy refuses unknown codes, so only a stored rule holds one. The guard counts
    /// as suspect, see [`RuleGuard`].
    Other(String),
}

impl GuardField {
    /// Returns the stored code of the field.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Kind(kind) => kind.as_str(),
            Self::Other(code) => code,
        }
    }
}

/// The text guard of a rule: its fields, threshold and suspect outcome.
///
/// It is copied with the rule into each review workflow input. A stored guard without a key
/// reads it as empty, zero or unset, so rows keep decoding when a later release adds a key.
///
/// A stored guard counts as suspect and behaves as `OnSuspect::Await`, whatever the score, when
/// any of these holds, so a guard this engine cannot fully read fails closed:
///
/// - a field is `GuardField::Other`;
/// - `fields` is empty;
/// - `on_suspect` is `OnSuspect::Unset` or `OnSuspect::Other`;
/// - `threshold` is 0.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct RuleGuard {
    /// The pull request texts the guard scans; never empty in a parsed policy.
    #[serde(
        serialize_with = "serialize_fields",
        deserialize_with = "deserialize_fields"
    )]
    pub fields: BTreeSet<GuardField>,
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
    fields: &BTreeSet<GuardField>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_seq(fields.iter().map(GuardField::as_str))
}

/// Reads the fields from their codes; null is empty, an unknown code is kept as `Other`.
///
/// A rollback must still decode rows a newer engine stored with a kind it added.
fn deserialize_fields<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeSet<GuardField>, D::Error> {
    let codes = Option::<Vec<String>>::deserialize(deserializer)?.unwrap_or_default();
    let field = |code: String| field_kind(&code).map_or(GuardField::Other(code), GuardField::Kind);
    Ok(codes.into_iter().map(field).collect())
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
        if !fields.insert(GuardField::Kind(kind)) {
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
