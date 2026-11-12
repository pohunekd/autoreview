//! The engine's data model: pull requests, review inputs, results, records and their identifiers.
//!
//! The serde shapes are the JSON that the Go reference stores, so a Go row reads unchanged.

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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::publish::plan::PublicationPlan;
    use autoreview_policy::Rule;
    use autoreview_schedule::scheduler::Record;
    use serde::de::DeserializeOwned;
    use serde::{Deserialize, Serialize};
    use serde_json::{Value, json};

    use crate::integrations::{CommentableLines, GitHubReview, SessionRequest};
    use crate::review::input::SlotState;

    use super::{
        Artifact, AwaitRecord, Discussion, Finding, PriorFinding, PriorResult, PublishedRecord,
        ReviewInput, ReviewOutcome, ReviewerResult, Verification,
    };

    /// Go-written persisted values, shared with the Go reference tests.
    const GOLDEN: &str = include_str!("../../../../testdata/parity/persisted.json");

    /// Golden cases of types that later slices port, so this test skips them.
    const LATER_CASES: &[&str] = &[];

    /// Keys whose golden JSON null is the nil slice of a Go list field.
    const LIST_KEYS: &[&str] = &[
        "comments",
        "findings",
        "incremental_reviewers",
        "prior",
        "project_sources",
        "reviewers",
        "summaries",
        "tests",
        "threads",
    ];

    /// Keys whose golden JSON null is the nil map of a Go map field.
    const MAP_KEYS: &[&str] = &["notifications"];

    /// One golden case: its name and the Go output. Every persisted case has a null input.
    #[derive(Debug, Deserialize)]
    struct Case {
        name: String,
        output: Value,
    }

    /// Replaces each null under a list key with `[]` and each null under a map key with `{}`.
    ///
    /// A null under any other key stays in place, so the comparison fails on it.
    fn normalize(value: &mut Value) {
        match value {
            Value::Object(map) => {
                for (key, child) in map.iter_mut() {
                    if child.is_null() && LIST_KEYS.contains(&key.as_str()) {
                        *child = json!([]);
                    } else if child.is_null() && MAP_KEYS.contains(&key.as_str()) {
                        *child = json!({});
                    } else {
                        normalize(child);
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(normalize),
            _ => {}
        }
    }

    /// Reads a golden output as `T`, writes it back, and returns both sides normalized.
    fn round_trip<T: DeserializeOwned + Serialize>(case: &Case) -> (Value, Value) {
        let value: T = serde_json::from_value(case.output.clone())
            .unwrap_or_else(|error| panic!("{}: the output reads as the type: {error}", case.name));
        let mut actual = serde_json::to_value(&value).expect("the value serializes");
        let mut expected = case.output.clone();
        normalize(&mut actual);
        normalize(&mut expected);
        (actual, expected)
    }

    /// Reads a golden `CommentableLines`; Go writes a nil map as `null`, which reads as empty.
    ///
    /// The alias carries no serde attribute, so the null rule is applied by this helper.
    fn round_trip_commentable_lines(case: &Case) -> (Value, Value) {
        let lines: CommentableLines = autoreview_go_compat::null_default::deserialize(&case.output)
            .unwrap_or_else(|error| panic!("{}: {error}", case.name));
        let actual = serde_json::to_value(&lines).expect("the value serializes");
        let expected = if case.output.is_null() {
            json!({})
        } else {
            case.output.clone()
        };
        (actual, expected)
    }

    /// Reads a golden discovery `rejected` map. A Go nil map reads as empty.
    fn round_trip_rejected(case: &Case) -> (Value, Value) {
        #[rustfmt::skip]
        let value: BTreeMap<String, String> = autoreview_go_compat::null_default::deserialize(
            case.output.clone(),
        )
        .unwrap_or_else(|error| panic!("{}: the output reads as the map: {error}", case.name));
        let actual = serde_json::to_value(&value).expect("the value serializes");
        let expected = if case.output.is_null() {
            json!({})
        } else {
            case.output.clone()
        };
        (actual, expected)
    }

    #[test]
    fn persisted_types_read_go_golden() {
        let cases: Vec<Case> = serde_json::from_str(GOLDEN).expect("golden is JSON");
        let mut checked = 0;
        for case in &cases {
            let (actual, expected) = match case.name.as_str() {
                "review_input_nil_slices" | "review_input_with_verifier" => {
                    round_trip::<ReviewInput>(case)
                }
                "review_outcome_superseded" | "review_outcome_published" => {
                    round_trip::<ReviewOutcome>(case)
                }
                "published_record_zero" | "published_record_awaiting" => {
                    round_trip::<PublishedRecord>(case)
                }
                "await_record_zero" | "await_record_label_trigger" => {
                    round_trip::<AwaitRecord>(case)
                }
                "artifact_nil_slices" | "artifact_empty_slices" | "artifact_full" => {
                    round_trip::<Artifact>(case)
                }
                "finding_zero" | "finding_blocking_verdict" => round_trip::<Finding>(case),
                "reviewer_result_nil" | "reviewer_result_empty" => {
                    round_trip::<ReviewerResult>(case)
                }
                "prior_finding_zero" => round_trip::<PriorFinding>(case),
                "prior_result_zero" => round_trip::<PriorResult>(case),
                "verification_zero" | "verification_error" => round_trip::<Verification>(case),
                "policy_rule_zero" => round_trip::<Rule>(case),
                "discussion_zero" => round_trip::<Discussion>(case),
                "publication_plan_zero" | "publication_plan_await" => {
                    round_trip::<PublicationPlan>(case)
                }
                "schedule_record_zero" | "schedule_record_failed" => round_trip::<Record>(case),
                "session_request_zero" => round_trip::<SessionRequest>(case),
                "discovery_rejected_nil"
                | "discovery_rejected_empty"
                | "discovery_rejected_entry" => round_trip_rejected(case),
                "commentable_lines_nil" | "commentable_lines_one_file" => {
                    round_trip_commentable_lines(case)
                }
                "github_review_zero" => round_trip::<GitHubReview>(case),
                "slot_state_zero" | "slot_state_with_session_and_result" => {
                    round_trip::<SlotState>(case)
                }
                name => {
                    assert!(
                        LATER_CASES.contains(&name),
                        "golden case {name} is not ported"
                    );
                    continue;
                }
            };
            assert_eq!(actual, expected, "case {}", case.name);
            checked += 1;
        }
        assert_eq!(
            checked, 34,
            "golden has the 34 persisted cases of this module"
        );
    }
}
