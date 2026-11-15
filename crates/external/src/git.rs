//! The git calls of `prune-branches`: the branches and worktrees of a repository, one deletion.
//!
//! Every call is an argv list that names the repository with `-C`, never a shell.

// Rust guideline compliant 2026-11-14

use autoreview_engine::Error;
use autoreview_engine::config::Config;
use tokio_util::sync::CancellationToken;

use crate::process::{GIT_INHERITED, child_environment, run};

/// Error code of a git call that could not start, timed out or failed.
const GIT_COMMAND_FAILED: &str = "git_command_failed";

/// Lists every local branch of the repository at `root`, one `refname NUL commit` line each.
///
/// # Errors
///
/// Returns `git_command_failed` when the call fails.
pub async fn branch_refs(
    config: &Config,
    cancel: &CancellationToken,
    git: &str,
    root: &str,
) -> Result<String, Error> {
    let command = [
        "for-each-ref",
        "--format=%(refname)%00%(objectname)",
        "refs/heads/",
    ];
    git_call(config, cancel, git, root, &command).await
}

/// Lists the worktrees of the repository at `root` in the porcelain form, fields ended by NUL.
///
/// # Errors
///
/// Returns `git_command_failed` when the call fails.
pub async fn worktrees(
    config: &Config,
    cancel: &CancellationToken,
    git: &str,
    root: &str,
) -> Result<String, Error> {
    let command = ["worktree", "list", "--porcelain", "-z"];
    git_call(config, cancel, git, root, &command).await
}

/// Runs `git -C <root> <command>` and returns its stdout.
///
/// The child gets the [`GIT_INHERITED`] variables without the secrets and is killed after
/// `pohunek.timeout_seconds`.
async fn git_call(
    config: &Config,
    cancel: &CancellationToken,
    git: &str,
    root: &str,
    command: &[&str],
) -> Result<String, Error> {
    let mut arguments = Vec::with_capacity(2 + command.len());
    arguments.push("-C".to_owned());
    arguments.push(root.to_owned());
    for item in command {
        arguments.push((*item).to_owned());
    }
    let environment = child_environment(
        std::env::vars_os(),
        &GIT_INHERITED,
        &config.secret_variables,
        std::iter::empty(),
    );
    run(
        cancel,
        git,
        &arguments,
        &[],
        &environment,
        config.pohunek_timeout,
    )
    .await
    .map_err(|error| Error::workflow(GIT_COMMAND_FAILED, false, Some(Box::new(error))))
}
