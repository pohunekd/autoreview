//! The `gh` CLI calls of the engine and the decoding of their JSON output.
//!
//! Every call is an argv list, never a shell, and the child gets the [`GH_INHERITED`] variables
//! of the engine and the token of one credential.

// Rust guideline compliant 2026-06-26

mod candidates;
mod discussion;
mod files;
mod permission;
mod pull_request;
mod reviews;
mod token;

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fmt;
use std::time::Duration;

use autoreview_engine::config::{Config, Credential, Credentials};
use autoreview_engine::error::{BoxError, Error};
use serde::Deserialize;
use serde::Serialize;
use serde::de::IgnoredAny;
use tokio_util::sync::CancellationToken;

use crate::process::{GH_INHERITED, child_environment, run};

/// Fields of a pull request in the GraphQL selection, in Go's order.
const PULL_REQUEST_FIELDS: &str = concat!(
    "id number title url state isDraft isCrossRepository\n",
    "      headRefOid headRefName baseRefOid baseRefName\n",
    "      author { __typename login }\n",
    "      labels(first: 100) { nodes { name } }\n",
    "      reviewRequests(first: 100) { nodes { requestedReviewer { ... on User { login } } } }\n",
    "      repository { nameWithOwner }",
);

/// Variables the `gh` child sets besides its token: plain output and no prompts.
const GH_CHILD_ENVIRONMENT: [(&str, &str); 2] = [("GH_PAGER", "cat"), ("GH_PROMPT_DISABLED", "1")];

/// Variable that carries the token of the call's credential to the `gh` child.
const TOKEN_VARIABLE: &str = "GH_TOKEN";

/// Configuration key named when a call that names no repository finds several credentials.
const CREDENTIALS_KEY: &str = "github.credentials";

/// Returns the GraphQL search that discovers pull requests (`graphQLSearch`).
pub(crate) fn graphql_search() -> String {
    format!(
        concat!(
            "query($searchQuery: String!, $after: String) {{\n",
            "  search(query: $searchQuery, type: ISSUE, first: 50, after: $after) {{\n",
            "    pageInfo {{ hasNextPage endCursor }}\n",
            "    nodes {{ ... on PullRequest {{ {PULL_REQUEST_FIELDS} }} }}\n",
            "  }}\n",
            "}}"
        ),
        PULL_REQUEST_FIELDS = PULL_REQUEST_FIELDS
    )
}

/// Returns the GraphQL query that reads one pull request (`graphQLPullRequest`).
fn graphql_pull_request() -> String {
    format!(
        concat!(
            "query($owner: String!, $name: String!, $number: Int!) {{\n",
            "  repository(owner: $owner, name: $name) {{\n",
            "    pullRequest(number: $number) {{ {PULL_REQUEST_FIELDS} }}\n",
            "  }}\n",
            "}}"
        ),
        PULL_REQUEST_FIELDS = PULL_REQUEST_FIELDS
    )
}

/// The `gh` CLI of the engine and the GitHub credentials that its calls use.
///
/// The tokens are left out of the `Debug` output.
pub(crate) struct GitHub {
    /// Path of the `gh` binary.
    binary: String,
    /// The credentials; a call that acts on a repository uses the one that serves it.
    credentials: Credentials,
    /// GitHub host that pull request URLs name, for example `example.org`.
    host: String,
    /// Timeout of one `gh` call.
    timeout: Duration,
    /// Number of results a review request search may return before it fails as truncated.
    poll_limit: i64,
    /// Number of pages a paged read may fetch before it fails.
    search_page_limit: i64,
    /// Variables of the engine's secrets, which the `gh` child does not inherit.
    secret_variables: BTreeSet<String>,
}

impl fmt::Debug for GitHub {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GitHub")
            .field("binary", &self.binary)
            .field("credentials", &self.credentials)
            .field("host", &self.host)
            .field("timeout", &self.timeout)
            .field("poll_limit", &self.poll_limit)
            .field("search_page_limit", &self.search_page_limit)
            .finish_non_exhaustive()
    }
}

impl GitHub {
    /// Creates the client from the `gh` settings and the credentials of the configuration.
    pub(crate) fn new(config: &Config) -> Self {
        Self {
            binary: config.gh_bin.clone(),
            credentials: config.credentials.clone(),
            host: config.github_host.clone(),
            timeout: config.gh_timeout,
            poll_limit: config.poll_limit,
            search_page_limit: config.search_page_limit,
            secret_variables: config.secret_variables.clone(),
        }
    }

