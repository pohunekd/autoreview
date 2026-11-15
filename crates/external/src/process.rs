//! Child processes of the engine: the `gh` and pohunek CLIs.
//!
//! A child gets an argv list, never a shell, and only the variables of its program's allowlist
//! plus the settings its caller adds, so it sees no secret of the engine or of other tools.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::io;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use autoreview_engine::config::SECRET_ENV_PREFIX;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio_util::sync::CancellationToken;

/// Variables of the engine that a `gh` child inherits.
///
/// gh finds its config, state, data and cache directories through `GH_CONFIG_DIR`, the `XDG_*`
/// homes and `HOME`. The proxy and CA variables reach the HTTPS client of `gh`.
/// `GH_TELEMETRY` and `DO_NOT_TRACK` keep the operator's telemetry choice.
pub(crate) const GH_INHERITED: [&str; 18] = [
    "PATH",
    "HOME",
    "TMPDIR",
    "XDG_CONFIG_HOME",
    "XDG_STATE_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "GH_CONFIG_DIR",
    "HTTPS_PROXY",
    "https_proxy",
    "HTTP_PROXY",
    "http_proxy",
    "NO_PROXY",
    "no_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "GH_TELEMETRY",
    "DO_NOT_TRACK",
];

/// Variables of the engine that a pohunek child inherits.
///
/// The CLI finds its config, data, cache, state and daemon socket through the `XDG_*` homes and
/// `HOME`, dials remote daemons on `POHUNEK_REMOTE_PORT` and runs the overlay CLI from `PATH`.
pub(crate) const POHUNEK_INHERITED: [&str; 9] = [
    "PATH",
    "HOME",
    "TMPDIR",
    "XDG_CONFIG_HOME",
    "XDG_STATE_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "XDG_RUNTIME_DIR",
    "POHUNEK_REMOTE_PORT",
];

/// Variables of the engine that a git child inherits.
///
/// No `HOME`, `XDG_CONFIG_HOME` or `GIT_*` variable reaches it: git reads no user
/// configuration, and the repository is the one that `-C` names.
pub(crate) const GIT_INHERITED: [&str; 2] = ["PATH", "TMPDIR"];

/// Errors of a child process run.
///
/// Callers wrap it as the cause of a workflow failure with their own code.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ProcessError {
    /// The child could not be started, fed or awaited.
    #[error("{binary}: {source}")]
    Io {
        /// The program that was run.
        binary: String,
        /// The error of the failed operation.
        #[source]
        source: io::Error,
    },
    /// The child ran past its timeout and was killed.
    #[error("{binary}: timed out after {timeout:?}")]
    Timeout {
        /// The program that was run.
        binary: String,
        /// The timeout that elapsed.
        timeout: Duration,
        /// The child's stdout read until the timeout.
        stdout: String,
    },
    /// The caller's token was cancelled; a running child was killed.
    #[error("{binary}: context canceled")]
    Canceled {
        /// The program that was run.
        binary: String,
        /// The child's stdout read until the cancel; empty when it never started.
        stdout: String,
    },
    /// The child exited unsuccessfully.
    #[error("{status}: {stderr}")]
    Exit {
        /// The text of the exit status, for example `exit status 1`.
        status: String,
        /// The exit code, or `None` for a child ended by a signal.
        code: Option<i32>,
        /// The child's stderr, trimmed.
        stderr: String,
        /// The child's stdout.
        stdout: String,
    },
}

impl ProcessError {
    /// Returns the stdout the child wrote before the run failed.
    ///
    /// It is empty for a child that did not start or whose pipes failed.
    pub(crate) fn stdout(&self) -> &str {
        match self {
            Self::Exit { stdout, .. }
            | Self::Timeout { stdout, .. }
            | Self::Canceled { stdout, .. } => stdout,
            Self::Io { .. } => "",
        }
    }
}

