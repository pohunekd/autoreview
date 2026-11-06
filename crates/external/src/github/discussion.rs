//! Discussion reads of the engine: the reply threads of the prior findings, the newest
//! conversation comments and the linear history of a pull request.
//!
//! Mirrors `cmd/autoreview/discussion.go:12-28, 37-50, 93-222`. The page reader takes the
//! captured `gh` output, so each decode is a pure function of its arguments.

// Rust guideline compliant 2026-06-26

use std::collections::{HashMap, HashSet};
use std::future::Future;

use autoreview_engine::discussion::{
    DiscussionBudget, ai_signed, discussion_role, finding_heading_id,
};
use autoreview_engine::error::Error;
use autoreview_engine::model::{
    Discussion, DiscussionComment, DiscussionThread, PriorFinding, PullRequest, prior_key,
    repository_valid, sha_valid,
};
use autoreview_go_compat::null_default;
use serde::{Deserialize, Serialize};

use super::{GitHub, response_invalid};

/// Query of one page of the discussion read (`graphQLDiscussion`).
const GRAPHQL_DISCUSSION: &str = concat!(
    "query($owner: String!, $name: String!, $number: Int!, $after: String) {\n",
    "  repository(owner: $owner, name: $name) {\n",
    "    pullRequest(number: $number) {\n",
    "      reviewThreads(first: 50, after: $after) {\n",
    "        pageInfo { hasNextPage endCursor }\n",
    "        nodes {\n",
    "          isResolved\n",
    "          comments(first: 50) { nodes { author { login __typename } body ",
    "pullRequestReview { databaseId } } }\n",
    "        }\n",
    "      }\n",
    "      comments(last: 50) { nodes { author { login __typename } body } }\n",
    "    }\n",
    "  }\n",
    "}",
);

/// GraphQL type name of an app author, such as a coding agent.
const BOT_TYPENAME: &str = "Bot";

/// The response of one page of the discussion query.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct DiscussionResponse {
    /// The data of the discussion query.
    #[serde(deserialize_with = "null_default::deserialize")]
    data: DiscussionData,
    /// The errors of the response; any entry makes the response invalid.
    #[serde(deserialize_with = "null_default::deserialize")]
    errors: Vec<serde_json::Value>,
}

autoreview_go_compat::object_only_serde!(DiscussionResponse);

/// The data of the discussion query.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self", deny_unknown_fields)]
struct DiscussionData {
    /// The repository, `null` when there is none.
    repository: Option<RepositoryNode>,
}

autoreview_go_compat::object_only_serde!(DiscussionData);

/// The repository node of the discussion query.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self", deny_unknown_fields)]
struct RepositoryNode {
    /// The pull request, `null` when the repository has none with that number.
    #[serde(rename = "pullRequest")]
    pull_request: Option<PullRequestNode>,
}

autoreview_go_compat::object_only_serde!(RepositoryNode);

/// The pull request node of the discussion query.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self", deny_unknown_fields)]
struct PullRequestNode {
    /// The review threads of the page; `null` reads as none.
    #[serde(
        rename = "reviewThreads",
        deserialize_with = "null_default::deserialize"
    )]
    review_threads: ThreadConnection,
    /// The newest conversation comments; `null` reads as none.
    #[serde(deserialize_with = "null_default::deserialize")]
    comments: CommentConnection,
}

autoreview_go_compat::object_only_serde!(PullRequestNode);

/// One page of review threads.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self", deny_unknown_fields)]
struct ThreadConnection {
    /// Paging of the threads; `null` reads as the last page.
    #[serde(rename = "pageInfo", deserialize_with = "null_default::deserialize")]
    page_info: PageInfo,
    /// The threads; a `null` entry is an empty thread.
    #[serde(deserialize_with = "null_default::deserialize")]
    nodes: Vec<Option<ThreadNode>>,
}

autoreview_go_compat::object_only_serde!(ThreadConnection);

