//! The pohunek session calls of the engine: the session list, the session start and the session
//! that one review ensures.
//!
//! Mirrors `cmd/autoreview/pohunek.go:128-250`. [`start_arguments`] and [`lookup_from_output`] are
//! pure functions of their inputs, so the argument list and the decoding are tested without a
//! child process.

// Rust guideline compliant 2026-06-26

use std::error::Error as StdError;
use std::fmt;

use autoreview_engine::Error;
use autoreview_engine::config::Config;
use autoreview_engine::error::BoxError;
use autoreview_engine::integrations::SessionRequest;
use autoreview_engine::model::SessionIdentity;
use autoreview_go_compat::{null_default, null_entries};
use autoreview_schedule::diagnostic::fetch_failure;
use autoreview_schedule::metadata::{WorkSession, metadata_args};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::{
    decode_ok, failure_code, is_identifier, pohunek_arguments, pohunek_call, pohunek_environment,
    response_invalid,
};
use crate::process::run;

/// Name of the configuration key that holds the pohunek request timeout.
const POHUNEK_TIMEOUT_KEY: &str = "pohunek_timeout";

/// One session of `pohunek session list --json`.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
pub(crate) struct PohunekSession {
    /// Identifier of the session.
    #[serde(deserialize_with = "null_default::deserialize")]
    pub(crate) id: String,
    /// Name of the session, which finds the session of a request.
    #[serde(deserialize_with = "null_default::deserialize")]
    pub(crate) name: String,
    /// Agent profile the session runs.
    #[serde(deserialize_with = "null_default::deserialize")]
    pub(crate) agent: String,
    /// Identifier of the project the session belongs to.
    #[serde(deserialize_with = "null_default::deserialize")]
    pub(crate) project_id: String,
    /// Branch the session works on.
    #[serde(deserialize_with = "null_default::deserialize")]
    pub(crate) branch: String,
    /// Lifecycle state of the session; `running` for a live session.
    #[serde(deserialize_with = "null_default::deserialize")]
    pub(crate) state: String,
    /// Whether the session runs in a linked worktree.
    #[serde(deserialize_with = "null_default::deserialize")]
    pub(crate) is_linked_worktree: bool,
    /// Warnings the session reported; a `null` list is empty and a `null` entry is empty.
    #[serde(deserialize_with = "null_entries::deserialize")]
    pub(crate) warnings: Vec<PohunekWarning>,
    /// Runtime of the worker that runs the session.
    #[serde(deserialize_with = "null_default::deserialize")]
    pub(crate) runtime: PohunekRuntime,
}

autoreview_go_compat::object_only_serde!(PohunekSession);

/// One warning of a pohunek session.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
pub(crate) struct PohunekWarning {
    /// Kind of the warning; only `fetch` is a known kind.
    #[serde(deserialize_with = "null_default::deserialize")]
    pub(crate) kind: String,
    /// Text of the warning.
    #[serde(deserialize_with = "null_default::deserialize")]
    pub(crate) message: String,
    /// Detail that classifies the failure of a fetch.
    #[serde(deserialize_with = "null_default::deserialize")]
    pub(crate) detail: String,
}

autoreview_go_compat::object_only_serde!(PohunekWarning);

/// The runtime of a pohunek session.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
pub(crate) struct PohunekRuntime {
    /// Lifecycle state of the runtime; `live` for a live runtime.
    #[serde(deserialize_with = "null_default::deserialize")]
    pub(crate) state: String,
    /// Identifier of the worker instance.
    #[serde(deserialize_with = "null_default::deserialize")]
    pub(crate) worker_instance_id: String,
    /// Generation of the runtime, as decimal text.
    #[serde(deserialize_with = "null_default::deserialize")]
    pub(crate) runtime_generation: String,
}

autoreview_go_compat::object_only_serde!(PohunekRuntime);

/// What the session list says about the session of one request.
#[derive(Debug)]
pub(crate) enum Lookup {
    /// No session of the request's name exists.
    Absent,
    /// The session of the request's name is live, healthy and owned by the request.
    Owned(SessionIdentity),
    /// The session of the request's name is owned but unhealthy, and the error says why.
    Unhealthy(SessionIdentity, Error),
}

/// Returns the argument list of `session list` on `host`.
pub(crate) fn session_list_arguments(host: &str) -> Vec<String> {
    pohunek_arguments(host, &["session", "list", "--json"])
}

