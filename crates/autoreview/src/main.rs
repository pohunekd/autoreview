//! The `autoreview` command: its JSON on stdout, its logs on stderr.
//!
//! Mirrors `cmd/autoreview/main.go:1-224`.

// Rust guideline compliant 2026-06-26

mod config_file;
mod error_response;
mod init;
mod logging;
mod mcp_server;
mod profiles;

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use autoreview_engine::config::Config;
use autoreview_engine::integrations::Integrations;
use autoreview_engine::mcp::McpConfig;
use autoreview_engine::model::{
    ApproveRequest, ResolveRequest, RetryRequest, head_prefix_valid, repository_valid, sha_valid,
};
use autoreview_engine::store::{ReviewFilter, Store};
use autoreview_engine::{
    BoxError, Engine, Error, SystemClock, WORKFLOW_FAILED, error_code, go_json,
};
use autoreview_external::External;
use clap::error::ErrorKind;
use clap::{Args, CommandFactory, Parser, Subcommand};
use serde::Serialize;
use time::OffsetDateTime;
use tokio::signal::unix::{SignalKind, signal};
use tokio_util::sync::CancellationToken;

use crate::error_response::{
    COMMAND_INVALID, HEAD_PREFIX_INVALID, HEAD_SHA_INVALID, error_response,
};

/// Environment variable that names the config file when `--config` is absent.
const CONFIG_ENV: &str = "AUTOREVIEW_CONFIG";

/// The global option that names the config file.
const CONFIG_FLAG: &str = "--config";

/// Prefix of the global option written with its value, as `--config=PATH`.
const CONFIG_FLAG_WITH_VALUE: &str = "--config=";

/// The end-of-options marker of clap, which Go's argument reading does not know.
const ESCAPE: &str = "--";

/// Configuration key named when the configuration holds no policy.
const POLICY_KEY: &str = "policy_file";

/// Configuration key named when the token belongs to another login than the credential.
const LOGIN_KEY: &str = "github.credentials.login";

/// Error code of malformed repository names.
const REPOSITORY_NOT_ALLOWED: &str = "repository_not_allowed";

/// Error code of a malformed pull request number.
const REVIEW_IDENTITY_INVALID: &str = "review_identity_invalid";

/// Error code of a configuration value that the command refuses.
const CONFIGURATION_INVALID: &str = "configuration_invalid";

/// Error code of a configuration key that the command needs and the file lacks.
const CONFIGURATION_MISSING: &str = "configuration_missing";

/// Configuration section that `autoreview mcp` needs.
const MCP_KEY: &str = "mcp";

/// Command line of `autoreview`.
#[derive(Debug, Parser)]
#[command(name = "autoreview", version, disable_help_subcommand = true)]
struct Cli {
    /// Path of `config.yaml`; `AUTOREVIEW_CONFIG` is read when it is absent.
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,
    /// The command to run.
    #[command(subcommand)]
    command: Invocation,
}

/// The commands of `autoreview`: the host commands, and the commands that load the configuration.
#[derive(Debug, Subcommand)]
enum Invocation {
    /// Writes a new configuration directory from an example; it never overwrites a file.
    Init(init::InitArguments),
    /// Manages the pohunek agent profiles of the reviewers on this host.
    #[command(subcommand)]
    Profiles(profiles::ProfilesCommand),
    /// A command that loads the configuration first.
    #[command(flatten)]
    Configured(Command),
}

/// The commands that load the configuration, as `cmd/autoreview/main.go:42-152` dispatches them.
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
    /// Serves the MCP tools over Streamable HTTP until SIGINT or SIGTERM.
    Mcp,
    /// Runs one discovery pass and prints what it queued.
    Poll,
    /// Starts a new attempt of a stopped review.
    Retry(ReviewArguments),
    /// Prints the full head of the review awaiting approval at a head prefix.
    Resolve(ReviewArguments),
    /// Approves a clean review that waits for a human.
    Approve(ReviewArguments),
    /// Prints the reviews that wait for a human approval.
    Awaiting,
    /// Sends the review queue digest and prints it.
    ReviewQueue,
    /// Prints the newest reviews.
    Reviews {
        /// Most reviews printed, from 1 to `cli.review_list_limit`.
        limit: Option<String>,
    },
    /// Prints everything the engine knows about one pull request.
    Review(ReviewArguments),
    /// Writes a consistent copy of the store to a new absolute path.
    BackupDb {
        /// Absolute path of the copy; nothing may exist there yet.
        #[arg(allow_hyphen_values = true)]
        path: PathBuf,
    },
}

