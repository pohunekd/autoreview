//! Policy queries: rule resolution, trigger scopes, agent profiles and settings.

// Rust guideline compliant 2026-06-26

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use crate::author_limit::AuthorLimit;
use crate::authors::{Authors, Untrusted};
use crate::parse::{cut, matches};
use crate::types::{Event, Level, OPTIONAL_EVENTS, Policy, Rule, TriggerKind};

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

    /// Returns the author limit of the first rule that matches the lowercase repository.
    ///
    /// `None` means no rule covers the repository or its rule sets no limit.
    #[must_use]
    pub fn author_limit(&self, repository: &str) -> Option<&AuthorLimit> {
        self.rules
            .iter()
            .zip(&self.author_limits)
            .find(|(rule, _)| matches(&rule.match_glob, repository))
            .and_then(|(_, limit)| limit.as_ref())
    }

    /// Returns the longest window of any rule's author limit; zero when no rule sets one.
    #[must_use]
    pub fn longest_author_window(&self) -> Duration {
        let windows = self.author_limits.iter().flatten();
        windows.map(AuthorLimit::window).max().unwrap_or_default()
    }

    /// Returns the match pattern of the first rule with a guard section.
    ///
    /// `None` means no rule scans pull request text.
    #[must_use]
    pub fn guarded_rule(&self) -> Option<&str> {
        let mut guarded = self.rules.iter().filter(|rule| rule.guard.is_some());
        guarded.next().map(|rule| rule.match_glob.as_str())
    }

    /// Returns the match patterns of the repository rules, in file order.
    #[must_use]
    pub fn rule_matches(&self) -> Vec<&str> {
        self.rules
            .iter()
            .map(|rule| rule.match_glob.as_str())
            .collect()
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
    #[must_use]
    pub fn label_scopes(&self, label: &str) -> Vec<String> {
        Self::search_qualifiers(&self.label_matches(label))
    }

    /// Returns the distinct match patterns of the rules triggered by `label`, sorted.
    #[must_use]
    pub fn label_matches(&self, label: &str) -> Vec<String> {
        let mut set = BTreeSet::new();
        for rule in &self.rules {
            if rule.trigger.kind == TriggerKind::Label && rule.trigger.label == label {
                set.insert(rule.match_glob.clone());
            }
        }
        set.into_iter().collect()
    }

    /// Returns the search qualifiers of the match patterns `matches`, sorted.
    ///
    /// An owner pattern gives `user:<owner>` and a repository pattern `repo:<owner>/<name>`.
    /// GitHub combines them with OR, and `user:` matches organizations as well.
    #[must_use]
    pub fn search_qualifiers(matches: &[String]) -> Vec<String> {
        let mut set = BTreeSet::new();
        for pattern in matches {
            let (owner, name) = cut(pattern);
            if name == "*" {
                set.insert(format!("user:{owner}"));
            } else {
                set.insert(format!("repo:{pattern}"));
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

    /// Returns the names of the pipelines that a rule or its untrusted section runs, sorted.
    #[must_use]
    pub fn pipeline_names(&self) -> BTreeSet<&str> {
        let mut names = BTreeSet::new();
        for (rule, authors) in self.rules.iter().zip(&self.authors) {
            names.insert(rule.pipeline.name.as_str());
            if let Authors::Restricted {
                untrusted: Untrusted::Review(review),
                ..
            } = authors
            {
                names.insert(review.pipeline.name.as_str());
            }
        }
        names
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
    /// An optional event that the policy leaves out gets its level from [`OPTIONAL_EVENTS`].
    /// Parsing sets every required event, so a parsed policy never returns `None`.
    #[must_use]
    pub fn level(&self, event: Event) -> Option<Level> {
        let default = || OPTIONAL_EVENTS.iter().find(|(known, _)| *known == event);
        (self.notifications.get(&event).cloned()).or_else(|| default().map(|(_, l)| l.clone()))
    }

    /// Returns the level of every required event and of each optional event the policy names.
    #[must_use]
    pub fn notifications(&self) -> &BTreeMap<Event, Level> {
        &self.notifications
    }

    /// Reports whether the review queue digest carries the weekly report.
    ///
    /// `notifications.weekly_report: digest` sets it; `off` or no key leaves it out.
    #[must_use]
    pub fn weekly_report(&self) -> bool {
        self.weekly_report
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
