//! Policy value types, their string enums, the pattern limits of the policy file and the policy
//! digest inputs.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize, Serializer};

use crate::human_review::HumanReview;

/// Largest policy file in bytes.
pub(crate) const MAX_POLICY_BYTES: usize = 256 * 1024;

/// Largest prompt file in bytes.
pub(crate) const MAX_PROMPT_BYTES: usize = 64 * 1024;

/// Most rules a policy file may declare.
pub(crate) const MAX_RULES: usize = 200;

/// Largest approval timeout and notes reminder in hours.
pub(crate) const MAX_APPROVAL_HOURS: i64 = 720;

/// Most reviewers a pipeline may select for one pull request.
pub(crate) const MAX_REVIEWERS_PER_PR: usize = 4;

/// Longest reviewer id or pipeline name in bytes.
const MAX_IDENTIFIER_LEN: usize = 32;

/// Longest agent profile name in bytes.
const MAX_PROFILE_LEN: usize = 128;

/// Longest owner login in bytes.
const MAX_OWNER_LEN: usize = 39;

/// Longest repository name in bytes.
const MAX_REPO_NAME_LEN: usize = 100;

/// Longest label in bytes.
const MAX_LABEL_LEN: usize = 50;

/// Longest project source name in bytes.
const MAX_SOURCE_LEN: usize = 32;

/// Reviewer id that names the verifier's session and state; a reviewer of the policy cannot take
/// it.
pub const VERIFIER_REVIEWER_ID: &str = "verifier";

/// Finding prefix of the prior findings published again.
///
/// No reviewer of the policy may take it, so its findings never collide with them.
pub const REPUBLISHED_PREFIX: &str = "P";

/// Names the input and output contract of a reviewer agent profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Protocol {
    /// Sends a JSON request with an enforced schema and reads the claude-otel-noninteractive
    /// structured result wrapper.
    #[serde(rename = "claude-structured")]
    ClaudeStructured,
    /// Sends a plain prompt and reads the base64 result envelope printed by the pohunek Codex
    /// review wrapper.
    #[serde(rename = "codex-envelope")]
    CodexEnvelope,
    /// The empty value Go writes for an unset field; parsing never produces it.
    #[default]
    #[serde(rename = "")]
    Unset,
}

impl Protocol {
    /// Returns the text Go writes for this value.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeStructured => "claude-structured",
            Self::CodexEnvelope => "codex-envelope",
            Self::Unset => "",
        }
    }
}

/// Selects the commit the reviewer worktree starts from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Checkout {
    /// Starts at the base branch tip, so repository instructions are read from trusted content
    /// before the reviewer fetches the head.
    #[serde(rename = "base")]
    Base,
    /// Starts at refs/pull/<number>/head.
    #[serde(rename = "head")]
    Head,
    /// The empty value Go writes for an unset field; parsing never produces it.
    #[default]
    #[serde(rename = "")]
    Unset,
}

impl Checkout {
    /// Returns the text Go writes for this value.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Head => "head",
            Self::Unset => "",
        }
    }
}

/// Kind of event that starts a review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TriggerKind {
    /// The author or a reviewer requested a review.
    #[serde(rename = "review-requested")]
    ReviewRequested,
    /// A configured label was added to the pull request.
    #[serde(rename = "label")]
    Label,
    /// The empty value Go writes for an unset field; parsing never produces it.
    #[default]
    #[serde(rename = "")]
    Unset,
}

impl TriggerKind {
    /// Returns the text Go writes for this kind.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::ReviewRequested => "review-requested",
            Self::Label => "label",
            Self::Unset => "",
        }
    }
}

/// Decides the outcome of a review that has findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum OnFindings {
    /// Requests changes on the pull request.
    #[serde(rename = "request_changes")]
    RequestChanges,
    /// Posts the findings as a comment.
    #[serde(rename = "comment")]
    Comment,
    /// The empty value Go writes for an unset field; parsing never produces it.
    #[default]
    #[serde(rename = "")]
    Unset,
}

impl OnFindings {
    /// Returns the text Go writes for this value.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::RequestChanges => "request_changes",
            Self::Comment => "comment",
            Self::Unset => "",
        }
    }
}

