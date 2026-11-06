//! Core scenarios of the engine: serve, discovery, reviews and their publication.
//!
//! Mirrors `cmd/autoreview/engine_test.go:67-75` and the core tests of
//! `cmd/autoreview/review_integration_test.go`.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use autoreview_policy::Level;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::engine::{Engine, SystemClock};
use crate::error::{Error, error_code};
use crate::integrations::SessionRequest;
use crate::model::RejectedCandidate;
use crate::model::review::Decision;
use crate::protocol::fixtures::sha;
use crate::review::input::NOTIFY_STEP_PREFIX;
use crate::testing::fake::{count_notes, reviewer_output};
use crate::testing::harness::{integration_pull_request, start_harness};

/// Output of a reviewer session that has not finished: Go's nil output.
fn unfinished(_: &SessionRequest) -> String {
    String::new()
}

// A second engine on the same store is refused while the first one serves.
#[tokio::test]
async fn second_engine_on_the_same_store_is_refused() {
    let mut h = start_harness(|_| {}).await;
    let second = Arc::new(Engine::new(
        h.config(),
        Arc::clone(&h.store),
        Arc::clone(&h.fake),
        SystemClock,
    ));
    let err = second
        .serve(&CancellationToken::new())
        .await
        .expect_err("the second engine is refused");
    assert_eq!(
        error_code(&err),
        "engine_already_running",
        "second engine error = {err:?}"
    );
    h.stop_engine().await;
}

