//! Side effects of the review engine behind one trait, and its session requests.
//!
//! The engine reaches GitHub, pohunek and Matrix only through [`Integrations`]. Its methods mirror
//! `cmd/autoreview/integrations.go:17-44`.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeMap;
use std::future::Future;

use autoreview_policy::{Checkout, Level, Permission, Protocol, Reviewer, Trigger};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::config::NotifyRoute;
use crate::error::Error;
use crate::model::{
    CandidateList, Decision, Discussion, PriorFinding, ProjectRoute, PublishResult, PullRequest,
    SessionIdentity,
};

/// A draft that discovery marked ready for review.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadiedDraft {
    /// Repository as `owner/name`.
    pub repository: String,
    /// Pull request number.
    pub number: i64,
    /// Login of the author.
    pub author: String,
}

/// The state of a pull request that the hand-off reminder and the queue read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PullRequestState {
    /// Whether the pull request is still open.
    pub open: bool,
    /// Whether the login is requested again.
    pub requested: bool,
    /// Pull request title.
    pub title: String,
    /// Pull request URL.
    pub url: String,
    /// Login of the author.
    pub author: String,
    /// Head commit SHA.
    pub head_sha: String,
}

/// RIGHT-side lines of each changed file that accept an inline comment, by path.
pub type CommentableLines = BTreeMap<String, Vec<i64>>;

/// One inline comment of a review submission.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct InlineComment {
    /// Path of the commented file.
    pub path: String,
    /// Line the comment is attached to.
    pub line: i64,
    /// Side of the diff the line is on.
    pub side: String,
    /// Text of the comment.
    pub body: String,
}

autoreview_go_compat::object_only_serde!(InlineComment);

/// One review submission. The marker makes the submission idempotent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct GitHubReview {
    /// GitHub review event of the submission.
    pub decision: Decision,
    /// Body text of the review.
    pub body: String,
    /// Inline comments of the review.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    pub comments: Vec<InlineComment>,
    /// Hidden marker that identifies the review.
    pub marker: String,
}

autoreview_go_compat::object_only_serde!(GitHubReview);

/// An open, non-draft pull request that asks the configured login for a review.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewRequest {
    /// Repository as `owner/name`.
    pub repository: String,
    /// Pull request number.
    pub number: i64,
    /// Pull request title.
    pub title: String,
    /// Login of the author.
    pub author: String,
    /// Pull request URL.
    pub url: String,
}

/// The result of a search of requested reviews.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewRequestList {
    /// Review requests found, at most the requested maximum.
    pub requests: Vec<ReviewRequest>,
    /// Whether the search found more than the requested maximum.
    pub truncated: bool,
}

/// The side effects of the review engine: GitHub, pohunek and Matrix.
///
/// Each method returns a `Send` future, so any executor can await it. Each method takes the
/// caller's cancellation token, as each Go method takes the caller's context.
pub trait Integrations {
    /// Returns the pull requests the policy covers and the ones whose data was refused.
    fn list_candidates(
        &self,
        cancel: &CancellationToken,
    ) -> impl Future<Output = Result<CandidateList, Error>> + Send;

    /// Marks the requesting coding agents' drafts ready for review and returns them.
    fn ready_drafts(
        &self,
        cancel: &CancellationToken,
    ) -> impl Future<Output = Result<Vec<ReadiedDraft>, Error>> + Send;

    /// Reads a pull request and checks it against the trigger of its rule.
    fn get_pull_request(
        &self,
        cancel: &CancellationToken,
        repository: &str,
        number: i64,
        trigger: &Trigger,
    ) -> impl Future<Output = Result<PullRequest, Error>> + Send;

    /// Reads a pull request with no trigger check, for notes waiting on their author.
    fn read_pull_request_state(
        &self,
        cancel: &CancellationToken,
        repository: &str,
        number: i64,
    ) -> impl Future<Output = Result<PullRequestState, Error>> + Send;

    /// Returns the repository permission of a pull request author, `None` below write.
    ///
    /// It reads with the credential that serves the repository; a failure is never a
    /// permission.
    fn author_permission(
        &self,
        cancel: &CancellationToken,
        repository: &str,
        login: &str,
    ) -> impl Future<Output = Result<Option<Permission>, Error>> + Send;

