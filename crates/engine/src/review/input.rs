//! The checks of a stored review input, the persisted step names and the slot state.
//!
//! Mirrors `cmd/autoreview/review.go:15-127, 143-151`.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeSet;

use autoreview_policy::{Protocol, WORKFLOW_EVENTS};
use serde::{Deserialize, Serialize};

use crate::config::{github_login_valid, identifier_valid};
use crate::error::{BoxError, Error};
use crate::integrations::GitHubReview;
use crate::model::ids::{
    INPUT_VERSION, MIN_INPUT_VERSION, PRIOR_FINDINGS_VERSION, review_identity_valid,
};
use crate::model::pull_request::{SCOPE_FULL, SCOPE_INCREMENTAL};
use crate::model::{
    Artifact, PullRequest, ReviewInput, ReviewerResult, SessionIdentity, prior_key, sha_valid,
};
use crate::verify::VerifierResult;

/// Step that stores the discussion the reviewers of prior findings read.
///
/// Step and state names are persisted; `STEP_ARTIFACT` and `STEP_PUBLICATION` are in `engine`.
pub(crate) const STEP_DISCUSSION: &str = "discussion";

/// Step that stores the lines that accept an inline comment.
pub(crate) const STEP_COMMENTABLE: &str = "commentable";

/// Step that stores the publication plan.
pub(crate) const STEP_PLAN: &str = "plan";

/// Step that stores the rendered GitHub review.
pub(crate) const STEP_RENDERED: &str = "rendered";

/// Step that stores the time of the publication.
pub(crate) const STEP_PUBLISHED_AT: &str = "published_at";

/// Step that stores the verification of the blocking claims.
pub(crate) const STEP_VERIFICATION: &str = "verification";

/// Step that stores the human review signals the review matched.
pub(crate) const STEP_HUMAN_REVIEW: &str = "human_review";

/// State that holds the deadline of the reviewer sessions.
pub(crate) const STATE_DEADLINE: &str = "collect_deadline";

/// State that holds the deadline of the verifier, which runs after the reviewers.
pub(crate) const STATE_VERIFIER_DEADLINE: &str = "verify_deadline";

/// Prefix of the state that holds the progress of one slot.
pub(crate) const SLOT_STATE_PREFIX: &str = "slot/";

/// Prefix of the steps that record a sent notification.
pub(crate) const NOTIFY_STEP_PREFIX: &str = "notify/";

/// Checks that a stored review input is complete and consistent.
///
/// The checks run in Go's order, so the first failing one names the error.
pub(crate) fn validate_input(input: &ReviewInput) -> Result<(), BoxError> {
    let pull_request = &input.pull_request;
    if input.version < MIN_INPUT_VERSION
        || input.version > INPUT_VERSION
        || !review_identity_valid(
            &pull_request.repository,
            pull_request.number,
            &pull_request.head_sha,
        )
        || input.attempt < 0
        || pull_request.review_request_id < 0
    {
        return Err(BoxError::from("review identity is invalid"));
    }
    if input.rule.match_glob.is_empty() || input.reviewers.is_empty() {
        return Err(BoxError::from("review rule is missing"));
    }
    for reviewer in &input.reviewers {
        let member = input
            .rule
            .pipeline
            .reviewers
            .iter()
            .any(|member| member == reviewer);
        if !member || reviewer.protocol != input.rule.pipeline.protocol {
            return Err(BoxError::from("reviewer is not part of the pipeline"));
        }
    }
    validate_prior(input)?;
    if let Some(handoff) = input.handoff.as_ref() {
        let workflow_valid = identifier_valid(&handoff.workflow_id);
        let head_valid = sha_valid(&handoff.head_sha);
        let head_current = !handoff.adopt || handoff.head_sha == pull_request.head_sha;
        if !workflow_valid || !head_valid || !head_current {
            return Err(BoxError::from("notes hand-off is invalid"));
        }
    }
    if input.scope.mode == SCOPE_FULL {
        if !input.scope.previous_head_sha.is_empty() {
            return Err(BoxError::from("full review names a previous head"));
        }
    } else if input.scope.mode == SCOPE_INCREMENTAL {
        if !sha_valid(&input.scope.previous_head_sha)
            || input.scope.previous_head_sha == pull_request.head_sha
        {
            return Err(BoxError::from(
                "incremental review needs a different previous head",
            ));
        }
    } else {
        return Err(BoxError::from("review scope is invalid"));
    }
    if !identifier_valid(&input.route.host)
        || !identifier_valid(&input.route.project_id)
        || !github_login_valid(&input.publisher_login)
        || input.approval_timeout_seconds <= 0
        || !input.limits.valid()
    {
        return Err(BoxError::from("review settings are invalid"));
    }
    for event in WORKFLOW_EVENTS {
        if !matches!(
            input.notifications.get(event),
            Some(autoreview_policy::Level::Action)
                | Some(autoreview_policy::Level::Digest)
                | Some(autoreview_policy::Level::Log)
        ) {
            return Err(BoxError::from(format!(
                "notification level of {} is missing",
                event.as_str()
            )));
        }
    }
    Ok(())
}

