//! Delivery of review and discovery messages to routes with several backends (#30).

// Rust guideline compliant 2026-11-14

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Semaphore;

use crate::config::Config;
use crate::integrations::SessionRequest;
use crate::messages::APPROVAL_REQUESTED_TITLE;
use crate::model::PullRequest;
use crate::protocol::fixtures::sha;
use crate::testing::fake::{INTEGRATION_POLICY, reviewer_output};
use crate::testing::harness::{Harness, integration_pull_request, start_harness};

/// Name of the backend that the routes of these scenarios add next to `matrix`.
const EXTRA: &str = "extra";

/// Code that no notifier drops, so a failing `extra` fails at every level.
const REFUSED: &str = "notify_backend_unknown";

/// Prompt of every reviewer of the integration policy.
const PROMPT: &[u8] = b"Review the pull request.";

/// Part of the notice of a review that stopped and gets no further automatic attempt.
const STOPPED_NOTE: &str = "No further automatic attempt is scheduled";

/// Part of the notice of a review that waits for a review host.
const HOST_WAIT_NOTE: &str = "wait for a review host";

/// Start of the `published` notification of a clean review of `claude/base`.
const APPROVED_NOTE: &str = "Approved claude/base PR";

/// Start of the `started` notification of every review.
const STARTED_NOTE: &str = "GitHub review started";

/// Routes every level to `first`, then `second`; `extra` is a copy of `matrix`.
fn route_levels(cfg: &mut Config, first: &str, second: &str) {
    let mut extra = cfg.notify.backends[0].clone();
    extra.name = EXTRA.to_owned();
    cfg.notify.backends.push(extra);
    let both = vec![first.to_owned(), second.to_owned()];
    cfg.notify.action = both.clone();
    cfg.notify.digest = both.clone();
    cfg.notify.log = both;
}

/// Starts a harness whose levels route to `extra`, which refuses everything, then `matrix`.
async fn failing_extra_first(configure: impl Fn(&mut Config) + 'static) -> Harness {
    let h = start_harness(move |cfg| {
        configure(cfg);
        route_levels(cfg, EXTRA, "matrix");
    })
    .await;
    let refused = (EXTRA.to_owned(), REFUSED.to_owned());
    h.fake.lock().backend_fail.extend([refused]);
    h
}

/// Counts the notes on `backend` whose text contains `fragment`.
fn sent(h: &Harness, backend: &str, fragment: &str) -> usize {
    let (_, notes, _, _) = h.fake.snapshot();
    let matching = notes
        .iter()
        .filter(|note| note.backend == backend && note.message.contains(fragment));
    matching.count()
}

/// Returns the stored value of the step `name` of the first review, `None` before it ran.
fn step(h: &Harness, name: &str) -> Option<bool> {
    let (_, _, started, _) = h.fake.snapshot();
    let workflow_id = &started.first()?.workflow_id;
    (h.store.load_step::<bool>(workflow_id, name)).expect("the step reads")
}

/// Stops the engine while a held note waits on `gate`, so the send ends by shutdown.
async fn stop_while_held(h: &mut Harness, gate: Arc<Semaphore>) {
    tokio::join!(biased; h.stop_engine(), async { gate.add_permits(1) });
}

/// Opens `pr` with the rule of its repository.
fn open(h: &Harness, pr: &PullRequest) {
    h.fake.open(pr.clone(), h.rule(&pr.repository));
}

/// Makes `approved` an action event, so the publication notice is an action message.
fn approved_as_action(cfg: &mut Config) {
    let document = INTEGRATION_POLICY.replacen("  approved: digest\n", "  approved: action\n", 1);
    let parsed = autoreview_policy::parse(document.as_bytes(), |_| Ok(PROMPT.to_vec()));
    cfg.policy = Some(parsed.expect("the changed integration policy parses"));
}

