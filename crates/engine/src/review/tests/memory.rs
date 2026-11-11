//! Notes, prior finding and human review scenarios of the engine against the fake integration.
//!
//! Mirrors the notes, prior finding and human review tests of
//! `cmd/autoreview/review_integration_test.go`.

// Rust guideline compliant 2026-06-26

use autoreview_policy::Level;
use serde_json::json;

use crate::integrations::SessionRequest;
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
        h.fake.has_note(Level::Action, "Non-blocking notes: 1") && h.publications() == 0,
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
            && published[0].body.contains("1 non-blocking note(s)"),
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
            .has_note(Level::Digest, "Handed 1 non-blocking note(s) on acme/notes")
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
            && h.fake.has_note(Level::Action, "Non-blocking notes: 1"),
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
            && published[1].body.contains("1 non-blocking note(s)"),
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
// It settles the earlier blocking findings, on a pipeline of either protocol (#23).
#[tokio::test]
async fn open_prior_finding_carries_over_until_it_is_fixed() {
    prior_finding_carries_over_until_it_is_fixed("acme/manual", "O001", 3).await;
}

#[tokio::test]
async fn open_prior_finding_carries_over_until_it_is_fixed_on_a_codex_pipeline() {
    prior_finding_carries_over_until_it_is_fixed("codex/manual", "X001", 2).await;
}