/// Returns the argument list of `session new` for the reviewer session of `request`.
///
/// The list holds the request timeout in milliseconds, the terminal size and the session group
/// metadata. The prompt is not part of the list; it is written to the child's stdin.
pub(crate) fn start_arguments(
    request: &SessionRequest,
    group: &str,
    timeout_ms: i64,
    cols: i64,
    rows: i64,
) -> Result<Vec<String>, Error> {
    let work = WorkSession {
        repository: request.pull_request.repository.clone(),
        number: request.pull_request.number,
        url: request.pull_request.url.clone(),
        head_branch: request.pull_request.head_branch.clone(),
        head_sha: request.pull_request.head_sha.clone(),
    };
    let metadata = metadata_args(&work, group)
        .map_err(|error| Error::workflow("review_identity_invalid", true, Some(Box::new(error))))?;
    let name = request.name();
    let branch = request.branch();
    let base_ref = request.base_ref();
    let timeout = timeout_ms.to_string();
    let cols = cols.to_string();
    let rows = rows.to_string();
    let mut arguments = vec![
        "--host".to_owned(),
        request.route.host.clone(),
        "session".to_owned(),
        "new".to_owned(),
        "--agent".to_owned(),
        request.reviewer.agent_profile.clone(),
        "--request-timeout-ms".to_owned(),
        timeout,
        "--json".to_owned(),
        "--yes".to_owned(),
        "--name".to_owned(),
        name,
        "--project".to_owned(),
        request.route.project_id.clone(),
        "--branch".to_owned(),
        branch,
        "--base-branch".to_owned(),
        base_ref,
        "--cols".to_owned(),
        cols,
        "--rows".to_owned(),
        rows,
        "--input-stdin".to_owned(),
    ];
    for item in metadata {
        arguments.push(item);
    }
    Ok(arguments)
}

/// Returns what the session list `output` says about the session of `request`.
///
/// The output must hold a session list; a second session of the same name is ambiguous.
pub(crate) fn lookup_from_output(output: &str, request: &SessionRequest) -> Result<Lookup, Error> {
    let entries: Vec<Option<PohunekSession>> = decode_ok(output).ok_or_else(response_invalid)?;
    let sessions = null_entries::zero_entries(entries);
    let name = request.name();
    let mut matches: Vec<PohunekSession> = Vec::new();
    for session in sessions {
        if session.name == name {
            matches.push(session);
        }
    }
    if matches.is_empty() {
        return Ok(Lookup::Absent);
    }
    if matches.len() > 1 {
        return Err(Error::workflow("session_identity_ambiguous", true, None));
    }
    let session = matches.pop().ok_or_else(response_invalid)?;
    let generation = session.runtime.runtime_generation.parse::<i64>();
    let identity_valid = session.agent == request.reviewer.agent_profile
        && session.project_id == request.route.project_id
        && session.branch == request.branch()
        && is_identifier(&session.id)
        && is_identifier(&session.runtime.worker_instance_id)
        && generation.as_ref().is_ok_and(|value| *value > 0);
    if !identity_valid {
        let cause = match generation {
            Err(error) => Some(Box::new(error) as BoxError),
            Ok(_) => None,
        };
        return Err(Error::workflow("session_identity_invalid", true, cause));
    }
    let identity = SessionIdentity {
        host: request.route.host.clone(),
        session_id: session.id.clone(),
        worker_instance_id: session.runtime.worker_instance_id.clone(),
        runtime_generation: session.runtime.runtime_generation.clone(),
        reviewer_id: request.reviewer.id.clone(),
    };
    if session.state != "running" || !session.is_linked_worktree || session.runtime.state != "live"
    {
        return Ok(Lookup::Unhealthy(
            identity,
            Error::workflow("session_identity_invalid", true, None),
        ));
    }
    let Some((first, rest)) = session.warnings.split_first() else {
        return Ok(Lookup::Owned(identity));
    };
    for warning in &session.warnings {
        if warning.kind != "fetch"
            || warning.message.trim().is_empty()
            || warning.detail.trim().is_empty()
        {
            return Ok(Lookup::Unhealthy(
                identity,
                Error::workflow("pohunek_session_warning", true, None),
            ));
        }
    }
    let (mut code, mut permanent) = fetch_failure(&first.detail);
    for warning in rest {
        let (next_code, next_permanent) = fetch_failure(&warning.detail);
        if next_permanent || code == "pohunek_base_fetch_failed" {
            code = next_code;
            permanent = next_permanent;
        }
    }
    Ok(Lookup::Unhealthy(
        identity,
        Error::workflow(code, permanent, None),
    ))
}

/// Returns the session of `request` as the session list reports it.
pub(crate) async fn find_session(
    config: &Config,
    cancel: &CancellationToken,
    request: &SessionRequest,
) -> Result<Lookup, Error> {
    let output = pohunek_call(config, cancel, &session_list_arguments(&request.route.host)).await?;
    lookup_from_output(&output, request)
}