/// Accepts prior findings only from the input version that settles them.
///
/// Only reviewers that can return verdicts may get them.
fn validate_prior(input: &ReviewInput) -> Result<(), BoxError> {
    if input.prior.is_empty() {
        return Ok(());
    }
    if input.version < PRIOR_FINDINGS_VERSION
        || input.rule.pipeline.protocol != Protocol::ClaudeStructured
    {
        return Err(BoxError::from(
            "prior findings are not supported by this input",
        ));
    }
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for finding in &input.prior {
        if !seen.insert(finding.key.as_str())
            || finding.key != prior_key(&finding.head_sha, &finding.id)
            || !sha_valid(&finding.head_sha)
            || finding.github_review_id < 0
        {
            return Err(BoxError::from("prior finding is invalid"));
        }
    }
    Ok(())
}

/// Fails with `head_changed` when the reviewed head is no longer the pull request head.
///
/// A renewed review request of the same head keeps the running review.
pub(crate) fn still_wanted(expected: &PullRequest, current: &PullRequest) -> Result<(), Error> {
    if current.head_sha != expected.head_sha {
        return Err(Error::workflow("head_changed", true, None));
    }
    Ok(())
}

/// The persisted progress of one reviewer or of the verifier.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub(crate) struct SlotState {
    /// Counts the sessions of the slot; a fresh session gets the next run.
    pub(crate) run: i64,
    /// Start attempt of the current run, so a restart finds its session by name.
    #[serde(skip_serializing_if = "is_zero")]
    pub(crate) start: i64,
    /// The open session of the slot, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) session: Option<SessionIdentity>,
    /// The checked result of a reviewer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) result: Option<ReviewerResult>,
    /// The checked result of the verifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) verification: Option<VerifierResult>,
}

autoreview_go_compat::object_only_serde!(SlotState);

/// Stored outcome of combining the reviewer results.
///
/// A combine failure is stored as its code, so a resumed review fails the same way.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub(crate) struct ArtifactOutcome {
    /// The combined artifact.
    pub(crate) artifact: Artifact,
    /// Code of the combine failure, empty on success.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) error_code: String,
}

autoreview_go_compat::object_only_serde!(ArtifactOutcome);

/// Stored outcome of rendering the GitHub review.
///
/// A render failure is stored as its code, so a resumed review fails the same way.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub(crate) struct RenderOutcome {
    /// The rendered review.
    pub(crate) review: GitHubReview,
    /// Code of the render failure, empty on success.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) error_code: String,
}

autoreview_go_compat::object_only_serde!(RenderOutcome);

