//! The read of the issues that a pull request closes, for the intent check.
//!
//! Only issues of the pull request's own repository are read: an issue of another repository
//! may be private while the review is public, so its text is never requested. The decodes take
//! the captured `gh` output, so each is a pure function of its arguments.

// Rust guideline compliant 2026-11-15

use std::collections::BTreeMap;
use std::future::Future;

use autoreview_engine::config::Credential;
use autoreview_engine::error::Error;
use autoreview_engine::model::{LinkedIssue, LinkedIssues, PullRequest, repository_valid};
use autoreview_go_compat::null_default;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::{GitHub, response_invalid};

/// Query of the closing issue references of one pull request, without their text.
///
/// One page of 10 is read; `hasNextPage` tells the reviewers that the pull request closes more.
const GRAPHQL_CLOSING_REFERENCES: &str = concat!(
    "query($owner: String!, $name: String!, $number: Int!) {\n",
    "  repository(owner: $owner, name: $name) {\n",
    "    pullRequest(number: $number) {\n",
    "      closingIssuesReferences(first: 10) {\n",
    "        pageInfo { hasNextPage }\n",
    "        nodes { number repository { nameWithOwner } }\n",
    "      }\n",
    "    }\n",
    "  }\n",
    "}",
);

/// Alias prefix of each issue in the text query; the index follows it.
const ISSUE_ALIAS: &str = "issue";

/// The response of the closing reference query.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct ReferencesResponse {
    /// The data of the query.
    #[serde(deserialize_with = "null_default::deserialize")]
    data: ReferencesData,
    /// The errors of the response; any entry makes the response invalid.
    #[serde(deserialize_with = "null_default::deserialize")]
    errors: Vec<serde_json::Value>,
}

autoreview_go_compat::object_only_serde!(ReferencesResponse);

/// The data of the closing reference query.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self", deny_unknown_fields)]
struct ReferencesData {
    /// The repository, `null` when there is none.
    repository: Option<ReferencesRepository>,
}

autoreview_go_compat::object_only_serde!(ReferencesData);

/// The repository node of the closing reference query.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self", deny_unknown_fields)]
struct ReferencesRepository {
    /// The pull request, `null` when the repository has none with that number.
    #[serde(rename = "pullRequest")]
    pull_request: Option<ReferencesPullRequest>,
}

autoreview_go_compat::object_only_serde!(ReferencesRepository);

/// The pull request node of the closing reference query.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self", deny_unknown_fields)]
struct ReferencesPullRequest {
    /// The issues that the pull request closes.
    #[serde(rename = "closingIssuesReferences")]
    closing: Option<ReferenceConnection>,
}

autoreview_go_compat::object_only_serde!(ReferencesPullRequest);

/// One page of closing issue references.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self", deny_unknown_fields)]
struct ReferenceConnection {
    /// Paging of the references.
    #[serde(rename = "pageInfo", deserialize_with = "null_default::deserialize")]
    page_info: ReferencePageInfo,
    /// The references; a `null` entry is an issue GitHub did not return.
    #[serde(deserialize_with = "null_default::deserialize")]
    nodes: Vec<Option<ReferenceNode>>,
}

autoreview_go_compat::object_only_serde!(ReferenceConnection);

/// Paging of the closing issue references.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self", deny_unknown_fields)]
struct ReferencePageInfo {
    /// Whether more references follow.
    #[serde(rename = "hasNextPage", deserialize_with = "null_default::deserialize")]
    has_next_page: bool,
}

autoreview_go_compat::object_only_serde!(ReferencePageInfo);

/// One closing issue reference.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self", deny_unknown_fields)]
struct ReferenceNode {
    /// Issue number.
    number: i64,
    /// Repository of the issue.
    repository: Option<ReferenceRepository>,
}

autoreview_go_compat::object_only_serde!(ReferenceNode);

/// The repository of an issue, in either query.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self", deny_unknown_fields)]
struct ReferenceRepository {
    /// Repository as `owner/name`.
    #[serde(
        rename = "nameWithOwner",
        deserialize_with = "null_default::deserialize"
    )]
    name_with_owner: String,
}

autoreview_go_compat::object_only_serde!(ReferenceRepository);

/// The response of the issue text query.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct TextResponse {
    /// The data of the query.
    #[serde(deserialize_with = "null_default::deserialize")]
    data: TextData,
    /// The errors of the response; any entry makes the response invalid.
    #[serde(deserialize_with = "null_default::deserialize")]
    errors: Vec<serde_json::Value>,
}

