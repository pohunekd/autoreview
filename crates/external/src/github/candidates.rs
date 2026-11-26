//! Candidate discovery of the engine: the review request and label searches, the drafts that
//! coding agents ask to have marked ready, and the search of requested reviews in every
//! repository.
//!
//! Each search is an argv list, and each decode function takes the captured `gh` output, so
//! the selection is a pure function of its input.

// Rust guideline compliant 2026-06-26

use std::collections::{BTreeMap, BTreeSet};

use autoreview_engine::config::{Credential, independent_scopes};
use autoreview_engine::error::Error;
use autoreview_engine::error_code;
use autoreview_engine::integrations::{ReadiedDraft, ReviewRequest, ReviewRequestList};
use autoreview_engine::model::{
    Candidate, CandidateList, PullRequest, REJECTED_CANDIDATE_CODES, RejectedCandidate,
    pull_request_url_valid, repository_valid,
};
use autoreview_go_compat::{null_default, null_entries};
use autoreview_policy::{Authors, Policy, Rule, Trigger, TriggerKind};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::pull_request::{GitHubPullRequest, decode_pull_request, ready_draft_id};
use super::{GitHub, graphql_search, response_invalid};

/// Query that marks a draft pull request ready for review (`graphQLMarkReady`).
const GRAPHQL_MARK_READY: &str = concat!(
    "mutation($id: ID!) {\n",
    "  markPullRequestReadyForReview(input: {pullRequestId: $id}) { pullRequest { isDraft } }\n",
    "}",
);

/// One result of a review request search, read from `gh search prs --json number,repository`.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct SearchIdentity {
    /// Pull request number.
    #[serde(deserialize_with = "null_default::deserialize")]
    number: i64,
    /// Repository, an object with `nameWithOwner` or a plain string as the search prints it.
    repository: serde_json::Value,
}

autoreview_go_compat::object_only_serde!(SearchIdentity);

/// One result of the search of requested reviews in every repository.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct ReviewRequestItem {
    /// Pull request number.
    #[serde(deserialize_with = "null_default::deserialize")]
    number: i64,
    /// Repository, as in [`SearchIdentity`].
    repository: serde_json::Value,
    /// Pull request title, kept as GitHub returns it.
    #[serde(deserialize_with = "null_default::deserialize")]
    title: String,
    /// Pull request URL, which must name the repository and the number.
    #[serde(deserialize_with = "null_default::deserialize")]
    url: String,
    /// Author of the pull request, `null` for a deleted account.
    author: Option<SearchAuthor>,
}

autoreview_go_compat::object_only_serde!(ReviewRequestItem);

/// The author of a search result.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct SearchAuthor {
    /// Login of the author.
    #[serde(deserialize_with = "null_default::deserialize")]
    login: String,
}

autoreview_go_compat::object_only_serde!(SearchAuthor);

/// The GraphQL response of the label search.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct SearchResponse {
    /// The data of the response.
    #[serde(deserialize_with = "null_default::deserialize")]
    data: SearchData,
    /// The errors of the response; any entry makes the response invalid.
    #[serde(deserialize_with = "null_default::deserialize")]
    errors: Vec<serde_json::Value>,
}

autoreview_go_compat::object_only_serde!(SearchResponse);

/// The data of the label search response.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct SearchData {
    /// The search of the query.
    #[serde(deserialize_with = "null_default::deserialize")]
    search: SearchResult,
}

autoreview_go_compat::object_only_serde!(SearchData);

/// The search result of the label search: one page and its paging.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct SearchResult {
    /// Paging of the search.
    #[serde(rename = "pageInfo", deserialize_with = "null_default::deserialize")]
    page_info: PageInfo,
    /// The pull requests of the page; a `null` list is refused.
    ///
    /// A `null` entry is an empty pull request, which no rule covers.
    nodes: Option<Vec<Option<GitHubPullRequest>>>,
}

autoreview_go_compat::object_only_serde!(SearchResult);

/// The paging of a search.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct PageInfo {
    /// Whether another page follows.
    #[serde(rename = "hasNextPage", deserialize_with = "null_default::deserialize")]
    has_next_page: bool,
    /// Cursor of the next page, empty when there is none; `null` reads as empty.
    #[serde(rename = "endCursor", deserialize_with = "null_default::deserialize")]
    end_cursor: String,
}

