use autoreview_engine::model::WORKFLOW_ID_PREFIX;
use autoreview_external::SessionSummary;

use super::select_eval_sessions;

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
