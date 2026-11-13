//! Test doubles of the engine's side effects.

// Rust guideline compliant 2026-06-26

pub(crate) mod fake;
pub(crate) mod harness;

/// The flow rule of `testdata/review-policy.yaml`, up to its end.
const FLOW_RULE_END: &str = "deleted_tests: [\"**/tests/**\"]}}";

/// The outcome line of each block rule of `testdata/review-policy.yaml`.
const BLOCK_RULE_LINE: &str = "    on_clean: approve\n";

/// Returns `testdata/review-policy.yaml` with `trusted: any` on every rule.
///
/// @TODO remove after #111 and write the sections into the fixture.
pub(crate) fn deployed_policy_document() -> String {
    let document = include_str!("../../../../testdata/review-policy.yaml");
    assert!(
        document.contains(FLOW_RULE_END) && document.contains(BLOCK_RULE_LINE),
        "the policy fixture changed; update the authors injection"
    );
    document
        .replacen(
            FLOW_RULE_END,
            "deleted_tests: [\"**/tests/**\"]}, authors: {trusted: any}}",
            1,
        )
        .replace(
            BLOCK_RULE_LINE,
            "    on_clean: approve\n    authors:\n      trusted: any\n",
        )
}
