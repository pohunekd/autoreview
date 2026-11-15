//! The text guard section of a repository rule.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeSet;

use autoreview_guard::FieldKind;
use serde::de::{self, Deserializer};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};

/// What a pull request gets when its guard score reaches the threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
}

impl OnSuspect {
    /// Returns the policy file name of the outcome.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Await => "await",
            Self::Comment => "comment",
            Self::Log => "log",
        }
    }
}

/// The text guard of a rule: its fields, threshold and suspect outcome.
///
/// It is copied with the rule into each review workflow input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleGuard {
    /// The kinds of pull request text the guard scans; never empty.
    #[serde(
        serialize_with = "serialize_fields",
        deserialize_with = "deserialize_fields"
    )]
    pub fields: BTreeSet<FieldKind>,
    /// The guard score from which a pull request is suspect, 1 to `u32::MAX`.
    pub threshold: u32,
    /// What a suspect pull request gets.
    pub on_suspect: OnSuspect,
}

/// Returns the field kind of a policy file code.
fn field_kind(code: &str) -> Option<FieldKind> {
    FieldKind::ALL
        .into_iter()
        .find(|kind| kind.as_str() == code)
}

/// Writes the fields as a list of their codes.
fn serialize_fields<S: Serializer>(
    fields: &BTreeSet<FieldKind>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_seq(fields.iter().map(|kind| kind.as_str()))
}

/// Reads the fields from their codes; an unknown code is an error.
fn deserialize_fields<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeSet<FieldKind>, D::Error> {
    let codes = Vec::<String>::deserialize(deserializer)?;
    codes
        .iter()
        .map(|code| {
            field_kind(code)
                .ok_or_else(|| de::Error::custom(format!("unknown guard field {code:?}")))
        })
        .collect()
}
