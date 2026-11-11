//! Policy queries: rule resolution, trigger scopes, agent profiles and settings.

// Rust guideline compliant 2026-06-26

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use crate::authors::Authors;
use crate::parse::{cut, matches};
use crate::types::{Event, Level, Policy, Rule, TriggerKind};

impl Policy {
    /// Returns the first rule that matches the lowercase repository.
    ///
    /// Rules are tried in file order. `None` means no rule covers the repository.
    #[must_use]
    pub fn resolve(&self, repository: &str) -> Option<&Rule> {
        self.resolve_with_authors(repository).map(|(rule, _)| rule)
    }

    /// Returns the first rule that matches the lowercase repository and its authors section.
    ///
    /// Both come from one match, so a candidate's trust section is always its rule's own.
    /// `None` means no rule covers the repository.
    #[must_use]
    pub fn resolve_with_authors(&self, repository: &str) -> Option<(&Rule, &Authors)> {
        self.rules
            .iter()
            .zip(&self.authors)
            .find(|(rule, _)| matches(&rule.match_glob, repository))
    }

    /// Returns the distinct labels used by label triggers, sorted.
    #[must_use]
    pub fn labels(&self) -> Vec<String> {
        let mut set = BTreeSet::new();
        for rule in &self.rules {
            if rule.trigger.kind == TriggerKind::Label {
                set.insert(rule.trigger.label.clone());
            }
        }
        set.into_iter().collect()
    }

    /// Returns the search qualifiers of the rules triggered by label, sorted.
    ///
    /// An owner rule gives `user:<owner>` and a repository rule `repo:<owner>/<name>`.
    /// GitHub combines them with OR, and `user:` matches organizations as well.
    #[must_use]
    pub fn label_scopes(&self, label: &str) -> Vec<String> {
        let mut set = BTreeSet::new();
        for rule in &self.rules {
            if rule.trigger.kind == TriggerKind::Label && rule.trigger.label == label {
                let (owner, name) = cut(&rule.match_glob);
                if name == "*" {
                    set.insert(format!("user:{owner}"));
                } else {
                    set.insert(format!("repo:{}", rule.match_glob));
                }
            }
        }
        set.into_iter().collect()
    }

    /// Returns the distinct agent profiles of the reviewers and verifiers, sorted.
    ///
    /// Incremental reviewers are not read: parsing makes each one a pipeline reviewer.
    #[must_use]
    pub fn agent_profiles(&self) -> Vec<String> {
        let mut set = BTreeSet::new();
        for rule in &self.rules {
            for reviewer in &rule.pipeline.reviewers {
                set.insert(reviewer.agent_profile.clone());
            }
            if let Some(verifier) = &rule.pipeline.verifier {
                set.insert(verifier.reviewer.agent_profile.clone());
            }
        }
        set.into_iter().collect()
    }

    /// Returns the distinct match patterns of review-requested rules, sorted.
    ///
    /// The review request search covers only these repositories.
    #[must_use]
    pub fn review_request_scopes(&self) -> Vec<String> {
        let mut set = BTreeSet::new();
        for rule in &self.rules {
            if rule.trigger.kind == TriggerKind::ReviewRequested {
                set.insert(rule.match_glob.clone());
            }
        }
        set.into_iter().collect()
    }

    /// Returns the sorted logins whose drafts are marked ready for review.
    ///
    /// Only drafts that ask for a review are marked, so they can be reviewed.
    #[must_use]
    pub fn ready_draft_authors(&self) -> &[String] {
        &self.ready_draft_authors
    }

    /// Returns the level of a notification event.
    ///
    /// Parsing sets every event, so a parsed policy never returns `None`.
    #[must_use]
    pub fn level(&self, event: Event) -> Option<Level> {
        self.notifications.get(&event).cloned()
    }

    /// Returns the level of every notification event.
    #[must_use]
    pub fn notifications(&self) -> &BTreeMap<Event, Level> {
        &self.notifications
    }

    /// Returns how long a review waits for the user's approval.
    #[must_use]
    pub fn approval_timeout(&self) -> Duration {
        self.approval_timeout
    }

    /// Returns how long handed-off notes wait before the user is reminded once.
    ///
    /// It is zero when no rule hands notes off.
    #[must_use]
    pub fn handoff_reminder(&self) -> Duration {
        self.handoff_reminder
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use crate::parse;
    use crate::testing::with_trusted_any;
    use crate::types::{
        Checkout, Event, Level, OnClean, OnFindings, OnNotes, Policy, Protocol, Rule,
    };

    /// One repository the deployed policy fixture must resolve.
    struct Case {
        repository: &'static str,
        pipeline: &'static str,
        trigger: &'static str,
        on_findings: OnFindings,
        on_clean: OnClean,
        reviewers: &'static [&'static str],
    }

