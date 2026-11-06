//! Pull request reads of the engine: the GraphQL pull request and the review request events.
//!
//! Mirrors `cmd/autoreview/github.go:88-322`. The decode functions take the `gh` output as it
//! was captured, so each one is a pure function of its arguments.

// Rust guideline compliant 2026-06-26

use autoreview_engine::error::Error;
use autoreview_engine::integrations::PullRequestState;
use autoreview_engine::model::{PullRequest, repository_valid, sha_valid};
use autoreview_go_compat::{go_time, null_default, null_entries};
use autoreview_policy::{Trigger, TriggerKind};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::{GitHub, graphql_pull_request, response_invalid};

/// Largest title in bytes that a pull request may have.
const MAX_TITLE_BYTES: usize = 1024;

/// Largest branch name in bytes.
const MAX_BRANCH_BYTES: usize = 255;

/// Largest author login in bytes, before the `[bot]` suffix.
const MAX_AUTHOR_BYTES: usize = 39;

/// Page size of the review events API.
const EVENTS_PER_PAGE: usize = 100;

/// Unix nanoseconds of Go's zero `time.Time`, 0001-01-01T00:00:00Z.
///
/// Go reads an absent time as this value, and a review request event with it is refused.
const GO_ZERO_TIME_NANOS: i128 = -62_135_596_800_000_000_000;

/// The author type that marks an app, such as a coding agent.
const BOT_TYPENAME: &str = "Bot";

/// The pull request state that the engine reviews.
const OPEN_STATE: &str = "OPEN";

/// The suffix that GitHub adds to the login of an app author.
const BOT_SUFFIX: &str = "[bot]";

/// The repository that the query names, `null` when there is none.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct GraphQlData {
    /// The repository that the query names, `null` when there is none.
    repository: Option<RepositoryNode>,
}

autoreview_go_compat::object_only_serde!(GraphQlData);

/// The repository node of the response.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct RepositoryNode {
    /// The pull request, `null` when the repository has none with that number.
    #[serde(rename = "pullRequest")]
    pull_request: Option<GitHubPullRequest>,
}

autoreview_go_compat::object_only_serde!(RepositoryNode);

/// The data of the response.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct GraphQlResponse {
    /// The data of the response.
    #[serde(deserialize_with = "null_default::deserialize")]
    data: GraphQlData,
    /// The errors of the response; any entry makes the response invalid.
    #[serde(deserialize_with = "null_default::deserialize")]
    errors: Vec<serde_json::Value>,
}

autoreview_go_compat::object_only_serde!(GraphQlResponse);

/// The pull request as GitHub returns it.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
pub(crate) struct GitHubPullRequest {
    /// Node identifier, decoded so that a value of another type is refused as Go refuses it.
    #[serde(deserialize_with = "null_default::deserialize")]
    id: String,
    /// Pull request number.
    #[serde(deserialize_with = "null_default::deserialize")]
    number: i64,
    /// Pull request title, before trimming.
    #[serde(deserialize_with = "null_default::deserialize")]
    title: String,
    /// Pull request URL.
    #[serde(deserialize_with = "null_default::deserialize")]
    url: String,
    /// Pull request state, `OPEN`, `CLOSED` or `MERGED`.
    #[serde(deserialize_with = "null_default::deserialize")]
    state: String,
    /// Whether the pull request is a draft.
    #[serde(rename = "isDraft", deserialize_with = "null_default::deserialize")]
    is_draft: bool,
    /// Head commit SHA.
    #[serde(rename = "headRefOid", deserialize_with = "null_default::deserialize")]
    head_sha: String,
    /// Head branch name.
    #[serde(rename = "headRefName", deserialize_with = "null_default::deserialize")]
    head_branch: String,
    /// Base commit SHA.
    #[serde(rename = "baseRefOid", deserialize_with = "null_default::deserialize")]
    base_sha: String,
    /// Base branch name.
    #[serde(rename = "baseRefName", deserialize_with = "null_default::deserialize")]
    base_branch: String,
    /// The author, `null` for a deleted account.
    author: Option<Author>,
    /// The labels of the pull request.
    #[serde(deserialize_with = "null_default::deserialize")]
    labels: Labels,
    /// The review requests of the pull request.
    #[serde(
        rename = "reviewRequests",
        deserialize_with = "null_default::deserialize"
    )]
    review_requests: ReviewRequests,
    /// The repository of the pull request.
    #[serde(deserialize_with = "null_default::deserialize")]
    repository: RepositoryName,
}

