//! Core scenarios of the engine: serve, discovery, reviews and their publication.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeSet;
use std::sync::Arc;

use autoreview_guard::{FieldKind, Lexicon};
use autoreview_policy::{GuardField, Level, OnSuspect, Rule, RuleGuard};
use autoreview_schedule::scheduler::{AUTHOR_LIMIT_REACHED, HEAD_FETCH_MISMATCH};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::json;

use crate::config::{Config, Credential, Credentials, RouteOverride, TextGuard};
use crate::engine::STEP_ARTIFACT;
use crate::integrations::{GuardText, SessionRequest};
use crate::messages::APPROVAL_REQUESTED_TITLE;
use crate::model::review::Decision;
use crate::model::{GuardOutcome, PullRequest};
use crate::protocol::claude::CLAUDE_RESULT_PROTOCOL;
use crate::protocol::fixtures::sha;
use crate::review::input::STEP_GUARD;
use crate::store::ReviewFilter;
use crate::testing::fake::{count_notes, reviewer_output};
use crate::testing::harness::{Harness, integration_pull_request, start_harness};

/// Serves `acme/*` with the credential `acme` and every other repository with `rest`.
fn two_credentials(config: &mut Config) {
    let credential = |name: &str, login: &str, pattern: &str| Credential {
        name: name.to_owned(),
        login: login.to_owned(),
        token: format!("{name}-token"),
        matches: vec![pattern.to_owned()],
    };
    let list = vec![
        credential("acme", "acme-bot", "acme/*"),
        credential("rest", "reviewer", "*"),
    ];
    config.credentials = Credentials::new(list).expect("the credentials load");
}

/// Makes the searches of `name` fail with `code` and its reads fail; `None` fails none.
fn fail_credential(h: &Harness, failing: Option<(&str, &str)>) {
    let mut state = h.fake.lock();
    state.failing_credentials.clear();
    if let Some((name, code)) = failing {
        state
            .failing_credentials
            .insert(name.to_owned(), code.to_owned());
    }
}

