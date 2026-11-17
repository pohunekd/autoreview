//! The signal read of the engine: the state of the reviews of a pull request and the comments
//! that start its review threads, with their reactions, in one GraphQL query.
//!
//! The page reader takes the captured `gh` output, so each decode is a pure function of its
//! arguments.

// Rust guideline compliant 2026-06-26

use std::future::Future;

use autoreview_engine::config::Credential;
use autoreview_engine::error::Error;
use autoreview_engine::integrations::{CommentReaction, PullRequestSignals, ThreadComment};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use super::{GitHub, repository_variables, response_invalid};

/// Query of one page of the signal read.
///
/// The reviews and the threads page on their own cursors and `@include` drops a connection that
/// ended. A thread's first comment is the inline comment of a finding; its reactions are read
/// up to 100 and a longer list is a partial read.
const GRAPHQL_SIGNALS: &str = concat!(
    "query($owner: String!, $name: String!, $number: Int!, $withReviews: Boolean!, ",
    "$withThreads: Boolean!, $reviewsAfter: String, $threadsAfter: String) {\n",
    "  repository(owner: $owner, name: $name) {\n",
    "    pullRequest(number: $number) {\n",
    "      author { login }\n",
    "      reviews(first: 100, after: $reviewsAfter) @include(if: $withReviews) {\n",
    "        pageInfo { hasNextPage endCursor }\n",
    "        nodes { fullDatabaseId state }\n",
    "      }\n",
    "      reviewThreads(first: 50, after: $threadsAfter) @include(if: $withThreads) {\n",
    "        pageInfo { hasNextPage endCursor }\n",
    "        nodes { comments(first: 1) { nodes { body pullRequestReview { fullDatabaseId }\n",
    "          reactions(first: 100) { pageInfo { hasNextPage } ",
    "nodes { content user { login } } } } } }\n",
    "      }\n",
    "    }\n",
    "  }\n",
    "}",
);

/// A GitHub `BigInt`, which the API sends as text and may send as a number.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum BigId {
    /// The ID as a JSON number.
    Number(i64),
    /// The ID as decimal text.
    Text(String),
}

impl BigId {
    /// Returns the ID, or an invalid-response error for text that is no positive number.
    fn value(&self) -> Result<i64, Error> {
        let id = match self {
            Self::Number(number) => *number,
            Self::Text(text) => text.parse().map_err(|_| response_invalid(None))?,
        };
        if id <= 0 {
            return Err(response_invalid(None));
        }
        Ok(id)
    }
}

/// The pull request node of the signal query.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct SignalPull {
    /// The author, `null` for a deleted account.
    author: Option<Login>,
    /// One page of the reviews; empty when the query left them out.
    reviews: Connection<ReviewNode>,
    /// One page of the review threads; empty when the query left them out.
    review_threads: Connection<ThreadNode>,
}

/// A login of an account.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Login {
    /// The login.
    login: String,
}

/// One page of a connection.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Connection<T> {
    /// Paging of the connection.
    page_info: PageInfo,
    /// The nodes; a `null` entry is skipped.
    nodes: Vec<Option<T>>,
}

/// Paging of a connection.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct PageInfo {
    /// Whether another page follows.
    has_next_page: bool,
    /// Cursor of the next page.
    end_cursor: Option<String>,
}

/// One review.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewNode {
    /// GitHub review ID.
    full_database_id: Option<BigId>,
    /// State of the review.
    state: String,
}

/// One review thread.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ThreadNode {
    /// The first comment of the thread.
    comments: Connection<CommentNode>,
}

/// One inline comment.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommentNode {
    /// Body text of the comment.
    body: String,
    /// The review the comment belongs to, `null` when there is none.
    pull_request_review: Option<ReviewRef>,
    /// Reactions to the comment.
    reactions: Connection<ReactionNode>,
}

/// The review of a comment.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewRef {
    /// GitHub review ID.
    full_database_id: Option<BigId>,
}

/// One reaction.
#[derive(Debug, Default, Deserialize)]
struct ReactionNode {
    /// GraphQL name of the reaction, such as `THUMBS_UP`.
    content: String,
    /// The account that reacted, `null` for a deleted account.
    user: Option<Login>,
}

