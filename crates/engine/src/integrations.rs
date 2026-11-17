//! Side effects of the review engine behind one trait, and its session requests.
//!
//! The engine reaches GitHub, pohunek and Matrix only through [`Integrations`].

// Rust guideline compliant 2026-06-26

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::time::Duration;

use autoreview_guard::FieldKind;
use autoreview_policy::{Checkout, Permission, Protocol, Reviewer, Trigger};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

use crate::config::{Credential, HostHooksChoice};
use crate::error::Error;
use crate::model::{
    CandidateList, Decision, Discussion, HostCapacity, LinkedIssues, PriorFinding, ProjectRoute,
    PublishResult, PullRequest, SessionIdentity,
};
use crate::notify::Notification;

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

/// The state of a pull request that the hand-off reminder, the queue and outcome checks read.
///
/// A pull request that GitHub does not return is the default state: not open, no head SHA.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PullRequestState {
    /// Whether the pull request is still open.
    pub open: bool,
    /// Whether the pull request was merged.
    pub merged: bool,
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

/// One file that a pull request changes, as the changed-files listing reports it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ChangedFile {
    /// Path of the file, relative to the repository root.
    pub path: String,
    /// Change status, such as `added`, `modified`, `renamed` or `removed`.
    pub status: String,
    /// Lines the pull request adds to the file.
    pub additions: i64,
    /// Lines the pull request deletes from the file.
    pub deletions: i64,
    /// Path before a rename; set only for a renamed file.
    pub previous_path: Option<String>,
}

/// The changed files of a pull request and whether the listing stopped before its end.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ChangedFiles {
    /// The files listed, in listing order.
    pub files: Vec<ChangedFile>,
    /// Set when the listing ended at a limit, so `files` may not be every changed file.
    pub truncated: bool,
}

/// The files changed between two commits, with the lines that accept an inline comment.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RangeFiles {
    /// The changed files, in listing order; never cut short.
    pub changed: ChangedFiles,
    /// RIGHT-side commentable lines of each changed file.
    pub commentable: CommentableLines,
}

/// One file that a commit changes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommitFile {
    /// Path of the file after the commit.
    pub path: String,
    /// Change status, such as `added`, `modified`, `renamed` or `removed`.
    pub status: String,
    /// Path before a rename; set only for a renamed file.
    pub previous_path: Option<String>,
    /// Unified diff of the file; empty for a change without text lines, such as a binary file.
    pub patch: String,
}

/// A commit with its one parent and the files it changes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommitRead {
    /// SHA of the parent commit.
    pub parent: String,
    /// First line of the commit message.
    pub subject: String,
    /// The changed files, never cut short.
    pub files: Vec<CommitFile>,
}

/// What a read of one repository file at a commit found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepositoryFile {
    /// The commit has no file at the path.
    Absent,
    /// The file is UTF-8 text within the size limit.
    Text(String),
    /// The file is larger than the size limit.
    TooLarge,
    /// The file is binary, truncated, not valid UTF-8 or not a regular file.
    NotText,
}

/// One pull request text that the text guard scans.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardText {
    /// Where the text comes from.
    pub kind: FieldKind,
    /// The text; for `FieldKind::Patch` the unified diff of one file.
    pub text: String,
}

/// The texts of the fields that a guard scans, and whether a read left text out.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GuardTexts {
    /// The texts in read order.
    pub texts: Vec<GuardText>,
    /// Set when a read stopped at the byte bound or the page limit, or GitHub omitted a patch.
    pub truncated: bool,
}

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

/// One reaction to an inline comment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommentReaction {
    /// Login of the account that reacted.
    pub login: String,
    /// The reaction as GitHub's REST API names it: `+1`, `-1`, `laugh`, `hooray`, `confused`,
    /// `heart`, `rocket` or `eyes`.
    pub content: String,
}

/// The comment that starts one review thread, with its reactions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThreadComment {
    /// GitHub ID of the review that the comment belongs to.
    pub review_id: i64,
    /// Body text of the comment.
    pub body: String,
    /// Reactions to the comment, all of them.
    pub reactions: Vec<CommentReaction>,
}

/// What one read of a pull request says about its reviews and review threads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PullRequestSignals {
    /// Login of the pull request author; empty for a deleted account.
    pub author: String,
    /// GitHub state of every review, such as `CHANGES_REQUESTED` or `DISMISSED`, by review ID.
    pub reviews: BTreeMap<i64, String>,
    /// The comment that starts each review thread.
    pub threads: Vec<ThreadComment>,
}