/// Reports whether an integer is zero, which Go omits from the JSON.
fn is_zero(value: &i64) -> bool {
    *value == 0
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use autoreview_policy::{
        Checkout, EVENTS, Event, Level, OnClean, OnFindings, Pipeline, Protocol, Reviewer, Rule,
        Trigger, TriggerKind,
    };

    use super::validate_input;
    use crate::model::ids::INPUT_VERSION;
    use crate::model::pull_request::{SCOPE_FULL, SCOPE_INCREMENTAL};
    use crate::model::{
        PriorFinding, ProjectRoute, PullRequest, ReviewInput, ReviewScope, RunLimits, prior_key,
    };

    /// One change that makes a valid input invalid.
    type Mutation = fn(&mut ReviewInput);

    fn sha(character: &str) -> String {
        character.repeat(40)
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

    fn codex() -> Reviewer {
        Reviewer {
            id: "codex".to_owned(),
            label: "Codex".to_owned(),
            protocol: Protocol::CodexEnvelope,
            agent_profile: "codex-profile".to_owned(),
            finding_prefix: "X".to_owned(),
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

    fn test_rule(reviewer: Reviewer, on_findings: OnFindings, on_clean: OnClean) -> Rule {
        let (checkout, trigger) = if reviewer.protocol == Protocol::CodexEnvelope {
            let trigger = Trigger {
                kind: TriggerKind::Label,
                label: "ai:review".to_owned(),
            };
            (Checkout::Head, trigger)
        } else {
            let trigger = Trigger {
                kind: TriggerKind::ReviewRequested,
                label: String::new(),
            };
            (Checkout::Base, trigger)
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
            head_sha: sha("a"),
            review_request_id: 7,
            title: "Add widget".to_owned(),
            url: format!("https://github.com/acme/widget/pull/{number}"),
            author: "colleague".to_owned(),
            head_branch: "feature".to_owned(),
            base_branch: "main".to_owned(),
            base_sha: sha("b"),
            ..PullRequest::default()
        }
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

    #[test]
    fn validate_input_rejects_inconsistent_input() {
        let rule = test_rule(opus(), OnFindings::RequestChanges, OnClean::Approve);
        let valid = test_input(rule, test_pull_request(2));
        if let Err(err) = validate_input(&valid) {
            panic!("{err}");
        }
        let mutations: [(&str, Mutation); 6] = [
            ("version", |input| input.version = 1),
            ("foreign reviewer", |input| input.reviewers = vec![codex()]),
            ("incremental head", |input| {
                input.scope = ReviewScope {
                    mode: SCOPE_INCREMENTAL.to_owned(),
                    previous_head_sha: input.pull_request.head_sha.clone(),
                };
            }),
            ("missing level", |input| {
                input.notifications.remove(&Event::Superseded);
            }),
            ("no approval window", |input| {
                input.approval_timeout_seconds = 0
            }),
            ("no limits", |input| input.limits = RunLimits::default()),
        ];
        for (name, mutate) in mutations {
            let mut input = valid.clone();
            input.notifications = notification_levels();
            mutate(&mut input);
            assert!(
                validate_input(&input).is_err(),
                "{name}: invalid input accepted"
            );
        }
    }

    #[test]
    fn prior_findings_need_the_input_version_that_settles_them() {
        let rule = test_rule(opus(), OnFindings::RequestChanges, OnClean::Await);
        let mut input = test_input(rule, test_pull_request(1));
        input.prior = vec![PriorFinding {
            key: prior_key(&sha("c"), "O001"),
            id: "O001".to_owned(),
            head_sha: sha("c"),
            github_review_id: 4,
            ..PriorFinding::default()
        }];
        if let Err(err) = validate_input(&input) {
            panic!("a current input with prior findings must be valid: {err}");
        }
        input.version = 3;
        assert!(
            validate_input(&input).is_err(),
            "an earlier input must not carry prior findings"
        );
        input.version = INPUT_VERSION;
        input.prior[0].key = "cccccccccccc:O002".to_owned();
        assert!(
            validate_input(&input).is_err(),
            "a prior finding whose key does not name it was accepted"
        );
    }
}
