//! The authors section of a repository rule: who is trusted and what everyone else gets, and
//! the author class of a pull request.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;

use serde::de::value::MapAccessDeserializer;
use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde::{Deserialize, Serialize};

use crate::Error;
use crate::types::{
    Checkout, OnClean, OnFindings, OnNotes, Pipeline, Rule, is_managed_user, is_owner,
};

/// The trusted value that trusts every author.
const TRUSTED_ANY: &str = "any";

/// The untrusted value that leaves untrusted authors unreviewed.
const UNTRUSTED_SKIP: &str = "skip";

/// The suffix that marks a listed login as an app or bot author.
const BOT_SUFFIX: &str = "[bot]";

/// The GraphQL type name of a user author.
const USER_TYPENAME: &str = "User";

/// The GraphQL type name of an app or bot author.
const BOT_TYPENAME: &str = "Bot";

/// Outcomes an untrusted review may give findings.
const UNTRUSTED_ON_FINDINGS: [OnFindings; 2] = [OnFindings::RequestChanges, OnFindings::Comment];

/// Outcomes an untrusted review may give a clean review; approve is never one.
const UNTRUSTED_ON_CLEAN: [OnClean; 2] = [OnClean::Comment, OnClean::Await];

/// Lowest repository permission that makes the author of a same-repository pull request trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Permission {
    /// The write role, which can push to the repository.
    Write,
    /// The maintain role.
    Maintain,
    /// The admin role.
    Admin,
}

impl Permission {
    /// Returns the permission of a GitHub repository role name, `None` below write.
    ///
    /// Triage, read, none and custom roles give `None`, so they never make an author trusted.
    #[must_use]
    pub fn from_role(role: &str) -> Option<Self> {
        match role {
            "write" => Some(Self::Write),
            "maintain" => Some(Self::Maintain),
            "admin" => Some(Self::Admin),
            _ => None,
        }
    }

    /// Returns the role name of this permission, as the policy writes it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Write => "write",
            Self::Maintain => "maintain",
            Self::Admin => "admin",
        }
    }
}

/// The kind of account that authored a pull request, as GitHub reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorType {
    /// A user account.
    User,
    /// A GitHub App or bot account.
    Bot,
    /// No type, or one that is neither; such an author is never matched by login.
    Unknown,
}

impl AuthorType {
    /// Returns the type of a GraphQL `__typename`: `User`, `Bot`, or unknown for anything else.
    #[must_use]
    pub fn from_typename(typename: &str) -> Self {
        match typename {
            USER_TYPENAME => Self::User,
            BOT_TYPENAME => Self::Bot,
            _ => Self::Unknown,
        }
    }
}

/// Who a rule trusts and what an untrusted author gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Authors {
    /// Every author is trusted, forks included.
    Any,
    /// Only the listed logins and the authors with the permission are trusted.
    Restricted {
        /// Who is trusted.
        trusted: TrustedAuthors,
        /// What every other author gets.
        untrusted: Untrusted,
    },
}

/// The authors a restricted rule trusts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedAuthors {
    /// Lowercase logins that are trusted whatever their permission, forks included.
    ///
    /// A plain login names a user; `name[bot]` names the app or bot `name`.
    pub logins: Vec<String>,
    /// Lowest permission that trusts the author of a same-repository pull request.
    pub permission: Option<Permission>,
}

/// What a restricted rule gives an untrusted author.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Untrusted {
    /// The pull request is not reviewed.
    Skip,
    /// The pull request is reviewed with these outcomes; boxed, because a pipeline is large.
    Review(Box<UntrustedReview>),
}

/// The review of an untrusted author: a base-checkout pipeline and outcomes that never approve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UntrustedReview {
    /// The pipeline of the review; its checkout is base.
    pub pipeline: Pipeline,
    /// Outcome when findings exist.
    pub on_findings: OnFindings,
    /// Outcome of a clean review, comment or await.
    pub on_clean: OnClean,
}

