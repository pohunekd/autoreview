//! `autoreview eval prune`: removes the sessions and the branches that a killed `eval run` left
//! on the review hosts.
//!
//! The sessions go first, so that their branches no longer count as `session_live`; the
//! branches are then planned and deleted as `prune-branches` does, under the eval branch prefix
//! and without a store.

// Rust guideline compliant 2026-11-14

#[cfg(test)]
mod tests;

use autoreview_engine::config::Config;
use autoreview_engine::eval::config::EvalConfig;
use autoreview_engine::integrations::Integrations;
use autoreview_engine::model::SessionIdentity;
use autoreview_engine::{Error, error_code};
use autoreview_external::{External, SessionSummary, list_sessions};
use clap::Args;
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use super::{RepositoryReport, prune_eval_host, review_hosts};
use crate::command_invalid;
use crate::eval_cli::{EVAL_PRUNE_INCOMPLETE, reading_external};
use crate::eval_prune::select_eval_sessions;

/// Error code of a session removal that ran past `session.timeout_seconds`.
const SESSION_REMOVAL_TIMED_OUT: &str = "session_removal_timed_out";

/// The options of `eval prune`.
#[derive(Debug, Args)]
pub(crate) struct EvalPruneArguments {
    /// Absolute path of the git program that lists and deletes the branches.
    #[arg(long, value_name = "PATH")]
    git_binary: String,
    /// The review host to prune, as `pohunek.hosts` or a route override names it; every one
    /// when absent.
    #[arg(long, value_name = "HOST")]
    host: Option<String>,
    /// Removes the sessions and deletes the branches; without it nothing changes.
    #[arg(long)]
    yes: bool,
}

/// What `eval prune` prints.
#[derive(Debug, Serialize)]
pub(crate) struct EvalPruneResult {
    /// Whether the command removed sessions and deleted branches (`--yes`).
    delete: bool,
    /// One entry per review host: `pohunek.hosts` in order, then the route override hosts.
    hosts: Vec<EvalHostReport>,
    /// Whether `--host` named the one host; then an unchecked host fails the command.
    #[serde(skip)]
    named: bool,
}

impl EvalPruneResult {
    /// Fails a run that left work undone, after its document was printed.
    ///
    /// # Errors
    ///
    /// Returns `eval_prune_incomplete` when [`incomplete`] reports the run.
    pub(crate) fn status(&self) -> Result<(), Error> {
        if incomplete(&self.hosts, self.named) {
            return Err(Error::workflow(EVAL_PRUNE_INCOMPLETE, false, None));
        }
        Ok(())
    }
}

/// The eval sessions and branches of one review host.
#[derive(Debug, Serialize)]
struct EvalHostReport {
    /// The host as `pohunek.hosts` names it.
    host: String,
    /// The error code of the failed session list; nothing was done on the host. Empty otherwise.
    #[serde(skip_serializing_if = "String::is_empty")]
    skipped: String,
    /// Why the branches were not checked: `host_not_local`, or the error code of a failed call.
    /// The sessions were handled; empty when the branches were checked.
    #[serde(skip_serializing_if = "String::is_empty")]
    branches_skipped: String,
    /// The sessions of an eval run, with the result of their removal under `--yes`.
    sessions: Vec<SessionReport>,
    /// The repositories of the host's projects, as `prune-branches` reports them.
    repositories: Vec<RepositoryReport>,
}

/// An eval session and, under `--yes`, the result of its removal.
#[derive(Debug, Serialize)]
struct SessionReport {
    /// Identifier of the session on the host.
    id: String,
    /// Name of the session.
    name: String,
    /// Branch of the session.
    branch: String,
    /// Lifecycle state of the session when it was listed.
    state: String,
    /// Whether `--yes` removed the session and confirmed that it is gone.
    removed: bool,
    /// Error code of a failed removal.
    #[serde(skip_serializing_if = "String::is_empty")]
    error: String,
}

impl SessionReport {
    /// Reports `session` as not removed.
    fn listed(session: &SessionSummary) -> Self {
        Self {
            id: session.id.clone(),
            name: session.name.clone(),
            branch: session.branch.clone(),
            state: session.state.clone(),
            removed: false,
            error: String::new(),
        }
    }
}

