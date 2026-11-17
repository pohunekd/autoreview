use std::time::Duration;

use autoreview_engine::model::WORKFLOW_ID_PREFIX;
use autoreview_external::SessionSummary;
use time::OffsetDateTime;

use super::{SessionSets, eval_session_sets, select_eval_sessions};
use crate::prune_branches::plan::{Owners, Rules, Verdict, plan};

const NAME_PREFIX: &str = "eval-review-";

const BRANCH_PREFIX: &str = "autoreview/eval/";

fn token(digit: char) -> String {
    format!(
        "{WORKFLOW_ID_PREFIX}{}-opus-run1",
        digit.to_string().repeat(64)
    )
}

fn session(id: &str, name: &str, branch: &str, state: &str) -> SessionSummary {
    SessionSummary {
        id: id.to_owned(),
        name: name.to_owned(),
        branch: branch.to_owned(),
        state: state.to_owned(),
    }
}

#[test]
fn select_eval_sessions_keeps_only_sessions_that_an_eval_run_started() {
    let (one, two) = (token('1'), token('2'));
    let listed = [
        session(
            "eval-running",
            &format!("{NAME_PREFIX}{one}"),
            &format!("{BRANCH_PREFIX}{one}"),
            "running",
        ),
        session(
            "service",
            &format!("github-review-{one}"),
            &format!("autoreview/review/{one}"),
            "running",
        ),
        session(
            "person-session",
            &format!("fix-{one}"),
            &format!("{BRANCH_PREFIX}{one}"),
            "running",
        ),
        session(
            "nested-prefix",
            &format!("{NAME_PREFIX}sub/{one}"),
            &format!("{BRANCH_PREFIX}sub/{one}"),
            "running",
        ),
        session(
            "no-workflow-id",
            &format!("{NAME_PREFIX}manual"),
            &format!("{BRANCH_PREFIX}manual"),
            "running",
        ),
        session(
            "token-mismatch",
            &format!("{NAME_PREFIX}{one}"),
            &format!("{BRANCH_PREFIX}{two}"),
            "running",
        ),
        session(
            "empty-branch",
            &format!("{NAME_PREFIX}{one}"),
            "",
            "running",
        ),
        session(
            "eval-failed",
            &format!("{NAME_PREFIX}{two}"),
            &format!("{BRANCH_PREFIX}{two}"),
            "failed",
        ),
    ];
    let selected = select_eval_sessions(&listed, NAME_PREFIX, BRANCH_PREFIX);
    let ids: Vec<&str> = selected.iter().map(|session| session.id.as_str()).collect();
    assert_eq!(ids, ["eval-running", "eval-failed"]);
}

/// Plans `branch` with the session sets of `eval_session_sets` and a worktree that has it
/// checked out.
fn planned(read: &[&str], selected: &[&SessionSummary], delete: bool, branch: &str) -> Verdict {
    let read = read.iter().map(|branch| (*branch).to_owned()).collect();
    let SessionSets { live, freed } = eval_session_sets(read, selected, delete);
    let owners = Owners::default();
    let rules = Rules {
        prefix: BRANCH_PREFIX,
        owners: &owners,
        sessions: &live,
        freed: &freed,
        only: None,
        older_than: None,
        retention: Duration::ZERO,
        now: OffsetDateTime::UNIX_EPOCH,
    };
    let refs = format!("refs/heads/{branch}\0{:040}\0\n", 0);
    let worktrees = format!(
        "worktree /work\0HEAD {:040}\0branch refs/heads/{branch}\0\0",
        0
    );
    plan(&refs, &worktrees, &rules)[0].verdict
}

#[test]
fn eval_session_sets_predict_the_removals_of_a_dry_run_only() {
    let branch = format!("{BRANCH_PREFIX}{}", token('1'));
    let eval = session("one", "one", &branch, "running");
    let selected = [&eval];

    // A dry run reads before the removal that frees the branch and its worktree.
    assert_eq!(
        planned(&[&branch], &selected, false, &branch),
        Verdict::Delete
    );
    // With `--yes` the removal failed: the session and its worktree are still there.
    assert_eq!(
        planned(&[&branch], &selected, true, &branch),
        Verdict::SessionLive
    );
    // With `--yes` the session is gone but a worktree outlived it.
    assert_eq!(planned(&[], &selected, true, &branch), Verdict::CheckedOut);
}
