//! The `autoreview eval` commands.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use autoreview_engine::config::{Config, Credential, NotifySettings};
use autoreview_engine::eval::add::{
    FixSelection, Skipped, clean_title, fix_description, fix_ranges, write_case,
};
use autoreview_engine::eval::case::{
    Case, CaseError, Expect, Expected, head_branch_valid, identifier_valid, load_cases,
};
use autoreview_engine::eval::config::EvalConfig;
use autoreview_engine::eval::integration::{EvalIntegration, RunCounters};
use autoreview_engine::eval::report::{
    CaseKind, CaseReport, Gate, REPORT_SCHEMA, Totals, TrialReport, TrialStatus, VerifierState,
    check_thresholds_have_cases, evaluate_gate, exit_status, totals,
};
use autoreview_engine::eval::trial::{eval_pull_request, eval_rule};
use autoreview_engine::integrations::Integrations;
use autoreview_engine::model::{branch_valid, repository_valid, sha_valid};
use autoreview_engine::store::Store;
use autoreview_engine::{Engine, Error, SystemClock};
use autoreview_external::External;
use autoreview_policy::Checkout;
use clap::{Args, Subcommand};
use serde::Serialize;
use time::OffsetDateTime;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::env_file::EnvFile;
use crate::prune_branches::{RunBranch, delete_run_branches};
use crate::{CONFIG_ENV, cancel_on_signal, command_invalid, config_file, print_json};

/// Error code of a cases directory with an invalid case; its document is the stdout object.
pub(crate) const EVAL_CASES_INVALID: &str = "eval_cases_invalid";

/// Error code of a run whose gate is not met; its report is the stdout object.
const THRESHOLD_NOT_MET: &str = "eval_threshold_not_met";

/// Error code of a run with a trial that could not be measured; its report is the stdout object.
const TRIALS_FAILED: &str = "eval_trials_failed";

/// Exit code of a run whose gate is not met.
const EXIT_THRESHOLD: u8 = 2;

/// Exit code of a run with a failed trial.
const EXIT_TRIALS_FAILED: u8 = 3;

/// Code of a trial that ran past `eval.trial_timeout_seconds`.
const TRIAL_TIMEOUT: &str = "eval_trial_timeout";

/// Code of a trial whose task panicked.
const TRIAL_PANICKED: &str = "eval_trial_panicked";

/// The `eval` commands.
#[derive(Debug, Subcommand)]
pub(crate) enum EvalCommand {
    /// Checks the case files of a directory, offline, and prints every error found.
    Validate(ValidateArguments),
    /// Reviews every case with the real reviewers and verifier, publishes nothing, and prints
    /// the report.
    Run(RunArguments),
    /// Writes a new case from the commit that fixed a defect a review let through, and prints it.
    Add(AddArguments),
}

/// The options of `eval add`.
#[derive(Debug, Args)]
pub(crate) struct AddArguments {
    /// Directory of the case files; the new case is written there as `<id>.yaml`.
    #[arg(long, value_name = "DIR")]
    cases: PathBuf,
    /// The pull request that was reviewed, as `owner/name#number`.
    #[arg(long, value_name = "OWNER/NAME#N", value_parser = pull_request_ref)]
    from_pr: (String, i64),
    /// The commit that fixed the defect, as 40 lowercase hex digits.
    #[arg(long, value_name = "SHA")]
    fix: String,
    /// Identifier of the case and name of its file without `.yaml`.
    #[arg(long, value_name = "ID")]
    id: String,
    /// Base commit of the reviewed diff.
    #[arg(long, value_name = "SHA")]
    base_sha: String,
    /// Head commit of the reviewed diff.
    #[arg(long, value_name = "SHA")]
    head_sha: String,
    /// Base branch the reviewer worktree starts from.
    #[arg(long, value_name = "BRANCH")]
    base_branch: String,
    /// Fixture branch that holds the head; required when the pull request ref moved on.
    #[arg(long, value_name = "BRANCH")]
    head_branch: Option<String>,
    /// File to keep, as the reviewed head names it; repeat it to keep several files.
    #[arg(long = "path", value_name = "PATH")]
    paths: Vec<String>,
    /// Range that replaces the one computed for a file, as `PATH:FIRST-LAST`.
    #[arg(long = "lines", value_name = "PATH:FIRST-LAST", value_parser = path_lines)]
    lines: Vec<(String, (i64, i64))>,
}

