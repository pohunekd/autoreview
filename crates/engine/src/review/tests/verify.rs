//! Verifier scenarios of the engine against the fake integration.
//!
//! Mirrors `cmd/autoreview/verify_integration_test.go`.

// Rust guideline compliant 2026-06-26

use autoreview_policy::Level;
use serde_json::json;

use crate::integrations::SessionRequest;
use crate::model::review::Decision;
use crate::protocol::fixtures::sha;
use crate::testing::fake::{count_notes, reviewer_output, verifying};
use crate::testing::harness::{integration_pull_request, start_harness};
use crate::verify::{ClaimFinding, ClaimPrior, VERDICT_REFUTED, VERDICT_UPHELD};

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
