//! The `autoreview` command: its JSON on stdout, its logs on stderr.
//!
//! Mirrors `cmd/autoreview/main.go:1-224`.

// Rust guideline compliant 2026-06-26

mod config_file;
mod error_response;
mod logging;

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use autoreview_engine::config::Config;
use autoreview_engine::integrations::Integrations;
use autoreview_engine::store::{ReviewFilter, Store};
use autoreview_engine::{
    BoxError, Engine, Error, SystemClock, WORKFLOW_FAILED, error_code, go_json,
};
use autoreview_external::External;
use clap::error::ErrorKind;
use clap::{CommandFactory, Parser, Subcommand};
use serde::Serialize;
use time::OffsetDateTime;
use tokio::signal::unix::{SignalKind, signal};
use tokio_util::sync::CancellationToken;

use crate::error_response::{COMMAND_INVALID, error_response};

/// Environment variable that names the config file when `--config` is absent.
const CONFIG_ENV: &str = "AUTOREVIEW_CONFIG";

/// The end-of-options marker of clap, which Go's argument reading does not know.
const ESCAPE: &str = "--";

/// Configuration key named when the configuration holds no policy.
const POLICY_KEY: &str = "policy_file";

/// Configuration key named when the token belongs to another login than the credential.
const LOGIN_KEY: &str = "github.credentials.login";

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
    /// Prints the pull requests that the next discovery pass would consider.
    Candidates,
    /// A command that runs on the store and the engine.
    #[command(flatten)]
    Engine(EngineCommand),
}

/// The commands that open the store and build the engine first.
#[derive(Debug, Subcommand)]
enum EngineCommand {
    /// Runs discovery and reviews until SIGINT or SIGTERM.
    Serve,
    /// Runs one discovery pass and prints what it queued.
    Poll,
    /// Prints the reviews that wait for a human approval.
    Awaiting,
    /// Sends the review queue digest and prints it.
    ReviewQueue,
    /// Prints the newest reviews.
    Reviews {
        /// Most reviews printed, from 1 to `cli.review_list_limit`.
        limit: Option<String>,
    },
    /// Writes a consistent copy of the store to a new absolute path.
    BackupDb {
        /// Absolute path of the copy; nothing may exist there yet.
        #[arg(allow_hyphen_values = true)]
        path: PathBuf,
    },
}

/// What `validate` prints: what the policy requires of the review hosts.
#[derive(Debug, Serialize)]
struct ValidationResult {
    /// Digest that identifies the effective policy.
    policy_digest: String,
    /// The pohunek agent profiles that the policy's reviewers run.
    agent_profiles: Vec<String>,
}

/// What `backup-db` prints after the copy was written.
#[derive(Debug, Serialize)]
struct BackupResult {
    /// Always `true`.
    ok: bool,
    /// The path of the copy, as given.
    path: String,
}

#[tokio::main]
async fn main() -> ExitCode {
    let result = match logging::init() {
        Ok(()) => match parse(std::env::args_os().collect()) {
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

/// Parses the command line and refuses the `--` escape, which Go reads as an argument.
///
/// # Errors
///
/// Returns the clap error; a `--` argument is an `UnknownArgument` error.
fn parse(arguments: Vec<OsString>) -> Result<Cli, clap::Error> {
    if arguments.iter().skip(1).any(|argument| argument == ESCAPE) {
        return Err(Cli::command().error(
            ErrorKind::UnknownArgument,
            format!("{ESCAPE} is not an argument of any command"),
        ));
    }
    Cli::try_parse_from(arguments)
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
    let external = Arc::new(External::new(config.clone())?);
    let cancel = CancellationToken::new();
    cancel_on_signal(cancel.clone())?;
    match cli.command {
        Command::Validate => validate(&config),
        Command::Candidates => print_json(&external.list_candidates(&cancel).await?),
        Command::Engine(command) => run_engine(command, config, external, &cancel).await,
    }
}

/// Opens the store, builds the engine and runs `command` on them (`main.go:53-152`).
async fn run_engine(
    command: EngineCommand,
    config: Config,
    external: Arc<External>,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    let review_list_limit = config.review_list_limit;
    let github_login = config.github_login.clone();
    let store = Arc::new(Store::open(
        Path::new(&config.state_db_path),
        Box::new(OffsetDateTime::now_utc),
    )?);
    let engine = Arc::new(Engine::new(
        config,
        Arc::clone(&store),
        Arc::clone(&external),
        SystemClock,
    ));
    match command {
        EngineCommand::Serve => serve(&engine, &external, &github_login, cancel).await,
        EngineCommand::Poll => print_json(&engine.poll(cancel).await?),
        EngineCommand::Awaiting => print_json(&engine.list_awaiting(cancel)?),
        EngineCommand::ReviewQueue => print_json(&engine.review_queue(cancel).await?),
        EngineCommand::Reviews { limit } => {
            let limit = review_limit(limit.as_deref(), review_list_limit)?;
            print_json(&store.list_reviews(&ReviewFilter::All, limit)?)
        }
        EngineCommand::BackupDb { path } => {
            store.backup_to(&path)?;
            print_json(&BackupResult {
                ok: true,
                path: path.to_string_lossy().into_owned(),
            })
        }
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

/// Runs the engine after checking that the token belongs to the configured login.
///
/// A review panic ends the process, as a goroutine panic ends Go's.
async fn serve(
    engine: &Arc<Engine<External, SystemClock>>,
    external: &External,
    github_login: &str,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    let login = external.viewer_login(cancel).await?;
    if login != github_login {
        return Err(Error::config(
            CONFIGURATION_INVALID,
            LOGIN_KEY,
            Some(format!("token belongs to {login}").into()),
        ));
    }
    engine.serve(cancel).await
}

/// Cancels `cancel` on the first SIGINT or SIGTERM, as Go's `signal.NotifyContext` does.
///
/// # Errors
///
/// Returns `workflow_failed` when a signal handler cannot be installed.
fn cancel_on_signal(cancel: CancellationToken) -> Result<(), Error> {
    let mut interrupt = signal(SignalKind::interrupt()).map_err(failed)?;
    let mut terminate = signal(SignalKind::terminate()).map_err(failed)?;
    tokio::spawn(async move {
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = terminate.recv() => {}
        }
        cancel.cancel();
    });
    Ok(())
}

/// Returns the `reviews` limit: `limit` when given, otherwise `cli.review_list_limit`.
///
/// # Errors
///
/// Returns `command_invalid` when `limit` is not an integer from 1 to `maximum`.
fn review_limit(limit: Option<&str>, maximum: i64) -> Result<i64, Error> {
    let Some(text) = limit else {
        return Ok(maximum);
    };
    match text.parse::<i64>() {
        Ok(value) if value > 0 && value <= maximum => Ok(value),
        _ => Err(command_invalid(Some(
            format!("limit must be between 1 and {maximum}").into(),
        ))),
    }
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
