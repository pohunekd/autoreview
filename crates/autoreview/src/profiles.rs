//! The `profiles install` command: writes the pohunek agent profile of a reviewer on this host.
//!
//! It needs no configuration. It asks the local pohunek CLI two read-only questions through the
//! engine's pohunek runner, writes the reviewer adapter embedded in this binary and the profile
//! that starts it.

// Rust guideline compliant 2026-11-10

mod files;
mod pohunek;
mod render;

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use autoreview_engine::Error;
use autoreview_engine::config::identifier_valid;
use clap::{Args, Subcommand, ValueEnum};
use rustix::fs::{Access, access};
use serde::Serialize;

use crate::{CONFIGURATION_INVALID, command_invalid};

/// Error code of a `--host` that does not name the pohunek daemon of this machine.
pub(crate) const PROFILE_HOST_NOT_LOCAL: &str = "profile_host_not_local";

/// Error code of a file of that name that `profiles install` did not write.
pub(crate) const PROFILE_FOREIGN: &str = "profile_foreign";

/// Error code of a path that cannot be checked, created or written.
pub(crate) const PROFILE_WRITE_FAILED: &str = "profile_write_failed";

/// Error code of a base runtime that no selected official package serves on this host.
pub(crate) const RUNTIME_PACKAGE_MISSING: &str = "runtime_package_missing";

/// Error code of a pohunek CLI call that failed or printed an unexpected answer.
pub(crate) const POHUNEK_COMMAND_FAILED: &str = "pohunek_command_failed";

/// The subcommands of `profiles`.
#[derive(Debug, Subcommand)]
pub(crate) enum ProfilesCommand {
    /// Writes the pohunek agent profile of a reviewer on this host.
    Install(InstallArguments),
}

/// The agent runtimes that a reviewer profile can start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum Agent {
    /// Claude Code, for `claude-structured` reviewers.
    Claude,
    /// Codex, for `codex-envelope` reviewers.
    Codex,
}

impl Agent {
    /// Returns the pohunek runtime id that the profile names as its `base`.
    pub(crate) fn base(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

/// The effort levels of `--effort`; Codex accepts all but `max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum Effort {
    /// The lowest level.
    Low,
    /// The middle level.
    Medium,
    /// A high level.
    High,
    /// A higher level.
    Xhigh,
    /// The highest level, Claude Code only.
    Max,
}

impl Effort {
    /// Returns the level as the adapter receives it.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

/// The arguments of `profiles install`; every value is required, none has a default.
#[derive(Debug, Args)]
pub(crate) struct InstallArguments {
    /// The pohunek host name of this machine, as the configuration lists it.
    #[arg(long, value_name = "HOST")]
    pub(crate) host: String,
    /// The agent runtime the profile starts.
    #[arg(long, value_enum, value_name = "AGENT")]
    pub(crate) agent: Agent,
    /// Name of the profile, as the policy's `agent_profile` names it.
    #[arg(long, value_name = "NAME")]
    pub(crate) name: String,
    /// The model the review run uses.
    #[arg(long, value_name = "MODEL")]
    pub(crate) model: String,
    /// The effort level of the review run.
    #[arg(long, value_enum, value_name = "EFFORT")]
    pub(crate) effort: Effort,
    /// Absolute path of the Claude Code or Codex executable that the adapter runs.
    #[arg(long, value_name = "PATH")]
    pub(crate) agent_binary: PathBuf,
    /// The largest request the adapter accepts, in bytes.
    #[arg(long, value_name = "BYTES", value_parser = clap::value_parser!(u64).range(1..))]
    pub(crate) maximum_input_bytes: u64,
    /// The largest report the Codex adapter prints, in bytes; Codex only.
    #[arg(long, value_name = "BYTES", value_parser = clap::value_parser!(u64).range(1..))]
    pub(crate) maximum_result_bytes: Option<u64>,
    /// How long the Codex adapter stays alive after its report; Codex only.
    #[arg(long, value_name = "SECONDS", value_parser = clap::value_parser!(u64).range(1..))]
    pub(crate) maximum_retention_seconds: Option<u64>,
    /// Seconds after which each pohunek call is killed, as `pohunek.timeout_seconds`.
    #[arg(long, value_name = "SECONDS", value_parser = clap::value_parser!(u64).range(1..))]
    pub(crate) pohunek_timeout_seconds: u64,
    /// Overwrite a file of that name that this command did not write.
    #[arg(long)]
    pub(crate) replace: bool,
}

/// What `profiles install` did to the profile and the files it starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Outcome {
    /// No profile of that name existed; it was written.
    Created,
    /// Every file already held what this command writes; nothing was written.
    Unchanged,
    /// A profile written by this command, or one of its files, was rewritten.
    Updated,
    /// Another file of that name was overwritten because `--replace` was given.
    Replaced,
}

/// What `profiles install` prints after it wrote or kept the profile.
#[derive(Debug, Serialize)]
pub(crate) struct InstallResult {
    /// The host, as given.
    host: String,
    /// The profile name, as given.
    name: String,
    /// The absolute path of the profile file.
    path: String,
    /// The absolute path of the adapter that the profile starts.
    program: String,
    /// The hex SHA-256 of the adapter, to compare with the file of the release tag.
    adapter_sha256: String,
    /// What was done to the files.
    result: Outcome,
}

/// The package that a profile pins: the pohunek package id and its archive digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pin {
    /// Package id, for example `pohunek.runtime.claude`.
    pub(crate) package: String,
    /// Archive digest, `sha256:` followed by 64 lowercase hex digits.
    pub(crate) digest: String,
}