/// Reads `owner/name#number`.
fn pull_request_ref(text: &str) -> Result<(String, i64), String> {
    let invalid = || "must be a lowercase owner/name#number".to_owned();
    let (repository, number) = text.split_once('#').ok_or_else(invalid)?;
    let number: i64 = number.parse().map_err(|_| invalid())?;
    if repository_valid(repository) && number >= 1 {
        Ok((repository.to_owned(), number))
    } else {
        Err(invalid())
    }
}

/// Reads `PATH:FIRST-LAST` with `1 <= FIRST <= LAST`.
fn path_lines(text: &str) -> Result<(String, (i64, i64)), String> {
    let invalid = || "must be PATH:FIRST-LAST with 1 <= FIRST <= LAST".to_owned();
    let (path, range) = text.rsplit_once(':').ok_or_else(invalid)?;
    let (first, last) = range.split_once('-').ok_or_else(invalid)?;
    let (first, last): (i64, i64) = match (first.parse(), last.parse()) {
        (Ok(first), Ok(last)) if (1..=last).contains(&first) => (first, last),
        _ => return Err(invalid()),
    };
    Ok((path.to_owned(), (first, last)))
}

/// The options of `eval validate`.
#[derive(Debug, Args)]
pub(crate) struct ValidateArguments {
    /// Directory of the case files, each named `<id>.yaml`.
    #[arg(long, value_name = "DIR")]
    cases: PathBuf,
}

/// The options of `eval run`.
#[derive(Debug, Args)]
pub(crate) struct RunArguments {
    /// Directory of the case files, each named `<id>.yaml`.
    #[arg(long, value_name = "DIR")]
    cases: PathBuf,
    /// Policy pipeline to evaluate; the rule of its first use supplies the route.
    #[arg(long, value_name = "NAME")]
    pipeline: String,
    /// Reviews per case, from 1 to `eval.max_trials`.
    #[arg(long, value_name = "N", default_value_t = 1)]
    repeat: i64,
    /// Least recall after verification, from 0 to 1; exit code 2 below it.
    #[arg(long, value_name = "RATIO", value_parser = ratio)]
    min_recall: Option<f64>,
    /// Greatest false blocking rate after verification, from 0 to 1; exit code 2 above it.
    #[arg(long, value_name = "RATIO", value_parser = ratio)]
    max_false_blocking: Option<f64>,
    /// Absolute path of the git program that deletes the review branches of the run.
    #[arg(long, value_name = "PATH")]
    git_binary: String,
}

/// Reads a threshold between 0 and 1.
fn ratio(text: &str) -> Result<f64, String> {
    let value: f64 = text.parse().map_err(|_| "not a number".to_owned())?;
    if (0.0..=1.0).contains(&value) {
        Ok(value)
    } else {
        Err("must be from 0 to 1".to_owned())
    }
}

/// What `eval validate` prints for valid cases.
#[derive(Debug, Serialize)]
struct ValidCases {
    /// Always `true`.
    ok: bool,
    /// Number of cases.
    cases: usize,
    /// Cases that expect blocking findings.
    defect: usize,
    /// Cases that expect none.
    clean: usize,
}

/// What `eval validate` prints for invalid cases.
#[derive(Debug, Serialize)]
struct InvalidCases {
    /// Always `false`.
    ok: bool,
    /// Every problem of every file.
    errors: Vec<CaseError>,
}

/// What a run used.
#[derive(Debug, Serialize)]
struct Usage {
    /// Trials run.
    trials: usize,
    /// Sessions started, retries included.
    sessions_started: usize,
    /// Reviews that would have been published.
    would_publish: usize,
}

