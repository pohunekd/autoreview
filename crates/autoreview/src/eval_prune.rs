//! Decisions of `autoreview eval prune` on plain data: which sessions an eval run left behind.

// Rust guideline compliant 2026-11-14

#![cfg_attr(
    not(test),
    expect(dead_code, reason = "called by the eval prune command")
)]

#[cfg(test)]
mod tests;

use std::collections::BTreeSet;

use autoreview_external::SessionSummary;

use crate::prune_branches::plan::workflow_id;

/// Returns the sessions of `listed` that an eval run started, in listing order.
///
/// A session qualifies only when its name starts with `name_prefix`, its branch starts with
/// `branch_prefix`, both continue with the same token, and the branch holds a workflow ID after
/// its prefix. A session of the service, or one that a person named alike, fails one of these.
pub(crate) fn select_eval_sessions<'a>(
    listed: &'a [SessionSummary],
    name_prefix: &str,
    branch_prefix: &str,
) -> Vec<&'a SessionSummary> {
    listed
        .iter()
        .filter(|session| is_eval_session(session, name_prefix, branch_prefix))
        .collect()
}

/// Returns the branches that keep a branch `session_live` when the eval branches are planned.
///
/// `read` is a read of the host's session branches. A dry run (`delete` unset) reads before any
/// removal, so the branches of `selected` leave the set: the plan shows the verdicts that
/// `--yes` produces. With `delete`, `read` follows the removals and stays as read, so the
/// branch of a session whose removal failed keeps its `session_live` verdict.
pub(crate) fn live_session_branches(
    mut read: BTreeSet<String>,
    selected: &[&SessionSummary],
    delete: bool,
) -> BTreeSet<String> {
    if !delete {
        for session in selected {
            read.remove(&session.branch);
        }
    }
    read
}

/// Reports whether `session` has the name, branch and workflow ID of an eval session start.
fn is_eval_session(session: &SessionSummary, name_prefix: &str, branch_prefix: &str) -> bool {
    let name_token = session.name.strip_prefix(name_prefix);
    let branch_token = session.branch.strip_prefix(branch_prefix);
    match (name_token, branch_token) {
        (Some(name), Some(branch)) => {
            name == branch && workflow_id(&session.branch, branch_prefix).is_some()
        }
        _ => false,
    }
}
