//! Decisions of `autoreview eval prune` on plain data: which sessions an eval run left behind.

// Rust guideline compliant 2026-11-14

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

/// The session facts that the plan of the eval branches of one host uses.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SessionSets {
    /// Branches that a session works on; they plan `session_live`.
    pub(crate) live: BTreeSet<String>,
    /// Branches whose worktree `--yes` removes before it plans; they plan without `checked_out`.
    pub(crate) freed: BTreeSet<String>,
}

/// Returns the session sets that the eval branches are planned with.
///
/// `read` is a read of the host's session branches. A dry run (`delete` unset) reads before any
/// removal, while `--yes` removes the selected sessions and their worktrees first. So a dry run
/// takes the branches of `selected` out of the live set and frees them, and the plan shows the
/// verdicts that `--yes` produces. With `delete`, `read` follows the removals and stays as read
/// and nothing is freed: the branch of a session whose removal failed keeps `session_live`, and
/// a worktree that outlives its session keeps `checked_out`. Known asymmetry: a person's
/// worktree on a selected eval branch plans `delete` in the dry run, and `--yes` keeps the
/// branch `checked_out`, the safe direction.
pub(crate) fn eval_session_sets(
    mut read: BTreeSet<String>,
    selected: &[&SessionSummary],
    delete: bool,
) -> SessionSets {
    let mut freed = BTreeSet::new();
    if !delete {
        for session in selected {
            read.remove(&session.branch);
            freed.insert(session.branch.clone());
        }
    }
    SessionSets { live: read, freed }
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