// An action message held on a backend while the engine stops, twice, reaches each backend of a
// two-backend route once. On a one-backend route it is sent again after each restart and no
// backend step is written, as before #30.
#[tokio::test]
async fn an_action_message_reaches_each_backend_once_across_two_restarts() {
    for several in [false, true] {
        let mut h = start_harness(move |cfg| {
            approved_as_action(cfg);
            if several {
                route_levels(cfg, "matrix", EXTRA);
            }
        })
        .await;
        let gate = h.fake.hold_notes(APPROVED_NOTE);
        let pr = integration_pull_request("claude/base", &sha("a"));
        open(&h, &pr);
        h.poll_until("the held notice", || sent(&h, "matrix", APPROVED_NOTE) == 1)
            .await;
        stop_while_held(&mut h, gate).await;
        // The first restart stops again while the notice is held on the next backend.
        let gate = h.fake.hold_notes(APPROVED_NOTE);
        h.start();
        let (next, count) = if several { (EXTRA, 1) } else { ("matrix", 2) };
        h.poll_until("the notice held after the restart", || {
            sent(&h, next, APPROVED_NOTE) == count
        })
        .await;
        stop_while_held(&mut h, gate).await;
        h.fake.lock().held_notes = None;
        h.start();
        h.poll_until("the stored notice", || {
            step(&h, "notify/published").is_some()
        })
        .await;
        h.stop_engine().await;
        let expected = if several { [1, 1] } else { [3, 0] };
        let counts = [
            sent(&h, "matrix", APPROVED_NOTE),
            sent(&h, EXTRA, APPROVED_NOTE),
        ];
        assert_eq!(counts, expected, "notices with several = {several}");
        let steps = [
            step(&h, "notify/published"),
            step(&h, "notify/published@matrix"),
            step(&h, "notify/published@extra"),
        ];
        let expected = if several {
            [Some(true), Some(true), Some(true)]
        } else {
            [Some(true), None, None]
        };
        assert_eq!(steps, expected, "steps with several = {several}");
    }
}

// A route that shrinks between a shutdown and the resume still skips the backend that took
// the action message: `matrix` accepts it, the engine stops while `extra` holds it, and the
// restarted engine routes the level to `matrix` alone (review of PR 388).
#[tokio::test]
async fn a_route_that_shrinks_across_a_restart_sends_no_action_message_twice() {
    let shrunk = Arc::new(AtomicBool::new(false));
    let switch = Arc::clone(&shrunk);
    let mut h = start_harness(move |cfg| {
        approved_as_action(cfg);
        route_levels(cfg, "matrix", EXTRA);
        if switch.load(Ordering::SeqCst) {
            cfg.notify.action = vec!["matrix".to_owned()];
        }
    })
    .await;
    let first = h.fake.hold_notes(APPROVED_NOTE);
    let pr = integration_pull_request("claude/base", &sha("a"));
    open(&h, &pr);
    h.poll_until("the notice held on matrix", || {
        sent(&h, "matrix", APPROVED_NOTE) == 1
    })
    .await;
    // The next hold catches the notice on `extra` once `matrix` has returned.
    let second = h.fake.hold_notes(APPROVED_NOTE);
    first.add_permits(1);
    h.wait_for("the notice held on extra", || {
        sent(&h, EXTRA, APPROVED_NOTE) == 1
    })
    .await;
    stop_while_held(&mut h, second).await;
    h.fake.lock().held_notes = None;
    shrunk.store(true, Ordering::SeqCst);
    h.start();
    h.poll_until("the stored notice", || {
        step(&h, "notify/published").is_some()
    })
    .await;
    h.stop_engine().await;
    let counts = [
        sent(&h, "matrix", APPROVED_NOTE),
        sent(&h, EXTRA, APPROVED_NOTE),
    ];
    assert_eq!(counts, [1, 1], "notices per backend");
    assert_eq!(step(&h, "notify/published@matrix"), Some(true));
}

// A backend that refuses a message leaves it stored as dropped, and the backend gets the next
// one when it works again. A digest message that a backend answered during a shutdown is sent
// to it again, as on a one-backend route, because a dropped failure looks like an acceptance.
#[tokio::test]
async fn a_refused_message_stays_dropped_and_a_held_digest_is_sent_again() {
    let mut h = start_harness(|cfg| route_levels(cfg, "matrix", EXTRA)).await;
    let refused = (EXTRA.to_owned(), REFUSED.to_owned());
    h.fake.lock().backend_fail.extend([refused]);
    let gate = h.fake.hold_notes(APPROVED_NOTE);
    let pr = integration_pull_request("claude/base", &sha("a"));
    open(&h, &pr);
    h.poll_until("the held published notification", || {
        sent(&h, "matrix", APPROVED_NOTE) == 1
    })
    .await;
    stop_while_held(&mut h, gate).await;
    {
        let mut state = h.fake.lock();
        state.held_notes = None;
        state.backend_fail.clear();
    }
    h.start();
    h.poll_until("the stored published notification", || {
        step(&h, "notify/published").is_some()
    })
    .await;
    h.stop_engine().await;
    let counts = [
        sent(&h, "matrix", APPROVED_NOTE),
        sent(&h, EXTRA, APPROVED_NOTE),
        sent(&h, "matrix", STARTED_NOTE),
        sent(&h, EXTRA, STARTED_NOTE),
    ];
    assert_eq!(counts, [2, 1, 1, 0], "published and started per backend");
    let steps = [
        ("notify/started", Some(false)),
        ("notify/started@matrix", Some(true)),
        ("notify/started@extra", None),
        ("notify/published", Some(true)),
        ("notify/published@matrix", Some(true)),
        ("notify/published@extra", Some(true)),
    ];
    for (name, expected) in steps {
        assert_eq!(step(&h, name), expected, "{name}");
    }
}