/// `--repository OWNER/REPO` and the positional arguments, read as Go reads them.
#[derive(Debug, Args)]
struct ReviewArguments {
    /// The arguments in their order; `--repository` takes the next one as its value.
    #[arg(allow_hyphen_values = true, value_name = "ARGUMENTS", value_parser = raw_argument)]
    arguments: Vec<String>,
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
            Ok(Cli {
                config,
                command: Invocation::Init(arguments),
            }) => init::run(config.as_deref(), &arguments).and_then(|result| print_json(&result)),
            Ok(Cli {
                config,
                command: Invocation::Profiles(command),
            }) => profiles::run(config.as_deref(), &command).and_then(|done| print_json(&done)),
            Ok(Cli {
                config,
                command: Invocation::Configured(command),
            }) => run(config, command).await,
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
async fn run(config_path: Option<PathBuf>, command: Command) -> Result<(), Error> {
    let environment = std::env::var_os(CONFIG_ENV);
    let path = config_file::resolve_path(config_path.as_deref(), environment.as_deref())?;
    let (config, mcp) = config_file::load(&path)?;
    let external = Arc::new(External::new(config.clone())?);
    let cancel = CancellationToken::new();
    cancel_on_signal(cancel.clone())?;
    match command {
        Command::Validate => validate(&config),
        Command::Candidates => print_json(&external.list_candidates(&cancel).await?),
        Command::Engine(command) => run_engine(command, config, mcp, external, &cancel).await,
    }
}

/// Opens the store, builds the engine and runs `command` on them (`main.go:53-152`).
async fn run_engine(
    command: EngineCommand,
    config: Config,
    mcp: Option<McpConfig>,
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
        EngineCommand::Mcp => {
            let Some(mcp) = mcp else {
                return Err(Error::config(CONFIGURATION_MISSING, MCP_KEY, None));
            };
            mcp_server::serve(engine, &mcp, cancel).await
        }
        EngineCommand::Poll => print_json(&engine.poll(cancel).await?),
        EngineCommand::Retry(arguments) => {
            let (repository, number, head_sha) = review_arguments(&arguments.arguments, true)?;
            let request = RetryRequest {
                repository,
                number,
                head_sha,
            };
            print_json(&engine.retry(cancel, &request).await?)
        }
        EngineCommand::Resolve(arguments) => {
            let (repository, number, head_prefix) = review_arguments(&arguments.arguments, true)?;
            let request = ResolveRequest {
                repository,
                number,
                head_prefix,
            };
            print_json(&engine.resolve(cancel, &request)?)
        }
        EngineCommand::Approve(arguments) => {
            let (repository, number, head_sha) = review_arguments(&arguments.arguments, false)?;
            let request = ApproveRequest {
                repository,
                number,
                head_sha,
            };
            print_json(&engine.approve_head(cancel, &request)?)
        }
        EngineCommand::Awaiting => print_json(&engine.list_awaiting(cancel)?),
        EngineCommand::ReviewQueue => print_json(&engine.review_queue(cancel).await?),
        EngineCommand::Reviews { limit } => {
            let limit = review_limit(limit.as_deref(), review_list_limit)?;
            print_json(&store.list_reviews(&ReviewFilter::All, limit)?)
        }
        EngineCommand::Review(arguments) => {
            let (repository, number) = pull_request_arguments(&arguments.arguments)?;
            print_json(&engine.pull_request_status(cancel, &repository, number)?)
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

/// Reads `--repository OWNER/REPO NUMBER HEAD`, as `main.go:178-198` does.
///
/// With `prefix`, HEAD may be an abbreviated head of at least 12 characters.
///
/// # Errors
///
/// Returns `repository_not_allowed`, `command_invalid` when not exactly two positional
/// arguments remain, `review_identity_invalid` for a bad number, and then
/// `head_prefix_invalid` (with `prefix`) or `head_sha_invalid` for a bad head.
fn review_arguments(arguments: &[String], prefix: bool) -> Result<(String, i64, String), Error> {
    let (repository, positional) = split_repository(arguments);
    if !repository_valid(&repository) {
        return Err(Error::workflow(REPOSITORY_NOT_ALLOWED, true, None));
    }
    let [number, head] = positional.as_slice() else {
        return Err(command_invalid(None));
    };
    let Some(number) = parse_number(number) else {
        return Err(Error::workflow(REVIEW_IDENTITY_INVALID, true, None));
    };
    let (head_valid, head_code) = if prefix {
        (head_prefix_valid(head), HEAD_PREFIX_INVALID)
    } else {
        (sha_valid(head), HEAD_SHA_INVALID)
    };
    if !head_valid {
        return Err(Error::workflow(head_code, true, None));
    }
    Ok((repository, number, (*head).to_owned()))
}

/// Reads `--repository OWNER/REPO NUMBER`, as `main.go:200-213` does.
///
/// # Errors
///
/// Returns `repository_not_allowed`, `command_invalid` when not exactly one positional
/// argument remains, and `review_identity_invalid` for a bad number.
fn pull_request_arguments(arguments: &[String]) -> Result<(String, i64), Error> {
    let (repository, positional) = split_repository(arguments);
    if !repository_valid(&repository) {
        return Err(Error::workflow(REPOSITORY_NOT_ALLOWED, true, None));
    }
    let [number] = positional.as_slice() else {
        return Err(command_invalid(None));
    };
    match parse_number(number) {
        Some(number) => Ok((repository, number)),
        None => Err(Error::workflow(REVIEW_IDENTITY_INVALID, true, None)),
    }
}

/// Refuses `--config` among the raw arguments, where clap reads it as an argument.
///
/// # Errors
///
/// Returns the reason that clap reports as a `command_invalid` parse error.
fn raw_argument(value: &str) -> Result<String, String> {
    if value == CONFIG_FLAG || value.starts_with(CONFIG_FLAG_WITH_VALUE) {
        return Err(format!(
            "{CONFIG_FLAG} must come before the arguments of the command"
        ));
    }
    Ok(value.to_owned())
}

/// Splits the value of the last `--repository` from the positional arguments (`main.go:215`).
///
/// A trailing `--repository` without a value stays a positional argument.
fn split_repository(arguments: &[String]) -> (String, Vec<&str>) {
    let mut repository = String::new();
    let mut positional = Vec::new();
    let mut rest = arguments.iter();
    while let Some(argument) = rest.next() {
        if argument == "--repository"
            && let Some(value) = rest.as_slice().first()
        {
            repository.clone_from(value);
            rest.next();
            continue;
        }
        positional.push(argument.as_str());
    }
    (repository, positional)
}

/// Parses a positive pull request number as Go's `strconv.Atoi` followed by `> 0`.
fn parse_number(text: &str) -> Option<i64> {
    text.parse::<i64>().ok().filter(|number| *number > 0)
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

#[cfg(test)]
mod tests {
    use autoreview_engine::error_code;

    use super::review_arguments;

    /// A full head SHA of 40 lowercase hexadecimal digits.
    const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

    /// A refused head names the argument; a bad number stays `review_identity_invalid`.
    #[test]
    fn review_arguments_name_the_refused_head() {
        let too_long = format!("{HEAD}8");
        // Number, head, whether a prefix is accepted, and the code ("" when accepted).
        let cases = [
            ("7", &HEAD[..12], true, ""),
            ("7", HEAD, true, ""),
            ("7", HEAD, false, ""),
            ("7", &HEAD[..8], true, "head_prefix_invalid"),
            ("7", "0123456789AB", true, "head_prefix_invalid"),
            ("7", "0123456789ag", true, "head_prefix_invalid"),
            ("7", too_long.as_str(), true, "head_prefix_invalid"),
            ("7", &HEAD[..8], false, "head_sha_invalid"),
            ("7", &HEAD[..12], false, "head_sha_invalid"),
            ("7", too_long.as_str(), false, "head_sha_invalid"),
            ("0", HEAD, true, "review_identity_invalid"),
            ("x", HEAD, false, "review_identity_invalid"),
            ("0", &HEAD[..8], true, "review_identity_invalid"),
            ("0", &HEAD[..8], false, "review_identity_invalid"),
        ];
        for (number, head, prefix, expected) in cases {
            let arguments = ["--repository", "acme/widget", number, head].map(str::to_owned);
            let result = review_arguments(&arguments, prefix);
            let code = match &result {
                Ok(_) => "",
                Err(error) => error_code(error),
            };
            assert_eq!(code, expected, "{number} {head} {prefix}");
        }
    }
}
