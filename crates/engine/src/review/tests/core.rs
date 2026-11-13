//! Core scenarios of the engine: serve, discovery, reviews and their publication.

// Rust guideline compliant 2026-06-26

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use autoreview_policy::{Event, Level};
use serde_json::json;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

use crate::config::{HostOrder, HostStateChoice, HostStateRules, NotifyRoute};
use crate::engine::{Engine, SystemClock};
use crate::error::{Error, error_code};
use crate::integrations::SessionRequest;
use crate::model::review::Decision;
use crate::model::{
    HostCapacity, HostCpu, HostGpu, HostMemory, HostModels, HostPolicy, HostWindow, PullRequest,
    RejectedCandidate,
};
use crate::protocol::fixtures::sha;
use crate::review::input::NOTIFY_STEP_PREFIX;
use crate::testing::fake::{INTEGRATION_POLICY, count_notes, reviewer_output};
use crate::testing::harness::{FakeClock, integration_pull_request, start_harness};

/// Output of a reviewer session that has not finished: Go's nil output.
fn unfinished(_: &SessionRequest) -> String {
    String::new()
}

// A second engine on the same store is refused while the first one serves.
#[tokio::test]
async fn second_engine_on_the_same_store_is_refused() {
    let mut h = start_harness(|_| {}).await;
    let second = Arc::new(Engine::new(
        h.config(),
        Arc::clone(&h.store),
        Arc::clone(&h.fake),
        SystemClock,
    ));
    let err = second
        .serve(&CancellationToken::new())
        .await
        .expect_err("the second engine is refused");
    assert_eq!(
        error_code(&err),
        "engine_already_running",
        "second engine error = {err:?}"
    );
    h.stop_engine().await;
}

