//! GitHub credentials: first-match resolution and the coverage of the policy's repositories.

// Rust guideline compliant 2026-11-13

use std::fmt;

use crate::error::{BoxError, Error};

/// Key of the credential list in `config.yaml`.
const CREDENTIALS_KEY: &str = "github.credentials";

/// Key of a credential's repository patterns in `config.yaml`.
const MATCH_KEY: &str = "github.credentials.match";

/// Code of a repository that no credential matches.
const NOT_COVERED: &str = "credential_not_covered";

/// One GitHub token, the account it belongs to and the repositories it serves.
#[derive(Clone)]
pub struct Credential {
    /// Unique name of the credential, used in error messages.
    pub name: String,
    /// The lowercase login of the account the token belongs to.
    pub login: String,
    /// The token; it never appears in debug output.
    pub token: String,
    /// Repository patterns in file order: `*`, `owner/*` or `owner/repository`.
    pub matches: Vec<String>,
}

impl fmt::Debug for Credential {
    /// Shows everything except the token.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credential")
            .field("name", &self.name)
            .field("login", &self.login)
            .field("matches", &self.matches)
            .finish_non_exhaustive()
    }
}

/// The credentials of `github.credentials` in file order; the first match wins.
///
/// The list is never empty, and no pattern is covered by an earlier one.
#[derive(Clone, Debug)]
pub struct Credentials {
    /// The credentials in file order.
    list: Vec<Credential>,
}

impl Credentials {
    /// Checks that the list is not empty and that every pattern can match first.
    ///
    /// # Errors
    ///
    /// Returns `configuration_invalid` naming `github.credentials` for an empty list, or
    /// naming `github.credentials.match` for a pattern that an earlier pattern covers.
    pub fn new(list: Vec<Credential>) -> Result<Self, Error> {
        if list.is_empty() {
            return Err(error(
                "configuration_invalid",
                CREDENTIALS_KEY,
                "must list at least one credential".to_owned(),
            ));
        }
        let mut earlier: Vec<(&str, &str)> = Vec::new();
        for credential in &list {
            for pattern in &credential.matches {
                if let Some((first, owner)) =
                    earlier.iter().find(|(first, _)| covers(first, pattern))
                {
                    let message = format!(
                        "{pattern} of {} is unreachable, {first} of {owner} matches first",
                        credential.name
                    );
                    return Err(error("configuration_invalid", MATCH_KEY, message));
                }
                earlier.push((pattern.as_str(), credential.name.as_str()));
            }
        }
        Ok(Self { list })
    }

    /// Checks the credentials against the repository patterns the policy reviews.
    ///
    /// # Errors
    ///
    /// Returns `credential_not_covered` for a policy pattern with a repository that no
    /// credential matches, and `credential_unused` for a credential that serves no repository
    /// of the policy.
    pub fn check_coverage(&self, reviewed: &[&str]) -> Result<(), Error> {
        for rule in reviewed {
            let covered = self
                .list
                .iter()
                .any(|credential| credential.matches.iter().any(|p| covers(p, rule)));
            if !covered {
                let message = format!("no credential matches {rule} of the policy");
                return Err(error(NOT_COVERED, CREDENTIALS_KEY, message));
            }
        }
        for (index, credential) in self.list.iter().enumerate() {
            let earlier = self.list.get(..index).unwrap_or_default();
            let serves = credential.matches.iter().any(|pattern| {
                reviewed.iter().any(|rule| {
                    narrower(pattern, rule).is_some_and(|shared| {
                        !earlier
                            .iter()
                            .any(|first| first.matches.iter().any(|p| covers(p, shared)))
                    })
                })
            });
            if !serves {
                let message = format!("{} matches no repository of the policy", credential.name);
                return Err(error("credential_unused", CREDENTIALS_KEY, message));
            }
        }
        Ok(())
    }

    /// Returns the first credential whose patterns match the lowercase repository.
    #[must_use]
    pub fn resolve(&self, repository: &str) -> Option<&Credential> {
        self.list
            .iter()
            .find(|credential| credential.matches.iter().any(|p| covers(p, repository)))
    }

    /// Returns the credential that serves `repository`, compared in lower case.
    ///
    /// GitHub names ignore case, so a mixed-case name never falls through to a later pattern.
    ///
    /// # Errors
    ///
    /// Returns `credential_not_covered` naming `github.credentials` when no credential
    /// matches the repository.
    pub fn serving(&self, repository: &str) -> Result<&Credential, Error> {
        self.resolve(&repository.to_ascii_lowercase())
            .ok_or_else(|| {
                let message = format!("no credential matches {repository}");
                error(NOT_COVERED, CREDENTIALS_KEY, message)
            })
    }

    /// Returns the parts of the policy patterns `scopes` that `credential` searches, in order.
    ///
    /// A part is a pattern of `credential` narrowed to a scope, left out when an earlier
    /// credential matches all of it. Its search may still find repositories of an earlier,
    /// narrower credential; the caller drops them with [`Credentials::serving`].
    #[must_use]
    pub fn search_scopes(&self, credential: &Credential, scopes: &[String]) -> Vec<String> {
        let earlier: Vec<&Credential> = (self.list.iter())
            .take_while(|first| first.name != credential.name)
            .collect();
        let mut parts: Vec<String> = Vec::new();
        for scope in scopes {
            for pattern in &credential.matches {
                let Some(part) = narrower(pattern, scope) else {
                    continue;
                };
                let taken =
                    (earlier.iter()).any(|first| first.matches.iter().any(|p| covers(p, part)));
                if !taken && !parts.iter().any(|known| known == part) {
                    parts.push(part.to_owned());
                }
            }
        }
        parts
    }

    /// Returns the credential when it is the only one.
    #[must_use]
    pub fn single(&self) -> Option<&Credential> {
        match self.list.as_slice() {
            [only] => Some(only),
            _ => None,
        }
    }

    /// Returns the credentials in file order.
    pub fn iter(&self) -> impl Iterator<Item = &Credential> {
        self.list.iter()
    }

    /// Returns one credential of `login` that serves every repository.
    #[cfg(test)]
    pub(crate) fn single_for_tests(login: &str) -> Self {
        Self {
            list: vec![Credential {
                name: "default".to_owned(),
                login: login.to_owned(),
                token: "placeholder-token".to_owned(),
                matches: vec!["*".to_owned()],
            }],
        }
    }
}

/// Reports whether `pattern` matches every repository of `target`.
///
/// Both are `*`, `owner/*` or `owner/repository`; a repository is its own pattern.
fn covers(pattern: &str, target: &str) -> bool {
    if pattern == "*" || pattern == target {
        return true;
    }
    match (pattern.split_once('/'), target.split_once('/')) {
        (Some((owner, "*")), Some((target_owner, _))) => owner == target_owner,
        _ => false,
    }
}

/// Returns the pattern of the repositories that both patterns match, if any.
///
/// Patterns either nest or share no repository, so the result is one of the two.
fn narrower<'a>(left: &'a str, right: &'a str) -> Option<&'a str> {
    if covers(left, right) {
        Some(right)
    } else if covers(right, left) {
        Some(left)
    } else {
        None
    }
}

/// Builds the error `code` for the configuration key `key`.
fn error(code: &str, key: &str, message: String) -> Error {
    Error::config(code, key, Some(BoxError::from(message)))
}
