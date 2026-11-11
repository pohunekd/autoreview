//! The runs of the review steps, as the statistics document reads them from the store.

// Rust guideline compliant 2026-06-26

use autoreview_policy::{Level, VERIFIER_REVIEWER_ID};
use serde_json::{Value, json};
use time::{Duration, OffsetDateTime};
use tokio_util::sync::CancellationToken;

use crate::integrations::SessionRequest;
use crate::protocol::fixtures::sha;
use crate::testing::fake::{reviewer_output, verifying};
use crate::testing::harness::{Harness, integration_pull_request, start_harness};
use crate::verify::{VERDICT_CONFIRMED, VERDICT_UPHELD};

/// Stands in for a measured duration, which differs from run to run.
const MEASURED: &str = "measured";

/// Start of the notification that asks to remove reviewer sessions by hand.
const CLEANUP_NOTE: &str = "Pohunek could not remove reviewer sessions";

/// Reads `history.steps` of the statistics document from yesterday on.
fn recorded_steps(h: &Harness) -> Value {
    // A day before today, so a review that ends just after midnight UTC is still inside.
    let since = (OffsetDateTime::now_utc() - Duration::days(1))
        .date()
        .to_string();
    let document = h
        .engine
        .review_stats(&CancellationToken::new(), Some(&since), None)
        .expect("the statistics read");
    let history = serde_json::to_value(document.history).expect("the history serializes");
    history["steps"].clone()
}

/// Waits for the publication record, the last step, and returns every step entry.
///
/// Each duration is set to [`MEASURED`].
async fn measured_steps(h: &Harness) -> Value {
    h.wait_for("the publication record", || {
        recorded_steps(h)
            .as_array()
            .is_some_and(|runs| runs.iter().any(|run| run["key"] == "publication"))
    })
    .await;
    let mut steps = recorded_steps(h);
    for entry in steps.as_array_mut().into_iter().flatten() {
        for field in ["duration_median_ms", "duration_p90_ms"] {
            assert!(entry[field].is_i64(), "{field} of {entry}");
            entry[field] = json!(MEASURED);
        }
    }
    steps
}

/// Returns the entry of the step `key` with one succeeded run and `retries`.
fn succeeded_once(key: &str, retries: i64) -> Value {
    json!({
        "key": key, "runs": 1, "succeeded": 1, "superseded": 0, "failed": 0,
        "retries": retries, "duration_median_ms": MEASURED, "duration_p90_ms": MEASURED,
    })
}

/// A review records its reviewers and publication steps; a fresh reviewer run is a retry.
#[tokio::test]
async fn review_records_its_steps_with_their_retries() {
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
    // No prior findings and no blocking claim: neither discussion nor verifier ran.
    assert_eq!(
        measured_steps(&h).await,
        json!([
            succeeded_once("publication", 0),
            succeeded_once("reviewers", 1)
        ])
    );
    h.stop_engine().await;
}

/// A shutdown after the last reviewer result is stored still leaves one reviewers record.
///
/// The cleanup notification of the only reviewer session is held, so the engine stops after
/// the result is stored and before the reviewers step ends.
#[tokio::test]
async fn shutdown_after_the_last_reviewer_result_still_records_the_reviewers_step() {
    let mut h = start_harness(|_| {}).await;
    h.fake.lock().remove_failing.insert("s-1".to_owned());
    let gate = h.fake.hold_notes(CLEANUP_NOTE);
    // One reviewer, so its stored result leaves no reviewer to run after the restart.
    let pr = integration_pull_request("claude/base", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the held cleanup notification", || {
        h.fake.has_note(Level::Action, CLEANUP_NOTE)
    })
    .await;
    // The engine is cancelled before the notification returns, so the step ends by shutdown.
    tokio::join!(biased; h.stop_engine(), async { gate.add_permits(1) });
    h.fake.lock().held_notes = None;
    h.start();
    h.poll_until("the publication after the restart", || {
        h.publications() == 1
    })
    .await;
    assert_eq!(
        measured_steps(&h).await,
        json!([
            succeeded_once("publication", 0),
            succeeded_once("reviewers", 0)
        ])
    );
    h.stop_engine().await;
}

/// Output where the opus reviewer reports one blocking finding and every other reviewer none.
fn opus_blocks(request: &SessionRequest) -> String {
    let output = if request.reviewer.id == "opus" {
        reviewer_output(vec![json!({
            "severity": "high", "confidence": "high", "blocking": true, "path": "src/a.go",
            "line": 3, "title": "Broken", "body": "It breaks.",
        })])
    } else {
        reviewer_output(Vec::new())
    };
    output(request)
}

/// A verifier run that a shutdown interrupts after its failure records the run after it.
///
/// The verifier session cannot be read and its cleanup notification is held, so the engine
/// stops before the failed verification is stored. After the restart the verifier succeeds.
#[tokio::test]
async fn verifier_failure_before_its_verification_is_stored_records_the_resumed_run() {
    let mut h = start_harness(|_| {}).await;
    h.fake.lock().commentable = [("src/a.go".to_owned(), vec![3])].into();
    let gate = h.fake.hold_notes(CLEANUP_NOTE);
    h.set_output(|request: &SessionRequest| {
        if request.reviewer.id == VERIFIER_REVIEWER_ID {
            return String::new();
        }
        opus_blocks(request)
    });
    let pr = integration_pull_request("checked/widget", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the verifier session", || h.started_count() == 3)
        .await;
    {
        // The host loses the verifier session and refuses its removal.
        let mut state = h.fake.lock();
        let verifier = state
            .sessions
            .iter()
            .find(|(_, request)| request.reviewer.id == VERIFIER_REVIEWER_ID)
            .map(|(id, _)| id.clone())
            .expect("the verifier session runs");
        state.sessions.remove(&verifier);
        state.remove_failing.insert(verifier);
    }
    h.poll_until("the held cleanup notification", || {
        h.fake.has_note(Level::Action, CLEANUP_NOTE)
    })
    .await;
    // The engine is cancelled before the notification returns, so the run ends by shutdown.
    tokio::join!(biased; h.stop_engine(), async { gate.add_permits(1) });
    h.fake.lock().held_notes = None;
    h.set_output(verifying(
        opus_blocks,
        |_| VERDICT_CONFIRMED,
        |_| VERDICT_UPHELD,
    ));
    h.start();
    h.poll_until("the publication after the restart", || {
        h.publications() == 1
    })
    .await;
    assert_eq!(
        measured_steps(&h).await,
        json!([
            succeeded_once("publication", 0),
            succeeded_once("reviewers", 0),
            succeeded_once("verifier", 0)
        ])
    );
    h.stop_engine().await;
}
