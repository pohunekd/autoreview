//! The `autoreview eval` commands.

// Rust guideline compliant 2026-06-26

use std::path::PathBuf;

use autoreview_engine::Error;
use autoreview_engine::eval::case::{CaseError, load_cases};
use clap::{Args, Subcommand};
use serde::Serialize;

use crate::print_json;

/// Error code of a cases directory with an invalid case; its document is the stdout object.
pub(crate) const EVAL_CASES_INVALID: &str = "eval_cases_invalid";

/// The `eval` commands.
#[derive(Debug, Subcommand)]
pub(crate) enum EvalCommand {
    /// Checks the case files of a directory, offline, and prints every error found.
    Validate(ValidateArguments),
}

/// The options of `eval validate`.
#[derive(Debug, Args)]
pub(crate) struct ValidateArguments {
    /// Directory of the case files, each named `<id>.yaml`.
    #[arg(long, value_name = "DIR")]
    cases: PathBuf,
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

/// Runs an `eval` command that reads no configuration.
///
/// # Errors
///
/// Returns `eval_cases_invalid` after printing the errors, and `workflow_failed` when stdout
/// cannot be written.
pub(crate) fn run(command: &EvalCommand) -> Result<(), Error> {
    match command {
        EvalCommand::Validate(arguments) => match load_cases(&arguments.cases) {
            Ok(cases) => {
                let defect = cases.iter().filter(|case| case.is_defect()).count();
                print_json(&ValidCases {
                    ok: true,
                    cases: cases.len(),
                    defect,
                    clean: cases.len().saturating_sub(defect),
                })
            }
            Err(errors) => {
                print_json(&InvalidCases { ok: false, errors })?;
                Err(Error::workflow(EVAL_CASES_INVALID, true, None))
            }
        },
    }
}
