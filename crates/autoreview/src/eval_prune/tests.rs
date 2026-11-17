use std::collections::BTreeSet;

use autoreview_engine::model::WORKFLOW_ID_PREFIX;
use autoreview_external::SessionSummary;

use super::{live_session_branches, select_eval_sessions};

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

#[test]
fn live_session_branches_predicts_the_removals_of_a_dry_run_only() {
    let (one, two, other) = (token('1'), token('2'), "autoreview/eval/other".to_owned());
    let eval = |id: &str, branch: &str| session(id, id, branch, "running");
    let selected = [eval("one", &one), eval("two", &two)];
    let selected: Vec<&SessionSummary> = selected.iter().collect();

    // A dry run reads before any removal: both selected branches are predicted free.
    let before = BTreeSet::from([one.clone(), two.clone(), other.clone()]);
    let dry_run = live_session_branches(before, &selected, false);
    assert_eq!(dry_run, BTreeSet::from([other.clone()]));

    // After `--yes` the read still lists the session whose removal failed.
    let after = BTreeSet::from([two.clone(), other.clone()]);
    let removed = live_session_branches(after.clone(), &selected, true);
    assert_eq!(removed, after);
}