#[tokio::test]
async fn clean_review_of_an_approving_rule_is_approved_once_without_the_user() {
    let mut h = start_harness(two_credentials).await;
    // A truncated search fails only its credential; the other's pull request is reviewed.
    fail_credential(&h, Some(("rest", "github_search_limit_exceeded")));
    let pr = integration_pull_request("acme/widget", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    let other = integration_pull_request("risky/tool", &sha("b"));
    h.fake.open(other.clone(), h.rule(&other.repository));
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

/// Adds `isolated-host` and pins `public/widget` and `public/gadget` to `example-host`; their
/// untrusted authors go to `isolated-host` and to `absent-host`, which is not configured.
fn untrusted_routes(config: &mut Config) {
    config.pohunek_hosts = vec!["example-host".to_owned(), "isolated-host".to_owned()];
    let pin = |host: &str| RouteOverride {
        host: host.to_owned(),
        project_id: "project".to_owned(),
    };
    for (repository, untrusted) in [
        ("public/widget", "isolated-host"),
        ("public/gadget", "absent-host"),
    ] {
        (config.route_overrides).insert(repository.to_owned(), pin("example-host"));
        (config.untrusted_route_overrides).insert(repository.to_owned(), pin(untrusted));
    }
}

/// The author class of the discovery pass picks the route (#400): a trusted author's review
/// runs on the repository's route, an untrusted author's only on its untrusted route, and an
/// untrusted route that no host serves fails before any session starts, also on a retry.
#[tokio::test]
async fn untrusted_author_reviews_run_only_on_the_untrusted_route() {
    let mut h = start_harness(untrusted_routes).await;
    let user = |repository: &str, head: &str, login: &str| {
        let mut pr = integration_pull_request(repository, &sha(head));
        login.clone_into(&mut pr.author);
        pr.author_type = "User".to_owned();
        pr
    };
    let maintainer = user("public/widget", "a", "maintainer");
    let stranger = user("public/widget", "b", "stranger");
    let unrouted = user("public/gadget", "c", "stranger");
    for pr in [&maintainer, &stranger, &unrouted] {
        h.fake.open(pr.clone(), h.rule(&pr.repository));
    }
    let record = |pr: &PullRequest| {
        let state = h.store.load_discovery().expect("the discovery state loads");
        state.schedule.get(&pr.key()).cloned().unwrap_or_default()
    };
    // A review that ran on a wrong host completes, so the wait ends and the check below fails.
    h.poll_until("both reviews and the end of the third", || {
        let phase = record(&unrouted).phase;
        h.publications() >= 2 && (phase == "failed" || phase == "completed")
    })
    .await;
    let retried = h.retry(&unrouted).await.map(|result| result.workflow_id);
    h.settle().await;
    let (_, _, started, _) = h.fake.snapshot();
    let sessions: Vec<(&str, &str)> = (started.iter())
        .filter(|request| request.pull_request.number != unrouted.number)
        .map(|request| {
            (
                request.pull_request.author.as_str(),
                request.route.host.as_str(),
            )
        })
        .collect();
    let host = |author: &str| match author {
        "maintainer" => "example-host",
        _ => "isolated-host",
    };
    let failed = record(&unrouted);
    assert!(
        sessions.len() == started.len()
            && sessions.iter().any(|(author, _)| *author == "maintainer")
            && sessions.iter().any(|(author, _)| *author == "stranger")
            && sessions.iter().all(|(author, used)| *used == host(author))
            && failed.phase == "failed"
            && failed.error_code == "project_route_not_found"
            && failed.workflow_id.is_empty()
            && retried
                .as_ref()
                .is_err_and(|err| err.to_string() == "review_retry_not_dispatched"),
        "sessions={sessions:?} started={} failed={failed:?} retried={retried:?}",
        started.len()
    );
    h.stop_engine().await;
}

/// Returns the Claude adapter's refusal of a base checkout whose head is not in the repository.
fn head_unavailable_output() -> String {
    let stderr =
        "claude-adapter: trusted pull request head is unavailable in the review checkout\n";
    let refusal = json!({
        "protocol": CLAUDE_RESULT_PROTOCOL,
        "mode": "structured",
        "exit_code": 64,
        "stdout_base64": "",
        "stderr_base64": STANDARD.encode(stderr),
        "refusal": "head_unavailable",
    });
    format!("{refusal}\n")
}

/// Returns the failure codes of the reviews of `pr`, oldest first; a success is empty.
fn review_failures(h: &Harness, pr: &PullRequest) -> Vec<String> {
    let filter = ReviewFilter::ReviewKey(pr.key());
    let reviews = h.store.list_reviews(&filter, 50).expect("the reviews read");
    reviews
        .into_iter()
        .rev()
        .filter(|review| review.status == "completed")
        .map(|review| review.failure)
        .collect()
}

/// A base-checkout session fetches the pull request head only where the host advertises it
/// (#255). Without the capability, or when the host does not answer the capability read, the
/// session starts as before and a missing head stops the review; with it, a fetched head of
/// another commit re-plans the review and a failed fetch is retried. A head whose fetched ref
/// stays at another commit stops after the retry budget with one notice that names both
/// commits, also across a restart (#416).
#[tokio::test]
async fn base_checkout_session_fetches_the_head_where_the_host_supports_it() {
    let mut h = start_harness(|_| {}).await;
    // The head is in the review host's repository only when the session start fetched it.
    let clean = reviewer_output(Vec::new());
    h.set_output(move |request: &SessionRequest| {
        if request.fetch_head {
            clean(request)
        } else {
            head_unavailable_output()
        }
    });
    let missing = integration_pull_request("claude/base", &sha("c"));
    h.fake.open(missing.clone(), h.rule(&missing.repository));
    h.poll_until("the stopped review", || {
        review_failures(&h, &missing) == ["review_head_unavailable"]
    })
    .await;
    h.settle().await;
    let (_, _, started, _) = h.fake.snapshot();
    assert!(
        h.publications() == 0
            && review_failures(&h, &missing) == ["review_head_unavailable"]
            && started.len() == 1
            && !started[0].fetch_head,
        "without the capability: started={started:?}"
    );
    let host = started[0].route.host.clone();
    {
        let mut state = h.fake.lock();
        state.fetch_ref_hosts.insert(host);
        // The next capability read finds the host unavailable.
        state.fetch_ref_failures = 1;
    }
    let unknown = integration_pull_request("claude/base", &sha("e"));
    h.fake.open(unknown.clone(), h.rule(&unknown.repository));
    h.poll_until("the review started without the fetch", || {
        review_failures(&h, &unknown) == ["review_head_unavailable"]
    })
    .await;
    let (_, _, started, _) = h.fake.snapshot();
    assert!(
        h.publications() == 0 && started.len() == 2 && !started[1].fetch_head,
        "an unanswered capability read: started={started:?}"
    );
    {
        let mut state = h.fake.lock();
        state
            .start_failures
            .push_back((HEAD_FETCH_MISMATCH.to_owned(), true));
        state
            .start_failures
            .push_back(("pohunek_head_fetch_failed".to_owned(), false));
    }
    let fetched = integration_pull_request("claude/base", &sha("d"));
    h.fake.open(fetched.clone(), h.rule(&fetched.repository));
    h.poll_until("the approval of the fetched head", || h.publications() == 1)
        .await;
    h.settle().await;
    let (published, _, started, _) = h.fake.snapshot();
    let heads = h.fake.lock().published_heads.clone();
    assert!(
        published.len() == 1
            && published[0].decision == Decision::Approve
            && heads == [sha("d")]
            && review_failures(&h, &fetched) == [HEAD_FETCH_MISMATCH, ""]
            && review_failures(&h, &missing) == ["review_head_unavailable"]
            && review_failures(&h, &unknown) == ["review_head_unavailable"]
            && started.len() == 3
            && started[2].fetch_head
            && started[2].start == 2
            && started[2].pull_request.number == fetched.number,
        "with the capability: failures={:?} started={started:?}",
        review_failures(&h, &fetched)
    );
    // Every start of this head fetches another commit; the third failure is used by a retry.
    let lagging = sha("f");
    {
        let mut state = h.fake.lock();
        state.fetched_head.clone_from(&lagging);
        for _ in 0..3 {
            let failure = (HEAD_FETCH_MISMATCH.to_owned(), true);
            state.start_failures.push_back(failure);
        }
    }
    let stuck = integration_pull_request("claude/base", &sha("9"));
    let stopped = format!("Review for {} at", stuck.key());
    let fake = Arc::clone(&h.fake);
    let notes = move || count_notes(&fake.lock().notes, Level::Action, &stopped);
    h.fake.open(stuck.clone(), h.rule(&stuck.repository));
    h.poll_until("the stop of the lagging head", || notes() == 1)
        .await;
    h.settle().await;
    h.stop_engine().await;
    h.start();
    h.settle().await;
    h.settle().await;
    let note = (h.fake.lock().notes.iter())
        .find(|note| note.message.contains(&stuck.key()))
        .map(|note| note.message.clone())
        .unwrap_or_default();
    let expected = format!("Expected commit: {}. Fetched commit: {lagging}.", sha("9"));
    assert!(
        notes() == 1
            && note.contains(&expected)
            && note.contains(HEAD_FETCH_MISMATCH)
            && review_failures(&h, &stuck) == [HEAD_FETCH_MISMATCH, HEAD_FETCH_MISMATCH]
            && h.fake.lock().start_failures.len() == 1
            && h.publications() == 1,
        "the lagging head: failures={:?} note={note}",
        review_failures(&h, &stuck)
    );
    // A retry starts the stopped head once; a ref still at the other commit stops it at once.
    h.retry(&stuck)
        .await
        .expect("the retry of the stopped head");
    h.poll_until("the second stop of the lagging head", || notes() == 2)
        .await;
    assert!(
        review_failures(&h, &stuck) == [HEAD_FETCH_MISMATCH; 3]
            && h.fake.lock().start_failures.is_empty(),
        "the retried head: failures={:?}",
        review_failures(&h, &stuck)
    );
    h.stop_engine().await;
}

/// Returns the phase and error code of the schedule record of `pr`.
fn schedule_state(h: &Harness, pr: &PullRequest) -> Option<(String, String)> {
    let state = h.store.load_discovery().expect("the discovery state reads");
    let record = state.schedule.get(&pr.key())?;
    Some((record.phase.clone(), record.error_code.clone()))
}

/// A rule's author limit counts an author's starts from the store, across a restart, and from
/// the same discovery pass, by login in any case. Over the bound a review waits, or on a
/// skipping rule stops with one notice for the pull request until `retry` starts it (#401).
#[tokio::test]
async fn author_limit_counts_every_start_of_an_author_once() {
    let mut h = start_harness(|_| {}).await;
    let first = integration_pull_request("limit/one", &sha("a"));
    h.fake.open(first.clone(), h.rule(&first.repository));
    h.poll_until("the first review", || h.publications() == 1)
        .await;
    // The stored start counts after a restart; two more come due in one pass, and the second
    // pull request of the pass (by key) is the one over the bound.
    h.stop_engine().await;
    h.start();
    let second = integration_pull_request("limit/one", &sha("b"));
    let third = PullRequest {
        author: "Colleague".to_owned(),
        ..integration_pull_request("limit/two", &sha("c"))
    };
    let other = PullRequest {
        author: "maintainer".to_owned(),
        ..integration_pull_request("limit/one", &sha("d"))
    };
    for pr in [&second, &third, &other] {
        h.fake.open(pr.clone(), h.rule(&pr.repository));
    }
    h.poll_until("the reviews under the limit", || h.publications() == 3)
        .await;
    h.settle().await;
    let waiting = Some(("waiting".to_owned(), String::new()));
    assert!(
        h.publications() == 3 && schedule_state(&h, &third) == waiting,
        "the start over the stored one and the one of the pass must wait: {:?}",
        schedule_state(&h, &third)
    );
    // A skipping rule stops a start over the bound, once per pull request, until a retry.
    let allowed = integration_pull_request("limitskip/one", &sha("e"));
    h.fake.open(allowed.clone(), h.rule(&allowed.repository));
    h.poll_until("the review under the skip limit", || h.publications() == 4)
        .await;
    let skipped = integration_pull_request("limitskip/one", &sha("f"));
    h.fake.open(skipped.clone(), h.rule(&skipped.repository));
    let stopped = Some(("failed".to_owned(), AUTHOR_LIMIT_REACHED.to_owned()));
    h.poll_until("the skip", || schedule_state(&h, &skipped) == stopped)
        .await;
    let pushed = PullRequest {
        head_sha: sha("9"),
        ..skipped.clone()
    };
    h.fake.open(pushed.clone(), h.rule(&pushed.repository));
    h.settle().await;
    let notice = "Diagnostic: author_limit_reached.";
    let (_, notes, _, _) = h.fake.snapshot();
    assert!(
        h.publications() == 4
            && schedule_state(&h, &pushed) == stopped
            && count_notes(&notes, Level::Action, notice) == 1,
        "the skips of one pull request must notify once: {notes:?}"
    );
    h.retry(&pushed).await.expect("the retry is accepted");
    h.poll_until("the retried review", || h.publications() == 5)
        .await;
    h.stop_engine().await;
}

/// Returns the rule of `repository` with a guard on the body and the outcome `on_suspect`.
fn guarded_rule(h: &Harness, repository: &str, on_suspect: OnSuspect) -> Rule {
    let mut rule = h.rule(repository);
    rule.guard = Some(RuleGuard {
        fields: BTreeSet::from([GuardField::Kind(FieldKind::Body)]),
        threshold: 100,
        on_suspect,
    });
    rule
}

/// A suspect pull request on an approving rule waits for the user and publishes nothing before
/// the approval; under `log` it is approved as without a guard. The guard message carries
/// signal codes only, never the text (#345).
#[tokio::test]
async fn text_guard_holds_a_suspect_approval_and_log_changes_nothing() {
    let mut h = start_harness(|config| {
        let lexicon = Lexicon::parse("40 ignore previous instructions");
        config.text_guard = Some(TextGuard {
            max_input_bytes: 4096,
            lexicon: lexicon.expect("the lexicon parses"),
        });
    })
    .await;
    // A run of Unicode tag characters spells hidden text that the page does not show.
    let planted = "Fix the build\u{E0041}\u{E0050}\u{E0050}\u{E0052}";
    h.fake.lock().guard_texts = vec![GuardText {
        kind: FieldKind::Body,
        text: planted.to_owned(),
    }];
    let held = integration_pull_request("acme/widget", &sha("c"));
    h.fake.open(
        held.clone(),
        guarded_rule(&h, &held.repository, OnSuspect::Await),
    );
    h.poll_until("the approval request", || {
        h.fake.has_note(Level::Action, APPROVAL_REQUESTED_TITLE)
    })
    .await;
    let (_, notes, _, _) = h.fake.snapshot();
    let flagged: Vec<&str> = (notes.iter())
        .filter(|note| note.level == Level::Action && note.message.contains("Text guard flagged"))
        .map(|note| note.message.as_str())
        .collect();
    assert!(
        h.publications() == 0
            && flagged.len() == 1
            && flagged[0].contains("Signals: body unicode_tags x1")
            && !flagged[0].contains("Fix the build")
            && !flagged[0].contains('\u{E0041}')
            && h.fake
                .has_note(Level::Action, "the text guard flagged the pull request"),
        "a held review published or its guard message is wrong: {flagged:?}"
    );
    h.wait_for("the approval to be accepted", || {
        let Ok(resolved) = h.resolve(&held) else {
            return false;
        };
        h.approve(&resolved.workflow_id, &held).is_ok()
    })
    .await;
    h.wait_for("the approved publication", || h.publications() == 1)
        .await;
    let logged = integration_pull_request("acme/widget", &sha("d"));
    h.fake.open(
        logged.clone(),
        guarded_rule(&h, &logged.repository, OnSuspect::Log),
    );
    h.poll_until("the logged approval", || h.publications() == 2)
        .await;
    h.wait_for("the logged guard message", || {
        h.fake
            .has_note(Level::Action, "Outcome: log, nothing changes")
    })
    .await;
    let (published, _, _, _) = h.fake.snapshot();
    let workflow_id = logged.workflow_id(0);
    let artifact = (h.store)
        .load_step::<serde_json::Value>(&workflow_id, STEP_ARTIFACT)
        .expect("the artifact reads")
        .expect("the artifact is stored");
    let step = (h.store)
        .load_step::<GuardOutcome>(&workflow_id, STEP_GUARD)
        .expect("the guard step reads");
    assert!(
        published[0].decision == Decision::Approve
            && published[0]
                .body
                .contains("The text guard flagged this pull request: `unicode_tags`.")
            && published[1].decision == Decision::Approve
            && !published[1].body.contains("text guard")
            && artifact.get("guard").is_none()
            && step.is_some_and(|step| step.suspect)
            && h.fake.lock().guard_reads == 2,
        "held={:?} logged={:?} artifact={artifact}",
        published[0].body,
        published[1].body
    );
    h.stop_engine().await;
}
