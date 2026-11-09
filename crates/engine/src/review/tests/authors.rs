//! Author classes against the fake integration: trusted and untrusted pull requests of one rule.

// Rust guideline compliant 2026-06-26

use autoreview_policy::{Checkout, Permission};

use crate::integrations::SessionRequest;
use crate::model::PullRequest;
use crate::model::review::Decision;
use crate::protocol::fixtures::sha;
use crate::testing::harness::{Harness, integration_pull_request, start_harness};

/// Returns the sessions that were started for `pr`.
fn sessions_of(h: &Harness, pr: &PullRequest) -> Vec<SessionRequest> {
    let (_, _, started, _) = h.fake.snapshot();
    started
        .into_iter()
        .filter(|request| request.pull_request.number == pr.number)
        .collect()
}

// The public/* rule approves a trusted author's clean review from a head checkout. A fork pull
// request is untrusted unless its author is listed, so its clean review is a comment from the
// base checkout; a listed maintainer and a member with maintain are approved (#33, #34).
#[tokio::test]
async fn clean_review_of_a_fork_pull_request_is_a_comment_from_the_base_checkout() {
    let mut h = start_harness(|_| {}).await;
    let mut fork = integration_pull_request("public/widget", &sha("a"));
    fork.author = "stranger".to_owned();
    fork.author_type = "User".to_owned();
    fork.fork = true;
    h.fake.open(fork.clone(), h.rule(&fork.repository));
    h.poll_until("the untrusted review", || h.publications() == 1)
        .await;
    let untrusted = sessions_of(&h, &fork);
    let (published, _, _, _) = h.fake.snapshot();
    assert!(
        published[0].decision == Decision::Comment
            && untrusted.len() == 2
            && untrusted
                .iter()
                .all(|request| request.checkout == Checkout::Base
                    && request.reviewer.id != "codex"),
        "decision={:?} sessions={untrusted:?}",
        published[0].decision
    );

    let mut listed = integration_pull_request("public/widget", &sha("c"));
    listed.author = "maintainer".to_owned();
    listed.author_type = "User".to_owned();
    listed.fork = true;
    h.fake.open(listed.clone(), h.rule(&listed.repository));
    let mut member = integration_pull_request("public/widget", &sha("d"));
    member.author = "member".to_owned();
    member.author_type = "User".to_owned();
    h.fake
        .lock()
        .permissions
        .insert("public/widget member".to_owned(), Permission::Maintain);
    h.fake.open(member.clone(), h.rule(&member.repository));
    h.poll_until("the trusted reviews", || h.publications() == 3)
        .await;
    let (published, _, _, _) = h.fake.snapshot();
    for pr in [&listed, &member] {
        let sessions = sessions_of(&h, pr);
        assert!(
            sessions.len() == 1
                && sessions[0].checkout == Checkout::Head
                && sessions[0].reviewer.id == "codex",
            "{}: sessions={sessions:?}",
            pr.author
        );
    }
    assert!(
        published[1..]
            .iter()
            .all(|review| review.decision == Decision::Approve),
        "trusted decisions = {:?}",
        published
            .iter()
            .map(|review| review.decision.clone())
            .collect::<Vec<_>>()
    );
    let reads = h.fake.lock().permission_reads.clone();
    assert!(
        !reads.is_empty() && reads.iter().all(|read| read == "public/widget member"),
        "only the unlisted author of a same-repository pull request is looked up: {reads:?}"
    );
    h.stop_engine().await;
}
