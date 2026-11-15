//! Core scenarios of the engine: serve, discovery, reviews and their publication.

// Rust guideline compliant 2026-06-26

use std::sync::Arc;

use autoreview_policy::Level;
use autoreview_schedule::scheduler::HEAD_FETCH_MISMATCH;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::json;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

use crate::config::{Config, Credential, Credentials};
use crate::integrations::{SessionRequest, TokenAnswer};
use crate::model::review::Decision;
use crate::model::{PullRequest, RejectedCandidate};
use crate::protocol::claude::CLAUDE_RESULT_PROTOCOL;
use crate::protocol::fixtures::sha;
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
    // `rest`'s search also finds `acme`'s pull requests; only `acme` may review them.
    // A refusal of `acme`'s search is announced once, also across a failed search.
    let refused = RejectedCandidate {
        repository: "acme/broken".to_owned(),
        number: 7,
        code: "github_response_invalid".to_owned(),
        permission_login: None,
    };
    h.fake.lock().rejected.push(refused);
    h.poll().await;
    fail_credential(&h, Some(("acme", "github_request_failed")));
    let mut state = h.store.load_discovery().expect("the discovery state loads");
    state
        .skipped
        .insert("acme/hidden#8".to_owned(), "untrusted_author".to_owned());
    (h.store
        .in_tx(|transaction| h.store.save_discovery(transaction, &state)))
    .expect("the discovery state saves");
    let next = integration_pull_request("acme/gadget", &sha("c"));
    h.fake.open(next.clone(), h.rule(&next.repository));
    h.poll_until("the review of rest's pull request", || {
        h.publications() == 2
    })
    .await;
    // Only a search that GitHub refuses marks its credential for a token check (#19).
    let checks = h.fake.lock().token_checks.len();
    h.engine.recheck_tokens(&CancellationToken::new()).await;
    assert_eq!(
        h.fake.lock().token_checks.len(),
        checks,
        "a failed search checked its token"
    );
    // A retry names the failed search, not a withdrawn review request.
    match h.retry(&next).await {
        Err(err) => assert_eq!(
            err.to_string(),
            "github_request_failed: the search of credential acme failed"
        ),
        Ok(result) => panic!("a retry of an unsearched pull request ran: {result:?}"),
    }
    h.settle().await;
    let state = h.store.load_discovery().expect("the discovery state loads");
    assert!(
        h.fake.lock().published_heads == [sha("a"), sha("b")]
            && state.schedule.contains_key(&pr.key())
            && state.skipped.contains_key("acme/hidden#8")
            && !state.schedule.contains_key(&next.key()),
        "an unsearched credential's pull request was reviewed or its record dropped"
    );
    fail_credential(&h, None);
    h.poll_until("the review once acme's search works", || {
        h.publications() == 3
    })
    .await;
    h.settle().await;
    let refusals = count_notes(&h.fake.lock().notes, Level::Action, "acme/broken#7");
    assert!(
        h.fake.lock().published_heads == [sha("a"), sha("b"), sha("c")] && refusals == 1,
        "a head was reviewed again, or a known refusal was announced again"
    );
    // A transient failure costs its credential one search per pass, so it delays no other.
    h.fake
        .lock()
        .transient_credentials
        .insert("acme".to_owned());
    let before = h.fake.lock().listings;
    h.poll().await;
    h.fake.lock().transient_credentials.clear();
    assert_eq!(
        h.fake.lock().listings,
        before + 2,
        "a transiently failing credential was searched again in the pass"
    );
    // `acme-bot` wrote both pull requests; only the one its own credential serves is its own.
    for (repository, head) in [("acme/own", sha("d")), ("risky/own", sha("e"))] {
        let mut own = integration_pull_request(repository, &head);
        own.author = "acme-bot".to_owned();
        h.fake.open(own.clone(), h.rule(&own.repository));
    }
    h.poll_until("the reviews of acme-bot's pull requests", || {
        h.publications() == 5
    })
    .await;
    {
        let state = h.fake.lock();
        let decision = |head: &str| {
            let index = state.published_heads.iter().position(|seen| seen == head);
            index.map(|index| state.published[index].decision.clone())
        };
        let signature = &h.engine.config.ai_signature;
        assert!(
            decision(&sha("d")) == Some(Decision::Comment)
                && decision(&sha("e")) == Some(Decision::Approve)
                && state
                    .published
                    .iter()
                    .all(|review| review.body.ends_with(signature)),
            "own pull requests follow the wrong login: {:?}",
            state.published
        );
    }
    // A token that expires within the window warns once a UTC day per credential, not at
    // every check (#19).
    let expires_at = Some(OffsetDateTime::now_utc() + time::Duration::days(3));
    for (token, login) in [("acme-token", "acme-bot"), ("rest-token", "reviewer")] {
        let accepted = TokenAnswer::Accepted {
            login: login.to_owned(),
            expires_at,
        };
        (h.fake.lock().token_answers).insert(token.to_owned(), accepted);
    }
    for _ in 0..2 {
        h.engine.check_tokens(&CancellationToken::new()).await;
    }
    for name in ["acme", "rest"] {
        let expiring = format!("The GitHub token of credential {name} expires at");
        assert_eq!(
            count_notes(&h.fake.lock().notes, Level::Action, &expiring),
            1,
            "credential {name} was not warned exactly once a day"
        );
    }
    // A refused token suspends only its credential (#19, D1): `credential_invalid` is raised
    // once, `acme`'s pull requests wait unsearched, and `rest` still reviews.
    let cancel = CancellationToken::new();
    fail_credential(&h, Some(("acme", "credential_invalid")));
    (h.fake.lock().token_answers).insert("acme-token".to_owned(), TokenAnswer::Rejected);
    h.poll().await;
    h.engine.recheck_tokens(&cancel).await;
    let refused = "GitHub refuses the token of credential acme";
    let fake = Arc::clone(&h.fake);
    let notes = move || count_notes(&fake.lock().notes, Level::Action, refused);
    assert_eq!(notes(), 1, "the refused search did not check the token");
    let held = integration_pull_request("acme/held", &sha("f"));
    h.fake.open(held.clone(), h.rule(&held.repository));
    let other = integration_pull_request("risky/after", &sha("9"));
    h.fake.open(other.clone(), h.rule(&other.repository));
    h.poll_until("rest's review while acme is suspended", || {
        h.publications() == 6
    })
    .await;
    h.settle().await;
    let before = h.fake.lock().listings;
    h.poll().await;
    h.engine.recheck_tokens(&cancel).await;
    assert!(
        notes() == 1
            && h.fake.lock().listings == before + 1
            && !h.fake.lock().published_heads.contains(&sha("f")),
        "a suspended credential was searched or reviewed, or announced more than once"
    );
    // The check after the next poll accepts the token and lifts the suspension, which raises
    // `credential_restored` once (#378).
    fail_credential(&h, None);
    let accepted = TokenAnswer::Accepted {
        login: "acme-bot".to_owned(),
        expires_at: None,
    };
    (h.fake.lock().token_answers).insert("acme-token".to_owned(), accepted);
    h.engine.recheck_tokens(&cancel).await;
    h.poll_until("acme's review after the lift", || h.publications() == 7)
        .await;
    let restored = "GitHub accepts the token of credential acme again";
    let fake = Arc::clone(&h.fake);
    let lifts = move || count_notes(&fake.lock().notes, Level::Action, restored);
    assert_eq!(lifts(), 1, "a lift was not announced exactly once");
    // A review queued before a start that refuses its token waits until the lift.
    h.stop_engine().await;
    let queued = integration_pull_request("acme/queued", &sha("8"));
    h.fake.open(queued.clone(), h.rule(&queued.repository));
    h.settle().await;
    let waiting = h.store.ready_reviews().expect("the queue reads");
    assert_eq!(waiting.len(), 1, "the review of acme/queued was not queued");
    (h.fake.lock().token_answers).insert("acme-token".to_owned(), TokenAnswer::Rejected);
    h.start();
    h.wait_for("the start check of the new engine", || notes() == 2)
        .await;
    h.settle().await;
    assert!(
        h.publications() == 7 && h.store.ready_reviews().is_ok_and(|ready| ready.len() == 1),
        "the queued review of a suspended credential started"
    );
    let accepted = TokenAnswer::Accepted {
        login: "acme-bot".to_owned(),
        expires_at: None,
    };
    (h.fake.lock().token_answers).insert("acme-token".to_owned(), accepted);
    h.engine.recheck_tokens(&cancel).await;
    h.wait_for("the queued review after the lift", || h.publications() == 8)
        .await;
    assert_eq!(lifts(), 2, "the lift after the restart was not announced");
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