/// Decides the outcome of a clean review, one without findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum OnClean {
    /// Approves the pull request automatically.
    #[serde(rename = "approve")]
    Approve,
    /// Waits for a human to approve.
    #[serde(rename = "await")]
    Await,
    /// Posts a comment only.
    #[serde(rename = "comment")]
    Comment,
    /// The empty value Go writes for an unset field; parsing never produces it.
    #[default]
    #[serde(rename = "")]
    Unset,
}

impl OnClean {
    /// Returns the text Go writes for this value.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Await => "await",
            Self::Comment => "comment",
            Self::Unset => "",
        }
    }
}

/// Decides a review whose findings are all non-blocking. Unset follows the rule's clean outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum OnNotes {
    /// Hands the notes to the author in a comment first; once the author requests the review again,
    /// the clean outcome follows.
    #[serde(rename = "comment")]
    Comment,
    /// The empty value, which means the rule's clean outcome applies.
    #[default]
    #[serde(rename = "")]
    Unset,
}

impl OnNotes {
    /// Returns the text Go writes for this value.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Comment => "comment",
            Self::Unset => "",
        }
    }
}

/// Level of the Matrix room a notification goes to. Only action messages mention the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Level {
    /// The action room, which mentions the user.
    #[serde(rename = "action")]
    Action,
    /// The digest room.
    #[serde(rename = "digest")]
    Digest,
    /// The log room.
    #[serde(rename = "log")]
    Log,
}

impl Level {
    /// Returns the text Go writes for this value.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Action => "action",
            Self::Digest => "digest",
            Self::Log => "log",
        }
    }
}

/// Notification event. The variants are declared in wire-name order, so the derived Ord sorts map
/// keys as Go sorts them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Event {
    /// Approval expired event.
    #[serde(rename = "approval_expired")]
    ApprovalExpired,
    /// Approval requested event.
    #[serde(rename = "approval_requested")]
    ApprovalRequested,
    /// Approved event.
    #[serde(rename = "approved")]
    Approved,
    /// Changes requested event.
    #[serde(rename = "changes_requested")]
    ChangesRequested,
    /// Cleanup manual event.
    #[serde(rename = "cleanup_manual")]
    CleanupManual,
    /// Command failed event.
    #[serde(rename = "command_failed")]
    CommandFailed,
    /// Commented event.
    #[serde(rename = "commented")]
    Commented,
    /// Findings on own PR event.
    #[serde(rename = "findings_on_own_pr")]
    FindingsOnOwnPr,
    /// Handoff reminder event.
    #[serde(rename = "handoff_reminder")]
    HandoffReminder,
    /// Retry scheduled event.
    #[serde(rename = "retry_scheduled")]
    RetryScheduled,
    /// Review stopped event.
    #[serde(rename = "review_stopped")]
    ReviewStopped,
    /// Started event.
    #[serde(rename = "started")]
    Started,
    /// Superseded event.
    #[serde(rename = "superseded")]
    Superseded,
}

impl Event {
    /// Returns the text Go writes for this value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ApprovalExpired => "approval_expired",
            Self::ApprovalRequested => "approval_requested",
            Self::Approved => "approved",
            Self::ChangesRequested => "changes_requested",
            Self::CleanupManual => "cleanup_manual",
            Self::CommandFailed => "command_failed",
            Self::Commented => "commented",
            Self::FindingsOnOwnPr => "findings_on_own_pr",
            Self::HandoffReminder => "handoff_reminder",
            Self::RetryScheduled => "retry_scheduled",
            Self::ReviewStopped => "review_stopped",
            Self::Started => "started",
            Self::Superseded => "superseded",
        }
    }
}

/// Every notification event; the policy must assign each a level.
pub const EVENTS: &[Event] = &[
    Event::ApprovalRequested,
    Event::FindingsOnOwnPr,
    Event::ReviewStopped,
    Event::CleanupManual,
    Event::CommandFailed,
    Event::Approved,
    Event::ChangesRequested,
    Event::Commented,
    Event::Started,
    Event::RetryScheduled,
    Event::Superseded,
    Event::ApprovalExpired,
    Event::HandoffReminder,
];

