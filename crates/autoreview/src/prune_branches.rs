//! `autoreview prune-branches`: lists the review branches that no review needs on the local
//! review hosts, and deletes them with `--yes`.
//!
//! The branches are listed before the store is read, so a review that the store does not show
//! as active cannot own a listed branch: a new review gets a new workflow ID and new branches.

// Rust guideline compliant 2026-11-14

pub(crate) mod eval;
pub(crate) mod plan;

#[cfg(test)]
mod tests;

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use autoreview_engine::config::Config;
use autoreview_engine::store::{ReviewFilter, Store};
use autoreview_engine::{Error, error_code};
use autoreview_external::{
    SessionSummary, branch_refs, config_entries, delete_ref, host_is_local, project_roots,
    session_branches, worktrees,
};
use clap::Args;
use serde::Serialize;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

use crate::command_invalid;
use crate::eval_prune::{SessionSets, eval_session_sets};
use plan::{BranchPlan, HEADS, Owners, Rules, Verdict};

/// Error of a repository whose configuration names a hook that a deletion would run.
const REPOSITORY_HOOK_CONFIGURED: &str = "repository_hook_configured";

/// Reason of a host whose repositories are not on this machine.
const HOST_NOT_LOCAL: &str = "host_not_local";

/// Error code of a store database that does not exist.
const STORE_UNAVAILABLE: &str = "store_unavailable";

/// Error code of a run that left work undone; its document is already on stdout.
pub(crate) const BRANCH_PRUNE_INCOMPLETE: &str = "branch_prune_incomplete";

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
    /// Whether `--host` named the one host; then a skipped host fails the command.
    #[serde(skip)]
    named: bool,
}

impl PruneResult {
    /// Fails a run that left work undone, after its document was printed.
    ///
    /// # Errors
    ///
    /// Returns `branch_prune_incomplete` when [`incomplete`] reports the run.
    pub(crate) fn status(&self) -> Result<(), Error> {
        if incomplete(&self.hosts, self.named) {
            return Err(Error::workflow(BRANCH_PRUNE_INCOMPLETE, false, None));
        }
        Ok(())
    }
}

/// Reports whether a run left work undone: a deletion failed, or the host that `--host` named
/// was skipped or has a repository whose git calls failed.
///
/// Without `--host` a skipped host or repository is only reported, so one unreachable host
/// does not fail the others.
fn incomplete(hosts: &[HostReport], named: bool) -> bool {
    hosts.iter().any(|host| {
        let failed_delete = host
            .repositories
            .iter()
            .flat_map(|repository| &repository.branches)
            .any(|branch| !branch.error.is_empty());
        let skipped = !host.skipped.is_empty()
            || host
                .repositories
                .iter()
                .any(|repository| !repository.error.is_empty());
        failed_delete || (named && skipped)
    })
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
pub(crate) struct RepositoryReport {
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
    /// `for-each-ref`, `worktree list` and `config --list` output, or the first error.
    output: Result<(String, String, String), Error>,
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
        let freed = BTreeSet::new();
        let rules = Rules {
            prefix: &config.session_branch_prefix,
            owners: &owners,
            sessions: &sessions,
            freed: &freed,
            only: None,
            older_than,
            retention: config.review_retention,
            now,
        };
        let mut repositories = Vec::new();
        for listing in listings {
            let report =
                prune_repository(config, cancel, git, &host, listing, &rules, arguments.yes).await;
            repositories.push(report);
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
        named: arguments.host.is_some(),
    })
}

/// Returns the git output of every repository of `host`, or why the host is skipped.
///
/// The host must be the daemon of this machine, so that the repository roots it reports are
/// paths here.
async fn list_host(
    config: &Config,
    cancel: &CancellationToken,
    git: &str,
    host: &str,
) -> Result<Vec<Listing>, String> {
    let roots = match host_is_local(config, cancel, host).await {
        Ok(true) => project_roots(config, cancel, host).await,
        Ok(false) => {
            tracing::warn!(host, "prune host skipped: not local");
            return Err(HOST_NOT_LOCAL.to_owned());
        }
        Err(error) => Err(error),
    };
    let roots = roots.map_err(|error| {
        tracing::warn!(host, error = %error, "prune host skipped");
        error_code(&error).to_owned()
    })?;
    let mut listings = Vec::new();
    for path in roots {
        let output = read_repository(config, cancel, git, &path).await;
        listings.push(Listing { path, output });
    }
    Ok(listings)
}