/// Paging of a connection.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self", deny_unknown_fields)]
struct PageInfo {
    /// Whether another page follows.
    #[serde(rename = "hasNextPage", deserialize_with = "null_default::deserialize")]
    has_next_page: bool,
    /// Cursor of the next page; empty when there is none.
    #[serde(rename = "endCursor", deserialize_with = "null_default::deserialize")]
    end_cursor: String,
}

autoreview_go_compat::object_only_serde!(PageInfo);

/// One review thread of the discussion query.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self", deny_unknown_fields)]
struct ThreadNode {
    /// Whether the thread is resolved.
    #[serde(rename = "isResolved", deserialize_with = "null_default::deserialize")]
    is_resolved: bool,
    /// The comments of the thread; the first one is the inline comment of a finding.
    #[serde(deserialize_with = "null_default::deserialize")]
    comments: CommentConnection,
}

autoreview_go_compat::object_only_serde!(ThreadNode);

/// A list of comments; a `null` entry reads as a comment with no author.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self", deny_unknown_fields)]
struct CommentConnection {
    /// The comments; `null` reads as none.
    #[serde(deserialize_with = "null_default::deserialize")]
    nodes: Vec<Option<CommentNode>>,
}

autoreview_go_compat::object_only_serde!(CommentConnection);

/// One comment of the discussion query.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self", deny_unknown_fields)]
struct CommentNode {
    /// The author, `null` for a deleted account.
    author: Option<AuthorNode>,
    /// Body of the comment, as GitHub holds it.
    #[serde(deserialize_with = "null_default::deserialize")]
    body: String,
    /// The review of an inline comment, `null` for a conversation comment.
    #[serde(rename = "pullRequestReview")]
    pull_request_review: Option<ReviewNode>,
}

autoreview_go_compat::object_only_serde!(CommentNode);

/// The author of a comment.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self", deny_unknown_fields)]
struct AuthorNode {
    /// Login of the author.
    #[serde(deserialize_with = "null_default::deserialize")]
    login: String,
    /// GraphQL type name, `User` or `Bot`.
    #[serde(rename = "__typename", deserialize_with = "null_default::deserialize")]
    typename: String,
}

autoreview_go_compat::object_only_serde!(AuthorNode);

/// The review that an inline comment belongs to.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self", deny_unknown_fields)]
struct ReviewNode {
    /// Database identifier of the review.
    #[serde(rename = "databaseId", deserialize_with = "null_default::deserialize")]
    database_id: i64,
}

autoreview_go_compat::object_only_serde!(ReviewNode);

/// The comparison of two commits that `gh api` returns.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct Comparison {
    /// Status of the comparison, `ahead`, `behind`, `diverged` or `identical`.
    #[serde(deserialize_with = "null_default::deserialize")]
    status: String,
    /// Number of commits in the comparison, which may exceed the listed commits.
    #[serde(deserialize_with = "null_default::deserialize")]
    total_commits: i64,
    /// The listed commits; a `null` entry is a commit with no parents.
    #[serde(deserialize_with = "null_default::deserialize")]
    commits: Vec<Option<ComparedCommit>>,
}

autoreview_go_compat::object_only_serde!(Comparison);

/// One listed commit of a comparison.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct ComparedCommit {
    /// Parents of the commit, one entry per parent.
    #[serde(deserialize_with = "null_default::deserialize")]
    parents: Vec<serde_json::Value>,
}

autoreview_go_compat::object_only_serde!(ComparedCommit);

/// What one page of the discussion query says about the pages after it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PageStep {
    /// Whether GitHub reports another page.
    has_next_page: bool,
    /// Cursor of the next page; empty when there is none.
    end_cursor: String,
}

/// Collects the discussion across the pages of one read.
///
/// The budget is shared by the threads of every page and then by the conversation, so the
/// replies answer the findings before the newest comments fill what is left.
struct DiscussionReader<'a> {
    /// Pull request whose discussion is read.
    pull_request: &'a PullRequest,
    /// Login of the publishing account.
    publisher: &'a str,
    /// Head SHA of each prior finding, by the identifier of its GitHub review.
    heads: HashMap<i64, String>,
    /// Keys of the prior findings.
    keys: HashSet<String>,
    /// Characters that the comment bodies may still take.
    budget: DiscussionBudget,
    /// Threads that answer prior findings, in page order.
    threads: Vec<DiscussionThread>,
    /// Conversation comments of the first page, in GitHub's order.
    conversation: Vec<Option<CommentNode>>,
}