impl UntrustedReview {
    /// Returns the rule that the review of an untrusted author of `rule` runs under.
    ///
    /// It keeps the match and the trigger, takes this pipeline and these outcomes, and has no
    /// notes hand-off and no human review signals.
    #[must_use]
    pub fn rule(&self, rule: &Rule) -> Rule {
        Rule {
            match_glob: rule.match_glob.clone(),
            trigger: rule.trigger.clone(),
            pipeline: self.pipeline.clone(),
            on_findings: self.on_findings.clone(),
            on_clean: self.on_clean.clone(),
            on_notes: OnNotes::Unset,
            human_review: None,
        }
    }
}

/// The author class of a review, decided when it is dispatched and stored with it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AuthorClass {
    /// The rule trusts the author.
    #[serde(rename = "trusted")]
    Trusted,
    /// The rule does not trust the author; the review runs under its untrusted section.
    #[serde(rename = "untrusted")]
    Untrusted,
    /// The empty value of a review stored before author classes; its rule trusted every author.
    #[default]
    #[serde(rename = "")]
    Unset,
    /// A value that another engine stored and this one does not know; it counts as untrusted.
    #[serde(untagged)]
    Other(String),
}

impl AuthorClass {
    /// Reports whether the review must get the protections of an untrusted author.
    ///
    /// Only trusted and the unset value of an older review are not; an unknown value is.
    #[must_use]
    pub fn untrusted(&self) -> bool {
        !matches!(self, Self::Trusted | Self::Unset)
    }

    /// Returns the stored text of this class.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Trusted => "trusted",
            Self::Untrusted => "untrusted",
            Self::Unset => "",
            Self::Other(value) => value,
        }
    }
}

impl Authors {
    /// Reports whether the class of an author depends on their repository permission.
    ///
    /// Only a restricted rule with a permission asks, for a user author with a login whose
    /// pull request is not from a fork and whom the rule does not list.
    #[must_use]
    pub fn needs_permission(&self, login: &str, author_type: AuthorType, fork: bool) -> bool {
        match self {
            Self::Any => false,
            Self::Restricted { trusted, .. } => {
                trusted.permission.is_some()
                    && !login.is_empty()
                    && author_type == AuthorType::User
                    && !fork
                    && !listed(&trusted.logins, login, author_type)
            }
        }
    }

    /// Returns the class of a pull request author.
    ///
    /// A listed author is trusted, forks included: a user by a plain login, an app or bot by
    /// `name[bot]`. An author of unknown type, a fork, an app that is not listed, or an author
    /// without a login such as a deleted account is untrusted otherwise. A user is trusted by
    /// `permission`, the author's repository permission, `None` below write or when not read.
    #[must_use]
    pub fn class(
        &self,
        login: &str,
        author_type: AuthorType,
        fork: bool,
        permission: Option<Permission>,
    ) -> AuthorClass {
        let Self::Restricted { trusted, .. } = self else {
            return AuthorClass::Trusted;
        };
        if listed(&trusted.logins, login, author_type) {
            return AuthorClass::Trusted;
        }
        if fork || login.is_empty() || author_type != AuthorType::User {
            return AuthorClass::Untrusted;
        }
        match (trusted.permission, permission) {
            (Some(required), Some(actual)) if actual >= required => AuthorClass::Trusted,
            _ => AuthorClass::Untrusted,
        }
    }
}

