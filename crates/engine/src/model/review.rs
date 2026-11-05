//! Review results, the records of published reviews and the requests that wait on them.

// Rust guideline compliant 2026-06-26

use autoreview_policy::{Event, Level, Trigger};
use serde::{Deserialize, Serialize};
use time::Duration;
use time::OffsetDateTime;
use time::macros::datetime;

use super::finding::{Finding, ReviewerSummary, is_false};
use super::ids::BLOCKING_GATE_VERSION;
use super::pull_request::{PriorFinding, PullRequest, ReviewInput, ReviewScope, RunLimits};

/// Characters of the head SHA that a prior finding key keeps.
const PRIOR_KEY_HEAD_CHARS: usize = 12;

/// Verdict of a prior finding that the code settled as fixed.
pub(crate) const PRIOR_FIXED: &str = "fixed";

/// Verdict of a prior finding that stays open. Only an open one keeps blocking.
pub(crate) const PRIOR_OPEN: &str = "still_open";

/// Verdict of a prior finding that its author answered.
pub(crate) const PRIOR_ANSWERED: &str = "answered";

/// Kind of an approval decision that approves the review.
#[expect(dead_code, reason = "called by S29")]
pub(crate) const APPROVAL_KIND_APPROVE: &str = "approve";

/// Kind of an approval decision that a newer review superseded.
#[expect(dead_code, reason = "called by S29")]
pub(crate) const APPROVAL_KIND_SUPERSEDED: &str = "superseded";

/// Kind of an approval decision whose wait expired.
#[expect(dead_code, reason = "called by S29")]
pub(crate) const APPROVAL_KIND_EXPIRED: &str = "expired";

/// Returns the key of a prior finding, the head cut to 12 characters, a colon and the ID.
///
/// Go's `%.12s` counts runes, so the cut is by `char`, not by byte.
pub fn prior_key(head: &str, id: &str) -> String {
    let short: String = head.chars().take(PRIOR_KEY_HEAD_CHARS).collect();
    format!("{short}:{id}")
}

/// Returns a duration of `value` milliseconds.
///
/// The product wraps on overflow, as Go's `int64` multiplication does.
#[expect(dead_code, reason = "called by S25")]
pub(crate) fn milliseconds(value: i64) -> Duration {
    Duration::nanoseconds(value.wrapping_mul(1_000_000))
}

/// Verdict of one reviewer on a prior finding.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct PriorVerdict {
    /// Key of the prior finding.
    pub key: String,
    /// Outcome of the verdict.
    pub status: String,
    /// Note explaining the verdict.
    pub note: String,
    /// Login of the comment that an answered verdict rests on, empty when the code alone decides.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub relies_on: String,
}

autoreview_go_compat::object_only_serde!(PriorVerdict);

/// Combined verdict on a prior finding.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct PriorResult {
    /// Original prior finding.
    pub finding: PriorFinding,
    /// Verdict on the finding.
    #[serde(flatten)]
    pub verdict: PriorVerdict,
    /// Identifier of the new finding it became.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub republished: String,
    /// Marks an answered verdict that was reopened.
    #[serde(skip_serializing_if = "is_false")]
    pub reopened: bool,
}

autoreview_go_compat::object_only_serde!(PriorResult);

/// Discussion on the pull request since earlier reviews.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct Discussion {
    /// Threads linked to prior findings.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    pub threads: Vec<DiscussionThread>,
    /// Top-level comments on the pull request.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    pub comments: Vec<DiscussionComment>,
}

autoreview_go_compat::object_only_serde!(Discussion);

/// Thread of replies for one inline comment.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct DiscussionThread {
    /// Key of the prior finding this thread answers.
    pub key: String,
    /// Whether the thread is resolved.
    pub resolved: bool,
    /// Replies in the thread.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    pub replies: Vec<DiscussionComment>,
}

autoreview_go_compat::object_only_serde!(DiscussionThread);

