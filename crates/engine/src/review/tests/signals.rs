//! GitHub signal scenarios of the engine against the fake integration: a dismissed review and
//! the reactions of the pull request author.

// Rust guideline compliant 2026-06-26

use std::time::Duration;

use serde_json::json;
use time::OffsetDateTime;

use crate::config::GuidanceConfig;
use crate::integrations::CommentReaction;
use crate::model::PullRequest;
use crate::protocol::fixtures::sha;
use crate::store::outcomes::ListedOutcome;
use crate::store::signals::SignalRow;
use crate::testing::fake::reviewer_output;
use crate::testing::harness::{Harness, integration_pull_request, start_harness};

/// The `guidance` section of the scenarios: two fixed findings of a category make a suggestion.
const GUIDANCE: GuidanceConfig = GuidanceConfig {
    recurrence_threshold: 2,
    window: Duration::from_secs(30 * 24 * 60 * 60),
};

/// Starts an engine that reads signals at every pass and publishes a request for changes with
/// two blocking findings of one category on `acme/widgets`.
async fn publish_two_findings() -> (Harness, PullRequest) {
    let h = start_harness(|config| {
        config.signal_check_interval = Some(Duration::from_millis(1));
        config.guidance = Some(GUIDANCE);
    })
    .await;
    h.fake.lock().commentable = [("src/a.go".to_owned(), vec![3, 4])].into();
    let finding = |line: i64, title: &str| {
        json!({
            "severity": "high", "confidence": "high", "blocking": true, "path": "src/a.go",
            "line": line, "title": title, "body": "It breaks.", "category": "missing-test",
        })
    };
    h.set_output(reviewer_output(vec![
        finding(3, "Broken"),
        finding(4, "Also broken"),
    ]));
    let pr = integration_pull_request("acme/widgets", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the request for changes", || h.publications() == 1)
        .await;
    (h, pr)
}

/// Returns the listed outcomes of `pr`, oldest first.
fn listed(h: &Harness, pr: &PullRequest) -> Vec<ListedOutcome> {
    let rows = (h.store).list_outcomes(&pr.repository, OffsetDateTime::UNIX_EPOCH);
    let rows = rows.expect("the outcomes read");
    rows.into_iter()
        .filter(|row| row.finding.number == pr.number)
        .collect()
}

/// Returns the signal rows of `pr`.
fn signal_rows(h: &Harness, pr: &PullRequest) -> Vec<SignalRow> {
    (h.store)
        .signal_rows(&pr.repository, pr.number)
        .expect("the signal rows read")
}

/// Returns the number of signal reads of `pr`.
fn signal_reads(h: &Harness, pr: &PullRequest) -> usize {
    let state = h.fake.lock();
    state
        .signal_reads
        .iter()
        .filter(|key| **key == pr.key())
        .count()
}

/// Waits until both findings of `pr` carry the review ID and the ID of their inline comment.
async fn wait_for_ids(h: &Harness, pr: &PullRequest) {
    h.wait_for("the review and finding IDs", || {
        let rows = signal_rows(h, pr);
        let commented = rows.iter().all(|row| !row.finding_id.is_empty());
        rows.len() == 2 && commented && rows.iter().all(|row| row.review_id == 1)
    })
    .await;
}

// A review that requested changes and was dismissed settles its open findings as `dismissed`
// once, and a dismissed finding does not count for a guidance suggestion.
#[tokio::test]
async fn dismissed_request_for_changes_ends_its_open_findings() {
    let (mut h, pr) = publish_two_findings().await;
    wait_for_ids(&h, &pr).await;
    assert!(signal_rows(&h, &pr).iter().all(|row| row.requested_changes));
    // The review stands, so the read changes no outcome.
    h.poll().await;
    assert!(signal_reads(&h, &pr) > 0, "the signals were read");
    let outcomes = |h: &Harness| -> Vec<String> {
        let rows = listed(h, &pr);
        rows.into_iter().map(|row| row.finding.outcome).collect()
    };
    assert_eq!(outcomes(&h), ["open", "open"]);
    h.fake.dismiss(0);
    h.poll_until("the dismissed outcomes", || {
        outcomes(&h) == ["dismissed", "dismissed"]
    })
    .await;
    let rows = listed(&h, &pr);
    assert!(
        rows.iter()
            .all(|row| row.finding.outcome_head_sha == pr.head_sha),
        "the outcome is decided at the head of the dismissed review"
    );
    // Only `fixed` findings count for a suggestion.
    let raised = h.store.raise_suggestions(&pr.repository, "check", GUIDANCE);
    assert!(raised.expect("the suggestions").is_empty());
    h.stop_engine().await;
}

// The reactions of the pull request author are stored beside the outcome; others' reactions
// never count, a removed reaction is removed, and a failed read changes nothing.
#[tokio::test]
async fn author_reactions_are_stored_beside_the_unchanged_outcome() {
    let (mut h, pr) = publish_two_findings().await;
    wait_for_ids(&h, &pr).await;
    let reaction = |login: &str, content: &str| CommentReaction {
        login: login.to_owned(),
        content: content.to_owned(),
    };
    let reactions = |h: &Harness| -> Vec<Vec<String>> {
        let rows = signal_rows(h, &pr);
        rows.into_iter().map(|row| row.author_reactions).collect()
    };
    // The first inline comment of the first review.
    let react = |list: Vec<CommentReaction>| {
        h.fake.lock().reactions.insert((1, 0), list);
    };
    react(vec![reaction(&pr.author, "-1"), reaction("someone", "+1")]);
    h.poll_until("the author's reaction", || {
        reactions(&h) == [vec!["-1".to_owned()], Vec::<String>::new()]
    })
    .await;
    assert!(
        listed(&h, &pr)
            .iter()
            .all(|row| row.finding.outcome == "open"),
        "a reaction never changes the outcome"
    );
    // A failed read keeps the stored reaction, even though the comment lost it.
    react(Vec::new());
    h.fake.lock().failing_signal_reads.insert(pr.key());
    let reads = signal_reads(&h, &pr);
    h.poll_until("a failed read", || signal_reads(&h, &pr) > reads)
        .await;
    assert_eq!(reactions(&h), [vec!["-1".to_owned()], Vec::<String>::new()]);
    h.fake.lock().failing_signal_reads.clear();
    h.poll_until("the removed reaction", || {
        reactions(&h) == [Vec::<String>::new(), Vec::<String>::new()]
    })
    .await;
    h.stop_engine().await;
}