/// What `eval run` prints.
#[derive(Debug, Serialize)]
struct RunReport {
    /// Schema name of the report.
    schema: &'static str,
    /// The evaluated pipeline.
    pipeline: String,
    /// Digest of the policy under test.
    policy_digest: String,
    /// Reviews per case.
    repeat: i64,
    /// Wall-clock time of the run.
    duration_ms: i64,
    /// What the run used.
    usage: Usage,
    /// Every case, skipped ones included.
    cases: Vec<CaseReport>,
    /// The rates.
    totals: Totals,
    /// The thresholds and whether they are met.
    gate: Gate,
    /// The review branches of the run that were deleted from their host.
    branches_deleted: Vec<RunBranch>,
    /// The review branches of the run that stay on their host.
    branches_left: Vec<RunBranch>,
    /// The sessions of the run that could not be removed and stay open on their host.
    sessions_left: Vec<RunSession>,
}

/// A review session that stays open on its host.
#[derive(Debug, Serialize)]
struct RunSession {
    /// The host as `pohunek.hosts` names it.
    host: String,
    /// Identifier of the session on the host.
    session_id: String,
}

/// Returns the report of a trial whose task panicked.
fn panicked_trial(repeat: i64, sessions: u32) -> TrialReport {
    TrialReport {
        repeat,
        status: TrialStatus::Failed,
        error_code: TRIAL_PANICKED.to_owned(),
        duration_ms: 0,
        sessions,
        verifier: VerifierState::None,
        findings: Vec::new(),
        stages: None,
    }
}

/// Returns the exit code of an error that `eval` already printed its document for.
pub(crate) fn exit_code(code: &str) -> Option<u8> {
    match code {
        EVAL_CASES_INVALID => Some(1),
        THRESHOLD_NOT_MET => Some(EXIT_THRESHOLD),
        TRIALS_FAILED => Some(EXIT_TRIALS_FAILED),
        _ => None,
    }
}

/// Runs an `eval` command.
///
/// # Errors
///
/// Returns `command_invalid` for `--config` with `eval validate`, the code of a printed
/// document (`eval_cases_invalid`, `eval_threshold_not_met`, `eval_trials_failed`), a
/// configuration error, or the error that stopped the run.
pub(crate) async fn run(
    config_path: Option<PathBuf>,
    env_file_path: Option<PathBuf>,
    command: EvalCommand,
) -> Result<(), Error> {
    match command {
        EvalCommand::Validate(arguments) => {
            if config_path.is_some() {
                return Err(command_invalid(Some(
                    "--config is not an option of eval validate; it reads no configuration".into(),
                )));
            }
            let cases = read_cases(&arguments.cases)?;
            let defect = cases.iter().filter(|case| case.is_defect()).count();
            print_json(&ValidCases {
                ok: true,
                cases: cases.len(),
                defect,
                clean: cases.len().saturating_sub(defect),
            })
        }
        EvalCommand::Run(arguments) => run_suite(config_path, env_file_path, &arguments).await,
        EvalCommand::Add(arguments) => add_case(config_path, env_file_path, &arguments).await,
    }
}

/// Loads the configuration file and its required `eval` section.
fn load_eval(
    config_path: Option<PathBuf>,
    env_file_path: Option<PathBuf>,
) -> Result<(Config, EvalConfig), Error> {
    let environment = std::env::var_os(CONFIG_ENV);
    let path = config_file::resolve_path(config_path.as_deref(), environment.as_deref())?;
    let env_file = env_file_path.as_deref().map(EnvFile::read).transpose()?;
    let loaded = config_file::load(&path, env_file.as_ref())?;
    let eval = (loaded.eval).ok_or_else(|| Error::config("configuration_missing", "eval", None))?;
    Ok((loaded.config, eval))
}

/// Returns the credential that `eval` reads with, or `credential_not_covered`.
fn eval_credential(config: &Config, eval: &EvalConfig) -> Result<Credential, Error> {
    (config.credentials.iter())
        .find(|credential| credential.name == eval.credential)
        .cloned()
        .ok_or_else(|| Error::workflow("credential_not_covered", true, None))
}