autoreview_go_compat::object_only_serde!(PageInfo);

/// The GraphQL response of the mark-ready mutation.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct MarkReadyResponse {
    /// The data of the response.
    #[serde(deserialize_with = "null_default::deserialize")]
    data: MarkReadyData,
    /// The errors of the response; any entry makes the response invalid.
    #[serde(deserialize_with = "null_default::deserialize")]
    errors: Vec<serde_json::Value>,
}

autoreview_go_compat::object_only_serde!(MarkReadyResponse);

/// The data of the mark-ready response.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct MarkReadyData {
    /// The mutation result, `null` when GitHub did not run it.
    #[serde(rename = "markPullRequestReadyForReview")]
    mark: Option<MarkReadyPayload>,
}

autoreview_go_compat::object_only_serde!(MarkReadyData);

/// The payload of the mark-ready mutation.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct MarkReadyPayload {
    /// The pull request after the mutation.
    #[serde(rename = "pullRequest", deserialize_with = "null_default::deserialize")]
    pull_request: MarkedPullRequest,
}

autoreview_go_compat::object_only_serde!(MarkReadyPayload);

/// The pull request that the mark-ready mutation returns.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct MarkedPullRequest {
    /// Whether the pull request is still a draft.
    #[serde(rename = "isDraft", deserialize_with = "null_default::deserialize")]
    is_draft: bool,
}

autoreview_go_compat::object_only_serde!(MarkedPullRequest);

/// One page of the label search, decoded.
#[derive(Debug)]
struct LabelSearchPage {
    /// The pull requests of the page, in GitHub's order.
    nodes: Vec<GitHubPullRequest>,
    /// Whether another page follows.
    has_next_page: bool,
    /// Cursor of the next page.
    end_cursor: String,
}

/// Returns the repository that a search result names.
///
/// A value that is neither a string nor an object with a string `nameWithOwner` gives the empty
/// name, which no later check accepts.
fn repository_name(value: &serde_json::Value) -> &str {
    if let Some(text) = value.as_str() {
        text
    } else if let Some(map) = value.as_object() {
        if let Some(name) = map.get("nameWithOwner")
            && let Some(text) = name.as_str()
        {
            return text;
        }
        ""
    } else {
        ""
    }
}

/// Reports whether a failed read is a pull request that changed after the search
/// (`skippableCandidateError`).
fn skippable_candidate_error(error: &Error) -> bool {
    matches!(
        error_code(error),
        "pull_request_not_open"
            | "pull_request_draft"
            | "review_request_not_active"
            | "review_label_removed"
    )
}

/// Returns the rule that covers `repository` for `trigger`, with its authors section.
///
/// Both come from the policy's first match. A first match with another trigger covers nothing,
/// so a later rule never lends its section.
fn covering<'a>(
    policy: &'a Policy,
    repository: &str,
    trigger: &Trigger,
) -> Option<(&'a Rule, &'a Authors)> {
    policy
        .resolve_with_authors(repository)
        .filter(|(rule, _)| rule.trigger == *trigger)
}

/// Sorts the outcome of reading one candidate: kept, skipped because its trigger lapsed,
/// rejected because its data is invalid or unsupported, or a failure that stops the search
/// (`classify`).
///
/// A kept candidate carries `rule` and `authors`, which come from one policy match.
fn classify(
    list: &mut CandidateList,
    repository: &str,
    number: i64,
    (rule, authors): (&Rule, &Authors),
    outcome: Result<PullRequest, Error>,
) -> Result<(), Error> {
    match outcome {
        Ok(pull_request) => {
            list.candidates.push(Candidate {
                pull_request,
                rule: rule.clone(),
                author_class: None,
                authors: Some(authors.clone()),
            });
            Ok(())
        }
        Err(error) => {
            if skippable_candidate_error(&error) {
                Ok(())
            } else {
                let code = error_code(&error);
                if REJECTED_CANDIDATE_CODES.contains(&code) {
                    list.rejected.push(RejectedCandidate {
                        repository: repository.to_owned(),
                        number,
                        code: code.to_owned(),
                        permission_login: None,
                    });
                    Ok(())
                } else {
                    Err(error)
                }
            }
        }
    }
}