/// What GitHub answered to `GET /user` with the token of one credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenAnswer {
    /// GitHub accepted the token.
    Accepted {
        /// Login of the account the token belongs to, in lower case.
        login: String,
        /// When the token expires; `None` when GitHub sent no expiry.
        expires_at: Option<OffsetDateTime>,
    },
    /// GitHub refused the token with HTTP 401: revoked, expired or never valid.
    Rejected,
}

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
/// caller's cancellation token.
pub trait Integrations {
    /// Returns the pull requests the policy covers and the ones whose data was refused.
    ///
    /// It searches with `credential`'s token, only where `credential` serves repositories.
    fn list_candidates(
        &self,
        cancel: &CancellationToken,
        credential: &Credential,
    ) -> impl Future<Output = Result<CandidateList, Error>> + Send;

    /// Marks the requesting coding agents' drafts ready for review and returns them.
    ///
    /// It searches with `credential`'s token, only where `credential` serves repositories.
    fn ready_drafts(
        &self,
        cancel: &CancellationToken,
        credential: &Credential,
    ) -> impl Future<Output = Result<Vec<ReadiedDraft>, Error>> + Send;

    /// Reads a pull request and checks it against the trigger of its rule.
    ///
    /// It reads with `credential`'s token, and the trigger check uses its login.
    fn get_pull_request(
        &self,
        cancel: &CancellationToken,
        credential: &Credential,
        repository: &str,
        number: i64,
        trigger: &Trigger,
    ) -> impl Future<Output = Result<PullRequest, Error>> + Send;