#[tokio::test]
async fn clean_review_of_an_approving_rule_is_approved_once_without_the_user() {
    let mut h = start_harness(|_| {}).await;
    let pr = integration_pull_request("acme/widget", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("an automatic approval", || h.publications() == 1)
        .await;
    h.wait_for("the approval notification", || {
        h.fake.has_note(Level::Digest, "Approved acme/widget PR")
    })
    .await;
    let (published, notes, started, open) = h.fake.snapshot();
    assert!(
        published[0].decision == Decision::Approve
            && published[0].comments.is_empty()
            && started.len() == 2
            && open == 0,
        "decision={:?} comments={} started={} open={open}",
        published[0].decision,
        published[0].comments.len(),
        started.len()
    );
    for entry in &notes {
        assert!(
            entry.level != Level::Action,
            "an automatic approval must not ping the user: {entry:?}"
        );
    }
    for _ in 0..3 {
        h.poll().await;
    }
    assert!(
        h.started_count() == 2 && h.publications() == 1,
        "a published head was reviewed again"
    );
    h.stop_engine().await;
}

#[tokio::test]
async fn review_sessions_use_the_configured_name_and_branch_prefixes() {
    let mut h = start_harness(|cfg| {
        cfg.session_name_prefix = "team-review-".to_owned();
        cfg.session_branch_prefix = "team/review/".to_owned();
    })
    .await;
    let pr = integration_pull_request("acme/widget", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("an automatic approval", || h.publications() == 1)
        .await;
    let (_, _, started, _) = h.fake.snapshot();
    assert_eq!(started.len(), 2, "started = {started:?}");
    for request in &started {
        let token = format!(
            "{}-{}-run{}",
            request.workflow_id, request.reviewer.id, request.run
        );
        assert_eq!(request.name(), format!("team-review-{token}"));
        assert_eq!(request.branch(), format!("team/review/{token}"));
    }
    h.stop_engine().await;
}

#[tokio::test]
async fn findings_request_changes_and_the_next_head_gets_a_full_review() {
    let mut h = start_harness(|_| {}).await;
    h.set_output(reviewer_output(vec![json!({
        "severity": "high", "confidence": "high", "path": "src/a.go", "line": 3,
        "title": "Broken", "body": "It breaks.",
    })]));
    h.fake.lock().commentable = [("src/a.go".to_owned(), vec![3])].into();
    let pr = integration_pull_request("acme/widget", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("a request for changes", || h.publications() == 1)
        .await;
    let (published, _, _, _) = h.fake.snapshot();
    assert!(
        published[0].decision == Decision::RequestChanges && published[0].comments.len() == 1,
        "decision={:?} comments={}",
        published[0].decision,
        published[0].comments.len()
    );
    h.set_output(reviewer_output(Vec::new()));
    let mut next = pr.clone();
    next.head_sha = sha("c");
    h.fake.open(next, h.rule(&pr.repository));
    h.poll_until("the review of the next head", || h.publications() == 2)
        .await;
    let (_, _, started, _) = h.fake.snapshot();
    assert_eq!(
        started.len(),
        4,
        "after a request for changes both reviewers must review in full, started {} sessions",
        started.len()
    );
    let (published, _, _, _) = h.fake.snapshot();
    assert!(
        published[1].decision == Decision::Approve && published[1].body.contains("full review"),
        "second review = {:?}: {}",
        published[1].decision,
        published[1].body
    );
    h.stop_engine().await;
}

// Two reviewers that report the same defect publish it once, as the first reviewer's finding.
#[tokio::test]
async fn same_finding_of_two_reviewers_is_published_once() {
    let mut h = start_harness(|_| {}).await;
    let opus = reviewer_output(vec![json!({
        "severity": "high", "confidence": "medium", "path": "src/a.go", "line": 3,
        "title": "Broken", "body": "It breaks.",
    })]);
    let sonnet = reviewer_output(vec![json!({
        "severity": "high", "confidence": "high", "path": "src/a.go", "line": 3,
        "title": "Broken on empty input", "body": "It breaks on an empty input.",
    })]);
    h.set_output(move |request: &SessionRequest| {
        if request.reviewer.id == "sonnet" {
            sonnet(request)
        } else {
            opus(request)
        }
    });
    h.fake.lock().commentable = [("src/a.go".to_owned(), vec![3])].into();
    let pr = integration_pull_request("acme/widget", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("a request for changes", || h.publications() == 1)
        .await;
    let (published, _, _, _) = h.fake.snapshot();
    let review = &published[0];
    let heading = "**O001 HIGH — Broken on empty input** (confidence: high, Opus + Sonnet)\n\n\
                   It breaks on an empty input.";
    assert!(
        review.decision == Decision::RequestChanges
            && review.comments.len() == 1
            && review.comments[0].body.starts_with(heading)
            && review.marker.contains(":findings=1:")
            && review
                .body
                .contains("1 blocking finding(s) are posted as inline comments.")
            && review.body.contains("**Opus:** ")
            && !review.body.contains("**Sonnet:** "),
        "decision={:?} comments={:?} marker={} body={}",
        review.decision,
        review.comments,
        review.marker,
        review.body
    );
    h.stop_engine().await;
}

#[tokio::test]
async fn approved_head_is_followed_by_an_incremental_review() {
    let mut h = start_harness(|_| {}).await;
    let pr = integration_pull_request("acme/widget", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the first approval", || h.publications() == 1)
        .await;
    let mut next = pr.clone();
    next.head_sha = sha("c");
    next.review_request_id = 12;
    h.fake.open(next, h.rule(&pr.repository));
    h.poll_until("the incremental approval", || h.publications() == 2)
        .await;
    let (published, _, started, _) = h.fake.snapshot();
    assert!(
        started.len() == 4 && published[1].body.contains("incremental review"),
        "an automatically approved re-review must use every reviewer, started {} sessions: {}",
        started.len(),
        published[1].body
    );
    h.stop_engine().await;
}

#[tokio::test]
async fn stuck_session_cleanup_is_reported_and_the_review_still_publishes() {
    let mut h = start_harness(|_| {}).await;
    h.fake.lock().remove_failing.insert("s-1".to_owned());
    let pr = integration_pull_request("acme/widget", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the publication despite a stuck session", || {
        h.publications() == 1
    })
    .await;
    assert!(
        h.fake
            .has_note(Level::Action, "Pohunek could not remove reviewer sessions")
            && h.fake
                .has_note(Level::Action, "runtime_supervision_ambiguous"),
        "the stuck session was not reported for manual removal"
    );
    h.stop_engine().await;
}

#[tokio::test]
async fn unusable_reviewer_output_gets_a_fresh_run() {
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
    let (_, _, started, _) = h.fake.snapshot();
    let runs = started
        .iter()
        .filter(|request| request.reviewer.id == "opus")
        .count();
    assert_eq!(runs, 2, "opus ran {runs} times");
    // A wrapper refusal is not the model's mistake, so no request names a refused check.
    let check = "the output check review_output_invalid";
    assert!(
        !started.iter().any(|request| request.prompt.contains(check)),
        "a request names a refused check: {started:?}"
    );
    h.stop_engine().await;
}

/// A reviewer output that passes the schema but fails the engine's own limits, such as a
/// multiline title, is a model mistake that a fresh run can fix.
///
/// The request of the fresh run names the check that refused the run before.
#[tokio::test]
async fn reviewer_output_over_the_limits_gets_a_fresh_run() {
    let mut h = start_harness(|_| {}).await;
    let valid = reviewer_output(Vec::new());
    let invalid = reviewer_output(vec![json!({
        "severity": "high", "confidence": "high", "blocking": true, "path": "src/a.go",
        "line": 3, "title": "Bug\nsecond line", "body": "Details.",
    })]);
    h.set_output(move |request: &SessionRequest| {
        if request.reviewer.id == "opus" && request.run == 1 {
            return invalid(request);
        }
        valid(request)
    });
    let pr = integration_pull_request("acme/widget", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the publication after a reviewer retry", || {
        h.publications() == 1
    })
    .await;
    let (_, _, started, _) = h.fake.snapshot();
    let runs = started
        .iter()
        .filter(|request| request.reviewer.id == "opus")
        .count();
    assert_eq!(runs, 2, "opus ran {runs} times");
    let told: Vec<(i64, bool)> = started
        .iter()
        .filter(|request| request.reviewer.id == "opus")
        .map(|request| {
            let named = request
                .prompt
                .contains("refused by the output check review_output_invalid because");
            (request.run, named)
        })
        .collect();
    assert_eq!(
        told,
        vec![(1, false), (2, true)],
        "opus runs and the named check"
    );
    h.stop_engine().await;
}

/// The most findings with the longest bodies still give a short review body.
///
/// Every finding is a note at the body limit, and every reviewer request states the limits.
#[tokio::test]
async fn review_with_the_most_findings_keeps_a_short_body() {
    let mut h = start_harness(|_| {}).await;
    let cfg = h.config();
    assert!(
        cfg.max_findings * cfg.max_body_characters > cfg.github_body_max_bytes,
        "the note bodies alone must exceed the GitHub body limit"
    );
    let count = usize::try_from(cfg.max_findings).expect("the finding limit is positive");
    let longest = usize::try_from(cfg.max_body_characters).expect("the body limit is positive");
    let notes: Vec<_> = (1..=count)
        .map(|line| {
            json!({
                "severity": "low", "confidence": "high", "blocking": false, "path": "src/a.go",
                "line": line, "title": format!("Note {line}"), "body": "n".repeat(longest),
            })
        })
        .collect();
    let opus = reviewer_output(notes);
    let sonnet = reviewer_output(Vec::new());
    h.set_output(move |request: &SessionRequest| {
        if request.reviewer.id == "sonnet" {
            sonnet(request)
        } else {
            opus(request)
        }
    });
    let pr = integration_pull_request("acme/widget", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the review with the most findings", || {
        h.publications() == 1
    })
    .await;
    let (published, _, started, _) = h.fake.snapshot();
    let body = &published[0].body;
    assert!(
        published[0].decision == Decision::Approve
            && body.contains(&format!("{count} non-blocking note(s)"))
            && body.contains(&format!("**O{count:03} LOW — Note "))
            && !body.contains("nnnn")
            && body.chars().count() < longest,
        "decision={:?} length={} body={body}",
        published[0].decision,
        body.chars().count()
    );
    let limits = format!("a body of at most {} characters", cfg.max_body_characters);
    assert!(
        started.len() == 2
            && started
                .iter()
                .all(|request| request.prompt.contains(&limits)),
        "every reviewer request must state the limits: {started:?}"
    );
    h.stop_engine().await;
}

#[tokio::test]
async fn base_fetch_locked_by_a_concurrent_review_is_retried_on_a_fresh_branch() {
    let mut h = start_harness(|_| {}).await;
    h.fake.lock().start_fail = Some(Arc::new(|request: &SessionRequest| {
        if request.reviewer.id == "opus" && request.start <= 1 {
            return Err(Error::workflow(
                "pohunek_base_fetch_ref_locked",
                false,
                None,
            ));
        }
        Ok(())
    }));
    let pr = integration_pull_request("acme/widget", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the publication after a locked base fetch", || {
        h.publications() == 1
    })
    .await;
    let branches: BTreeSet<String> = h
        .fake
        .lock()
        .attempts
        .iter()
        .filter(|request| request.reviewer.id == "opus")
        .map(SessionRequest::branch)
        .collect();
    // Pohunek keeps the failed start's branch, so reusing it would check out a stale branch
    // instead of the fresh base.
    assert_eq!(branches.len(), 2, "opus started on branches {branches:?}");
    h.stop_engine().await;
}

#[tokio::test]
async fn own_pull_request_gets_a_comment_and_findings_ping_the_author() {
    let mut h = start_harness(|_| {}).await;
    h.fake.lock().commentable = [("a.go".to_owned(), vec![1])].into();
    h.set_output(reviewer_output(vec![json!({
        "severity": "medium", "confidence": "high", "path": "a.go", "line": 1,
        "title": "Fix me", "body": "Details.",
    })]));
    let mut pr = integration_pull_request("acme/widget", &sha("a"));
    let served = h.engine.config.credentials.resolve("acme/widget");
    pr.author = served
        .expect("a credential serves acme/widget")
        .login
        .clone();
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the comment", || h.publications() == 1).await;
    let (published, _, _, _) = h.fake.snapshot();
    assert!(
        published[0].decision == Decision::Comment
            && published[0].body.contains("own pull request"),
        "own pull request review = {:?}: {}",
        published[0].decision,
        published[0].body
    );
    h.wait_for("the action notification", || {
        h.fake.has_note(Level::Action, "Commented on acme/widget")
    })
    .await;
    h.stop_engine().await;
}

#[tokio::test]
async fn rejected_pull_request_is_reported_once_and_does_not_block_others() {
    let mut h = start_harness(|_| {}).await;
    let broken = integration_pull_request("acme/widget", &sha("e"));
    h.fake.lock().rejected = vec![RejectedCandidate {
        repository: broken.repository.clone(),
        number: broken.number,
        code: "github_response_invalid".to_owned(),
        permission_login: None,
    }];
    let pr = integration_pull_request("acme/widget", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the review of the valid pull request", || {
        h.publications() == 1
    })
    .await;
    let (_, notes, _, _) = h.fake.snapshot();
    let fragment = format!("{} is not started", broken.key());
    let reports = count_notes(&notes, Level::Action, &fragment);
    assert_eq!(
        reports, 1,
        "the rejected pull request was reported {reports} times"
    );
    h.stop_engine().await;
}

