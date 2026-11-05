//! The policy digest: a SHA-256 over every rule, notification level and timing setting.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use crate::types::{Event, Level, Reviewer, Rule};

/// Returns the hex SHA-256 digest that identifies the effective policy.
///
/// The bytes hashed match Go's `digestOf` exactly, because review inputs and logs store the digest.
pub(crate) fn digest_of(
    rules: &[Rule],
    notifications: &BTreeMap<Event, Level>,
    timeout_hours: i64,
    reminder_hours: i64,
    ready_draft_authors: &[String],
) -> String {
    let mut events = Vec::new();
    for (event, level) in notifications {
        events.push(format!("{}={}", event.as_str(), level.as_str()));
    }
    events.sort();
    let mut hasher = Sha256::new();
    for rule in rules {
        let pipeline = &rule.pipeline;
        hasher.update(format!(
            "rule\x00{}\x00{}\x00{}\x00{}\x00{}\x00{}\x00{}\x00{}\x00",
            rule.match_glob,
            rule.trigger,
            pipeline.name,
            pipeline.checkout.as_str(),
            rule.on_findings.as_str(),
            rule.on_clean.as_str(),
            rule.on_notes.as_str(),
            pipeline.project_sources.join(",")
        ));
        for group in [&pipeline.reviewers, &pipeline.incremental_reviewers] {
            for reviewer in group {
                hasher.update(reviewer_text(reviewer));
            }
            hasher.update(b"\x00");
        }
        hasher.update(format!(
            "prompt\x00{}\x00{}",
            pipeline.prompt.len(),
            pipeline.prompt
        ));
        if let Some(verifier) = &pipeline.verifier {
            let reviewer = &verifier.reviewer;
            hasher.update(format!(
                "verifier\x00{}\x00{}\x00{}\x00{}\x00{}\x00{}\x00prompt\x00{}\x00{}",
                reviewer.id,
                reviewer.label,
                reviewer.protocol.as_str(),
                reviewer.agent_profile,
                verifier.escalate_severities.join(","),
                verifier.escalate_count,
                verifier.prompt.len(),
                verifier.prompt
            ));
        }
        if let Some(human) = &rule.human_review {
            hasher.update(format!(
                "human_review\x00{}\x00{}\x00",
                human.labels.join(","),
                human.deleted_tests.join(",")
            ));
        }
        hasher.update(b"\x00");
    }
    hasher.update(format!(
        "notifications\x00{}\x00approval\x00{}\x00notes_handoff\x00{}\x00ready_draft_authors\x00{}",
        events.join(","),
        timeout_hours,
        reminder_hours,
        ready_draft_authors.join(",")
    ));
    hex::encode(hasher.finalize())
}

/// Returns the digest text of one pipeline reviewer.
fn reviewer_text(reviewer: &Reviewer) -> String {
    format!(
        "reviewer\x00{}\x00{}\x00{}\x00{}\x00{}\x00",
        reviewer.id,
        reviewer.label,
        reviewer.protocol.as_str(),
        reviewer.agent_profile,
        reviewer.finding_prefix
    )
}
