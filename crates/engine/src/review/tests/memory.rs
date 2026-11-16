//! Incremental scope and prior finding scenarios of the engine against the fake integration.

// Rust guideline compliant 2026-06-26

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use autoreview_go_compat::go_time::{format_rfc3339_nano, parse_rfc3339};
use autoreview_policy::{Level, VERIFIER_REVIEWER_ID};
use serde_json::json;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

use crate::config::{Credential, Credentials};
use crate::error::error_code;
use crate::integrations::SessionRequest;
use crate::messages::APPROVAL_REQUESTED_TITLE;
use crate::model::review::{Decision, DiscussionThread, PRIOR_FIXED, PRIOR_OPEN, prior_key};
use crate::model::{PullRequest, Verification};
use crate::protocol::fixtures::sha;
use crate::report::{ReportRequest, ReviewerCounts};
use crate::review::input::STEP_VERIFICATION;
use crate::stats::Keyed;
use crate::store::{FindingOutcome, REVIEW_COMPLETED, ReviewFilter};
use crate::testing::fake::{reviewer_output, settling_output, verifier_output};
use crate::testing::harness::{Harness, integration_pull_request, start_harness};
use crate::verify::{VERDICT_CONFIRMED, VERDICT_REFUTED, VERDICT_UPHELD};

/// Returns the stored finding outcomes of `pr` as (finding key, tracked key, outcome, head).
fn outcomes(h: &Harness, pr: &PullRequest) -> Vec<(String, String, String, String)> {
    let stored = (h.store)
        .outcomes_since(&pr.repository, OffsetDateTime::UNIX_EPOCH)
        .expect("the outcomes read");
    stored
        .into_iter()
        .filter(|row| row.number == pr.number)
        .map(|row| {
            let head = row.outcome_head_sha;
            (row.finding_key, row.tracked_key, row.outcome, head)
        })
        .collect()
}

/// Returns the number of state reads of `pr`.
fn state_reads(h: &Harness, pr: &PullRequest) -> usize {
    let prefix = format!("{} ", pr.key());
    let state = h.fake.lock();
    state
        .state_reads
        .iter()
        .filter(|read| read.starts_with(&prefix))
        .count()
}

/// Returns the number of completed reviews of `pr`; a published review completes last.
fn completed(h: &Harness, pr: &PullRequest) -> usize {
    let filter = ReviewFilter::ReviewKey(pr.key());
    let reviews = h.store.list_reviews(&filter, 50).expect("the reviews read");
    reviews
        .iter()
        .filter(|review| review.status == REVIEW_COMPLETED)
        .count()
}

/// Approves the clean review that waits on `pr` and waits until it is the `count`th publication.
async fn approve_waiting(h: &Harness, pr: &PullRequest, count: usize) {
    let workflow_id = h.resolve(pr).expect("the review waits").workflow_id;
    h.approve(&workflow_id, pr)
        .expect("the approval is accepted");
    h.wait_for("the approval", || h.publications() == count)
        .await;
}