/// Returns the environment of a child process.
///
/// It is the variables of `parent` whose name is in `inherited`, in the parent's order, followed
/// by `extra`. A parent variable is left out when `extra` sets its name, when `secret_variables`
/// holds it or when it starts with [`SECRET_ENV_PREFIX`]; names match exactly. A secret reaches
/// a child only when the caller passes it in `extra`.
pub(crate) fn child_environment(
    parent: impl IntoIterator<Item = (OsString, OsString)>,
    inherited: &[&str],
    secret_variables: &BTreeSet<String>,
    extra: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    let extra: Vec<(OsString, OsString)> = extra.into_iter().collect();
    let mut environment: Vec<(OsString, OsString)> = parent
        .into_iter()
        .filter(|(name, _)| {
            let name = name.as_os_str();
            let prefixed = name
                .as_encoded_bytes()
                .starts_with(SECRET_ENV_PREFIX.as_bytes());
            inherited.iter().any(|kept| name == OsStr::new(kept))
                && !prefixed
                && !secret_variables
                    .iter()
                    .any(|secret| name == OsStr::new(secret))
                && !extra.iter().any(|(set, _)| name == set.as_os_str())
        })
        .collect();
    environment.extend(extra);
    environment
}

/// Runs `binary` with `args` and returns its stdout.
///
/// The child reads `input` on stdin and gets exactly `environment`. It is killed
/// when `timeout` elapses or `cancel` is cancelled, and it is never started after `cancel`.
pub(crate) async fn run(
    cancel: &CancellationToken,
    binary: &str,
    args: &[String],
    input: &[u8],
    environment: &[(OsString, OsString)],
    timeout: Duration,
) -> Result<String, ProcessError> {
    let io_error = |source| ProcessError::Io {
        binary: binary.to_string(),
        source,
    };
    let not_piped = |stream: &str| io_error(io::Error::other(format!("{stream} is not piped")));
    if cancel.is_cancelled() {
        return Err(ProcessError::Canceled {
            binary: binary.to_string(),
            stdout: String::new(),
        });
    }
    let mut child = Command::new(binary)
        .args(args)
        .env_clear()
        .envs(environment.iter().map(|(name, value)| (name, value)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(io_error)?;
    let mut stdin = child.stdin.take().ok_or_else(|| not_piped("stdin"))?;
    let mut stdout_pipe = child.stdout.take().ok_or_else(|| not_piped("stdout"))?;
    let mut stderr_pipe = child.stderr.take().ok_or_else(|| not_piped("stderr"))?;
    let write_block = async move {
        let result = stdin.write_all(input).await;
        drop(stdin);
        result
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    // The buffers live outside the timed future, so a timeout keeps what was read.
    let finished = tokio::time::timeout(
        timeout,
        cancel.run_until_cancelled(async {
            tokio::join!(
                write_block,
                stdout_pipe.read_to_end(&mut stdout),
                stderr_pipe.read_to_end(&mut stderr),
                child.wait()
            )
        }),
    )
    .await;
    let (written, stdout_read, stderr_read, status) = match finished {
        Ok(Some(finished)) => finished,
        Ok(None) => {
            kill(&mut child).await.map_err(io_error)?;
            return Err(ProcessError::Canceled {
                binary: binary.to_string(),
                stdout: String::from_utf8_lossy(&stdout).into_owned(),
            });
        }
        Err(_) => {
            kill(&mut child).await.map_err(io_error)?;
            return Err(ProcessError::Timeout {
                binary: binary.to_string(),
                timeout,
                stdout: String::from_utf8_lossy(&stdout).into_owned(),
            });
        }
    };
    let status = status.map_err(io_error)?;
    stdout_read.map_err(io_error)?;
    stderr_read.map_err(io_error)?;
    if !status.success() {
        return Err(ProcessError::Exit {
            status: status_text(status),
            code: status.code(),
            stderr: String::from_utf8_lossy(&stderr).trim().to_string(),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
        });
    }
    // A child may exit without reading its input, so a broken pipe on stdin is ignored.
    if let Err(error) = written
        && error.kind() != io::ErrorKind::BrokenPipe
    {
        return Err(io_error(error));
    }
    Ok(String::from_utf8_lossy(&stdout).into_owned())
}

/// Kills a child that ran past its timeout or its token and reaps it.
///
/// A child that already exited, with its pipes still held open, is not killed again.
async fn kill(child: &mut Child) -> io::Result<()> {
    if child.try_wait()?.is_none() {
        child.start_kill()?;
        child.wait().await?;
    }
    Ok(())
}

/// Renders an unsuccessful `status` as `exit status N` for an exit code.
///
/// A child ended by a signal keeps Rust's text, for example `signal: 9 (SIGKILL)`.
fn status_text(status: ExitStatus) -> String {
    if let Some(code) = status.code() {
        format!("exit status {code}")
    } else {
        status.to_string()
    }
}