    /// Reads a pull request with no trigger check, for notes waiting on their author.
    ///
    /// It reads with `credential`'s token, and `requested` uses its login.
    fn read_pull_request_state(
        &self,
        cancel: &CancellationToken,
        credential: &Credential,
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

    /// Asks GitHub whom the token of `credential` belongs to and when it expires.
    ///
    /// The call uses that credential's token only.
    fn check_token(
        &self,
        cancel: &CancellationToken,
        credential: &Credential,
    ) -> impl Future<Output = Result<TokenAnswer, Error>> + Send;

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

    /// Returns the state that the `moc.host-state` plugin reports for one pohunek host.
    ///
    /// `timeout` is the daemon's time limit for the plugin's command.
    fn host_capacity(
        &self,
        cancel: &CancellationToken,
        host: &str,
        timeout: Duration,
    ) -> impl Future<Output = Result<HostCapacity, Error>> + Send;

    /// Reports whether one pohunek host fetches an extra ref when a session starts.
    ///
    /// Only an advertised `session_new_fetch_ref_supported` of `true` is support.
    fn fetch_ref_supported(
        &self,
        cancel: &CancellationToken,
        host: &str,
    ) -> impl Future<Output = Result<bool, Error>> + Send;

    /// Returns what one pohunek host reports about the hooks of a head checkout session.
    ///
    /// One `host inspect` call; a failed call is the pohunek failure of the call.
    fn head_checkout_hooks(
        &self,
        cancel: &CancellationToken,
        host: &str,
    ) -> impl Future<Output = Result<HeadCheckoutHooks, Error>> + Send;

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
    /// `None` means that the reviewer still runs. The bytes are unconverted.
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
    ///
    /// It reads with `credential`'s token.
    fn list_commentable_lines(
        &self,
        cancel: &CancellationToken,
        credential: &Credential,
        pull_request: &PullRequest,
    ) -> impl Future<Output = Result<CommentableLines, Error>> + Send;

    /// Returns the files the pull request changes, in listing order.
    ///
    /// A listing that stops at a limit returns what it listed with `truncated` set.
    /// It reads with `credential`'s token.
    fn list_changed_files(
        &self,
        cancel: &CancellationToken,
        credential: &Credential,
        pull_request: &PullRequest,
    ) -> impl Future<Output = Result<ChangedFiles, Error>> + Send;

    /// Reads the reply threads of the prior findings and the conversation, bounded in size.
    ///
    /// It reads with `credential`'s token; the replies of its login are the reviewer's.
    fn list_discussion(
        &self,
        cancel: &CancellationToken,
        credential: &Credential,
        pull_request: &PullRequest,
        prior: &[PriorFinding],
    ) -> impl Future<Output = Result<Discussion, Error>> + Send;

    /// Reads the issues that the pull request closes in its own repository, with full text.
    ///
    /// It reads with `credential`'s token. Issues of other repositories are never read, as one
    /// may be private while the review is public; `other_repositories` tells of them. A
    /// response that GitHub answers only in part is an error, never a shorter list.
    fn list_closing_issues(
        &self,
        cancel: &CancellationToken,
        credential: &Credential,
        pull_request: &PullRequest,
    ) -> impl Future<Output = Result<LinkedIssues, Error>> + Send;

    /// Reads the files that `head` changes against `base`, as the three-dot compare shows them.
    ///
    /// It reads with `credential`'s token. An answer without a file list, a comparison that
    /// GitHub cuts short, or a changed file whose patch GitHub leaves out, is an error, never
    /// a shorter answer.
    fn read_range_files(
        &self,
        cancel: &CancellationToken,
        credential: &Credential,
        repository: &str,
        base: &str,
        head: &str,
    ) -> impl Future<Output = Result<RangeFiles, Error>> + Send;

    /// Reads the file `path` of `repository` at the commit `sha`.
    ///
    /// It reads with `credential`'s token. A file over `max_bytes` bytes is `TooLarge` and its
    /// text is not returned. A commit that the repository does not have is an error, never
    /// `Absent`.
    fn read_repository_file(
        &self,
        cancel: &CancellationToken,
        credential: &Credential,
        repository: &str,
        sha: &str,
        path: &str,
        max_bytes: u64,
    ) -> impl Future<Output = Result<RepositoryFile, Error>> + Send;

    /// Reads the texts of the `fields` that a rule's text guard scans.
    ///
    /// It reads with `credential`'s token and leaves out what `publisher` wrote. Reading stops
    /// once `max_bytes` bytes of text are read or at the page limit, and then sets `truncated`.
    fn read_guard_texts(
        &self,
        cancel: &CancellationToken,
        credential: &Credential,
        pull_request: &PullRequest,
        fields: &BTreeSet<FieldKind>,
        max_bytes: usize,
        publisher: &str,
    ) -> impl Future<Output = Result<GuardTexts, Error>> + Send;

    /// Reports whether `head` extends `previous` by commits that each have one parent.
    fn linear_history(
        &self,
        cancel: &CancellationToken,
        repository: &str,
        previous: &str,
        head: &str,
    ) -> impl Future<Output = Result<bool, Error>> + Send;

    /// Publishes one review of the pull request and returns what GitHub recorded.
    ///
    /// It posts with `credential`'s token and looks for an earlier review of its login.
    fn publish_review(
        &self,
        cancel: &CancellationToken,
        credential: &Credential,
        pull_request: &PullRequest,
        review: &GitHubReview,
    ) -> impl Future<Output = Result<PublishResult, Error>> + Send;

    /// Returns the review with the marker of `review` already on the head, without posting.
    ///
    /// It is the lookup that [`Integrations::publish_review`] makes before it posts.
    /// It reads with `credential`'s token and matches only reviews of its login.
    fn find_published_review(
        &self,
        cancel: &CancellationToken,
        credential: &Credential,
        pull_request: &PullRequest,
        review: &GitHubReview,
    ) -> impl Future<Output = Result<Option<PublishResult>, Error>> + Send;

    /// Reads the review states and the review threads of a pull request.
    ///
    /// It reads with `credential`'s token and never writes. A read that GitHub answers only in
    /// part, or that passes the page limit, is an error, never a shorter answer.
    fn read_pull_request_signals(
        &self,
        cancel: &CancellationToken,
        credential: &Credential,
        repository: &str,
        number: i64,
    ) -> impl Future<Output = Result<PullRequestSignals, Error>> + Send;

    /// Sends `notification` to the backend called `backend`.
    ///
    /// The level decides the mention and whether the backend drops a failure.
    fn notify(
        &self,
        cancel: &CancellationToken,
        backend: &str,
        notification: &Notification,
    ) -> impl Future<Output = Result<(), Error>> + Send;

    /// Sends `notification` to the backend called `backend` and returns every failure.
    ///
    /// Unlike [`Integrations::notify`], a `digest` or `log` message is never dropped silently.
    fn notify_unmuted(
        &self,
        cancel: &CancellationToken,
        backend: &str,
        notification: &Notification,
    ) -> impl Future<Output = Result<(), Error>> + Send;

    /// Returns the open review requests of `credential`'s login, at most `max` of them.
    ///
    /// It searches with `credential`'s token.
    fn list_review_requests(
        &self,
        cancel: &CancellationToken,
        credential: &Credential,
        max: i64,
    ) -> impl Future<Output = Result<ReviewRequestList, Error>> + Send;

    /// Posts a digest of the queue to the backend called `backend` under the transaction ID.
    fn send_queue_digest(
        &self,
        cancel: &CancellationToken,
        backend: &str,
        transaction: &str,
        plain: &str,
        formatted: &str,
    ) -> impl Future<Output = Result<(), Error>> + Send;
}

/// Error code of a head checkout on a host that cannot skip the checkout's own hooks.
pub const REPO_HOOKS_UNSUPPORTED: &str = "review_host_repo_hooks_unsupported";

/// Error code of a head checkout on a host whose own hook scripts would run in its worktree.
pub const HOST_HOOKS_REFUSED: &str = "review_host_hooks_refused";

/// Error code of a head checkout on a host that requires a hook only the checkout has.
pub const HOOK_REQUIRED_SKIPPED: &str = "review_host_hook_required";

/// What a pohunek host reports about the hooks of a head checkout session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeadCheckoutHooks {
    /// Whether the daemon runs a session without the checkout's own hooks.
    ///
    /// Only an advertised `session_new_skip_repo_hooks_supported` of `true` is support.
    pub skip_supported: bool,
    /// Whether `host_hooks` reports no host script for an event that runs in the worktree.
    ///
    /// `false` when a script exists and when the host does not report it.
    pub no_worktree_scripts: bool,
}