#[tokio::test]
async fn clean_review_of_an_approving_rule_is_approved_once_without_the_user() {
    let mut h = start_harness(|_| {}).await;
    let pr = integration_pull_request("acme/widget", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("an automatic approval", || h.publications() == 1)
        .await;
    h.wait_for("the approval notification", || {
        h.fake.has_note(Level::Digest, "Approved acme/widget PR")
    })
    .await;
    let (published, notes, started, open) = h.fake.snapshot();
    assert!(
        published[0].decision == Decision::Approve
            && published[0].comments.is_empty()
            && started.len() == 2
            && open == 0,
        "decision={:?} comments={} started={} open={open}",
        published[0].decision,
        published[0].comments.len(),
        started.len()
    );
    for entry in &notes {
        assert!(
            entry.level != Level::Action,
            "an automatic approval must not ping the user: {entry:?}"
        );
    }
    for _ in 0..3 {
        h.poll().await;
    }
    assert!(
        h.started_count() == 2 && h.publications() == 1,
        "a published head was reviewed again"
    );
    h.stop_engine().await;
}

#[tokio::test]
async fn review_sessions_use_the_configured_name_and_branch_prefixes() {
    let mut h = start_harness(|cfg| {
        cfg.session_name_prefix = "team-review-".to_owned();
        cfg.session_branch_prefix = "team/review/".to_owned();
    })
    .await;
    let pr = integration_pull_request("acme/widget", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("an automatic approval", || h.publications() == 1)
        .await;
    let (_, _, started, _) = h.fake.snapshot();
    assert_eq!(started.len(), 2, "started = {started:?}");
    for request in &started {
        let token = format!(
            "{}-{}-run{}",
            request.workflow_id, request.reviewer.id, request.run
        );
        assert_eq!(request.name(), format!("team-review-{token}"));
        assert_eq!(request.branch(), format!("team/review/{token}"));
    }
    h.stop_engine().await;
}

#[tokio::test]
async fn findings_request_changes_and_the_next_head_gets_a_full_review() {
    let mut h = start_harness(|_| {}).await;
    h.set_output(reviewer_output(vec![json!({
        "severity": "high", "confidence": "high", "path": "src/a.go", "line": 3,
        "title": "Broken", "body": "It breaks.",
    })]));
    h.fake.lock().commentable = [("src/a.go".to_owned(), vec![3])].into();
    let pr = integration_pull_request("acme/widget", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("a request for changes", || h.publications() == 1)
        .await;
    let (published, _, _, _) = h.fake.snapshot();
    assert!(
        published[0].decision == Decision::RequestChanges && published[0].comments.len() == 1,
        "decision={:?} comments={}",
        published[0].decision,
        published[0].comments.len()
    );
    h.set_output(reviewer_output(Vec::new()));
    let mut next = pr.clone();
    next.head_sha = sha("c");
    h.fake.open(next, h.rule(&pr.repository));
    h.poll_until("the review of the next head", || h.publications() == 2)
        .await;
    let (_, _, started, _) = h.fake.snapshot();
    assert_eq!(
        started.len(),
        4,
        "after a request for changes both reviewers must review in full, started {} sessions",
        started.len()
    );
    let (published, _, _, _) = h.fake.snapshot();
    assert!(
        published[1].decision == Decision::Approve && published[1].body.contains("full review"),
        "second review = {:?}: {}",
        published[1].decision,
        published[1].body
    );
    h.stop_engine().await;
}

// Two reviewers that report the same defect publish it once, as the first reviewer's finding.
#[tokio::test]
async fn same_finding_of_two_reviewers_is_published_once() {
    let mut h = start_harness(|_| {}).await;
    let opus = reviewer_output(vec![json!({
        "severity": "high", "confidence": "medium", "path": "src/a.go", "line": 3,
        "title": "Broken", "body": "It breaks.",
    })]);
    let sonnet = reviewer_output(vec![json!({
        "severity": "high", "confidence": "high", "path": "src/a.go", "line": 3,
        "title": "Broken on empty input", "body": "It breaks on an empty input.",
    })]);
    h.set_output(move |request: &SessionRequest| {
        if request.reviewer.id == "sonnet" {
            sonnet(request)
        } else {
            opus(request)
        }
    });
    h.fake.lock().commentable = [("src/a.go".to_owned(), vec![3])].into();
    let pr = integration_pull_request("acme/widget", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("a request for changes", || h.publications() == 1)
        .await;
    let (published, _, _, _) = h.fake.snapshot();
    let review = &published[0];
    let heading = "**O001 HIGH — Broken on empty input** (confidence: high, Opus + Sonnet)\n\n\
                   It breaks on an empty input.";
    assert!(
        review.decision == Decision::RequestChanges
            && review.comments.len() == 1
            && review.comments[0].body.starts_with(heading)
            && review.marker.contains(":findings=1:")
            && review
                .body
                .contains("1 blocking finding(s) are posted as inline comments.")
            && review.body.contains("**Opus:** ")
            && !review.body.contains("**Sonnet:** "),
        "decision={:?} comments={:?} marker={} body={}",
        review.decision,
        review.comments,
        review.marker,
        review.body
    );
    h.stop_engine().await;
}

#[tokio::test]
async fn approved_head_is_followed_by_an_incremental_review() {
    let mut h = start_harness(|_| {}).await;
    let pr = integration_pull_request("acme/widget", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the first approval", || h.publications() == 1)
        .await;
    let mut next = pr.clone();
    next.head_sha = sha("c");
    next.review_request_id = 12;
    h.fake.open(next, h.rule(&pr.repository));
    h.poll_until("the incremental approval", || h.publications() == 2)
        .await;
    let (published, _, started, _) = h.fake.snapshot();
    assert!(
        started.len() == 4 && published[1].body.contains("incremental review"),
        "an automatically approved re-review must use every reviewer, started {} sessions: {}",
        started.len(),
        published[1].body
    );
    h.stop_engine().await;
}

#[tokio::test]
async fn stuck_session_cleanup_is_reported_and_the_review_still_publishes() {
    let mut h = start_harness(|_| {}).await;
    h.fake.lock().remove_failing.insert("s-1".to_owned());
    let pr = integration_pull_request("acme/widget", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the publication despite a stuck session", || {
        h.publications() == 1
    })
    .await;
    assert!(
        h.fake
            .has_note(Level::Action, "Pohunek could not remove reviewer sessions")
            && h.fake
                .has_note(Level::Action, "runtime_supervision_ambiguous"),
        "the stuck session was not reported for manual removal"
    );
    h.stop_engine().await;
}

#[tokio::test]
async fn unusable_reviewer_output_gets_a_fresh_run() {
    let mut h = start_harness(|_| {}).await;
    let valid = reviewer_output(Vec::new());
    h.set_output(move |request: &SessionRequest| {
        if request.reviewer.id == "opus" && request.run == 1 {
            return "{\"protocol\":\"broken\"}\n".to_owned();
        }
        valid(request)
    });
    let pr = integration_pull_request("acme/widget", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the publication after a reviewer retry", || {
        h.publications() == 1
    })
    .await;
    let (_, _, started, _) = h.fake.snapshot();
    let runs = started
        .iter()
        .filter(|request| request.reviewer.id == "opus")
        .count();
    assert_eq!(runs, 2, "opus ran {runs} times");
    h.stop_engine().await;
}

/// A reviewer output that passes the schema but fails the engine's own limits, such as a
/// multiline title, is a model mistake that a fresh run can fix.
#[tokio::test]
async fn reviewer_output_over_the_limits_gets_a_fresh_run() {
    let mut h = start_harness(|_| {}).await;
    let valid = reviewer_output(Vec::new());
    let invalid = reviewer_output(vec![json!({
        "severity": "high", "confidence": "high", "blocking": true, "path": "src/a.go",
        "line": 3, "title": "Bug\nsecond line", "body": "Details.",
    })]);
    h.set_output(move |request: &SessionRequest| {
        if request.reviewer.id == "opus" && request.run == 1 {
            return invalid(request);
        }
        valid(request)
    });
    let pr = integration_pull_request("acme/widget", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the publication after a reviewer retry", || {
        h.publications() == 1
    })
    .await;
    let (_, _, started, _) = h.fake.snapshot();
    let runs = started
        .iter()
        .filter(|request| request.reviewer.id == "opus")
        .count();
    assert_eq!(runs, 2, "opus ran {runs} times");
    h.stop_engine().await;
}

/// The most findings with the longest bodies still give a short review body.
///
/// Every finding is a note at the body limit, and every reviewer request states the limits.
#[tokio::test]
async fn review_with_the_most_findings_keeps_a_short_body() {
    let mut h = start_harness(|_| {}).await;
    let cfg = h.config();
    assert!(
        cfg.max_findings * cfg.max_body_characters > cfg.github_body_max_bytes,
        "the note bodies alone must exceed the GitHub body limit"
    );
    let count = usize::try_from(cfg.max_findings).expect("the finding limit is positive");
    let longest = usize::try_from(cfg.max_body_characters).expect("the body limit is positive");
    let notes: Vec<_> = (1..=count)
        .map(|line| {
            json!({
                "severity": "low", "confidence": "high", "blocking": false, "path": "src/a.go",
                "line": line, "title": format!("Note {line}"), "body": "n".repeat(longest),
            })
        })
        .collect();
    let opus = reviewer_output(notes);
    let sonnet = reviewer_output(Vec::new());
    h.set_output(move |request: &SessionRequest| {
        if request.reviewer.id == "sonnet" {
            sonnet(request)
        } else {
            opus(request)
        }
    });
    let pr = integration_pull_request("acme/widget", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the review with the most findings", || {
        h.publications() == 1
    })
    .await;
    let (published, _, started, _) = h.fake.snapshot();
    let body = &published[0].body;
    assert!(
        published[0].decision == Decision::Approve
            && body.contains(&format!("{count} non-blocking note(s)"))
            && body.contains(&format!("**O{count:03} LOW — Note "))
            && !body.contains("nnnn")
            && body.chars().count() < longest,
        "decision={:?} length={} body={body}",
        published[0].decision,
        body.chars().count()
    );
    let limits = format!("a body of at most {} characters", cfg.max_body_characters);
    assert!(
        started.len() == 2
            && started
                .iter()
                .all(|request| request.prompt.contains(&limits)),
        "every reviewer request must state the limits: {started:?}"
    );
    h.stop_engine().await;
}

#[tokio::test]
async fn base_fetch_locked_by_a_concurrent_review_is_retried_on_a_fresh_branch() {
    let mut h = start_harness(|_| {}).await;
    h.fake.lock().start_fail = Some(Arc::new(|request: &SessionRequest| {
        if request.reviewer.id == "opus" && request.start <= 1 {
            return Err(Error::workflow(
                "pohunek_base_fetch_ref_locked",
                false,
                None,
            ));
        }
        Ok(())
    }));
    let pr = integration_pull_request("acme/widget", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the publication after a locked base fetch", || {
        h.publications() == 1
    })
    .await;
    let branches: BTreeSet<String> = h
        .fake
        .lock()
        .attempts
        .iter()
        .filter(|request| request.reviewer.id == "opus")
        .map(SessionRequest::branch)
        .collect();
    // Pohunek keeps the failed start's branch, so reusing it would check out a stale branch
    // instead of the fresh base.
    assert_eq!(branches.len(), 2, "opus started on branches {branches:?}");
    h.stop_engine().await;
}