/// Publishes a notes-only review of a hand-off rule and checks it reached the author.
///
/// The notes go to the author as a comment, without pinging the user.
async fn hand_notes_to_the_author(h: &Harness) -> PullRequest {
    h.fake.lock().commentable = [("src/a.go".to_owned(), vec![3])].into();
    h.set_output(reviewer_output(vec![json!({
        "severity": "low", "confidence": "high", "blocking": false, "path": "src/a.go",
        "line": 3, "title": "Naming", "body": "Could be clearer.",
    })]));
    let pr = integration_pull_request("acme/notes", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the hand-off comment", || h.publications() == 1)
        .await;
    h.wait_for("the hand-off notification", || {
        h.fake
            .has_note(Level::Digest, "Handed 1 non-blocking note(s) on acme/notes")
    })
    .await;
    let (published, notes, _, _) = h.fake.snapshot();
    assert!(
        published[0].decision == Decision::Comment
            && published[0].comments.is_empty()
            && published[0]
                .body
                .contains("Request this review again once you have decided"),
        "decision={:?} comments={} body={}",
        published[0].decision,
        published[0].comments.len(),
        published[0].body
    );
    for entry in &notes {
        assert!(
            entry.level != Level::Action,
            "notes handed to the author must not ping the user: {entry:?}"
        );
    }
    pr
}

#[tokio::test]
async fn renewed_request_after_a_notes_handoff_asks_for_approval_without_a_review() {
    let mut h = start_harness(|_| {}).await;
    let pr = hand_notes_to_the_author(&h).await;
    h.settle().await;
    assert!(
        h.started_count() == 2 && !h.fake.has_note(Level::Action, APPROVAL_REQUESTED_TITLE),
        "a hand-off must wait for the author's renewed request"
    );
    let mut renewed = pr.clone();
    renewed.review_request_id = 99;
    // The hand-off has no request time; the approval of the renewed request has one.
    let requested_at = OffsetDateTime::now_utc() - time::Duration::hours(1);
    renewed.requested_at = format_rfc3339_nano(requested_at);
    h.fake.open(renewed.clone(), h.rule(&pr.repository));
    h.poll_until("the approval request", || {
        h.fake.has_note(Level::Action, APPROVAL_REQUESTED_TITLE)
    })
    .await;
    assert!(
        h.started_count() == 2
            && h.publications() == 1
            && h.fake
                .has_note(Level::Action, "The author requested the review again")
            && h.fake.has_note(Level::Action, "Non-blocking notes: 1"),
        "the renewed request must reuse the hand-off review: {} sessions, {} publications",
        h.started_count(),
        h.publications()
    );
    // The notification precedes the stored wait by a moment.
    h.wait_for("the approval to be accepted", || {
        let Ok(resolved) = h.resolve(&renewed) else {
            return false;
        };
        h.approve(&resolved.workflow_id, &renewed).is_ok()
    })
    .await;
    h.wait_for("the approval", || h.publications() == 2).await;
    let (published, _, _, _) = h.fake.snapshot();
    assert!(
        published[1].decision == Decision::Approve
            && published[1].comments.is_empty()
            && published[1].body.contains("1 non-blocking note(s)"),
        "decision={:?} comments={} body={}",
        published[1].decision,
        published[1].comments.len(),
        published[1].body
    );
    h.settle().await;
    assert!(
        h.started_count() == 2 && h.publications() == 2,
        "an approved head was reviewed or published again"
    );
    // A new head under the same request publishes again and adds no request time; the rule
    // keys on the request ID, and the earlier time given here would raise the p90.
    let mut next = renewed.clone();
    next.head_sha = sha("c");
    next.requested_at = format_rfc3339_nano(requested_at - time::Duration::hours(2));
    h.fake.open(next.clone(), h.rule(&pr.repository));
    h.poll_until("the new head's hand-off", || h.publications() == 3)
        .await;
    let filter = ReviewFilter::ReviewKey(next.key());
    let reviews = h.store.list_reviews(&filter, 1).expect("the reviews read");
    let latest = reviews.first().expect("the new head's review is stored");
    assert_eq!(latest.input.pull_request.head_sha, next.head_sha);
    let latest = latest.created_at.clone();
    // A checked pull request: the verifier refutes the finding of Opus and confirms the one of
    // Sonnet. On the next head it answers for another head and does not finish.
    h.fake.lock().commentable = [("src/a.go".to_owned(), vec![3, 7])].into();
    let finding = |line: i64, title: &str| {
        json!({
            "severity": "high", "confidence": "high", "blocking": true, "path": "src/a.go",
            "line": line, "title": title, "body": "Breaks the build.",
        })
    };
    let opus = reviewer_output(vec![finding(3, "Overflow")]);
    let sonnet = reviewer_output(vec![finding(7, "Leak")]);
    let output = move |request: &SessionRequest| match request.reviewer.id.as_str() {
        VERIFIER_REVIEWER_ID => verifier_output(
            request,
            |claim| match claim.reviewer.as_str() {
                "Opus" => VERDICT_REFUTED,
                _ => VERDICT_CONFIRMED,
            },
            |_| VERDICT_UPHELD,
        ),
        "opus" => opus(request),
        _ => sonnet(request),
    };
    h.set_output(output);
    let checked = integration_pull_request("checked/widgets", &sha("d"));
    h.fake.open(checked.clone(), h.rule(&checked.repository));
    h.poll_until("the verified review", || h.publications() == 4)
        .await;
    let opus = reviewer_output(vec![finding(3, "Race")]);
    let sonnet = reviewer_output(Vec::new());
    let output = move |request: &SessionRequest| match request.reviewer.id.as_str() {
        VERIFIER_REVIEWER_ID => {
            let mut other = request.clone();
            other.pull_request.head_sha = sha("f");
            verifier_output(&other, |_| VERDICT_CONFIRMED, |_| VERDICT_UPHELD)
        }
        "opus" => opus(request),
        _ => sonnet(request),
    };
    h.set_output(output);
    let mut pushed = checked.clone();
    pushed.head_sha = sha("e");
    h.fake.open(pushed.clone(), h.rule(&checked.repository));
    h.poll_until("the unverified review", || h.publications() == 5)
        .await;
    let filter = ReviewFilter::ReviewKey(pushed.key());
    let reviews = h.store.list_reviews(&filter, 1).expect("the reviews read");
    let unverified = reviews.first().expect("the next head's review is stored");
    let stored = (h.store).load_step::<Verification>(&unverified.workflow_id, STEP_VERIFICATION);
    let stored = stored.expect("the verification reads");
    assert!(
        unverified.input.pull_request.head_sha == pushed.head_sha
            && stored.is_some_and(|verification| !verification.error_code.is_empty()),
        "the next head's verification must be stored as not finished"
    );
    // Each hand-off counts in `notes_to_author`, from its stored plan and after a prune.
    let today = OffsetDateTime::now_utc().date();
    let since = today
        .previous_day()
        .expect("a day before today")
        .to_string();
    let history = || {
        let cancel = CancellationToken::new();
        let document = h.engine.review_stats(&cancel, Some(&since), None);
        let document = document.expect("the statistics read");
        let mut document = serde_json::to_value(document).expect("the document encodes");
        let mut history = document["history"].take();
        if let Some(history) = history.as_object_mut() {
            history.remove("pruned_before");
        }
        history
    };
    // The request times of the report, of every repository and of the pull request's one.
    let requests = || {
        [None, Some(pr.repository.clone())].map(|repository| {
            let request = ReportRequest {
                since: since.clone(),
                repository,
                ..ReportRequest::default()
            };
            let report = h.engine.review_report(&CancellationToken::new(), &request);
            let reviews = report.expect("the report reads").reviews;
            let median = reviews.request_to_publication_median_seconds;
            (median, reviews.request_to_publication_p90_seconds)
        })
    };
    // The verifier's counts and the reviewer disagreement of every repository, of the checked
    // one and of the other one.
    let checks = || {
        let (checked, other) = (checked.repository.clone(), pr.repository.clone());
        [None, Some(checked), Some(other)].map(|repository| {
            let request = ReportRequest {
                since: since.clone(),
                repository,
                ..ReportRequest::default()
            };
            let report = h.engine.review_report(&CancellationToken::new(), &request);
            let reviews = report.expect("the report reads").reviews;
            let attempts = (reviews.multi_reviewer, reviews.disagreed);
            (attempts, reviews.disagreed_percent, reviews.by_reviewer)
        })
    };
    let verdicts = |key: &str, checked, demoted, percent, blocked_alone| Keyed {
        key: key.to_owned(),
        counts: ReviewerCounts {
            verifier_checked: checked,
            verifier_demoted: demoted,
            verifier_demoted_percent: Some(percent),
            blocked_alone,
        },
    };
    // Sonnet found nothing on the checked pull request's second head, so Opus blocked alone;
    // both blocked on its first head. Both reviewers left notes on the first notes head, the
    // adopted hand-off ran no reviewer and the incremental notes head only Sonnet.
    let verified = vec![
        verdicts("opus", 1, 1, 100.0, 1),
        verdicts("sonnet", 1, 0, 0.0, 0),
    ];
    let wanted_checks = [
        ((3, 1), Some(33.3), verified.clone()),
        ((2, 1), Some(50.0), verified),
        ((1, 0), Some(0.0), Vec::new()),
    ];
    // The review part of a report of a repository and of a pipeline, each every one when
    // absent.
    let review_part = |repository: Option<&str>, pipeline: Option<&str>| {
        let request = ReportRequest {
            since: since.clone(),
            repository: repository.map(str::to_owned),
            pipeline: pipeline.map(str::to_owned),
            ..ReportRequest::default()
        };
        let report = h.engine.review_report(&CancellationToken::new(), &request);
        report.expect("the report reads").reviews
    };
    // Each pipeline runs on one repository here, so its counts are that repository's.
    let (notes, verified) = (pr.repository.as_str(), checked.repository.as_str());
    let pipelines = || {
        [
            review_part(None, Some("manual")),
            review_part(None, Some("checked")),
            review_part(Some(verified), Some("checked")),
        ]
    };
    let wanted_pipelines = [
        review_part(Some(notes), None),
        review_part(Some(verified), None),
        review_part(Some(verified), None),
    ];
    let kept = history();
    let kept_requests = requests();
    let kept_checks = checks();
    let kept_pipelines = pipelines();
    // A verification that does not decode stops the report of the range but not the statistics,
    // which count neither the verifier's steps nor the reviewer slots.
    let original = (h.store).load_step::<Verification>(&unverified.workflow_id, STEP_VERIFICATION);
    let original = original.expect("the verification reads");
    let damaged = h
        .store
        .save_step(&unverified.workflow_id, STEP_VERIFICATION, "damaged");
    damaged.expect("the verification is damaged");
    let statistics = history();
    let request = ReportRequest {
        since: since.clone(),
        ..ReportRequest::default()
    };
    let report = h.engine.review_report(&CancellationToken::new(), &request);
    let stopped = report.err().map(|err| error_code(&err).to_owned());
    let restored = h
        .store
        .save_step(&unverified.workflow_id, STEP_VERIFICATION, &original);
    restored.expect("the verification is restored");
    assert!(
        statistics == kept && stopped.as_deref() == Some("store_state_invalid"),
        "statistics = {statistics}, kept = {kept}, report stopped with {stopped:?}"
    );
    // The first prune leaves the new head's review beside the rollup of its request, and the
    // checked pull request's reviews as rows.
    let split = parse_rfc3339(&latest).expect("the creation time decodes");
    let partly = h.store.prune_reviews(split).expect("prune the earlier");
    let partly_requests = requests();
    let partly_checks = checks();
    let partly_pipelines = pipelines();
    let cutoff = OffsetDateTime::now_utc() + time::Duration::days(1);
    let pruned = h.store.prune_reviews(cutoff).expect("prune the reviews");
    let rolled_up = history();
    let rolled_up_requests = requests();
    let rolled_up_checks = checks();
    let rolled_up_pipelines = pipelines();
    let (median, p90) = kept_requests[0];
    assert!(
        partly == 2
            && pruned == 3
            && kept_checks == wanted_checks
            && partly_checks == kept_checks
            && rolled_up_checks == kept_checks
            && kept["totals"]["notes_to_author"] == json!(2)
            && kept["by_repository"][0]["notes_to_author"] == json!(2)
            && rolled_up == kept
            && median.is_some_and(|seconds| seconds >= 3600)
            && p90 == median
            && kept_requests[1] == kept_requests[0]
            && partly_requests == kept_requests
            && rolled_up_requests == kept_requests,
        "pruned {partly} and {pruned}: kept = {kept}, rolled up = {rolled_up}, request times \
         kept = {kept_requests:?}, partly = {partly_requests:?}, rolled up = \
         {rolled_up_requests:?}, checks kept = {kept_checks:?}, partly = {partly_checks:?}, \
         rolled up = {rolled_up_checks:?}"
    );
    assert!(
        kept_pipelines == wanted_pipelines
            && kept_pipelines[0].published == 3
            && kept_pipelines[1].multi_reviewer == 2
            && partly_pipelines == kept_pipelines
            && rolled_up_pipelines == kept_pipelines,
        "pipelines kept = {kept_pipelines:?}, wanted = {wanted_pipelines:?}, partly = \
         {partly_pipelines:?}, rolled up = {rolled_up_pipelines:?}"
    );
    h.stop_engine().await;
}

#[tokio::test]
async fn new_commits_after_a_notes_handoff_are_reviewed_and_wait_for_the_user() {
    let mut h = start_harness(|_| {}).await;
    let pr = hand_notes_to_the_author(&h).await;
    let mut next = pr.clone();
    next.head_sha = sha("c");
    next.review_request_id = 99;
    h.fake.open(next.clone(), h.rule(&pr.repository));
    h.poll_until("the approval request", || {
        h.fake.has_note(Level::Action, APPROVAL_REQUESTED_TITLE)
    })
    .await;
    assert!(
        h.started_count() == 3 && h.publications() == 1,
        "new commits must get an incremental review that waits for the user: {} sessions, {} \
         publications",
        h.started_count(),
        h.publications()
    );
    if let Err(err) = h.resolve(&next) {
        panic!("the new head must wait for approval: {err}");
    }
    h.stop_engine().await;
}

// On a rule the user approves, the next review covers only the new commits and settles the
// earlier blocking finding: the first review finds it, the second keeps it open, the third
// finds it fixed. The pipeline is Claude; the Codex verdicts are checked by
// `protocol::tests::codex_verdicts_settle_the_prior_findings`.
#[tokio::test]
async fn open_prior_finding_carries_over_until_it_is_fixed() {
    let prior_id = "O001";
    let mut h = start_harness(|_| {}).await;
    h.fake.lock().commentable = [("src/a.go".to_owned(), vec![3])].into();
    h.set_output(reviewer_output(vec![json!({
        "severity": "high", "confidence": "high", "blocking": true, "path": "src/a.go",
        "line": 3, "title": "Broken", "body": "It breaks.",
    })]));
    let pr = integration_pull_request("acme/manual", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the request for changes", || h.publications() == 1)
        .await;
    let key = prior_key(&pr.head_sha, prior_id);
    let open = vec![(key.clone(), key.clone(), "open".to_owned(), String::new())];
    h.wait_for("the open outcome", || outcomes(&h, &pr) == open)
        .await;

    h.fake.lock().discussion.threads = vec![DiscussionThread {
        key: prior_key(&pr.head_sha, prior_id),
        replies: Vec::new(),
        ..DiscussionThread::default()
    }];
    h.set_output(settling_output(Vec::new(), |_| PRIOR_OPEN));
    let mut second = pr.clone();
    second.head_sha = sha("c");
    h.fake.open(second, h.rule(&pr.repository));
    h.poll_until("the second review", || h.publications() == 2)
        .await;
    let (published, _, started, _) = h.fake.snapshot();
    assert!(
        started.len() == 3 && published[1].body.contains("incremental review"),
        "the re-review must be incremental with the incremental reviewer only, started {} \
         sessions: {}",
        started.len(),
        published[1].body
    );
    assert!(
        published[1].decision == Decision::RequestChanges
            && published[1].comments.is_empty()
            && published[1].marker.contains(":findings=1:")
            && published[1]
                .body
                .matches("**Still open, blocking**")
                .count()
                == 1
            && published[1]
                .body
                .contains(&format!("**Still open, blocking**: {prior_id} HIGH"))
            && !published[1].body.contains("S001"),
        "open prior findings must keep blocking without new threads: decision={:?} \
         comments={} marker={} body={}",
        published[1].decision,
        published[1].comments.len(),
        published[1].marker,
        published[1].body
    );

    h.set_output(reviewer_output(Vec::new()));
    let mut third = pr.clone();
    third.head_sha = sha("d");
    h.fake.open(third.clone(), h.rule(&pr.repository));
    let third_review_id = format!("Review ID: {}", third.review_id());
    h.poll_until("the approval request", || {
        h.fake.has_note(Level::Action, &third_review_id)
    })
    .await;
    assert!(
        h.fake.has_note(Level::Action, APPROVAL_REQUESTED_TITLE),
        "fixed prior findings must leave the review clean"
    );
    let reads = h.fake.lock().discussion_reads.clone();
    assert!(
        reads.len() == 2 && reads[1].len() == 1 && reads[1][0].head_sha == pr.head_sha,
        "the third review must still carry the findings of the first, read {reads:?}"
    );
    assert_eq!(outcomes(&h, &pr), open, "a finding left open stays open");
    approve_waiting(&h, &third, 3).await;
    let fixed = vec![(key.clone(), key, "fixed".to_owned(), third.head_sha.clone())];
    h.wait_for("the fixed outcome", || outcomes(&h, &pr) == fixed)
        .await;
    h.stop_engine().await;
}

#[tokio::test]
async fn rewritten_history_gets_a_full_review_that_settles_prior_findings() {
    let mut h = start_harness(|_| {}).await;
    h.fake.lock().commentable = [("src/a.go".to_owned(), vec![3])].into();
    h.set_output(reviewer_output(vec![json!({
        "severity": "high", "confidence": "high", "blocking": true, "path": "src/a.go",
        "line": 3, "title": "Broken", "body": "It breaks.",
    })]));
    let pr = integration_pull_request("acme/manual", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the request for changes", || h.publications() == 1)
        .await;
    h.set_output(reviewer_output(Vec::new()));
    let mut rebased = pr.clone();
    rebased.head_sha = sha("c");
    h.fake.lock().rewritten.insert(rebased.head_sha.clone());
    h.fake.open(rebased.clone(), h.rule(&pr.repository));
    let rebased_review_id = format!("Review ID: {}", rebased.review_id());
    h.poll_until("the approval request", || {
        h.fake.has_note(Level::Action, &rebased_review_id)
    })
    .await;
    let (_, _, started, _) = h.fake.snapshot();
    assert!(
        started.len() == 4 && h.fake.has_note(Level::Action, "Scope: full"),
        "a rebased head must get a full review by every reviewer, started {} sessions",
        started.len()
    );
    let reads = h.fake.lock().discussion_reads.len();
    assert_eq!(
        reads, 1,
        "the full review must still settle the prior findings"
    );
    h.stop_engine().await;
}

// A Codex pipeline that approves on its own reviews a rewritten head in full: an incremental
// review would approve the rebased head on the diff from a head it does not extend (#237).
#[tokio::test]
async fn rewritten_history_gets_a_full_review_on_a_codex_pipeline() {
    let mut h = start_harness(|_| {}).await;
    h.fake.lock().commentable = [("src/a.go".to_owned(), vec![3])].into();
    h.set_output(reviewer_output(vec![json!({
        "severity": "high", "confidence": "high", "path": "src/a.go", "line": 3,
        "title": "Broken", "body": "It breaks.",
    })]));
    let mut pr = integration_pull_request("public/widget", &sha("a"));
    pr.author = "maintainer".to_owned();
    pr.author_type = "User".to_owned();
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the request for changes", || h.publications() == 1)
        .await;
    h.set_output(reviewer_output(Vec::new()));
    for (head, rewritten, count) in [("c", false, 2), ("d", false, 3), ("e", true, 4)] {
        let mut next = pr.clone();
        next.head_sha = sha(head);
        if rewritten {
            h.fake.lock().rewritten.insert(next.head_sha.clone());
        }
        h.fake.open(next, h.rule(&pr.repository));
        h.poll_until("the next review", || h.publications() == count)
            .await;
    }
    let (published, _, started, _) = h.fake.snapshot();
    let scopes: Vec<(Decision, bool, bool)> = published
        .iter()
        .zip(&started)
        .map(|(review, request)| {
            (
                review.decision.clone(),
                review.body.contains("full review of"),
                request
                    .prompt
                    .contains("Trusted previously reviewed head SHA"),
            )
        })
        .collect();
    assert_eq!(
        scopes,
        [
            (Decision::RequestChanges, true, false),
            (Decision::Approve, true, false),
            (Decision::Approve, false, true),
            (Decision::Approve, true, false),
        ],
        "after a request for changes and after a rewritten history the review must be full; \
         only the linear push after the approval is incremental (decision, full, previous head)"
    );
    h.stop_engine().await;
}

// A pull request retargeted to another base branch is reviewed in full: commits that joined the
// diff through the retarget were never reviewed, so an incremental approval would cover them
// (#240). A new base SHA on the same branch keeps the review incremental.
#[tokio::test]
async fn retargeted_pull_request_gets_a_full_review_before_it_is_approved() {
    let mut h = start_harness(|_| {}).await;
    h.set_output(reviewer_output(Vec::new()));
    let pr = integration_pull_request("claude/base", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the first approval", || h.publications() == 1)
        .await;
    for (head, base_sha, count) in [("c", "e", 2), ("d", "f", 3)] {
        let mut next = pr.clone();
        next.head_sha = sha(head);
        next.base_branch = "release".to_owned();
        next.base_sha = sha(base_sha);
        h.fake.open(next, h.rule(&pr.repository));
        h.poll_until("the next approval", || h.publications() == count)
            .await;
    }
    let (published, _, started, _) = h.fake.snapshot();
    let scopes: Vec<(Decision, bool, bool)> = published
        .iter()
        .zip(&started)
        .map(|(review, request)| {
            (
                review.decision.clone(),
                review.body.contains("full review of"),
                request.prompt.contains("Review scope: incremental"),
            )
        })
        .collect();
    assert_eq!(
        scopes,
        [
            (Decision::Approve, true, false),
            (Decision::Approve, true, false),
            (Decision::Approve, false, true),
        ],
        "the push after the retarget must be reviewed in full before it is approved; the next \
         push on the new base is incremental (decision, full, previous head)"
    );
    h.stop_engine().await;
}

// A retarget while a review runs supersedes that review: it reads the diff against the old
// base, so its approval would cover commits it never saw. The next review is full (#279).
#[tokio::test]
async fn retarget_during_a_review_publishes_nothing_and_the_next_review_is_full() {
    let mut h = start_harness(|_| {}).await;
    h.set_output(reviewer_output(Vec::new()));
    let pr = integration_pull_request("claude/base", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the first approval", || h.publications() == 1)
        .await;
    h.set_output(|_: &SessionRequest| String::new());
    let mut pushed = pr.clone();
    pushed.head_sha = sha("c");
    h.fake.open(pushed.clone(), h.rule(&pr.repository));
    h.poll_until("the incremental review", || h.started_count() == 2)
        .await;
    let mut retargeted = pushed.clone();
    retargeted.base_branch = "release".to_owned();
    retargeted.base_sha = sha("e");
    h.fake.open(retargeted, h.rule(&pr.repository));
    h.set_output(reviewer_output(Vec::new()));
    h.poll_until("the next approval", || h.publications() == 2)
        .await;
    let (published, _, started, _) = h.fake.snapshot();
    let scopes: Vec<bool> = started
        .iter()
        .map(|request| request.prompt.contains("Review scope: incremental"))
        .collect();
    assert_eq!(
        (scopes, published[1].body.contains("full review of")),
        (vec![false, true, false], true),
        "the review that ran during the retarget must publish nothing, and the review after it \
         must be full (incremental per started review, full publication)"
    );
    assert!(
        h.fake
            .has_note(Level::Log, "superseded (base_branch_changed)"),
        "the retarget must supersede the running review"
    );
    // The superseded review read `main` but published nothing, so `main` is reviewed again.
    h.fake.open(pushed, h.rule(&pr.repository));
    h.poll_until("the review after the retarget back", || {
        h.started_count() == 4
    })
    .await;
    h.settle().await;
    let (_, _, started, _) = h.fake.snapshot();
    assert!(
        started.len() == 4 && !started[3].prompt.contains("Review scope: incremental"),
        "the retarget back to main must get one full review, started {} sessions",
        started.len()
    );
    h.stop_engine().await;
}

// A retarget without a push gets a full review of the same head: the diff gained commits that
// no review read. It settles the findings of the published review and runs once (#280).
#[tokio::test]
async fn retarget_without_a_push_gets_a_full_review_of_the_same_head() {
    let mut h = start_harness(|_| {}).await;
    h.fake.lock().commentable = [("src/a.go".to_owned(), vec![3])].into();
    h.set_output(reviewer_output(vec![json!({
        "severity": "high", "confidence": "high", "blocking": true, "path": "src/a.go",
        "line": 3, "title": "Broken", "body": "It breaks.",
    })]));
    let pr = integration_pull_request("claude/base", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the request for changes", || h.publications() == 1)
        .await;
    h.set_output(reviewer_output(Vec::new()));
    let mut retargeted = pr.clone();
    retargeted.base_branch = "release".to_owned();
    retargeted.base_sha = sha("e");
    h.fake.open(retargeted, h.rule(&pr.repository));
    h.poll_until("the review after the retarget", || h.publications() == 2)
        .await;
    h.settle().await;
    let (published, _, started, _) = h.fake.snapshot();
    let reviews: Vec<(Decision, bool)> = published
        .iter()
        .map(|review| {
            (
                review.decision.clone(),
                review.body.contains("full review of"),
            )
        })
        .collect();
    let reads = h.fake.lock().discussion_reads.len();
    assert_eq!(
        (reviews, started.len(), reads),
        (
            vec![(Decision::RequestChanges, true), (Decision::Approve, true)],
            2,
            1
        ),
        "a retarget of the reviewed head must get one full review that settles the earlier \
         findings (decision, full; sessions; discussion reads)"
    );
    h.stop_engine().await;
}

// A return to a base branch whose approval GitHub dismissed at the retarget gets a review that
// approves again; a return to one whose approval stands reviews nothing. Each return looks the
// approval up once, and a settled pull request is not looked up again (#316). A narrower
// credential of another account added after the approvals does not change whose approval the
// lookup finds (#18).
#[tokio::test]
async fn dismissed_approval_is_replaced_after_a_retarget_away_and_back() {
    let narrow = Arc::new(AtomicBool::new(false));
    let switch = Arc::clone(&narrow);
    let mut h = start_harness(move |config| {
        if switch.load(Ordering::SeqCst) {
            let only = config
                .credentials
                .single()
                .cloned()
                .expect("one credential");
            let narrower = Credential {
                name: "narrow".to_owned(),
                login: "narrow-bot".to_owned(),
                token: "narrow-token".to_owned(),
                matches: vec!["claude/base".to_owned()],
            };
            config.credentials =
                Credentials::new(vec![narrower, only]).expect("the credentials load");
        }
    })
    .await;
    h.set_output(reviewer_output(Vec::new()));
    let pr = integration_pull_request("claude/base", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the first approval", || h.publications() == 1)
        .await;
    let mut release = pr.clone();
    release.base_branch = "release".to_owned();
    release.base_sha = sha("e");
    h.fake.open(release.clone(), h.rule(&pr.repository));
    h.poll_until("the approval on release", || h.publications() == 2)
        .await;
    h.stop_engine().await;
    narrow.store(true, Ordering::SeqCst);
    h.start();
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.settle().await;
    h.fake.open(release.clone(), h.rule(&pr.repository));
    h.settle().await;
    let standing = (h.started_count(), h.publications(), h.fake.lock().lookups);
    h.fake.dismiss(0);
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.settle().await;
    assert_eq!(
        (standing, h.started_count()),
        ((2, 2, 2), 3),
        "returns to standing approvals must review nothing and look each up once, and the \
         return to the dismissed approval must start a review ((sessions, publications, \
         lookups) while standing, sessions after the dismissal)"
    );
    h.poll_until("the approval that replaces the dismissed one", || {
        h.publications() == 3
    })
    .await;
    h.settle().await;
    let lookups = h.fake.lock().lookups;
    h.poll().await;
    h.poll().await;
    let (published, _, started, _) = h.fake.snapshot();
    assert_eq!(
        (
            published[2].decision.clone(),
            published[2].body.contains("full review of"),
            published[2].marker == published[0].marker,
            started.len(),
            h.fake.lock().lookups - lookups,
        ),
        (Decision::Approve, true, true, 3, 0),
        "the review after the dismissal must post a new full approval for main and the settled \
         pull request must not be looked up again (decision, full, marker of main, sessions, \
         lookups of two more passes)"
    );
    h.stop_engine().await;
}

#[tokio::test]
async fn missing_previous_review_falls_back_to_a_full_review() {
    let mut h = start_harness(|_| {}).await;
    h.fake.lock().commentable = [("src/a.go".to_owned(), vec![3])].into();
    h.set_output(reviewer_output(vec![json!({
        "severity": "high", "confidence": "high", "blocking": true, "path": "src/a.go",
        "line": 3, "title": "Broken", "body": "It breaks.",
    })]));
    let pr = integration_pull_request("acme/manual", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the request for changes", || h.publications() == 1)
        .await;
    h.store
        .in_tx(|transaction| {
            transaction
                .execute("DELETE FROM review_steps", [])
                .expect("the review steps are deleted");
            Ok(())
        })
        .expect("the delete of the review steps commits");
    h.set_output(reviewer_output(Vec::new()));
    let mut next = pr.clone();
    next.head_sha = sha("c");
    h.fake.open(next.clone(), h.rule(&pr.repository));
    let next_review_id = format!("Review ID: {}", next.review_id());
    h.poll_until("the approval request", || {
        h.fake.has_note(Level::Action, &next_review_id)
    })
    .await;
    let (_, _, started, _) = h.fake.snapshot();
    let reads = h.fake.lock().discussion_reads.len();
    assert!(
        started.len() == 4 && reads == 0,
        "without the previous result the review must be full and carry no prior findings: \
         started {}, discussion reads {reads}",
        started.len()
    );
    h.stop_engine().await;
}

// A resolved thread leaves an open prior finding nothing to answer.
// So the review publishes it again instead of only naming it.
#[tokio::test]
async fn open_prior_finding_without_an_open_thread_is_published_again() {
    let mut h = start_harness(|_| {}).await;
    h.fake.lock().commentable = [("src/a.go".to_owned(), vec![3, 4])].into();
    // Distinct lines keep the two reviewers' findings apart as O001 and S001.
    let opus = reviewer_output(vec![json!({
        "severity": "high", "confidence": "high", "blocking": true, "path": "src/a.go",
        "line": 3, "title": "Broken", "body": "It breaks.",
    })]);
    let sonnet = reviewer_output(vec![json!({
        "severity": "high", "confidence": "high", "blocking": true, "path": "src/a.go",
        "line": 4, "title": "Also broken", "body": "It breaks too.",
    })]);
    h.set_output(move |request: &SessionRequest| {
        if request.reviewer.id == "sonnet" {
            sonnet(request)
        } else {
            opus(request)
        }
    });
    let pr = integration_pull_request("acme/manual", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the request for changes", || h.publications() == 1)
        .await;
    h.fake.lock().discussion.threads = vec![
        DiscussionThread {
            key: prior_key(&pr.head_sha, "O001"),
            resolved: true,
            replies: Vec::new(),
        },
        DiscussionThread {
            key: prior_key(&pr.head_sha, "S001"),
            replies: Vec::new(),
            ..DiscussionThread::default()
        },
    ];
    h.set_output(settling_output(Vec::new(), |_| PRIOR_OPEN));
    let mut next = pr.clone();
    next.head_sha = sha("c");
    h.fake.open(next, h.rule(&pr.repository));
    h.poll_until("the second review", || h.publications() == 2)
        .await;
    let (published, _, _, _) = h.fake.snapshot();
    let review = &published[1];
    assert!(
        review.decision == Decision::RequestChanges
            && review.comments.len() == 1
            && review.comments[0].body.contains("Still open from O001")
            && review.marker.contains(":findings=2:"),
        "decision={:?} comments={:?} marker={}",
        review.decision,
        review.comments,
        review.marker
    );
    // The finding published again is the first one, tracked under the key of its new finding.
    let opus_key = prior_key(&pr.head_sha, "O001");
    let sonnet_key = prior_key(&pr.head_sha, "S001");
    let republished = prior_key(&sha("c"), "P001");
    let row = |key: &str, tracked: &str, outcome: &str, head: &str| {
        let (key, tracked) = (key.to_owned(), tracked.to_owned());
        (key, tracked, outcome.to_owned(), head.to_owned())
    };
    let rows = vec![
        row(&opus_key, &republished, "open", ""),
        row(&sonnet_key, &sonnet_key, "open", ""),
    ];
    h.wait_for("the second completed review", || completed(&h, &pr) == 2)
        .await;
    let stored = outcomes(&h, &pr);
    assert_eq!(stored, rows, "outcomes after the second review");

    h.set_output(reviewer_output(Vec::new()));
    let mut last = pr.clone();
    last.head_sha = sha("d");
    h.fake.open(last.clone(), h.rule(&pr.repository));
    h.poll_until("the approval wait", || h.resolve(&last).is_ok())
        .await;
    approve_waiting(&h, &last, 3).await;
    let fixed = vec![
        row(&opus_key, &republished, "fixed", &last.head_sha),
        row(&sonnet_key, &sonnet_key, "fixed", &last.head_sha),
    ];
    h.wait_for("the fixed outcomes", || outcomes(&h, &pr) == fixed)
        .await;
    h.stop_engine().await;
}

// A merge ends the findings that are still open at the merged head, a settled finding keeps
// its outcome, and the ended pull request is not read again. A pull request whose read keeps
// failing does not hold the merge back.
#[tokio::test]
async fn merge_ends_the_open_findings_and_keeps_the_settled_ones() {
    let mut h = start_harness(|config| {
        config.outcome_check_interval = Duration::from_millis(1);
    })
    .await;
    h.fake.lock().commentable = [("src/a.go".to_owned(), vec![3, 4])].into();
    h.set_output(reviewer_output(vec![
        json!({
            "severity": "high", "confidence": "high", "blocking": true, "path": "src/a.go",
            "line": 3, "title": "Broken", "body": "It breaks.",
        }),
        json!({
            "severity": "high", "confidence": "high", "blocking": true, "path": "src/a.go",
            "line": 4, "title": "Also broken", "body": "It breaks too.",
        }),
    ]));
    let pr = integration_pull_request("acme/manual", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the request for changes", || h.publications() == 1)
        .await;
    let fixed_key = prior_key(&pr.head_sha, "O001");
    let open_key = prior_key(&pr.head_sha, "O002");
    h.fake.lock().discussion.threads = vec![DiscussionThread {
        key: open_key.clone(),
        replies: Vec::new(),
        ..DiscussionThread::default()
    }];
    h.set_output(settling_output(Vec::new(), |prior| {
        match prior.id.as_str() {
            "O001" => PRIOR_FIXED,
            _ => PRIOR_OPEN,
        }
    }));
    let mut next = pr.clone();
    next.head_sha = sha("c");
    h.fake.open(next.clone(), h.rule(&pr.repository));
    h.poll_until("the second review", || h.publications() == 2)
        .await;
    let row = |key: &str, outcome: &str, head: &str| {
        let (key, outcome, head) = (key.to_owned(), outcome.to_owned(), head.to_owned());
        (key.clone(), key, outcome, head)
    };
    let fixed = row(&fixed_key, "fixed", &next.head_sha);
    let settled = vec![fixed.clone(), row(&open_key, "open", "")];
    h.wait_for("the settled outcomes", || outcomes(&h, &pr) == settled)
        .await;

    // A pull request whose state read always fails sorts first on the same credential.
    let broken = FindingOutcome {
        repository: pr.repository.clone(),
        finding_key: fixed_key.clone(),
        outcome: "open".to_owned(),
        published_at: "2026-01-01T00:00:00Z".to_owned(),
        ..FindingOutcome::default()
    };
    (h.store)
        .record_outcomes(&pr.repository, 0, &pr.head_sha, &[], &[broken])
        .expect("the broken outcome is stored");
    let key = format!("{}#0", pr.repository);
    h.fake.lock().failing_state_reads.insert(key);
    let mut merged = pr.clone();
    merged.head_sha = sha("e");
    h.fake.merge(merged.clone());
    let ended = vec![fixed, row(&open_key, "open_at_merge", &merged.head_sha)];
    h.poll_until("the open_at_merge outcome", || outcomes(&h, &pr) == ended)
        .await;
    let reads = state_reads(&h, &pr);
    h.settle().await;
    assert_eq!(state_reads(&h, &pr), reads, "an ended pull request");
    h.stop_engine().await;
}

// A close ends the open notes at the closed head. A pull request that left the search is read
// at most once per check interval, so the close is seen only after the interval.
#[tokio::test]
async fn close_ends_the_open_notes_after_the_check_interval() {
    let short = Arc::new(AtomicBool::new(false));
    let switch = Arc::clone(&short);
    let mut h = start_harness(move |config| {
        if switch.load(Ordering::SeqCst) {
            config.outcome_check_interval = Duration::from_millis(1);
        }
    })
    .await;
    let pr = hand_notes_to_the_author(&h).await;
    let key = prior_key(&pr.head_sha, "O001");
    let row = |outcome: &str, head: &str| {
        let (outcome, head) = (outcome.to_owned(), head.to_owned());
        vec![(key.clone(), key.clone(), outcome, head)]
    };
    let open = row("open", "");
    h.wait_for("the open note outcome", || outcomes(&h, &pr) == open)
        .await;
    // The pull request leaves the search while it is still open.
    h.fake.lock().candidates.remove(&pr.key());
    h.poll().await;
    assert_eq!(state_reads(&h, &pr), 1, "a pull request out of the search");
    h.fake.close(pr.clone());
    h.settle().await;
    let (reads, stored) = (state_reads(&h, &pr), outcomes(&h, &pr));
    assert!(
        reads == 1 && stored == open,
        "no read within the interval: reads={reads} outcomes={stored:?}"
    );
    h.stop_engine().await;
    short.store(true, Ordering::SeqCst);
    h.start();
    let closed = row("open_at_close", &pr.head_sha);
    h.poll_until("the open_at_close outcome", || outcomes(&h, &pr) == closed)
        .await;
    h.stop_engine().await;
}