/// The checked values that the rendered profile passes to its adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Settings {
    /// The model of the review run.
    pub(crate) model: String,
    /// The effort level of the review run.
    pub(crate) effort: Effort,
    /// Absolute path of the agent executable, as given.
    pub(crate) agent_binary: String,
    /// The largest request the adapter accepts, in bytes.
    pub(crate) maximum_input_bytes: u64,
    /// The runtime and the values that only it takes.
    pub(crate) runtime: Runtime,
}

/// The runtime of a profile, with the values that only the Codex adapter takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Runtime {
    /// The Claude adapter.
    Claude,
    /// The Codex adapter.
    Codex {
        /// The largest report the adapter prints, in bytes.
        maximum_result_bytes: u64,
        /// How long the adapter stays alive after its report, in seconds.
        maximum_retention_seconds: u64,
    },
}

impl Runtime {
    /// Returns the agent runtime of the profile.
    pub(crate) fn agent(self) -> Agent {
        match self {
            Self::Claude => Agent::Claude,
            Self::Codex { .. } => Agent::Codex,
        }
    }
}

/// Runs `profiles install` and returns what it did.
///
/// # Errors
///
/// Returns `command_invalid` when `--config` is given or a flag does not fit the agent,
/// `configuration_invalid` naming a flag or a variable, `pohunek_command_failed`,
/// `profile_host_not_local`, `runtime_package_missing`, `profile_foreign` and
/// `profile_write_failed`.
pub(crate) async fn run(
    config_path: Option<&Path>,
    command: &ProfilesCommand,
) -> Result<InstallResult, Error> {
    let ProfilesCommand::Install(arguments) = command;
    if config_path.is_some() {
        return Err(command_invalid(Some(
            "--config is not an option of profiles; it writes a host profile".into(),
        )));
    }
    let settings = settings(arguments)?;
    let home = std::env::var_os("HOME");
    let agents = agents_directory(
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        home.as_deref(),
    )?;
    let adapters = adapters_directory(
        std::env::var_os("XDG_DATA_HOME").as_deref(),
        home.as_deref(),
    )?
    .join(&arguments.name);
    let timeout = Duration::from_secs(arguments.pohunek_timeout_seconds);
    pohunek::check_local(&arguments.host, timeout).await?;
    let pin = pohunek::package_pin(settings.runtime.agent(), timeout).await?;
    let plan = render::plan(&arguments.name, &pin, &settings, &agents, &adapters)?;
    let result = files::install(&plan, arguments.replace)?;
    Ok(InstallResult {
        host: arguments.host.clone(),
        name: arguments.name.clone(),
        path: plan.profile.to_string_lossy().into_owned(),
        program: plan.program.clone(),
        adapter_sha256: plan.adapter_sha256.clone(),
        result,
    })
}

