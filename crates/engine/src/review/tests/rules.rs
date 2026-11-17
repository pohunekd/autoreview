//! Scenario of the repository review rules: `REVIEW.md` is read at the base commit only.

// Rust guideline compliant 2026-11-17

use serde_json::Value;

use crate::integrations::{ChangedFile, SessionRequest};
use crate::model::PullRequest;
use crate::protocol::fixtures::sha;
use crate::store::ReviewFilter;
use crate::testing::fake::reviewer_output;
use crate::testing::harness::{Harness, integration_pull_request, start_harness};

/// Returns a pull request of `repository` whose base commit is `base`.
fn pull_request(repository: &str, base: &str) -> PullRequest {
    PullRequest {
        base_sha: sha(base),
        ..integration_pull_request(repository, &sha("a"))
    }
}

/// Returns the prompt text of the reviewer request started for `pr`.
fn prompt_of(started: &[SessionRequest], pr: &PullRequest) -> String {
    let request = (started.iter())
        .find(|request| request.pull_request.key() == pr.key())
        .map(|request| request.prompt.as_str())
        .unwrap_or_default();
    let parsed: Value = serde_json::from_str(request).expect("a Claude request is JSON");
    parsed["prompt"].as_str().unwrap_or_default().to_owned()
}

/// Returns the failure codes of the completed reviews of `pr`, oldest first; a success is empty.
fn failures(h: &Harness, pr: &PullRequest) -> Vec<String> {
    let filter = ReviewFilter::ReviewKey(pr.key());
    let reviews = h.store.list_reviews(&filter, 50).expect("the reviews read");
    (reviews.into_iter().rev())
        .filter(|review| review.status == "completed")
        .map(|review| review.failure)
        .collect()
}

/// D3: the base file is read once at the base commit, never at the head. A file that is too
/// large, not valid, or not readable fails the review with its code; the same invalid file
/// does not fail a pull request that changes `REVIEW.md`, which is reviewed without rules.
#[tokio::test]
async fn the_base_review_rules_are_read_and_an_unusable_file_fails_the_review() {
    let h = start_harness(|config| config.max_review_rules_bytes = 64).await;
    h.set_output(reviewer_output(Vec::new()));
    let valid = pull_request("rules/valid", "c");
    let at = |_: &PullRequest, commit: &str| format!("{commit}:REVIEW.md");
    h.fake.lock().files.insert(
        at(&valid, &valid.base_sha),
        "---\nskip: [docs/**]\n---\nBe strict.".to_owned(),
    );
    // A file that only the head holds is never read.
    h.fake
        .lock()
        .files
        .insert(at(&valid, &valid.head_sha), "---\nskp: x\n---\n".to_owned());
    h.fake.open(valid.clone(), h.rule(&valid.repository));
    h.poll_until("the review with rules", || h.publications() == 1)
        .await;
    assert_eq!(
        h.fake.lock().file_reads,
        [format!("rules/valid@{}:REVIEW.md", valid.base_sha)]
    );
    assert_eq!(failures(&h, &valid), [String::new()]);
    // D1: the reviewer request carries the base rules on one line, and nothing of the head file.
    let (_, _, started, _) = h.fake.snapshot();
    let prompt = prompt_of(&started, &valid);
    let line = r#"{"skip":["docs/**"],"ci_tools":[],"text":"Be strict."}"#;
    assert!(
        prompt.contains("Repository review rules (untrusted JSON") && prompt.contains(line),
        "prompt = {prompt}"
    );
    assert!(!prompt.contains("skp"), "prompt = {prompt}");

    let cases = [
        (
            "rules/broken",
            "d",
            "---\nskp: x\n---\n",
            "review_rules_invalid",
        ),
        (
            "rules/large",
            "e",
            &"x".repeat(65),
            "review_rules_too_large",
        ),
    ];
    for (repository, base, text, code) in cases {
        let pr = pull_request(repository, base);
        h.fake
            .lock()
            .files
            .insert(at(&pr, &pr.base_sha), text.to_owned());
        h.fake.open(pr.clone(), h.rule(repository));
        h.poll_until(code, || !failures(&h, &pr).is_empty()).await;
        assert_eq!(failures(&h, &pr), [code.to_owned()], "{repository}");
    }

    // The same invalid file on a pull request that changes REVIEW.md: reviewed without rules.
    let fix = pull_request("rules/fix", "d");
    h.fake.lock().changed = vec![ChangedFile {
        path: "REVIEW.md".to_owned(),
        status: "modified".to_owned(),
        ..ChangedFile::default()
    }];
    h.fake.open(fix.clone(), h.rule(&fix.repository));
    h.poll_until("the review of the fix", || h.publications() == 2)
        .await;
    assert_eq!(failures(&h, &fix), [String::new()]);
    h.fake.lock().changed = Vec::new();

    let unreadable = pull_request("rules/gone", "f");
    h.fake.lock().file_failure = Some(("github_commit_not_found".to_owned(), true));
    h.fake
        .open(unreadable.clone(), h.rule(&unreadable.repository));
    h.poll_until("the unreadable file", || {
        !failures(&h, &unreadable).is_empty()
    })
    .await;
    assert_eq!(
        failures(&h, &unreadable),
        ["review_rules_unreadable".to_owned()]
    );
}
