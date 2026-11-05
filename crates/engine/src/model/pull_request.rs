//! Pull requests, discovery candidates and the stored review input.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeMap;

use autoreview_policy::{Event, Level, Reviewer, Rule};
use serde::{Deserialize, Serialize, Serializer};

/// Scope mode of a review that reads the whole pull request.
#[expect(dead_code, reason = "called by S22")]
pub(crate) const SCOPE_FULL: &str = "full";

/// Scope mode of a review of the commits since the last published head.
pub(crate) const SCOPE_INCREMENTAL: &str = "incremental";

/// A pull request head as the stored review input names it.
///
/// The scheduler reads `repository`, `number`, `head_sha` and `review_request_id` from it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct PullRequest {
    /// Repository as `owner/name`.
    pub repository: String,
    /// Pull request number.
    pub number: i64,
    /// Head commit SHA.
    pub head_sha: String,
    /// Review request identifier.
    pub review_request_id: i64,
    /// Pull request title.
    pub title: String,
    /// Pull request URL.
    pub url: String,
    /// Login of the author.
    pub author: String,
    /// Marks an app author, such as a coding agent.
    ///
    /// Such an author never decides on notes handed to it.
    #[serde(skip_serializing_if = "is_false")]
    pub author_bot: bool,
    /// Head branch name.
    pub head_branch: String,
    /// Base branch name.
    pub base_branch: String,
    /// Base commit SHA.
    pub base_sha: String,
    /// The pull request's labels when it was read.
    #[serde(
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "autoreview_go_compat::null_default::deserialize"
    )]
    pub labels: Vec<String>,
}

autoreview_go_compat::object_only_serde!(PullRequest);

/// A pull request the policy covers, found by its rule's trigger.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Candidate {
    /// The pull request head.
    pub pull_request: PullRequest,
    /// The policy rule that covers the repository.
    pub rule: Rule,
}

/// A pull request a trigger found but whose data the engine refuses.
///
/// An example is a response that fails validation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RejectedCandidate {
    /// Repository as `owner/name`.
    pub repository: String,
    /// Pull request number.
    pub number: i64,
    /// Error code of the refusal.
    pub code: String,
}

/// The result of one discovery search. A rejected pull request does not hold up the others.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CandidateList {
    /// Pull requests to review.
    pub candidates: Vec<Candidate>,
    /// Pull requests whose data was refused.
    pub rejected: Vec<RejectedCandidate>,
}

/// Whether a review reads the whole pull request or only its newest commits.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct ReviewScope {
    /// Scope mode, `full` or `incremental`.
    pub mode: String,
    /// Head of the last published review, set for an incremental review.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub previous_head_sha: String,
}

autoreview_go_compat::object_only_serde!(ReviewScope);

impl ReviewScope {
    /// Reports whether the scope mode is `incremental`.
    #[must_use]
    pub fn incremental(&self) -> bool {
        self.mode == SCOPE_INCREMENTAL
    }
}

/// The pohunek host and project that run the reviewer sessions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct ProjectRoute {
    /// Pohunek host name.
    pub host: String,
    /// Pohunek project identifier.
    pub project_id: String,
}

autoreview_go_compat::object_only_serde!(ProjectRoute);

/// Everything the workflow decides by.
///
/// A policy or configuration change never alters a review that is already running.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct ReviewInput {
    /// Contract version of the input.
    pub version: i64,
    /// The pull request head under review.
    pub pull_request: PullRequest,
    /// Attempt number of the review identity.
    pub attempt: i64,
    /// The policy rule the review runs under.
    pub rule: Rule,
    /// Reviewers that run this review.
    #[serde(
        serialize_with = "nil_when_empty",
        deserialize_with = "autoreview_go_compat::null_default::deserialize"
    )]
    pub reviewers: Vec<Reviewer>,
    /// Scope of the review.
    pub scope: ReviewScope,
    /// Route of the reviewer sessions.
    pub route: ProjectRoute,
    /// Notification level of each event.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    pub notifications: BTreeMap<Event, Level>,
    /// How long an approval waits for the user, in seconds.
    pub approval_timeout_seconds: i64,
    /// Digest of the policy the review was dispatched with.
    pub policy_digest: String,
    /// GitHub login that publishes the review.
    pub publisher_login: String,
    /// Loop limits of the review.
    pub limits: RunLimits,
    /// Blocking findings that earlier reviews left open. A full review carries them too.
    #[serde(
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "autoreview_go_compat::null_default::deserialize"
    )]
    pub prior: Vec<PriorFinding>,
    /// The published review that handed non-blocking findings to the author.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handoff: Option<NotesHandoff>,
}

