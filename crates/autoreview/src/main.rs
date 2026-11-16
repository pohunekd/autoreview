//! The `autoreview` command: its JSON on stdout, its logs on stderr.

// Rust guideline compliant 2026-06-26

mod config_file;
mod env_file;
mod error_response;
mod guard_scan;
mod init;
mod logging;
mod mcp_server;
mod metrics_server;
mod profiles;
mod prune_branches;
mod store_mode;

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use autoreview_engine::config::{Config, Credentials};
use autoreview_engine::credential_check::{CredentialStatus, check_credentials};
use autoreview_engine::mcp::McpConfig;
use autoreview_engine::metrics::MetricsConfig;
use autoreview_engine::model::{
    ApproveRequest, ResolveRequest, RetryRequest, head_prefix_valid, repository_valid, sha_valid,
};
use autoreview_engine::stats::parse_day;
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

use crate::env_file::{ENV_FILE_FLAG, EnvFile};
use crate::error_response::{
    COMMAND_INVALID, HEAD_PREFIX_INVALID, HEAD_SHA_INVALID, error_response,
};

/// Environment variable that names the config file when `--config` is absent.
const CONFIG_ENV: &str = "AUTOREVIEW_CONFIG";

/// The global option that names the config file.
const CONFIG_FLAG: &str = "--config";

/// Prefix of the global option written with its value, as `--config=PATH`.
const CONFIG_FLAG_WITH_VALUE: &str = "--config=";

/// Prefix of the secrets file option written with its value, as `--env-file=PATH`.
const ENV_FILE_FLAG_WITH_VALUE: &str = "--env-file=";

/// The end-of-options marker of clap, which the command line refuses.
const ESCAPE: &str = "--";

/// Configuration key named when the configuration holds no policy.
const POLICY_KEY: &str = "policy_file";

/// Error code of malformed repository names.
const REPOSITORY_NOT_ALLOWED: &str = "repository_not_allowed";

/// Error code of a malformed pull request number.
const REVIEW_IDENTITY_INVALID: &str = "review_identity_invalid";

/// Error code of a configuration value that the command refuses.
const CONFIGURATION_INVALID: &str = "configuration_invalid";

/// Error code of a configuration key that the command needs and the file lacks.
const CONFIGURATION_MISSING: &str = "configuration_missing";

/// Cause of the `command_invalid` error of a `findings list --since` that is not a day.
const SINCE_INVALID_CAUSE: &str = "--since is not an existing UTC day written as YYYY-MM-DD";

/// Configuration section that `autoreview mcp` needs.
const MCP_KEY: &str = "mcp";

/// Command line of `autoreview`.
#[derive(Debug, Parser)]
#[command(name = "autoreview", version, disable_help_subcommand = true)]
struct Cli {
    /// Path of `config.yaml`; `AUTOREVIEW_CONFIG` is read when it is absent.
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,
    /// Path of a file of `NAME=value` secret lines; its values win over the environment.
    #[arg(long, global = true, value_name = "PATH")]
    env_file: Option<PathBuf>,
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
    /// Runs the text guard over a corpus of pull request texts, offline.
    #[command(subcommand)]
    Guard(guard_scan::GuardCommand),
    /// A command that loads the configuration first.
    #[command(flatten)]
    Configured(Command),
}

/// The commands that load the configuration.
#[derive(Debug, Subcommand)]
enum Command {
    /// Validates the configuration and the policy and prints the policy digest.
    Validate(ValidateArguments),
    /// Lists the review branches that no review needs on the local review hosts; `--yes`
    /// deletes them.
    PruneBranches(prune_branches::PruneArguments),
    /// A command that runs on the store and the engine.
    #[command(flatten)]
    Engine(EngineCommand),
}

/// The options of `validate`.
#[derive(Debug, Args)]
struct ValidateArguments {
    /// Also checks every credential's token with GitHub and prints its login and expiry.
    #[arg(long)]
    check_credentials: bool,
}

/// The commands that open the store and build the engine first.
#[derive(Debug, Subcommand)]
enum EngineCommand {
    /// Runs discovery and reviews, and the metrics endpoint when configured, until a signal.
    Serve,
    /// Serves the MCP tools over Streamable HTTP until SIGINT or SIGTERM.
    Mcp,
    /// Runs one discovery pass and prints what it queued.
    Poll,
    /// Prints the pull requests that the next discovery pass would consider, classified.
    Candidates,
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
    /// Prints the review statistics: the current state and, with `--since`, the daily counts.
    Stats(StatsArguments),
    /// Prints the recorded finding outcomes.
    #[command(subcommand)]
    Findings(FindingsCommand),
    /// Writes a consistent copy of the store to a new absolute path.
    BackupDb {
        /// Absolute path of the copy; nothing may exist there yet.
        #[arg(allow_hyphen_values = true)]
        path: PathBuf,
    },
}