autoreview_go_compat::object_only_serde!(GitHubPullRequest);

impl GitHubPullRequest {
    /// Returns the lower-case repository and the number of the pull request as the search
    /// names them.
    pub(super) fn repository_and_number(&self) -> (String, i64) {
        (
            self.repository.name_with_owner.to_ascii_lowercase(),
            self.number,
        )
    }
}

/// The author of a pull request.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct Author {
    /// GraphQL type name, `User` or `Bot`.
    #[serde(rename = "__typename", deserialize_with = "null_default::deserialize")]
    typename: String,
    /// Login of the author.
    #[serde(deserialize_with = "null_default::deserialize")]
    login: String,
}

autoreview_go_compat::object_only_serde!(Author);

/// The labels of a pull request.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct Labels {
    /// The label nodes; `null` reads as none and a `null` entry as an empty label.
    #[serde(deserialize_with = "null_entries::deserialize")]
    nodes: Vec<LabelNode>,
}

autoreview_go_compat::object_only_serde!(Labels);

/// One label of a pull request.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct LabelNode {
    /// Name of the label.
    #[serde(deserialize_with = "null_default::deserialize")]
    name: String,
}

autoreview_go_compat::object_only_serde!(LabelNode);

/// The review requests of a pull request.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct ReviewRequests {
    /// The request nodes; `null` reads as none and a `null` entry as an empty request.
    #[serde(deserialize_with = "null_entries::deserialize")]
    nodes: Vec<ReviewRequestNode>,
}

autoreview_go_compat::object_only_serde!(ReviewRequests);

/// One review request of a pull request.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct ReviewRequestNode {
    /// The requested reviewer, `null` for a request to a team.
    #[serde(rename = "requestedReviewer")]
    requested_reviewer: Option<Reviewer>,
}

autoreview_go_compat::object_only_serde!(ReviewRequestNode);

/// A user that a review was requested from.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct Reviewer {
    /// Login of the reviewer.
    #[serde(deserialize_with = "null_default::deserialize")]
    login: String,
}

autoreview_go_compat::object_only_serde!(Reviewer);

/// The repository of a pull request.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct RepositoryName {
    /// Repository as `owner/name`, in the case GitHub returns.
    #[serde(
        rename = "nameWithOwner",
        deserialize_with = "null_default::deserialize"
    )]
    name_with_owner: String,
}

autoreview_go_compat::object_only_serde!(RepositoryName);

/// One entry of the review events API, with the fields that the engine decodes.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct ReviewEvent {
    /// Event identifier.
    #[serde(deserialize_with = "null_default::deserialize")]
    id: i64,
    /// Kind of the event, such as `review_requested`.
    #[serde(deserialize_with = "null_default::deserialize")]
    event: String,
    /// Creation time as RFC 3339 text; `null` reads as the zero time.
    created_at: Option<String>,
    /// The reviewer that the event requested, for a review request.
    requested_reviewer: Option<Reviewer>,
}

autoreview_go_compat::object_only_serde!(ReviewEvent);

/// A `review_requested` event of the configured login.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReviewRequestEvent {
    /// Event identifier, which is positive.
    pub(crate) id: i64,
    /// Creation time in Unix nanoseconds.
    pub(crate) created_at_nanos: i128,
}

/// The review request events of one page of the events API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReviewRequestPage {
    /// The events of the page that request a review from the configured login.
    pub(crate) events: Vec<ReviewRequestEvent>,
    /// The number of events on the page, of every kind.
    pub(crate) count: usize,
}

