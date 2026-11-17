//! One eval trial: a case reviewed once through the production review flow.
//!
//! The flow runs unchanged on a throwaway store behind [`EvalIntegration`]; the trial reads
//! the stored steps afterwards and scores the findings.

// Rust guideline compliant 2026-06-26

use std::time::Instant;

use autoreview_policy::{AuthorClass, OnClean, OnFindings, OnNotes, Rule};
use tokio_util::sync::CancellationToken;

use super::case::Case;
use super::integration::EvalIntegration;
use super::matching::{MissReason, ObservedFinding, score_trial};
use super::report::{TrialReport, TrialStatus, VerifierState};
use crate::config::Credential;
use crate::discovery::SHUTDOWN;
use crate::discovery::route::HostPass;
use crate::engine::{Clock, Engine, STEP_ARTIFACT, STEP_COMBINED};
use crate::error::{Error, error_code};
use crate::integrations::Integrations;
use crate::model::pull_request::SCOPE_FULL;
use crate::model::{Artifact, Candidate, PullRequest, ReviewScope};
use crate::review::input::ArtifactOutcome;
use crate::store::RESULT_SUCCESS;
use crate::verify::{VERDICT_NOT_BLOCKING, VERDICT_REFUTED};

/// Error code of a verifier that did not finish: the review would publish unchecked claims.
pub const VERIFIER_FAILED: &str = "eval_verifier_failed";

/// Error code of a review that ended without a stored result.
const REVIEW_INCOMPLETE: &str = "eval_review_incomplete";

/// Login of the pull request author that an eval reads as.
const EVAL_AUTHOR: &str = "eval-author";

/// Returns the pull request that a case describes.
///
/// `nonce` is the review request identifier, so two runs never share a workflow or a session.
#[must_use]
pub fn eval_pull_request(case: &Case, github_host: &str, nonce: i64) -> PullRequest {
    let title = if case.title.is_empty() {
        format!("Eval case {}", case.id)
    } else {
        case.title.clone()
    };
    PullRequest {
        repository: case.repository.clone(),
        number: case.pull_request,
        head_sha: case.head_sha.clone(),
        review_request_id: nonce,
        title,
        url: format!(
            "https://{github_host}/{}/pull/{}",
            case.repository, case.pull_request
        ),
        author: EVAL_AUTHOR.to_owned(),
        head_branch: format!("eval-{}", case.id),
        base_branch: case.base_branch.clone(),
        base_sha: case.base_sha.clone(),
        ..PullRequest::default()
    }
}

/// Returns `rule` with the outcomes that never wait for a person.
///
/// Decisions are not scored, so a rule that approves, awaits, hands notes off or guards texts
/// would only hold a trial back.
#[must_use]
pub fn eval_rule(rule: &Rule) -> Rule {
    Rule {
        on_findings: OnFindings::Comment,
        on_clean: OnClean::Comment,
        on_notes: OnNotes::Unset,
        human_review: None,
        guard: None,
        ..rule.clone()
    }
}

/// Returns the observed findings of a finished review.
fn observe(artifact: &Artifact, combined: &Artifact) -> Vec<ObservedFinding> {
    let demoted = |verdict: &str| verdict == VERDICT_REFUTED || verdict == VERDICT_NOT_BLOCKING;
    artifact
        .findings
        .iter()
        .map(|finding| {
            let first = combined.findings.iter().find(|c| c.id == finding.id);
            let after = !finding.non_blocking;
            let before = after || demoted(&finding.verdict);
            let reason = if before && after {
                None
            } else if before {
                Some(MissReason::DemotedByVerifier)
            } else if first.is_some_and(|c| c.non_blocking) {
                Some(MissReason::ReportedAsNote)
            } else if !finding.skipped_by.is_empty() {
                Some(MissReason::SkippedByRules)
            } else {
                Some(MissReason::NoteAfterAnchoring)
            };
            ObservedFinding {
                id: finding.id.clone(),
                path: finding.path.clone(),
                reported_line: first.map_or(finding.line, |c| c.line),
                line: finding.line,
                severity: finding.severity.clone(),
                title: finding.title.clone(),
                verdict: finding.verdict.clone(),
                blocking_before: before,
                blocking_after: after,
                reason,
            }
        })
        .collect()
}

