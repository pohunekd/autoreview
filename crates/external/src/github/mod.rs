//! The `gh` CLI calls of the engine and the decoding of their JSON output.
//!
//! Every call is an argv list, never a shell, and the child gets the [`GH_INHERITED`] variables
//! of the engine and the token of one credential.

// Rust guideline compliant 2026-06-26

mod candidates;
mod contents;
mod discussion;
mod files;
mod guard;
mod issues;
mod permission;
mod pull_request;
mod reviews;
mod signals;
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

/// Returns the fields of a pull request in the GraphQL selection.
///
/// `team_members` adds the members of a requested team. It is left out of the search, where the
/// nested page sizes would exceed GitHub's node limit (`MAX_NODE_LIMIT_EXCEEDED`) and fail the
/// label search of every pull request.
fn pull_request_fields(team_members: bool) -> String {
    let members = if team_members {
        " ... on Team { members(first: 100) { nodes { login } } }"
    } else {
        ""
    };
    format!(
        concat!(
            "id number title url state isDraft isCrossRepository\n",
            "      headRefOid headRefName baseRefOid baseRefName\n",
            "      author {{ __typename login }}\n",
            "      labels(first: 100) {{ nodes {{ name }} }}\n",
            "      reviewRequests(first: 100) {{ nodes {{ requestedReviewer {{ __typename ... on User {{ login }}{members} }} }} }}\n",
            "      latestReviews(first: 100) {{ nodes {{ author {{ login }} }} }}\n",
            "      repository {{ nameWithOwner }}",
        ),
        members = members
    )
}

/// Variables the `gh` child sets besides its token: plain output and no prompts.
const GH_CHILD_ENVIRONMENT: [(&str, &str); 2] = [("GH_PAGER", "cat"), ("GH_PROMPT_DISABLED", "1")];

/// Variable that carries the token of the call's credential to the `gh` child.
const TOKEN_VARIABLE: &str = "GH_TOKEN";

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
        PULL_REQUEST_FIELDS = pull_request_fields(false)
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
        PULL_REQUEST_FIELDS = pull_request_fields(true)
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
    /// Text that marks a discussion comment as machine-written.
    ai_signed_pattern: String,
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
            ai_signed_pattern: config.ai_signed_pattern.clone(),
        }
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

    /// Runs a GraphQL read of pull request data with the token of `credential` and returns its
    /// stdout.
    ///
    /// `gh` exits 1 when the response holds errors, with the response on stdout. A response whose
    /// only errors are on a requested reviewer (a team the token cannot read) is returned like a
    /// success; the decoders then read the team request as an unreadable non-user request. A
    /// response whose errors are all `FORBIDDEN` fails with the permanent `github_read_forbidden`,
    /// and a GraphQL error body of typed errors that are not a rate limit with the permanent
    /// `github_read_unrecognized`, so one pull request never stops the pass. Every other failure,
    /// an untyped error included, maps as in [`GitHub::github`] and stays retryable.
    pub(crate) async fn graphql_read(
        &self,
        cancel: &CancellationToken,
        credential: &Credential,
        args: &[String],
    ) -> Result<String, Error> {
        let environment = self.environment(std::env::vars_os(), credential);
        let result = run(cancel, &self.binary, args, &[], &environment, self.timeout).await;
        let output = match result {
            Ok(output) => output,
            Err(error) => {
                return match graphql_failure(error.stdout()) {
                    GraphqlFailure::ReviewerOnly => {
                        tracing::warn!(
                            credential = %credential.name,
                            "pull request read has errors only on a requested reviewer"
                        );
                        Ok(error.stdout().to_owned())
                    }
                    GraphqlFailure::Forbidden => Err(Error::workflow(
                        "github_read_forbidden",
                        true,
                        Some(Box::new(error)),
                    )),
                    GraphqlFailure::Unrecognized => {
                        tracing::warn!(
                            credential = %credential.name,
                            error_types = %error_types(error.stdout()),
                            "pull request read has GraphQL errors of an unknown shape, skipping it"
                        );
                        Err(Error::workflow(
                            "github_read_unrecognized",
                            true,
                            Some(Box::new(error)),
                        ))
                    }
                    GraphqlFailure::Other => {
                        let (code, permanent) = gh_failure(&error.to_string());
                        Err(Error::workflow(code, permanent, Some(Box::new(error))))
                    }
                };
            }
        };
        if serde_json::from_str::<IgnoredAny>(&output).is_err() {
            return Err(response_invalid(None));
        }
        Ok(output)
    }
}