/// Events a review workflow sends itself, so its input carries their levels. Discovery sends the
/// others with the current policy.
pub const WORKFLOW_EVENTS: &[Event] = &[
    Event::ApprovalRequested,
    Event::FindingsOnOwnPr,
    Event::ReviewStopped,
    Event::CleanupManual,
    Event::CommandFailed,
    Event::Approved,
    Event::ChangesRequested,
    Event::Commented,
    Event::Started,
    Event::RetryScheduled,
    Event::Superseded,
    Event::ApprovalExpired,
];

/// Starts a review of a rule.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct Trigger {
    /// Kind of the trigger.
    pub kind: TriggerKind,
    /// The label name, set only for the label kind.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub label: String,
}

autoreview_go_compat::object_only_serde!(Trigger);

impl fmt::Display for Trigger {
    /// Formats the trigger as label:<name> for a label trigger and as its kind otherwise.
    /// The policy digest embeds this text, so it must not change.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.kind == TriggerKind::Label {
            write!(f, "label:{}", self.label)
        } else {
            write!(f, "{}", self.kind.as_str())
        }
    }
}

/// A reviewer agent profile as the policy declares it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct Reviewer {
    /// Identifier of the reviewer.
    pub id: String,
    /// Display label of the reviewer.
    pub label: String,
    /// Protocol the reviewer speaks.
    pub protocol: Protocol,
    /// Agent profile name.
    pub agent_profile: String,
    /// Prefix for findings from this reviewer.
    pub finding_prefix: String,
}

autoreview_go_compat::object_only_serde!(Reviewer);

/// The reviewers and prompt that run one review of a rule.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct Pipeline {
    /// Name of the pipeline.
    pub name: String,
    /// Protocol of the pipeline.
    pub protocol: Protocol,
    /// Reviewers that run the review.
    #[serde(
        deserialize_with = "autoreview_go_compat::null_default::deserialize",
        serialize_with = "nil_when_empty"
    )]
    pub reviewers: Vec<Reviewer>,
    /// Reviewers for incremental reviews.
    #[serde(
        deserialize_with = "autoreview_go_compat::null_default::deserialize",
        serialize_with = "nil_when_empty"
    )]
    pub incremental_reviewers: Vec<Reviewer>,
    /// Prompt text for the pipeline.
    pub prompt: String,
    /// Checkout strategy.
    pub checkout: Checkout,
    /// Project sources for the pipeline.
    #[serde(
        deserialize_with = "autoreview_go_compat::null_default::deserialize",
        serialize_with = "nil_when_empty"
    )]
    pub project_sources: Vec<String>,
    /// Checks every blocking finding before it is published. Absent for a pipeline that publishes
    /// its findings unchecked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verifier: Option<Verifier>,
}

autoreview_go_compat::object_only_serde!(Pipeline);

/// Checks every blocking finding of a review before it is published.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct Verifier {
    /// Reviewer that verifies findings.
    pub reviewer: Reviewer,
    /// Prompt for the verifier.
    pub prompt: String,
    /// On a rule that approves clean reviews without the user, the user decides instead once the
    /// verifier demoted a blocking finding of one of these severities, or escalate_count blocking
    /// findings.
    #[serde(
        deserialize_with = "autoreview_go_compat::null_default::deserialize",
        serialize_with = "nil_when_empty"
    )]
    pub escalate_severities: Vec<String>,
    /// Threshold count for escalation.
    pub escalate_count: i64,
}

autoreview_go_compat::object_only_serde!(Verifier);

/// The resolved policy for one repository. It is copied into each review workflow input, so a
/// policy change affects only reviews dispatched after it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct Rule {
    /// Glob over repository names, such as acme/*.
    #[serde(rename = "match")]
    pub match_glob: String,
    /// Trigger for the rule.
    pub trigger: Trigger,
    /// Pipeline for the rule.
    pub pipeline: Pipeline,
    /// Outcome when findings exist.
    pub on_findings: OnFindings,
    /// Outcome for clean reviews.
    pub on_clean: OnClean,
    /// Outcome for notes-only findings.
    #[serde(skip_serializing_if = "is_unset_notes")]
    pub on_notes: OnNotes,
    /// Signals that make a clean review wait for a human. Absent on a rule whose clean reviews need
    /// no signal check before an automatic approval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub human_review: Option<HumanReview>,
}