/// Reports whether `left` and `right` match under Unicode simple case folding, as Go's
/// `strings.EqualFold` reports.
///
/// The bot filter drops comments before the budget counts them, so it must agree with Go on
/// every login, or a later comment would take the budget that Go spends on this one.
fn go_equal_fold(left: &str, right: &str) -> bool {
    let mut left_chars = left.chars();
    let mut right_chars = right.chars();
    loop {
        match (left_chars.next(), right_chars.next()) {
            (None, None) => return true,
            (Some(left_char), Some(right_char)) => {
                if left_char == right_char {
                    continue;
                }
                if left_char.to_lowercase().eq(right_char.to_lowercase()) {
                    continue;
                }
                if left_char.to_uppercase().eq(right_char.to_uppercase()) {
                    continue;
                }
                return false;
            }
            _ => return false,
        }
    }
}

impl<'a> DiscussionReader<'a> {
    /// Creates the reader of one discussion read.
    ///
    /// A limit of zero or less hands out no comment body.
    fn new(
        pull_request: &'a PullRequest,
        publisher: &'a str,
        prior: &[PriorFinding],
        max_characters: i64,
    ) -> Self {
        let mut heads = HashMap::new();
        let mut keys = HashSet::new();
        for finding in prior {
            heads.insert(finding.github_review_id, finding.head_sha.clone());
            keys.insert(finding.key.clone());
        }
        Self {
            pull_request,
            publisher,
            heads,
            keys,
            budget: DiscussionBudget::new(max_characters),
            threads: Vec::new(),
            conversation: Vec::new(),
        }
    }

    /// Reads one page of the discussion query and returns its paging.
    ///
    /// The first page also sets the conversation. A response that does not decode, carries an
    /// error or names no pull request is `github_response_invalid`.
    fn absorb_page(&mut self, page: i64, output: &str) -> Result<PageStep, Error> {
        let document = serde_json::from_str::<Option<DiscussionResponse>>(output)
            .map_err(|error| response_invalid(Some(Box::new(error))))?;
        let Some(response) = document else {
            return Err(response_invalid(None));
        };
        if !response.errors.is_empty() {
            return Err(response_invalid(None));
        }
        let Some(repository) = response.data.repository else {
            return Err(response_invalid(None));
        };
        let Some(pull) = repository.pull_request else {
            return Err(response_invalid(None));
        };
        if page == 1 {
            self.conversation = pull.comments.nodes;
        }
        for node in &pull.review_threads.nodes {
            let Some(thread) = node else {
                continue;
            };
            self.absorb_thread(thread);
        }
        Ok(PageStep {
            has_next_page: pull.review_threads.page_info.has_next_page,
            end_cursor: pull.review_threads.page_info.end_cursor.clone(),
        })
    }

    /// Keeps the thread when its first comment is the inline comment of a prior finding.
    ///
    /// The heading is searched in the whole body, because earlier comments led with the AI
    /// signature. Only replies of that thread take the budget.
    fn absorb_thread(&mut self, thread: &ThreadNode) {
        let Some(Some(first)) = thread.comments.nodes.first() else {
            return;
        };
        let Some(review) = &first.pull_request_review else {
            return;
        };
        let Some(identifier) = finding_heading_id(&first.body) else {
            return;
        };
        let Some(head) = self.heads.get(&review.database_id) else {
            return;
        };
        let key = prior_key(head, identifier);
        if !self.keys.contains(&key) {
            return;
        }
        let mut replies = Vec::new();
        for node in thread.comments.nodes.iter().skip(1) {
            if let Some(comment) = self.comment(node.as_ref()) {
                replies.push(comment);
            }
        }
        self.threads.push(DiscussionThread {
            key,
            resolved: thread.is_resolved,
            replies,
        });
    }

