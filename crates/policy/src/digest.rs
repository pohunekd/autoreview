//! The policy digest: a SHA-256 over every rule, notification level and timing setting.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use crate::author_limit::AuthorLimit;
use crate::authors::{Authors, Untrusted};
use crate::types::{Event, IntentCheck, Level, Pipeline, Policy, Reviewer, Rule};

/// Renders an optional limit for the digest; an absent limit is the empty text.
fn limit_text(limit: Option<i64>) -> String {
    limit.map_or_else(String::new, |value| value.to_string())
}

impl Policy {
    /// Returns the hex SHA-256 digest that identifies the effective policy.
    ///
    /// Logs and review workflow inputs carry it.
    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }
}

/// Returns the hex SHA-256 digest that identifies the effective policy.
///
/// The hashed bytes must not change, because review inputs and logs store the digest. A rule
/// with `trusted: any` adds nothing, so only a restricted authors section extends the bytes;
/// `authors` holds the section of each rule, in the order of `rules`. Only a rule with an
/// author limit hashes `author_limits`, and only a rule with a guard hashes it, so a policy
/// without either keeps its digest.
pub(crate) fn digest_of(
    rules: &[Rule],
    authors: &[Authors],
    author_limits: &[Option<AuthorLimit>],
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
    for (index, rule) in rules.iter().enumerate() {
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
        hash_pipeline_body(&mut hasher, pipeline);
        if let Some(human) = &rule.human_review {
            hasher.update(format!(
                "human_review\x00{}\x00{}\x00",
                human.labels.join(","),
                human.deleted_tests.join(",")
            ));
            if human.has_extended_signals() {
                hasher.update(format!(
                    "human_review_signals\x00{}\x00{}\x00{}\x00{}\x00{}\x00",
                    human.paths.join(","),
                    limit_text(human.max_changed_lines),
                    limit_text(human.max_changed_files),
                    limit_text(human.max_notes),
                    human.authors.join(",")
                ));
            }
        }
        if let Some(Authors::Restricted { trusted, untrusted }) = authors.get(index) {
            hasher.update(format!(
                "authors\x00{}\x00{}\x00",
                trusted.logins.join(","),
                trusted
                    .permission
                    .map_or("", |permission| permission.as_str())
            ));
            match untrusted {
                Untrusted::Skip => hasher.update(b"untrusted\x00skip\x00"),
                Untrusted::Review(review) => {
                    let pipeline = &review.pipeline;
                    hasher.update(format!(
                        "untrusted\x00{}\x00{}\x00{}\x00{}\x00{}\x00",
                        pipeline.name,
                        pipeline.checkout.as_str(),
                        review.on_findings.as_str(),
                        review.on_clean.as_str(),
                        pipeline.project_sources.join(",")
                    ));
                    hash_pipeline_body(&mut hasher, pipeline);
                }
            }
        }
        if let Some(Some(limit)) = author_limits.get(index) {
            hasher.update(format!(
                "author_limit\x00{}\x00{}\x00{}\x00",
                limit.reviews,
                limit.window_hours,
                limit.over.as_str()
            ));
        }
        // Only a guarded rule adds bytes, so other policies keep their digest.
        if let Some(guard) = &rule.guard {
            let fields: Vec<&str> = guard.fields.iter().map(|kind| kind.as_str()).collect();
            hasher.update(format!(
                "guard\x00{}\x00{}\x00{}\x00",
                fields.join(","),
                guard.threshold,
                guard.on_suspect.as_str()
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

/// Hashes the reviewers, the prompt and the verifier of a pipeline.
fn hash_pipeline_body(hasher: &mut Sha256, pipeline: &Pipeline) {
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
    // Only `issues` adds bytes, so a pipeline without the key or with `off` keeps its digest.
    if pipeline.intent_check == IntentCheck::Issues {
        hasher.update(b"intent_check\x00issues\x00");
    }
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