    /// Returns the only credential, which the calls that name no repository use.
    ///
    /// # Errors
    ///
    /// Returns `configuration_invalid` naming `github.credentials` when there are several.
    fn only_credential(&self) -> Result<&Credential, Error> {
        self.credentials.single().ok_or_else(|| {
            let message = "a gh call without a repository needs exactly one credential";
            Error::config(
                "configuration_invalid",
                CREDENTIALS_KEY,
                Some(message.into()),
            )
        })
    }

    /// Returns the environment of a `gh` child that acts with `credential`.
    ///
    /// It is the [`GH_INHERITED`] variables of `parent` without the secrets, then the token of
    /// `credential` as `GH_TOKEN` and the [`GH_CHILD_ENVIRONMENT`] settings.
    fn environment(
        &self,
        parent: impl IntoIterator<Item = (OsString, OsString)>,
        credential: &Credential,
    ) -> Vec<(OsString, OsString)> {
        let token = (
            OsString::from(TOKEN_VARIABLE),
            OsString::from(&credential.token),
        );
        let settings = GH_CHILD_ENVIRONMENT
            .iter()
            .map(|(name, value)| (OsString::from(*name), OsString::from(*value)));
        let extra = std::iter::once(token).chain(settings);
        child_environment(parent, &GH_INHERITED, &self.secret_variables, extra)
    }

    /// Runs `gh` with `args` and the token of `credential` and returns its stdout.
    ///
    /// The stdout is valid JSON. `input` is serialized to the child's stdin; `None` gives an
    /// empty stdin.
    pub(crate) async fn github<T: Serialize + ?Sized>(
        &self,
        cancel: &CancellationToken,
        credential: &Credential,
        args: &[String],
        input: Option<&T>,
    ) -> Result<String, Error> {
        let payload = if let Some(value) = input {
            serde_json::to_vec(value).map_err(|error| {
                Error::workflow("github_request_invalid", true, Some(Box::new(error)))
            })?
        } else {
            Vec::new()
        };
        let environment = self.environment(std::env::vars_os(), credential);
        let output = run(
            cancel,
            &self.binary,
            args,
            &payload,
            &environment,
            self.timeout,
        )
        .await
        .map_err(|error| {
            let (code, permanent) = gh_failure(&error.to_string());
            Error::workflow(code, permanent, Some(Box::new(error)))
        })?;
        if serde_json::from_str::<IgnoredAny>(&output).is_err() {
            return Err(response_invalid(None));
        }
        Ok(output)
    }
}

/// The viewer object of `gh api user`.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct Viewer {
    /// Login of the account.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    login: String,
}

autoreview_go_compat::object_only_serde!(Viewer);

/// Returns the lower-case login of the viewer object that `output` holds.
///
/// An empty login is refused, as Go refuses it.
fn decode_viewer_login(output: &str) -> Result<String, Error> {
    let viewer: Viewer =
        serde_json::from_str(output).map_err(|error| response_invalid(Some(Box::new(error))))?;
    if viewer.login.is_empty() {
        return Err(response_invalid(None));
    }
    Ok(viewer.login.to_ascii_lowercase())
}

/// Returns the code and the permanence of a failed `gh` call, read from its error message.
///
/// A message that names `HTTP 422` is a request GitHub rejected, which a retry does not change.
fn gh_failure(message: &str) -> (&'static str, bool) {
    if names_http_422(message) {
        ("github_request_rejected", true)
    } else {
        ("github_request_failed", false)
    }
}

/// Reports whether `message` names `HTTP 422`, as Go's `(?i)\bHTTP\s+422\b` does.
///
/// Word boundaries and `\s` are ASCII in RE2, so only `\t`, `\n`, `\f`, `\r` and space separate
/// the words, and `HTTP` matches in any ASCII case.
fn names_http_422(message: &str) -> bool {
    let bytes = message.as_bytes();
    let length = bytes.len();
    for start in 0..length {
        if start != 0
            && let Some(previous) = bytes.get(start - 1)
            && is_word_byte(*previous)
        {
            continue;
        }
        let Some(remainder) = bytes.get(start..) else {
            continue;
        };
        if remainder.len() < 4 {
            continue;
        }
        let Some((head, after_http)) = remainder.split_at_checked(4) else {
            continue;
        };
        if !head.eq_ignore_ascii_case(b"http") {
            continue;
        }
        let mut spaces = 0;
        let mut rest = after_http;
        while let Some(first) = rest.first() {
            if is_space_byte(*first) {
                spaces += 1;
                if let Some(next) = rest.get(1..) {
                    rest = next;
                } else {
                    rest = &[];
                    break;
                }
            } else {
                break;
            }
        }
        if spaces == 0 {
            continue;
        }
        let Some(after_422) = rest.strip_prefix(b"422") else {
            continue;
        };
        if let Some(next) = after_422.first()
            && is_word_byte(*next)
        {
            continue;
        }
        return true;
    }
    false
}

