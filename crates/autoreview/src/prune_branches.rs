//! `autoreview prune-branches`: lists the review branches that no review needs on the local
//! review hosts, and deletes them with `--yes`.
//!
//! The branches are listed before the store is read, so a review that the store does not show
//! as active cannot own a listed branch: a new review gets a new workflow ID and new branches.

// Rust guideline compliant 2026-11-14

mod plan;

#[cfg(test)]
mod tests;

use std::path::Path;
use std::time::Duration;

use autoreview_engine::config::Config;
use autoreview_engine::store::{ReviewFilter, Store};
use autoreview_engine::{Error, error_code};
use autoreview_external::{
    branch_refs, host_is_local, project_roots, session_branches, worktrees,
};
use clap::Args;
use serde::Serialize;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

use crate::command_invalid;
use plan::{BranchPlan, Owners, Rules};

/// Reason of a host whose repositories are not on this machine.
const HOST_NOT_LOCAL: &str = "host_not_local";

/// Error code of a store database that does not exist.
const STORE_UNAVAILABLE: &str = "store_unavailable";

/// Seconds of one day of `--older-than-days`.
const SECONDS_PER_DAY: u64 = 24 * 60 * 60;

/// The options of `prune-branches`.
#[derive(Debug, Args)]
pub(crate) struct PruneArguments {
    /// Absolute path of the git program that lists and deletes the branches.
    #[arg(long, value_name = "PATH")]
    git_binary: String,
    /// The review host to prune, as `pohunek.hosts` or a route override names it; every one
    /// when absent.
    #[arg(long, value_name = "HOST")]
    host: Option<String>,
    /// Keeps the branches whose review finished fewer than this many days ago.
    #[arg(long, value_name = "DAYS")]
    older_than_days: Option<u32>,
    /// Deletes the branches with the verdict `delete`; without it nothing changes.
    #[arg(long)]
    yes: bool,
}

/// What `prune-branches` prints.
#[derive(Debug, Serialize)]
pub(crate) struct PruneResult {
    /// Whether the command deleted branches (`--yes`).
    delete: bool,
    /// One entry per review host: `pohunek.hosts` in order, then the route override hosts.
    hosts: Vec<HostReport>,
}

/// The branches of one review host, or why it was skipped.
#[derive(Debug, Serialize)]
struct HostReport {
    /// The host as `pohunek.hosts` names it.
    host: String,
    /// `host_not_local`, or the error code of a pohunek call that failed; empty when pruned.
    #[serde(skip_serializing_if = "String::is_empty")]
    skipped: String,
    /// The repositories of the host's projects.
    repositories: Vec<RepositoryReport>,
}

/// The branches under the prefix of one repository.
#[derive(Debug, Serialize)]
struct RepositoryReport {
    /// Repository root, as `pohunek project list` reports it.
    path: String,
    /// Error code of a git call that failed; the repository is then skipped.
    #[serde(skip_serializing_if = "String::is_empty")]
    error: String,
    /// Every branch under `session.branch_prefix` with its verdict.
    branches: Vec<BranchPlan>,
}

/// The git output of one repository, read before the store.
#[derive(Debug)]
struct Listing {
    /// Repository root.
    path: String,
    /// `git for-each-ref` and `git worktree list` output, or the error of the first that failed.
    output: Result<(String, String), Error>,
}

/// Lists the review branches of the selected hosts and, with `--yes`, deletes the ones with
/// the verdict `delete`.
///
/// A host whose repositories are not on this machine, or whose pohunek calls fail, is skipped
/// and reported; the other hosts go on.
///
/// # Errors
///
/// Returns `command_invalid` for a relative `--git-binary` or a `--host` that is not a
/// configured review host, `store_unavailable` when the store database does not exist, and the
/// store errors of the review read.
pub(crate) async fn run(
    config: &Config,
    arguments: &PruneArguments,
    cancel: &CancellationToken,
) -> Result<PruneResult, Error> {
    if !arguments.git_binary.starts_with('/') {
        return Err(command_invalid(Some(
            "--git-binary must be an absolute path".into(),
        )));
    }
    let configured = review_hosts(config);
    let hosts: Vec<String> = match &arguments.host {
        Some(host) if configured.contains(host) => vec![host.clone()],
        Some(_) => {
            return Err(command_invalid(Some(
                "--host is not a configured review host".into(),
            )));
        }
        None => configured,
    };
    let path = Path::new(&config.state_db_path);
    // Opening creates a missing database, and an empty store would leave every branch unowned.
    if !path.is_file() {
        let cause = "the store database does not exist".into();
        return Err(Error::workflow(STORE_UNAVAILABLE, true, Some(cause)));
    }
    let store = Store::open(path, Box::new(OffsetDateTime::now_utc))?;
    let git = arguments.git_binary.as_str();
    let mut listed = Vec::new();
    for host in hosts {
        let listing = list_host(config, cancel, git, &host).await;
        listed.push((host, listing));
    }
    let reviews = store.list_reviews(&ReviewFilter::All, i64::MAX)?;
    store.close()?;
    let owners = Owners::from_reviews(&reviews);
    let older_than = arguments
        .older_than_days
        .map(|days| Duration::from_secs(u64::from(days) * SECONDS_PER_DAY));
    let now = OffsetDateTime::now_utc();
    let mut reports = Vec::new();
    for (host, listing) in listed {
        let listings = match listing {
            Ok(listings) => listings,
            Err(reason) => {
                reports.push(skipped(host, reason));
                continue;
            }
        };
        // A host whose sessions are unknown is skipped: a session may work on any branch.
        let sessions = match session_branches(config, cancel, &host).await {
            Ok(sessions) => sessions,
            Err(error) => {
                tracing::warn!(host, error = %error, "prune host skipped");
                reports.push(skipped(host, error_code(&error).to_owned()));
                continue;
            }
        };
        let rules = Rules {
            prefix: &config.session_branch_prefix,
            owners: &owners,
            sessions: &sessions,
            older_than,
            retention: config.review_retention,
            now,
        };
        let mut repositories = Vec::new();
        for listing in listings {
            repositories.push(plan_repository(&host, listing, &rules));
        }
        reports.push(HostReport {
            host,
            skipped: String::new(),
            repositories,
        });
    }
    Ok(PruneResult {
        delete: arguments.yes,
        hosts: reports,
    })
}
