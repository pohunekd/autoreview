//! The git calls of `prune-branches`: the branches and worktrees of a repository, one deletion.
//!
//! Every call is an argv list that names the repository with `-C`, never a shell.

// Rust guideline compliant 2026-11-14

use std::ffi::OsString;
use std::path::Path;

use autoreview_engine::Error;
use autoreview_engine::config::Config;
use tokio_util::sync::CancellationToken;

use crate::process::{GIT_INHERITED, child_environment, run};

/// Error code of a git call that could not start, timed out or failed.
const GIT_COMMAND_FAILED: &str = "git_command_failed";

/// Options before `-C`: no hook and no file system monitor of the repository runs.
const SAFE_OPTIONS: [&str; 4] = [
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "core.fsmonitor=false",
];

/// Variables that keep git from reading the system and the user configuration.
const NO_USER_CONFIG: [(&str, &str); 2] = [
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("GIT_CONFIG_GLOBAL", "/dev/null"),
];

/// Variable that stops the repository search of git above the given directory.
const CEILING_VARIABLE: &str = "GIT_CEILING_DIRECTORIES";

/// Lists every local branch of the repository at `root` as `refname NUL commit NUL symref`.
///
/// `symref` is the target of a symbolic ref and empty for a branch.
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
        "--format=%(refname)%00%(objectname)%00%(symref)",
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

/// Deletes `refname` in the repository at `root` only while it still points at `commit`.
///
/// # Errors
///
/// Returns `git_command_failed` when the call fails, also when the ref moved.
pub async fn delete_ref(
    config: &Config,
    cancel: &CancellationToken,
    git: &str,
    root: &str,
    refname: &str,
    commit: &str,
) -> Result<(), Error> {
    // Without `--no-deref` git deletes the target of a symbolic ref instead of the ref.
    let command = ["update-ref", "--no-deref", "-d", refname, commit];
    git_call(config, cancel, git, root, &command)
        .await
        .map(|_| ())
}

/// Runs `git -C <root> <command>` and returns its stdout.
///
/// The child gets the [`GIT_INHERITED`] variables without the secrets and is killed after
/// `pohunek.timeout_seconds`. It reads only the repository's configuration, runs no hook or
/// file system monitor, and does not search for a repository above `root`.
async fn git_call(
    config: &Config,
    cancel: &CancellationToken,
    git: &str,
    root: &str,
    command: &[&str],
) -> Result<String, Error> {
    let mut extra = Vec::with_capacity(NO_USER_CONFIG.len() + 1);
    for (name, value) in NO_USER_CONFIG {
        extra.push((OsString::from(name), OsString::from(value)));
    }
    if let Some(parent) = Path::new(root).parent() {
        // The variable is a colon-separated list, so a colon would split the parent.
        if parent.as_os_str().as_encoded_bytes().contains(&b':') {
            let cause = "the parent of the repository root holds a colon";
            return Err(Error::workflow(
                GIT_COMMAND_FAILED,
                true,
                Some(cause.into()),
            ));
        }
        extra.push((
            OsString::from(CEILING_VARIABLE),
            parent.as_os_str().to_owned(),
        ));
    }
    let mut arguments = Vec::with_capacity(SAFE_OPTIONS.len() + 2 + command.len());
    for item in SAFE_OPTIONS.iter().chain(&["-C", root]).chain(command) {
        arguments.push((*item).to_owned());
    }
    let environment = child_environment(
        std::env::vars_os(),
        &GIT_INHERITED,
        &config.secret_variables,
        extra,
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