/// Returns the GitHub reader of an eval: the real integrations, which can notify nowhere.
fn reading_external(config: &Config) -> Result<External, Error> {
    // The wrapper never notifies; with no backend configured nothing could.
    let mut config = config.clone();
    config.notify = NotifySettings {
        backends: Vec::new(),
        action: Vec::new(),
        digest: Vec::new(),
        log: Vec::new(),
        events: BTreeMap::new(),
        queue: Vec::new(),
    };
    External::new(config)
}

/// The fixture branch that a case with a moved pull request ref needs.
#[derive(Debug, Serialize)]
struct FixtureBranch {
    /// Branch name under `refs/heads/`.
    name: String,
    /// The commands that create it from the reviewed head; they are run by the operator.
    commands: Vec<String>,
}

/// What `eval add` prints.
#[derive(Debug, Serialize)]
struct AddedCase {
    /// Always `true`.
    ok: bool,
    /// Path of the written case file.
    file: String,
    /// Identifier of the case.
    id: String,
    /// `defect`: the case expects blocking findings.
    kind: &'static str,
    /// The text of the case file.
    yaml: String,
    /// Files of the fix that gave no expected finding.
    skipped: Vec<Skipped>,
    /// The branch to push before the case can run; `None` when the pull request ref serves.
    fixture_branch: Option<FixtureBranch>,
}

/// Refuses an argument of `eval add` that no case could hold.
fn check_add_arguments(
    arguments: &AddArguments,
    config: &Config,
    eval: &EvalConfig,
) -> Result<(), Error> {
    let invalid = |message: &str| command_invalid(Some(message.to_owned().into()));
    let shas = [
        ("--fix", &arguments.fix),
        ("--base-sha", &arguments.base_sha),
        ("--head-sha", &arguments.head_sha),
    ];
    if let Some((flag, _)) = shas.iter().find(|(_, sha)| !sha_valid(sha)) {
        return Err(invalid(&format!("{flag} must be 40 lowercase hex digits")));
    }
    if !identifier_valid(&arguments.id) {
        return Err(invalid("--id must be 1 to 64 of a-z, 0-9, `.`, `_`, `-`"));
    }
    if !branch_valid(&arguments.base_branch) {
        return Err(invalid("--base-branch is not a branch name"));
    }
    let Some(branch) = &arguments.head_branch else {
        return Ok(());
    };
    if !head_branch_valid(branch) {
        return Err(invalid(
            "--head-branch is not a branch name that git accepts",
        ));
    }
    let prefixes = [&config.session_branch_prefix, &eval.session_branch_prefix];
    if prefixes
        .iter()
        .any(|prefix| branch.starts_with(prefix.as_str()))
    {
        return Err(Error::workflow("eval_head_ref_reserved", true, None));
    }
    Ok(())
}

