//! The read of the pull request texts that a rule's text guard scans.
//!
//! Only the fields the guard names are read, in field order, so the byte bound cuts the later
//! kinds first. Every list is paged until it ends, the bound is reached or the page limit stops
//! it; text left out sets `truncated`.

// Rust guideline compliant 2026-11-15

use std::collections::BTreeSet;

use autoreview_engine::config::Credential;
use autoreview_engine::error::Error;
use autoreview_engine::integrations::{GuardText, GuardTexts};
use autoreview_engine::model::PullRequest;
use autoreview_go_compat::null_default;
use autoreview_guard::FieldKind;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use super::{GitHub, repository_variables, response_invalid};

/// Query of the title, the body and the changed-file count of one pull request.
const GRAPHQL_GUARD_TEXT: &str = concat!(
    "query($owner: String!, $name: String!, $number: Int!) {\n",
    "  repository(owner: $owner, name: $name) {\n",
    "    pullRequest(number: $number) { title body changedFiles }\n",
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
    /// The number of changed files, read by the title query only.
    #[serde(rename = "changedFiles")]
    changed_files: Option<u64>,
    /// One page of the list the query names.
    page: Option<Page>,
}

/// One page of a list, or the comments of one review thread.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Page {
    /// Paging of the list; a missing one makes the response invalid.
    #[serde(rename = "pageInfo")]
    page_info: Option<PageInfo>,
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
    /// The commit of a commit entry; a missing one makes the response invalid.
    commit: Option<Commit>,
    /// The author; `null` for a deleted account.
    author: Option<Author>,
    /// The text of a comment or a review; `null` reads as empty.
    #[serde(deserialize_with = "null_default::deserialize")]
    body: String,
    /// The comments of a review thread; missing ones make the response invalid.
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
    pub(super) fn full(&self) -> bool {
        self.bytes >= self.max_bytes
    }

    /// Reports whether more text cannot change the read.
    ///
    /// It holds once the bound is reached and text was left out.
    pub(super) fn settled(&self) -> bool {
        self.truncated && self.full()
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
    let mut args = vec![
        "api".to_owned(),
        "graphql".to_owned(),
        "-f".to_owned(),
        format!("query={query}"),
    ];
    args.extend(repository_variables(&pull_request.repository));
    args.extend(["-F".to_owned(), format!("number={}", pull_request.number)]);
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

/// Adds the texts of one entry of `list`, leaving out what the publisher `login` wrote.
///
/// The publisher's review-thread comments are left out too. A thread with more comments than
/// one page, or with a missing comment, marks the read truncated. An entry without its commit,
/// comments or their paging is `github_response_invalid`.
fn take_entry(list: List, entry: Node, login: &str, read: &mut GuardRead) -> Result<(), Error> {
    let own = |author: &Option<Author>| {
        author
            .as_ref()
            .is_some_and(|author| author.login.eq_ignore_ascii_case(login))
    };
    match list {
        List::Commits => {
            let commit = entry.commit.ok_or_else(|| response_invalid(None))?;
            read.push(list.kind(), &commit.message);
        }
        List::Comments | List::Reviews => {
            if !own(&entry.author) {
                read.push(list.kind(), &entry.body);
            }
        }
        List::Threads => {
            let comments = entry.comments.ok_or_else(|| response_invalid(None))?;
            let page_info = (comments.page_info).ok_or_else(|| response_invalid(None))?;
            if page_info.has_next_page {
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
    Ok(())
}

impl GitHub {
    /// Reads the texts of `fields` for the text guard, bounded by `max_bytes`.
    ///
    /// Each list is paged until it ends or the bound stops it. The page limit, the bound, a
    /// changed file with added lines whose patch GitHub omits and a changed-files listing that
    /// left files out mark the read truncated.
    pub(crate) async fn guard_texts(
        &self,
        cancel: &CancellationToken,
        credential: &Credential,
        pull_request: &PullRequest,
        fields: &BTreeSet<FieldKind>,
        max_bytes: usize,
        publisher: &str,
    ) -> Result<GuardTexts, Error> {
        let mut read = GuardRead {
            max_bytes,
            ..GuardRead::default()
        };
        let (title, body, patch) = (FieldKind::Title, FieldKind::Body, FieldKind::Patch);
        let mut changed_files: usize = 0;
        if [title, body, patch]
            .iter()
            .any(|kind| fields.contains(kind))
        {
            let args = guard_args(pull_request, GRAPHQL_GUARD_TEXT, "");
            let output = self.github::<()>(cancel, credential, &args, None).await?;
            let node = decode_pull_node(&output)?;
            changed_files = (node.changed_files)
                .and_then(|count| usize::try_from(count).ok())
                .ok_or_else(|| response_invalid(None))?;
            if fields.contains(&title) {
                read.push(title, &node.title);
            }
            if fields.contains(&body) {
                read.push(body, &node.body);
            }
        }
        for list in LISTS
            .into_iter()
            .filter(|list| fields.contains(&list.kind()))
        {
            self.read_list(cancel, credential, pull_request, list, publisher, &mut read)
                .await?;
        }
        if fields.contains(&patch) && !read.settled() {
            self.guard_patches(cancel, credential, pull_request, changed_files, &mut read)
                .await?;
        }
        Ok(GuardTexts {
            texts: read.texts,
            truncated: read.truncated,
        })
    }

    /// Reads the pages of `list` into `read` until it ends, the read settles or the page limit.
    ///
    /// Past the bound one page is still read, so a list without text does not set `truncated`;
    /// a further page then cuts the read.
    async fn read_list(
        &self,
        cancel: &CancellationToken,
        credential: &Credential,
        pull_request: &PullRequest,
        list: List,
        publisher: &str,
        read: &mut GuardRead,
    ) -> Result<(), Error> {
        let query = list_query(list);
        let mut after = String::new();
        for _ in 1..=self.search_page_limit {
            if read.settled() {
                return Ok(());
            }
            let args = guard_args(pull_request, &query, &after);
            let output = self.github::<()>(cancel, credential, &args, None).await?;
            let page = decode_pull_node(&output)?
                .page
                .ok_or_else(|| response_invalid(None))?;
            let page_info = page.page_info.ok_or_else(|| response_invalid(None))?;
            for entry in page.nodes {
                let entry = entry.ok_or_else(|| response_invalid(None))?;
                take_entry(list, entry, publisher, read)?;
            }
            if !page_info.has_next_page {
                return Ok(());
            }
            if read.full() {
                read.cut();
                return Ok(());
            }
            if page_info.end_cursor.is_empty() {
                return Err(response_invalid(None));
            }
            after = page_info.end_cursor;
        }
        read.cut();
        Ok(())
    }
}