/// Reads the branches, the worktrees and the configuration of the repository at `root`.
async fn read_repository(
    config: &Config,
    cancel: &CancellationToken,
    git: &str,
    root: &str,
) -> Result<(String, String, String), Error> {
    let refs = branch_refs(config, cancel, git, root).await?;
    let trees = worktrees(config, cancel, git, root).await?;
    let entries = config_entries(config, cancel, git, root).await?;
    Ok((refs, trees, entries))
}

/// Plans the branches of one repository and, when `delete` is set, deletes the `delete` ones.
///
/// Each deletion is logged with its branch and commit before it runs, and only succeeds while
/// the branch still points at the listed commit.
async fn prune_repository(
    config: &Config,
    cancel: &CancellationToken,
    git: &str,
    host: &str,
    listing: Listing,
    rules: &Rules<'_>,
    delete: bool,
) -> RepositoryReport {
    let (refs, trees, entries) = match listing.output {
        Ok(output) => output,
        Err(error) => {
            let path = listing.path.as_str();
            tracing::warn!(host, repository = path, error = %error, "prune repository skipped");
            return RepositoryReport {
                error: error_code(&error).to_owned(),
                path: listing.path,
                branches: Vec::new(),
            };
        }
    };
    // git 2.54 runs a configured `reference-transaction` hook despite the event switch.
    if plan::deletion_hook(&entries) {
        let path = listing.path.as_str();
        tracing::warn!(
            host,
            repository = path,
            "prune repository skipped: hook configured"
        );
        return RepositoryReport {
            error: REPOSITORY_HOOK_CONFIGURED.to_owned(),
            path: listing.path,
            branches: Vec::new(),
        };
    }
    let mut branches = plan::plan(&refs, &trees, rules);
    if delete {
        let path = listing.path.as_str();
        for branch in branches
            .iter_mut()
            .filter(|branch| branch.verdict == Verdict::Delete)
        {
            let (name, commit) = (branch.branch.as_str(), branch.commit.as_str());
            tracing::info!(
                host,
                repository = path,
                branch = name,
                commit,
                "branch delete"
            );
            let refname = format!("{HEADS}{name}");
            match delete_ref(config, cancel, git, path, &refname, commit).await {
                Ok(()) => branch.deleted = true,
                Err(error) => {
                    tracing::warn!(host, repository = path, branch = name, commit,
                        error = %error, "branch delete failed");
                    branch.error = error_code(&error).to_owned();
                }
            }
        }
    }
    RepositoryReport {
        path: listing.path,
        error: String::new(),
        branches,
    }
}

/// A review branch on one review host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RunBranch {
    /// The host as `pohunek.hosts` names it.
    host: String,
    /// Branch name without `refs/heads/`.
    branch: String,
}

/// What became of the review branches that an eval run started.
#[derive(Debug, Default)]
pub(crate) struct RunCleanup {
    /// The branches deleted from their host.
    pub(crate) deleted: Vec<RunBranch>,
    /// The branches that stay: their host is not local or cannot be read, a session, a
    /// worktree or a hook keeps them, or the deletion failed.
    pub(crate) left: Vec<RunBranch>,
}

/// Deletes the review branches that an eval run started, and no other branch.
///
/// `branches` holds the host and name of every session start of the run, under `prefix`. The
/// listing, the verdicts and the deletion are those of `prune-branches`, limited to these
/// names and without a store: no review of another process owns a branch of this run.
pub(crate) async fn delete_run_branches(
    config: &Config,
    cancel: &CancellationToken,
    git: &str,
    prefix: &str,
    branches: &BTreeSet<(String, String)>,
) -> RunCleanup {
    let mut cleanup = RunCleanup::default();
    let hosts: BTreeSet<&str> = branches.iter().map(|(host, _)| host.as_str()).collect();
    for host in hosts {
        let names: BTreeSet<&str> = (branches.iter())
            .filter(|(owner, _)| owner == host)
            .map(|(_, name)| name.as_str())
            .collect();
        let outcome = delete_host_branches(config, cancel, git, prefix, host, &names).await;
        let (deleted, kept) = outcome;
        for name in names {
            let branch = RunBranch {
                host: host.to_owned(),
                branch: name.to_owned(),
            };
            if deleted.contains(name) {
                cleanup.deleted.push(branch);
            } else if kept.as_ref().is_none_or(|kept| kept.contains(name)) {
                cleanup.left.push(branch);
            }
        }
    }
    cleanup
}

