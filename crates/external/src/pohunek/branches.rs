//! The pohunek questions of `prune-branches`: is a host local, its repositories, its sessions.
//!
//! Each is one read-only call of the configured pohunek CLI.

// Rust guideline compliant 2026-11-14

use std::collections::BTreeSet;

use autoreview_engine::Error;
use autoreview_engine::config::Config;
use autoreview_go_compat::null_entries::zero_entries;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use super::sessions::{PohunekSession, session_list_arguments};
use super::{PohunekProject, decode_ok, pohunek_call, projects_arguments, response_invalid};

/// The host name that pohunek resolves to the daemon of its own state home.
const LOCAL_HOST: &str = "local";

/// The part of `host governance inspect --json` that identifies a daemon.
#[derive(Debug, Deserialize)]
struct Governance {
    /// The stable identity of the daemon's host.
    host_id: String,
}

/// Reports whether `host` names the daemon that `local` names, by the `host_id` of both.
///
/// # Errors
///
/// Returns the pohunek failure of either call, and `pohunek_response_invalid` for an answer
/// without a `host_id`.
pub async fn host_is_local(
    config: &Config,
    cancel: &CancellationToken,
    host: &str,
) -> Result<bool, Error> {
    let local = daemon_id(config, cancel, LOCAL_HOST).await?;
    if host == LOCAL_HOST {
        return Ok(true);
    }
    Ok(daemon_id(config, cancel, host).await? == local)
}

/// Returns the `host_id` that `host governance inspect <host> --json` reports.
async fn daemon_id(
    config: &Config,
    cancel: &CancellationToken,
    host: &str,
) -> Result<String, Error> {
    let arguments = ["host", "governance", "inspect", host, "--json"].map(str::to_owned);
    let output = pohunek_call(config, cancel, &arguments).await?;
    let governance: Governance = decode_ok(&output).ok_or_else(response_invalid)?;
    if governance.host_id.is_empty() {
        return Err(response_invalid());
    }
    Ok(governance.host_id)
}

/// Returns the absolute repository roots of the projects of `host`, sorted, without repeats.
///
/// # Errors
///
/// Returns the pohunek failure of `project list`, and `pohunek_response_invalid` for an answer
/// that holds no project list.
pub async fn project_roots(
    config: &Config,
    cancel: &CancellationToken,
    host: &str,
) -> Result<Vec<String>, Error> {
    let output = pohunek_call(config, cancel, &projects_arguments(host)).await?;
    let entries: Vec<Option<PohunekProject>> = decode_ok(&output).ok_or_else(response_invalid)?;
    let roots: BTreeSet<String> = zero_entries(entries)
        .into_iter()
        .map(|project| project.repo_root)
        .filter(|root| root.starts_with('/'))
        .collect();
    Ok(roots.into_iter().collect())
}

/// One session of `session list --json`, reduced to what a prune decides on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    /// Identifier of the session.
    pub id: String,
    /// Name of the session.
    pub name: String,
    /// Branch the session works on; empty when it has none.
    pub branch: String,
    /// Lifecycle state of the session; `running` for a live session.
    pub state: String,
}

/// Returns every session of `host`, whatever its state.
///
/// # Errors
///
/// Returns the pohunek failure of `session list`, and `pohunek_response_invalid` for an answer
/// that holds no session list.
pub async fn list_sessions(
    config: &Config,
    cancel: &CancellationToken,
    host: &str,
) -> Result<Vec<SessionSummary>, Error> {
    let output = pohunek_call(config, cancel, &session_list_arguments(host)).await?;
    let entries: Vec<Option<PohunekSession>> = decode_ok(&output).ok_or_else(response_invalid)?;
    Ok(zero_entries(entries)
        .into_iter()
        .map(|session| SessionSummary {
            id: session.id,
            name: session.name,
            branch: session.branch,
            state: session.state,
        })
        .collect())
}

/// Returns the branch of every session of `host`, whatever its state.
///
/// # Errors
///
/// Returns the failures of [`list_sessions`].
pub async fn session_branches(
    config: &Config,
    cancel: &CancellationToken,
    host: &str,
) -> Result<BTreeSet<String>, Error> {
    Ok(list_sessions(config, cancel, host)
        .await?
        .into_iter()
        .map(|session| session.branch)
        .filter(|branch| !branch.is_empty())
        .collect())
}
