//! Approval, label and request scenarios of the engine against the fake integration.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeMap;

use autoreview_policy::{EVENTS, Event, Level, Rule};

use crate::messages::APPROVAL_REQUESTED_TITLE;
use crate::model::ids::INPUT_VERSION;
use crate::model::pull_request::SCOPE_FULL;
use crate::model::{ProjectRoute, PullRequest, ReviewInput, ReviewScope, RunLimits};
use crate::protocol::fixtures::sha;
use crate::testing::harness::{integration_pull_request, start_harness};

/// The notification level of every event in these scenarios.
fn notification_levels() -> BTreeMap<Event, Level> {
    let mut levels: BTreeMap<Event, Level> = EVENTS
        .iter()
        .map(|event| (event.clone(), Level::Log))
        .collect();
    levels.insert(Event::ApprovalRequested, Level::Action);
    levels.insert(Event::FindingsOnOwnPr, Level::Action);
    levels.insert(Event::ReviewStopped, Level::Action);
    levels.insert(Event::CleanupManual, Level::Action);
    levels.insert(Event::Approved, Level::Digest);
    levels.insert(Event::ChangesRequested, Level::Digest);
    levels.insert(Event::Commented, Level::Digest);
    levels
}

/// The review input of `pull_request` under `rule`.
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
    let level = input.notifications[&Event::ApprovalExpired].clone();
    h.wait_for("the approval to expire", || {
        h.fake
            .has_note(level.clone(), "expired after 1s without a reply")
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
