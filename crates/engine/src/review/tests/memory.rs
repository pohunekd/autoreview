//! Notes, prior finding and human review scenarios of the engine against the fake integration.
//!
//! Mirrors the notes, prior finding and human review tests of
//! `cmd/autoreview/review_integration_test.go`.

// Rust guideline compliant 2026-06-26

use autoreview_policy::Level;
use serde_json::json;

use crate::messages::APPROVAL_REQUESTED_TITLE;
use crate::model::PullRequest;
use crate::model::review::{Decision, DiscussionThread, PRIOR_OPEN, prior_key};
use crate::protocol::fixtures::sha;
use crate::testing::fake::{reviewer_output, settling_output};
use crate::testing::harness::{Harness, integration_pull_request, start_harness};

#[tokio::test]
async fn notes_only_review_of_an_await_rule_asks_for_approval_and_publishes_the_notes() {
    let mut h = start_harness(|_| {}).await;
    h.fake.lock().commentable = [("src/a.go".to_owned(), vec![3])].into();
    h.set_output(reviewer_output(vec![json!({
        "severity": "low", "confidence": "high", "blocking": false, "path": "src/a.go",
        "line": 3, "title": "Naming", "body": "Could be clearer.",
    })]));
    let pr = integration_pull_request("acme/manual", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the approval request", || {
        h.fake.has_note(Level::Action, APPROVAL_REQUESTED_TITLE)
    })
    .await;
    assert!(
        h.fake.has_note(Level::Action, "Non-blocking notes: 2") && h.publications() == 0,
        "the approval request must list the notes and nothing may be posted yet"
    );
    h.approve_when_awaiting(&pr)
        .await
        .expect("the waiting review is approved");
    h.wait_for("the approval", || h.publications() == 1).await;
    let (published, _, _, _) = h.fake.snapshot();
    assert!(
        published[0].decision == Decision::Approve
            && published[0].comments.is_empty()
            && published[0].body.contains("2 non-blocking note(s)"),
        "decision={:?} comments={} body={}",
        published[0].decision,
        published[0].comments.len(),
        published[0].body
    );
    h.stop_engine().await;
}