    /// Returns the routes of one pohunek host's projects that serve a repository.
    ///
    /// The routes keep the order of the host's project list; an empty list is no match.
    fn host_routes(
        &self,
        cancel: &CancellationToken,
        host: &str,
        repository: &str,
        sources: &[String],
    ) -> impl Future<Output = Result<Vec<ProjectRoute>, Error>> + Send;

    /// Starts the reviewer session, or returns the live session with the same name.
    fn ensure_session(
        &self,
        cancel: &CancellationToken,
        request: &SessionRequest,
    ) -> impl Future<Output = Result<SessionIdentity, Error>> + Send;

    /// Returns the live session of the same name, or `None` when there is none.
    fn find_session(
        &self,
        cancel: &CancellationToken,
        request: &SessionRequest,
    ) -> impl Future<Output = Result<Option<SessionIdentity>, Error>> + Send;

    /// Returns the output of a finished reviewer session, read with `protocol`.
    ///
    /// `None` means that the reviewer still runs, as Go's `nil` does. The bytes are unconverted.
    fn session_output(
        &self,
        cancel: &CancellationToken,
        session: &SessionIdentity,
        protocol: Protocol,
    ) -> impl Future<Output = Result<Option<Vec<u8>>, Error>> + Send;

    /// Removes a reviewer session and confirms that it is gone.
    fn remove_session_confirmed(
        &self,
        cancel: &CancellationToken,
        session: &SessionIdentity,
    ) -> impl Future<Output = Result<(), Error>> + Send;

    /// Returns the lines of the pull request that accept an inline comment.
    fn list_commentable_lines(
        &self,
        cancel: &CancellationToken,
        pull_request: &PullRequest,
    ) -> impl Future<Output = Result<CommentableLines, Error>> + Send;

    /// Returns the paths of the files the pull request deletes.
    fn list_deleted_files(
        &self,
        cancel: &CancellationToken,
        pull_request: &PullRequest,
    ) -> impl Future<Output = Result<Vec<String>, Error>> + Send;

    /// Reads the reply threads of the prior findings and the conversation, bounded in size.
    fn list_discussion(
        &self,
        cancel: &CancellationToken,
        pull_request: &PullRequest,
        prior: &[PriorFinding],
    ) -> impl Future<Output = Result<Discussion, Error>> + Send;

    /// Reports whether `head` extends `previous` by commits that each have one parent.
    fn linear_history(
        &self,
        cancel: &CancellationToken,
        repository: &str,
        previous: &str,
        head: &str,
    ) -> impl Future<Output = Result<bool, Error>> + Send;

    /// Publishes one review of the pull request and returns what GitHub recorded.
    fn publish_review(
        &self,
        cancel: &CancellationToken,
        pull_request: &PullRequest,
        review: &GitHubReview,
    ) -> impl Future<Output = Result<PublishResult, Error>> + Send;

    /// Sends a notification of `level` to the backends of `route`.
    ///
    /// The level decides the mention and whether a failure is dropped.
    fn notify(
        &self,
        cancel: &CancellationToken,
        route: NotifyRoute,
        level: Level,
        message: &str,
    ) -> impl Future<Output = Result<(), Error>> + Send;

    /// Sends a notification of `level` along `route` and returns every failure.
    ///
    /// Unlike [`Integrations::notify`], a `digest` or `log` message is never dropped silently.
    fn notify_unmuted(
        &self,
        cancel: &CancellationToken,
        route: NotifyRoute,
        level: Level,
        message: &str,
    ) -> impl Future<Output = Result<(), Error>> + Send;

    /// Returns the open review requests of the configured login, at most `max` of them.
    fn list_review_requests(
        &self,
        cancel: &CancellationToken,
        max: i64,
    ) -> impl Future<Output = Result<ReviewRequestList, Error>> + Send;

    /// Posts a digest of the queue under the transaction ID.
    fn send_queue_digest(
        &self,
        cancel: &CancellationToken,
        transaction: &str,
        plain: &str,
        formatted: &str,
    ) -> impl Future<Output = Result<(), Error>> + Send;
}