/// Reports whether a run left work undone: a removal or a deletion failed, or the host that
/// `--host` named was not checked, or has a repository whose git calls failed.
///
/// Without `--host` an unchecked host or repository is only reported, so one unreachable host
/// does not fail the others.
fn incomplete(hosts: &[EvalHostReport], named: bool) -> bool {
    hosts.iter().any(|host| {
        let failed_removal = host
            .sessions
            .iter()
            .any(|session| !session.error.is_empty());
        let failed_delete = host
            .repositories
            .iter()
            .flat_map(|repository| &repository.branches)
            .any(|branch| !branch.error.is_empty());
        let unchecked = !host.skipped.is_empty()
            || !host.branches_skipped.is_empty()
            || host
                .repositories
                .iter()
                .any(|repository| !repository.error.is_empty());
        failed_removal || failed_delete || (named && unchecked)
    })
}

/// Lists the eval sessions and branches of the selected hosts and, with `--yes`, removes the
/// sessions and deletes the branches with the verdict `delete`.
///
/// A host whose session list fails is skipped; a host whose branches cannot be checked keeps
/// its removed sessions in the report. The other hosts go on.
///
/// # Errors
///
/// Returns `command_invalid` for a relative `--git-binary` or a `--host` that is not a
/// configured review host, and with `--yes` the errors of building the session client.
pub(crate) async fn run(
    config: &Config,
    eval: &EvalConfig,
    arguments: &EvalPruneArguments,
    cancel: &CancellationToken,
) -> Result<EvalPruneResult, Error> {
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
    let external = arguments
        .yes
        .then(|| reading_external(config))
        .transpose()?;
    let mut reports = Vec::new();
    for host in hosts {
        let report = prune_host(config, eval, cancel, arguments, external.as_ref(), host).await;
        reports.push(report);
    }
    Ok(EvalPruneResult {
        delete: arguments.yes,
        hosts: reports,
        named: arguments.host.is_some(),
    })
}

/// Handles the eval sessions and then the eval branches of one host.
///
/// `external` is set under `--yes` and removes the sessions. A failed session list skips the
/// host before anything is done on it.
async fn prune_host(
    config: &Config,
    eval: &EvalConfig,
    cancel: &CancellationToken,
    arguments: &EvalPruneArguments,
    external: Option<&External>,
    host: String,
) -> EvalHostReport {
    let listed = match list_sessions(config, cancel, &host).await {
        Ok(listed) => listed,
        Err(error) => {
            tracing::warn!(host, error = %error, "eval prune host skipped: sessions unknown");
            return EvalHostReport {
                host,
                skipped: error_code(&error).to_owned(),
                branches_skipped: String::new(),
                sessions: Vec::new(),
                repositories: Vec::new(),
            };
        }
    };
    let (name_prefix, branch_prefix) = (&eval.session_name_prefix, &eval.session_branch_prefix);
    let selected = select_eval_sessions(&listed, name_prefix, branch_prefix);
    let mut sessions: Vec<SessionReport> = selected
        .iter()
        .map(|session| SessionReport::listed(session))
        .collect();
    if let Some(external) = external {
        for session in &mut sessions {
            remove_session(config, cancel, external, &host, session).await;
        }
    }
    let git = arguments.git_binary.as_str();
    let branches = prune_eval_host(
        config,
        cancel,
        git,
        &host,
        branch_prefix,
        &selected,
        arguments.yes,
    )
    .await;
    let (branches_skipped, repositories) = match branches {
        Ok(repositories) => (String::new(), repositories),
        Err(reason) => (reason, Vec::new()),
    };
    EvalHostReport {
        host,
        skipped: String::new(),
        branches_skipped,
        sessions,
        repositories,
    }
}

/// Removes one session, never accepting an unconfirmed removal, and records the result.
async fn remove_session(
    config: &Config,
    cancel: &CancellationToken,
    external: &External,
    host: &str,
    session: &mut SessionReport,
) {
    let identity = SessionIdentity {
        host: host.to_owned(),
        session_id: session.id.clone(),
        ..SessionIdentity::default()
    };
    tracing::info!(
        host,
        session = session.id,
        name = session.name,
        "eval session remove"
    );
    let removal = external.remove_session_confirmed(cancel, &identity);
    match tokio::time::timeout(config.session_timeout, removal).await {
        Ok(Ok(())) => session.removed = true,
        Ok(Err(error)) => {
            tracing::warn!(host, session = session.id, error = %error,
                "eval session removal failed");
            session.error = error_code(&error).to_owned();
        }
        Err(_elapsed) => {
            tracing::warn!(host, session = session.id, "eval session removal timed out");
            session.error = SESSION_REMOVAL_TIMED_OUT.to_owned();
        }
    }
}
