//! Approval, label and request scenarios of the engine against the fake integration.
//!
//! Mirrors the approval, label and request tests of `cmd/autoreview/review_integration_test.go`.

// Rust guideline compliant 2026-06-26

use autoreview_policy::Level;

use crate::messages::APPROVAL_REQUESTED_TITLE;
use crate::model::review::Decision;
use crate::protocol::fixtures::sha;
use crate::testing::harness::{integration_pull_request, start_harness};

#[tokio::test]
async fn awaited_approval_waits_for_the_user_and_publishes_once() {
    let mut h = start_harness(|_| {}).await;
    let pr = integration_pull_request("acme/manual", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the approval request", || {
        h.fake.has_note(Level::Action, APPROVAL_REQUESTED_TITLE)
    })
    .await;
    let review_id = format!("Review ID: {}", pr.review_id());
    assert!(
        h.fake.has_note(Level::Action, &review_id) && h.publications() == 0,
        "the approval request must carry the Review ID and nothing may be posted yet"
    );
    for _ in 0..3 {
        h.poll().await;
    }
    assert_eq!(
        h.started_count(),
        2,
        "a review waiting for approval was dispatched again"
    );
    let resolved = h.resolve(&pr);
    assert!(
        matches!(&resolved, Ok(resolved) if resolved.head_sha == pr.head_sha),
        "resolve = {resolved:?}"
    );
    let result = h.approve_when_awaiting(&pr).await;
    assert!(
        matches!(&result, Ok(result) if result.publication_status == "pending"),
        "approve = {result:?}"
    );
    h.wait_for("the approval", || h.publications() == 1).await;
    let (published, _, _, _) = h.fake.snapshot();
    assert_eq!(
        published[0].decision,
        Decision::Approve,
        "decision = {:?}",
        published[0].decision
    );
    h.wait_for("a repeated approval to report the publication", || {
        let Ok(again) = h.resolve(&pr) else {
            return false;
        };
        matches!(
            h.approve(&again.workflow_id, &pr),
            Ok(repeated)
                if repeated.publication_status == "published" && !repeated.review_url.is_empty()
        )
    })
    .await;
    assert_eq!(h.publications(), 1, "a repeated approval posted again");
    h.stop_engine().await;
}

#[tokio::test]
async fn newer_head_supersedes_a_waiting_approval() {
    let mut h = start_harness(|_| {}).await;
    let pr = integration_pull_request("acme/manual", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the approval request", || {
        h.fake.has_note(Level::Action, APPROVAL_REQUESTED_TITLE)
    })
    .await;
    let resolved = h.resolve(&pr).expect("the waiting review resolves");
    let mut next = pr.clone();
    next.head_sha = sha("c");
    h.fake.open(next.clone(), h.rule(&pr.repository));
    let superseded = format!("Review {} was superseded", pr.review_id());
    h.poll_until("the old wait to end", || {
        h.fake.has_note(Level::Log, &superseded)
    })
    .await;
    let approval = h.approve(&resolved.workflow_id, &pr);
    assert!(
        matches!(&approval, Err(err) if err.to_string().contains("review_superseded")),
        "approving a superseded review must fail, got {approval:?}"
    );
    let next_review_id = format!("Review ID: {}", next.review_id());
    h.poll_until("the approval request of the new head", || {
        h.fake.has_note(Level::Action, &next_review_id)
    })
    .await;
    assert_eq!(
        h.publications(),
        0,
        "nothing may be posted without an approval"
    );
    h.stop_engine().await;
}

#[tokio::test]
async fn missing_from_search_is_confirmed_before_a_wait_ends() {
    let mut h = start_harness(|_| {}).await;
    let pr = integration_pull_request("acme/manual", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the approval request", || {
        h.fake.has_note(Level::Action, APPROVAL_REQUESTED_TITLE)
    })
    .await;
    h.fake.lock().candidates.remove(&pr.key());
    for _ in 0..3 {
        h.poll().await;
    }
    if let Err(err) = h.resolve(&pr) {
        panic!("a search gap must not end a wait whose pull request is still open: {err}");
    }
    h.fake.lapse(pr.clone(), "pull_request_not_open");
    h.poll_until("the wait to end after the pull request closed", || {
        h.resolve(&pr).is_err()
    })
    .await;
    h.stop_engine().await;
}