/// Merges the outcome of one search into the candidates (`collect` in `ListCandidates`).
///
/// The first candidate of a pull request wins, and the last rejection of a pull request wins.
fn collect(
    found: &mut BTreeMap<String, Candidate>,
    rejected: &mut BTreeMap<String, RejectedCandidate>,
    list: CandidateList,
) {
    for candidate in list.candidates {
        found
            .entry(candidate.pull_request.key())
            .or_insert(candidate);
    }
    for item in list.rejected {
        rejected.insert(item.key(), item);
    }
}

/// Decodes the identities of a review request search and refuses a result at the poll limit
/// (`searchRequested`).
fn decode_search_identities(output: &str, poll_limit: i64) -> Result<Vec<SearchIdentity>, Error> {
    let entries: Vec<Option<SearchIdentity>> =
        serde_json::from_str(output).map_err(|error| response_invalid(Some(Box::new(error))))?;
    let identities = null_entries::zero_entries(entries);
    // A truncated search would silently drop requested reviews.
    let count = i64::try_from(identities.len()).unwrap_or(i64::MAX);
    if count >= poll_limit {
        return Err(Error::workflow("github_search_limit_exceeded", true, None));
    }
    Ok(identities)
}

/// Returns `lists` as one list without repeats, keeping the first occurrence of each pull
/// request (`searchRequested`).
///
/// Two results are the same pull request when their repository names match ignoring case and
/// their numbers match.
fn merge_identities(lists: Vec<Vec<SearchIdentity>>) -> Vec<SearchIdentity> {
    let mut seen: BTreeSet<(String, i64)> = BTreeSet::new();
    let mut merged = Vec::new();
    for identity in lists.into_iter().flatten() {
        let repository = repository_name(&identity.repository).to_ascii_lowercase();
        if seen.insert((repository, identity.number)) {
            merged.push(identity);
        }
    }
    merged
}

/// Returns `lists`, the results of the searches of one scope each, as one list without
/// repeats (`ListReviewRequests`).
///
/// The list is `truncated` when one search was, or when the union exceeds `max`. The union is
/// not cut: the queue sorts by priority first and cuts after. One list is returned as it is.
fn merge_review_requests(mut lists: Vec<ReviewRequestList>, max: i64) -> ReviewRequestList {
    if lists.len() == 1 {
        return lists.remove(0);
    }
    let mut truncated = false;
    let mut seen: BTreeSet<(String, i64)> = BTreeSet::new();
    let mut requests = Vec::new();
    for list in lists {
        truncated |= list.truncated;
        for request in list.requests {
            if seen.insert((request.repository.clone(), request.number)) {
                requests.push(request);
            }
        }
    }
    let limit = usize::try_from(max).unwrap_or(0);
    truncated |= requests.len() > limit;
    ReviewRequestList {
        requests,
        truncated,
    }
}

/// Returns the argv of a review request search of one `scope` with `extra` flags and the
/// negative qualifiers `exclusions` (`searchRequested`).
///
/// A scope `owner/*` searches by `--owner`; any other scope searches by `--repo`. `gh` joins
/// several scope flags into one search whose results GitHub may cut for a fine-grained token,
/// so a search carries one scope.
fn search_requested_args(
    poll_limit: i64,
    scope: &str,
    extra: &[String],
    exclusions: &[String],
) -> Vec<String> {
    let mut args = vec![
        "search".to_owned(),
        "prs".to_owned(),
        "--review-requested=@me".to_owned(),
        "--state=open".to_owned(),
        "--limit".to_owned(),
        poll_limit.to_string(),
        "--json".to_owned(),
        "number,repository".to_owned(),
    ];
    args.extend(extra.iter().cloned());
    args.extend(scope_args(scope));
    args.extend(query_args(exclusions));
    args
}

/// Returns the negative qualifiers `exclusions` as query words after `--`, none when empty.
///
/// `--` keeps `gh` from reading a word that starts with `-` as a flag.
fn query_args(exclusions: &[String]) -> Vec<String> {
    if exclusions.is_empty() {
        return Vec::new();
    }
    let mut args = vec!["--".to_owned()];
    args.extend(exclusions.iter().cloned());
    args
}