/// Deletes the `names` that exist on `host` and returns the deleted ones and the ones that stay.
///
/// The second set is `None` when the branches of the host cannot all be known: the host is not
/// local, or a call or a repository failed.
async fn delete_host_branches(
    config: &Config,
    cancel: &CancellationToken,
    git: &str,
    prefix: &str,
    host: &str,
    names: &BTreeSet<&str>,
) -> (BTreeSet<String>, Option<BTreeSet<String>>) {
    let mut deleted = BTreeSet::new();
    let Ok(listings) = list_host(config, cancel, git, host).await else {
        return (deleted, None);
    };
    let sessions = match session_branches(config, cancel, host).await {
        Ok(sessions) => sessions,
        Err(error) => {
            tracing::warn!(host, error = %error, "eval branches kept: sessions unknown");
            return (deleted, None);
        }
    };
    let owners = Owners::default();
    let freed = BTreeSet::new();
    let rules = Rules {
        prefix,
        owners: &owners,
        sessions: &sessions,
        freed: &freed,
        only: Some(names),
        older_than: None,
        retention: config.review_retention,
        now: OffsetDateTime::now_utc(),
    };
    let mut kept = Some(BTreeSet::new());
    for listing in listings {
        let report = prune_repository(config, cancel, git, host, listing, &rules, true).await;
        if !report.error.is_empty() {
            kept = None;
        }
        for branch in report.branches {
            if branch.deleted {
                deleted.insert(branch.branch);
            } else if let Some(kept) = kept.as_mut() {
                kept.insert(branch.branch);
            }
        }
    }
    (deleted, kept)
}

/// Plans the eval branches of one review host and, when `delete` is set, deletes the `delete`
/// ones, or returns why the host is skipped.
///
/// The git listing comes before the session read, as in `prune-branches`. `selected` are the
/// eval sessions found on the host; [`eval_session_sets`] decides which of their branches
/// still count as a live session or a checked-out worktree. No review owns an eval branch and
/// no age limit applies, so every well-formed branch under `prefix` that no live session,
/// worktree or hook keeps is `delete`.
pub(crate) async fn prune_eval_host(
    config: &Config,
    cancel: &CancellationToken,
    git: &str,
    host: &str,
    prefix: &str,
    selected: &[&SessionSummary],
    delete: bool,
) -> Result<Vec<RepositoryReport>, String> {
    let listings = list_host(config, cancel, git, host).await?;
    let read = session_branches(config, cancel, host)
        .await
        .map_err(|error| {
            tracing::warn!(host, error = %error, "eval prune host skipped: sessions unknown");
            error_code(&error).to_owned()
        })?;
    let SessionSets { live, freed } = eval_session_sets(read, selected, delete);
    let owners = Owners::default();
    let rules = Rules {
        prefix,
        owners: &owners,
        sessions: &live,
        freed: &freed,
        only: None,
        older_than: None,
        retention: config.review_retention,
        now: OffsetDateTime::now_utc(),
    };
    let mut repositories = Vec::new();
    for listing in listings {
        let report = prune_repository(config, cancel, git, host, listing, &rules, delete).await;
        repositories.push(report);
    }
    Ok(repositories)
}

/// Returns `pohunek.hosts`, then each other host that a route override names, without repeats.
///
/// An override, also an `untrusted` one, may name a host that `pohunek.hosts` does not list.
fn review_hosts(config: &Config) -> Vec<String> {
    let mut hosts = config.pohunek_hosts.clone();
    let routes = config.route_overrides.values();
    for route in routes.chain(config.untrusted_route_overrides.values()) {
        if !hosts.contains(&route.host) {
            hosts.push(route.host.clone());
        }
    }
    hosts
}

/// Returns the report of a skipped host.
fn skipped(host: String, reason: String) -> HostReport {
    HostReport {
        host,
        skipped: reason,
        repositories: Vec::new(),
    }
}
