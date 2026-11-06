//! Approval, label and request scenarios of the engine against the fake integration.
//!
//! Mirrors the approval, label and request tests of `cmd/autoreview/review_integration_test.go`.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use autoreview_policy::{EVENTS, Event, Level, Rule};

use crate::integrations::SessionRequest;
use crate::messages::APPROVAL_REQUESTED_TITLE;
use crate::model::ids::INPUT_VERSION;
use crate::model::pull_request::SCOPE_FULL;
use crate::model::review::Decision;
use crate::model::{ProjectRoute, PullRequest, ReviewInput, ReviewScope, RunLimits};
use crate::protocol::fixtures::sha;
use crate::testing::fake::reviewer_output;
use crate::testing::harness::{integration_pull_request, start_harness};

/// Output of a reviewer session that has not finished: Go's nil output.
fn unfinished(_: &SessionRequest) -> String {
    String::new()
}

/// The notification levels of the Go `notificationLevels` (`logic_test.go:23-36`).
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

/// The review input of the Go `testInput` (`logic_test.go:54-61`).
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

#[tokio::test]
async fn approval_expires_and_a_late_approval_is_rejected() {
    let mut h = start_harness(|_| {}).await;
    let pr = integration_pull_request("acme/manual", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    let mut input = test_input(h.rule(&pr.repository), pr.clone());
    input.approval_timeout_seconds = 1;
    input.limits = h.engine.config.run_limits();
    let workflow_id = pr.workflow_id(0);
    h.store
        .in_tx(|transaction| h.store.enqueue_review(transaction, &workflow_id, &input))
        .expect("the review is enqueued");
    let level = input.notifications[&Event::ApprovalExpired];
    h.wait_for("the approval to expire", || {
        h.fake.has_note(level, "expired after 1s without a reply")
    })
    .await;
    let approval = h.approve(&workflow_id, &pr);
    assert!(
        matches!(&approval, Err(err) if err.to_string().contains("approval_expired")),
        "a late approval must be rejected, got {approval:?}"
    );
    assert_eq!(h.publications(), 0, "an expired approval posted a review");
    h.stop_engine().await;
}

#[tokio::test]
async fn removed_label_stops_the_review_without_publishing() {
    let mut h = start_harness(|_| {}).await;
    h.set_output(unfinished);
    let mut pr = integration_pull_request("solo/tool", &sha("a"));
    pr.review_request_id = 0;
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the reviewer session", || h.started_count() == 1)
        .await;
    h.fake.lapse(pr.clone(), "review_label_removed");
    h.wait_for("the superseded notification", || {
        h.fake
            .has_note(Level::Log, "superseded (review_label_removed)")
    })
    .await;
    let (_, _, _, open) = h.fake.snapshot();
    assert!(
        h.publications() == 0 && open == 0,
        "published={} open sessions={open}",
        h.publications()
    );
    h.stop_engine().await;
}

#[tokio::test]
async fn stopped_review_is_retried_by_head_prefix() {
    let mut h = start_harness(|_| {}).await;
    let valid = reviewer_output(Vec::new());
    let broken = Arc::new(AtomicBool::new(true));
    let failing = Arc::clone(&broken);
    h.set_output(move |request: &SessionRequest| {
        if failing.load(Ordering::SeqCst) {
            return "{\"protocol\":\"broken\"}\n".to_owned();
        }
        valid(request)
    });
    let pr = integration_pull_request("acme/widget", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the stop notification", || {
        h.fake
            .has_note(Level::Action, "No further automatic attempt is scheduled")
    })
    .await;
    broken.store(false, Ordering::SeqCst);
    let result = h.retry(&pr).await;
    assert!(
        matches!(&result, Ok(result) if result.head_sha == pr.head_sha && result.attempt == 1),
        "retry = {result:?}"
    );
    h.poll_until("the retried review", || h.publications() == 1)
        .await;
    h.stop_engine().await;
}

#[tokio::test]
async fn readded_label_reviews_the_same_head_again() {
    let mut h = start_harness(|_| {}).await;
    let valid = reviewer_output(Vec::new());
    let running = Arc::new(AtomicBool::new(true));
    let waiting = Arc::clone(&running);
    h.set_output(move |request: &SessionRequest| {
        if waiting.load(Ordering::SeqCst) {
            return String::new();
        }
        valid(request)
    });
    let mut pr = integration_pull_request("solo/tool", &sha("a"));
    pr.review_request_id = 0;
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the first reviewer session", || h.started_count() == 1)
        .await;
    h.fake.lapse(pr.clone(), "review_label_removed");
    h.wait_for("the superseded review", || {
        h.fake
            .has_note(Level::Log, "superseded (review_label_removed)")
    })
    .await;
    for _ in 0..3 {
        h.poll().await;
    }
    running.store(false, Ordering::SeqCst);
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the review after the label returned", || {
        h.publications() == 1
    })
    .await;
    h.stop_engine().await;
}