/// Returns the scope flags of a search: `--owner` for `owner/*`, else `--repo`.
fn scope_args(scope: &str) -> Vec<String> {
    if let Some((owner, "*")) = scope.split_once('/') {
        return vec!["--owner".to_owned(), owner.to_owned()];
    }
    vec!["--repo".to_owned(), scope.to_owned()]
}

/// Returns the argv of the search of requested reviews in the credential pattern `scope`
/// without the negative qualifiers `exclusions` (`ListReviewRequests`).
///
/// The search asks for one result more than `max`. `wrapping_add` wraps `i64::MAX` to
/// `i64::MIN` in every build profile. Without a `scope`, it searches every repository.
fn list_review_requests_args(max: i64, scope: Option<&str>, exclusions: &[String]) -> Vec<String> {
    let mut args = vec![
        "search".to_owned(),
        "prs".to_owned(),
        "--review-requested=@me".to_owned(),
        "--state=open".to_owned(),
        "--draft=false".to_owned(),
        "--limit".to_owned(),
        max.wrapping_add(1).to_string(),
        "--json".to_owned(),
        "number,repository,title,url,author".to_owned(),
    ];
    if let Some(scope) = scope {
        args.extend(scope_args(scope));
    }
    args.extend(query_args(exclusions));
    args
}

/// Decodes the search of requested reviews in every repository and keeps the results whose URL
/// names their repository and number (`ListReviewRequests`).
///
/// `truncated` is decided on the count that GitHub returned, before any result is skipped.
fn decode_review_request_search(
    output: &str,
    max: i64,
    host: &str,
) -> Result<ReviewRequestList, Error> {
    let entries: Vec<Option<ReviewRequestItem>> =
        serde_json::from_str(output).map_err(|error| response_invalid(Some(Box::new(error))))?;
    let items = null_entries::zero_entries(entries);
    let count = i64::try_from(items.len()).unwrap_or(i64::MAX);
    let mut requests = Vec::new();
    for item in &items {
        let repository = repository_name(&item.repository).to_ascii_lowercase();
        if !repository_valid(&repository)
            || item.number <= 0
            || !pull_request_url_valid(&item.url, host, &repository, item.number)
        {
            tracing::warn!(
                repository = %repository,
                pull_request = item.number,
                "review request skipped: unexpected search result"
            );
            continue;
        }
        let author = item
            .author
            .as_ref()
            .map_or(String::new(), |value| value.login.clone());
        let url = format!("https://{host}/{repository}/pull/{}", item.number);
        requests.push(ReviewRequest {
            repository,
            number: item.number,
            title: item.title.clone(),
            author,
            url,
        });
    }
    Ok(ReviewRequestList {
        requests,
        truncated: count > max,
    })
}

/// Returns the search query of a label trigger in the match patterns `scopes`, without the
/// negative qualifiers `exclusions`.
///
/// The label is placed in double quotes without escaping. The policy parser admits only the
/// label pattern, which contains nothing that would need escaping.
fn label_search_query(label: &str, scopes: &[String], exclusions: &[String]) -> String {
    let mut qualifiers = Policy::search_qualifiers(scopes);
    qualifiers.extend(exclusions.iter().cloned());
    format!(
        "is:pr is:open draft:false label:\"{label}\" {} sort:created-asc",
        qualifiers.join(" ")
    )
}

/// Returns the argv of one page of the label search, with `after` only past the first page.
fn label_search_args(query: &str, after: &str) -> Vec<String> {
    let mut args = vec![
        "api".to_owned(),
        "graphql".to_owned(),
        "-f".to_owned(),
        format!("query={}", graphql_search()),
        "-f".to_owned(),
        format!("searchQuery={query}"),
    ];
    if !after.is_empty() {
        args.push("-f".to_owned());
        args.push(format!("after={after}"));
    }
    args
}

/// Returns the argv of the mark-ready mutation of the pull request with node identifier `id`.
fn mark_ready_args(id: &str) -> Vec<String> {
    vec![
        "api".to_owned(),
        "graphql".to_owned(),
        "-f".to_owned(),
        format!("query={GRAPHQL_MARK_READY}"),
        "-f".to_owned(),
        format!("id={id}"),
    ]
}

