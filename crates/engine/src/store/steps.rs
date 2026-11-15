//! The runs of the review steps: the `review_step_runs` table.
//!
//! One row per review and step holds how the step's run ended, its retries and its duration.
//! A pruned review takes its rows with it through the foreign key.

// Rust guideline compliant 2026-06-26

use autoreview_go_compat::go_time::format_rfc3339_nano;
use rusqlite::types::ToSql;
use rusqlite::{Connection, params};
use time::OffsetDateTime;

use super::reviews::{time_key, unavailable};
use super::{Store, store_error};
use crate::Error;

/// The DDL of the step runs, run after `STORE_SCHEMA`; a store that predates it gains the table.
pub(super) const STEP_RUNS_SCHEMA: &str = concat!(
    "\n",
    "CREATE TABLE IF NOT EXISTS review_step_runs (\n",
    "\tworkflow_id TEXT NOT NULL REFERENCES reviews (workflow_id) ON DELETE CASCADE,\n",
    "\tstep        TEXT NOT NULL,\n",
    "\toutcome     TEXT NOT NULL,\n",
    "\tretries     INTEGER NOT NULL,\n",
    "\tduration_ms INTEGER NOT NULL,\n",
    "\tfinished_at TEXT NOT NULL,\n",
    "\tPRIMARY KEY (workflow_id, step)\n",
    ");\n",
);

/// A step of the review workflow whose runs the store records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewStep {
    /// Reading the discussion of the prior findings from GitHub.
    Discussion,
    /// Running the reviewer sessions until each reviewer has a checked result.
    Reviewers,
    /// Running the verifier session on the blocking claims.
    Verifier,
    /// Publishing the rendered review on GitHub.
    Publication,
}

impl ReviewStep {
    /// Every step, in the order of the workflow.
    pub const ALL: [Self; 4] = [
        Self::Discussion,
        Self::Reviewers,
        Self::Verifier,
        Self::Publication,
    ];

    /// Returns the stored name of the step.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Discussion => "discussion",
            Self::Reviewers => "reviewers",
            Self::Verifier => "verifier",
            Self::Publication => "publication",
        }
    }

    /// Returns the step of a stored name, or `None` for a name this version does not know.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|step| step.as_str() == name)
    }
}

/// How one run of a step ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepOutcome {
    /// The step finished its work.
    Succeeded,
    /// The step stopped because a newer state of the pull request replaced the review.
    Superseded,
    /// The step failed with any other error.
    Failed,
}

impl StepOutcome {
    /// Every outcome.
    pub const ALL: [Self; 3] = [Self::Succeeded, Self::Superseded, Self::Failed];

    /// Returns the stored name of the outcome.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Superseded => "superseded",
            Self::Failed => "failed",
        }
    }

    /// Returns the outcome of a stored name, or `None` for a name this version does not know.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|outcome| outcome.as_str() == name)
    }
}

/// One recorded run of a step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StepRun {
    /// The step that ran.
    pub step: ReviewStep,
    /// How the run ended.
    pub outcome: StepOutcome,
    /// Repeated attempts inside the run: retried calls, session starts and reviewer runs.
    pub retries: i64,
    /// Milliseconds from the start of the run to its end, on the engine's monotonic clock.
    pub duration_ms: i64,
}

impl Store {
    /// Inserts the run of a step of the review `workflow_id`; the first record of a step wins.
    ///
    /// The end of the run is the store's current time. Call it in the transaction that stores
    /// the outcome of the step, so a record exists only with a stored outcome. Returns whether
    /// the row was inserted: `false` when the step already has its record.
    ///
    /// # Errors
    ///
    /// Returns `store_unavailable` when SQLite fails.
    pub(super) fn insert_step_run(
        &self,
        connection: &Connection,
        workflow_id: &str,
        run: &StepRun,
    ) -> Result<bool, Error> {
        let inserted = connection
            .execute(
                "INSERT INTO review_step_runs (workflow_id, step, outcome, retries, duration_ms, \
                 finished_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                 ON CONFLICT (workflow_id, step) DO NOTHING",
                params![
                    workflow_id,
                    run.step.as_str(),
                    run.outcome.as_str(),
                    run.retries,
                    run.duration_ms,
                    self.timestamp()
                ],
            )
            .map_err(unavailable)?;
        Ok(inserted == 1)
    }

    /// Lists the step runs that ended inside `range`, its first instant and the first after it.
    ///
    /// A row whose step or outcome this version does not know is left out.
    ///
    /// # Errors
    ///
    /// The errors of `collect_step_runs`.
    pub(super) fn stats_step_runs(
        connection: &Connection,
        (start, end): (OffsetDateTime, OffsetDateTime),
    ) -> Result<Vec<EndedStepRun>, Error> {
        let finished = time_key("finished_at");
        let (low, high) = (time_key("?1"), time_key("?2"));
        let sql = format!(
            "SELECT workflow_id, step, outcome, retries, duration_ms, finished_at \
             FROM review_step_runs WHERE {finished} >= {low} AND {finished} < {high} \
             ORDER BY workflow_id, step"
        );
        let (start, end) = (format_rfc3339_nano(start), format_rfc3339_nano(end));
        collect_step_runs(connection, &sql, &[&start, &end])
    }
}

/// One recorded run of a step with its review and the time it ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndedStepRun {
    /// Workflow identifier of the review the run belongs to.
    pub workflow_id: String,
    /// The run.
    pub run: StepRun,
    /// End of the run, as RFC 3339 text.
    pub finished_at: String,
}

/// Runs a step run select of `workflow_id, step, outcome, retries, duration_ms, finished_at`.
///
/// A row whose step or outcome this version does not know is left out.
///
/// # Errors
///
/// Returns `store_state_invalid` naming the review when a column does not read as its type,
/// and `store_unavailable` when SQLite fails.
pub(super) fn collect_step_runs(
    connection: &Connection,
    sql: &str,
    params: &[&dyn ToSql],
) -> Result<Vec<EndedStepRun>, Error> {
    let mut statement = connection.prepare(sql).map_err(unavailable)?;
    let mut rows = statement.query(params).map_err(unavailable)?;
    let mut runs = Vec::new();
    while let Some(row) = rows.next().map_err(unavailable)? {
        let workflow_id: String = row
            .get(0)
            .map_err(|err| step_run_invalid(&format!("workflow_id: {err}")))?;
        let column = |name: &str, err: rusqlite::Error| {
            step_run_invalid(&format!("of review {workflow_id} {name}: {err}"))
        };
        let step: String = row.get(1).map_err(|err| column("step", err))?;
        let outcome: String = row.get(2).map_err(|err| column("outcome", err))?;
        let retries: i64 = row.get(3).map_err(|err| column("retries", err))?;
        let duration_ms: i64 = row.get(4).map_err(|err| column("duration_ms", err))?;
        let finished_at: String = row.get(5).map_err(|err| column("finished_at", err))?;
        if let (Some(step), Some(outcome)) =
            (ReviewStep::parse(&step), StepOutcome::parse(&outcome))
        {
            runs.push(EndedStepRun {
                workflow_id,
                run: StepRun {
                    step,
                    outcome,
                    retries,
                    duration_ms,
                },
                finished_at,
            });
        }
    }
    Ok(runs)
}

/// Returns the `store_state_invalid` error of a step run row.
fn step_run_invalid(reason: &str) -> Error {
    store_error("store_state_invalid", true, format!("step run {reason}"))
}