/// Builds the case of a fixed defect from GitHub, writes it and prints it.
async fn add_case(
    config_path: Option<PathBuf>,
    env_file_path: Option<PathBuf>,
    arguments: &AddArguments,
) -> Result<(), Error> {
    let (config, eval) = load_eval(config_path, env_file_path)?;
    check_add_arguments(arguments, &config, &eval)?;
    let (repository, number) = (&arguments.from_pr.0, arguments.from_pr.1);
    let credential = eval_credential(&config, &eval)?;
    let covered = config
        .credentials
        .reviewing(&credential.name, repository, &credential.login);
    covered?;
    let existing = read_cases(&arguments.cases)?;
    let github = reading_external(&config)?;
    let cancel = CancellationToken::new();
    cancel_on_signal(cancel.clone())?;
    let state = github
        .read_pull_request_state(&cancel, &credential, repository, number)
        .await?;
    if state.head_sha.is_empty() {
        return Err(Error::workflow(
            "eval_add_pull_request_not_found",
            true,
            None,
        ));
    }
    if state.head_sha != arguments.head_sha && arguments.head_branch.is_none() {
        return Err(Error::workflow("eval_add_head_branch_required", true, None));
    }
    let commit = github
        .read_commit(&cancel, &credential, repository, &arguments.fix)
        .await?;
    let range = github
        .read_range_files(
            &cancel,
            &credential,
            repository,
            &arguments.base_sha,
            &arguments.head_sha,
        )
        .await?;
    let lines: BTreeMap<String, (i64, i64)> = arguments.lines.iter().cloned().collect();
    let select = FixSelection {
        paths: &arguments.paths,
        lines: &lines,
    };
    let (ranges, skipped) = fix_ranges(&commit.files, &range.commentable, &select)?;
    // A line range of the fix's parent is the reviewed head's only where the file is the same.
    let mut drifted = Vec::new();
    for found in ranges.iter().filter(|found| !found.overridden) {
        let (at_head, at_parent) = (&arguments.head_sha, &commit.parent);
        let first = (at_head.as_str(), found.path.as_str());
        let second = (at_parent.as_str(), found.path.as_str());
        let ids = github
            .read_blob_ids(&cancel, &credential, repository, first, second)
            .await?;
        if !matches!(&ids, [Some(head), Some(parent)] if head == parent) {
            drifted.push(found.path.as_str());
        }
    }
    if !drifted.is_empty() {
        let cause = format!("{}; give the ranges as --lines", drifted.join(", "));
        return Err(Error::workflow(
            "eval_add_fix_drift",
            true,
            Some(cause.into()),
        ));
    }
    let description = fix_description(repository, &arguments.fix, &commit.subject);
    let expected = ranges.into_iter().map(|found| Expected {
        path: found.path,
        first_line: found.first_line,
        last_line: found.last_line,
        description: description.clone(),
    });
    let head_ref = (arguments.head_branch.as_ref()).map_or_else(
        || format!("refs/pull/{number}/head"),
        |name| format!("refs/heads/{name}"),
    );
    let case = Case {
        id: arguments.id.clone(),
        repository: repository.clone(),
        pull_request: number,
        base_branch: arguments.base_branch.clone(),
        head_ref,
        base_sha: arguments.base_sha.clone(),
        head_sha: arguments.head_sha.clone(),
        pipelines: Vec::new(),
        title: clean_title(&state.title),
        expect: Expect::Blocking(expected.collect()),
    };
    let source = format!("{repository}#{number}, fixed by {}", arguments.fix);
    let yaml = write_case(&arguments.cases, &existing, &case, &source)?;
    let fixture_branch = arguments.head_branch.as_ref().map(|name| FixtureBranch {
        name: name.clone(),
        commands: vec![
            format!("git fetch origin {}", arguments.head_sha),
            format!("git push origin {}:refs/heads/{name}", arguments.head_sha),
        ],
    });
    print_json(&AddedCase {
        ok: true,
        file: arguments
            .cases
            .join(format!("{}.yaml", case.id))
            .display()
            .to_string(),
        id: case.id,
        kind: "defect",
        yaml,
        skipped,
        fixture_branch,
    })
}

/// Reads the cases, or prints every error and returns `eval_cases_invalid`.
fn read_cases(dir: &Path) -> Result<Vec<Case>, Error> {
    load_cases(dir).map_err(
        |errors| match print_json(&InvalidCases { ok: false, errors }) {
            Ok(()) => Error::workflow(EVAL_CASES_INVALID, true, None),
            Err(error) => error,
        },
    )
}

/// Returns a nonce that differs between runs, from the clock and the process.
fn run_nonce() -> i64 {
    let nanos = OffsetDateTime::now_utc().unix_timestamp_nanos();
    let mixed = nanos ^ (i128::from(std::process::id()) << 48);
    i64::try_from(mixed & i128::from(i64::MAX))
        .unwrap_or(1)
        .max(1)
}