/// Decodes one GraphQL response of a pull request query.
///
/// A response with a GraphQL error, or a missing repository or pull request, is refused as Go
/// refuses it; a missing pull request is `pull_request_not_open`.
pub(crate) fn decode_pull_request_response(output: &str) -> Result<GitHubPullRequest, Error> {
    let response: GraphQlResponse =
        serde_json::from_str(output).map_err(|error| response_invalid(Some(Box::new(error))))?;
    if !response.errors.is_empty() {
        return Err(response_invalid(None));
    }
    let repository = response
        .data
        .repository
        .ok_or_else(|| Error::workflow("pull_request_not_open", true, None))?;
    let pull_request = repository
        .pull_request
        .ok_or_else(|| Error::workflow("pull_request_not_open", true, None))?;
    Ok(pull_request)
}

/// Returns the node identifier of a draft that `login` is asked to review and that `author`
/// opened, or `None` when the pull request is not such a draft (the `ReadyDrafts` check).
///
/// The check compares the author, repository and number as ASCII case-insensitive text.
pub(super) fn ready_draft_id(
    raw: &GitHubPullRequest,
    author: &str,
    repository: &str,
    number: i64,
    login: &str,
) -> Option<String> {
    let eligible = raw.state == OPEN_STATE
        && raw.is_draft
        && raw
            .author
            .as_ref()
            .is_some_and(|raw_author| raw_author.login.eq_ignore_ascii_case(author))
        && raw
            .repository
            .name_with_owner
            .eq_ignore_ascii_case(repository)
        && raw.number == number
        && requests(raw, login)
        && !raw.id.is_empty();
    eligible.then(|| raw.id.clone())
}

/// Builds the argv of the GraphQL pull request query for `repository` and `number`.
///
/// The identity is checked before any call, as Go checks it at `github.go:284-286`.
pub(crate) fn pull_request_args(repository: &str, number: i64) -> Result<Vec<String>, Error> {
    let Some((owner, name)) = repository.split_once('/') else {
        return Err(Error::workflow("review_identity_invalid", true, None));
    };
    if !repository_valid(repository) || number <= 0 {
        return Err(Error::workflow("review_identity_invalid", true, None));
    }
    Ok(vec![
        "api".to_owned(),
        "graphql".to_owned(),
        "-f".to_owned(),
        format!("query={}", graphql_pull_request()),
        "-F".to_owned(),
        format!("owner={owner}"),
        "-F".to_owned(),
        format!("name={name}"),
        "-F".to_owned(),
        format!("number={number}"),
    ])
}

/// Reports whether the pull request asks `login` for a review, ignoring ASCII case.
pub(super) fn requests(raw: &GitHubPullRequest, login: &str) -> bool {
    for node in &raw.review_requests.nodes {
        if let Some(reviewer) = &node.requested_reviewer
            && reviewer.login.eq_ignore_ascii_case(login)
        {
            return true;
        }
    }
    false
}

/// Reports whether the pull request carries `label`, ignoring ASCII case.
fn has_label(raw: &GitHubPullRequest, label: &str) -> bool {
    for node in &raw.labels.nodes {
        if node.name.eq_ignore_ascii_case(label) {
            return true;
        }
    }
    false
}

/// Reports whether `url` is the URL of the pull request `repository#number` on `host`.
pub(super) fn url_matches(url: &str, host: &str, repository: &str, number: i64) -> bool {
    url.eq_ignore_ascii_case(&format!("https://{host}/{repository}/pull/{number}"))
}

/// Reports whether an author login passes Go's `authorPattern`.
///
/// An empty login is a deleted account and passes. Otherwise the login is one ASCII letter or
/// digit, then up to 38 letters, digits or hyphens, with an optional `[bot]` suffix.
fn author_valid(author: &str) -> bool {
    if author.is_empty() {
        return true;
    }
    let login = author.strip_suffix(BOT_SUFFIX).unwrap_or(author);
    if login.is_empty() || login.len() > MAX_AUTHOR_BYTES {
        return false;
    }
    let bytes = login.as_bytes();
    let Some(first) = bytes.first() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    for byte in bytes.iter().skip(1) {
        if !byte.is_ascii_alphanumeric() && *byte != b'-' {
            return false;
        }
    }
    true
}

