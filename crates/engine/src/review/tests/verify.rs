//! Verifier scenarios of the engine against the fake integration.
//!
//! Mirrors `cmd/autoreview/verify_integration_test.go`.

// Rust guideline compliant 2026-06-26

use autoreview_policy::{Level, VERIFIER_REVIEWER_ID};
use serde_json::json;

use crate::integrations::SessionRequest;
use crate::messages::APPROVAL_REQUESTED_TITLE;
use crate::model::review::Decision;
use crate::protocol::fixtures::sha;
use crate::testing::fake::{count_notes, request_claims, reviewer_output, verifying};
use crate::testing::harness::{integration_pull_request, start_harness};
use crate::verify::{
    ClaimFinding, ClaimPrior, VERDICT_NOT_BLOCKING, VERDICT_REFUTED, VERDICT_UPHELD,
};

/// Output where the opus reviewer reports one blocking finding of `severity`.
///
/// Every other reviewer reports none.
fn opus_reports(severity: &str) -> impl Fn(&SessionRequest) -> String + Send + Sync + 'static {
    let finding = reviewer_output(vec![json!({
        "severity": severity, "confidence": "high", "blocking": true, "path": "src/a.go",
        "line": 3, "title": "Broken", "body": "It breaks.",
    })]);
    let clean = reviewer_output(Vec::new());
    move |request: &SessionRequest| {
        if request.reviewer.id == "opus" {
            finding(request)
        } else {
            clean(request)
        }
    }
}

/// Gives every claimed finding the verdict `verdict`.
fn every(verdict: &'static str) -> impl Fn(&ClaimFinding) -> &'static str + Send + Sync + 'static {
    move |_| verdict
}

/// Gives every claimed prior finding the verdict `verdict`.
fn every_prior(
    verdict: &'static str,
) -> impl Fn(&ClaimPrior) -> &'static str + Send + Sync + 'static {
    move |_| verdict
}

#[tokio::test]
async fn refuted_finding_is_published_as_a_note_and_the_review_approved() {
    let mut h = start_harness(|_| {}).await;
    h.fake.lock().commentable = [("src/a.go".to_owned(), vec![3])].into();
    h.set_output(verifying(
        opus_reports("high"),
        every(VERDICT_REFUTED),
        every_prior(VERDICT_UPHELD),
    ));
    let pr = integration_pull_request("checked/widget", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the automatic approval", || h.publications() == 1)
        .await;
    let (published, notes, started, open) = h.fake.snapshot();
    let review = &published[0];
    assert!(
        review.decision == Decision::Approve
            && review.comments.is_empty()
            && review.marker.contains(":findings=0:")
            && started.len() == 3
            && open == 0,
        "decision={:?} comments={} marker={} started={} open={open}",
        review.decision,
        review.comments.len(),
        review.marker,
        started.len()
    );
    assert!(
        review.body.contains("1 non-blocking note(s)")
            && review
                .body
                .contains("Refuted by the verification by Opus: Traced the scenario at the head.")
            && review.body.contains(
                "Verification by Opus: blocking findings 0 confirmed, 0 not blocking, 1 refuted."
            ),
        "the refuted finding must stay visible as a note with the reason:\n{}",
        review.body
    );
    assert_eq!(
        count_notes(&notes, Level::Action, ""),
        0,
        "one refuted high finding must not ping the user: {notes:?}"
    );
    h.stop_engine().await;
}

#[tokio::test]
async fn demoted_critical_finding_on_an_approving_rule_waits_for_the_user() {
    let mut h = start_harness(|_| {}).await;
    h.fake.lock().commentable = [("src/a.go".to_owned(), vec![3])].into();
    h.set_output(verifying(
        opus_reports("critical"),
        every(VERDICT_NOT_BLOCKING),
        every_prior(VERDICT_UPHELD),
    ));
    let pr = integration_pull_request("checked/widget", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the approval request", || {
        h.fake.has_note(Level::Action, APPROVAL_REQUESTED_TITLE)
    })
    .await;
    assert!(
        h.fake.has_note(
            Level::Action,
            "the verification demoted these blocking findings, check before approving:\n\
             O001 [NOTE, NOT BLOCKING BY VERIFICATION, CRITICAL"
        ) && h.publications() == 0,
        "the approval request must name the demoted critical finding and nothing may be posted yet"
    );
    if let Err(err) = h.approve_when_awaiting(&pr).await {
        panic!("{err}");
    }
    h.wait_for("the approval", || h.publications() == 1).await;
    let (published, _, _, _) = h.fake.snapshot();
    assert!(
        published[0].decision == Decision::Approve
            && published[0]
                .body
                .contains("Not blocking according to the verification by Opus"),
        "decision={:?} body={}",
        published[0].decision,
        published[0].body
    );
    h.stop_engine().await;
}

#[tokio::test]
async fn confirmed_findings_still_request_changes() {
    let mut h = start_harness(|_| {}).await;
    h.fake.lock().commentable = [("src/a.go".to_owned(), vec![3])].into();
    h.set_output(reviewer_output(vec![json!({
        "severity": "high", "confidence": "high", "blocking": true, "path": "src/a.go",
        "line": 3, "title": "Broken", "body": "It breaks.",
    })]));
    let pr = integration_pull_request("checked/manual", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the request for changes", || h.publications() == 1)
        .await;
    let (published, _, started, _) = h.fake.snapshot();
    assert!(
        published[0].decision == Decision::RequestChanges
            && published[0].comments.len() == 2
            && published[0].body.contains("blocking findings 2 confirmed"),
        "decision={:?} comments={} body={}",
        published[0].decision,
        published[0].comments.len(),
        published[0].body
    );
    assert!(
        started.len() == 3
            && started[2].reviewer.id == VERIFIER_REVIEWER_ID
            && started[2].reviewer.agent_profile == "claude-profile",
        "one Opus verifier session must follow the reviewers: {started:?}"
    );
    let ids: Vec<String> = request_claims(&started[2].prompt)
        .findings
        .into_iter()
        .map(|claim| claim.id)
        .collect();
    assert_eq!(
        ids.join(","),
        "O001,S001",
        "the verifier must check every blocking finding, got {ids:?}"
    );
    h.stop_engine().await;
}