// An event route carries its event, in discovery and in a review (#187).
// Every other event keeps the route of its level.
#[tokio::test]
async fn event_routes_carry_their_events_and_the_rest_keep_their_levels() {
    let mut h = start_harness(|cfg| {
        cfg.matrix_rooms.events = BTreeMap::from([
            (Event::Approved, "!approved:example.org".to_owned()),
            (Event::ReviewStopped, "!stopped:example.org".to_owned()),
        ]);
    })
    .await;
    let broken = integration_pull_request("acme/widget", &sha("e"));
    h.fake.lock().rejected = vec![RejectedCandidate {
        repository: broken.repository.clone(),
        number: broken.number,
        code: "github_response_invalid".to_owned(),
        permission_login: None,
    }];
    let pr = integration_pull_request("acme/widget", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("an automatic approval", || h.publications() == 1)
        .await;
    h.wait_for("the approval notification", || {
        h.fake.has_note(Level::Digest, "Approved acme/widget PR")
    })
    .await;
    let (_, notes, _, _) = h.fake.snapshot();
    let rejected = format!("{} is not started", broken.key());
    let mut own = Vec::new();
    for note in &notes {
        let expected = if note.message.contains("Approved acme/widget PR") {
            NotifyRoute::Event(Event::Approved)
        } else if note.message.contains(&rejected) {
            NotifyRoute::Event(Event::ReviewStopped)
        } else {
            NotifyRoute::Level(note.level.clone())
        };
        assert_eq!(note.route, expected, "route of {note:?}");
        if matches!(note.route, NotifyRoute::Event(_)) {
            own.push(note.route.clone());
        }
    }
    assert_eq!(
        own,
        [
            NotifyRoute::Event(Event::ReviewStopped),
            NotifyRoute::Event(Event::Approved)
        ],
        "each routed event was sent once on its own route"
    );
    assert!(
        own.len() < notes.len(),
        "no notification took a level route: {notes:?}"
    );
    h.stop_engine().await;
}

// A review host that fails is skipped: the review runs on the next host of `pohunek.hosts`
// (#180). With host-state rules the idle host is tried first and a host on battery is never
// asked for its projects.
#[tokio::test]
async fn failing_first_host_is_skipped_and_the_review_runs_on_the_second() {
    let mut h = start_harness(|cfg| {
        cfg.pohunek_hosts = ["host-a", "host-b", "host-c"].map(str::to_owned).to_vec();
        cfg.host_state = Some(HostStateRules {
            max_age: Duration::from_secs(120),
            read_timeout: Duration::from_secs(20),
            unknown_state: HostStateChoice::Skip,
            order: HostOrder::IdleFirst,
            on_battery: HostStateChoice::Skip,
            min_battery_percent: 0,
            min_free_memory_mb: 0,
            required_models: Vec::new(),
            outside_window: HostStateChoice::Skip,
        });
    })
    .await;
    // The fake stamps each state with the current time.
    let state = |idle, ac_power| HostCapacity {
        schema_version: "v1".to_owned(),
        host_alias: "host".to_owned(),
        timestamp: OffsetDateTime::UNIX_EPOCH,
        idle,
        idle_reason: "test".to_owned(),
        seconds_since_input: 0,
        ac_power,
        battery_percent: None,
        cpu: HostCpu {
            cores: 1,
            load1m: 0.0,
        },
        memory: HostMemory {
            total_mb: 1,
            free_mb: 1,
        },
        gpu: HostGpu {
            present: false,
            name: None,
            free_mb: None,
        },
        local_models: HostModels {
            reachable: false,
            models: Vec::new(),
        },
        policy: HostPolicy {
            allowed_window: HostWindow {
                start: "00:00".to_owned(),
                end: "00:00".to_owned(),
            },
            inside_window: true,
            max_concurrent_agents: 1,
        },
    };
    {
        let mut fake = h.fake.lock();
        fake.capacities
            .insert("host-a".to_owned(), state(false, true));
        fake.capacities
            .insert("host-b".to_owned(), state(true, true));
        fake.capacities
            .insert("host-c".to_owned(), state(true, false));
        fake.host_fail
            .insert("host-b".to_owned(), "pohunek_request_timeout".to_owned());
    }
    let pr = integration_pull_request("acme/widget", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the review on the second host", || h.publications() == 1)
        .await;
    let (_, _, started, _) = h.fake.snapshot();
    let (queries, states) = {
        let fake = h.fake.lock();
        (fake.route_queries.clone(), fake.capacity_queries.clone())
    };
    assert!(
        queries.starts_with(&["host-b".to_owned(), "host-a".to_owned()]),
        "route queries = {queries:?}"
    );
    assert!(
        !queries.iter().any(|host| host == "host-c"),
        "the host on battery is never asked: {queries:?}"
    );
    assert!(
        states.starts_with(&["host-a", "host-b", "host-c"].map(str::to_owned)),
        "state reads = {states:?}"
    );
    assert!(
        !started.is_empty() && started.iter().all(|request| request.route.host == "host-a"),
        "session hosts = {:?}",
        started
            .iter()
            .map(|request| request.route.host.as_str())
            .collect::<Vec<_>>()
    );
    h.stop_engine().await;
}

// An outage of every review host that outlasts the retry budget does not stop a due review:
// the first pass after a host answers dispatches it without `retry`, and one notice, also
// across a restart and a push during the outage, says that it waits for a review host (#241).
#[tokio::test]
async fn review_waits_through_an_outage_of_every_review_host() {
    let mut h = start_harness(|cfg| {
        cfg.pohunek_hosts = vec!["host-a".to_owned(), "host-b".to_owned()];
    })
    .await;
    for host in ["host-a", "host-b"] {
        h.fake
            .lock()
            .host_fail
            .insert(host.to_owned(), "pohunek_request_failed".to_owned());
    }
    let pr = integration_pull_request("acme/widget", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    // Each settle polls for longer than the debounce and every retry delay together.
    h.settle().await;
    h.stop_engine().await;
    let listed = h.fake.listing_count();
    h.start();
    h.wait_for("the start-up pass of the restarted engine", || {
        h.fake.listing_count() > listed
    })
    .await;
    h.settle().await;
    let pushed = PullRequest {
        head_sha: sha("c"),
        ..pr.clone()
    };
    h.fake.open(pushed, h.rule(&pr.repository));
    h.settle().await;
    h.fake.lock().host_fail.remove("host-b");
    let result = h.poll().await;
    assert_eq!(
        result.dispatched, 1,
        "the first pass after a host answers dispatches the review: {result:?}"
    );
    h.wait_for("the review on the host that answers", || {
        h.publications() == 1
    })
    .await;
    let (_, notes, started, _) = h.fake.snapshot();
    assert!(
        started.iter().all(|request| request.route.host == "host-b"),
        "session hosts = {:?}",
        started
            .iter()
            .map(|request| request.route.host.as_str())
            .collect::<Vec<_>>()
    );
    let waits: Vec<_> = notes
        .iter()
        .filter(|note| note.message.contains("wait for a review host"))
        .collect();
    assert_eq!(waits.len(), 1, "one notice of the wait: {notes:?}");
    assert_eq!(waits[0].level, Level::Action, "level of {:?}", waits[0]);
    assert!(
        waits[0].message.contains("(failed: host-a, host-b)"),
        "the notice names the failed hosts: {:?}",
        waits[0]
    );
    assert!(
        waits[0]
            .message
            .contains("Diagnostic: pohunek_request_failed."),
        "the notice names the host error code: {:?}",
        waits[0]
    );
    assert!(
        !notes
            .iter()
            .any(|note| note.message.contains("stopped after attempt")),
        "the outage stopped the review: {notes:?}"
    );
    h.stop_engine().await;
}

// A host with a stored outage is asked on a pass that no due review asks it, so its recovery
// ends the outage and its next outage is announced again (#247).
#[tokio::test]
async fn host_recovery_without_a_due_review_ends_the_announced_outage() {
    let mut h = start_harness(|cfg| {
        cfg.pohunek_hosts = vec!["host-a".to_owned()];
    })
    .await;
    h.fake
        .lock()
        .host_fail
        .insert("host-a".to_owned(), "pohunek_request_failed".to_owned());
    let fake = Arc::clone(&h.fake);
    let waits = move || {
        let (_, notes, _, _) = fake.snapshot();
        count_notes(&notes, Level::Action, "wait for a review host")
    };
    let first = integration_pull_request("acme/widget", &sha("a"));
    h.fake.open(first.clone(), h.rule(&first.repository));
    h.poll_until("the notice of the first outage", || waits() == 1)
        .await;
    h.fake.close(first);
    h.fake.lock().host_fail.remove("host-a");
    let asked = h.fake.lock().route_queries.len();
    h.poll().await;
    let queries = h.fake.lock().route_queries[asked..].to_vec();
    assert_eq!(
        queries,
        ["host-a".to_owned()],
        "the pass without a due review asks the host of the stored outage"
    );
    let state = h.store.load_discovery().expect("the discovery state loads");
    assert!(
        state.host_outages.is_empty(),
        "the answer ends the outage: {:?}",
        state.host_outages
    );
    h.fake
        .lock()
        .host_fail
        .insert("host-a".to_owned(), "pohunek_request_failed".to_owned());
    let second = integration_pull_request("acme/widget", &sha("c"));
    h.fake.open(second.clone(), h.rule(&second.repository));
    h.poll_until("the notice of the second outage", || waits() == 2)
        .await;
    h.stop_engine().await;
}

// A notification backend that always fails neither stalls discovery nor keeps it from saving:
// the first notice gets one attempt after the save, the second one of the same level none, and
// the due review is queued (#210).
#[tokio::test]
async fn failing_notification_backend_does_not_stall_discovery() {
    let mut h = start_harness(|cfg| {
        cfg.step_retry = Duration::from_secs(900);
        cfg.schedule.debounce = time::Duration::ZERO;
    })
    .await;
    h.stop_engine().await;
    let engine = Engine::new(
        h.config(),
        Arc::clone(&h.store),
        Arc::clone(&h.fake),
        FakeClock::new(),
    );
    let broken = integration_pull_request("acme/widget", &sha("e"));
    let other = integration_pull_request("acme/widget", &sha("f"));
    {
        let mut state = h.fake.lock();
        state.notify_fail = Some("matrix_unavailable".to_owned());
        state.rejected = [&broken, &other]
            .iter()
            .map(|pr| RejectedCandidate {
                repository: pr.repository.clone(),
                number: pr.number,
                code: "github_response_invalid".to_owned(),
                permission_login: None,
            })
            .collect();
    }
    let pr = integration_pull_request("acme/widget", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    let cancel = CancellationToken::new();
    let result = engine
        .poll(&cancel)
        .await
        .expect("the poll succeeds while every notification fails");
    assert_eq!(result.dispatched, 1, "poll result = {result:?}");
    assert_eq!(
        engine.clock.elapsed(),
        Duration::ZERO,
        "the poll waited for a notification retry"
    );
    assert_eq!(
        h.fake.lock().notify_attempts,
        1,
        "the second notice of the failed level was attempted"
    );
    let state = h.store.load_discovery().expect("the discovery state loads");
    for pr in [&broken, &other] {
        assert_eq!(
            state.rejected.get(&pr.key()).map(String::as_str),
            Some("github_response_invalid")
        );
    }
    engine
        .poll(&cancel)
        .await
        .expect("the second poll succeeds");
    assert_eq!(
        h.fake.lock().notify_attempts,
        1,
        "the rejected pull request was notified again"
    );
    // At the log level the notifier mutes a failure for a review, but a pass still sees it.
    let mut cfg = h.config();
    let text = INTEGRATION_POLICY.replace("review_stopped: action", "review_stopped: log");
    let policy =
        autoreview_policy::parse(
            text.as_bytes(),
            |_| Ok(b"Review the pull request.".to_vec()),
        )
        .expect("the log-level policy parses");
    cfg.policy = Some(policy);
    let logged = Engine::new(
        cfg,
        Arc::clone(&h.store),
        Arc::clone(&h.fake),
        FakeClock::new(),
    );
    let quiet: Vec<_> = (0..2)
        .map(|_| integration_pull_request("acme/widget", &sha("c")))
        .collect();
    h.fake
        .lock()
        .rejected
        .extend(quiet.iter().map(|pr| RejectedCandidate {
            repository: pr.repository.clone(),
            number: pr.number,
            code: "github_response_invalid".to_owned(),
            permission_login: None,
        }));
    logged
        .poll(&cancel)
        .await
        .expect("the log-level poll succeeds");
    assert_eq!(
        h.fake.lock().notify_attempts,
        2,
        "the second notice of the failed log level was attempted"
    );
}

// A notification backend that always fails neither delays nor fails a review: each message
// gets one attempt and is stored as handled, so a resumed review does not resend it, and the
// review publishes (#212).
#[tokio::test]
async fn failing_notification_backend_does_not_stall_a_review() {
    let mut h = start_harness(|cfg| {
        cfg.step_retry = Duration::from_secs(900);
        cfg.schedule.debounce = time::Duration::ZERO;
    })
    .await;
    h.stop_engine().await;
    // At the action level the notifier returns the failure instead of muting it.
    let text = INTEGRATION_POLICY
        .replace("started: log", "started: action")
        .replace("approved: digest", "approved: action");
    let policy =
        autoreview_policy::parse(
            text.as_bytes(),
            |_| Ok(b"Review the pull request.".to_vec()),
        )
        .expect("the action-level policy parses");
    let mut cfg = h.config();
    cfg.policy = Some(policy);
    let engine = Arc::new(Engine::new(
        cfg,
        Arc::clone(&h.store),
        Arc::clone(&h.fake),
        FakeClock::new(),
    ));
    h.fake.lock().notify_fail = Some("matrix_unavailable".to_owned());
    let pr = integration_pull_request("acme/widget", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    let cancel = CancellationToken::new();
    let serving = {
        let engine = Arc::clone(&engine);
        let cancel = cancel.clone();
        tokio::spawn(async move { engine.serve(&cancel).await })
    };
    let published_step = format!("{NOTIFY_STEP_PREFIX}published");
    h.wait_for("the publication and its notification step", || {
        h.publications() == 1 && h.step_recorded(&published_step)
    })
    .await;
    cancel.cancel();
    let served = serving.await.expect("the engine task finishes");
    assert!(served.is_ok(), "engine stopped with error: {served:?}");
    assert_eq!(
        engine.clock.elapsed(),
        Duration::ZERO,
        "the review waited for a notification retry"
    );
    assert_eq!(
        h.fake.lock().notify_attempts,
        2,
        "the started and the published notification get one attempt each"
    );
    let workflow_id = h.fake.snapshot().2[0].workflow_id.clone();
    for step in ["started", "published"] {
        let stored = h
            .store
            .load_step::<bool>(&workflow_id, &format!("{NOTIFY_STEP_PREFIX}{step}"))
            .expect("the notification step loads");
        assert_eq!(stored, Some(false), "step {step} is not stored as dropped");
    }
}

// A transient outage longer than the step retry window ends the step with its own retryable
// code instead of pausing the review.
#[tokio::test]
async fn outage_longer_than_the_step_retry_window_is_retried_later() {
    let mut h = start_harness(|cfg| cfg.step_retry = Duration::from_secs(2)).await;
    h.set_output(unfinished);
    let pr = integration_pull_request("acme/widget", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the reviewer sessions", || h.started_count() == 2)
        .await;
    h.fake.lock().transient_until = Some(Instant::now() + Duration::from_secs(6));
    h.wait_for_polling("the retry notice", || {
        h.fake.has_note(Level::Log, "Next automatic attempt")
            && h.fake.has_note(Level::Log, "github_request_failed")
    })
    .await;
    h.set_output(reviewer_output(Vec::new()));
    h.poll_until("the retried review", || h.publications() == 1)
        .await;
    h.stop_engine().await;
}

// A restart while reviewers still run resumes the review from its stored sessions: no reviewer
// starts again and the review publishes exactly once.
#[tokio::test]
async fn restart_during_collect_resumes_without_starting_reviewers_again() {
    let mut h = start_harness(|_| {}).await;
    h.set_output(unfinished);
    let pr = integration_pull_request("acme/widget", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("both reviewer sessions", || h.started_count() == 2)
        .await;
    // Stopping before the started notice is stored sends it again after the restart.
    let started_step = format!("{NOTIFY_STEP_PREFIX}started");
    h.wait_for("the started notice to be stored", || {
        h.step_recorded(&started_step)
    })
    .await;
    h.stop_engine().await;
    h.start();
    h.set_output(reviewer_output(Vec::new()));
    h.poll_until("the publication after the restart", || {
        h.publications() == 1
    })
    .await;
    let (published, notes, started, _) = h.fake.snapshot();
    assert!(
        published.len() == 1 && published[0].decision == Decision::Approve,
        "published = {published:?}"
    );
    assert_eq!(
        started.len(),
        2,
        "the restart started {} sessions in total, want 2",
        started.len()
    );
    let notices = count_notes(&notes, Level::Log, "GitHub review started");
    assert_eq!(notices, 1, "the started notice was sent {notices} times");
    h.wait_for("the completed sessions to be removed", || {
        h.fake.snapshot().3 == 0
    })
    .await;
    h.stop_engine().await;
}