/// Returns the name that the REST API gives a GraphQL reaction, `None` for an unknown one.
fn reaction_name(content: &str) -> Option<&'static str> {
    let names = [
        ("THUMBS_UP", "+1"),
        ("THUMBS_DOWN", "-1"),
        ("LAUGH", "laugh"),
        ("HOORAY", "hooray"),
        ("CONFUSED", "confused"),
        ("HEART", "heart"),
        ("ROCKET", "rocket"),
        ("EYES", "eyes"),
    ];
    let known = names.iter().find(|(graphql, _)| *graphql == content);
    known.map(|(_, rest)| *rest)
}

/// The cursors of the next page of each connection, `None` for a connection that ended.
///
/// An empty cursor asks for the first page.
#[derive(Debug, PartialEq, Eq)]
struct NextPages {
    /// Cursor of the next page of reviews.
    reviews: Option<String>,
    /// Cursor of the next page of threads.
    threads: Option<String>,
}

impl NextPages {
    /// Returns the cursors of the first page: both connections, from their start.
    fn first() -> Self {
        Self {
            reviews: Some(String::new()),
            threads: Some(String::new()),
        }
    }
}

/// Returns the cursor of the page after `info`, `None` at the last page.
fn next_cursor(info: &PageInfo) -> Result<Option<String>, Error> {
    if !info.has_next_page {
        return Ok(None);
    }
    match &info.end_cursor {
        Some(cursor) if !cursor.is_empty() => Ok(Some(cursor.clone())),
        _ => Err(response_invalid(None)),
    }
}

/// Adds the content of one page of the signal query to `signals`.
///
/// A response that does not decode, carries an error, names no pull request, has a review
/// without an ID, or has a comment with more reactions than one page is refused.
fn absorb_page(signals: &mut PullRequestSignals, output: &str) -> Result<NextPages, Error> {
    let invalid = |error: serde_json::Error| response_invalid(Some(Box::new(error)));
    let response: serde_json::Value = serde_json::from_str(output).map_err(invalid)?;
    let pull = response.pointer("/data/repository/pullRequest");
    let pull = pull.filter(|pull| pull.is_object() && response.get("errors").is_none());
    let pull = pull.ok_or_else(|| response_invalid(None))?;
    let pull = SignalPull::deserialize(pull).map_err(invalid)?;
    signals.author = pull.author.map(|author| author.login).unwrap_or_default();
    for review in pull.reviews.nodes.into_iter().flatten() {
        let id = review.full_database_id.as_ref();
        let id = id.ok_or_else(|| response_invalid(None))?;
        signals.reviews.insert(id.value()?, review.state);
    }
    for thread in pull.review_threads.nodes.into_iter().flatten() {
        let Some(comment) = thread.comments.nodes.into_iter().flatten().next() else {
            continue;
        };
        if comment.reactions.page_info.has_next_page {
            return Err(Error::workflow("github_search_limit_exceeded", true, None));
        }
        // A comment of no review is not one that a review of the engine published.
        let Some(review) = comment.pull_request_review else {
            continue;
        };
        let id = review.full_database_id.as_ref();
        let review_id = id.ok_or_else(|| response_invalid(None))?.value()?;
        let reactions = comment.reactions.nodes.into_iter().flatten();
        let reactions = reactions.filter_map(|reaction| {
            Some(CommentReaction {
                login: reaction.user.map(|user| user.login).unwrap_or_default(),
                content: reaction_name(&reaction.content)?.to_owned(),
            })
        });
        signals.threads.push(ThreadComment {
            review_id,
            body: comment.body,
            reactions: reactions.collect(),
        });
    }
    Ok(NextPages {
        reviews: next_cursor(&pull.reviews.page_info)?,
        threads: next_cursor(&pull.review_threads.page_info)?,
    })
}

