//! The `gh` CLI calls of the engine and the decoding of their JSON output.
//!
//! Mirrors `cmd/autoreview/github.go:1-326`. Every call is an argv list, never a shell, and the
//! child gets a cleaned environment that carries the GitHub token.

// Rust guideline compliant 2026-06-26

mod candidates;
mod discussion;
mod files;
mod pull_request;
mod reviews;

use std::ffi::OsString;
use std::fmt;
use std::time::Duration;

use autoreview_engine::error::{BoxError, Error};
use serde::Deserialize;
use serde::Serialize;
use serde::de::IgnoredAny;

use crate::process::{child_environment, run};

/// Fields of a pull request in the GraphQL selection, in Go's order.
const PULL_REQUEST_FIELDS: &str = concat!(
    "id number title url state isDraft headRefOid headRefName baseRefOid baseRefName\n",
    "      author { __typename login }\n",
    "      labels(first: 100) { nodes { name } }\n",
    "      reviewRequests(first: 100) { nodes { requestedReviewer { ... on User { login } } } }\n",
    "      repository { nameWithOwner }",
);

/// Variables the `gh` child sets besides its token: plain output and no prompts.
const GH_CHILD_ENVIRONMENT: [(&str, &str); 2] = [("GH_PAGER", "cat"), ("GH_PROMPT_DISABLED", "1")];

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

/// The `gh` CLI of the engine and the GitHub identity that its calls use.
///
/// The token is left out of the `Debug` output.
pub(crate) struct GitHub {
    /// Path of the `gh` binary.
    binary: String,
    /// Token of the engine's GitHub account; only the `gh` child receives it.
    token: String,
    /// GitHub host that pull request URLs name, for example `example.org`.
    host: String,
    /// Login of the account that the engine reviews for.
    login: String,
    /// Timeout of one `gh` call.
    timeout: Duration,
    /// Number of results a review request search may return before it fails as truncated.
    poll_limit: i64,
    /// Number of pages a paged read may fetch before it fails.
    search_page_limit: i64,
}

impl fmt::Debug for GitHub {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GitHub")
            .field("binary", &self.binary)
            .field("host", &self.host)
            .field("login", &self.login)
            .field("timeout", &self.timeout)
            .field("poll_limit", &self.poll_limit)
            .field("search_page_limit", &self.search_page_limit)
            .finish_non_exhaustive()
    }
}

impl GitHub {
    /// Creates the client from the `gh` settings of the configuration.
    pub(crate) fn new(
        binary: String,
        token: String,
        host: String,
        login: String,
        timeout: Duration,
        poll_limit: i64,
        search_page_limit: i64,
    ) -> Self {
        Self {
            binary,
            token,
            host,
            login,
            timeout,
            poll_limit,
            search_page_limit,
        }
    }

    /// Runs `gh` with `args` and returns its stdout, which is valid JSON.
    ///
    /// `input` is serialized to the child's stdin; `None` gives an empty stdin.
    pub(crate) async fn github<T: Serialize + ?Sized>(
        &self,
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
        let extra = std::iter::once((
            OsString::from("GH_TOKEN"),
            OsString::from(self.token.clone()),
        ))
        .chain(
            GH_CHILD_ENVIRONMENT
                .iter()
                .map(|(name, value)| (OsString::from(*name), OsString::from(*value))),
        );
        let environment = child_environment(std::env::vars_os(), extra);
        let output = run(&self.binary, args, &payload, &environment, self.timeout)
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

    /// Returns the login of the account behind the GitHub token, in lower case.
    #[expect(dead_code, reason = "called by C01")]
    pub(crate) async fn viewer_login(&self) -> Result<String, Error> {
        let output = self
            .github::<()>(&["api".to_owned(), "user".to_owned()], None)
            .await?;
        decode_viewer_login(&output)
    }
}

/// The viewer object of `gh api user`.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct Viewer {
    /// Login of the account.
    login: String,
}

autoreview_go_compat::object_only_serde!(Viewer);

/// Returns the lower-case login of the viewer object that `output` holds.
///
/// An empty login is refused, as Go refuses it.
#[expect(dead_code, reason = "called by C01")]
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