    /// Returns the synthetic deployed policy fixture shared with the Go tests.
    fn deployed_policy_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/review-policy.yaml")
    }

    /// Returns the rule the policy resolves for repository, failing the test when none does.
    fn resolved<'a>(policy: &'a Policy, repository: &str) -> &'a Rule {
        policy
            .resolve(repository)
            .unwrap_or_else(|| panic!("{repository} is not covered by the deployed policy"))
    }

    #[test]
    fn deployed_policy_resolves_the_configured_repositories() {
        let path = deployed_policy_path();
        let document = std::fs::read_to_string(&path).expect("the deployed policy is readable");
        let directory = path.parent().expect("the fixture has a directory");
        let loaded = parse(with_trusted_any(&document).as_bytes(), |relative| {
            std::fs::read(directory.join(relative))
        })
        .expect("the deployed policy parses");
        let cases = [
            Case {
                repository: "example-org/example-repo",
                pipeline: "strict",
                trigger: "review-requested",
                on_findings: OnFindings::RequestChanges,
                on_clean: OnClean::Approve,
                reviewers: &["opus", "sonnet"],
            },
            Case {
                repository: "example-org/example-api",
                pipeline: "generic",
                trigger: "review-requested",
                on_findings: OnFindings::RequestChanges,
                on_clean: OnClean::Approve,
                reviewers: &["opus", "sonnet"],
            },
            Case {
                repository: "example-org/example-lib",
                pipeline: "generic",
                trigger: "review-requested",
                on_findings: OnFindings::RequestChanges,
                on_clean: OnClean::Approve,
                reviewers: &["opus", "sonnet"],
            },
            Case {
                repository: "example-owner/tool",
                pipeline: "personal",
                trigger: "label:ai:review",
                on_findings: OnFindings::RequestChanges,
                on_clean: OnClean::Approve,
                reviewers: &["codex"],
            },
        ];
        for case in &cases {
            let rule = resolved(&loaded, case.repository);
            assert!(
                rule.pipeline.name == case.pipeline
                    && rule.trigger.to_string() == case.trigger
                    && rule.on_findings == case.on_findings
                    && rule.on_clean == case.on_clean,
                "{} resolved to pipeline={} trigger={} findings={:?} clean={:?}",
                case.repository,
                rule.pipeline.name,
                rule.trigger,
                rule.on_findings,
                rule.on_clean
            );
            let ids: Vec<&str> = rule
                .pipeline
                .reviewers
                .iter()
                .map(|reviewer| reviewer.id.as_str())
                .collect();
            assert_eq!(
                ids.join(","),
                case.reviewers.join(","),
                "{} reviewers",
                case.repository
            );
        }
        assert!(
            loaded.resolve("octocat/hello-world").is_none(),
            "a repository outside every rule must not be reviewed"
        );
        let strict = resolved(&loaded, "example-org/example-repo");
        let generic = resolved(&loaded, "example-org/example-api");
        assert!(
            strict.on_notes == OnNotes::Unset && generic.on_notes == OnNotes::Unset,
            "notes-only reviews must be approved, not handed to the author: strict={:?} generic={:?}",
            strict.on_notes,
            generic.on_notes
        );
        for rule in [strict, generic] {
            assert_eq!(
                rule.pipeline.incremental_reviewers.len(),
                rule.pipeline.reviewers.len(),
                "{}: an automatically approved re-review must use every reviewer",
                rule.match_glob
            );
        }
        assert_ne!(
            strict.pipeline.prompt, generic.pipeline.prompt,
            "the strict pipeline must use its own prompt, not the generic one"
        );
        assert!(
            strict.pipeline.checkout == Checkout::Base
                && strict.pipeline.protocol == Protocol::ClaudeStructured,
            "the strict pipeline reviewers must start from the trusted base with the structured protocol"
        );
        let personal = resolved(&loaded, "example-owner/anything");
        assert!(
            personal.pipeline.checkout == Checkout::Head
                && personal.pipeline.protocol == Protocol::CodexEnvelope,
            "personal reviews must use the Codex envelope on the pull request head"
        );
        let labels = loaded.labels();
        assert!(
            labels == ["ai:review"]
                && loaded.review_request_scopes().join(",")
                    == "example-org/*,example-org/example-repo",
            "triggers = labels {labels:?}, review-requested {:?}",
            loaded.review_request_scopes()
        );
        assert!(
            loaded.level(Event::ApprovalRequested) == Some(Level::Action)
                && loaded.level(Event::ChangesRequested) == Some(Level::Digest)
                && loaded.level(Event::Started) == Some(Level::Log),
            "only events that need the user may reach the action room"
        );
        assert_eq!(
            loaded.agent_profiles().join(","),
            "claude-noninteractive,claude-noninteractive-sonnet,codex-review",
            "agent profiles"
        );
        assert_eq!(
            loaded.label_scopes("ai:review").join(","),
            "user:example-owner",
            "label scopes"
        );
        assert!(
            loaded.handoff_reminder() == Duration::ZERO
                && loaded.level(Event::HandoffReminder) == Some(Level::Action),
            "handoff reminder = {:?} at {:?}",
            loaded.handoff_reminder(),
            loaded.level(Event::HandoffReminder)
        );
        assert_eq!(
            loaded.approval_timeout(),
            Duration::from_hours(168),
            "approval timeout"
        );
        for (rule, escalation) in [(strict, "critical,high/1"), (generic, "critical/3")] {
            let verifier = rule.pipeline.verifier.as_ref();
            assert!(
                verifier.is_some_and(|verifier| verifier.reviewer.id == "opus"
                    && verifier.prompt != rule.pipeline.prompt
                    && format!(
                        "{}/{}",
                        verifier.escalate_severities.join(","),
                        verifier.escalate_count
                    ) == escalation),
                "{}: the blocking findings must be verified by Opus with its own prompt and escalate at {escalation}: {verifier:?}",
                rule.match_glob
            );
        }
        let human = strict.human_review.as_ref();
        assert!(
            human.is_some_and(|human| human.labels.join(",") == "needs-human-review"
                && human.deleted_tests.join(",") == "**/tests/**"),
            "the strict repository must hold clean reviews for the opt-in label and deleted tests: {human:?}"
        );
        assert!(
            generic.human_review.is_none(),
            "other organisation repositories have no human review signals"
        );
        assert_eq!(
            loaded.ready_draft_authors().join(","),
            "devin-ai-integration",
            "ready draft authors"
        );
        assert!(
            personal.pipeline.verifier.is_none(),
            "the Codex pipeline has no verifier"
        );
    }
}
