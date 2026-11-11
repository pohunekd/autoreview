//! The runs of the review steps, as the statistics document reads them from the store.

// Rust guideline compliant 2026-06-26

use serde_json::{Value, json};
use time::{Duration, OffsetDateTime};
use tokio_util::sync::CancellationToken;

use crate::integrations::SessionRequest;
use crate::protocol::fixtures::sha;
use crate::testing::fake::reviewer_output;
use crate::testing::harness::{integration_pull_request, start_harness};

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
    // A day before today, so a review that ends just after midnight UTC is still inside.
    let since = (OffsetDateTime::now_utc() - Duration::days(1))
        .date()
        .to_string();
    let steps = || -> Value {
        let document = h
            .engine
            .review_stats(&CancellationToken::new(), Some(&since), None)
            .expect("the statistics read");
        let history = serde_json::to_value(document.history).expect("the history serializes");
        history["steps"].clone()
    };
    h.wait_for("the records of both steps", || {
        steps().as_array().is_some_and(|runs| runs.len() == 2)
    })
    .await;
    let mut steps = steps();
    let measured = "measured";
    for entry in steps.as_array_mut().into_iter().flatten() {
        for field in ["duration_median_ms", "duration_p90_ms"] {
            assert!(entry[field].is_i64(), "{field} of {entry}");
            entry[field] = json!(measured);
        }
    }
    let counts = |key: &str, retries: i64| {
        json!({
            "key": key, "runs": 1, "succeeded": 1, "superseded": 0, "failed": 0,
            "retries": retries, "duration_median_ms": measured, "duration_p90_ms": measured,
        })
    };
    // No prior findings and no blocking claim: neither discussion nor verifier ran.
    assert_eq!(
        steps,
        json!([counts("publication", 0), counts("reviewers", 1)])
    );
    h.stop_engine().await;
}