autoreview_go_compat::object_only_serde!(TextResponse);

/// The data of the issue text query.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self", deny_unknown_fields)]
struct TextData {
    /// The issues by alias, `null` when the repository is missing.
    repository: Option<BTreeMap<String, Option<IssueText>>>,
}

autoreview_go_compat::object_only_serde!(TextData);

/// The text of one issue.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self", deny_unknown_fields)]
struct IssueText {
    /// Issue number.
    number: i64,
    /// Issue title.
    #[serde(deserialize_with = "null_default::deserialize")]
    title: String,
    /// Issue body; `null` reads as empty.
    #[serde(deserialize_with = "null_default::deserialize")]
    body: String,
    /// Repository of the issue; it must be the pull request's.
    repository: Option<ReferenceRepository>,
}

autoreview_go_compat::object_only_serde!(IssueText);

/// The closing references of a pull request, split by repository.
#[derive(Debug, Default, PartialEq, Eq)]
struct ClosingReferences {
    /// Numbers of the closed issues of the pull request's own repository, in GitHub's order.
    numbers: Vec<i64>,
    /// Set when the pull request also closes issues of other repositories.
    other_repositories: bool,
    /// Set when the pull request closes more issues than one read returns.
    more: bool,
}

/// Returns the owner and name of `pull_request`'s repository.
fn owner_and_name(pull_request: &PullRequest) -> (&str, &str) {
    match pull_request.repository.split_once('/') {
        Some((owner, name)) => (owner, name),
        None => (pull_request.repository.as_str(), ""),
    }
}

/// Returns the `gh` arguments of `query` on `pull_request`'s repository.
///
/// The pull request number is sent only when `number` is set.
fn query_args(pull_request: &PullRequest, query: &str, number: bool) -> Vec<String> {
    let (owner, name) = owner_and_name(pull_request);
    let mut args = vec![
        "api".to_owned(),
        "graphql".to_owned(),
        "-f".to_owned(),
        format!("query={query}"),
        "-F".to_owned(),
        format!("owner={owner}"),
        "-F".to_owned(),
        format!("name={name}"),
    ];
    if number {
        args.push("-F".to_owned());
        args.push(format!("number={}", pull_request.number));
    }
    args
}

/// Returns the query of the title and body of `numbers` in one repository, one alias each.
///
/// The numbers are validated integers, so the query text holds no outside text.
fn issue_text_query(numbers: &[i64]) -> String {
    let mut query = String::from(
        "query($owner: String!, $name: String!) {\n  repository(owner: $owner, name: $name) {\n",
    );
    for (index, number) in numbers.iter().enumerate() {
        query.push_str(&format!(
            "    {ISSUE_ALIAS}{index}: issue(number: {number}) \
             {{ number title body repository {{ nameWithOwner }} }}\n"
        ));
    }
    query.push_str("  }\n}");
    query
}

/// Decodes the closing references and splits them by `repository`, the pull request's own.
///
/// A response with an error entry, without the pull request, with a `null` reference, or with
/// a reference whose repository or number is not valid is `github_response_invalid`.
fn decode_closing_references(output: &str, repository: &str) -> Result<ClosingReferences, Error> {
    let response: ReferencesResponse =
        serde_json::from_str(output).map_err(|error| response_invalid(Some(Box::new(error))))?;
    if !response.errors.is_empty() {
        return Err(response_invalid(None));
    }
    let connection = (response.data.repository)
        .and_then(|repository| repository.pull_request)
        .and_then(|pull_request| pull_request.closing)
        .ok_or_else(|| response_invalid(None))?;
    let mut references = ClosingReferences {
        more: connection.page_info.has_next_page,
        ..ClosingReferences::default()
    };
    for node in connection.nodes {
        let Some(node) = node else {
            return Err(response_invalid(None));
        };
        let owner = (node.repository)
            .map(|repository| repository.name_with_owner.to_ascii_lowercase())
            .unwrap_or_default();
        if !repository_valid(&owner) || node.number < 1 {
            return Err(response_invalid(None));
        }
        if owner.eq_ignore_ascii_case(repository) {
            references.numbers.push(node.number);
        } else {
            references.other_repositories = true;
        }
    }
    Ok(references)
}