/// Decodes one page of the label search (`labelCandidates`, one page).
fn decode_label_search_page(output: &str) -> Result<LabelSearchPage, Error> {
    let response: SearchResponse =
        serde_json::from_str(output).map_err(|error| response_invalid(Some(Box::new(error))))?;
    if response.data.search.nodes.is_none() || !response.errors.is_empty() {
        return Err(response_invalid(None));
    }
    let nodes = match response.data.search.nodes {
        Some(value) => value,
        None => return Err(response_invalid(None)),
    };
    Ok(LabelSearchPage {
        nodes: null_entries::zero_entries(nodes),
        has_next_page: response.data.search.page_info.has_next_page,
        end_cursor: response.data.search.page_info.end_cursor.clone(),
    })
}

/// Checks the response of the mark-ready mutation: no error, and a pull request that is no longer
/// a draft.
fn decode_mark_ready_response(output: &str) -> Result<(), Error> {
    let response: MarkReadyResponse =
        serde_json::from_str(output).map_err(|error| response_invalid(Some(Box::new(error))))?;
    if response.errors.is_empty()
        && let Some(mark) = response.data.mark
        && !mark.pull_request.is_draft
    {
        return Ok(());
    }
    Err(response_invalid(None))
}

impl GitHub {
    /// Returns the pull requests that the policy covers and the ones whose data was refused
    /// (`ListCandidates`), searched with `credential` where it serves repositories.
    pub(crate) async fn list_candidates(
        &self,
        cancel: &CancellationToken,
        policy: &Policy,
        credential: &Credential,
    ) -> Result<CandidateList, Error> {
        let mut found: BTreeMap<String, Candidate> = BTreeMap::new();
        let mut rejected: BTreeMap<String, RejectedCandidate> = BTreeMap::new();
        let credentials = &self.credentials;
        let scopes = credentials.search_scopes(credential, &policy.review_request_scopes());
        if !scopes.is_empty() {
            let requested = self
                .review_requested_candidates(cancel, policy, credential, &scopes)
                .await?;
            collect(&mut found, &mut rejected, requested);
        }
        for label in policy.labels() {
            let scopes = credentials.search_scopes(credential, &policy.label_matches(&label));
            if scopes.is_empty() {
                continue;
            }
            let labelled =
                (self.label_candidates(cancel, policy, credential, &label, &scopes)).await?;
            collect(&mut found, &mut rejected, labelled);
        }
        let candidates = found.values().cloned().collect();
        let rejected = rejected
            .into_iter()
            .filter_map(|(key, item)| {
                if found.contains_key(&key) {
                    None
                } else {
                    Some(item)
                }
            })
            .collect();
        Ok(CandidateList {
            candidates,
            rejected,
        })
    }

    /// Marks the drafts of the policy's ready-draft authors that ask `credential`'s login for a
    /// review as ready for review (`ReadyDrafts`), searched where `credential` serves.
    pub(crate) async fn ready_drafts(
        &self,
        cancel: &CancellationToken,
        policy: &Policy,
        credential: &Credential,
    ) -> Result<Vec<ReadiedDraft>, Error> {
        let scopes = (self.credentials).search_scopes(credential, &policy.review_request_scopes());
        let mut readied: Vec<ReadiedDraft> = Vec::new();
        if scopes.is_empty() {
            return Ok(readied);
        }
        for author in policy.ready_draft_authors() {
            let extra = vec![
                "--draft=true".to_owned(),
                "--author".to_owned(),
                format!("app/{author}"),
            ];
            let identities = (self.search_requested(cancel, credential, &scopes, &extra)).await?;
            for identity in &identities {
                let repository = repository_name(&identity.repository).to_ascii_lowercase();
                let number = identity.number;
                let Some(rule) = policy.resolve(&repository) else {
                    continue;
                };
                if !self.owns(credential, &repository) {
                    continue;
                }
                if rule.trigger.kind != TriggerKind::ReviewRequested {
                    continue;
                }
                let raw = self
                    .fetch_pull_request(cancel, credential, &repository, number)
                    .await?;
                let login = &credential.login;
                let Some(id) = ready_draft_id(&raw, author, &repository, number, login) else {
                    continue;
                };
                let args = mark_ready_args(&id);
                let output = self.github::<()>(cancel, credential, &args, None).await?;
                decode_mark_ready_response(&output)?;
                tracing::info!(
                    repository = %repository,
                    pull_request = number,
                    author = %author,
                    "draft marked ready for review"
                );
                readied.push(ReadiedDraft {
                    repository,
                    number,
                    author: author.clone(),
                });
            }
        }
        Ok(readied)
    }