// A review that stops after its retries reaches the working backend behind a failing one, and
// so does its successful retry by hand.
#[tokio::test]
async fn a_stopped_review_and_its_retry_reach_the_working_backend() {
    let mut h = failing_extra_first(|_| {}).await;
    h.set_output(|_: &SessionRequest| "{\"protocol\":\"broken\"}\n".to_owned());
    let pr = integration_pull_request("claude/base", &sha("a"));
    open(&h, &pr);
    h.poll_until("the stop", || sent(&h, "matrix", STOPPED_NOTE) == 1)
        .await;
    h.set_output(reviewer_output(Vec::new()));
    h.retry(&pr).await.expect("the stopped review is retried");
    h.poll_until("the retried review", || h.publications() == 1)
        .await;
    h.settle().await;
    let counts = [
        sent(&h, "matrix", STOPPED_NOTE),
        sent(&h, "matrix", APPROVED_NOTE),
        sent(&h, EXTRA, ""),
    ];
    assert_eq!(
        counts,
        [1, 1, 0],
        "stop, approval, notes on the failing backend"
    );
    h.stop_engine().await;
}

// A review that stops at its route, before any run exists, is announced once on the working
// backend behind a failing one, whatever the route code.
#[tokio::test]
async fn a_review_stopped_at_its_route_reaches_the_working_backend() {
    for code in ["project_route_not_found", "project_route_ambiguous"] {
        let mut h = failing_extra_first(|_| {}).await;
        let failing = ("example-host".to_owned(), code.to_owned());
        h.fake.lock().host_fail.extend([failing]);
        let pr = integration_pull_request("claude/base", &sha("a"));
        open(&h, &pr);
        h.poll_until("the stop at the route", || {
            sent(&h, "matrix", STOPPED_NOTE) == 1
        })
        .await;
        h.settle().await;
        let runs = h.started_count();
        assert_eq!(
            (sent(&h, "matrix", STOPPED_NOTE), runs),
            (1, 0),
            "{code}: stop notices, reviewer sessions"
        );
        h.stop_engine().await;
    }
}

// An outage of every review host is announced once on the working backend behind a failing
// one, also across a restart, and the review runs when a host answers.
#[tokio::test]
async fn a_host_outage_is_announced_once_on_the_working_backend() {
    let mut h = failing_extra_first(|cfg| {
        cfg.pohunek_hosts = vec!["host-a".to_owned(), "host-b".to_owned()];
    })
    .await;
    for host in ["host-a", "host-b"] {
        let failing = (host.to_owned(), "pohunek_request_failed".to_owned());
        h.fake.lock().host_fail.extend([failing]);
    }
    let pr = integration_pull_request("acme/widget", &sha("a"));
    open(&h, &pr);
    h.settle().await;
    h.stop_engine().await;
    h.start();
    h.settle().await;
    h.fake.lock().host_fail.remove("host-b");
    h.poll_until("the review on the host that answers", || {
        h.publications() == 1
    })
    .await;
    assert_eq!(sent(&h, "matrix", HOST_WAIT_NOTE), 1, "notices of the wait");
    h.stop_engine().await;
}

// A waiting review superseded by a newer head, and the review of that head, reach the working
// backend behind a failing one: one request per head and one superseded notice.
#[tokio::test]
async fn a_superseded_wait_and_its_successor_reach_the_working_backend() {
    let mut h = failing_extra_first(|_| {}).await;
    let title = APPROVAL_REQUESTED_TITLE;
    let pr = integration_pull_request("acme/manual", &sha("a"));
    open(&h, &pr);
    h.poll_until("the first request", || sent(&h, "matrix", title) == 1)
        .await;
    let mut next = pr.clone();
    next.head_sha = sha("c");
    open(&h, &next);
    let superseded = format!("Review {} was superseded", pr.review_id());
    h.poll_until("the request of the new head", || {
        sent(&h, "matrix", title) == 2
    })
    .await;
    h.settle().await;
    let counts = [sent(&h, "matrix", title), sent(&h, "matrix", &superseded)];
    assert_eq!(counts, [2, 1], "requests and superseded notices");
    assert_eq!(h.publications(), 0, "nothing is posted without an approval");
    h.stop_engine().await;
}