#[tokio::test]
async fn own_pull_request_gets_a_comment_and_findings_ping_the_author() {
    let mut h = start_harness(|_| {}).await;
    h.fake.lock().commentable = [("a.go".to_owned(), vec![1])].into();
    h.set_output(reviewer_output(vec![json!({
        "severity": "medium", "confidence": "high", "path": "a.go", "line": 1,
        "title": "Fix me", "body": "Details.",
    })]));
    let mut pr = integration_pull_request("acme/widget", &sha("a"));
    pr.author = h.engine.config.github_login.clone();
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the comment", || h.publications() == 1).await;
    let (published, _, _, _) = h.fake.snapshot();
    assert!(
        published[0].decision == Decision::Comment
            && published[0].body.contains("own pull request"),
        "own pull request review = {:?}: {}",
        published[0].decision,
        published[0].body
    );
    h.wait_for("the action notification", || {
        h.fake.has_note(Level::Action, "Commented on acme/widget")
    })
    .await;
    h.stop_engine().await;
}

#[tokio::test]
async fn rejected_pull_request_is_reported_once_and_does_not_block_others() {
    let mut h = start_harness(|_| {}).await;
    let broken = integration_pull_request("acme/widget", &sha("e"));
    h.fake.lock().rejected = vec![RejectedCandidate {
        repository: broken.repository.clone(),
        number: broken.number,
        code: "github_response_invalid".to_owned(),
    }];
    let pr = integration_pull_request("acme/widget", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the review of the valid pull request", || {
        h.publications() == 1
    })
    .await;
    let (_, notes, _, _) = h.fake.snapshot();
    let fragment = format!("{} is not started", broken.key());
    let reports = count_notes(&notes, Level::Action, &fragment);
    assert_eq!(
        reports, 1,
        "the rejected pull request was reported {reports} times"
    );
    h.stop_engine().await;
}