/// Reports whether `byte` is an ASCII word character, as RE2's `\w` is.
fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Reports whether `byte` is one of the ASCII whitespace bytes that RE2's `\s` matches.
fn is_space_byte(byte: u8) -> bool {
    matches!(byte, b'\t' | b'\n' | 0x0C | b'\r' | b' ')
}

/// Returns the `github_response_invalid` error, with `cause` when there is one.
///
/// It is permanent: a response that does not decode does not change on a retry.
fn response_invalid(cause: Option<BoxError>) -> Error {
    Error::workflow("github_response_invalid", true, cause)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::ffi::OsString;
    use std::time::Duration;

    use autoreview_engine::config::{Credential, Credentials};
    use autoreview_engine::error_code;

    use super::GitHub;
    use crate::pohunek::local_environment;

    fn pairs(items: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        items
            .iter()
            .map(|(name, value)| (OsString::from(name), OsString::from(value)))
            .collect()
    }

    fn credential(name: &str, token: &str, pattern: &str) -> Credential {
        Credential {
            name: name.to_owned(),
            login: format!("{name}-bot"),
            token: token.to_owned(),
            matches: vec![pattern.to_owned()],
        }
    }

    /// With two credentials, each `gh` child holds only the token of its repository's
    /// credential, and a pohunek child holds none (#14).
    #[test]
    fn each_child_gets_only_the_token_of_its_credential() {
        let credentials = Credentials::new(vec![
            credential("first", "synthetic-first", "example-one/*"),
            credential("second", "synthetic-second", "example-two/*"),
        ])
        .expect("the credentials are valid");
        let github = GitHub {
            binary: "/usr/bin/gh".to_owned(),
            credentials,
            host: "github.com".to_owned(),
            timeout: Duration::from_secs(30),
            poll_limit: 100,
            search_page_limit: 10,
            secret_variables: BTreeSet::from(["EXAMPLE_SECOND_TOKEN".to_owned()]),
        };
        let parent = pairs(&[
            ("PATH", "/usr/bin"),
            ("GH_TOKEN", "synthetic-parent"),
            ("GITHUB_TOKEN", "synthetic-parent"),
            ("AUTOREVIEW_FIRST_TOKEN", "synthetic-first"),
            ("EXAMPLE_SECOND_TOKEN", "synthetic-second"),
        ]);
        // (repository, the token of its `gh` child or the error code)
        let rows = [
            ("example-one/app", Ok("synthetic-first")),
            ("example-two/app", Ok("synthetic-second")),
            ("Example-One/App", Ok("synthetic-first")),
            ("example-three/app", Err("credential_not_covered")),
        ];
        for (repository, expected) in rows {
            let built = github
                .credentials
                .serving(repository)
                .map(|credential| github.environment(parent.clone(), credential));
            match (built, expected) {
                (Ok(environment), Ok(token)) => assert_eq!(
                    environment,
                    pairs(&[
                        ("PATH", "/usr/bin"),
                        ("GH_TOKEN", token),
                        ("GH_PAGER", "cat"),
                        ("GH_PROMPT_DISABLED", "1"),
                    ]),
                    "{repository}"
                ),
                (Err(error), Err(code)) => assert_eq!(error_code(&error), code, "{repository}"),
                (built, _) => panic!("{repository}: {built:?}"),
            }
        }
        // A call that names no repository never picks one of several credentials.
        let unscoped = github
            .only_credential()
            .map_err(|error| error_code(&error).to_owned());
        assert_eq!(unscoped.err().as_deref(), Some("configuration_invalid"));
        assert_eq!(
            local_environment(parent),
            pairs(&[("PATH", "/usr/bin"), ("PAGER", "cat")])
        );
    }
}