/// Reviews three heads of a pull request on `repository`, which a rule the user approves covers.
///
/// The first review finds `prior_id`, the second keeps it open and the third finds it fixed.
/// `sessions` counts the reviewer sessions of the first two reviews.
async fn prior_finding_carries_over_until_it_is_fixed(
    repository: &str,
    prior_id: &str,
    sessions: usize,
) {
    let mut h = start_harness(|_| {}).await;
    h.fake.lock().commentable = [("src/a.go".to_owned(), vec![3])].into();
    h.set_output(reviewer_output(vec![json!({
        "severity": "high", "confidence": "high", "blocking": true, "path": "src/a.go",
        "line": 3, "title": "Broken", "body": "It breaks.",
    })]));
    let pr = integration_pull_request(repository, &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the request for changes", || h.publications() == 1)
        .await;

    h.fake.lock().discussion.threads = vec![DiscussionThread {
        key: prior_key(&pr.head_sha, prior_id),
        replies: Vec::new(),
        ..DiscussionThread::default()
    }];
    h.set_output(settling_output(Vec::new(), |_| PRIOR_OPEN));
    let mut second = pr.clone();
    second.head_sha = sha("c");
    h.fake.open(second, h.rule(&pr.repository));
    h.poll_until("the second review", || h.publications() == 2)
        .await;
    let (published, _, started, _) = h.fake.snapshot();
    assert!(
        started.len() == sessions && published[1].body.contains("incremental review"),
        "the re-review must be incremental with the incremental reviewer only, started {} \
         sessions: {}",
        started.len(),
        published[1].body
    );
    assert!(
        published[1].decision == Decision::RequestChanges
            && published[1].comments.is_empty()
            && published[1].marker.contains(":findings=1:")
            && published[1]
                .body
                .matches("**Still open, blocking**")
                .count()
                == 1
            && published[1]
                .body
                .contains(&format!("**Still open, blocking**: {prior_id} HIGH"))
            && !published[1].body.contains("S001"),
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
        reads.len() == 2 && reads[1].len() == 1 && reads[1][0].head_sha == pr.head_sha,
        "the third review must still carry the findings of the first, read {reads:?}"
    );
    h.stop_engine().await;
}

#[tokio::test]
async fn rewritten_history_gets_a_full_review_that_settles_prior_findings() {
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
    h.set_output(reviewer_output(Vec::new()));
    let mut rebased = pr.clone();
    rebased.head_sha = sha("c");
    h.fake.lock().rewritten.insert(rebased.head_sha.clone());
    h.fake.open(rebased.clone(), h.rule(&pr.repository));
    let rebased_review_id = format!("Review ID: {}", rebased.review_id());
    h.poll_until("the approval request", || {
        h.fake.has_note(Level::Action, &rebased_review_id)
    })
    .await;
    let (_, _, started, _) = h.fake.snapshot();
    assert!(
        started.len() == 4 && h.fake.has_note(Level::Action, "Scope: full"),
        "a rebased head must get a full review by every reviewer, started {} sessions",
        started.len()
    );
    let reads = h.fake.lock().discussion_reads.len();
    assert_eq!(
        reads, 1,
        "the full review must still settle the prior findings"
    );
    h.stop_engine().await;
}

// A Codex pipeline that approves on its own reviews a rewritten head in full: an incremental
// review would approve the rebased head on the diff from a head it does not extend (#237).
#[tokio::test]
async fn rewritten_history_gets_a_full_review_on_a_codex_pipeline() {
    let mut h = start_harness(|_| {}).await;
    h.fake.lock().commentable = [("src/a.go".to_owned(), vec![3])].into();
    h.set_output(reviewer_output(vec![json!({
        "severity": "high", "confidence": "high", "path": "src/a.go", "line": 3,
        "title": "Broken", "body": "It breaks.",
    })]));
    let mut pr = integration_pull_request("public/widget", &sha("a"));
    pr.author = "maintainer".to_owned();
    pr.author_type = "User".to_owned();
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the request for changes", || h.publications() == 1)
        .await;
    h.set_output(reviewer_output(Vec::new()));
    for (head, rewritten, count) in [("c", false, 2), ("d", false, 3), ("e", true, 4)] {
        let mut next = pr.clone();
        next.head_sha = sha(head);
        if rewritten {
            h.fake.lock().rewritten.insert(next.head_sha.clone());
        }
        h.fake.open(next, h.rule(&pr.repository));
        h.poll_until("the next review", || h.publications() == count)
            .await;
    }
    let (published, _, started, _) = h.fake.snapshot();
    let scopes: Vec<(Decision, bool, bool)> = published
        .iter()
        .zip(&started)
        .map(|(review, request)| {
            (
                review.decision.clone(),
                review.body.contains("full review of"),
                request
                    .prompt
                    .contains("Trusted previously reviewed head SHA"),
            )
        })
        .collect();
    assert_eq!(
        scopes,
        [
            (Decision::RequestChanges, true, false),
            (Decision::Approve, true, false),
            (Decision::Approve, false, true),
            (Decision::Approve, true, false),
        ],
        "after a request for changes and after a rewritten history the review must be full; \
         only the linear push after the approval is incremental (decision, full, previous head)"
    );
    h.stop_engine().await;
}

// A pull request retargeted to another base branch is reviewed in full: commits that joined the
// diff through the retarget were never reviewed, so an incremental approval would cover them
// (#240). A new base SHA on the same branch keeps the review incremental.
#[tokio::test]
async fn retargeted_pull_request_gets_a_full_review_before_it_is_approved() {
    let mut h = start_harness(|_| {}).await;
    h.set_output(reviewer_output(Vec::new()));
    let pr = integration_pull_request("claude/base", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the first approval", || h.publications() == 1)
        .await;
    for (head, base_sha, count) in [("c", "e", 2), ("d", "f", 3)] {
        let mut next = pr.clone();
        next.head_sha = sha(head);
        next.base_branch = "release".to_owned();
        next.base_sha = sha(base_sha);
        h.fake.open(next, h.rule(&pr.repository));
        h.poll_until("the next approval", || h.publications() == count)
            .await;
    }
    let (published, _, started, _) = h.fake.snapshot();
    let scopes: Vec<(Decision, bool, bool)> = published
        .iter()
        .zip(&started)
        .map(|(review, request)| {
            (
                review.decision.clone(),
                review.body.contains("full review of"),
                request.prompt.contains("Review scope: incremental"),
            )
        })
        .collect();
    assert_eq!(
        scopes,
        [
            (Decision::Approve, true, false),
            (Decision::Approve, true, false),
            (Decision::Approve, false, true),
        ],
        "the push after the retarget must be reviewed in full before it is approved; the next \
         push on the new base is incremental (decision, full, previous head)"
    );
    h.stop_engine().await;
}

// A retarget while a review runs supersedes that review: it reads the diff against the old
// base, so its approval would cover commits it never saw. The next review is full (#279).
#[tokio::test]
async fn retarget_during_a_review_publishes_nothing_and_the_next_review_is_full() {
    let mut h = start_harness(|_| {}).await;
    h.set_output(reviewer_output(Vec::new()));
    let pr = integration_pull_request("claude/base", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the first approval", || h.publications() == 1)
        .await;
    h.set_output(|_: &SessionRequest| String::new());
    let mut pushed = pr.clone();
    pushed.head_sha = sha("c");
    h.fake.open(pushed.clone(), h.rule(&pr.repository));
    h.poll_until("the incremental review", || h.started_count() == 2)
        .await;
    let mut retargeted = pushed.clone();
    retargeted.base_branch = "release".to_owned();
    retargeted.base_sha = sha("e");
    h.fake.open(retargeted, h.rule(&pr.repository));
    h.set_output(reviewer_output(Vec::new()));
    h.poll_until("the next approval", || h.publications() == 2)
        .await;
    let (published, _, started, _) = h.fake.snapshot();
    let scopes: Vec<bool> = started
        .iter()
        .map(|request| request.prompt.contains("Review scope: incremental"))
        .collect();
    assert_eq!(
        (scopes, published[1].body.contains("full review of")),
        (vec![false, true, false], true),
        "the review that ran during the retarget must publish nothing, and the review after it \
         must be full (incremental per started review, full publication)"
    );
    assert!(
        h.fake
            .has_note(Level::Log, "superseded (base_branch_changed)"),
        "the retarget must supersede the running review"
    );
    // The superseded review read `main` but published nothing, so `main` is reviewed again.
    h.fake.open(pushed, h.rule(&pr.repository));
    h.poll_until("the review after the retarget back", || {
        h.started_count() == 4
    })
    .await;
    h.settle().await;
    let (_, _, started, _) = h.fake.snapshot();
    assert!(
        started.len() == 4 && !started[3].prompt.contains("Review scope: incremental"),
        "the retarget back to main must get one full review, started {} sessions",
        started.len()
    );
    h.stop_engine().await;
}

// A retarget without a push gets a full review of the same head: the diff gained commits that
// no review read. It settles the findings of the published review and runs once (#280).
#[tokio::test]
async fn retarget_without_a_push_gets_a_full_review_of_the_same_head() {
    let mut h = start_harness(|_| {}).await;
    h.fake.lock().commentable = [("src/a.go".to_owned(), vec![3])].into();
    h.set_output(reviewer_output(vec![json!({
        "severity": "high", "confidence": "high", "blocking": true, "path": "src/a.go",
        "line": 3, "title": "Broken", "body": "It breaks.",
    })]));
    let pr = integration_pull_request("claude/base", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the request for changes", || h.publications() == 1)
        .await;
    h.set_output(reviewer_output(Vec::new()));
    let mut retargeted = pr.clone();
    retargeted.base_branch = "release".to_owned();
    retargeted.base_sha = sha("e");
    h.fake.open(retargeted, h.rule(&pr.repository));
    h.poll_until("the review after the retarget", || h.publications() == 2)
        .await;
    h.settle().await;
    let (published, _, started, _) = h.fake.snapshot();
    let reviews: Vec<(Decision, bool)> = published
        .iter()
        .map(|review| {
            (
                review.decision.clone(),
                review.body.contains("full review of"),
            )
        })
        .collect();
    let reads = h.fake.lock().discussion_reads.len();
    assert_eq!(
        (reviews, started.len(), reads),
        (
            vec![(Decision::RequestChanges, true), (Decision::Approve, true)],
            2,
            1
        ),
        "a retarget of the reviewed head must get one full review that settles the earlier \
         findings (decision, full; sessions; discussion reads)"
    );
    h.stop_engine().await;
}

#[tokio::test]
async fn missing_previous_review_falls_back_to_a_full_review() {
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
    h.store
        .in_tx(|transaction| {
            transaction
                .execute("DELETE FROM review_steps", [])
                .expect("the review steps are deleted");
            Ok(())
        })
        .expect("the delete of the review steps commits");
    h.set_output(reviewer_output(Vec::new()));
    let mut next = pr.clone();
    next.head_sha = sha("c");
    h.fake.open(next.clone(), h.rule(&pr.repository));
    let next_review_id = format!("Review ID: {}", next.review_id());
    h.poll_until("the approval request", || {
        h.fake.has_note(Level::Action, &next_review_id)
    })
    .await;
    let (_, _, started, _) = h.fake.snapshot();
    let reads = h.fake.lock().discussion_reads.len();
    assert!(
        started.len() == 4 && reads == 0,
        "without the previous result the review must be full and carry no prior findings: \
         started {}, discussion reads {reads}",
        started.len()
    );
    h.stop_engine().await;
}

// A resolved thread leaves an open prior finding nothing to answer.
// So the review publishes it again instead of only naming it.
#[tokio::test]
async fn open_prior_finding_without_an_open_thread_is_published_again() {
    let mut h = start_harness(|_| {}).await;
    h.fake.lock().commentable = [("src/a.go".to_owned(), vec![3, 4])].into();
    // Distinct lines keep the two reviewers' findings apart as O001 and S001.
    let opus = reviewer_output(vec![json!({
        "severity": "high", "confidence": "high", "blocking": true, "path": "src/a.go",
        "line": 3, "title": "Broken", "body": "It breaks.",
    })]);
    let sonnet = reviewer_output(vec![json!({
        "severity": "high", "confidence": "high", "blocking": true, "path": "src/a.go",
        "line": 4, "title": "Also broken", "body": "It breaks too.",
    })]);
    h.set_output(move |request: &SessionRequest| {
        if request.reviewer.id == "sonnet" {
            sonnet(request)
        } else {
            opus(request)
        }
    });
    let pr = integration_pull_request("acme/manual", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the request for changes", || h.publications() == 1)
        .await;
    h.fake.lock().discussion.threads = vec![
        DiscussionThread {
            key: prior_key(&pr.head_sha, "O001"),
            resolved: true,
            replies: Vec::new(),
        },
        DiscussionThread {
            key: prior_key(&pr.head_sha, "S001"),
            replies: Vec::new(),
            ..DiscussionThread::default()
        },
    ];
    h.set_output(settling_output(Vec::new(), |_| PRIOR_OPEN));
    let mut next = pr.clone();
    next.head_sha = sha("c");
    h.fake.open(next, h.rule(&pr.repository));
    h.poll_until("the second review", || h.publications() == 2)
        .await;
    let (published, _, _, _) = h.fake.snapshot();
    let review = &published[1];
    assert!(
        review.decision == Decision::RequestChanges
            && review.comments.len() == 1
            && review.comments[0].body.contains("Still open from O001")
            && review.marker.contains(":findings=2:"),
        "decision={:?} comments={:?} marker={}",
        review.decision,
        review.comments,
        review.marker
    );
    h.stop_engine().await;
}

#[tokio::test]
async fn human_review_signal_holds_a_clean_automatic_approval_for_the_user() {
    let mut h = start_harness(|_| {}).await;
    h.fake.lock().deleted = vec![
        "src/kept.go".to_owned(),
        "pkg/tests/OwnerTest.php".to_owned(),
    ];
    let mut pr = integration_pull_request("risky/widget", &sha("a"));
    pr.labels = vec!["Needs-Human".to_owned()];
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the approval request", || {
        h.fake.has_note(Level::Action, APPROVAL_REQUESTED_TITLE)
    })
    .await;
    assert!(
        h.fake.has_note(
            Level::Action,
            "human review signals: label needs-human; deleted test files \
             pkg/tests/OwnerTest.php."
        ) && h.publications() == 0,
        "the approval request must name the matched signals and nothing may be posted yet"
    );
    h.approve_when_awaiting(&pr)
        .await
        .expect("the waiting review is approved");
    h.wait_for("the approval", || h.publications() == 1).await;
    let (published, _, _, _) = h.fake.snapshot();
    assert!(
        published[0].decision == Decision::Approve
            && published[0].body.contains(
                "waited for a human approval because the pull request matched: label needs-human"
            ),
        "decision={:?} body={}",
        published[0].decision,
        published[0].body
    );
    h.stop_engine().await;
}

#[tokio::test]
async fn human_review_without_a_match_approves_without_the_user() {
    let mut h = start_harness(|_| {}).await;
    h.fake.lock().deleted = vec!["src/removed.go".to_owned()];
    let mut pr = integration_pull_request("risky/widget", &sha("a"));
    pr.labels = vec!["enhancement".to_owned()];
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("an automatic approval", || h.publications() == 1)
        .await;
    let (published, _, _, _) = h.fake.snapshot();
    assert!(
        published[0].decision == Decision::Approve
            && !published[0].body.contains("waited for a human")
            && !h.fake.has_note(Level::Action, APPROVAL_REQUESTED_TITLE),
        "an unmatched review must approve without the user: {}",
        published[0].body
    );
    h.stop_engine().await;
}