// A transient outage longer than the step retry window ends the step with its own retryable
// code instead of pausing the review.
#[tokio::test]
async fn outage_longer_than_the_step_retry_window_is_retried_later() {
    let mut h = start_harness(|cfg| cfg.step_retry = Duration::from_secs(2)).await;
    h.set_output(unfinished);
    let pr = integration_pull_request("acme/widget", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the reviewer sessions", || h.started_count() == 2)
        .await;
    h.fake.lock().transient_until = Some(Instant::now() + Duration::from_secs(6));
    h.wait_for_polling("the retry notice", || {
        h.fake.has_note(Level::Log, "Next automatic attempt")
            && h.fake.has_note(Level::Log, "github_request_failed")
    })
    .await;
    h.set_output(reviewer_output(Vec::new()));
    h.poll_until("the retried review", || h.publications() == 1)
        .await;
    h.stop_engine().await;
}

// A restart while reviewers still run resumes the review from its stored sessions: no reviewer
// starts again and the review publishes exactly once.
#[tokio::test]
async fn restart_during_collect_resumes_without_starting_reviewers_again() {
    let mut h = start_harness(|_| {}).await;
    h.set_output(unfinished);
    let pr = integration_pull_request("acme/widget", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("both reviewer sessions", || h.started_count() == 2)
        .await;
    // Stopping before the started notice is stored sends it again after the restart.
    let started_step = format!("{NOTIFY_STEP_PREFIX}started");
    h.wait_for("the started notice to be stored", || {
        h.step_recorded(&started_step)
    })
    .await;
    h.stop_engine().await;
    h.start();
    h.set_output(reviewer_output(Vec::new()));
    h.poll_until("the publication after the restart", || {
        h.publications() == 1
    })
    .await;
    let (published, notes, started, _) = h.fake.snapshot();
    assert!(
        published.len() == 1 && published[0].decision == Decision::Approve,
        "published = {published:?}"
    );
    assert_eq!(
        started.len(),
        2,
        "the restart started {} sessions in total, want 2",
        started.len()
    );
    let notices = count_notes(&notes, Level::Log, "GitHub review started");
    assert_eq!(notices, 1, "the started notice was sent {notices} times");
    h.wait_for("the completed sessions to be removed", || {
        h.fake.snapshot().3 == 0
    })
    .await;
    h.stop_engine().await;
}
