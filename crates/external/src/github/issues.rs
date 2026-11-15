//! The read of the issues that a pull request closes, for the intent check.
//!
//! The decode takes the captured `gh` output, so it is a pure function of its argument.

// Rust guideline compliant 2026-11-15

use autoreview_engine::config::Credential;
use autoreview_engine::error::Error;
use autoreview_engine::model::{LinkedIssue, LinkedIssues, PullRequest, repository_valid};
use autoreview_go_compat::null_default;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::{GitHub, response_invalid};

/// Query of the closing issue references of one pull request.
///
/// One page of 10 is read; `hasNextPage` tells the reviewers that the pull request closes more.
const GRAPHQL_CLOSING_ISSUES: &str = concat!(
    "query($owner: String!, $name: String!, $number: Int!) {\n",
    "  repository(owner: $owner, name: $name) {\n",
    "    pullRequest(number: $number) {\n",
    "      closingIssuesReferences(first: 10) {\n",
    "        pageInfo { hasNextPage }\n",
    "        nodes { number title body repository { nameWithOwner } }\n",
    "      }\n",
    "    }\n",
    "  }\n",
    "}",
);

/// The response of the closing issue query.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct IssuesResponse {
    /// The data of the query.
    #[serde(deserialize_with = "null_default::deserialize")]
    data: IssuesData,
    /// The errors of the response; any entry makes the response invalid.
    #[serde(deserialize_with = "null_default::deserialize")]
    errors: Vec<serde_json::Value>,
}

autoreview_go_compat::object_only_serde!(IssuesResponse);

/// The data of the closing issue query.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self", deny_unknown_fields)]
struct IssuesData {
    /// The repository, `null` when there is none.
    repository: Option<IssuesRepository>,
}

autoreview_go_compat::object_only_serde!(IssuesData);

/// The repository node of the closing issue query.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self", deny_unknown_fields)]
struct IssuesRepository {
    /// The pull request, `null` when the repository has none with that number.
    #[serde(rename = "pullRequest")]
    pull_request: Option<IssuesPullRequest>,
}

autoreview_go_compat::object_only_serde!(IssuesRepository);

/// The pull request node of the closing issue query.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self", deny_unknown_fields)]
struct IssuesPullRequest {
    /// The issues that the pull request closes.
    #[serde(rename = "closingIssuesReferences")]
    closing: Option<IssueConnection>,
}

autoreview_go_compat::object_only_serde!(IssuesPullRequest);

/// One page of closing issues.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self", deny_unknown_fields)]
struct IssueConnection {
    /// Paging of the issues.
    #[serde(rename = "pageInfo", deserialize_with = "null_default::deserialize")]
    page_info: IssuePageInfo,
    /// The issues; a `null` entry is an issue GitHub did not return.
    #[serde(deserialize_with = "null_default::deserialize")]
    nodes: Vec<Option<IssueNode>>,
}

autoreview_go_compat::object_only_serde!(IssueConnection);

/// Paging of the closing issues.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self", deny_unknown_fields)]
struct IssuePageInfo {
    /// Whether more issues follow.
    #[serde(rename = "hasNextPage", deserialize_with = "null_default::deserialize")]
    has_next_page: bool,
}

autoreview_go_compat::object_only_serde!(IssuePageInfo);

/// One closing issue.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self", deny_unknown_fields)]
struct IssueNode {
    /// Issue number.
    number: i64,
    /// Issue title.
    #[serde(deserialize_with = "null_default::deserialize")]
    title: String,
    /// Issue body; `null` reads as empty.
    #[serde(deserialize_with = "null_default::deserialize")]
    body: String,
    /// Repository of the issue.
    repository: Option<IssueRepository>,
}

autoreview_go_compat::object_only_serde!(IssueNode);

/// The repository of a closing issue.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self", deny_unknown_fields)]
struct IssueRepository {
    /// Repository as `owner/name`.
    #[serde(
        rename = "nameWithOwner",
        deserialize_with = "null_default::deserialize"
    )]
    name_with_owner: String,
}

autoreview_go_compat::object_only_serde!(IssueRepository);

/// Returns the `gh` arguments of the closing issue query of `pull_request`.
fn closing_issue_args(pull_request: &PullRequest) -> Vec<String> {
    let (owner, name) = match pull_request.repository.split_once('/') {
        Some((owner, name)) => (owner, name),
        None => (pull_request.repository.as_str(), ""),
    };
    vec![
        "api".to_owned(),
        "graphql".to_owned(),
        "-f".to_owned(),
        format!("query={GRAPHQL_CLOSING_ISSUES}"),
        "-F".to_owned(),
        format!("owner={owner}"),
        "-F".to_owned(),
        format!("name={name}"),
        "-F".to_owned(),
        format!("number={}", pull_request.number),
    ]
}

/// Decodes the closing issues that `gh api graphql` returned.
///
/// A response with an error entry, without the pull request, with a `null` issue, or with an
/// issue whose repository or number is not valid is `github_response_invalid`: partial data
/// is never read as the whole list. Repository names are lowercased, as the engine keeps them.
fn decode_closing_issues(output: &str) -> Result<LinkedIssues, Error> {
    let response: IssuesResponse =
        serde_json::from_str(output).map_err(|error| response_invalid(Some(Box::new(error))))?;
    if !response.errors.is_empty() {
        return Err(response_invalid(None));
    }
    let connection = (response.data.repository)
        .and_then(|repository| repository.pull_request)
        .and_then(|pull_request| pull_request.closing)
        .ok_or_else(|| response_invalid(None))?;
    let mut issues = Vec::with_capacity(connection.nodes.len());
    for node in connection.nodes {
        let Some(node) = node else {
            return Err(response_invalid(None));
        };
        let repository = (node.repository)
            .map(|repository| repository.name_with_owner.to_ascii_lowercase())
            .unwrap_or_default();
        if !repository_valid(&repository) || node.number < 1 {
            return Err(response_invalid(None));
        }
        issues.push(LinkedIssue {
            repository,
            number: node.number,
            title: node.title,
            body: node.body,
            truncated: false,
        });
    }
    Ok(LinkedIssues {
        read: true,
        issues,
        more: connection.page_info.has_next_page,
    })
}

impl GitHub {
    /// Reads the issues that `pull_request` closes with `credential`'s token.
    pub(crate) async fn list_closing_issues(
        &self,
        cancel: &CancellationToken,
        credential: &Credential,
        pull_request: &PullRequest,
    ) -> Result<LinkedIssues, Error> {
        let args = closing_issue_args(pull_request);
        let output = self.github::<()>(cancel, credential, &args, None).await?;
        decode_closing_issues(&output)
    }
}
