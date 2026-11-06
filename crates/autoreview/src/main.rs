//! The `autoreview` command: its JSON on stdout, its logs on stderr.
//!
//! Mirrors `cmd/autoreview/main.go:1-224`.

// Rust guideline compliant 2026-06-26

mod config_file;
mod error_response;
mod logging;

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use autoreview_engine::config::Config;
use autoreview_engine::{BoxError, Error, WORKFLOW_FAILED, error_code, go_json};
use clap::error::ErrorKind;
use clap::{Parser, Subcommand};
use serde::Serialize;

use crate::error_response::{COMMAND_INVALID, error_response};

/// Environment variable that names the config file when `--config` is absent.
const CONFIG_ENV: &str = "AUTOREVIEW_CONFIG";

/// Configuration key named when the configuration holds no policy.
const POLICY_KEY: &str = "policy_file";

/// Error code of a configuration value that the command refuses.
const CONFIGURATION_INVALID: &str = "configuration_invalid";

/// Command line of `autoreview`.
#[derive(Debug, Parser)]
#[command(name = "autoreview", version, disable_help_subcommand = true)]
struct Cli {
    /// Path of `config.yaml`; `AUTOREVIEW_CONFIG` is read when it is absent.
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,
    /// The command to run.
    #[command(subcommand)]
    command: Command,
}

/// The commands of `autoreview`, as `cmd/autoreview/main.go:42-152` dispatches them.
#[derive(Debug, Subcommand)]
enum Command {
    /// Validates the configuration and the policy and prints the policy digest.
    Validate,
}

/// What `validate` prints: what the policy requires of the review hosts.
#[derive(Debug, Serialize)]
struct ValidationResult {
    /// Digest that identifies the effective policy.
    policy_digest: String,
    /// The pohunek agent profiles that the policy's reviewers run.
    agent_profiles: Vec<String>,
}

#[tokio::main]
async fn main() -> ExitCode {
    let result = match logging::init() {
        Ok(()) => match Cli::try_parse() {
            Ok(cli) => run(cli).await,
            Err(error) if is_display(error.kind()) => print_display(&error),
            Err(error) => Err(command_invalid(Some(Box::new(error)))),
        },
        Err(error) => Err(failed(error)),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            report(&error);
            ExitCode::FAILURE
        }
    }
}

/// Reports whether clap stopped for the help or the version text.
fn is_display(kind: ErrorKind) -> bool {
    kind == ErrorKind::DisplayHelp || kind == ErrorKind::DisplayVersion
}

/// Prints the help or the version text that clap rendered.
fn print_display(display: &clap::Error) -> Result<(), Error> {
    display.print().map_err(failed)
}

/// Prints the JSON error object of `error` on stdout and logs the failure.
fn report(error: &Error) {
    let response = error_response(error);
    // A failed write leaves only the log line; Go ignores `fmt.Fprintln` errors.
    if let Ok(text) = go_json(&response) {
        let mut stdout = io::stdout().lock();
        if writeln!(stdout, "{text}")
            .and_then(|()| stdout.flush())
            .is_err()
        {
            tracing::warn!("command error not printed");
        }
    }
    tracing::error!(error_code = error_code(error), error = %error, "command failed");
}

/// Loads the configuration and runs the command in the order of `main.go:31-152`.
async fn run(cli: Cli) -> Result<(), Error> {
    let environment = std::env::var_os(CONFIG_ENV);
    let path = config_file::resolve_path(cli.config.as_deref(), environment.as_deref())?;
    let config = config_file::load(&path)?;
    match cli.command {
        Command::Validate => validate(&config),
    }
}

/// Prints the policy digest and agent profiles of the loaded configuration.
fn validate(config: &Config) -> Result<(), Error> {
    let Some(policy) = config.policy.as_ref() else {
        return Err(Error::config(CONFIGURATION_INVALID, POLICY_KEY, None));
    };
    print_json(&ValidationResult {
        policy_digest: policy.digest().to_owned(),
        agent_profiles: policy.agent_profiles(),
    })
}

/// Prints `value` as Go's `json.Encoder` does: escaped as `json.Marshal`, then a newline.
///
/// # Errors
///
/// Returns `workflow_failed` when `value` cannot be encoded or stdout cannot be written.
fn print_json<T: Serialize + ?Sized>(value: &T) -> Result<(), Error> {
    let text = go_json(value).map_err(failed)?;
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{text}")
        .and_then(|()| stdout.flush())
        .map_err(failed)
}

/// Returns the `command_invalid` error of malformed arguments.
fn command_invalid(cause: Option<BoxError>) -> Error {
    Error::workflow(COMMAND_INVALID, true, cause)
}

/// Wraps a failure that carries no engine code, which Go reports as `workflow_failed`.
fn failed(cause: impl Into<BoxError>) -> Error {
    Error::workflow(WORKFLOW_FAILED, false, Some(cause.into()))
}