    /// Converts one comment for the reviewers, or returns `None` when it is left out.
    ///
    /// A comment without an author, or by an app other than the pull request author, is left
    /// out. The AI signature is read from the whole body, before the budget cuts it.
    fn comment(&mut self, source: Option<&CommentNode>) -> Option<DiscussionComment> {
        let node = source?;
        let author = node.author.as_ref()?;
        if author.typename == BOT_TYPENAME
            && !go_equal_fold(&author.login, &self.pull_request.author)
        {
            return None;
        }
        let body = self.budget.take(&node.body)?;
        Some(DiscussionComment {
            author: author.login.clone(),
            role: discussion_role(&author.login, self.pull_request, self.publisher).to_owned(),
            ai_signed: ai_signed(&node.body),
            body,
        })
    }

    /// Returns the discussion, with the newest conversation comments in GitHub's order.
    fn finish(mut self) -> Discussion {
        let conversation = std::mem::take(&mut self.conversation);
        let mut comments = Vec::new();
        for node in conversation.iter().rev() {
            if let Some(comment) = self.comment(node.as_ref()) {
                comments.push(comment);
            }
        }
        comments.reverse();
        Discussion {
            threads: self.threads,
            comments,
        }
    }
}

/// Returns the `gh` arguments of one page of the discussion query, from `after` on.
///
/// The cursor is sent only when it is not empty, as in Go.
fn discussion_args(pull_request: &PullRequest, after: &str) -> Vec<String> {
    let (owner, name) = match pull_request.repository.split_once('/') {
        Some((owner, name)) => (owner, name),
        None => (pull_request.repository.as_str(), ""),
    };
    let mut args = vec![
        "api".to_owned(),
        "graphql".to_owned(),
        "-f".to_owned(),
        format!("query={GRAPHQL_DISCUSSION}"),
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

/// Reads the discussion of `pull_request`, fetching each page through `fetch`.
///
/// `fetch` receives the `gh` arguments of one page and returns its output. Paging stops after
/// the page that the search limit names, as in Go, and a limit of zero or less never stops it.
async fn read_discussion<F, Fut>(
    mut fetch: F,
    pull_request: &PullRequest,
    prior: &[PriorFinding],
    max_characters: i64,
    publisher: &str,
    search_page_limit: i64,
) -> Result<Discussion, Error>
where
    F: FnMut(Vec<String>) -> Fut,
    Fut: Future<Output = Result<String, Error>>,
{
    let mut reader = DiscussionReader::new(pull_request, publisher, prior, max_characters);
    let mut after = String::new();
    let mut page: i64 = 1;
    loop {
        let output = fetch(discussion_args(pull_request, &after)).await?;
        let step = reader.absorb_page(page, &output)?;
        if !step.has_next_page {
            break;
        }
        if page == search_page_limit {
            return Err(Error::workflow("github_search_limit_exceeded", true, None));
        }
        after = step.end_cursor;
        page += 1;
    }
    Ok(reader.finish())
}

/// Decodes the comparison that `gh api` returned and reports whether it is linear.
///
/// The comparison is linear when it is `ahead`, lists every commit it counts, and each listed
/// commit has exactly one parent. GitHub lists at most 250 commits, so a short list proves
/// nothing. A `null` document is the zero comparison, which is not `ahead`.
fn decode_linear_history(output: &str) -> Result<bool, Error> {
    let document = serde_json::from_str::<Option<Comparison>>(output)
        .map_err(|error| response_invalid(Some(Box::new(error))))?;
    let Some(comparison) = document else {
        return Ok(false);
    };
    if comparison.status != "ahead" {
        return Ok(false);
    }
    let Ok(length) = usize::try_from(comparison.total_commits) else {
        return Ok(false);
    };
    if length != comparison.commits.len() {
        return Ok(false);
    }
    for entry in &comparison.commits {
        let Some(commit) = entry else {
            return Ok(false);
        };
        if commit.parents.len() != 1 {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Validates the identity of a comparison, reads it through `fetch` and reports whether it is
/// linear.
async fn read_linear_history<F, Fut>(
    mut fetch: F,
    repository: &str,
    previous: &str,
    head: &str,
) -> Result<bool, Error>
where
    F: FnMut(Vec<String>) -> Fut,
    Fut: Future<Output = Result<String, Error>>,
{
    if !repository_valid(repository) || !sha_valid(previous) || !sha_valid(head) {
        return Err(Error::workflow("review_identity_invalid", true, None));
    }
    let args = vec![
        "api".to_owned(),
        format!("repos/{repository}/compare/{previous}...{head}"),
    ];
    let output = fetch(args).await?;
    decode_linear_history(&output)
}

impl GitHub {
    /// Reads the reply threads of the prior findings and the newest conversation comments.
    ///
    /// Comment bodies share `max_characters` characters, and the threads take them first.
    pub(crate) async fn list_discussion(
        &self,
        pull_request: &PullRequest,
        prior: &[PriorFinding],
        max_characters: i64,
    ) -> Result<Discussion, Error> {
        read_discussion(
            |args: Vec<String>| async move { self.github::<()>(&args, None).await },
            pull_request,
            prior,
            max_characters,
            &self.login,
            self.search_page_limit,
        )
        .await
    }

    /// Reports whether `head` extends `previous` by commits that each have one parent.
    ///
    /// A rebase, a force push or a merge of the base branch makes the diff between the two
    /// heads something other than the author's own change.
    pub(crate) async fn linear_history(
        &self,
        repository: &str,
        previous: &str,
        head: &str,
    ) -> Result<bool, Error> {
        read_linear_history(
            |args: Vec<String>| async move { self.github::<()>(&args, None).await },
            repository,
            previous,
            head,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    use autoreview_engine::error::Error;
    use autoreview_engine::model::{PriorFinding, PullRequest, prior_key};

    use super::{decode_linear_history, read_discussion, read_linear_history};

    /// Page of the discussion query that the Go test `TestDiscussionMatchesThreads...` sends.
    const CAPTURED_PAGE: &str = concat!(
        r#"{"data":{"repository":{"pullRequest":{"comments":{"nodes":[{"author":{"__typenam"#,
        r#"e":"User","login":"reviewer"},"body":"Ignore my comment about deleted projects.""#,
        r#"},{"author":{"__typename":"User","login":"someone"},"body":"A very long newest c"#,
        r#"omment."}]},"reviewThreads":{"nodes":[{"comments":{"nodes":[{"author":{"__typena"#,
        r#"me":"User","login":"reviewer"},"body":"**AI review**: machine-written.\n\n**O001"#,
        r#" HIGH — Lock name**\n\nDetails.","pullRequestReview":{"databaseId":11}},{"author"#,
        r#"":{"__typename":"User","login":"colleague"},"body":"Waived, the test builds the "#,
        r#"name.\n\n🤖 Written by AI: nanny"},{"author":{"__typename":"User","login":"review"#,
        r#"er"},"body":"Agreed, waive it."}]},"isResolved":false},{"comments":{"nodes":[{"a"#,
        r#"uthor":{"__typename":"User","login":"reviewer"},"body":"**S002 MEDIUM — Fake pro"#,
        r#"ves fake**\n\nDetails.\n\n🤖 Written by AI: test reviewer","pullRequestReview":{""#,
        r#"databaseId":11}},{"author":{"__typename":"Bot","login":"ci"},"body":"Bot noise.""#,
        r#"},{"author":{"__typename":"Bot","login":"colleague"},"body":"Fixed in the next c"#,
        r#"ommit."}]},"isResolved":true},{"comments":{"nodes":[{"author":{"__typename":"Use"#,
        r#"r","login":"reviewer"},"body":"**O001 HIGH — Other review**","pullRequestReview""#,
        r#":{"databaseId":99}}]},"isResolved":false},{"comments":{"nodes":[{"author":{"__ty"#,
        r#"pename":"User","login":"reviewer"},"body":"**O009 HIGH — Not prior**","pullReque"#,
        r#"stReview":{"databaseId":11}}]},"isResolved":false}],"pageInfo":{"endCursor":"",""#,
        r#"hasNextPage":false}}}}}}"#,
    );

    /// Comparison of two commits in which the head adds one commit with one parent.
    const CAPTURED_OWN: &str =
        r#"{"commits":[{"parents":[{"sha":"x"}]}],"status":"ahead","total_commits":1}"#;

    /// Comparison in which the head adds a merge commit.
    const CAPTURED_MERGE: &str =
        r#"{"commits":[{"parents":[{"sha":"x"},{"sha":"y"}]}],"status":"ahead","total_commits":1}"#;

    /// Comparison that is diverged rather than ahead.
    const CAPTURED_REBASE: &str =
        r#"{"commits":[{"parents":[{"sha":"x"}]}],"status":"diverged","total_commits":1}"#;

    /// Comparison that lists fewer commits than it counts.
    const CAPTURED_INCOMPLETE: &str =
        r#"{"commits":[{"parents":[{"sha":"x"}]}],"status":"ahead","total_commits":300}"#;

    /// Runs a future whose fetches complete on their first poll.
    fn block_on<F: Future>(future: F) -> F::Output {
        let mut future = pin!(future);
        let mut context = Context::from_waker(Waker::noop());
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("a captured fetch completes on its first poll"),
        }
    }

    #[test]
    fn discussion_matches_threads_by_review_and_finding_in_both_comment_layouts() {
        let head = "c".repeat(40);
        let prior = vec![
            PriorFinding {
                key: prior_key(&head, "O001"),
                id: "O001".to_owned(),
                head_sha: head.clone(),
                github_review_id: 11,
                ..PriorFinding::default()
            },
            PriorFinding {
                key: prior_key(&head, "S002"),
                id: "S002".to_owned(),
                head_sha: head.clone(),
                github_review_id: 11,
                ..PriorFinding::default()
            },
        ];
        let pull_request = PullRequest {
            repository: "acme/widget".to_owned(),
            number: 5,
            author: "colleague".to_owned(),
            ..PullRequest::default()
        };
        let fetch =
            |_args: Vec<String>| std::future::ready(Ok::<String, Error>(CAPTURED_PAGE.to_owned()));
        let discussion = block_on(read_discussion(
            fetch,
            &pull_request,
            &prior,
            140,
            "reviewer",
            5,
        ))
        .expect("the captured page reads");

        assert_eq!(discussion.threads.len(), 2);
        assert_eq!(discussion.threads[0].key, prior[0].key);
        assert_eq!(discussion.threads[1].key, prior[1].key);
        assert!(discussion.threads[1].resolved);
        let replies = &discussion.threads[0].replies;
        assert_eq!(replies.len(), 2);
        assert_eq!(replies[0].role, "author");
        assert!(replies[0].ai_signed);
        assert_eq!(replies[1].role, "reviewer");
        assert!(!replies[1].ai_signed);
        let bot = &discussion.threads[1].replies;
        assert_eq!(bot.len(), 1);
        assert_eq!(bot[0].author, "colleague");
        assert_eq!(bot[0].role, "author");

        let total: usize = discussion
            .threads
            .iter()
            .flat_map(|thread| thread.replies.iter())
            .chain(discussion.comments.iter())
            .map(|comment| comment.body.chars().count())
            .sum();
        assert!(total <= 140, "the budget is 140 characters, got {total}");
        assert!(!discussion.comments.is_empty());
        assert_eq!(
            discussion
                .comments
                .last()
                .map(|comment| comment.author.as_str()),
            Some("someone")
        );
    }

    #[test]
    fn linear_history_needs_an_ahead_head_of_single_parent_commits() {
        let previous = "a".repeat(40);
        let head = "c".repeat(40);
        let cases = [
            (CAPTURED_OWN, true),
            (CAPTURED_MERGE, false),
            (CAPTURED_REBASE, false),
            (CAPTURED_INCOMPLETE, false),
        ];
        for (output, want) in cases {
            let fetch =
                |_args: Vec<String>| std::future::ready(Ok::<String, Error>(output.to_owned()));
            let linear = block_on(read_linear_history(fetch, "acme/widget", &previous, &head))
                .expect("the captured comparison reads");
            assert_eq!(linear, want, "{output}");
        }
        assert_eq!(decode_linear_history("null").ok(), Some(false));
    }
}