/// Reports whether a branch name passes Go's `branchPattern`.
fn branch_valid(branch: &str) -> bool {
    if branch.is_empty() || branch.len() > MAX_BRANCH_BYTES {
        return false;
    }
    for byte in branch.as_bytes() {
        let valid = byte.is_ascii_alphanumeric() || matches!(*byte, b'.' | b'_' | b'/' | b'-');
        if !valid {
            return false;
        }
    }
    true
}

/// Decodes and checks a pull request against the trigger of its rule (`decodePullRequest`).
///
/// The result has `review_request_id` 0 for a `review-requested` trigger; the caller reads the
/// request id after this check, as Go does.
pub(crate) fn decode_pull_request(
    raw: &GitHubPullRequest,
    host: &str,
    login: &str,
    trigger: &Trigger,
) -> Result<PullRequest, Error> {
    if raw.state != OPEN_STATE {
        return Err(Error::workflow("pull_request_not_open", true, None));
    }
    let repository = raw.repository.name_with_owner.to_ascii_lowercase();
    let (author, author_bot) = if let Some(author) = &raw.author {
        (
            author.login.to_ascii_lowercase(),
            author.typename == BOT_TYPENAME,
        )
    } else {
        (String::new(), false)
    };
    let title = raw.title.trim().to_string();
    let labels = raw
        .labels
        .nodes
        .iter()
        .map(|node| node.name.clone())
        .collect();
    let pull_request = PullRequest {
        repository: repository.clone(),
        number: raw.number,
        head_sha: raw.head_sha.clone(),
        review_request_id: 0,
        title: title.clone(),
        url: raw.url.clone(),
        author: author.clone(),
        author_bot,
        head_branch: raw.head_branch.clone(),
        base_branch: raw.base_branch.clone(),
        base_sha: raw.base_sha.clone(),
        labels,
    };
    if !repository_valid(&repository)
        || pull_request.number <= 0
        || title.is_empty()
        || title.len() > MAX_TITLE_BYTES
        || !url_matches(&pull_request.url, host, &repository, pull_request.number)
        || !sha_valid(&pull_request.head_sha)
        || !sha_valid(&pull_request.base_sha)
        || !branch_valid(&pull_request.base_branch)
        || pull_request.base_branch.contains("..")
        || !author_valid(&author)
    {
        return Err(response_invalid(None));
    }
    if raw.is_draft {
        return Err(Error::workflow("pull_request_draft", true, None));
    }
    match trigger.kind {
        TriggerKind::Label => {
            if !has_label(raw, &trigger.label) {
                return Err(Error::workflow("review_label_removed", true, None));
            }
        }
        TriggerKind::ReviewRequested => {
            if !requests(raw, login) {
                return Err(Error::workflow("review_request_not_active", true, None));
            }
        }
        TriggerKind::Unset | TriggerKind::Other(_) => {
            return Err(Error::workflow("review_trigger_invalid", true, None));
        }
    }
    Ok(pull_request)
}

/// Decodes the state of a pull request that is read without a trigger (`ReadPullRequestState`).
///
/// A pull request that GitHub does not return as open gives the empty state with no error.
pub(crate) fn decode_pull_request_state(
    repository: &str,
    number: i64,
    output: &str,
    host: &str,
    login: &str,
) -> Result<PullRequestState, Error> {
    let raw = match decode_pull_request_response(output) {
        Ok(value) => value,
        Err(error) => {
            if autoreview_engine::error_code(&error) == "pull_request_not_open" {
                return Ok(PullRequestState::default());
            }
            return Err(error);
        }
    };
    let author = raw
        .author
        .as_ref()
        .map_or(String::new(), |a| a.login.to_ascii_lowercase());
    let state = PullRequestState {
        open: raw.state == OPEN_STATE,
        requested: requests(&raw, login),
        title: raw.title.trim().to_owned(),
        url: raw.url.clone(),
        author: author.clone(),
        head_sha: raw.head_sha.clone(),
    };
    if !raw
        .repository
        .name_with_owner
        .eq_ignore_ascii_case(repository)
        || raw.number != number
        || !sha_valid(&state.head_sha)
        || !author_valid(&author)
        || !url_matches(&state.url, host, repository, number)
    {
        return Err(response_invalid(None));
    }
    Ok(state)
}

