//! Delivery of review and discovery messages to routes with several backends (#30).

// Rust guideline compliant 2026-11-14

use std::sync::Arc;

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