autoreview_go_compat::object_only_serde!(ReviewInput);

impl ReviewInput {
    /// Reports whether the review takes the stored result of its handoff.
    #[expect(dead_code, reason = "called by S28")]
    pub(crate) fn adopts_handoff(&self) -> bool {
        self.handoff.as_ref().is_some_and(|handoff| handoff.adopt)
    }
}

/// A published review that handed non-blocking findings to the author.
///
/// Adopt means the head is unchanged and its result is stored, so the review reuses it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct NotesHandoff {
    /// Workflow ID of the published review.
    pub workflow_id: String,
    /// Head SHA of the published review.
    pub head_sha: String,
    /// Takes the stored result instead of running the reviewers again.
    #[serde(skip_serializing_if = "is_false")]
    pub adopt: bool,
}

autoreview_go_compat::object_only_serde!(NotesHandoff);

/// A blocking finding of an earlier review.
///
/// The key is unique across reviews, because each review numbers its findings from 1 again.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct PriorFinding {
    /// Key that is unique across reviews.
    pub key: String,
    /// Finding ID within its review.
    pub id: String,
    /// Head SHA the finding was made on.
    pub head_sha: String,
    /// GitHub review that published the finding.
    pub github_review_id: i64,
    /// Severity of the finding.
    pub severity: String,
    /// File path of the finding.
    pub path: String,
    /// Line of the finding.
    pub line: i64,
    /// Finding title.
    pub title: String,
    /// Finding body.
    pub body: String,
}

autoreview_go_compat::object_only_serde!(PriorFinding);

/// Bounds of the loops of one review, in milliseconds.
///
/// They are part of the input, so a resumed review keeps its limits after a config change.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct RunLimits {
    /// Timeout of one reviewer session.
    pub session_timeout_ms: i64,
    /// Interval between output polls.
    pub output_poll_ms: i64,
    /// Attempts to start a session.
    pub session_start_attempts: i64,
    /// Delay between session start attempts.
    pub session_start_retry_ms: i64,
    /// Attempts to clean up a session.
    pub session_cleanup_attempts: i64,
    /// Delay between session cleanup attempts.
    pub session_cleanup_retry_ms: i64,
    /// Attempts to run a reviewer.
    pub reviewer_run_attempts: i64,
    /// Delay between reviewer run attempts.
    pub reviewer_run_retry_ms: i64,
    /// Delay before a failed step is retried.
    pub step_retry_ms: i64,
    /// First delay of the step retry backoff.
    pub step_retry_initial_ms: i64,
    /// Largest delay of the step retry backoff.
    pub step_retry_max_interval_ms: i64,
}

autoreview_go_compat::object_only_serde!(RunLimits);

/// Reports whether a flag is unset, which Go omits from the JSON.
fn is_false(value: &bool) -> bool {
    !*value
}

/// Writes an empty list as JSON null, which Go writes for the nil reviewer list.
fn nil_when_empty<T: Serialize, S: Serializer>(
    value: &[T],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    if value.is_empty() {
        serializer.serialize_none()
    } else {
        value.serialize(serializer)
    }
}

#[cfg(test)]
mod tests {
    use super::PullRequest;

    #[test]
    fn pull_request_refuses_json_arrays() {
        let positional = r#"["acme/widget", 7, "0123", 9, "", "", "", false, "", "", "", []]"#;
        for raw in ["[]", positional] {
            let error = serde_json::from_str::<PullRequest>(raw).expect_err("array is refused");
            assert!(error.to_string().contains("PullRequest"), "{error}");
        }
        let empty: PullRequest = serde_json::from_str("{}").expect("empty object");
        assert_eq!(empty, PullRequest::default());
    }
}