/// Publishes a notes-only review of a hand-off rule and checks it reached the author.
///
/// The notes go to the author as a comment, without pinging the user.
async fn hand_notes_to_the_author(h: &Harness) -> PullRequest {
    h.fake.lock().commentable = [("src/a.go".to_owned(), vec![3])].into();
    h.set_output(reviewer_output(vec![json!({
        "severity": "low", "confidence": "high", "blocking": false, "path": "src/a.go",
        "line": 3, "title": "Naming", "body": "Could be clearer.",
    })]));
    let pr = integration_pull_request("acme/notes", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the hand-off comment", || h.publications() == 1)
        .await;
    h.wait_for("the hand-off notification", || {
        h.fake
            .has_note(Level::Digest, "Handed 2 non-blocking note(s) on acme/notes")
    })
    .await;
    let (published, notes, _, _) = h.fake.snapshot();
    assert!(
        published[0].decision == Decision::Comment
            && published[0].comments.is_empty()
            && published[0]
                .body
                .contains("Request this review again once you have decided"),
        "decision={:?} comments={} body={}",
        published[0].decision,
        published[0].comments.len(),
        published[0].body
    );
    for entry in &notes {
        assert!(
            entry.level != Level::Action,
            "notes handed to the author must not ping the user: {entry:?}"
        );
    }
    pr
}

#[tokio::test]
async fn renewed_request_after_a_notes_handoff_asks_for_approval_without_a_review() {
    let mut h = start_harness(|_| {}).await;
    let pr = hand_notes_to_the_author(&h).await;
    h.settle().await;
    assert!(
        h.started_count() == 2 && !h.fake.has_note(Level::Action, APPROVAL_REQUESTED_TITLE),
        "a hand-off must wait for the author's renewed request"
    );
    let mut renewed = pr.clone();
    renewed.review_request_id = 99;
    h.fake.open(renewed.clone(), h.rule(&pr.repository));
    h.poll_until("the approval request", || {
        h.fake.has_note(Level::Action, APPROVAL_REQUESTED_TITLE)
    })
    .await;
    assert!(
        h.started_count() == 2
            && h.publications() == 1
            && h.fake
                .has_note(Level::Action, "The author requested the review again")
            && h.fake.has_note(Level::Action, "Non-blocking notes: 2"),
        "the renewed request must reuse the hand-off review: {} sessions, {} publications",
        h.started_count(),
        h.publications()
    );
    // The notification precedes the stored wait by a moment.
    h.wait_for("the approval to be accepted", || {
        let Ok(resolved) = h.resolve(&renewed) else {
            return false;
        };
        h.approve(&resolved.workflow_id, &renewed).is_ok()
    })
    .await;
    h.wait_for("the approval", || h.publications() == 2).await;
    let (published, _, _, _) = h.fake.snapshot();
    assert!(
        published[1].decision == Decision::Approve
            && published[1].comments.is_empty()
            && published[1].body.contains("2 non-blocking note(s)"),
        "decision={:?} comments={} body={}",
        published[1].decision,
        published[1].comments.len(),
        published[1].body
    );
    h.settle().await;
    assert!(
        h.started_count() == 2 && h.publications() == 2,
        "an approved head was reviewed or published again"
    );
    h.stop_engine().await;
}

#[tokio::test]
async fn new_commits_after_a_notes_handoff_are_reviewed_and_wait_for_the_user() {
    let mut h = start_harness(|_| {}).await;
    let pr = hand_notes_to_the_author(&h).await;
    let mut next = pr.clone();
    next.head_sha = sha("c");
    next.review_request_id = 99;
    h.fake.open(next.clone(), h.rule(&pr.repository));
    h.poll_until("the approval request", || {
        h.fake.has_note(Level::Action, APPROVAL_REQUESTED_TITLE)
    })
    .await;
    assert!(
        h.started_count() == 3 && h.publications() == 1,
        "new commits must get an incremental review that waits for the user: {} sessions, {} \
         publications",
        h.started_count(),
        h.publications()
    );
    if let Err(err) = h.resolve(&next) {
        panic!("the new head must wait for approval: {err}");
    }
    h.stop_engine().await;
}

// On a rule the user approves, the next review covers only the new commits.
// It settles the earlier blocking findings.
#[tokio::test]
async fn open_prior_finding_carries_over_until_it_is_fixed() {
    let mut h = start_harness(|_| {}).await;
    h.fake.lock().commentable = [("src/a.go".to_owned(), vec![3])].into();
    h.set_output(reviewer_output(vec![json!({
        "severity": "high", "confidence": "high", "blocking": true, "path": "src/a.go",
        "line": 3, "title": "Broken", "body": "It breaks.",
    })]));
    let pr = integration_pull_request("acme/manual", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the request for changes", || h.publications() == 1)
        .await;

    h.fake.lock().discussion.threads = vec![
        DiscussionThread {
            key: prior_key(&pr.head_sha, "O001"),
            replies: Vec::new(),
            ..DiscussionThread::default()
        },
        DiscussionThread {
            key: prior_key(&pr.head_sha, "S001"),
            replies: Vec::new(),
            ..DiscussionThread::default()
        },
    ];
    h.set_output(settling_output(Vec::new(), |_| PRIOR_OPEN));
    let mut second = pr.clone();
    second.head_sha = sha("c");
    h.fake.open(second, h.rule(&pr.repository));
    h.poll_until("the second review", || h.publications() == 2)
        .await;
    let (published, _, started, _) = h.fake.snapshot();
    assert!(
        started.len() == 3 && published[1].body.contains("incremental review"),
        "the re-review must be incremental with the incremental reviewer only, started {} \
         sessions: {}",
        started.len(),
        published[1].body
    );
    assert!(
        published[1].decision == Decision::RequestChanges
            && published[1].comments.is_empty()
            && published[1].marker.contains(":findings=2:")
            && published[1]
                .body
                .matches("**Still open, blocking**")
                .count()
                == 2,
        "open prior findings must keep blocking without new threads: decision={:?} \
         comments={} marker={} body={}",
        published[1].decision,
        published[1].comments.len(),
        published[1].marker,
        published[1].body
    );

    h.set_output(reviewer_output(Vec::new()));
    let mut third = pr.clone();
    third.head_sha = sha("d");
    h.fake.open(third.clone(), h.rule(&pr.repository));
    let third_review_id = format!("Review ID: {}", third.review_id());
    h.poll_until("the approval request", || {
        h.fake.has_note(Level::Action, &third_review_id)
    })
    .await;
    assert!(
        h.fake.has_note(Level::Action, APPROVAL_REQUESTED_TITLE),
        "fixed prior findings must leave the review clean"
    );
    let reads = h.fake.lock().discussion_reads.clone();
    assert!(
        reads.len() == 2 && reads[1].len() == 2 && reads[1][0].head_sha == pr.head_sha,
        "the third review must still carry the findings of the first, read {reads:?}"
    );
    h.stop_engine().await;
}