/// `--repository OWNER/REPO` and the positional arguments, in the order given.
#[derive(Debug, Args)]
struct ReviewArguments {
    /// The arguments in their order; `--repository` takes the next one as its value.
    #[arg(allow_hyphen_values = true, value_name = "ARGUMENTS", value_parser = raw_argument)]
    arguments: Vec<String>,
}

/// The options of `stats`.
#[derive(Debug, Args)]
struct StatsArguments {
    /// First UTC day of the history, as YYYY-MM-DD; without it only the current state is shown.
    #[arg(long, value_name = "DATE")]
    since: Option<String>,
    /// Last UTC day of the history, as YYYY-MM-DD; today (UTC) when absent.
    #[arg(long, value_name = "DATE")]
    until: Option<String>,
    /// Prints the JSON document instead of the text summary.
    #[arg(long)]
    json: bool,
}

/// The `findings` commands.
#[derive(Debug, Subcommand)]
enum FindingsCommand {
    /// Prints the finding outcomes of one repository as JSON lines, oldest first.
    List(FindingsListArguments),
}

/// The options of `findings list`.
#[derive(Debug, Args)]
struct FindingsListArguments {
    /// Repository as lowercase `owner/name`.
    #[arg(long, value_name = "OWNER/REPO")]
    repo: String,
    /// First UTC day, as YYYY-MM-DD: findings published at or after its midnight are printed.
    #[arg(long, value_name = "DATE")]
    since: String,
}

/// What `validate` prints: what the policy requires of the review hosts.
#[derive(Debug, Serialize)]
struct ValidationResult {
    /// Digest that identifies the effective policy.
    policy_digest: String,
    /// The pohunek agent profiles that the policy's reviewers run.
    agent_profiles: Vec<String>,
    /// Version of the pohunek CLI at `pohunek.bin`, inside the supported range.
    pohunek_version: String,
    /// Each credential's login and token expiry; only with `--check-credentials`.
    #[serde(skip_serializing_if = "Option::is_none")]
    credentials: Option<Vec<CredentialStatus>>,
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
                ..
            }) => init::run(config.as_deref(), &arguments).and_then(|result| print_json(&result)),
            Ok(Cli {
                config,
                command: Invocation::Profiles(command),
                ..
            }) => profiles::run(config.as_deref(), &command)
                .await
                .and_then(|done| print_json(&done)),
            Ok(Cli {
                config,
                command: Invocation::Guard(command),
                ..
            }) => guard_scan::run(config.as_deref(), &command).and_then(|done| print_json(&done)),
            Ok(Cli {
                config,
                env_file,
                command: Invocation::Configured(command),
            }) => run(config, env_file, command).await,
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