/// Returns the identity of the reviewer session of `request`, starting the session when absent.
///
/// `remove` removes a session and confirms that it is gone; it is called only on a session that
/// this request owns and that cannot be used. The daemon may accept a start after the client
/// timed out, so the session list is read again after every start. That read ignores `cancel`,
/// as Go reads it with `context.WithoutCancel`, so a start the daemon accepted is still found.
pub(crate) async fn ensure_session<R, F>(
    config: &Config,
    cancel: &CancellationToken,
    request: &SessionRequest,
    remove: &R,
) -> Result<SessionIdentity, Error>
where
    R: Fn(SessionIdentity) -> F,
    F: Future<Output = Result<(), Error>>,
{
    match find_session(config, cancel, request).await {
        Ok(Lookup::Owned(identity)) => return Ok(identity),
        Ok(Lookup::Absent) => {}
        Ok(Lookup::Unhealthy(identity, cause)) => {
            return Err(reject(remove, Some(identity), cause).await);
        }
        Err(cause) => return Err(reject(remove, None, cause).await),
    }
    let timeout_ms = i64::try_from(config.pohunek_timeout.as_millis()).map_err(|error| {
        Error::config(
            "configuration_invalid",
            POHUNEK_TIMEOUT_KEY,
            Some(Box::new(error)),
        )
    })?;
    let arguments = start_arguments(
        request,
        &config.session_group,
        timeout_ms,
        config.terminal_cols,
        config.terminal_rows,
    )?;
    let environment = pohunek_environment(config, std::env::vars_os());
    let started = run(
        cancel,
        &config.pohunek_bin,
        &arguments,
        request.prompt.as_bytes(),
        &environment,
        config.pohunek_timeout,
    )
    .await;
    match find_session(config, &CancellationToken::new(), request).await {
        Ok(Lookup::Owned(identity)) => return Ok(identity),
        Ok(Lookup::Unhealthy(identity, cause)) => {
            return Err(reject(remove, Some(identity), cause).await);
        }
        Err(cause) => return Err(cause),
        Ok(Lookup::Absent) => {}
    }
    match started {
        Err(error) => {
            let output = error.stdout().to_owned();
            let code = failure_code(&output);
            let cause: BoxError = if code.is_empty() {
                Box::new(error)
            } else {
                Box::new(Error::pohunek_code(code, Box::new(error)))
            };
            Err(Error::workflow(
                "pohunek_session_start_failed",
                false,
                Some(cause),
            ))
        }
        Ok(_) => Err(response_invalid()),
    }
}

/// Returns the error of a failed request: `cause`, or the failed removal of an owned session.
async fn reject<R, F>(remove: &R, session: Option<SessionIdentity>, cause: Error) -> Error
where
    R: Fn(SessionIdentity) -> F,
    F: Future<Output = Result<(), Error>>,
{
    let Some(session) = session else {
        return cause;
    };
    match remove(session).await {
        Ok(()) => cause,
        Err(cleanup) => Error::workflow(
            "pohunek_session_cleanup_failed",
            false,
            Some(Box::new(JoinedError { cause, cleanup })),
        ),
    }
}

/// A failed removal together with the failure it followed, as Go's `errors.Join` reports both.
#[derive(Debug)]
struct JoinedError {
    /// The failure that made the session unusable.
    cause: Error,
    /// The failed removal.
    cleanup: Error,
}

impl fmt::Display for JoinedError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}\n{}", self.cause, self.cleanup)
    }
}

impl StdError for JoinedError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(&self.cause)
    }
}

#[cfg(test)]
mod tests {
    use autoreview_engine::integrations::SessionRequest;
    use autoreview_engine::model::{ProjectRoute, PullRequest};
    use autoreview_policy::{Checkout, Protocol, Reviewer};

    use super::start_arguments;

    #[test]
    fn review_sessions_start_in_the_companion_group() {
        let head = "a".repeat(40);
        let request = SessionRequest {
            pull_request: PullRequest {
                repository: "acme/widget".to_owned(),
                number: 1,
                url: "https://github.com/acme/widget/pull/1".to_owned(),
                head_branch: "feature".to_owned(),
                head_sha: head,
                ..PullRequest::default()
            },
            route: ProjectRoute {
                host: "host".to_owned(),
                project_id: "project".to_owned(),
            },
            reviewer: Reviewer {
                id: "opus".to_owned(),
                label: "Opus".to_owned(),
                protocol: Protocol::ClaudeStructured,
                agent_profile: "claude-profile".to_owned(),
                finding_prefix: "O".to_owned(),
            },
            checkout: Checkout::Base,
            workflow_id: "prr-v1-abc".to_owned(),
            run: 1,
            ..SessionRequest::default()
        };
        let arguments =
            start_arguments(&request, "autoreview", 1000, 120, 100).expect("valid session start");
        assert!(
            arguments
                .windows(2)
                .any(|pair| pair[0] == "--meta" && pair[1] == "companion.group=autoreview"),
            "arguments = {arguments:?}"
        );
    }
}
