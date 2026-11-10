//! The two read-only questions that `profiles install` asks the local pohunek CLI.

use std::process::{Command, Stdio};

use autoreview_engine::{BoxError, Error};
use serde::Deserialize;

use super::{
    Agent, POHUNEK_COMMAND_FAILED, PROFILE_HOST_NOT_LOCAL, Pin, RUNTIME_PACKAGE_MISSING,
    profile_name_valid,
};

/// The pohunek CLI, found on `PATH` as a shell finds it.
const POHUNEK_PROGRAM: &str = "pohunek";

/// The host name that pohunek resolves to this machine's daemon.
const LOCAL_HOST: &str = "local";

/// The origin of a package that pohunek authorized through its signed catalog.
const OFFICIAL_ORIGIN: &str = "official";

/// The prefix of a package digest.
const DIGEST_PREFIX: &str = "sha256:";

/// Length of the hex text of a SHA-256 digest.
const DIGEST_HEX_LENGTH: usize = 64;

/// The longest pohunek error code that a failed call may report.
const ERROR_CODE_MAX_LENGTH: usize = 64;

/// The process envelope of a pohunek `--json` answer; a missing or `null` `ok` is refused.
#[derive(Debug, Deserialize)]
struct Envelope<T> {
    /// The result of the call.
    ok: Option<T>,
}

/// The envelope of a failed pohunek `--json` call.
#[derive(Debug, Deserialize)]
struct FailureEnvelope {
    /// The error of the call.
    err: FailureDetail,
}

/// The error object of a failed pohunek call.
#[derive(Debug, Deserialize)]
struct FailureDetail {
    /// The error code that pohunek reports.
    code: String,
}

/// The part of `host governance inspect --json` that identifies a daemon.
#[derive(Debug, Deserialize)]
struct Governance {
    /// The stable identity of the daemon's host.
    host_id: String,
}

/// The part of `plugin list --json` that names the installed packages.
#[derive(Debug, Deserialize)]
struct PackageList {
    /// Every installed package.
    packages: Vec<Package>,
}

/// One installed package of `plugin list --json`.
#[derive(Debug, Deserialize)]
struct Package {
    /// Archive digest of the package.
    digest: String,
    /// Package id and version.
    package: PackageIdentity,
    /// How pohunek authorized the package.
    origin: String,
    /// Whether fresh launches may use the package.
    enabled: bool,
    /// Whether bare requests of the package id resolve to this version.
    selected: bool,
    /// The runtime the package serves; absent when its descriptor cannot be loaded.
    runtime_id: Option<String>,
    /// Why the package cannot serve; absent when it is healthy.
    fault: Option<serde::de::IgnoredAny>,
}

/// The id and version of a package.
#[derive(Debug, Deserialize)]
struct PackageIdentity {
    /// Package id, for example `pohunek.runtime.claude`.
    id: String,
}

/// Refuses a `host` that does not name the daemon of this machine.
///
/// Both names are looked up with `pohunek host governance inspect`, which changes nothing; the
/// host is local when its `host_id` equals the one of `local`.
///
/// # Errors
///
/// Returns `pohunek_command_failed` when `local` cannot be inspected, and
/// `profile_host_not_local` naming `--host` when `host` cannot be inspected or is another host.
pub(super) fn check_local(host: &str) -> Result<(), Error> {
    let arguments = ["host", "governance", "inspect", LOCAL_HOST, "--json"];
    let local: Governance = pohunek_json(&arguments)?;
    if host == LOCAL_HOST {
        return Ok(());
    }
    let named: Governance = pohunek_json(&["host", "governance", "inspect", host, "--json"])
        .map_err(|error| Error::config(PROFILE_HOST_NOT_LOCAL, "--host", Some(Box::new(error))))?;
    if named.host_id != local.host_id {
        return Err(Error::config(PROFILE_HOST_NOT_LOCAL, "--host", None));
    }
    Ok(())
}

/// Returns the package that serves `agent` on this host, from `pohunek plugin list --json`.
///
/// The package must be official, enabled, selected and healthy, and serve the runtime `agent`.
///
/// # Errors
///
/// Returns `pohunek_command_failed`, and `runtime_package_missing` naming the runtime when not
/// exactly one package qualifies.
pub(super) fn package_pin(agent: Agent) -> Result<Pin, Error> {
    let list: PackageList = pohunek_json(&["plugin", "list", "--json"])?;
    let mut serving = list.packages.into_iter().filter(|package| {
        package.origin == OFFICIAL_ORIGIN
            && package.enabled
            && package.selected
            && package.fault.is_none()
            && package.runtime_id.as_deref() == Some(agent.base())
    });
    let (Some(package), None) = (serving.next(), serving.next()) else {
        return Err(Error::config(RUNTIME_PACKAGE_MISSING, agent.base(), None));
    };
    if !digest_valid(&package.digest) || !profile_name_valid(&package.package.id) {
        return Err(pohunek_failed("pohunek plugin list --json", None));
    }
    Ok(Pin {
        package: package.package.id,
        digest: package.digest,
    })
}

/// Reports whether `digest` is `sha256:` followed by 64 lowercase hex digits.
fn digest_valid(digest: &str) -> bool {
    digest.strip_prefix(DIGEST_PREFIX).is_some_and(|hex| {
        hex.len() == DIGEST_HEX_LENGTH
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// Runs the pohunek CLI with `arguments` and returns the `ok` value of its JSON envelope.
///
/// The child inherits the caller's environment, so it reaches the caller's own daemon.
///
/// # Errors
///
/// Returns `pohunek_command_failed` naming the command when it cannot start, exits non-zero or
/// prints something else than the expected JSON. After a non-zero exit, the cause carries the
/// error code of the `err` envelope on stdout when the code is well-formed.
fn pohunek_json<T: for<'de> Deserialize<'de>>(arguments: &[&str]) -> Result<T, Error> {
    let command = format!("{POHUNEK_PROGRAM} {}", arguments.join(" "));
    let output = Command::new(POHUNEK_PROGRAM)
        .args(arguments)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|error| pohunek_failed(&command, Some(Box::new(error))))?;
    if !output.status.success() {
        let cause: BoxError = format!("exited with {}", output.status).into();
        let cause = match failure_code(&output.stdout) {
            Some(code) => Box::new(Error::pohunek_code(code, cause)),
            None => cause,
        };
        return Err(pohunek_failed(&command, Some(cause)));
    }
    let envelope: Envelope<T> = serde_json::from_slice(&output.stdout)
        .map_err(|error| pohunek_failed(&command, Some(Box::new(error))))?;
    envelope
        .ok
        .ok_or_else(|| pohunek_failed(&command, Some("the answer holds no ok value".into())))
}

/// Returns the error code of the `err` envelope in `stdout`, when it matches
/// `^[a-z][a-z0-9_]{0,63}$`.
///
/// The rule is the one of the engine's pohunek calls (`failure_code` in the `external` crate),
/// which that crate keeps private to its runner; this command runs pohunek itself.
fn failure_code(stdout: &[u8]) -> Option<String> {
    let envelope: FailureEnvelope = serde_json::from_slice(stdout).ok()?;
    let code = envelope.err.code;
    let mut bytes = code.bytes();
    let well_formed = code.len() <= ERROR_CODE_MAX_LENGTH
        && bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
    well_formed.then_some(code)
}

/// Returns the error of a pohunek CLI call that failed.
fn pohunek_failed(command: &str, cause: Option<BoxError>) -> Error {
    Error::config(POHUNEK_COMMAND_FAILED, command, cause)
}