/// Returns the `gh` arguments of one page of the signal query.
///
/// A connection that ended is left out of the query; a cursor is sent only when it is not empty.
fn signal_args(repository: &str, number: i64, next: &NextPages) -> Vec<String> {
    let mut args = vec![
        "api".to_owned(),
        "graphql".to_owned(),
        "-f".to_owned(),
        format!("query={GRAPHQL_SIGNALS}"),
    ];
    args.extend(repository_variables(repository));
    args.extend(["-F".to_owned(), format!("number={number}")]);
    let pages = [
        ("withReviews", "reviewsAfter", &next.reviews),
        ("withThreads", "threadsAfter", &next.threads),
    ];
    for (include, after, cursor) in pages {
        args.extend(["-F".to_owned(), format!("{include}={}", cursor.is_some())]);
        if let Some(cursor) = cursor.as_deref().filter(|cursor| !cursor.is_empty()) {
            args.extend(["-f".to_owned(), format!("{after}={cursor}")]);
        }
    }
    args
}

/// Reads the signals of a pull request, fetching each page through `fetch`.
///
/// A connection that ended is not asked again, so the read takes as many pages as the longer
/// connection has. One that still has a page after `search_page_limit` pages is
/// `github_search_limit_exceeded`.
async fn read_signals<F, Fut>(
    mut fetch: F,
    repository: &str,
    number: i64,
    search_page_limit: i64,
) -> Result<PullRequestSignals, Error>
where
    F: FnMut(Vec<String>) -> Fut,
    Fut: Future<Output = Result<String, Error>>,
{
    let mut signals = PullRequestSignals::default();
    let mut next = NextPages::first();
    for _ in 0..search_page_limit {
        let output = fetch(signal_args(repository, number, &next)).await?;
        let page = absorb_page(&mut signals, &output)?;
        next = NextPages {
            reviews: next.reviews.and(page.reviews),
            threads: next.threads.and(page.threads),
        };
        if next.reviews.is_none() && next.threads.is_none() {
            return Ok(signals);
        }
    }
    Err(Error::workflow("github_search_limit_exceeded", true, None))
}

