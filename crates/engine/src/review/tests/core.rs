//! Core scenarios of the engine: serve, discovery, reviews and their publication.
//!
//! Mirrors `cmd/autoreview/engine_test.go:67-75` and the core tests of
//! `cmd/autoreview/review_integration_test.go`.

// Rust guideline compliant 2026-06-26

use std::sync::Arc;

use autoreview_policy::Level;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::engine::{Engine, SystemClock};
use crate::error::error_code;
use crate::integrations::SessionRequest;
use crate::model::review::Decision;
use crate::protocol::fixtures::sha;
use crate::testing::fake::reviewer_output;
use crate::testing::harness::{integration_pull_request, start_harness};

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
        published[0].decision == Decision::RequestChanges && published[0].comments.len() == 2,
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