impl<I: Integrations + Send + Sync, C: Clock> Engine<EvalIntegration<I>, C> {
    /// Reviews `case` once under `rule` and scores the findings.
    ///
    /// Nothing is published: the integrations of the engine only read. A review that cannot be
    /// measured is a failed trial with its error code, never a score. A token that is already
    /// cancelled fails the trial with `shutdown` before anything starts, and every session
    /// that the trial started is removed before it returns, a completed trial included; one
    /// that stays is listed in `RunCounters::sessions_left`.
    pub async fn run_eval_trial(
        &self,
        cancel: &CancellationToken,
        case: &Case,
        rule: &Rule,
        credential: &Credential,
        repeat: i64,
    ) -> TrialReport {
        let started = Instant::now();
        let workflow_id = self.integration.pull_request().workflow_id(repeat);
        let measured = if cancel.is_cancelled() {
            Err(Error::workflow(SHUTDOWN, false, None))
        } else {
            self.measure(cancel, rule, credential, repeat, &workflow_id)
                .await
        };
        self.integration
            .remove_sessions(self.config.session_timeout)
            .await;
        let duration_ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
        let sessions = self.integration.trial_sessions();
        let mut trial = TrialReport {
            repeat,
            status: TrialStatus::Failed,
            error_code: String::new(),
            duration_ms,
            sessions,
            verifier: VerifierState::None,
            findings: Vec::new(),
            stages: None,
        };
        match measured {
            Ok((findings, verifier)) => {
                trial.status = TrialStatus::Completed;
                trial.stages = Some(score_trial(&case.expect, &findings));
                trial.findings = findings;
                trial.verifier = verifier;
            }
            Err(error) => error_code(&error).clone_into(&mut trial.error_code),
        }
        trial
    }

    /// Runs the review flow on a stored input and reads its result.
    async fn measure(
        &self,
        cancel: &CancellationToken,
        rule: &Rule,
        credential: &Credential,
        repeat: i64,
        workflow_id: &str,
    ) -> Result<(Vec<ObservedFinding>, VerifierState), Error> {
        let pr = self.integration.pull_request().clone();
        let mut pass = HostPass::default();
        let sources = &rule.pipeline.project_sources;
        let route = (self.resolve_route(
            cancel,
            &pr.repository,
            Some(&AuthorClass::Trusted),
            sources,
            &mut pass,
        ))
        .await?;
        let candidate = Candidate {
            pull_request: pr,
            rule: rule.clone(),
            author_class: Some(AuthorClass::Trusted),
            authors: None,
        };
        let scope = ReviewScope {
            mode: SCOPE_FULL.to_owned(),
            previous_head_sha: String::new(),
        };
        let mut input = self.build_input(&candidate, route, scope, Vec::new(), None, repeat)?;
        input.credential.clone_from(&credential.name);
        input.publisher_login.clone_from(&credential.login);
        self.store
            .in_tx(|tx| self.store.enqueue_review(tx, workflow_id, &input))?;
        let stored = self.store.get_review(workflow_id)?.ok_or_else(incomplete)?;
        self.run_review(cancel, stored).await?;
        let row = self.store.get_review(workflow_id)?.ok_or_else(incomplete)?;
        if row.result != RESULT_SUCCESS {
            let code = if row.failure.is_empty() {
                REVIEW_INCOMPLETE
            } else {
                &row.failure
            };
            return Err(Error::workflow(code.to_owned(), true, None));
        }
        let artifact: Artifact =
            (self.store.load_step(workflow_id, STEP_ARTIFACT)?).ok_or_else(incomplete)?;
        let combined: Option<ArtifactOutcome> = self.store.load_step(workflow_id, STEP_COMBINED)?;
        let verifier = match &artifact.verification {
            Some(check) if !check.error_code.is_empty() => {
                return Err(Error::workflow(VERIFIER_FAILED, true, None));
            }
            Some(_) => VerifierState::Ran,
            None if input.verifies() => VerifierState::NotNeeded,
            None => VerifierState::None,
        };
        // Without the combined step the reviewers' own lines are unknown, so the trial is not
        // measured rather than scored on the anchored lines.
        let combined = combined.ok_or_else(incomplete)?.artifact;
        Ok((observe(&artifact, &combined), verifier))
    }
}