/// Checks the flags of `arguments` and returns the values the profile passes to its adapter.
///
/// # Errors
///
/// Returns `command_invalid` for a Codex flag given with `--agent claude` or missing with
/// `--agent codex`, and `configuration_invalid` naming the flag whose value is refused.
pub(crate) fn settings(arguments: &InstallArguments) -> Result<Settings, Error> {
    let runtime = match (
        arguments.agent,
        arguments.maximum_result_bytes,
        arguments.maximum_retention_seconds,
    ) {
        (Agent::Claude, None, None) => Runtime::Claude,
        (Agent::Claude, _, _) => {
            return Err(command_invalid(Some(
                "--maximum-result-bytes and --maximum-retention-seconds are Codex options".into(),
            )));
        }
        (Agent::Codex, Some(maximum_result_bytes), Some(maximum_retention_seconds)) => {
            Runtime::Codex {
                maximum_result_bytes,
                maximum_retention_seconds,
            }
        }
        (Agent::Codex, _, _) => {
            return Err(command_invalid(Some(
                "Codex needs --maximum-result-bytes and --maximum-retention-seconds".into(),
            )));
        }
    };
    if !profile_name_valid(&arguments.name) {
        return Err(invalid_flag(
            "--name",
            "must match [A-Za-z0-9][A-Za-z0-9._-]* without ..",
        ));
    }
    if !identifier_valid(&arguments.host) {
        return Err(invalid_flag(
            "--host",
            "must match [A-Za-z0-9][A-Za-z0-9._-]*",
        ));
    }
    if !identifier_valid(&arguments.model) {
        return Err(invalid_flag(
            "--model",
            "must match [A-Za-z0-9][A-Za-z0-9._-]*",
        ));
    }
    if runtime.agent() == Agent::Codex && arguments.effort == Effort::Max {
        return Err(invalid_flag(
            "--effort",
            "Codex accepts low, medium, high or xhigh",
        ));
    }
    let agent_binary = agent_binary(&arguments.agent_binary)
        .map_err(|reason| invalid_flag("--agent-binary", reason))?;
    Ok(Settings {
        model: arguments.model.clone(),
        effort: arguments.effort,
        agent_binary,
        maximum_input_bytes: arguments.maximum_input_bytes,
        runtime,
    })
}

/// Returns `path` as text when it names, by an absolute path, a regular file that this user
/// may execute.
///
/// The adapter reads the path back from a one-line file, and the profile quotes it, so a
/// control character is refused. A symbolic link is followed, as the adapter follows it. The
/// execute check is the `access(X_OK)` call that the adapter's `os.access(X_OK)` makes, so the
/// kernel decides it from the owner, the groups including supplementary ones, and any ACL.
///
/// # Errors
///
/// Returns the reason why the path is refused.
pub(crate) fn agent_binary(path: &Path) -> Result<String, &'static str> {
    let Some(text) = path.to_str() else {
        return Err("must be UTF-8");
    };
    if !path.is_absolute() {
        return Err("must be an absolute path");
    }
    if text.chars().any(char::is_control) {
        return Err("must not contain a control character");
    }
    let metadata = fs::metadata(path).map_err(|_error| "cannot be read")?;
    if !metadata.is_file() {
        return Err("must be a regular file");
    }
    access(path, Access::EXEC_OK).map_err(|_error| "must be executable by this user")?;
    Ok(text.to_owned())
}

/// Reports whether `name` is a profile name that pohunek accepts and that names one file.
///
/// The rule is pohunek's `validate_name`: a first character other than `.` and `-`, then only
/// ASCII letters, digits, `.`, `_` and `-`, and no `..`.
pub(crate) fn profile_name_valid(name: &str) -> bool {
    identifier_valid(name) && !name.contains("..")
}

