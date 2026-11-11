//! The two read-only questions that `profiles install` asks the local pohunek CLI.
//!
//! Both run through the engine's pohunek runner (`autoreview_external::local_pohunek_json`).

use std::time::Duration;

use autoreview_engine::{BoxError, Error};
use autoreview_external::local_pohunek_json;
use serde::Deserialize;
use serde::de::DeserializeOwned;

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
/// host is local when its `host_id` equals the one of `local`. Each call is killed after
/// `timeout`.
///
/// # Errors
///
/// Returns `pohunek_command_failed` when `local` cannot be inspected, and
/// `profile_host_not_local` naming `--host` when `host` cannot be inspected or is another host.
pub(super) async fn check_local(host: &str, timeout: Duration) -> Result<(), Error> {
    let arguments = ["host", "governance", "inspect", LOCAL_HOST, "--json"];
    let local: Governance = pohunek_json(&arguments, timeout).await?;
    if host == LOCAL_HOST {
        return Ok(());
    }
    let named: Governance =
        pohunek_json(&["host", "governance", "inspect", host, "--json"], timeout)
            .await
            .map_err(|error| host_not_local(Some(Box::new(error))))?;
    if named.host_id != local.host_id {
        return Err(host_not_local(None));
    }
    Ok(())
}

/// Returns the package that serves `agent` on this host, from `pohunek plugin list --json`.
///
/// The package must be official, enabled, selected and healthy, and serve the runtime `agent`.
/// The call is killed after `timeout`.
///
/// # Errors
///
/// Returns `pohunek_command_failed`, and `runtime_package_missing` naming the runtime when not
/// exactly one package qualifies.
pub(super) async fn package_pin(agent: Agent, timeout: Duration) -> Result<Pin, Error> {
    let list: PackageList = pohunek_json(&["plugin", "list", "--json"], timeout).await?;
    let mut serving = list.packages.into_iter().filter(|package| {
        package.origin == OFFICIAL_ORIGIN
            && package.enabled
            && package.selected
            && package.fault.is_none()
            && package.runtime_id.as_deref() == Some(agent.base())
    });
    let (Some(package), None) = (serving.next(), serving.next()) else {
        return Err(package_missing(agent.base()));
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

/// Runs the pohunek CLI with `arguments` and returns the `ok` value of its JSON answer.
///
/// The child keeps the caller's state and runtime directories, so it reaches the caller's own
/// daemon, and is killed after `timeout`.
///
/// # Errors
///
/// Returns `pohunek_command_failed` naming the command when it cannot start, times out, exits
/// unsuccessfully or prints something else than the expected JSON. The cause carries the error
/// code of the `err` envelope when the output reports a well-formed one.
async fn pohunek_json<T: DeserializeOwned>(
    arguments: &[&str],
    timeout: Duration,
) -> Result<T, Error> {
    let command = format!("{POHUNEK_PROGRAM} {}", arguments.join(" "));
    let arguments: Vec<String> = arguments.iter().map(|item| (*item).to_owned()).collect();
    local_pohunek_json(POHUNEK_PROGRAM, &arguments, timeout)
        .await
        .map_err(|cause| pohunek_failed(&command, Some(cause)))
}

/// Returns the error of a `--host` that does not name the daemon of this machine.
fn host_not_local(cause: Option<BoxError>) -> Error {
    let name = "--host";
    let message =
        format!("{name} does not name this machine; run profiles install on the review host.");
    Error::config_with_message(PROFILE_HOST_NOT_LOCAL, name, message, cause)
}

/// Returns the error of the base runtime `name` that no selected official package serves.
fn package_missing(name: &str) -> Error {
    let message = format!(
        "No selected official pohunek package serves {name}. From the extracted pohunek \
release, run: pohunek plugin install runtime/packages/{name}.tar.zst --catalog \
runtime/runtime-catalog.json --yes"
    );
    Error::config_with_message(RUNTIME_PACKAGE_MISSING, name, message, None)
}

/// Returns the error of a pohunek CLI call that failed.
fn pohunek_failed(command: &str, cause: Option<BoxError>) -> Error {
    let message = format!("{command} failed; check the pohunek daemon.");
    Error::config_with_message(POHUNEK_COMMAND_FAILED, command, message, cause)
}