/// Single comment in a discussion thread.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct DiscussionComment {
    /// Login of the comment author.
    pub author: String,
    /// Role of the author, `reviewer`, `author` or `other`.
    pub role: String,
    /// Whether the comment was AI signed.
    pub ai_signed: bool,
    /// Body of the comment.
    pub body: String,
}

autoreview_go_compat::object_only_serde!(DiscussionComment);

/// Identity of the session that ran the review.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct SessionIdentity {
    /// Host name of the session runner.
    pub host: String,
    /// Identifier of the session in the session runner.
    pub session_id: String,
    /// Identifier of the worker instance.
    pub worker_instance_id: String,
    /// Runtime generation of the worker.
    pub runtime_generation: String,
    /// Reviewer that ran in the session.
    pub reviewer_id: String,
}

autoreview_go_compat::object_only_serde!(SessionIdentity);

impl RunLimits {
    /// Reports whether every bound is positive and the poll interval fits the session timeout.
    #[expect(dead_code, reason = "called by S25")]
    pub(crate) fn valid(&self) -> bool {
        self.session_timeout_ms > 0
            && self.output_poll_ms > 0
            && self.output_poll_ms <= self.session_timeout_ms
            && self.session_start_attempts > 0
            && self.session_start_retry_ms > 0
            && self.session_cleanup_attempts > 0
            && self.session_cleanup_retry_ms > 0
            && self.reviewer_run_attempts > 0
            && self.reviewer_run_retry_ms > 0
            && self.step_retry_ms > 0
            && self.step_retry_initial_ms > 0
            && self.step_retry_max_interval_ms >= self.step_retry_initial_ms
    }
}

impl ReviewInput {
    /// Returns the notification level of an event, or `None` when the policy names none.
    #[expect(dead_code, reason = "called by S28")]
    pub(crate) fn level(&self, event: Event) -> Option<Level> {
        self.notifications.get(&event).copied()
    }

    /// Reports whether a finding holds the merge back.
    ///
    /// Inputs before the blocking gate version block every finding.
    pub(crate) fn blocks(&self, finding: &Finding) -> bool {
        self.version < BLOCKING_GATE_VERSION || !finding.non_blocking
    }

    /// Splits the findings into those that hold the merge back and the notes.
    pub(crate) fn split_findings(&self, findings: &[Finding]) -> (Vec<Finding>, Vec<Finding>) {
        let mut blocking = Vec::new();
        let mut notes = Vec::new();
        for finding in findings {
            if self.blocks(finding) {
                blocking.push(finding.clone());
            } else {
                notes.push(finding.clone());
            }
        }
        (blocking, notes)
    }
}

/// Combined review of one pull request head.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct Artifact {
    /// Repository of the pull request.
    pub repository: String,
    /// Pull request number.
    pub number: i64,
    /// Head SHA of the reviewed commit.
    pub head_sha: String,
    /// Scope of the review.
    pub scope: ReviewScope,
    /// Summaries of the reviewers.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    pub summaries: Vec<ReviewerSummary>,
    /// Findings of the review.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    pub findings: Vec<Finding>,
    /// Tests reported by the reviewers.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    pub tests: Vec<String>,
    /// Time the review was completed.
    pub reviewed_at: String,
    /// Settled prior findings.
    #[serde(
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "autoreview_go_compat::null_default::deserialize"
    )]
    pub prior_results: Vec<PriorResult>,
    /// Verification run for blocking claims.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification: Option<Verification>,
    /// Human review signals that this review matched.
    #[serde(
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "autoreview_go_compat::null_default::deserialize"
    )]
    pub human_review: Vec<String>,
}

autoreview_go_compat::object_only_serde!(Artifact);

impl Artifact {
    /// Returns the prior findings that this review left open.
    ///
    /// A republished prior finding counts as the new finding it became, so it is left out.
    pub(crate) fn open_prior(&self) -> Vec<PriorResult> {
        self.prior_results
            .iter()
            .filter(|result| result.verdict.status == PRIOR_OPEN && result.republished.is_empty())
            .cloned()
            .collect()
    }
}