/// Decodes the issue texts of `numbers`, asked for with [`issue_text_query`].
///
/// A response with an error entry, a missing or `null` issue, or an issue of another number or
/// of another repository than `repository` is `github_response_invalid`. The repository check
/// keeps out an issue transferred after the reference query, even when its number matches.
fn decode_issue_texts(
    output: &str,
    repository: &str,
    numbers: &[i64],
) -> Result<Vec<LinkedIssue>, Error> {
    let response: TextResponse =
        serde_json::from_str(output).map_err(|error| response_invalid(Some(Box::new(error))))?;
    if !response.errors.is_empty() {
        return Err(response_invalid(None));
    }
    let mut texts = response
        .data
        .repository
        .ok_or_else(|| response_invalid(None))?;
    let mut issues = Vec::with_capacity(numbers.len());
    for (index, number) in numbers.iter().enumerate() {
        let text = texts
            .remove(&format!("{ISSUE_ALIAS}{index}"))
            .flatten()
            .filter(|text| {
                let owner = text.repository.as_ref().map(|owner| &owner.name_with_owner);
                text.number == *number
                    && owner.is_some_and(|owner| owner.eq_ignore_ascii_case(repository))
            })
            .ok_or_else(|| response_invalid(None))?;
        issues.push(LinkedIssue {
            repository: repository.to_owned(),
            number: *number,
            title: text.title,
            body: text.body,
            truncated: false,
        });
    }
    Ok(issues)
}

/// Reads the issues that `pull_request` closes in its own repository, through `fetch`.
///
/// `fetch` receives the `gh` arguments of one query and returns its output. The text query
/// names only issues of the pull request's repository and runs only when there is one.
async fn read_closing_issues<F, Fut>(
    mut fetch: F,
    pull_request: &PullRequest,
) -> Result<LinkedIssues, Error>
where
    F: FnMut(Vec<String>) -> Fut,
    Fut: Future<Output = Result<String, Error>>,
{
    let repository = pull_request.repository.as_str();
    let args = query_args(pull_request, GRAPHQL_CLOSING_REFERENCES, true);
    let references = decode_closing_references(&fetch(args).await?, repository)?;
    let issues = if references.numbers.is_empty() {
        Vec::new()
    } else {
        let query = issue_text_query(&references.numbers);
        let output = fetch(query_args(pull_request, &query, false)).await?;
        decode_issue_texts(&output, repository, &references.numbers)?
    };
    Ok(LinkedIssues {
        read: true,
        issues,
        more: references.more,
        other_repositories: references.other_repositories,
    })
}

