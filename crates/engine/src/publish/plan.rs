//! Publication plan of a finished review: its GitHub decision, approval wait and notification.

// Rust guideline compliant 2026-06-26

use autoreview_policy::{Event, OnClean, OnFindings, OnNotes};
use serde::{Deserialize, Serialize};

use crate::model::finding::is_false;
use crate::model::{Artifact, Decision, ReviewInput};
use crate::verify::escalates;

/// What the workflow does with a finished review.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct PublicationPlan {
    /// GitHub review event of the publication.
    pub decision: Decision,
    /// Holds an approval until the user confirms it.
    #[serde(rename = "await")]
    pub await_approval: bool,
    /// The reviewing account authored the pull request, so GitHub accepts only a comment.
    pub own_pull_request: bool,
    /// Hands the non-blocking findings to the author in a comment.
    ///
    /// The author's renewed review request then asks the user for approval.
    #[serde(skip_serializing_if = "is_false")]
    pub notes_to_author: bool,
}

autoreview_go_compat::object_only_serde!(PublicationPlan);

/// Counts the new blocking findings and the prior findings the review left open.
pub(crate) fn blocking_count(input: &ReviewInput, artifact: &Artifact) -> usize {
    let (blocking, _) = input.split_findings(&artifact.findings);
    blocking.len() + artifact.open_prior().len()
}

/// Decides the publication of a review by its blocking findings only.
///
/// A review whose findings are all notes, and that leaves no prior finding open, is clean and
/// follows the rule's clean outcome. A clean review that the rule would approve without the user
/// waits for them when the verifier demoted findings the policy wants them to see, or when the
/// pull request matched a human review signal of the rule. A rule with on_notes comment hands a
/// review of notes only to a human author first, unless the author already had them.
#[cfg_attr(not(test), expect(dead_code, reason = "called by S28"))]
pub(crate) fn plan_publication(
    input: &ReviewInput,
    artifact: &Artifact,
    login: &str,
) -> PublicationPlan {
    let own = input.pull_request.author == login;
    let blocking = blocking_count(input, artifact);
    let (_, notes) = input.split_findings(&artifact.findings);
    let mut plan = PublicationPlan {
        own_pull_request: own,
        ..PublicationPlan::default()
    };
    if blocking > 0 && input.rule.on_findings == OnFindings::RequestChanges {
        plan.decision = Decision::RequestChanges;
    } else if blocking > 0 {
        plan.decision = Decision::Comment;
    } else if input.rule.on_clean == OnClean::Approve {
        plan.decision = Decision::Approve;
        plan.await_approval = escalates(input, artifact) || !artifact.human_review.is_empty();
    } else if input.rule.on_notes == OnNotes::Comment
        && !notes.is_empty()
        && input.handoff.is_none()
        && !own
        && !input.pull_request.author_bot
    {
        plan.decision = Decision::Comment;
        plan.notes_to_author = true;
    } else if input.rule.on_clean == OnClean::Await {
        plan.decision = Decision::Approve;
        plan.await_approval = true;
    } else {
        plan.decision = Decision::Comment;
    }
    if own && plan.decision != Decision::Comment {
        plan.decision = Decision::Comment;
        plan.await_approval = false;
    }
    plan
}