/// Parses the command line and refuses the `--` escape, which no command takes.
///
/// # Errors
///
/// Returns the clap error; a `--` argument is an `UnknownArgument` error, and `--env-file`
/// with `init`, `profiles` or `guard`, which read no secret, an `ArgumentConflict` error.
fn parse(arguments: Vec<OsString>) -> Result<Cli, clap::Error> {
    if arguments.iter().skip(1).any(|argument| argument == ESCAPE) {
        return Err(Cli::command().error(
            ErrorKind::UnknownArgument,
            format!("{ESCAPE} is not an argument of any command"),
        ));
    }
    let cli = Cli::try_parse_from(arguments)?;
    if cli.env_file.is_some() && !matches!(cli.command, Invocation::Configured(_)) {
        return Err(Cli::command().error(
            ErrorKind::ArgumentConflict,
            format!("{ENV_FILE_FLAG} is not an option of init, profiles and guard"),
        ));
    }
    Ok(cli)
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
///
/// A `branch_prune_incomplete` failure is only logged: its document is the stdout object.
fn report(error: &Error) {
    let code = error_code(error);
    // `prune-branches` printed its document; one object on stdout stays the rule.
    let printed = code == prune_branches::BRANCH_PRUNE_INCOMPLETE;
    let response = error_response(error);
    // A failed write leaves only the log line; there is nowhere else to report it.
    if let Some(text) = go_json(&response).ok().filter(|_| !printed) {
        let mut stdout = io::stdout().lock();
        if writeln!(stdout, "{text}")
            .and_then(|()| stdout.flush())
            .is_err()
        {
            tracing::warn!("command error not printed");
        }
    }
    tracing::error!(error_code = code, error = %error, "command failed");
}

/// Loads the configuration and runs the command.
///
/// The `--env-file` is read after the config path is chosen and before the config is loaded.
async fn run(
    config_path: Option<PathBuf>,
    env_file_path: Option<PathBuf>,
    command: Command,
) -> Result<(), Error> {
    let environment = std::env::var_os(CONFIG_ENV);
    let path = config_file::resolve_path(config_path.as_deref(), environment.as_deref())?;
    let env_file = env_file_path.as_deref().map(EnvFile::read).transpose()?;
    let (config, mcp, metrics) = config_file::load(&path, env_file.as_ref())?;
    let cancel = CancellationToken::new();
    cancel_on_signal(cancel.clone())?;
    match command {
        Command::Validate(arguments) => {
            validate(&config, arguments.check_credentials, &cancel).await
        }
        Command::PruneBranches(arguments) => {
            let result = prune_branches::run(&config, &arguments, &cancel).await?;
            print_json(&result)?;
            result.status()
        }
        Command::Engine(command) => {
            let external = Arc::new(External::new(config.clone())?);
            let sections = Sections { mcp, metrics };
            run_engine(command, config, sections, external, &cancel).await
        }
    }
}

/// The optional sections of the configuration that only one command needs.
struct Sections {
    /// The `mcp` section of `autoreview mcp`.
    mcp: Option<McpConfig>,
    /// The `metrics` section of `autoreview serve`.
    metrics: Option<MetricsConfig>,
}

/// Opens the store, builds the engine and runs `command` on them.
async fn run_engine(
    command: EngineCommand,
    config: Config,
    sections: Sections,
    external: Arc<External>,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    let review_list_limit = config.review_list_limit;
    let credentials = config.credentials.clone();
    let store = Arc::new(Store::open(
        Path::new(&config.state_db_path),
        Box::new(OffsetDateTime::now_utc),
    )?);
    let engine = Engine::new(
        config,
        Arc::clone(&store),
        Arc::clone(&external),
        SystemClock,
    );
    // Only the process that runs the reviews counts them, and only for its endpoint.
    let counted = matches!(command, EngineCommand::Serve) && sections.metrics.is_some();
    let engine = Arc::new(if counted {
        engine.with_metrics()
    } else {
        engine
    });
    match command {
        EngineCommand::Serve => {
            serve(&engine, &external, &credentials, sections.metrics, cancel).await
        }
        EngineCommand::Mcp => {
            let Some(mcp) = sections.mcp else {
                return Err(Error::config(CONFIGURATION_MISSING, MCP_KEY, None));
            };
            mcp_server::serve(engine, &mcp, cancel).await
        }
        EngineCommand::Poll => print_json(&engine.poll(cancel).await?),
        EngineCommand::Candidates => print_json(&engine.list_classified(cancel).await?),
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
        EngineCommand::Stats(arguments) => {
            let (since, until) = (arguments.since.as_deref(), arguments.until.as_deref());
            let document = engine.review_stats(cancel, since, until)?;
            if arguments.json {
                print_json(&document)
            } else {
                print_text(&document.to_string())
            }
        }
        EngineCommand::Findings(FindingsCommand::List(arguments)) => {
            list_findings(&store, &arguments)
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

/// Prints the policy digest, the agent profiles and the pohunek version, and with `check` each
/// credential's check.
///
/// Without `check`, it makes no GitHub call; it always runs `pohunek --version`. It first
/// checks that the store and its directory are private to their owner.
async fn validate(config: &Config, check: bool, cancel: &CancellationToken) -> Result<(), Error> {
    let Some(policy) = config.policy.as_ref() else {
        return Err(Error::config(CONFIGURATION_INVALID, POLICY_KEY, None));
    };
    store_mode::check(Path::new(&config.state_db_path))?;
    let external = External::new(config.clone())?;
    let pohunek_version = external.pohunek_version(cancel).await?;
    let credentials = if check {
        Some(check_credentials(&external, &config.credentials, cancel).await?)
    } else {
        None
    };
    print_json(&ValidationResult {
        policy_digest: policy.digest().to_owned(),
        agent_profiles: policy.agent_profiles(),
        pohunek_version,
        credentials,
    })
}

/// Runs the engine after checking the pohunek version and every credential's token with GitHub.
///
/// With `metrics`, the endpoint is bound after the credential check and before the engine
/// takes its lock, and it stops when the engine stops. A review panic ends the process. A
/// signal during either check is a normal stop, as a signal while the engine runs: it logs
/// the engine's stop line and returns `Ok`.
async fn serve(
    engine: &Arc<Engine<External, SystemClock>>,
    external: &External,
    credentials: &Credentials,
    metrics: Option<MetricsConfig>,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    let pohunek_version = match external.pohunek_version(cancel).await {
        Ok(version) => version,
        Err(_) if cancel.is_cancelled() => {
            tracing::info!("pull request review engine stopping");
            return Ok(());
        }
        Err(err) => return Err(err),
    };
    tracing::info!(pohunek_version = %pohunek_version, "pohunek version checked");
    let statuses = match check_credentials(external, credentials, cancel).await {
        Ok(statuses) => statuses,
        // Read from the token: the unit's SIGTERM may kill `gh` first, which fails as an exit.
        Err(_) if cancel.is_cancelled() => {
            tracing::info!("pull request review engine stopping");
            return Ok(());
        }
        Err(err) => return Err(err),
    };
    for status in &statuses {
        let (name, expires_at) = (&status.name, status.expires_at.as_deref());
        tracing::info!(credential = %name, expires_at, "credential checked");
    }
    let Some(metrics) = metrics else {
        return engine.serve(cancel).await;
    };
    let listener = metrics_server::bind(&metrics).await?;
    let stop = cancel.child_token();
    let endpoint = tokio::spawn(metrics_server::run(
        listener,
        Arc::clone(engine),
        metrics,
        stop.clone(),
    ));
    let result = engine.serve(cancel).await;
    stop.cancel();
    let stopped = endpoint.await;
    result?;
    stopped.map_err(failed)
}

/// Cancels `cancel` on the first SIGINT or SIGTERM.
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

/// Reads `--repository OWNER/REPO NUMBER HEAD`.
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

/// Reads `--repository OWNER/REPO NUMBER`.
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

/// Prints the outcomes of `--repo` published since `--since`, one JSON object per line.
///
/// Without outcomes it prints nothing. The repository is not lowercased: discovery stores it
/// lowercase, and the other commands refuse a name with capitals too.
///
/// A reader that closes stdout early, as `head` does, ends the output without an error.
///
/// # Errors
///
/// Returns `repository_not_allowed` for a repository that is not a lowercase `owner/name`,
/// `command_invalid` for a day that is not an existing `YYYY-MM-DD`, the store's errors, and
/// `workflow_failed` when a row cannot be encoded or stdout fails other than by a closed pipe.
fn list_findings(store: &Store, arguments: &FindingsListArguments) -> Result<(), Error> {
    if !repository_valid(&arguments.repo) {
        return Err(Error::workflow(REPOSITORY_NOT_ALLOWED, true, None));
    }
    // The day error carries the code of `stats`; this command reports a usage error instead.
    let since = parse_day(&arguments.since)
        .map_err(|_| command_invalid(Some(SINCE_INVALID_CAUSE.into())))?;
    let mut stdout = io::stdout().lock();
    for outcome in store.list_outcomes(&arguments.repo, since.midnight().assume_utc())? {
        let text = go_json(&outcome).map_err(failed)?;
        match writeln!(stdout, "{text}").and_then(|()| stdout.flush()) {
            Ok(()) => {}
            // The Rust runtime ignores SIGPIPE, so a closed reader surfaces as this error.
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => return Ok(()),
            Err(error) => return Err(failed(error)),
        }
    }
    Ok(())
}

/// Refuses `--config` and `--env-file` among the raw arguments, where clap reads them as
/// arguments.
///
/// # Errors
///
/// Returns the reason that clap reports as a `command_invalid` parse error.
fn raw_argument(value: &str) -> Result<String, String> {
    let global = [
        (CONFIG_FLAG, CONFIG_FLAG_WITH_VALUE),
        (ENV_FILE_FLAG, ENV_FILE_FLAG_WITH_VALUE),
    ];
    for (flag, with_value) in global {
        if value == flag || value.starts_with(with_value) {
            return Err(format!(
                "{flag} must come before the arguments of the command"
            ));
        }
    }
    Ok(value.to_owned())
}

/// Splits the value of the last `--repository` from the positional arguments.
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

/// Parses a positive pull request number: an optional sign and decimal digits, above 0.
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

/// Prints `value` as JSON, then a newline.
///
/// `<`, `>`, `&`, U+2028 and U+2029 are escaped, as in all JSON command output on stdout;
/// the JSON log lines on stderr do not use this escaping.
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

/// Prints `text`, which ends with a newline, on stdout.
///
/// # Errors
///
/// Returns `workflow_failed` when stdout cannot be written.
fn print_text(text: &str) -> Result<(), Error> {
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
        .map_err(failed)
}

/// Returns the `command_invalid` error of malformed arguments.
fn command_invalid(cause: Option<BoxError>) -> Error {
    Error::workflow(COMMAND_INVALID, true, cause)
}

/// Wraps a failure that carries no engine code as `workflow_failed`.
fn failed(cause: impl Into<BoxError>) -> Error {
    Error::workflow(WORKFLOW_FAILED, false, Some(cause.into()))
}