    /// Searches the repositories of `credential`'s patterns for requested reviews
    /// (`ListReviewRequests`).
    pub(crate) async fn list_review_requests(
        &self,
        cancel: &CancellationToken,
        credential: &Credential,
        max: i64,
    ) -> Result<ReviewRequestList, Error> {
        let matches = &credential.matches;
        if matches.iter().any(|pattern| pattern == "*") {
            let exclusions = self.credentials.search_exclusions(credential, matches);
            let args = list_review_requests_args(max, None, &exclusions);
            self.wait_for_search_slot(cancel, credential).await;
            let output = self.github::<()>(cancel, credential, &args, None).await?;
            return decode_review_request_search(&output, max, &self.host);
        }
        let mut lists = Vec::new();
        for scope in independent_scopes(matches) {
            let exclusions = self
                .credentials
                .search_exclusions(credential, std::slice::from_ref(&scope));
            let args = list_review_requests_args(max, Some(&scope), &exclusions);
            self.wait_for_search_slot(cancel, credential).await;
            let output = self.github::<()>(cancel, credential, &args, None).await?;
            let list = decode_review_request_search(&output, max, &self.host)?;
            tracing::debug!(
                credential = %credential.name,
                scope = %scope,
                found = list.requests.len(),
                "review request queue search finished"
            );
            lists.push(list);
        }
        Ok(merge_review_requests(lists, max))
    }

    /// Runs the review request search of each of `scopes` with the `extra` flags and
    /// `credential`'s token, and returns the results without repeats (`searchRequested`).
    ///
    /// Each independent scope is one `gh` call, so no call holds an OR group of scopes.
    async fn search_requested(
        &self,
        cancel: &CancellationToken,
        credential: &Credential,
        scopes: &[String],
        extra: &[String],
    ) -> Result<Vec<SearchIdentity>, Error> {
        let mut lists = Vec::new();
        for scope in independent_scopes(scopes) {
            let exclusions = self
                .credentials
                .search_exclusions(credential, std::slice::from_ref(&scope));
            let args = search_requested_args(self.poll_limit, &scope, extra, &exclusions);
            self.wait_for_search_slot(cancel, credential).await;
            let output = self.github::<()>(cancel, credential, &args, None).await?;
            let identities = decode_search_identities(&output, self.poll_limit)?;
            tracing::debug!(
                credential = %credential.name,
                scope = %scope,
                found = identities.len(),
                "review request search finished"
            );
            lists.push(identities);
        }
        Ok(merge_identities(lists))
    }

    /// Returns the candidates that request a review from the login in the scopes of the policy
    /// (`reviewRequestedCandidates`).
    async fn review_requested_candidates(
        &self,
        cancel: &CancellationToken,
        policy: &Policy,
        credential: &Credential,
        scopes: &[String],
    ) -> Result<CandidateList, Error> {
        let draft = ["--draft=false".to_owned()];
        let identities = (self.search_requested(cancel, credential, scopes, &draft)).await?;
        let mut list = CandidateList::default();
        let trigger = Trigger {
            kind: TriggerKind::ReviewRequested,
            label: String::new(),
        };
        for identity in &identities {
            let repository = repository_name(&identity.repository).to_ascii_lowercase();
            let number = identity.number;
            let Some(covered) = covering(policy, &repository, &trigger) else {
                tracing::info!(
                    repository = %repository,
                    pull_request = number,
                    "requested review is not covered by the policy"
                );
                continue;
            };
            if !repository_valid(&repository) || number <= 0 {
                tracing::info!(
                    repository = %repository,
                    pull_request = number,
                    "requested review is not covered by the policy"
                );
                continue;
            }
            if !self.owns(credential, &repository) {
                continue;
            }
            let outcome = self
                .get_pull_request(cancel, credential, &repository, number, &trigger)
                .await;
            classify(&mut list, &repository, number, covered, outcome)?;
        }
        Ok(list)
    }

