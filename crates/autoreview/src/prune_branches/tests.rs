use std::collections::BTreeSet;
use std::time::Duration;

use autoreview_engine::model::WORKFLOW_ID_PREFIX;
use autoreview_engine::store::StoredReview;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::plan::{Owners, Rules, Verdict, plan};

const PREFIX: &str = "autoreview/review/";

const DAY: u64 = 24 * 60 * 60;

fn workflow(digit: char) -> String {
    format!("{WORKFLOW_ID_PREFIX}{}", digit.to_string().repeat(64))
}

fn review(digit: char, status: &str, cleanup_pending: bool, updated_at: &str) -> StoredReview {
    StoredReview {
        workflow_id: workflow(digit),
        status: status.to_owned(),
        cleanup_pending,
        updated_at: updated_at.to_owned(),
        ..StoredReview::default()
    }
}

fn branch(digit: char, rest: &str) -> String {
    format!("{PREFIX}{}{rest}", workflow(digit))
}

fn time(text: &str) -> OffsetDateTime {
    OffsetDateTime::parse(text, &Rfc3339).expect("time")
}

#[test]
fn plan_keeps_every_branch_a_review_session_or_worktree_needs() {
    let reviews = [
        review('1', "queued", false, "2026-10-01T00:00:00Z"),
        review('2', "running", false, "2026-10-01T00:00:00Z"),
        review('3', "awaiting", false, "2026-10-01T00:00:00Z"),
        review('4', "completed", true, "2026-10-01T00:00:00Z"),
        review('5', "completed", false, "2026-11-04T12:00:00.5Z"),
        review('6', "completed", false, "2026-11-13T12:00:00Z"),
        review('7', "completed", false, "not a time"),
        review('8', "paused", false, "2026-10-01T00:00:00Z"),
    ];
    let owners = Owners::from_reviews(&reviews);
    let sessions = BTreeSet::from([branch('5', "-sonnet-run2")]);
    let upper = format!("{PREFIX}{WORKFLOW_ID_PREFIX}{}-opus-run1", "A".repeat(64));
    let names = [
        branch('1', "-opus-run1"),
        branch('2', "-opus-run1-start2"),
        branch('3', "-opus-run1"),
        branch('4', "-opus-run1"),
        branch('8', "-opus-run1"),
        branch('5', "-opus-run1"),
        branch('6', "-opus-run1"),
        branch('7', "-opus-run1"),
        branch('9', "-opus-run1"),
        branch('5', "-sonnet-run2"),
        branch('5', "-codex-run1"),
        format!("{PREFIX}manual"),
        upper,
        branch('5', ""),
        branch('5', "-"),
    ];
    let mut refs = String::from("refs/heads/main\0aaaa\n");
    refs.push_str("refs/heads/autoreview/reviewer\0bbbb\n");
    refs.push_str(&format!(
        "refs/remotes/origin/{}\0cccc\n",
        branch('5', "-x-run1")
    ));
    refs.push_str(&format!(
        "refs/heads/{} without a commit\n",
        branch('5', "-y-run1")
    ));
    for (index, name) in names.iter().enumerate() {
        refs.push_str(&format!("refs/heads/{name}\0{index:040}\n"));
    }
    let worktrees = format!(
        "worktree /srv/repo\0HEAD aaaa\0branch refs/heads/main\0\0\
         worktree /srv/branch refs/heads/{}\0HEAD bbbb\0detached\0\0\
         worktree /srv/review\0HEAD cccc\0branch refs/heads/{}\0\0",
        branch('6', "-opus-run1"),
        branch('5', "-codex-run1"),
    );
    use Verdict::{
        AgeUnknown, CheckedOut, Delete, NameUnknown, ReviewActive, SessionLive, TooRecent,
    };
    let active = [ReviewActive; 5];
    let kept = [
        SessionLive,
        CheckedOut,
        NameUnknown,
        NameUnknown,
        NameUnknown,
        NameUnknown,
    ];
    let cases = [
        (None, [Delete, Delete, Delete, Delete]),
        (Some(7), [Delete, TooRecent, AgeUnknown, Delete]),
        (Some(30), [TooRecent, TooRecent, AgeUnknown, Delete]),
        (Some(31), [TooRecent, TooRecent, AgeUnknown, AgeUnknown]),
    ];
    for (days, aged) in cases {
        let rules = Rules {
            prefix: PREFIX,
            owners: &owners,
            sessions: &sessions,
            older_than: days.map(|days| Duration::from_secs(days * DAY)),
            retention: Duration::from_secs(30 * DAY),
            now: time("2026-11-14T12:00:00Z"),
        };
        let plans = plan(&refs, &worktrees, &rules);
        let got: Vec<(&str, Verdict)> = plans
            .iter()
            .map(|plan| (plan.branch.as_str(), plan.verdict))
            .collect();
        let verdicts = active.iter().chain(&aged).chain(&kept);
        let want: Vec<(&str, Verdict)> = names
            .iter()
            .map(String::as_str)
            .zip(verdicts.copied())
            .collect();
        assert_eq!(got, want, "older than {days:?} days");
        for (index, plan) in plans.iter().enumerate() {
            assert_eq!(plan.commit, format!("{index:040}"));
            assert!(!plan.deleted && plan.error.is_empty());
        }
    }
}
