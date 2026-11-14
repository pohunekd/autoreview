//! Test doubles of the engine's side effects.

// Rust guideline compliant 2026-06-26

pub(crate) mod fake;
pub(crate) mod harness;

/// Returns `testdata/review-policy.yaml`, whose rules all say `trusted: any`.
pub(crate) fn deployed_policy_document() -> String {
    include_str!("../../../../testdata/review-policy.yaml").to_owned()
}