    /// Returns the candidates of one label trigger in `scopes`, reading the label search page
    /// by page with `credential`'s token (`labelCandidates`).
    async fn label_candidates(
        &self,
        cancel: &CancellationToken,
        policy: &Policy,
        credential: &Credential,
        name: &str,
        scopes: &[String],
    ) -> Result<CandidateList, Error> {
        let exclusions = self.credentials.search_exclusions(credential, scopes);
        let query = label_search_query(name, scopes, &exclusions);
        let mut after = String::new();
        let mut list = CandidateList::default();
        let trigger = Trigger {
            kind: TriggerKind::Label,
            label: name.to_owned(),
        };
        for _ in 0..self.search_page_limit {
            let args = label_search_args(&query, &after);
            let output = self.github::<()>(cancel, credential, &args, None).await?;
            let page = decode_label_search_page(&output)?;
            for node in &page.nodes {
                let (repository, number) = node.repository_and_number();
                let Some(covered) = covering(policy, &repository, &trigger) else {
                    continue;
                };
                if !self.owns(credential, &repository) {
                    continue;
                }
                let outcome = decode_pull_request(node, &self.host, &credential.login, &trigger);
                classify(&mut list, &repository, number, covered, outcome)?;
            }
            if !page.has_next_page {
                return Ok(list);
            }
            after = page.end_cursor.clone();
            if after.is_empty() {
                return Err(response_invalid(None));
            }
        }
        Err(Error::workflow("github_search_limit_exceeded", true, None))
    }

    /// Reports whether `credential` serves `repository`; another credential's result is dropped.
    ///
    /// The negative qualifiers leave the earlier credentials' repositories out of the search,
    /// so they do not count toward its limits; this check drops any other credential's result.
    fn owns(&self, credential: &Credential, repository: &str) -> bool {
        (self.credentials.serving(repository)).is_ok_and(|serving| serving.name == credential.name)
    }
}

#[cfg(test)]
mod tests {
    use autoreview_engine::model::{CandidateList, PullRequest};
    use autoreview_policy::{Authors, Trigger, TriggerKind};

    use super::{GitHubPullRequest, classify, covering, decode_pull_request};