/// Decodes one page of the review events API (`latestReviewRequestID`, one page).
///
/// Every element is decoded, and its time must parse, as Go decodes it. Only the events that
/// request a review from `login` are checked for an id and a non-zero time.
pub(crate) fn decode_review_request_page(
    output: &str,
    login: &str,
) -> Result<ReviewRequestPage, Error> {
    let entries: Vec<Option<ReviewEvent>> =
        serde_json::from_str(output).map_err(|error| response_invalid(Some(Box::new(error))))?;
    let events = null_entries::zero_entries(entries);
    let count = events.len();
    let mut requested = Vec::new();
    for event in events {
        let created_at_nanos = if let Some(text) = event.created_at {
            let parsed = go_time::parse_rfc3339(&text)
                .map_err(|error| response_invalid(Some(Box::new(error))))?;
            parsed.unix_timestamp_nanos()
        } else {
            GO_ZERO_TIME_NANOS
        };
        let is_requested = event.event == "review_requested"
            && event
                .requested_reviewer
                .as_ref()
                .is_some_and(|reviewer| reviewer.login.eq_ignore_ascii_case(login));
        if !is_requested {
            continue;
        }
        if event.id <= 0 || created_at_nanos == GO_ZERO_TIME_NANOS {
            return Err(response_invalid(None));
        }
        requested.push(ReviewRequestEvent {
            id: event.id,
            created_at_nanos,
        });
    }
    Ok(ReviewRequestPage {
        events: requested,
        count,
    })
}

/// Returns the id of the newest review request, or `None` when there is none.
///
/// The newest event has the latest time, and the larger id on a tie. An event at or before
/// the zero time is never taken, as in Go's `After` and `Equal` test.
pub(crate) fn newest_review_request(events: &[ReviewRequestEvent]) -> Option<i64> {
    let mut latest_at = GO_ZERO_TIME_NANOS;
    let mut latest_id = 0;
    for event in events {
        if event.created_at_nanos > latest_at
            || (event.created_at_nanos == latest_at && event.id > latest_id)
        {
            latest_at = event.created_at_nanos;
            latest_id = event.id;
        }
    }
    (latest_id != 0).then_some(latest_id)
}

impl GitHub {
    /// Reads a pull request and checks it against the trigger of its rule (`GetPullRequest`).
    pub(crate) async fn get_pull_request(
        &self,
        cancel: &CancellationToken,
        repository: &str,
        number: i64,
        trigger: &Trigger,
    ) -> Result<PullRequest, Error> {
        let raw = self.fetch_pull_request(cancel, repository, number).await?;
        let mut pull_request = decode_pull_request(&raw, &self.host, &self.login, trigger)?;
        if trigger.kind == TriggerKind::ReviewRequested {
            pull_request.review_request_id = self
                .latest_review_request_id(cancel, &pull_request.repository, pull_request.number)
                .await?;
        }
        if pull_request.repository != repository || pull_request.number != number {
            return Err(response_invalid(None));
        }
        Ok(pull_request)
    }

    /// Reads a pull request without a trigger check (`ReadPullRequestState`).
    pub(crate) async fn read_pull_request_state(
        &self,
        cancel: &CancellationToken,
        repository: &str,
        number: i64,
    ) -> Result<PullRequestState, Error> {
        let args = pull_request_args(repository, number)?;
        let output = self.github::<()>(cancel, &args, None).await?;
        decode_pull_request_state(repository, number, &output, &self.host, &self.login)
    }

    /// Reads the raw pull request data of one pull request (`fetchPullRequest`).
    pub(super) async fn fetch_pull_request(
        &self,
        cancel: &CancellationToken,
        repository: &str,
        number: i64,
    ) -> Result<GitHubPullRequest, Error> {
        let args = pull_request_args(repository, number)?;
        let output = self.github::<()>(cancel, &args, None).await?;
        decode_pull_request_response(&output)
    }

