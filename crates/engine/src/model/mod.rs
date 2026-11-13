//! The engine's data model: pull requests, review inputs, results, records and their identifiers.
//!
//! The serde shapes are the stored JSON, so every existing row reads unchanged.

// Rust guideline compliant 2026-06-26

pub(crate) mod finding;
pub(crate) mod host_state;
pub(crate) mod ids;
pub(crate) mod pull_request;
pub(crate) mod review;

#[doc(inline)]
pub use finding::{Finding, ReviewerResult, ReviewerSummary};
#[doc(inline)]
pub use host_state::{
    HOST_STATE_UNAVAILABLE, HostCapacity, HostCpu, HostGpu, HostMemory, HostModels, HostPolicy,
    HostWindow,
};
#[doc(inline)]
pub use ids::{HEAD_PREFIX_MIN_LEN, SHA_LEN, head_prefix_valid, repository_valid, sha_valid};
#[doc(inline)]
pub use pull_request::{
    Candidate, CandidateList, ClassifiedCandidates, NotesHandoff, PriorFinding, ProjectRoute,
    PullRequest, RejectedCandidate, ReviewInput, ReviewScope, RunLimits, SkippedCandidate,
};
#[doc(inline)]
pub use review::{
    ApprovalDecision, ApprovalResult, ApproveRequest, Artifact, AwaitRecord, Decision, Discussion,
    DiscussionComment, DiscussionThread, FindingCheck, Outcome, PollResult, PriorCheck,
    PriorResult, PriorVerdict, PublishResult, PublishedRecord, ResolveRequest, ResolveResult,
    RetryRequest, RetryResult, ReviewOutcome, SessionIdentity, Verification, prior_key,
};