/// Result of the verifier on blocking claims.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct Verification {
    /// Label of the verifier.
    pub reviewer_label: String,
    /// Checks for findings.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    pub findings: Vec<FindingCheck>,
    /// Checks for prior findings.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    pub prior: Vec<PriorCheck>,
    /// Set when the verifier did not finish, so every claim stands as the reviewers made it.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub error_code: String,
}

autoreview_go_compat::object_only_serde!(Verification);

/// Verdict of the verifier on a finding.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct FindingCheck {
    /// Identifier of the finding.
    pub id: String,
    /// Verdict of the check.
    pub verdict: String,
    /// Note explaining the verdict.
    pub note: String,
}

autoreview_go_compat::object_only_serde!(FindingCheck);

/// Verdict of the verifier on a prior finding.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct PriorCheck {
    /// Key of the prior finding.
    pub key: String,
    /// Verdict of the check.
    pub verdict: String,
    /// Note explaining the verdict.
    pub note: String,
}

autoreview_go_compat::object_only_serde!(PriorCheck);

/// GitHub review event decision.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Decision {
    /// Approve the pull request.
    #[serde(rename = "APPROVE")]
    Approve,
    /// Request changes on the pull request.
    #[serde(rename = "REQUEST_CHANGES")]
    RequestChanges,
    /// Comment without approval.
    #[serde(rename = "COMMENT")]
    Comment,
    /// Unset decision.
    #[default]
    #[serde(rename = "")]
    Unset,
}

/// Result of publishing a review to GitHub.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct PublishResult {
    /// Decision of the review.
    pub decision: Decision,
    /// GitHub review identifier.
    pub github_review_id: i64,
    /// URL of the published review.
    pub review_url: String,
    /// Whether an existing review was reused.
    pub reused: bool,
}

autoreview_go_compat::object_only_serde!(PublishResult);

/// Last published review for a pull request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct PublishedRecord {
    /// Repository of the pull request.
    pub repository: String,
    /// Pull request number.
    pub number: i64,
    /// Head SHA of the published review.
    pub head_sha: String,
    /// Review request identifier.
    pub review_request_id: i64,
    /// Decision of the published review.
    pub decision: Decision,
    /// Workflow identifier of the publication.
    pub workflow_id: String,
    /// Time the review was published.
    #[serde(with = "autoreview_go_compat::go_time")]
    pub published_at: OffsetDateTime,
    /// Marks a comment that handed findings to the author.
    #[serde(skip_serializing_if = "is_false")]
    pub awaits_author: bool,
    /// Marks that the author was reminded once.
    #[serde(skip_serializing_if = "is_false")]
    pub reminded: bool,
}

autoreview_go_compat::object_only_serde!(PublishedRecord);

impl Default for PublishedRecord {
    fn default() -> Self {
        Self {
            repository: String::default(),
            number: i64::default(),
            head_sha: String::default(),
            review_request_id: i64::default(),
            decision: Decision::default(),
            workflow_id: String::default(),
            published_at: go_zero_time(),
            awaits_author: bool::default(),
            reminded: bool::default(),
        }
    }
}

impl PublishedRecord {
    /// Returns the `repository#number` key shared by every head of the pull request.
    #[must_use]
    pub fn key(&self) -> String {
        format!("{}#{}", self.repository, self.number)
    }

    /// Reports whether this record covers the pull request.
    #[expect(dead_code, reason = "called by S22")]
    pub(crate) fn covers(&self, pr: &PullRequest) -> bool {
        self.head_sha == pr.head_sha
            && !(self.awaits_author && self.review_request_id != pr.review_request_id)
    }
}