    /// Returns the id of the newest review request of the login (`latestReviewRequestID`).
    ///
    /// The events API is read page by page up to the configured page limit; a full last page
    /// at the limit fails with `github_search_limit_exceeded`.
    async fn latest_review_request_id(
        &self,
        cancel: &CancellationToken,
        repository: &str,
        number: i64,
    ) -> Result<i64, Error> {
        let mut events = Vec::new();
        for page in 1..=self.search_page_limit {
            let query = format!("per_page={EVENTS_PER_PAGE}&page={page}");
            let path = format!("repos/{repository}/issues/{number}/events?{query}");
            let args = vec!["api".to_owned(), path];
            let output = self.github::<()>(cancel, &args, None).await?;
            let decoded = decode_review_request_page(&output, &self.login)?;
            events.extend(decoded.events);
            if decoded.count < EVENTS_PER_PAGE {
                break;
            }
            if page == self.search_page_limit {
                return Err(Error::workflow("github_search_limit_exceeded", true, None));
            }
        }
        newest_review_request(&events)
            .ok_or_else(|| Error::workflow("review_request_event_missing", true, None))
    }
}

#[cfg(test)]
mod tests {
    use autoreview_engine::integrations::PullRequestState;
    use autoreview_policy::{Trigger, TriggerKind};

    use super::{decode_pull_request, decode_pull_request_response, decode_pull_request_state};

    /// Login of the reviewing account in the captured responses.
    const LOGIN: &str = "reviewer";

    /// Host of the captured pull request URLs.
    const HOST: &str = "example.org";

    /// Captured response of pull request 3, opened by an app.
    const BOT_RESPONSE: &str = concat!(
        r#"{"data":{"repository":{"pullRequest":{"id":"PR_1","number":3,"#,
        r#""title":"Add widget","url":"https://example.org/acme/widget/pull/3","#,
        r#""state":"OPEN","isDraft":false,"headRefOid":""#,
        r#"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","headRefName":"feature","#,
        r#""baseRefOid":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","#,
        r#""baseRefName":"main","author":{"__typename":"Bot","#,
        r#""login":"devin-ai-integration"},"labels":{"nodes":[]},"#,
        r#""reviewRequests":{"nodes":[{"requestedReviewer":{"login":"reviewer"}}]},"#,
        r#""repository":{"nameWithOwner":"acme/widget"}}}}}"#,
    );

    /// Captured response of pull request 4, opened by a person.
    const HUMAN_RESPONSE: &str = concat!(
        r#"{"data":{"repository":{"pullRequest":{"id":"PR_1","number":4,"#,
        r#""title":"Add widget","url":"https://example.org/acme/widget/pull/4","#,
        r#""state":"OPEN","isDraft":false,"headRefOid":""#,
        r#"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","headRefName":"feature","#,
        r#""baseRefOid":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","#,
        r#""baseRefName":"main","author":{"__typename":"User","#,
        r#""login":"colleague"},"labels":{"nodes":[]},"#,
        r#""reviewRequests":{"nodes":[{"requestedReviewer":{"login":"reviewer"}}]},"#,
        r#""repository":{"nameWithOwner":"acme/widget"}}}}}"#,
    );

    #[test]
    fn app_author_is_marked_as_a_bot_and_state_is_read_without_a_trigger() {
        let trigger = Trigger {
            kind: TriggerKind::ReviewRequested,
            label: String::new(),
        };
        let bot_raw = decode_pull_request_response(BOT_RESPONSE).expect("bot response decodes");
        let bot = decode_pull_request(&bot_raw, HOST, LOGIN, &trigger).expect("bot is decoded");
        assert!(bot.author_bot);
        assert_eq!(bot.author, "devin-ai-integration");

        let human_raw =
            decode_pull_request_response(HUMAN_RESPONSE).expect("human response decodes");
        let human =
            decode_pull_request(&human_raw, HOST, LOGIN, &trigger).expect("human is decoded");
        assert!(!human.author_bot);

        let state = decode_pull_request_state("acme/widget", 4, HUMAN_RESPONSE, HOST, LOGIN)
            .expect("state is read");
        assert_eq!(
            state,
            PullRequestState {
                open: true,
                requested: true,
                title: "Add widget".to_owned(),
                url: "https://example.org/acme/widget/pull/4".to_owned(),
                author: "colleague".to_owned(),
                head_sha: "a".repeat(40),
            }
        );
    }
}