impl GitHub {
    /// Reads the issues that `pull_request` closes in its own repository with `credential`.
    pub(crate) async fn list_closing_issues(
        &self,
        cancel: &CancellationToken,
        credential: &Credential,
        pull_request: &PullRequest,
    ) -> Result<LinkedIssues, Error> {
        read_closing_issues(
            |args: Vec<String>| async move {
                self.github::<()>(cancel, credential, &args, None).await
            },
            pull_request,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use autoreview_engine::error_code;
    use serde_json::json;

    use super::{
        ClosingReferences, decode_closing_references, decode_issue_texts, issue_text_query,
    };

    /// Returns the reference page of `nodes` as GitHub answers it.
    fn references(nodes: serde_json::Value, more: bool) -> String {
        json!({"data": {"repository": {"pullRequest": {"closingIssuesReferences": {
            "pageInfo": {"hasNextPage": more}, "nodes": nodes
        }}}}})
        .to_string()
    }

    /// Returns one reference node.
    fn node(repository: &str, number: i64) -> serde_json::Value {
        json!({"number": number, "repository": {"nameWithOwner": repository}})
    }

    /// Returns the text page of `issues` under aliases in order.
    ///
    /// Each issue is `(repository, number, title, body)`; `None` sends the repository as `null`.
    fn texts(issues: &[(Option<&str>, i64, &str, &str)]) -> String {
        let mut page = serde_json::Map::new();
        for (index, (repository, number, title, body)) in issues.iter().enumerate() {
            let repository = repository.map(|name| json!({"nameWithOwner": name}));
            let text = json!({
                "number": number, "title": title, "body": body, "repository": repository
            });
            page.insert(format!("issue{index}"), text);
        }
        json!({"data": {"repository": page}}).to_string()
    }

    /// Each row is a reference page of a pull request of `acme/widget` and the references read
    /// from it, or the error code. An issue of another repository only sets
    /// `other_repositories`: its number never reaches the text query, so its text is neither
    /// asked for nor passed on (it may be private while the review is public).
    #[test]
    fn only_issues_of_the_pull_request_repository_are_read() {
        let read = |numbers: &[i64], other: bool, more: bool| {
            Ok(ClosingReferences {
                numbers: numbers.to_vec(),
                other_repositories: other,
                more,
            })
        };
        let cases: [(&str, String, Result<ClosingReferences, &str>); 6] = [
            (
                "own repository in another case",
                references(json!([node("Acme/Widget", 12)]), false),
                read(&[12], false, false),
            ),
            (
                "a cross-repository reference is not read",
                references(
                    json!([node("acme/widget", 12), node("secret/tracker", 3)]),
                    true,
                ),
                read(&[12], true, true),
            ),
            (
                "only cross-repository references",
                references(json!([node("secret/tracker", 3)]), false),
                read(&[], true, false),
            ),
            (
                "no reference",
                references(json!([]), false),
                read(&[], false, false),
            ),
            (
                "an error entry",
                json!({"data": null, "errors": [{"type": "FORBIDDEN"}]}).to_string(),
                Err("github_response_invalid"),
            ),
            (
                "a null reference",
                references(json!([null]), false),
                Err("github_response_invalid"),
            ),
        ];
        for (name, page, want) in cases {
            let got = decode_closing_references(&page, "acme/widget");
            let got = got.map_err(|err| error_code(&err).to_owned());
            assert_eq!(got, want.map_err(str::to_owned), "{name}");
        }
        let query = issue_text_query(&[12, 14]);
        assert!(
            query.contains(concat!(
                "    issue0: issue(number: 12) ",
                "{ number title body repository { nameWithOwner } }\n",
                "    issue1: issue(number: 14) ",
                "{ number title body repository { nameWithOwner } }\n",
            )),
            "{query}"
        );
        // Each row is a text page for the numbers 12 and 14 of `acme/widget`. A text is kept
        // only when GitHub returns it with the asked number and repository; any other text,
        // such as one of an issue transferred between the two queries, fails the whole read.
        let own = Some("acme/widget");
        let both = [
            ("acme/widget", 12, "Own", "Body", false),
            ("acme/widget", 14, "Second", "", false),
        ];
        let missing = json!({"data": {"repository": {
            "issue0": {"number": 12, "title": "Own", "body": "Body"},
            "issue1": {"number": 14, "title": "Second", "body": ""}
        }}});
        type Texts = Vec<(&'static str, i64, &'static str, &'static str, bool)>;
        let text_cases: [(&str, String, Result<Texts, &str>); 6] = [
            (
                "own repository",
                texts(&[(own, 12, "Own", "Body"), (own, 14, "Second", "")]),
                Ok(both.to_vec()),
            ),
            (
                "own repository in another case",
                texts(&[
                    (Some("Acme/Widget"), 12, "Own", "Body"),
                    (own, 14, "Second", ""),
                ]),
                Ok(both.to_vec()),
            ),
            (
                "an issue of another repository",
                texts(&[
                    (own, 12, "Own", "Body"),
                    (Some("secret/tracker"), 14, "Moved", ""),
                ]),
                Err("github_response_invalid"),
            ),
            (
                "an issue with a null repository",
                texts(&[(own, 12, "Own", "Body"), (None, 14, "Second", "")]),
                Err("github_response_invalid"),
            ),
            (
                "issues without their repository",
                missing.to_string(),
                Err("github_response_invalid"),
            ),
            (
                "an issue of another number",
                texts(&[(own, 12, "Own", "Body"), (own, 13, "Other", "")]),
                Err("github_response_invalid"),
            ),
        ];
        for (name, page, want) in text_cases {
            let got = decode_issue_texts(&page, "acme/widget", &[12, 14]);
            let got = got.map_err(|err| error_code(&err).to_owned());
            let got = (got.as_ref())
                .map(|issues| {
                    (issues.iter())
                        .map(|issue| {
                            let (title, body) = (issue.title.as_str(), issue.body.as_str());
                            let repository = issue.repository.as_str();
                            (repository, issue.number, title, body, issue.truncated)
                        })
                        .collect::<Vec<_>>()
                })
                .map_err(String::as_str);
            assert_eq!(got, want, "{name}");
        }
    }
}