/// Clean review that waits for manual approval.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct AwaitRecord {
    /// Repository of the pull request.
    pub repository: String,
    /// Pull request number.
    pub number: i64,
    /// Head SHA of the waiting review.
    pub head_sha: String,
    /// Review request identifier.
    pub review_request_id: i64,
    /// Trigger that waits for approval.
    pub trigger: Trigger,
    /// Workflow identifier of the waiting review.
    pub workflow_id: String,
}

autoreview_go_compat::object_only_serde!(AwaitRecord);

impl AwaitRecord {
    /// Returns the `repository#number` key shared by every head of the pull request.
    #[must_use]
    pub fn key(&self) -> String {
        format!("{}#{}", self.repository, self.number)
    }

    /// Reports whether the pull request matches the wait.
    #[expect(dead_code, reason = "called by S23")]
    pub(crate) fn matches(&self, pr: &PullRequest) -> bool {
        pr.head_sha == self.head_sha
    }
}

/// Outcome of a review publication.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    /// Review was published.
    #[serde(rename = "published")]
    Published,
    /// Review was superseded.
    #[serde(rename = "superseded")]
    Superseded,
    /// Approval wait expired.
    #[serde(rename = "approval_expired")]
    Expired,
    /// Unset outcome.
    #[default]
    #[serde(rename = "")]
    Unset,
}

/// Result of the review publication step.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct ReviewOutcome {
    /// Outcome of the review.
    pub outcome: Outcome,
    /// Publication result when published.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publication: Option<PublishResult>,
}

autoreview_go_compat::object_only_serde!(ReviewOutcome);

/// Approval decision kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalDecision {
    /// Kind of the approval decision.
    pub kind: String,
}

/// Request to approve a waiting review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApproveRequest {
    /// Repository of the pull request.
    pub repository: String,
    /// Pull request number.
    pub number: i64,
    /// Head SHA of the waiting review.
    pub head_sha: String,
}

/// Result of an approval command.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ApprovalResult {
    /// Whether the command succeeded.
    pub ok: bool,
    /// Identifier of the review request.
    pub review_id: String,
    /// Status of the publication.
    pub publication_status: String,
    /// GitHub review identifier.
    #[serde(skip_serializing_if = "is_zero")]
    pub github_review_id: i64,
    /// URL of the published review.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub review_url: String,
}

/// Request to resolve a discovery notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveRequest {
    /// Repository of the pull request.
    pub repository: String,
    /// Pull request number.
    pub number: i64,
    /// Prefix of the head SHA.
    pub head_prefix: String,
}

/// Result of a resolve command.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ResolveResult {
    /// Whether the command succeeded.
    pub ok: bool,
    /// Workflow identifier of the resolved review.
    pub workflow_id: String,
    /// Head SHA of the resolved review.
    pub head_sha: String,
    /// Number of findings in the review.
    pub findings: i64,
}

/// Request to retry a review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryRequest {
    /// Repository of the pull request.
    pub repository: String,
    /// Pull request number.
    pub number: i64,
    /// Head SHA of the review to retry.
    pub head_sha: String,
}

/// Result of a retry command.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RetryResult {
    /// Whether the command succeeded.
    pub ok: bool,
    /// Workflow identifier of the retried review.
    pub workflow_id: String,
    /// Head SHA of the retried review.
    pub head_sha: String,
    /// Attempt number of the retry.
    pub attempt: i64,
}

/// Result of polling the queue.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PollResult {
    /// Number of dispatched reviews.
    pub dispatched: i64,
    /// Number of open reviews.
    pub open: i64,
    /// Number of superseded reviews.
    pub superseded: i64,
}

/// Returns Go's zero `time.Time`, which Go writes for a record without a publication time.
fn go_zero_time() -> OffsetDateTime {
    datetime!(0001-01-01 0:00 UTC)
}

/// Reports whether an integer is zero, which Go omits from the JSON when `omitempty` is set.
fn is_zero(value: &i64) -> bool {
    *value == 0
}
