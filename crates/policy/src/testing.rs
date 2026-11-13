//! Helpers shared by the tests of the policy crate.

// Rust guideline compliant 2026-06-26

/// The flow rule of `testdata/review-policy.yaml`, up to its end.
const FLOW_RULE_END: &str = "deleted_tests: [\"**/tests/**\"]}}";

/// The outcome line of each block rule of `testdata/review-policy.yaml`.
const BLOCK_RULE_LINE: &str = "    on_clean: approve\n";

/// Adds `authors` with `trusted: any` to every rule of `testdata/review-policy.yaml`.
///
/// @TODO remove after #111 and write the sections into the fixture.
pub(crate) fn with_trusted_any(document: &str) -> String {
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
