//! The authors section of a repository rule: who is trusted and what everyone else gets.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;

use serde::Deserialize;
use serde::de::value::MapAccessDeserializer;
use serde::de::{self, Deserializer, MapAccess, Visitor};

use crate::Error;
use crate::types::{Checkout, OnClean, OnFindings, Pipeline, is_owner};

/// The trusted value that trusts every author.
const TRUSTED_ANY: &str = "any";

/// The untrusted value that leaves untrusted authors unreviewed.
const UNTRUSTED_SKIP: &str = "skip";

/// Repository permissions that can make an author trusted, lowest first.
const PERMISSIONS: &[&str] = &["write", "maintain", "admin"];

/// Outcomes an untrusted review may give findings.
const UNTRUSTED_ON_FINDINGS: [OnFindings; 2] = [OnFindings::RequestChanges, OnFindings::Comment];

/// Outcomes an untrusted review may give a clean review; approve is never one.
const UNTRUSTED_ON_CLEAN: [OnClean; 2] = [OnClean::Comment, OnClean::Await];

/// The authors section of a rule as written in the policy file.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct FileAuthors {
    /// Who is trusted; `None` when the key is absent or null.
    trusted: Option<NameOr<FileTrusted>>,
    /// What an untrusted author gets; `None` when the key is absent or null.
    untrusted: Option<NameOr<FileUntrusted>>,
}

/// A value written either as one name or as a mapping.
#[derive(Debug)]
enum NameOr<T> {
    /// A scalar, such as `any` or `skip`.
    Name(String),
    /// A mapping of the section's keys.
    Mapping(T),
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for NameOr<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(NameOrVisitor(PhantomData))
    }
}

/// Reads a scalar as a name and a mapping as `T`, keeping the unknown-key errors of `T`.
struct NameOrVisitor<T>(PhantomData<T>);

impl<'de, T: Deserialize<'de>> Visitor<'de> for NameOrVisitor<T> {
    type Value = NameOr<T>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a name or a mapping")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(NameOr::Name(value.to_owned()))
    }

    fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        T::deserialize(MapAccessDeserializer::new(map)).map(NameOr::Mapping)
    }
}

/// The trusted mapping as written in the policy file.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileTrusted {
    /// Lowercase logins that are trusted.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    logins: Vec<String>,
    /// Lowest repository permission that is trusted; empty when unset.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    permission: String,
}

/// The untrusted mapping as written in the policy file.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileUntrusted {
    /// Pipeline name.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    pipeline: String,
    /// Outcome name for findings.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    on_findings: String,
    /// Outcome name for clean reviews.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    on_clean: String,
    /// Read only to refuse it by name: an untrusted review never hands notes off.
    on_notes: Option<String>,
}

/// Validates the authors section of a rule against the pipelines of the policy.
///
/// # Errors
///
/// Returns an error when the section is missing, `trusted` is not `any` or a mapping with valid
/// logins or a permission, or `untrusted` is missing, misplaced or breaks an untrusted rule.
pub(crate) fn validate_authors(
    raw: Option<&FileAuthors>,
    pipelines: &BTreeMap<String, Pipeline>,
) -> Result<(), Error> {
    let Some(raw) = raw else {
        return Err(Error::AuthorsMissing);
    };
    match &raw.trusted {
        Some(NameOr::Name(name)) if name == TRUSTED_ANY => {
            if raw.untrusted.is_some() {
                return Err(Error::AuthorsUntrustedWithAny);
            }
            return Ok(());
        }
        Some(NameOr::Mapping(trusted)) => validate_trusted(trusted)?,
        Some(NameOr::Name(_)) | None => return Err(Error::AuthorsTrusted),
    }
    match &raw.untrusted {
        None => Err(Error::AuthorsUntrustedMissing),
        Some(NameOr::Name(name)) if name == UNTRUSTED_SKIP => Ok(()),
        Some(NameOr::Name(_)) => Err(Error::AuthorsUntrusted),
        Some(NameOr::Mapping(untrusted)) => validate_untrusted(untrusted, pipelines),
    }
}

/// Reports whether an authors section is `trusted: any`, which trusts every author.
pub(crate) fn trusts_any(raw: Option<&FileAuthors>) -> bool {
    matches!(
        raw.and_then(|authors| authors.trusted.as_ref()),
        Some(NameOr::Name(name)) if name == TRUSTED_ANY
    )
}

/// Validates the trusted mapping: at least one key, valid logins listed once, a known permission.
fn validate_trusted(raw: &FileTrusted) -> Result<(), Error> {
    if raw.logins.is_empty() && raw.permission.is_empty() {
        return Err(Error::AuthorsTrusted);
    }
    let mut seen: Vec<&str> = Vec::new();
    for login in &raw.logins {
        // is_owner refuses the app/ prefix and the [bot] suffix, so a login is written plain.
        if !is_owner(login) || seen.contains(&login.as_str()) {
            return Err(Error::AuthorsLogin {
                login: login.clone(),
            });
        }
        seen.push(login);
    }
    if !raw.permission.is_empty() && !PERMISSIONS.contains(&raw.permission.as_str()) {
        return Err(Error::AuthorsPermission);
    }
    Ok(())
}

/// Validates the untrusted mapping: a base-checkout pipeline and outcomes that never approve.
fn validate_untrusted(
    raw: &FileUntrusted,
    pipelines: &BTreeMap<String, Pipeline>,
) -> Result<(), Error> {
    let Some(pipeline) = pipelines.get(&raw.pipeline) else {
        return Err(Error::UntrustedUnknownPipeline {
            name: raw.pipeline.clone(),
        });
    };
    if pipeline.checkout != Checkout::Base {
        // A head checkout makes the author's code the reviewer's working tree.
        return Err(Error::UntrustedCheckout {
            pipeline: pipeline.name.clone(),
        });
    }
    if !UNTRUSTED_ON_FINDINGS
        .iter()
        .any(|value| value.as_str() == raw.on_findings)
    {
        return Err(Error::UntrustedOnFindings);
    }
    if !UNTRUSTED_ON_CLEAN
        .iter()
        .any(|value| value.as_str() == raw.on_clean)
    {
        return Err(Error::UntrustedOnClean);
    }
    if raw.on_notes.is_some() {
        return Err(Error::UntrustedOnNotes);
    }
    Ok(())
}
