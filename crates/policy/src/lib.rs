//! Review policy parsing, queries, digest, types, glob matching and human review signals.

// Rust guideline compliant 2026-06-26

use std::io;
use std::path::PathBuf;

mod authors;
mod digest;
mod human_review;
mod parse;
mod query;
#[cfg(test)]
mod testing;
mod types;

pub use authors::{
    AuthorClass, AuthorType, Authors, Permission, TrustedAuthors, Untrusted, UntrustedReview,
    login_valid, user_login_valid,
};
pub use human_review::{HumanReview, match_glob};
pub use parse::{load, parse};
pub use types::{
    Checkout, EVENTS, Event, Level, OnClean, OnFindings, OnNotes, Pipeline, Policy, Protocol,
    REPUBLISHED_PREFIX, Reviewer, Rule, Trigger, TriggerKind, VERIFIER_REVIEWER_ID, Verifier,
    WORKFLOW_EVENTS,
};

/// Errors from policy parsing, policy types and human review validation.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Human review needs on_clean approve.
    #[error("human_review needs on_clean approve")]
    HumanReviewNeedsApprove,

    /// Human review must set labels or deleted tests.
    #[error("human_review must set labels or deleted_tests")]
    HumanReviewEmpty,

    /// Human review lists too many entries.
    #[error("human_review lists at most {limit} labels and {limit} deleted_tests globs")]
    HumanReviewTooManyEntries {
        /// Maximum entries allowed.
        limit: usize,
    },

    /// Human review label is invalid or duplicated.
    #[error("human_review labels: {label:?} must be a valid label listed once")]
    HumanReviewLabel {
        /// The rejected label.
        label: String,
    },

    /// Human review glob is invalid or duplicated.
    #[error("human_review deleted_tests: {glob:?} must be a valid relative glob listed once")]
    HumanReviewGlob {
        /// The rejected glob.
        glob: String,
    },

    /// Pattern syntax error.
    #[error("syntax error in pattern")]
    BadPattern,

    /// The policy file cannot be read.
    #[error("review policy {path}: {source}", path = .path.display())]
    PolicyFile {
        /// Path of the policy file as given.
        path: PathBuf,
        /// The read failure.
        #[source]
        source: io::Error,
    },

    /// The policy file is not one valid YAML document of the policy structure.
    #[error("review policy is not valid YAML: {message}")]
    InvalidYaml {
        /// What the YAML reader rejected.
        message: String,
    },

    /// The policy file holds more than one YAML document.
    #[error("review policy must contain exactly one YAML document")]
    MultipleDocuments,

    /// The policy version is not 1.
    #[error("review policy version must be 1")]
    Version,

    /// The unmatched setting is not skip.
    #[error("review policy unmatched must be skip")]
    Unmatched,

    /// The approval timeout is out of range.
    #[error("review policy approval.timeout_hours must be between 1 and {max}")]
    ApprovalTimeout {
        /// Largest accepted value in hours.
        max: i64,
    },

    /// A ready draft author is malformed or repeated.
    #[error("ready_draft_authors: {author:?} must be a lowercase login listed once")]
    ReadyDraftAuthor {
        /// The rejected login.
        author: String,
    },

    /// The policy defines no reviewers.
    #[error("review policy defines no reviewers")]
    NoReviewers,

    /// A reviewer id is malformed or reserved.
    #[error("reviewer id {id:?} is invalid or reserved")]
    ReviewerId {
        /// The rejected id.
        id: String,
    },

    /// A reviewer protocol is unknown.
    #[error("reviewer {id}: protocol must be claude-structured or codex-envelope")]
    ReviewerProtocol {
        /// The reviewer id.
        id: String,
    },

    /// A reviewer agent profile is malformed.
    #[error("reviewer {id}: agent_profile is invalid")]
    ReviewerProfile {
        /// The reviewer id.
        id: String,
    },

    /// A reviewer label is blank, too long or spans lines.
    #[error("reviewer {id}: label must be a single line of at most {max} characters")]
    ReviewerLabel {
        /// The reviewer id.
        id: String,
        /// Most characters a label may have.
        max: usize,
    },

    /// A reviewer finding prefix is not one uppercase letter.
    #[error("reviewer {id}: finding_prefix must be one uppercase letter")]
    ReviewerPrefix {
        /// The reviewer id.
        id: String,
    },

    /// A reviewer finding prefix is the one reserved for republished prior findings.
    #[error("reviewer {id}: finding_prefix {prefix} is reserved for republished findings")]
    ReviewerPrefixReserved {
        /// The reviewer id.
        id: String,
        /// The reserved prefix.
        prefix: String,
    },

    /// Two reviewers share a finding prefix.
    #[error("reviewers {first} and {second} share finding_prefix {prefix}")]
    SharedPrefix {
        /// The reviewer that took the prefix first.
        first: String,
        /// The reviewer that repeats the prefix.
        second: String,
        /// The shared prefix.
        prefix: String,
    },

    /// The policy defines no pipelines.
    #[error("review policy defines no pipelines")]
    NoPipelines,

    /// A pipeline name is malformed.
    #[error("pipeline name {name:?} is invalid")]
    PipelineName {
        /// The rejected name.
        name: String,
    },

    /// A pipeline lists no reviewers or too many.
    #[error("pipeline {pipeline}: {field} must list between 1 and {max} reviewers")]
    ReviewerCount {
        /// The pipeline name.
        pipeline: String,
        /// The reviewer list field.
        field: &'static str,
        /// Most reviewers the list may name.
        max: usize,
    },

    /// A pipeline names an unknown reviewer or one reviewer twice.
    #[error("pipeline {pipeline}: {field} names unknown or repeated reviewer {id:?}")]
    ReviewerUnknown {
        /// The pipeline name.
        pipeline: String,
        /// The reviewer list field.
        field: &'static str,
        /// The rejected reviewer id.
        id: String,
    },

    /// An incremental reviewer is not one of the pipeline reviewers.
    #[error("pipeline {pipeline}: incremental reviewer {id} is not one of its reviewers")]
    IncrementalReviewer {
        /// The pipeline name.
        pipeline: String,
        /// The incremental reviewer id.
        id: String,
    },

    /// The reviewers of a pipeline use different protocols.
    #[error("pipeline {pipeline}: all reviewers must use the same protocol")]
    MixedProtocols {
        /// The pipeline name.
        pipeline: String,
    },

    /// A pipeline checkout is unknown.
    #[error("pipeline {pipeline}: checkout must be base or head")]
    PipelineCheckout {
        /// The pipeline name.
        pipeline: String,
    },

    /// A pipeline lists no project sources.
    #[error("pipeline {pipeline}: project_sources must not be empty")]
    NoProjectSources {
        /// The pipeline name.
        pipeline: String,
    },

    /// A project source is malformed or repeated.
    #[error("pipeline {pipeline}: project source {name:?} is invalid or repeated")]
    ProjectSource {
        /// The pipeline name.
        pipeline: String,
        /// The rejected source name.
        name: String,
    },

    /// A prompt path leaves the policy directory or is not clean.
    #[error("pipeline {pipeline}: prompt must be a relative path inside the policy directory")]
    PromptPath {
        /// The pipeline name.
        pipeline: String,
    },

    /// A prompt file cannot be read.
    #[error("pipeline {pipeline}: prompt {path}: {source}")]
    PromptRead {
        /// The pipeline name.
        pipeline: String,
        /// The prompt path as written in the policy.
        path: String,
        /// The read failure.
        #[source]
        source: io::Error,
    },

    /// A prompt is empty, too large, not UTF-8 or holds a NUL byte.
    #[error(
        "pipeline {pipeline}: prompt {path} must be non-empty UTF-8 text of at most {max} bytes"
    )]
    PromptContent {
        /// The pipeline name.
        pipeline: String,
        /// The prompt path as written in the policy.
        path: String,
        /// Largest accepted prompt in bytes.
        max: usize,
    },

    /// A verifier names an unknown reviewer.
    #[error("pipeline {pipeline}: verifier names unknown reviewer {reviewer:?}")]
    VerifierReviewer {
        /// The pipeline name.
        pipeline: String,
        /// The rejected reviewer id.
        reviewer: String,
    },

    /// A verifier reviewer does not use the structured protocol.
    #[error("pipeline {pipeline}: verifier must use protocol claude-structured")]
    VerifierProtocol {
        /// The pipeline name.
        pipeline: String,
    },

    /// A verifier sets no escalation severities or no escalation count.
    #[error(
        "pipeline {pipeline}: verifier needs escalate_severities and an escalate_count of at least 1"
    )]
    VerifierEscalation {
        /// The pipeline name.
        pipeline: String,
    },

    /// A verifier escalation severity is not blocking or is repeated.
    #[error(
        "pipeline {pipeline}: verifier escalate_severities must list critical or high once each"
    )]
    VerifierSeverities {
        /// The pipeline name.
        pipeline: String,
    },

    /// The policy lists no repository rules or too many.
    #[error("review policy must list between 1 and {max} repository rules")]
    RuleCount {
        /// Most rules a policy may list.
        max: usize,
    },

    /// A repository rule is invalid.
    #[error("{position}: {source}")]
    Rule {
        /// The rule index and match, as repositories[index] (match).
        position: String,
        /// Why the rule is invalid.
        #[source]
        source: Box<Error>,
    },

    /// A rule match is not owner/repository or owner/* in lowercase.
    #[error("match must be owner/repository or owner/* in lowercase")]
    RuleMatch,

    /// An earlier rule covers every repository of this rule.
    #[error("unreachable, {earlier} matches first")]
    RuleUnreachable {
        /// The match of the earlier rule.
        earlier: String,
    },

    /// A rule trigger is unknown.
    #[error("trigger {raw:?} must be review-requested or label:<name>")]
    RuleTrigger {
        /// The trigger as written in the policy.
        raw: String,
    },

    /// A rule names an unknown pipeline.
    #[error("unknown pipeline {name:?}")]
    RuleUnknownPipeline {
        /// The rejected pipeline name.
        name: String,
    },

    /// A rule on_findings value is unknown.
    #[error("on_findings must be request_changes or comment")]
    RuleOnFindings,

    /// A rule on_clean value is unknown.
    #[error("on_clean must be approve, await or comment")]
    RuleOnClean,

    /// An automatically approving rule re-reviews with fewer reviewers.
    #[error("on_clean approve needs incremental_reviewers equal to reviewers")]
    RuleApproveNeedsFullReReview,

    /// A rule on_notes value is unknown.
    #[error("on_notes must be comment when set")]
    RuleOnNotes,

    /// A rule hands notes off without a manual approval after a review request.
    #[error("on_notes comment needs on_clean await and trigger review-requested")]
    RuleOnNotesNeedsAwait,

    /// A rule has no authors section.
    #[error("authors is required")]
    AuthorsMissing,

    /// The trusted value of a rule's authors is not any or a usable mapping.
    #[error("authors.trusted must be any or a mapping with logins or permission")]
    AuthorsTrusted,

    /// A trusted login is malformed or repeated.
    #[error(
        "authors.trusted.logins: {login:?} must be a lowercase login, or an app's login with \
         [bot], listed once"
    )]
    AuthorsLogin {
        /// The rejected login.
        login: String,
    },

    /// The trusted permission is unknown.
    #[error("authors.trusted.permission must be write, maintain or admin")]
    AuthorsPermission,

    /// A rule that trusts every author also sets an untrusted section.
    #[error("authors.untrusted is not allowed with trusted any")]
    AuthorsUntrustedWithAny,

    /// A rule that trusts only some authors sets no untrusted section.
    #[error("authors.untrusted is required unless trusted is any")]
    AuthorsUntrustedMissing,

    /// The untrusted value is neither skip nor a mapping.
    #[error("authors.untrusted must be skip or a mapping with pipeline, on_findings and on_clean")]
    AuthorsUntrusted,

    /// The untrusted section names an unknown pipeline.
    #[error("authors.untrusted: unknown pipeline {name:?}")]
    UntrustedUnknownPipeline {
        /// The rejected pipeline name.
        name: String,
    },

    /// The untrusted pipeline does not start from the base branch.
    #[error("untrusted pipeline must use checkout base: {pipeline}")]
    UntrustedCheckout {
        /// The pipeline name.
        pipeline: String,
    },

    /// The untrusted on_findings value is unknown.
    #[error("authors.untrusted.on_findings must be request_changes or comment")]
    UntrustedOnFindings,

    /// The untrusted on_clean value is unknown or approve.
    #[error("authors.untrusted.on_clean must be comment or await, never approve")]
    UntrustedOnClean,

    /// The untrusted section sets on_notes.
    #[error("authors.untrusted: on_notes is not allowed")]
    UntrustedOnNotes,

    /// A rule hands notes off but notes_handoff is absent.
    #[error(
        "review policy needs notes_handoff.reminder_hours because a rule sets on_notes comment"
    )]
    NotesHandoffMissing,

    /// The notes_handoff section is set but no rule hands notes off.
    #[error("review policy notes_handoff is set but no rule sets on_notes comment")]
    NotesHandoffUnused,

    /// The notes handoff reminder is out of range.
    #[error("review policy notes_handoff.reminder_hours must be between 1 and {max}")]
    NotesHandoffHours {
        /// Largest accepted value in hours.
        max: i64,
    },

    /// The notifications name an unknown event.
    #[error("notifications: unknown event {event:?}")]
    UnknownEvent {
        /// The event as written in the policy.
        event: String,
    },

    /// A notification event has no level.
    #[error("notifications: event {event} needs a level")]
    EventLevelMissing {
        /// The event name.
        event: &'static str,
    },

    /// A notification level is unknown.
    #[error("notifications: event {event} level must be action, digest or log")]
    EventLevel {
        /// The event name.
        event: &'static str,
    },
}