/// Reports whether `logins` lists the author: a user by a plain login, an app or bot by
/// `name[bot]`. An author of unknown type is never listed.
fn listed(logins: &[String], login: &str, author_type: AuthorType) -> bool {
    let plain = login.strip_suffix(BOT_SUFFIX).unwrap_or(login);
    if plain.is_empty() {
        return false;
    }
    logins.iter().any(|item| match author_type {
        AuthorType::User => item.eq_ignore_ascii_case(login),
        AuthorType::Bot => item
            .strip_suffix(BOT_SUFFIX)
            .is_some_and(|name| name.eq_ignore_ascii_case(plain)),
        AuthorType::Unknown => false,
    })
}

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
pub(crate) fn parse_authors(
    raw: Option<&FileAuthors>,
    pipelines: &BTreeMap<String, Pipeline>,
) -> Result<Authors, Error> {
    let Some(raw) = raw else {
        return Err(Error::AuthorsMissing);
    };
    let trusted = match &raw.trusted {
        Some(NameOr::Name(name)) if name == TRUSTED_ANY => {
            if raw.untrusted.is_some() {
                return Err(Error::AuthorsUntrustedWithAny);
            }
            return Ok(Authors::Any);
        }
        Some(NameOr::Mapping(trusted)) => parse_trusted(trusted)?,
        Some(NameOr::Name(_)) | None => return Err(Error::AuthorsTrusted),
    };
    let untrusted = match &raw.untrusted {
        None => return Err(Error::AuthorsUntrustedMissing),
        Some(NameOr::Name(name)) if name == UNTRUSTED_SKIP => Untrusted::Skip,
        Some(NameOr::Name(_)) => return Err(Error::AuthorsUntrusted),
        Some(NameOr::Mapping(untrusted)) => {
            Untrusted::Review(Box::new(parse_untrusted(untrusted, pipelines)?))
        }
    };
    Ok(Authors::Restricted { trusted, untrusted })
}

/// Reports whether `login` is a lowercase GitHub login, or an app's login with `[bot]`.
///
/// A user login is a plain login or an Enterprise Managed User login (`handle_shortcode`). An
/// app's name has no underscore, and no login has a path character.
#[must_use]
pub fn login_valid(login: &str) -> bool {
    match login.strip_suffix(BOT_SUFFIX) {
        Some(app) => is_owner(app),
        None => is_owner(login) || is_managed_user(login),
    }
}

/// Validates the trusted mapping: at least one key, valid logins listed once, a known permission.
fn parse_trusted(raw: &FileTrusted) -> Result<TrustedAuthors, Error> {
    if raw.logins.is_empty() && raw.permission.is_empty() {
        return Err(Error::AuthorsTrusted);
    }
    let mut logins: Vec<String> = Vec::new();
    for login in &raw.logins {
        if !login_valid(login) || logins.contains(login) {
            return Err(Error::AuthorsLogin {
                login: login.clone(),
            });
        }
        logins.push(login.clone());
    }
    let permission = if raw.permission.is_empty() {
        None
    } else {
        let Some(permission) = Permission::from_role(&raw.permission) else {
            return Err(Error::AuthorsPermission);
        };
        Some(permission)
    };
    Ok(TrustedAuthors { logins, permission })
}