impl GitHub {
    /// Reads the review states and the review threads of a pull request.
    pub(crate) async fn pull_request_signals(
        &self,
        cancel: &CancellationToken,
        credential: &Credential,
        repository: &str,
        number: i64,
    ) -> Result<PullRequestSignals, Error> {
        read_signals(
            |args: Vec<String>| async move {
                self.github::<()>(cancel, credential, &args, None).await
            },
            repository,
            number,
            self.search_page_limit,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use autoreview_engine::error::Error;
    use autoreview_engine::integrations::PullRequestSignals;

    use super::{absorb_page, read_signals};

    /// A captured page: two reviews, one with a numeric ID, and three threads, one of them a
    /// comment of no review and one whose first comment has no reactions.
    const CAPTURED_PAGE: &str = concat!(
        r#"{"data":{"repository":{"pullRequest":{"author":{"login":"dev"},"#,
        r#""reviews":{"pageInfo":{"hasNextPage":false,"endCursor":"r"},"nodes":["#,
        r#"{"fullDatabaseId":"4000000001","state":"DISMISSED"},"#,
        r#"{"fullDatabaseId":12,"state":"COMMENTED"}]},"#,
        r#""reviewThreads":{"pageInfo":{"hasNextPage":true,"endCursor":"t1"},"nodes":["#,
        r#"{"comments":{"nodes":[{"body":"**O001 BLOCKER — Bug**","#,
        r#""pullRequestReview":{"fullDatabaseId":"4000000001"},"reactions":{"#,
        r#""pageInfo":{"hasNextPage":false},"nodes":[{"content":"THUMBS_DOWN","#,
        r#""user":{"login":"dev"}},{"content":"CONFUSED","user":null},"#,
        r#"{"content":"UNKNOWN","user":{"login":"dev"}}]}}]}},"#,
        r#"{"comments":{"nodes":[{"body":"x","pullRequestReview":null,"#,
        r#""reactions":{"pageInfo":{"hasNextPage":false},"nodes":[]}}]}},"#,
        r#"{"comments":{"nodes":[]}},null]}}}}}"#,
    );

    /// A page decodes to its reviews and threads, keeping only the eight known reactions; a
    /// page that is partial or names no pull request is refused.
    #[test]
    fn a_signal_page_decodes_and_a_partial_one_is_refused() {
        let mut signals = PullRequestSignals::default();
        let next = absorb_page(&mut signals, CAPTURED_PAGE).expect("the page decodes");
        assert_eq!((next.reviews, next.threads), (None, Some("t1".to_owned())));
        assert_eq!(signals.author, "dev");
        let states: Vec<_> = (signals.reviews.iter())
            .map(|(id, state)| (*id, state.as_str()))
            .collect();
        assert_eq!(states, [(12, "COMMENTED"), (4_000_000_001, "DISMISSED")]);
        assert_eq!(signals.threads.len(), 1);
        let thread = &signals.threads[0];
        assert_eq!(
            (thread.review_id, thread.body.as_str()),
            (4_000_000_001, "**O001 BLOCKER — Bug**")
        );
        let reactions: Vec<_> = (thread.reactions.iter())
            .map(|reaction| (reaction.login.as_str(), reaction.content.as_str()))
            .collect();
        assert_eq!(reactions, [("dev", "-1"), ("", "confused")]);
        let refused_reactions = CAPTURED_PAGE.replace(
            r#""hasNextPage":false},"nodes":[{"content""#,
            r#""hasNextPage":true},"nodes":[{"content""#,
        );
        let refused = [
            refused_reactions.clone(),
            CAPTURED_PAGE.replace(r#"{"fullDatabaseId":12,"state""#, r#"{"state""#),
            CAPTURED_PAGE.replace(
                r#"{"fullDatabaseId":"4000000001"},"reactions""#,
                r#"{"fullDatabaseId":"0"},"reactions""#,
            ),
            CAPTURED_PAGE.replace(r#""body":"**O001 BLOCKER — Bug**","#, ""),
            r#"{"data":{"repository":{"pullRequest":null}}}"#.to_owned(),
            r#"{"data":null,"errors":[{"message":"x"}]}"#.to_owned(),
            "[]".to_owned(),
        ];
        for output in refused {
            let mut signals = PullRequestSignals::default();
            assert!(absorb_page(&mut signals, &output).is_err(), "{output}");
        }
        // More reactions than one page carries is refused with its own code.
        let mut signals = PullRequestSignals::default();
        let error = absorb_page(&mut signals, &refused_reactions).expect_err("refused");
        assert_eq!(error.to_string(), "github_search_limit_exceeded");
    }

    /// With two pages of reviews and three of threads the read takes three requests, and the
    /// third leaves the reviews out instead of asking for their first page again.
    #[tokio::test]
    async fn a_connection_that_ended_is_left_out_of_the_next_request() {
        let pages = [
            concat!(
                r#"{"data":{"repository":{"pullRequest":{"author":null,"#,
                r#""reviews":{"pageInfo":{"hasNextPage":true,"endCursor":"r1"},"nodes":[]},"#,
                r#""reviewThreads":{"pageInfo":{"hasNextPage":true,"endCursor":"t1"},"nodes":[]}"#,
                r#"}}}}"#,
            ),
            concat!(
                r#"{"data":{"repository":{"pullRequest":{"author":null,"#,
                r#""reviews":{"pageInfo":{"hasNextPage":false,"endCursor":"r2"},"nodes":[]},"#,
                r#""reviewThreads":{"pageInfo":{"hasNextPage":true,"endCursor":"t2"},"nodes":[]}"#,
                r#"}}}}"#,
            ),
            concat!(
                r#"{"data":{"repository":{"pullRequest":{"author":null,"#,
                r#""reviewThreads":{"pageInfo":{"hasNextPage":false,"endCursor":null},"#,
                r#""nodes":[]}}}}}"#,
            ),
        ];
        let seen = RefCell::new(Vec::<Vec<String>>::new());
        let fetch = |args: Vec<String>| {
            let page = pages[seen.borrow().len()];
            seen.borrow_mut().push(args);
            async move { Ok::<_, Error>(page.to_owned()) }
        };
        read_signals(fetch, "acme/widgets", 7, 10)
            .await
            .expect("the read ends");
        let seen = seen.into_inner();
        let has = |request: usize, argument: &str| seen[request].iter().any(|arg| arg == argument);
        assert_eq!(seen.len(), 3);
        assert!(has(0, "withReviews=true") && !has(0, "reviewsAfter=r1"));
        assert!(
            has(1, "withReviews=true") && has(1, "reviewsAfter=r1") && has(1, "threadsAfter=t1")
        );
        assert!(has(2, "withReviews=false") && has(2, "threadsAfter=t2"));
        assert!(!seen[2].iter().any(|arg| arg.starts_with("reviewsAfter=")));
    }
}
