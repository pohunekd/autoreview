//! Test doubles of the engine's side effects.

// Rust guideline compliant 2026-06-26

#[expect(dead_code, reason = "called by S30")]
pub(crate) mod fake;
pub(crate) mod harness;
