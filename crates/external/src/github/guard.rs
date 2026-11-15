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
use autoreview_engine::integrations::GuardText;
use autoreview_engine::model::PullRequest;
use autoreview_go_compat::null_default;
use autoreview_guard::FieldKind;
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

/// A paged list of texts, read under the alias `page`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum List {
    /// The commits; each message is one text.
    Commits,
    /// The conversation comments.
    Comments,
    /// The review bodies.
    Reviews,
    /// The review threads; each comment is one text.
    Threads,
}

/// The lists in field order.
const LISTS: [List; 4] = [List::Commits, List::Comments, List::Reviews, List::Threads];

impl List {
    /// Returns the field kind of the list's texts.
    const fn kind(self) -> FieldKind {
        match self {
            Self::Commits => FieldKind::CommitMessage,
            Self::Comments | Self::Reviews => FieldKind::Comment,
            Self::Threads => FieldKind::ReviewThread,
        }
    }

    /// Returns the connection of the list and the selection of one entry.
    const fn selection(self) -> (&'static str, &'static str) {
        match self {
            Self::Commits => ("commits", "commit { message }"),
            Self::Comments => ("comments", AUTHORED_TEXT),
            Self::Reviews => ("reviews", AUTHORED_TEXT),
            Self::Threads => ("reviewThreads", THREAD_COMMENTS),
        }
    }
}

/// The selection of a comment or a review.
const AUTHORED_TEXT: &str = "author { login } body";

/// The selection of the comments of one review thread, at most one page of them.
const THREAD_COMMENTS: &str = concat!(
    "comments(first: 100) { pageInfo { hasNextPage endCursor } ",
    "nodes { author { login } body } }"
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

/// The texts read so far against the byte bound.
#[derive(Debug, Default)]
pub(super) struct GuardRead {
    /// The texts in read order.
    texts: Vec<GuardText>,
    /// Bytes read so far.
    bytes: usize,
    /// The byte bound.
    max_bytes: usize,
    /// Set when text was left out.
    truncated: bool,
}

impl GuardRead {
    /// Reports whether the bound is reached, so any further text is left out.
    fn full(&self) -> bool {
        self.bytes >= self.max_bytes
    }

    /// Adds one text, or marks the read truncated once the bound is reached.
    pub(super) fn push(&mut self, kind: FieldKind, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.full() {
            self.truncated = true;
            return;
        }
        self.bytes = self.bytes.saturating_add(text.len());
        self.texts.push(GuardText {
            kind,
            text: text.to_owned(),
        });
    }

    /// Marks the read truncated: a listing stopped before its end.
    pub(super) fn cut(&mut self) {
        self.truncated = true;
    }
}

/// Returns the `gh` arguments of `query` on `pull_request`, from `after` on when it is set.
fn guard_args(pull_request: &PullRequest, query: &str, after: &str) -> Vec<String> {
    let (owner, name) = match pull_request.repository.split_once('/') {
        Some((owner, name)) => (owner, name),
        None => (pull_request.repository.as_str(), ""),
    };
    let mut args = vec![
        "api".to_owned(),
        "graphql".to_owned(),
        "-f".to_owned(),
        format!("query={query}"),
        "-F".to_owned(),
        format!("owner={owner}"),
        "-F".to_owned(),
        format!("name={name}"),
        "-F".to_owned(),
        format!("number={}", pull_request.number),
    ];
    if !after.is_empty() {
        args.push("-f".to_owned());
        args.push(format!("after={after}"));
    }
    args
}

/// Returns the query of one page of `list`.
fn list_query(list: List) -> String {
    let (connection, entry) = list.selection();
    format!(
        concat!(
            "query($owner: String!, $name: String!, $number: Int!, $after: String) {{\n",
            "  repository(owner: $owner, name: $name) {{\n",
            "    pullRequest(number: $number) {{\n",
            "      page: {connection}(first: 100, after: $after) {{\n",
            "        pageInfo {{ hasNextPage endCursor }}\n",
            "        nodes {{ {entry} }}\n",
            "      }}\n",
            "    }}\n",
            "  }}\n",
            "}}"
        ),
        connection = connection,
        entry = entry
    )
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

/// Adds the texts of one entry of `list`, leaving out what `publisher` wrote.
///
/// A thread with more comments than one page, or with a missing comment, marks the read
/// truncated.
fn take_entry(list: List, entry: Node, publisher: &str, read: &mut GuardRead) {
    let own = |author: &Option<Author>| {
        author
            .as_ref()
            .is_some_and(|author| author.login.eq_ignore_ascii_case(publisher))
    };
    match list {
        List::Commits => {
            if let Some(commit) = entry.commit {
                read.push(list.kind(), &commit.message);
            }
        }
        List::Comments | List::Reviews => {
            if !own(&entry.author) {
                read.push(list.kind(), &entry.body);
            }
        }
        List::Threads => {
            let comments = entry.comments.unwrap_or_default();
            if comments.page_info.has_next_page {
                read.cut();
            }
            for comment in comments.nodes {
                match comment {
                    // A comment that GitHub did not return was not scanned.
                    None => read.cut(),
                    Some(comment) if !own(&comment.author) => {
                        read.push(list.kind(), &comment.body);
                    }
                    Some(_) => {}
                }
            }
        }
    }
}
