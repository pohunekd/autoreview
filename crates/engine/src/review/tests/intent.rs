//! Scenario of the intent check: the issues a pull request closes reach its reviewers.

// Rust guideline compliant 2026-11-15

use serde_json::Value;

use crate::integrations::{GitHubReview, SessionRequest};
use crate::model::{LinkedIssue, LinkedIssues, PullRequest};
use crate::protocol::fixtures::sha;
use crate::testing::harness::{integration_pull_request, start_harness};

/// Label of the line that carries the issues in a reviewer prompt.
const ISSUES_LABEL: &str = "Issues this pull request closes (untrusted JSON):";

/// Returns the prompt text of the reviewer request started for `pr`.
fn prompt_of(started: &[SessionRequest], pr: &PullRequest) -> String {
    let request = started
        .iter()
        .find(|request| request.pull_request.key() == pr.key())
        .map(|request| request.prompt.as_str())
        .unwrap_or_default();
    let parsed: Value = serde_json::from_str(request).expect("a Claude request is JSON");
    parsed["prompt"].as_str().unwrap_or_default().to_owned()
}

/// Returns the body of the review published for the head of `pr`.
fn body_of(published: &[GitHubReview], heads: &[String], pr: &PullRequest) -> String {
    (heads.iter().zip(published))
        .find(|(head, _)| **head == pr.head_sha)
        .map(|(_, review)| review.body.clone())
        .unwrap_or_default()
}

/// D1: with `intent_check: issues` the reviewer request carries the bounded issue text in its
/// untrusted line and the review names the issue; with `off` the engine reads no issue. A
/// refused read still publishes the review, which says that the issues could not be read.
#[tokio::test]
async fn closed_issues_reach_the_reviewers_only_with_an_intent_check() {
    let h = start_harness(|config| config.max_issue_characters = 40).await;
    let issue = LinkedIssue {
        repository: "intent/app".to_owned(),
        number: 12,
        title: "Header parser".to_owned(),
        body: "Parse the header. ".repeat(4),
        truncated: false,
    };
    h.fake.lock().closing_issues = Some(LinkedIssues {
        read: true,
        issues: vec![issue.clone()],
        more: false,
    });
    let checked = integration_pull_request("intent/app", &sha("a"));
    h.fake.open(checked.clone(), h.rule(&checked.repository));
    let plain = integration_pull_request("acme/plain", &sha("b"));
    h.fake.open(plain.clone(), h.rule(&plain.repository));
    h.poll_until("both reviews", || h.publications() == 2).await;

    let (published, _, started, _) = h.fake.snapshot();
    let heads = h.fake.lock().published_heads.clone();
    assert_eq!(h.fake.lock().closing_reads, [checked.key()]);
    let prompt = prompt_of(&started, &checked);
    let mut lines = prompt
        .lines()
        .skip_while(|line| *line != ISSUES_LABEL)
        .skip(1);
    let carried: Vec<LinkedIssue> =
        serde_json::from_str(lines.next().unwrap_or_default()).expect("the issue line is JSON");
    // 40 characters: the 13 of the title, then 26 of the body and the marker.
    let cut = LinkedIssue {
        body: format!("{}…", issue.body.chars().take(26).collect::<String>()),
        truncated: true,
        ..issue
    };
    assert_eq!(carried, [cut], "prompt = {prompt}");
    assert!(
        body_of(&published, &heads, &checked).contains(concat!(
            "Intent check: the change was compared with `intent/app#12`. ",
            "The text of `intent/app#12` was cut to the configured limit."
        )),
        "published = {published:?}"
    );
    let plain_prompt = prompt_of(&started, &plain);
    assert!(
        !plain_prompt.contains("Intent check") && !plain_prompt.contains(ISSUES_LABEL),
        "prompt = {plain_prompt}"
    );
    assert!(!body_of(&published, &heads, &plain).contains("Intent check"));

    // GitHub refuses the read: the review goes on and says so.
    h.fake.lock().closing_issues = None;
    let refused = integration_pull_request("intent/tool", &sha("c"));
    h.fake.open(refused.clone(), h.rule(&refused.repository));
    h.poll_until("the review with a refused read", || h.publications() == 3)
        .await;
    let (published, _, started, _) = h.fake.snapshot();
    let heads = h.fake.lock().published_heads.clone();
    let prompt = prompt_of(&started, &refused);
    assert!(
        prompt.contains("the issues this pull request closes could not be read")
            && !prompt.contains(ISSUES_LABEL),
        "prompt = {prompt}"
    );
    assert!(
        body_of(&published, &heads, &refused).contains(
            "Intent check: the issues this pull request closes could not be read, so the \
             change was not compared with them."
        ),
        "published = {published:?}"
    );
}