/// Names the notification of a published review; `blocking` is its number of blocking findings.
#[cfg_attr(not(test), expect(dead_code, reason = "called by S28"))]
pub(crate) fn publication_event(plan: PublicationPlan, blocking: usize) -> Event {
    if plan.decision == Decision::Approve {
        Event::Approved
    } else if plan.decision == Decision::RequestChanges {
        Event::ChangesRequested
    } else if plan.own_pull_request && blocking > 0 {
        Event::FindingsOnOwnPr
    } else {
        Event::Commented
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use autoreview_policy::{
        Checkout, EVENTS, Event, Level, OnClean, OnFindings, OnNotes, Pipeline, Protocol, Reviewer,
        Rule, Trigger, TriggerKind,
    };

    use super::{PublicationPlan, plan_publication, publication_event};
    use crate::model::ids::INPUT_VERSION;
    use crate::model::pull_request::SCOPE_FULL;
    use crate::model::{
        Artifact, Decision, Finding, NotesHandoff, ProjectRoute, PullRequest, ReviewInput,
        ReviewScope, RunLimits,
    };
    use crate::publish::anchor::anchor_findings;
    use crate::review::input::validate_input;

    fn sha(character: char) -> String {
        character.to_string().repeat(40)
    }

    fn opus() -> Reviewer {
        Reviewer {
            id: "opus".to_owned(),
            label: "Opus".to_owned(),
            protocol: Protocol::ClaudeStructured,
            agent_profile: "claude-profile".to_owned(),
            finding_prefix: "O".to_owned(),
        }
    }

    fn test_rule(reviewer: Reviewer, on_findings: OnFindings, on_clean: OnClean) -> Rule {
        let (checkout, trigger) = if reviewer.protocol == Protocol::CodexEnvelope {
            (
                Checkout::Head,
                Trigger {
                    kind: TriggerKind::Label,
                    label: "ai:review".to_owned(),
                },
            )
        } else {
            (
                Checkout::Base,
                Trigger {
                    kind: TriggerKind::ReviewRequested,
                    label: String::new(),
                },
            )
        };
        Rule {
            match_glob: "acme/*".to_owned(),
            trigger,
            on_findings,
            on_clean,
            pipeline: Pipeline {
                name: "main".to_owned(),
                protocol: reviewer.protocol,
                reviewers: vec![reviewer.clone()],
                incremental_reviewers: vec![reviewer],
                prompt: "Review the pull request.".to_owned(),
                checkout,
                project_sources: vec!["manual".to_owned()],
                verifier: None,
            },
            ..Rule::default()
        }
    }

    fn test_pull_request(number: i64) -> PullRequest {
        PullRequest {
            repository: "acme/widget".to_owned(),
            number,
            head_sha: sha('a'),
            review_request_id: 7,
            title: "Add widget".to_owned(),
            url: format!("https://example.invalid/acme/widget/pull/{number}"),
            author: "colleague".to_owned(),
            head_branch: "feature".to_owned(),
            base_branch: "main".to_owned(),
            base_sha: sha('b'),
            ..PullRequest::default()
        }
    }

    fn notification_levels() -> BTreeMap<Event, Level> {
        let mut levels: BTreeMap<Event, Level> =
            EVENTS.iter().map(|event| (*event, Level::Log)).collect();
        levels.insert(Event::ApprovalRequested, Level::Action);
        levels.insert(Event::FindingsOnOwnPr, Level::Action);
        levels.insert(Event::ReviewStopped, Level::Action);
        levels.insert(Event::CleanupManual, Level::Action);
        levels.insert(Event::Approved, Level::Digest);
        levels.insert(Event::ChangesRequested, Level::Digest);
        levels.insert(Event::Commented, Level::Digest);
        levels
    }

    fn test_input(rule: Rule, pull_request: PullRequest) -> ReviewInput {
        ReviewInput {
            version: INPUT_VERSION,
            pull_request,
            reviewers: rule.pipeline.reviewers.clone(),
            rule,
            scope: ReviewScope {
                mode: SCOPE_FULL.to_owned(),
                previous_head_sha: String::new(),
            },
            route: ProjectRoute {
                host: "review-host".to_owned(),
                project_id: "project".to_owned(),
            },
            notifications: notification_levels(),
            approval_timeout_seconds: 3600,
            policy_digest: "d".repeat(64),
            publisher_login: "reviewer".to_owned(),
            limits: RunLimits {
                session_timeout_ms: 60000,
                output_poll_ms: 1000,
                session_start_attempts: 2,
                session_start_retry_ms: 1000,
                session_cleanup_attempts: 2,
                session_cleanup_retry_ms: 1000,
                reviewer_run_attempts: 2,
                reviewer_run_retry_ms: 1000,
                step_retry_ms: 60000,
                step_retry_initial_ms: 10,
                step_retry_max_interval_ms: 1000,
            },
            ..ReviewInput::default()
        }
    }

    fn note() -> Finding {
        Finding {
            id: "O001".to_owned(),
            non_blocking: true,
            ..Finding::default()
        }
    }

    fn blocking() -> Finding {
        Finding {
            id: "S001".to_owned(),
            ..Finding::default()
        }
    }

    #[test]
    fn only_blocking_findings_decide_the_publication() {
        let cases = [
            (
                "notes only follow the clean outcome",
                INPUT_VERSION,
                OnClean::Await,
                vec![note()],
                PublicationPlan {
                    decision: Decision::Approve,
                    await_approval: true,
                    ..PublicationPlan::default()
                },
            ),
            (
                "notes only are approved on an approving rule",
                INPUT_VERSION,
                OnClean::Approve,
                vec![note()],
                PublicationPlan {
                    decision: Decision::Approve,
                    ..PublicationPlan::default()
                },
            ),
            (
                "one blocking finding asks for changes",
                INPUT_VERSION,
                OnClean::Await,
                vec![note(), blocking()],
                PublicationPlan {
                    decision: Decision::RequestChanges,
                    ..PublicationPlan::default()
                },
            ),
            (
                "a stored earlier input keeps every finding blocking",
                2,
                OnClean::Await,
                vec![note()],
                PublicationPlan {
                    decision: Decision::RequestChanges,
                    ..PublicationPlan::default()
                },
            ),
        ];
        for (name, version, on_clean, findings, want) in cases {
            let mut input = test_input(
                test_rule(opus(), OnFindings::RequestChanges, on_clean),
                test_pull_request(1),
            );
            input.version = version;
            if let Err(err) = validate_input(&input) {
                panic!("{name}: input version {version} must stay valid: {err}");
            }
            let artifact = Artifact {
                findings,
                ..Artifact::default()
            };
            assert_eq!(
                plan_publication(&input, &artifact, "reviewer"),
                want,
                "{name}"
            );
        }
    }

    #[test]
    fn notes_go_to_the_author_once_before_the_user_approves() {
        let handoff = NotesHandoff {
            workflow_id: "pr-review-earlier".to_owned(),
            head_sha: sha('e'),
            ..NotesHandoff::default()
        };
        let cases = [
            (
                "notes only go to the author",
                "colleague",
                false,
                None,
                vec![note()],
                PublicationPlan {
                    decision: Decision::Comment,
                    notes_to_author: true,
                    ..PublicationPlan::default()
                },
            ),
            (
                "a clean review waits for the user",
                "colleague",
                false,
                None,
                vec![],
                PublicationPlan {
                    decision: Decision::Approve,
                    await_approval: true,
                    ..PublicationPlan::default()
                },
            ),
            (
                "a blocking finding asks for changes",
                "colleague",
                false,
                None,
                vec![note(), blocking()],
                PublicationPlan {
                    decision: Decision::RequestChanges,
                    ..PublicationPlan::default()
                },
            ),
            (
                "notes after a hand-off wait for the user",
                "colleague",
                false,
                Some(handoff),
                vec![note()],
                PublicationPlan {
                    decision: Decision::Approve,
                    await_approval: true,
                    ..PublicationPlan::default()
                },
            ),
            (
                "notes of a bot wait for the user",
                "review-bot",
                true,
                None,
                vec![note()],
                PublicationPlan {
                    decision: Decision::Approve,
                    await_approval: true,
                    ..PublicationPlan::default()
                },
            ),
            (
                "an own pull request gets a plain comment",
                "reviewer",
                false,
                None,
                vec![note()],
                PublicationPlan {
                    decision: Decision::Comment,
                    own_pull_request: true,
                    ..PublicationPlan::default()
                },
            ),
        ];
        for (name, author, bot, handoff, findings, want) in cases {
            let mut rule = test_rule(opus(), OnFindings::RequestChanges, OnClean::Await);
            rule.on_notes = OnNotes::Comment;
            let mut pull_request = test_pull_request(1);
            pull_request.author = author.to_owned();
            pull_request.author_bot = bot;
            let mut input = test_input(rule, pull_request);
            input.handoff = handoff;
            let artifact = Artifact {
                findings,
                ..Artifact::default()
            };
            assert_eq!(
                plan_publication(&input, &artifact, "reviewer"),
                want,
                "{name}"
            );
        }
    }

    #[test]
    fn finding_outside_the_changed_files_never_blocks() {
        let commentable = BTreeMap::from([("src/a.go".to_owned(), vec![10])]);
        let anchored = anchor_findings(
            Artifact {
                findings: vec![
                    Finding {
                        id: "O001".to_owned(),
                        path: "src/a.go".to_owned(),
                        line: 14,
                        ..Finding::default()
                    },
                    Finding {
                        id: "S001".to_owned(),
                        path: "src/base-pull-request.go".to_owned(),
                        line: 3,
                        ..Finding::default()
                    },
                ],
                ..Artifact::default()
            },
            &commentable,
        );
        assert!(
            !anchored.findings[0].non_blocking && anchored.findings[1].non_blocking,
            "findings = {:?}",
            anchored.findings
        );
        let input = test_input(
            test_rule(opus(), OnFindings::RequestChanges, OnClean::Await),
            test_pull_request(1),
        );
        let artifact = Artifact {
            findings: anchored.findings[1..].to_vec(),
            ..Artifact::default()
        };
        let plan = plan_publication(&input, &artifact, "reviewer");
        assert_eq!(
            plan.decision,
            Decision::Approve,
            "a finding in a file the pull request does not change asked for changes: {plan:?}"
        );
    }

    #[test]
    fn plan_publication_follows_the_rule_and_github_own_pull_request_limit() {
        let findings = vec![Finding {
            id: "O001".to_owned(),
            ..Finding::default()
        }];
        let cases = [
            (
                "findings request changes",
                OnFindings::RequestChanges,
                OnClean::Approve,
                findings.clone(),
                "colleague",
                PublicationPlan {
                    decision: Decision::RequestChanges,
                    ..PublicationPlan::default()
                },
            ),
            (
                "findings comment",
                OnFindings::Comment,
                OnClean::Comment,
                findings.clone(),
                "colleague",
                PublicationPlan {
                    decision: Decision::Comment,
                    ..PublicationPlan::default()
                },
            ),
            (
                "clean approve",
                OnFindings::RequestChanges,
                OnClean::Approve,
                vec![],
                "colleague",
                PublicationPlan {
                    decision: Decision::Approve,
                    ..PublicationPlan::default()
                },
            ),
            (
                "clean await",
                OnFindings::RequestChanges,
                OnClean::Await,
                vec![],
                "colleague",
                PublicationPlan {
                    decision: Decision::Approve,
                    await_approval: true,
                    ..PublicationPlan::default()
                },
            ),
            (
                "clean comment",
                OnFindings::Comment,
                OnClean::Comment,
                vec![],
                "colleague",
                PublicationPlan {
                    decision: Decision::Comment,
                    ..PublicationPlan::default()
                },
            ),
            (
                "own pull request cannot be approved",
                OnFindings::RequestChanges,
                OnClean::Await,
                vec![],
                "reviewer",
                PublicationPlan {
                    decision: Decision::Comment,
                    own_pull_request: true,
                    ..PublicationPlan::default()
                },
            ),
            (
                "own pull request cannot get changes requested",
                OnFindings::RequestChanges,
                OnClean::Approve,
                findings.clone(),
                "reviewer",
                PublicationPlan {
                    decision: Decision::Comment,
                    own_pull_request: true,
                    ..PublicationPlan::default()
                },
            ),
        ];
        for (name, on_findings, on_clean, findings, author, want) in cases {
            let mut pull_request = test_pull_request(1);
            pull_request.author = author.to_owned();
            let input = test_input(test_rule(opus(), on_findings, on_clean), pull_request);
            let artifact = Artifact {
                findings,
                ..Artifact::default()
            };
            assert_eq!(
                plan_publication(&input, &artifact, "reviewer"),
                want,
                "{name}"
            );
        }
        let own_comment = PublicationPlan {
            decision: Decision::Comment,
            own_pull_request: true,
            ..PublicationPlan::default()
        };
        assert_eq!(
            publication_event(own_comment, 2),
            Event::FindingsOnOwnPr,
            "findings on an own pull request need the user"
        );
        let changes = PublicationPlan {
            decision: Decision::RequestChanges,
            ..PublicationPlan::default()
        };
        assert_eq!(publication_event(changes, 2), Event::ChangesRequested);
    }
}