impl HeadCheckoutHooks {
    /// Returns the error code that refuses a head checkout on the host, or `None`.
    ///
    /// A host that cannot skip the checkout's hooks is always refused. `choice` decides about
    /// the host's own scripts; `Refuse` also refuses a host that does not report them.
    #[must_use]
    pub fn refusal(self, choice: HostHooksChoice) -> Option<&'static str> {
        if !self.skip_supported {
            return Some(REPO_HOOKS_UNSUPPORTED);
        }
        match choice {
            HostHooksChoice::Allow => None,
            HostHooksChoice::Refuse if self.no_worktree_scripts => None,
            HostHooksChoice::Refuse => Some(HOST_HOOKS_REFUSED),
        }
    }
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
    /// Not serialized: it comes from the configuration, so the request JSON leaves it out.
    #[serde(skip)]
    pub name_prefix: String,
    /// Prefix of the reviewer branch, from `session.branch_prefix`.
    ///
    /// Not serialized: it comes from the configuration, so the request JSON leaves it out.
    #[serde(skip)]
    pub branch_prefix: String,
    /// Whether the session start fetches the pull request head at the trusted head SHA.
    ///
    /// Not serialized: it follows the host's capability at each start attempt.
    #[serde(skip)]
    pub fetch_head: bool,
    /// The ref the session fetches the head from instead of the pull request ref.
    ///
    /// Not serialized: only an eval sets it, to review a head that the pull request ref no
    /// longer points at.
    #[serde(skip)]
    pub fetch_ref: Option<String>,
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
            self.head_ref()
        } else {
            self.pull_request.base_branch.clone()
        }
    }

    /// Returns the ref of the pull request head on the GitHub remote, or the `fetch_ref`
    /// override.
    #[must_use]
    pub fn head_ref(&self) -> String {
        self.fetch_ref
            .clone()
            .unwrap_or_else(|| format!("refs/pull/{}/head", self.pull_request.number))
    }

    /// Reports whether the session starts without the checkout's own hooks.
    ///
    /// A head checkout holds the pull request's code, so its hooks never run.
    #[must_use]
    pub fn skips_repo_hooks(&self) -> bool {
        self.checkout == Checkout::Head
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
    use super::{HOST_HOOKS_REFUSED, HeadCheckoutHooks, REPO_HOOKS_UNSUPPORTED};
    use crate::config::HostHooksChoice::{Allow, Refuse};

    /// A head checkout starts only where pohunek skips the checkout's hooks, and under `refuse`
    /// only where the host reports no script of its own for the worktree (#399).
    #[test]
    fn head_checkout_starts_only_without_the_checkouts_hooks() {
        let cases = [
            (false, false, Refuse, Some(REPO_HOOKS_UNSUPPORTED)),
            (false, true, Refuse, Some(REPO_HOOKS_UNSUPPORTED)),
            (false, false, Allow, Some(REPO_HOOKS_UNSUPPORTED)),
            (false, true, Allow, Some(REPO_HOOKS_UNSUPPORTED)),
            (true, false, Refuse, Some(HOST_HOOKS_REFUSED)),
            (true, true, Refuse, None),
            (true, false, Allow, None),
            (true, true, Allow, None),
        ];
        for (skip_supported, no_worktree_scripts, choice, want) in cases {
            let hooks = HeadCheckoutHooks {
                skip_supported,
                no_worktree_scripts,
            };
            assert_eq!(hooks.refusal(choice), want, "{hooks:?} {choice:?}");
        }
    }
}
