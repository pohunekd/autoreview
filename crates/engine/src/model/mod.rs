//! The engine's data model: pull requests, review inputs and their identifiers.
//!
//! The serde shapes are the JSON that the Go reference stores, so a Go row reads unchanged.

// Rust guideline compliant 2026-06-26

pub(crate) mod ids;
pub(crate) mod pull_request;

#[doc(inline)]
pub use ids::{head_prefix_valid, repository_valid, sha_valid};
#[doc(inline)]
pub use pull_request::{
    Candidate, CandidateList, NotesHandoff, PriorFinding, ProjectRoute, PullRequest,
    RejectedCandidate, ReviewInput, ReviewScope, RunLimits,
};