/// One reviewer session of one workflow attempt.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct SessionRequest {
    /// The pull request head the session reviews.
    pub pull_request: PullRequest,
    /// The pohunek host and project that run the session.
    pub route: ProjectRoute,
    /// The reviewer the session runs.
    pub reviewer: Reviewer,
    /// The commit the reviewer worktree starts from.
    pub checkout: Checkout,
    /// Workflow identifier of the attempt.
    pub workflow_id: String,
    /// Counts the sessions of this reviewer in the workflow.
    ///
    /// A reviewer whose output was unusable gets a fresh session with the next run.
    pub run: i64,
    /// Counts the start attempts of one run.
    ///
    /// Pohunek keeps the branch of a removed session, so every start after the first gets its
    /// own branch to begin from the base again.
    #[serde(skip_serializing_if = "is_zero")]
    pub start: i64,
    /// Prompt the reviewer receives.
    pub prompt: String,
    /// Prefix of the session name, from `session.name_prefix`.
    ///
    /// Not serialized: the request keeps the JSON shape of the Go engine.
    #[serde(skip)]
    pub name_prefix: String,
    /// Prefix of the reviewer branch, from `session.branch_prefix`.
    ///
    /// Not serialized: the request keeps the JSON shape of the Go engine.
    #[serde(skip)]
    pub branch_prefix: String,
}

autoreview_go_compat::object_only_serde!(SessionRequest);

impl SessionRequest {
    /// Returns the session name, which pohunek uses to find a live session.
    #[must_use]
    pub fn name(&self) -> String {
        format!("{}{}", self.name_prefix, self.token())
    }

    /// Returns the branch that holds the reviewer worktree.
    #[must_use]
    pub fn branch(&self) -> String {
        format!("{}{}", self.branch_prefix, self.token())
    }

    /// Returns the ref the reviewer worktree starts from.
    #[must_use]
    pub fn base_ref(&self) -> String {
        if self.checkout == Checkout::Head {
            format!("refs/pull/{}/head", self.pull_request.number)
        } else {
            self.pull_request.base_branch.clone()
        }
    }

    fn token(&self) -> String {
        let mut token = format!("{}-{}-run{}", self.workflow_id, self.reviewer.id, self.run);
        if self.start > 1 {
            token.push_str(&format!("-start{}", self.start));
        }
        token
    }
}

fn is_zero(value: &i64) -> bool {
    *value == 0
}

#[cfg(test)]
mod tests {
    use autoreview_policy::{Checkout, Protocol, Reviewer};

    use crate::model::PullRequest;

    use super::SessionRequest;

    fn sha(character: &str) -> String {
        character.repeat(40)
    }

    fn test_pull_request(number: i64) -> PullRequest {
        PullRequest {
            repository: "acme/widget".to_string(),
            number,
            head_sha: sha("a"),
            review_request_id: 7,
            title: "Add widget".to_string(),
            url: format!("https://github.com/acme/widget/pull/{number}"),
            author: "colleague".to_string(),
            head_branch: "feature".to_string(),
            base_branch: "main".to_string(),
            base_sha: sha("b"),
            ..PullRequest::default()
        }
    }

    fn opus() -> Reviewer {
        Reviewer {
            id: "opus".to_string(),
            label: "Opus".to_string(),
            protocol: Protocol::ClaudeStructured,
            agent_profile: "claude-profile".to_string(),
            finding_prefix: "O".to_string(),
        }
    }

    #[test]
    fn session_request_names_are_unique_per_reviewer_run() {
        let mut request = SessionRequest {
            pull_request: test_pull_request(1),
            reviewer: opus(),
            workflow_id: "prr-v1-abc".to_string(),
            run: 1,
            checkout: Checkout::Base,
            ..SessionRequest::default()
        };
        let retry = SessionRequest {
            run: 2,
            ..request.clone()
        };
        assert_ne!(request.name(), retry.name());
        assert_ne!(request.branch(), retry.branch());
        assert_eq!(request.base_ref(), "main");
        request.checkout = Checkout::Head;
        assert_eq!(request.base_ref(), "refs/pull/1/head");
    }
}