/// How a failed GraphQL read is told apart from its stdout.
#[derive(Debug, PartialEq, Eq)]
enum GraphqlFailure {
    /// `data` is present and every error is on a requested reviewer of a review request.
    ReviewerOnly,
    /// Every error is of type `FORBIDDEN`: GitHub refuses the token's read.
    Forbidden,
    /// A GraphQL error body whose errors all have a type that is none of the above, such as an
    /// unknown type: the read of this pull request is skipped and the rest of the pass goes on.
    Unrecognized,
    /// Anything else: no GraphQL body, an error without a type, or a rate limit, which a later
    /// pass can retry.
    Other,
}

/// Error type of a GraphQL rate limit, which is transient.
const RATE_LIMITED: &str = "RATE_LIMITED";

/// Returns the `type` of a GraphQL error, `None` when it has none or it is not text.
fn error_type(error: &serde_json::Value) -> Option<&str> {
    error.get("type").and_then(serde_json::Value::as_str)
}

/// Sorts the stdout of a failed `gh api graphql` call.
///
/// The decision rests on the documented GraphQL error format (`errors[].type`, `message` and
/// `path`), on the error paths and types, never on the message text.
fn graphql_failure(stdout: &str) -> GraphqlFailure {
    let Ok(response) = serde_json::from_str::<serde_json::Value>(stdout) else {
        return GraphqlFailure::Other;
    };
    let Some(errors) = response
        .get("errors")
        .and_then(serde_json::Value::as_array)
        .filter(|errors| !errors.is_empty())
    else {
        return GraphqlFailure::Other;
    };
    if reviewer_errors_tolerated(stdout, errors) {
        GraphqlFailure::ReviewerOnly
    } else if errors
        .iter()
        .all(|error| error_type(error).is_some_and(|kind| kind.eq_ignore_ascii_case("FORBIDDEN")))
    {
        GraphqlFailure::Forbidden
    } else if errors
        .iter()
        .any(|error| error_type(error).is_none_or(|kind| kind.eq_ignore_ascii_case(RATE_LIMITED)))
    {
        GraphqlFailure::Other
    } else {
        GraphqlFailure::Unrecognized
    }
}