/// Returns the agents directory of the pohunek daemon from `XDG_CONFIG_HOME` and `HOME`.
///
/// pohunek reads profiles from `$XDG_CONFIG_HOME/pohunek/agents`, or from
/// `$HOME/.config/pohunek/agents` when `XDG_CONFIG_HOME` is unset. pohunek refuses a set but
/// empty `XDG_CONFIG_HOME`, and a path with an empty, `.` or `..` part or a trailing `/`, so
/// this does too.
///
/// # Errors
///
/// Returns `configuration_invalid` naming the variable that is empty, relative, not UTF-8,
/// holds a control character, or has an empty, `.` or `..` part or a trailing `/`. When the
/// variable is unset, it names `HOME` if `HOME` is unset, empty or refused the same way.
pub(crate) fn agents_directory(
    xdg_config_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Result<PathBuf, Error> {
    let base = base_directory(("XDG_CONFIG_HOME", xdg_config_home), home, ".config")?;
    Ok(base.join("pohunek").join("agents"))
}

/// Returns the directory of the installed adapters from `XDG_DATA_HOME` and `HOME`.
///
/// The adapters live in `$XDG_DATA_HOME/autoreview/adapters`, or in
/// `$HOME/.local/share/autoreview/adapters` when `XDG_DATA_HOME` is unset. A set but empty
/// `XDG_DATA_HOME`, or a path with an empty, `.` or `..` part or a trailing `/`, is refused,
/// as pohunek refuses it.
///
/// # Errors
///
/// Returns `configuration_invalid` as [`agents_directory`] does.
pub(crate) fn adapters_directory(
    xdg_data_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Result<PathBuf, Error> {
    let base = base_directory(("XDG_DATA_HOME", xdg_data_home), home, ".local/share")?;
    Ok(base.join("autoreview").join("adapters"))
}

/// Returns the XDG base directory: the variable when it is set, else `fallback` under `HOME`.
///
/// A set but empty variable is refused: pohunek refuses it rather than use the default. The
/// value used, the variable or `HOME`, must have clean parts, as [`path_parts_clean`] checks.
fn base_directory(
    (name, value): (&'static str, Option<&OsStr>),
    home: Option<&OsStr>,
    fallback: &str,
) -> Result<PathBuf, Error> {
    let (variable, root, base) = match (value, home) {
        (Some(value), _) if value.is_empty() => {
            return Err(invalid_flag(
                name,
                "must not be empty when set; unset it for the default under HOME",
            ));
        }
        (Some(value), _) => (name, value, PathBuf::from(value)),
        (None, Some(value)) if !value.is_empty() => {
            ("HOME", value, Path::new(value).join(fallback))
        }
        (None, _) => {
            return Err(invalid_flag(
                "HOME",
                "must be set to find the pohunek agents",
            ));
        }
    };
    let usable = base
        .to_str()
        .is_some_and(|text| !text.chars().any(char::is_control));
    if !base.is_absolute() || !usable {
        return Err(invalid_flag(
            variable,
            "must be an absolute UTF-8 path without control characters",
        ));
    }
    // The raw value is checked: `Path::join` drops a trailing `/` of `HOME`.
    if !root.to_str().is_some_and(path_parts_clean) {
        return Err(invalid_flag(
            variable,
            "must not have an empty, '.' or '..' part or a trailing '/'",
        ));
    }
    Ok(base)
}

/// Reports whether `text` is `/` followed by parts that are neither empty, `.` nor `..`.
///
/// This is pohunek's rule for an XDG home: no `//`, no `.` or `..` part, no trailing `/`.
fn path_parts_clean(text: &str) -> bool {
    text.strip_prefix('/')
        .is_some_and(|rest| rest.split('/').all(|part| !matches!(part, "" | "." | "..")))
}

/// Returns the configuration error of a flag or variable value that the command refuses.
fn invalid_flag(flag: &str, reason: &'static str) -> Error {
    Error::config(CONFIGURATION_INVALID, flag, Some(reason.into()))
}

#[cfg(test)]
mod tests;
