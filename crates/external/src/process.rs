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
/// homes and `HOME`. Go reads the proxy and CA variables. `GH_TELEMETRY` and `DO_NOT_TRACK`
/// keep the operator's telemetry choice.
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
        /// Go's text of the exit status, for example `exit status 1`.
        status: String,
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
/// when `timeout` elapses or `cancel` is cancelled, and never started once `cancel` is cancelled,
/// as Go's `exec.CommandContext` does.
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
            stderr: String::from_utf8_lossy(&stderr).trim().to_string(),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
        });
    }
    // A child may exit without reading its input; Go ignores that broken pipe too.
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

/// Renders an unsuccessful `status`: Go's `exit status N` for an exit code.
///
/// A child ended by a signal keeps Rust's text, for example `signal: 9 (SIGKILL)`.
fn status_text(status: ExitStatus) -> String {
    if let Some(code) = status.code() {
        format!("exit status {code}")
    } else {
        status.to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    use super::{GH_INHERITED, POHUNEK_INHERITED, child_environment};

    /// Every variable that a child may inherit, then variables that no child inherits.
    const PARENT: [&str; 40] = [
        "PATH",
        "HOME",
        "TMPDIR",
        "XDG_CONFIG_HOME",
        "XDG_STATE_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
        "XDG_RUNTIME_DIR",
        "POHUNEK_REMOTE_PORT",
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
        "SHARED_TOOL_TOKEN",
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "GH_ENTERPRISE_TOKEN",
        "GITHUB_ENTERPRISE_TOKEN",
        "MATRIX_ACCESS_TOKEN",
        "AUTOREVIEW_GH_TOKEN",
        "POHUNEK_SESSION_ID",
        "POHUNEK_DAEMON_ID",
        "GH_HOST",
        "GH_DEBUG",
        "GH_REPO",
        "PAGER",
        "LANG",
        "TZ",
        "DBUS_SESSION_BUS_ADDRESS",
        "SSH_AUTH_SOCK",
        "path",
        "PATHX",
        "HOME_X",
    ];

    fn pairs(items: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        items
            .iter()
            .map(|(name, value)| (OsString::from(name), OsString::from(value)))
            .collect()
    }

    fn names(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|name| (*name).to_owned()).collect()
    }

    /// Returns the parent's value of `name`: bytes that are not UTF-8, which a child gets as is.
    fn parent_value(name: &str) -> OsString {
        let mut bytes = b"parent-\xff-".to_vec();
        bytes.extend_from_slice(name.as_bytes());
        OsString::from_vec(bytes)
    }

    /// One allowlist, the settings its caller adds and what a child keeps of [`PARENT`].
    struct Case {
        /// The allowlist under test.
        inherited: &'static [&'static str],
        /// The settings the caller adds.
        extra: &'static [(&'static str, &'static str)],
        /// The parent variables the child keeps, in the parent's order.
        kept: &'static [&'static str],
    }

    #[test]
    fn child_environment_passes_only_the_inherited_variables() {
        let cases = [
            Case {
                inherited: &GH_INHERITED,
                extra: &[
                    ("GH_TOKEN", "child-token"),
                    ("GH_PAGER", "cat"),
                    ("GH_PROMPT_DISABLED", "1"),
                ],
                kept: &[
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
                ],
            },
            Case {
                inherited: &POHUNEK_INHERITED,
                extra: &[
                    ("XDG_STATE_HOME", "/var/lib/engine/state"),
                    ("XDG_RUNTIME_DIR", "/run/engine"),
                    ("PAGER", "cat"),
                ],
                kept: &[
                    "PATH",
                    "HOME",
                    "TMPDIR",
                    "XDG_CONFIG_HOME",
                    "XDG_DATA_HOME",
                    "XDG_CACHE_HOME",
                    "POHUNEK_REMOTE_PORT",
                ],
            },
        ];
        for Case {
            inherited,
            extra,
            kept,
        } in cases
        {
            let mut parent: Vec<(OsString, OsString)> = PARENT
                .iter()
                .map(|name| (OsString::from(name), parent_value(name)))
                .collect();
            parent.push((
                OsString::from_vec(b"PATH\xff".to_vec()),
                parent_value("PATH"),
            ));
            let mut expected: Vec<(OsString, OsString)> = kept
                .iter()
                .map(|name| (OsString::from(name), parent_value(name)))
                .collect();
            expected.extend(pairs(extra));

            assert_eq!(
                child_environment(parent, inherited, &BTreeSet::new(), pairs(extra)),
                expected,
                "allowlist {inherited:?}"
            );
        }
    }

    #[test]
    fn child_environment_drops_every_configured_secret() {
        // An allowlist that names a secret still drops it.
        let inherited = [
            "PATH",
            "AUTOREVIEW_GH_TOKEN",
            "AUTOREVIEW_STALE_TOKEN",
            "EXAMPLE_SECRET",
            "EXAMPLE_AUTOREVIEW_TOKEN",
            "autoreview_gh_token",
        ];
        let parent = pairs(&[
            ("PATH", "/usr/bin"),
            ("AUTOREVIEW_GH_TOKEN", "synthetic-gh"),
            ("AUTOREVIEW_MATRIX_TOKEN", "synthetic-matrix"),
            ("AUTOREVIEW_STALE_TOKEN", "synthetic-unnamed"),
            ("AUTOREVIEW_CONFIG", "/etc/autoreview/config.yaml"),
            ("MATRIX_ACCESS_TOKEN", "synthetic-go-matrix"),
            ("EXAMPLE_SECRET", "synthetic-named"),
            ("EXAMPLE_AUTOREVIEW_TOKEN", "kept"),
            ("autoreview_gh_token", "kept"),
        ]);
        let secrets = names(&[
            "AUTOREVIEW_GH_TOKEN",
            "AUTOREVIEW_MATRIX_TOKEN",
            "EXAMPLE_SECRET",
        ]);
        let extra = pairs(&[("GH_TOKEN", "synthetic-child"), ("PAGER", "cat")]);

        assert_eq!(
            child_environment(parent, &inherited, &secrets, extra),
            pairs(&[
                ("PATH", "/usr/bin"),
                ("EXAMPLE_AUTOREVIEW_TOKEN", "kept"),
                ("autoreview_gh_token", "kept"),
                ("GH_TOKEN", "synthetic-child"),
                ("PAGER", "cat"),
            ])
        );
    }
}
