//! The read of the pull request texts that a rule's text guard scans.
//!
//! Only the fields the guard names are read, in field order, so the byte bound cuts the later
//! kinds first. Every list is paged until it ends, the bound is reached or the page limit stops
//! it; text left out sets `truncated`.

// Rust guideline compliant 2026-11-15

#![expect(
    dead_code,
    reason = "GitHub::guard_texts calls it once the Integrations method exists"
)]

use autoreview_engine::error::Error;
use autoreview_go_compat::null_default;
use serde::Deserialize;

use super::response_invalid;

/// Query of the title and body of one pull request.
const GRAPHQL_GUARD_TEXT: &str = concat!(
    "query($owner: String!, $name: String!, $number: Int!) {\n",
    "  repository(owner: $owner, name: $name) {\n",
    "    pullRequest(number: $number) { title body }\n",
    "  }\n",
    "}",
);

/// The response of a guard query.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Response {
    /// The data of the query.
    #[serde(deserialize_with = "null_default::deserialize")]
    data: Data,
    /// The errors of the response; any entry makes the response invalid.
    #[serde(deserialize_with = "null_default::deserialize")]
    errors: Vec<serde_json::Value>,
}

/// The data of a guard query.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Data {
    /// The repository, `null` when there is none.
    repository: Option<Repository>,
}

/// The repository node of a guard query.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Repository {
    /// The pull request, `null` when the repository has none with that number.
    #[serde(rename = "pullRequest")]
    pull_request: Option<PullNode>,
}

/// The pull request node: its title and body, or one page of a list.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PullNode {
    /// The title; `null` reads as empty.
    #[serde(deserialize_with = "null_default::deserialize")]
    title: String,
    /// The body; `null` reads as empty.
    #[serde(deserialize_with = "null_default::deserialize")]
    body: String,
    /// One page of the list the query names.
    page: Option<Page>,
}

/// One page of a list, or the comments of one review thread.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Page {
    /// Paging of the list.
    #[serde(rename = "pageInfo", deserialize_with = "null_default::deserialize")]
    page_info: PageInfo,
    /// The entries; a `null` entry makes the response invalid.
    #[serde(deserialize_with = "null_default::deserialize")]
    nodes: Vec<Option<Node>>,
}

/// Paging of a list.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PageInfo {
    /// Whether more entries follow.
    #[serde(rename = "hasNextPage", deserialize_with = "null_default::deserialize")]
    has_next_page: bool,
    /// The cursor of the last entry; `null` reads as empty.
    #[serde(rename = "endCursor", deserialize_with = "null_default::deserialize")]
    end_cursor: String,
}

/// One entry of a list: a commit, a comment, a review or a review thread.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Node {
    /// The commit of a commit entry.
    commit: Option<Commit>,
    /// The author; `null` for a deleted account.
    author: Option<Author>,
    /// The text of a comment or a review; `null` reads as empty.
    #[serde(deserialize_with = "null_default::deserialize")]
    body: String,
    /// The comments of a review thread.
    comments: Option<Page>,
}

/// The commit of a commit entry.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Commit {
    /// The full commit message; `null` reads as empty.
    #[serde(deserialize_with = "null_default::deserialize")]
    message: String,
}

/// The author of a comment or a review.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Author {
    /// Login of the author.
    #[serde(deserialize_with = "null_default::deserialize")]
    login: String,
}

/// Decodes the pull request node of a guard query.
///
/// A response with an error entry or without the pull request is `github_response_invalid`.
fn decode_pull_node(output: &str) -> Result<PullNode, Error> {
    let response: Response =
        serde_json::from_str(output).map_err(|error| response_invalid(Some(Box::new(error))))?;
    if !response.errors.is_empty() {
        return Err(response_invalid(None));
    }
    (response.data.repository)
        .and_then(|repository| repository.pull_request)
        .ok_or_else(|| response_invalid(None))
}