/// Validates the untrusted mapping: a base-checkout pipeline and outcomes that never approve.
fn parse_untrusted(
    raw: &FileUntrusted,
    pipelines: &BTreeMap<String, Pipeline>,
) -> Result<UntrustedReview, Error> {
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
    let Some(on_findings) = UNTRUSTED_ON_FINDINGS
        .iter()
        .find(|value| value.as_str() == raw.on_findings)
    else {
        return Err(Error::UntrustedOnFindings);
    };
    let Some(on_clean) = UNTRUSTED_ON_CLEAN
        .iter()
        .find(|value| value.as_str() == raw.on_clean)
    else {
        return Err(Error::UntrustedOnClean);
    };
    if raw.on_notes.is_some() {
        return Err(Error::UntrustedOnNotes);
    }
    Ok(UntrustedReview {
        pipeline: pipeline.clone(),
        on_findings: on_findings.clone(),
        on_clean: on_clean.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::{AuthorClass, AuthorType, Authors, Permission, TrustedAuthors, Untrusted};

    /// A rule that trusts the user `maintainer`, the app `coding-agent` and anyone with
    /// maintain, and skips everyone else.
    fn restricted() -> Authors {
        Authors::Restricted {
            trusted: TrustedAuthors {
                logins: vec!["maintainer".to_owned(), "coding-agent[bot]".to_owned()],
                permission: Some(Permission::Maintain),
            },
            untrusted: Untrusted::Skip,
        }
    }

    #[test]
    fn author_class_follows_the_decision_table() {
        use AuthorType::{Bot, Unknown, User};
        let authors = restricted();
        let maintain = Some(Permission::Maintain);
        let cases = [
            // (login, type, fork, permission, class)
            ("stranger", User, false, None, AuthorClass::Untrusted),
            (
                "member",
                User,
                false,
                Some(Permission::Write),
                AuthorClass::Untrusted,
            ),
            ("member", User, false, maintain, AuthorClass::Trusted),
            (
                "member",
                User,
                false,
                Some(Permission::Admin),
                AuthorClass::Trusted,
            ),
            (
                "member",
                User,
                true,
                Some(Permission::Admin),
                AuthorClass::Untrusted,
            ),
            // A user listed plainly is trusted, forks included, in any case.
            ("maintainer", User, true, None, AuthorClass::Trusted),
            ("Maintainer", User, false, None, AuthorClass::Trusted),
            // A bot listed with [bot] is trusted, whether its login carries the suffix or not.
            ("coding-agent", Bot, true, None, AuthorClass::Trusted),
            ("coding-agent[bot]", Bot, false, None, AuthorClass::Trusted),
            // A user whose login equals a listed bot's name, and a bot named like a listed user.
            ("coding-agent", User, false, None, AuthorClass::Untrusted),
            ("maintainer", Bot, false, None, AuthorClass::Untrusted),
            // A missing or unknown type never matches, and the permission does not help it.
            ("maintainer", Unknown, false, None, AuthorClass::Untrusted),
            ("member", Unknown, false, maintain, AuthorClass::Untrusted),
            ("helper", Bot, false, maintain, AuthorClass::Untrusted),
            ("", User, false, maintain, AuthorClass::Untrusted),
        ];
        for (login, author_type, fork, permission, class) in cases {
            assert_eq!(
                authors.class(login, author_type, fork, permission),
                class,
                "login={login} type={author_type:?} fork={fork} permission={permission:?}"
            );
        }
        assert_eq!(
            Authors::Any.class("stranger", Unknown, true, None),
            AuthorClass::Trusted,
            "trusted any trusts every author"
        );
    }

    #[test]
    fn permission_is_read_only_when_it_can_change_the_class() {
        use AuthorType::{Bot, Unknown, User};
        let authors = restricted();
        let cases = [
            // (login, type, fork, needs the permission)
            ("member", User, false, true),
            ("member", User, true, false),
            ("maintainer", User, false, false),
            ("helper", Bot, false, false),
            ("member", Unknown, false, false),
            ("", User, false, false),
        ];
        for (login, author_type, fork, needed) in cases {
            assert_eq!(
                authors.needs_permission(login, author_type, fork),
                needed,
                "login={login} type={author_type:?} fork={fork}"
            );
        }
        let logins_only = Authors::Restricted {
            trusted: TrustedAuthors {
                logins: vec!["maintainer".to_owned()],
                permission: None,
            },
            untrusted: Untrusted::Skip,
        };
        assert!(
            !logins_only.needs_permission("member", User, false)
                && !Authors::Any.needs_permission("member", User, false),
            "a rule without a permission never reads one"
        );
    }

    #[test]
    fn github_roles_below_write_and_unknown_classes_never_trust() {
        let roles = [
            "admin", "maintain", "write", "triage", "read", "none", "custom", "",
        ];
        let mapped: Vec<Option<Permission>> = roles
            .iter()
            .map(|role| Permission::from_role(role))
            .collect();
        assert_eq!(
            mapped,
            vec![
                Some(Permission::Admin),
                Some(Permission::Maintain),
                Some(Permission::Write),
                None,
                None,
                None,
                None,
                None
            ]
        );
        let types: Vec<AuthorType> = ["User", "Bot", "Mannequin", "Organization", "", "user"]
            .iter()
            .map(|name| AuthorType::from_typename(name))
            .collect();
        assert_eq!(
            types,
            vec![
                AuthorType::User,
                AuthorType::Bot,
                AuthorType::Unknown,
                AuthorType::Unknown,
                AuthorType::Unknown,
                AuthorType::Unknown
            ]
        );
        let other = AuthorClass::Other("maintainer".to_owned());
        assert!(
            other.untrusted()
                && AuthorClass::Untrusted.untrusted()
                && !AuthorClass::Trusted.untrusted()
                && !AuthorClass::Unset.untrusted(),
            "only trusted and the unset class of an older review are trusted"
        );
    }
}