/// Loads the configuration, reviews every case and prints the report.
async fn run_suite(
    config_path: Option<PathBuf>,
    env_file_path: Option<PathBuf>,
    arguments: &RunArguments,
) -> Result<(), Error> {
    let (config, eval) = load_eval(config_path, env_file_path)?;
    let cases = read_cases(&arguments.cases)?;
    let policy = config
        .policy
        .as_ref()
        .ok_or_else(|| Error::config("configuration_missing", "policy_file", None))?;
    let rule = policy
        .rule_for_pipeline(&arguments.pipeline)
        .map(eval_rule)
        .ok_or_else(|| Error::workflow("eval_pipeline_unknown", true, None))?;
    let selected: Vec<&Case> = cases
        .iter()
        .filter(|case| case.runs_under(&arguments.pipeline))
        .collect();
    let repeat = usize::try_from(arguments.repeat)
        .ok()
        .filter(|repeat| (1..=eval.max_trials).contains(repeat))
        .ok_or_else(|| Error::workflow("eval_repeat_invalid", true, None))?;
    if selected.is_empty() {
        return Err(Error::workflow("eval_no_cases", true, None));
    }
    if selected.len() * repeat > eval.max_trials {
        return Err(Error::workflow("eval_too_many_trials", true, None));
    }
    let (min_recall, max_false_blocking) = (arguments.min_recall, arguments.max_false_blocking);
    check_thresholds_have_cases(&cases, &arguments.pipeline, min_recall, max_false_blocking)?;
    // A head checkout starts at the tip of the ref, which nothing pins to the case's head.
    if rule.pipeline.checkout != Checkout::Base {
        return Err(Error::workflow("eval_checkout_unsupported", true, None));
    }
    let prefixes = [&config.session_branch_prefix, &eval.session_branch_prefix];
    let reserved = |branch: &str| {
        prefixes
            .iter()
            .any(|prefix| branch.starts_with(prefix.as_str()))
    };
    if (selected.iter())
        .filter_map(|case| case.head_ref.strip_prefix("refs/heads/"))
        .any(reserved)
    {
        return Err(Error::workflow("eval_head_ref_reserved", true, None));
    }
    if !arguments.git_binary.starts_with('/') {
        let cause = "--git-binary must be an absolute path".into();
        return Err(command_invalid(Some(cause)));
    }
    let credential = eval_credential(&config, &eval)?;
    for case in &selected {
        let covered =
            config
                .credentials
                .reviewing(&credential.name, &case.repository, &credential.login);
        covered?;
    }
    tracing::info!(trials = selected.len() * repeat, "eval run starting");
    let mut engine_config = config.clone();
    engine_config
        .session_name_prefix
        .clone_from(&eval.session_name_prefix);
    engine_config
        .session_branch_prefix
        .clone_from(&eval.session_branch_prefix);
    let directory = tempfile::tempdir()
        .map_err(|error| Error::workflow("store_unavailable", true, Some(Box::new(error))))?;
    let store = Arc::new(Store::open(
        &directory.path().join("eval.db"),
        Box::new(OffsetDateTime::now_utc),
    )?);
    let inner = Arc::new(reading_external(&config)?);
    let cancel = CancellationToken::new();
    cancel_on_signal(cancel.clone())?;
    let counters = Arc::new(RunCounters::default());
    let (nonce, started) = (run_nonce(), Instant::now());
    let permits = Arc::new(Semaphore::new(eval.max_concurrent_trials));
    // The case index, repeat and integration of every spawned trial, for one that panics.
    let (mut tasks, mut spawned) = (JoinSet::new(), BTreeMap::new());
    for (index, case) in selected.iter().enumerate() {
        for attempt in 1..=arguments.repeat {
            let pull_request = eval_pull_request(case, &config.github_host, nonce);
            let integration = Arc::new(EvalIntegration::new(
                Arc::clone(&inner),
                pull_request,
                case.head_ref.clone(),
                Arc::clone(&counters),
                eval.max_sessions,
            ));
            let tracked = Arc::clone(&integration);
            let engine = Engine::new(
                engine_config.clone(),
                Arc::clone(&store),
                integration,
                SystemClock,
            );
            let (case, rule, credential) = ((*case).clone(), rule.clone(), credential.clone());
            let (permits, child, limit) = (
                Arc::clone(&permits),
                cancel.child_token(),
                eval.trial_timeout,
            );
            let task = tasks.spawn(async move {
                let permit = permits.acquire().await;
                let run = engine.run_eval_trial(&child, &case, &rule, &credential, attempt);
                tokio::pin!(run);
                let trial = tokio::select! {
                    done = &mut run => done,
                    () = tokio::time::sleep(limit) => {
                        child.cancel();
                        let mut done = run.await;
                        done.status = TrialStatus::Failed;
                        done.stages = None;
                        TRIAL_TIMEOUT.clone_into(&mut done.error_code);
                        done
                    }
                };
                drop(permit);
                (index, trial)
            });
            spawned.insert(task.id(), (index, attempt, tracked));
        }
    }
    let mut found: Vec<Vec<TrialReport>> = selected.iter().map(|_| Vec::new()).collect();
    let mut panicked = false;
    while let Some(joined) = tasks.join_next_with_id().await {
        match joined {
            Ok((_, (index, trial))) => {
                if let Some(slot) = found.get_mut(index) {
                    slot.push(trial);
                }
            }
            // A panicked trial is a failed one; the other trials stop through the token.
            Err(error) => {
                tracing::error!(%error, "eval trial panicked");
                cancel.cancel();
                panicked = true;
                if let Some((index, repeat, integration)) = spawned.get(&error.id())
                    && let Some(slot) = found.get_mut(*index)
                {
                    slot.push(panicked_trial(*repeat, integration.trial_sessions()));
                }
            }
        }
    }
    if panicked {
        for (_, _, integration) in spawned.values() {
            integration.remove_sessions(config.session_timeout).await;
        }
    }
    let branches = counters.branches();
    let git = arguments.git_binary.as_str();
    let cleaned = delete_run_branches(
        &config,
        &CancellationToken::new(),
        git,
        &eval.session_branch_prefix,
        &branches,
    )
    .await;
    let mut reports = Vec::new();
    for case in &cases {
        let position = selected.iter().position(|chosen| chosen.id == case.id);
        let mut trials = position
            .and_then(|at| found.get_mut(at))
            .map(std::mem::take);
        if let Some(list) = trials.as_mut() {
            list.sort_by_key(|trial| trial.repeat);
        }
        let kind = if case.is_defect() {
            CaseKind::Defect
        } else {
            CaseKind::Clean
        };
        let skipped = trials.is_none();
        reports.push(CaseReport::new(
            case.id.clone(),
            kind,
            skipped,
            trials.unwrap_or_default(),
        ));
    }
    let rates = totals(&reports);
    // A cancelled run measured part of the cases, so no threshold is judged on it.
    let gate = if cancel.is_cancelled() {
        Gate::not_evaluated(min_recall, max_false_blocking)
    } else {
        evaluate_gate(&rates, min_recall, max_false_blocking)
    };
    let status = exit_status(&gate, rates.failed_trials);
    print_json(&RunReport {
        schema: REPORT_SCHEMA,
        pipeline: arguments.pipeline.clone(),
        policy_digest: policy.digest().to_owned(),
        repeat: arguments.repeat,
        duration_ms: i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX),
        usage: Usage {
            trials: selected.len() * repeat,
            sessions_started: counters.sessions.load(Ordering::SeqCst),
            would_publish: counters.would_publish.load(Ordering::SeqCst),
        },
        cases: reports,
        totals: rates,
        gate,
        branches_deleted: cleaned.deleted,
        branches_left: cleaned.left,
        sessions_left: counters
            .sessions_left()
            .into_iter()
            .map(|(host, session_id)| RunSession { host, session_id })
            .collect(),
    })?;
    match status {
        0 => Ok(()),
        EXIT_THRESHOLD => Err(Error::workflow(THRESHOLD_NOT_MET, true, None)),
        _ => Err(Error::workflow(TRIALS_FAILED, true, None)),
    }
}