    /// A policy whose first and second rule both match `acme/special` and `solo/special`.
    const OVERLAPPING_POLICY: &str = "version: 1
reviewers:
  opus: {protocol: claude-structured, agent_profile: claude-profile, label: Opus,
    finding_prefix: O}
pipelines:
  main: {reviewers: [opus], incremental_reviewers: [opus], prompt: prompt.md, checkout: base,
    project_sources: [manual], repository_review_rules: ignore}
repositories:
  - {match: acme/special, trigger: review-requested, pipeline: main, on_findings: comment,
    on_clean: comment, authors: {trusted: {logins: [maintainer]}, untrusted: skip}}
  - {match: acme/*, trigger: review-requested, pipeline: main, on_findings: comment,
    on_clean: comment, authors: {trusted: any}}
  - {match: solo/special, trigger: review-requested, pipeline: main, on_findings: comment,
    on_clean: comment, authors: {trusted: {logins: [maintainer]}, untrusted: skip}}
  - {match: solo/*, trigger: \"label:ai:review\", pipeline: main, on_findings: comment,
    on_clean: comment, authors: {trusted: any}}
unmatched: skip
approval: {timeout_hours: 1}
notifications:
  approval_requested: action
  findings_on_own_pr: action
  review_stopped: action
  cleanup_manual: action
  command_failed: action
  approved: digest
  changes_requested: digest
  commented: digest
  approval_expired: digest
  started: log
  retry_scheduled: log
  superseded: log
  handoff_reminder: action
";

    /// Review-requested trigger, the kind of the rule that covers `acme/*`.
    fn review_requested() -> Trigger {
        Trigger {
            kind: TriggerKind::ReviewRequested,
            label: String::new(),
        }
    }

    // A candidate's authors section comes from the match that gave its rule: the first rule
    // that matches, never a later rule that matches as well or has the searched trigger (#228).
    #[test]
    fn candidate_carries_the_authors_section_of_its_own_rule() {
        let policy = autoreview_policy::parse(OVERLAPPING_POLICY.as_bytes(), |_| {
            Ok(b"Review the pull request.".to_vec())
        })
        .expect("the overlapping policy parses");
        let label = Trigger {
            kind: TriggerKind::Label,
            label: "ai:review".to_owned(),
        };
        let other_label = Trigger {
            kind: TriggerKind::Label,
            label: "other".to_owned(),
        };
        let cases = [
            (
                "acme/special",
                review_requested(),
                Some(("acme/special", "maintainer")),
            ),
            ("acme/widget", review_requested(), Some(("acme/*", "any"))),
            ("solo/special", label.clone(), None),
            ("solo/tool", label, Some(("solo/*", "any"))),
            ("solo/tool", other_label, None),
            ("solo/tool", review_requested(), None),
            ("other/repository", review_requested(), None),
        ];
        for (repository, trigger, want) in cases {
            let covered = covering(&policy, repository, &trigger);
            let mut list = CandidateList::default();
            if let Some(covered) = covered {
                let pull_request = PullRequest {
                    repository: repository.to_owned(),
                    number: 1,
                    ..PullRequest::default()
                };
                classify(&mut list, repository, 1, covered, Ok(pull_request))
                    .expect("a read pull request is kept");
            }
            let got = list.candidates.first().map(|candidate| {
                let trusted = match &candidate.authors {
                    Some(Authors::Any) => "any".to_owned(),
                    Some(Authors::Restricted { trusted, .. }) => trusted.logins.join(","),
                    None => "none".to_owned(),
                };
                (candidate.rule.match_glob.clone(), trusted)
            });
            let got = got
                .as_ref()
                .map(|(glob, trusted)| (glob.as_str(), trusted.as_str()));
            assert_eq!(got, want, "{repository} {trigger}");
            assert!(
                covered.is_none_or(|(rule, _)| policy.resolve(repository) == Some(rule)),
                "{repository}: the pair is not the rule that resolve returns"
            );
        }
    }

    // An unsupported base branch name gets its own code (#446). An empty one stays a
    // malformed response, and neither stops the search.
    #[test]
    fn unsupported_base_branch_is_rejected_with_its_own_code() {
        let policy = autoreview_policy::parse(OVERLAPPING_POLICY.as_bytes(), |_| {
            Ok(b"Review the pull request.".to_vec())
        })
        .expect("the overlapping policy parses");
        let label = Trigger {
            kind: TriggerKind::Label,
            label: "ai:review".to_owned(),
        };
        let covered = covering(&policy, "solo/tool", &label).expect("solo/* covers the label");
        let longest = "a".repeat(255);
        let too_long = "a".repeat(256);
        let cases = [
            ("main", None),
            ("release/2.0_rc-1", None),
            (longest.as_str(), None),
            ("", Some("github_response_invalid")),
            (too_long.as_str(), Some("base_branch_unsupported")),
            ("release..next", Some("base_branch_unsupported")),
            ("feature x", Some("base_branch_unsupported")),
            ("feature\nx", Some("base_branch_unsupported")),
            ("feature\u{2028}x", Some("base_branch_unsupported")),
            ("caf\u{e9}", Some("base_branch_unsupported")),
        ];
        for (base, want) in cases {
            let node = serde_json::json!({
                "id": "PR_node", "number": 7, "title": "Add widget",
                "url": "https://github.com/solo/tool/pull/7", "state": "OPEN",
                "isDraft": false, "isCrossRepository": false,
                "headRefOid": "a".repeat(40), "headRefName": "feature",
                "baseRefOid": "b".repeat(40), "baseRefName": base,
                "author": {"__typename": "User", "login": "colleague"},
                "labels": {"nodes": [{"name": "ai:review"}]},
                "repository": {"nameWithOwner": "solo/tool"},
            });
            let raw: GitHubPullRequest = serde_json::from_value(node).expect("the node decodes");
            let outcome = decode_pull_request(&raw, "github.com", "example-user", &label);
            let mut list = CandidateList::default();
            classify(&mut list, "solo/tool", 7, covered, outcome)
                .expect("a refused pull request never fails the search");
            let got = list.rejected.first().map(|rejected| rejected.code.as_str());
            assert_eq!(got, want, "{base:?}");
            assert_eq!(list.candidates.is_empty(), want.is_some(), "{base:?}");
        }
    }
}
