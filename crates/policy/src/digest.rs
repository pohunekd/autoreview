//! The policy digest: a SHA-256 over every rule, notification level and timing setting.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use crate::types::{Event, Level, Policy, Reviewer, Rule};

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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io;
    use std::path::{Path, PathBuf};

    use serde::Deserialize;

    use crate::testing::with_trusted_any;
    use crate::{load, parse};

    /// Go-written policy digests, shared with the Go reference tests.
    const GOLDEN: &str = include_str!("../../../testdata/parity/digest.json");

    /// One golden case: a policy built from the fixture and its Go digest.
    #[derive(Debug, Deserialize)]
    struct Case {
        name: String,
        input: Input,
        output: Output,
    }

    /// How the case builds its policy from the fixture.
    #[derive(Debug, Deserialize)]
    struct Input {
        policy_file: String,
        loader: Option<String>,
        replacements: Option<Vec<(String, String)>>,
        prompt_overrides: Option<BTreeMap<String, String>>,
    }

    /// The digest Go computed for the case.
    #[derive(Debug, Deserialize)]
    struct Output {
        digest: String,
    }

    /// Returns the repository root, which the golden paths are relative to.
    fn repository_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    /// Loads the fixture with `trusted: any` on every rule from a copy in a temporary directory.
    ///
    /// The fixture itself has no authors sections, which Go refuses (#32).
    fn load_with_trusted_any(policy_path: &Path) -> crate::Policy {
        let directory = tempfile::tempdir().expect("temporary directory");
        let source = std::fs::read_to_string(policy_path).expect("the fixture is readable");
        let copy = directory.path().join("review-policy.yaml");
        std::fs::write(&copy, with_trusted_any(&source)).expect("the fixture copy");
        let prompts = policy_path.with_file_name("prompts");
        std::fs::create_dir(directory.path().join("prompts")).expect("the prompts directory");
        for entry in std::fs::read_dir(&prompts).expect("the prompts are readable") {
            let path = entry.expect("a prompts entry").path();
            let target = directory
                .path()
                .join("prompts")
                .join(path.file_name().expect("a name"));
            std::fs::copy(&path, target).expect("the prompt copy");
        }
        load(&copy).expect("the fixture loads")
    }

    // Every rule gets `trusted: any`, which adds nothing to the digest, so Go's digests hold.
    #[test]
    fn digest_matches_go_golden() {
        let cases: Vec<Case> = serde_json::from_str(GOLDEN).expect("golden is JSON");
        assert!(!cases.is_empty(), "golden has cases");
        let root = repository_root();
        for case in &cases {
            let name = &case.name;
            let policy_path = root.join(&case.input.policy_file);
            let policy = if case.input.loader.as_deref() == Some("policy.Load") {
                load_with_trusted_any(&policy_path)
            } else {
                let mut document = with_trusted_any(
                    &std::fs::read_to_string(&policy_path).expect("the fixture is readable"),
                );
                for (target, replacement) in case.input.replacements.iter().flatten() {
                    assert!(
                        document.contains(target.as_str()),
                        "{name}: replacement target {target:?} is not in the policy fixture"
                    );
                    document = document.replacen(target.as_str(), replacement, 1);
                }
                let directory = policy_path.parent().expect("the fixture has a directory");
                let overrides = case.input.prompt_overrides.as_ref();
                parse(document.as_bytes(), |relative| -> io::Result<Vec<u8>> {
                    match overrides.and_then(|overrides| overrides.get(relative)) {
                        Some(content) => Ok(content.clone().into_bytes()),
                        None => std::fs::read(directory.join(relative)),
                    }
                })
                .unwrap_or_else(|error| panic!("{name}: the policy parses: {error}"))
            };
            assert_eq!(policy.digest(), case.output.digest, "{name}");
        }
    }
}