autoreview_go_compat::object_only_serde!(Rule);

impl Rule {
    /// Reports whether a clean review of this rule can end in an approval, which decides whether a
    /// later review may be incremental.
    #[must_use]
    pub fn approves_clean(&self) -> bool {
        self.on_clean == OnClean::Approve || self.on_clean == OnClean::Await
    }
}

/// The parsed review policy: its rules, notification levels and the digest that identifies it.
#[derive(Debug, Clone)]
pub struct Policy {
    /// The repository rules in file order; the first match wins.
    pub(crate) rules: Vec<Rule>,
    /// Sorted logins whose draft pull requests are marked ready for review.
    pub(crate) ready_draft_authors: Vec<String>,
    /// The level of every notification event.
    pub(crate) notifications: BTreeMap<Event, Level>,
    /// How long a review waits for the user's approval.
    pub(crate) approval_timeout: Duration,
    /// How long notes handed to an author wait before the user is reminded; zero when no rule hands
    /// notes off.
    pub(crate) handoff_reminder: Duration,
    /// Hex SHA-256 digest that identifies the effective policy.
    pub(crate) digest: String,
}

/// Writes an empty list as JSON null, which Go writes for a nil slice. Every list a parsed policy
/// stores is non-empty, so null marks the absent list.
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

/// Reports whether an on_notes value is unset, which Go omits from the JSON.
fn is_unset_notes(value: &OnNotes) -> bool {
    *value == OnNotes::Unset
}

/// Reports whether value is 1 to max_len ASCII bytes whose first byte matches first and whose other
/// bytes match rest.
fn is_token(value: &str, max_len: usize, first: fn(u8) -> bool, rest: fn(u8) -> bool) -> bool {
    if value.is_empty() {
        return false;
    }
    if value.len() > max_len {
        return false;
    }
    let bytes = value.as_bytes();
    if !first(bytes[0]) {
        return false;
    }
    for &byte in &bytes[1..] {
        if !rest(byte) {
            return false;
        }
    }
    true
}

fn is_lower_alnum(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit()
}

fn is_repo_byte(byte: u8) -> bool {
    is_lower_alnum(byte) || byte == b'_' || byte == b'.' || byte == b'-'
}

fn is_ascii_alphanumeric(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
}

fn is_ascii_lowercase(byte: u8) -> bool {
    byte.is_ascii_lowercase()
}

/// Reports whether value is a reviewer id or pipeline name.
pub(crate) fn is_identifier(value: &str) -> bool {
    is_token(value, MAX_IDENTIFIER_LEN, is_lower_alnum, |byte| {
        is_lower_alnum(byte) || byte == b'_' || byte == b'-'
    })
}

/// Reports whether value is an agent profile name.
pub(crate) fn is_profile(value: &str) -> bool {
    is_token(value, MAX_PROFILE_LEN, is_ascii_alphanumeric, |byte| {
        byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'_' || byte == b'-'
    })
}

/// Reports whether value is one uppercase ASCII letter.
pub(crate) fn is_finding_prefix(value: &str) -> bool {
    value.len() == 1 && value.as_bytes()[0].is_ascii_uppercase()
}

/// Reports whether value is a lowercase GitHub login.
pub(crate) fn is_owner(value: &str) -> bool {
    is_token(value, MAX_OWNER_LEN, is_lower_alnum, |byte| {
        is_lower_alnum(byte) || byte == b'-'
    })
}

/// Reports whether value is a repository name without its owner.
pub(crate) fn is_repo_name(value: &str) -> bool {
    is_token(value, MAX_REPO_NAME_LEN, is_repo_byte, is_repo_byte)
}

/// Reports whether value is a label that a human review or a review request may name.
pub(crate) fn is_label(value: &str) -> bool {
    is_token(value, MAX_LABEL_LEN, is_ascii_alphanumeric, |byte| {
        byte.is_ascii_alphanumeric() || byte == b':' || byte == b'.' || byte == b'_' || byte == b'-'
    })
}

/// Reports whether value is a project source name.
pub(crate) fn is_source(value: &str) -> bool {
    is_token(value, MAX_SOURCE_LEN, is_ascii_lowercase, |byte| {
        is_lower_alnum(byte) || byte == b'_' || byte == b'-'
    })
}