/// Returns the error of a review that left no stored result.
fn incomplete() -> Error {
    Error::workflow(REVIEW_INCOMPLETE, true, None)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    use super::{eval_pull_request, eval_rule};
    use crate::engine::{Engine, SystemClock};
    use crate::eval::case::{Case, Expect, Expected};
    use crate::eval::integration::{EvalIntegration, RunCounters};
    use crate::eval::report::TrialStatus;
    use crate::integrations::{ChangedFile, Integrations, SessionRequest};
    use crate::protocol::fixtures::sha;
    use crate::store::Store;
    use crate::testing::fake::{FakeIntegration, reviewer_output};
    use crate::testing::harness::integration_config;

    fn case(id: &str, expect: Expect) -> Case {
        Case {
            id: id.to_owned(),
            repository: "acme/widget".to_owned(),
            pull_request: 7,
            base_branch: "main".to_owned(),
            head_ref: "refs/pull/7/head".to_owned(),
            base_sha: sha("b"),
            head_sha: sha("a"),
            pipelines: Vec::new(),
            title: String::new(),
            expect,
        }
    }

    /// No trial reaches a publication, a notification or a lookup of a published review while
    /// the findings are scored, a session fetches the ref of its case, and a trial that
    /// fails, runs out of budget or is cancelled leaves no session open.
    #[tokio::test]
    async fn trials_score_findings_and_never_publish() {
        let dir = tempfile::tempdir().expect("create the store directory");
        let config = integration_config(dir.path());
        let store = Arc::new(
            Store::open(
                &dir.path().join("eval.db"),
                Box::new(time::OffsetDateTime::now_utc),
            )
            .expect("open the store"),
        );
        let host = config.pohunek_hosts.first().expect("a host").clone();
        let fake = Arc::new(FakeIntegration::new());
        let finding = json!({"severity": "high", "confidence": "high", "blocking": true,
            "path": "src/a.rs", "line": 45, "title": "Race", "body": "Details."});
        let defect = reviewer_output(vec![finding]);
        let none = reviewer_output(Vec::new());
        let stop = CancellationToken::new();
        let stop_in_review = stop.clone();
        {
            let mut state = fake.lock();
            state.policy.clone_from(&config.policy);
            state.fetch_ref_hosts.insert(host.clone());
            state.commentable.insert("src/a.rs".to_owned(), vec![45]);
            state.changed.push(ChangedFile {
                path: "src/a.rs".to_owned(),
                status: "modified".to_owned(),
                additions: 1,
                ..ChangedFile::default()
            });
            state.output = Some(Arc::new(move |request: &SessionRequest| {
                // Only the first reviewer of the pipeline sees the defect.
                match request.pull_request.head_branch.as_str() {
                    "eval-defect" if request.reviewer.id == "opus" => defect(request),
                    // The run is interrupted while the reviewers work.
                    "eval-cancelled" => {
                        stop_in_review.cancel();
                        none(request)
                    }
                    _ => none(request),
                }
            }));
        }
        let rule = eval_rule(
            config
                .policy
                .as_ref()
                .and_then(|p| p.rule_for_pipeline("manual"))
                .expect("a rule"),
        );
        let credential = config
            .credentials
            .serving("acme/widget")
            .expect("a credential")
            .clone();
        let counters = Arc::new(RunCounters::default());
        let wanted = Expect::Blocking(vec![Expected {
            path: "src/a.rs".to_owned(),
            first_line: 40,
            last_line: 52,
            description: "d".to_owned(),
        }]);
        let trial = |case: Case, nonce: i64, budget: usize| {
            let pr = eval_pull_request(&case, "github.com", nonce);
            let head_ref = case.head_ref.clone();
            let wrapper = EvalIntegration::new(
                Arc::clone(&fake),
                pr,
                head_ref,
                Arc::clone(&counters),
                budget,
            );
            let engine = Engine::new(
                config.clone(),
                Arc::clone(&store),
                Arc::new(wrapper),
                SystemClock,
            );
            (engine, case)
        };
        let cancel = CancellationToken::new();
        let mut defect_case = case("defect", wanted);
        defect_case.head_ref = "refs/heads/eval-cases/defect".to_owned();
        let (engine, defect_case) = trial(defect_case, 11, 100);
        let first = engine
            .run_eval_trial(&cancel, &defect_case, &rule, &credential, 1)
            .await;
        assert_eq!(first.status, TrialStatus::Completed, "{}", first.error_code);
        let stages = first.stages.expect("a completed trial has a score");
        assert_eq!(stages.after_verification.caught, [0]);
        let (engine, clean_case) = trial(case("clean", Expect::Clean), 12, 100);
        let second = engine
            .run_eval_trial(&cancel, &clean_case, &rule, &credential, 1)
            .await;
        assert_eq!(
            second.status,
            TrialStatus::Completed,
            "{}",
            second.error_code
        );
        let after = second.stages.expect("a score").after_verification;
        assert_eq!(after.false_blocking, Some(false));
        assert_eq!(counters.would_publish.load(Ordering::SeqCst), 2);
        let (published, notes, started, open) = fake.snapshot();
        assert!(
            published.is_empty() && notes.is_empty(),
            "an eval wrote to GitHub or Matrix"
        );
        assert_eq!((fake.lock().lookups, open), (0, 0));
        for request in &started {
            let expected = match request.pull_request.head_branch.as_str() {
                "eval-defect" => "refs/heads/eval-cases/defect",
                _ => "refs/pull/7/head",
            };
            assert_eq!(request.head_ref(), expected);
        }
        // The session budget refuses the start past it, and the trial fails.
        let (engine, budget_case) = trial(case("budget", Expect::Clean), 13, 0);
        let third = engine
            .run_eval_trial(&cancel, &budget_case, &rule, &credential, 1)
            .await;
        assert_eq!(third.status, TrialStatus::Failed);
        // A host that cannot fetch the head would review whatever it holds.
        fake.lock().fetch_ref_hosts.clear();
        let (engine, unpinned_case) = trial(case("unpinned", Expect::Clean), 14, 100);
        let fourth = engine
            .run_eval_trial(&cancel, &unpinned_case, &rule, &credential, 1)
            .await;
        assert_eq!(fourth.status, TrialStatus::Failed);
        // A run cancelled while the reviewers work leaves no session behind, and a trial that
        // starts after the cancellation starts nothing.
        fake.lock().fetch_ref_hosts.insert(host);
        let (engine, cancelled_case) = trial(case("cancelled", Expect::Clean), 15, 100);
        let fifth = engine
            .run_eval_trial(&stop, &cancelled_case, &rule, &credential, 1)
            .await;
        assert_eq!(fifth.status, TrialStatus::Failed);
        let started_before = fake.snapshot().2.len();
        let (engine, late_case) = trial(case("late", Expect::Clean), 16, 100);
        let sixth = engine
            .run_eval_trial(&stop, &late_case, &rule, &credential, 1)
            .await;
        assert_eq!(
            (sixth.status, sixth.error_code.as_str()),
            (TrialStatus::Failed, "shutdown")
        );
        let (published, _, started, open) = fake.snapshot();
        assert_eq!(
            (published.len(), started.len(), open),
            (0, started_before, 0)
        );
        // A completed trial whose sessions the host keeps lists them, and it stays completed.
        assert!(counters.sessions_left().is_empty());
        fake.lock().keeping_sessions = true;
        let (engine, kept_case) = trial(case("kept", Expect::Clean), 17, 100);
        let seventh = engine
            .run_eval_trial(&cancel, &kept_case, &rule, &credential, 1)
            .await;
        assert_eq!(seventh.status, TrialStatus::Completed);
        let open = fake.snapshot().3;
        assert!(open > 0 && counters.sessions_left().len() == open);
        // A later removal that succeeds takes the sessions out of the list again.
        fake.lock().keeping_sessions = false;
        engine
            .integration
            .remove_sessions(config.session_timeout)
            .await;
        assert!(counters.sessions_left().is_empty());
        assert_eq!(fake.snapshot().3, 0);
        // A session that only the lookup of a pending request finds is listed when its removal
        // fails, and a later call takes it out of the list although the lookup finds nothing.
        let request = fake.snapshot().2.remove(0);
        fake.lock()
            .start_failures
            .push_back(("pohunek_request_failed".to_owned(), false));
        let start = engine.integration.ensure_session(&cancel, &request).await;
        assert!(start.is_err());
        {
            let mut state = fake.lock();
            state.sessions.insert("s-lost".to_owned(), request);
            state.finding_sessions = true;
            state.keeping_sessions = true;
        }
        engine
            .integration
            .remove_sessions(config.session_timeout)
            .await;
        assert!(
            counters
                .sessions_left()
                .iter()
                .any(|(_, id)| id == "s-lost")
        );
        {
            let mut state = fake.lock();
            state.sessions.clear();
            state.keeping_sessions = false;
        }
        engine
            .integration
            .remove_sessions(config.session_timeout)
            .await;
        assert!(counters.sessions_left().is_empty());
    }
}