/// Lists the error types of a GraphQL error body for the log, `none` for an error without one.
///
/// Only the types are logged, never the message text.
fn error_types(stdout: &str) -> String {
    let Ok(response) = serde_json::from_str::<serde_json::Value>(stdout) else {
        return String::new();
    };
    response
        .get("errors")
        .and_then(serde_json::Value::as_array)
        .map(|errors| {
            errors
                .iter()
                .map(|error| error_type(error).unwrap_or("none"))
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default()
}

/// Reports whether `errors` is non-empty and every error is on the `requestedReviewer` of a
/// review request node.
///
/// The error path contains `reviewRequests`, `nodes`, an index and `requestedReviewer`, and may
/// go deeper into the reviewer, such as its team members.
pub(super) fn reviewer_errors_only(errors: &[serde_json::Value]) -> bool {
    !errors.is_empty() && errors.iter().all(reviewer_error)
}

/// Reports whether one GraphQL error is on a review request's `requestedReviewer`.
fn reviewer_error(error: &serde_json::Value) -> bool {
    let Some(path) = error.get("path").and_then(serde_json::Value::as_array) else {
        return false;
    };
    path.windows(4).any(|window| {
        window[0].as_str() == Some("reviewRequests")
            && window[1].as_str() == Some("nodes")
            && window[2].is_u64()
            && window[3].as_str() == Some("requestedReviewer")
    })
}

/// Reports whether a decoded response with `errors` may be used: `data` is an object and every
/// error is on a requested reviewer.
pub(super) fn reviewer_errors_tolerated(output: &str, errors: &[serde_json::Value]) -> bool {
    reviewer_errors_only(errors)
        && serde_json::from_str::<serde_json::Value>(output).is_ok_and(|response| {
            response
                .get("data")
                .is_some_and(serde_json::Value::is_object)
        })
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
/// An empty login is refused.
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
/// One that names `HTTP 401` is a token GitHub refuses: `credential_invalid`, retried by the
/// step, which the scheduler does not retry.
fn gh_failure(message: &str) -> (&'static str, bool) {
    if names_http(message, b"422") {
        ("github_request_rejected", true)
    } else if names_http(message, b"401") {
        ("credential_invalid", false)
    } else {
        ("github_request_failed", false)
    }
}

/// Reports whether `message` names `HTTP` and `status`, as `(?i)\bHTTP\s+<status>\b` does.
///
/// `HTTP` matches in any ASCII case and must start a word. One or more of `\t`, `\n`, `\f`,
/// `\r` and space separate it from `status`, which must end a word (ASCII word characters).
fn names_http(message: &str, status: &[u8]) -> bool {
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
        let Some(after_status) = rest.strip_prefix(status) else {
            continue;
        };
        if let Some(next) = after_status.first()
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

/// Returns the `gh api graphql` arguments that pass `repository` as the `owner` and `name`
/// variables; a value without a `/` is the owner of an empty name.
///
/// Both are sent as raw strings (`-f`): a typed field (`-F`) would turn a repository named
/// `true`, `null` or `123` into a boolean, null or number.
fn repository_variables(repository: &str) -> [String; 4] {
    let (owner, name) = repository.split_once('/').unwrap_or((repository, ""));
    [
        "-f".to_owned(),
        format!("owner={owner}"),
        "-f".to_owned(),
        format!("name={name}"),
    ]
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

    use super::{
        GitHub, GraphqlFailure, graphql_failure, graphql_pull_request, graphql_search,
        repository_variables,
    };
    use crate::pohunek::local_environment;

    /// Each row is a repository and the owner and name that `gh` receives as raw strings: a
    /// name that reads as a JSON literal or a number must not be typed by `-F`.
    #[test]
    fn repository_variables_are_raw_strings() {
        let rows = [
            ("acme/widget", "owner=acme", "name=widget"),
            ("true/123", "owner=true", "name=123"),
            ("null/false", "owner=null", "name=false"),
            ("acme", "owner=acme", "name="),
        ];
        for (repository, owner, name) in rows {
            let got = repository_variables(repository);
            assert_eq!(got, ["-f", owner, "-f", name], "{repository}");
        }
    }

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
            ai_signed_pattern: "Written by AI:".to_owned(),
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
        assert_eq!(
            local_environment(parent),
            pairs(&[("PATH", "/usr/bin"), ("PAGER", "cat")])
        );
    }

    /// The error of a requested reviewer node a token cannot read, as GraphQL reports it.
    fn reviewer_error(index: u64) -> serde_json::Value {
        serde_json::json!({
            "type": "FORBIDDEN",
            "path": ["repository", "pullRequest", "reviewRequests", "nodes", index,
                "requestedReviewer"],
            "message": "Resource not accessible by personal access token",
        })
    }

    /// Each row is the stdout of a failed GraphQL read and how it is sorted (#576).
    #[test]
    fn failed_graphql_read_is_sorted_by_error_path_and_type() {
        let data = serde_json::json!({"repository": {"pullRequest": {}}});
        let other = serde_json::json!({
            "type": "FORBIDDEN", "path": ["repository", "pullRequest"], "message": "x",
        });
        let unknown = serde_json::json!({"type": "INTERNAL", "message": "x"});
        let rows = [
            (
                serde_json::json!({"data": data, "errors": [reviewer_error(0), reviewer_error(2)]}),
                GraphqlFailure::ReviewerOnly,
            ),
            (
                serde_json::json!({"data": null, "errors": [reviewer_error(0)]}),
                GraphqlFailure::Forbidden,
            ),
            (
                serde_json::json!({"data": data, "errors": [reviewer_error(0), other.clone()]}),
                GraphqlFailure::Forbidden,
            ),
            (
                serde_json::json!({"data": data, "errors": [reviewer_error(0), unknown]}),
                GraphqlFailure::Unrecognized,
            ),
            (
                serde_json::json!({"data": null, "errors": [{"message": "no type"}, "text"]}),
                GraphqlFailure::Other,
            ),
            (
                serde_json::json!({"data": null, "errors": [{"type": "forbidden", "path": [],
                    "message": "x"}]}),
                GraphqlFailure::Forbidden,
            ),
            (
                serde_json::json!({"data": null, "errors": [{"type": "RATE_LIMITED",
                    "message": "x"}, unknown]}),
                GraphqlFailure::Other,
            ),
            (
                serde_json::json!({"data": data, "errors": []}),
                GraphqlFailure::Other,
            ),
            (serde_json::json!({"data": data}), GraphqlFailure::Other),
        ];
        for (stdout, want) in rows {
            assert_eq!(graphql_failure(&stdout.to_string()), want, "{stdout}");
        }
        assert_eq!(graphql_failure("not json"), GraphqlFailure::Other);
        assert_eq!(graphql_failure(""), GraphqlFailure::Other);
    }

    /// Each row is the review requests of a pull request, with the reviewer errors GitHub adds,
    /// and the result of decoding it for `example-user` (#576).
    #[test]
    fn team_review_requests_decode_to_a_candidate_or_a_rejection() {
        use super::pull_request::{decode_pull_request, decode_pull_request_response};
        use autoreview_policy::{Trigger, TriggerKind};

        let user = serde_json::json!({"requestedReviewer": {"__typename": "User",
            "login": "example-user"}});
        let other_user = serde_json::json!({"requestedReviewer": {"__typename": "User",
            "login": "colleague"}});
        let team = serde_json::json!({"requestedReviewer": {"__typename": "Team"}});
        let unreadable = serde_json::json!({"requestedReviewer": null});
        let stray = serde_json::json!({"type": "FORBIDDEN", "path": ["repository", "labels"]});
        let rows = [
            (
                vec![user.clone(), unreadable.clone()],
                vec![reviewer_error(1)],
                Ok(()),
            ),
            (vec![user.clone(), team.clone()], vec![], Ok(())),
            (
                vec![unreadable.clone()],
                vec![reviewer_error(0)],
                Err("review_requested_from_team"),
            ),
            (
                vec![team.clone()],
                vec![],
                Err("review_requested_from_team"),
            ),
            (
                vec![other_user.clone()],
                vec![],
                Err("review_request_not_active"),
            ),
            (vec![], vec![], Err("review_request_not_active")),
            (
                vec![user.clone(), unreadable.clone()],
                vec![reviewer_error(1), stray],
                Err("github_response_invalid"),
            ),
        ];
        let trigger = Trigger {
            kind: TriggerKind::ReviewRequested,
            label: String::new(),
        };
        for (nodes, errors, want) in rows {
            let response = serde_json::json!({
                "data": {"repository": {"pullRequest": {
                    "id": "PR_node", "number": 7, "title": "Add widget",
                    "url": "https://github.com/acme/widget/pull/7", "state": "OPEN",
                    "isDraft": false, "isCrossRepository": false,
                    "headRefOid": "a".repeat(40), "headRefName": "feature",
                    "baseRefOid": "b".repeat(40), "baseRefName": "main",
                    "author": {"__typename": "User", "login": "colleague"},
                    "labels": {"nodes": []},
                    "reviewRequests": {"nodes": nodes},
                    "repository": {"nameWithOwner": "acme/widget"},
                }}},
                "errors": errors,
            })
            .to_string();
            let got = decode_pull_request_response(&response)
                .and_then(|raw| decode_pull_request(&raw, "github.com", "example-user", &trigger))
                .map(|_| ())
                .map_err(|error| error_code(&error).to_owned());
            assert_eq!(got, want.map_err(str::to_owned), "{response}");
        }
        let without_data = serde_json::json!({"data": null, "errors": [reviewer_error(0)]});
        let got = decode_pull_request_response(&without_data.to_string())
            .map_err(|error| error_code(&error).to_owned());
        assert_eq!(got.err().as_deref(), Some("github_response_invalid"));
    }

    /// The search selection stays within GitHub's node limit: it names no team members, while the
    /// single pull request read does (#576).
    #[test]
    fn search_query_leaves_team_members_out() {
        assert!(!graphql_search().contains("members"));
        assert!(graphql_pull_request().contains("members(first: 100)"));
    }

    /// A team request that stays on the pull request after the credential's login or a readable
    /// team member has reviewed is not a team-only request (#576, MEDIUM1).
    #[test]
    fn answered_team_request_is_not_reported_as_team_only() {
        use super::pull_request::{decode_pull_request, decode_pull_request_response};
        use autoreview_policy::{Trigger, TriggerKind};

        let team = serde_json::json!({"requestedReviewer": {"__typename": "Team",
            "members": {"nodes": [{"login": "Team-Mate"}, {"login": "other-mate"}]}}});
        let team_with_author = serde_json::json!({"requestedReviewer": {"__typename": "Team",
            "members": {"nodes": [{"login": "colleague"}, {"login": "team-mate"}]}}});
        let unreadable = serde_json::json!({"requestedReviewer": null});
        let review = |login: &str| serde_json::json!({"author": {"login": login}});
        let rows = [
            (vec![team.clone()], vec![], "review_requested_from_team"),
            (
                vec![team.clone()],
                vec![review("team-mate")],
                "review_request_not_active",
            ),
            (
                vec![team.clone()],
                vec![review("outsider")],
                "review_requested_from_team",
            ),
            (
                vec![unreadable.clone()],
                vec![review("Example-User")],
                "review_request_not_active",
            ),
            (
                vec![unreadable.clone()],
                vec![review("team-mate")],
                "review_requested_from_team",
            ),
            // The pull request author is a team member and reviewed their own pull request.
            (
                vec![team_with_author.clone()],
                vec![review("Colleague")],
                "review_requested_from_team",
            ),
            (
                vec![team_with_author.clone()],
                vec![review("colleague"), review("team-mate")],
                "review_request_not_active",
            ),
        ];
        let trigger = Trigger {
            kind: TriggerKind::ReviewRequested,
            label: String::new(),
        };
        for (nodes, reviews, want) in rows {
            let response = serde_json::json!({
                "data": {"repository": {"pullRequest": {
                    "id": "PR_node", "number": 7, "title": "Add widget",
                    "url": "https://github.com/acme/widget/pull/7", "state": "OPEN",
                    "isDraft": false, "isCrossRepository": false,
                    "headRefOid": "a".repeat(40), "headRefName": "feature",
                    "baseRefOid": "b".repeat(40), "baseRefName": "main",
                    "author": {"__typename": "User", "login": "colleague"},
                    "labels": {"nodes": []},
                    "reviewRequests": {"nodes": nodes},
                    "latestReviews": {"nodes": reviews},
                    "repository": {"nameWithOwner": "acme/widget"},
                }}},
            })
            .to_string();
            let got = decode_pull_request_response(&response)
                .and_then(|raw| decode_pull_request(&raw, "github.com", "example-user", &trigger))
                .map(|_| ())
                .map_err(|error| error_code(&error).to_owned());
            assert_eq!(got, Err(want.to_owned()), "{response}");
        }
    }
}
