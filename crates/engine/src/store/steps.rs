//! The runs of the review steps: the `review_step_runs` table.
//!
//! One row per review and step holds how the step's run ended, its retries and its duration.
//! The Go reference neither writes nor reads the table; its prune removes the rows with their
//! review through the foreign key.

// Rust guideline compliant 2026-06-26

use autoreview_go_compat::go_time::format_rfc3339_nano;
use rusqlite::{Connection, params};
use time::OffsetDateTime;

use super::Store;
use super::reviews::{time_key, unavailable};
use crate::Error;

/// The DDL of the step runs, run after the Go schema; a store that predates it gains the table.
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
    /// the outcome of the step, so a record exists only with a stored outcome.
    ///
    /// # Errors
    ///
    /// Returns `store_unavailable` when SQLite fails.
    pub(super) fn insert_step_run(
        &self,
        connection: &Connection,
        workflow_id: &str,
        run: &StepRun,
    ) -> Result<(), Error> {
        connection
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
        Ok(())
    }

    /// Lists the step runs that ended inside `range`, its first instant and the first after it.
    ///
    /// A row whose step or outcome this version does not know is left out.
    ///
    /// # Errors
    ///
    /// Returns `store_unavailable` when SQLite fails.
    pub(super) fn stats_step_runs(
        connection: &Connection,
        (start, end): (OffsetDateTime, OffsetDateTime),
    ) -> Result<Vec<StepRun>, Error> {
        let finished = time_key("finished_at");
        let (low, high) = (time_key("?1"), time_key("?2"));
        let sql = format!(
            "SELECT step, outcome, retries, duration_ms FROM review_step_runs \
             WHERE {finished} >= {low} AND {finished} < {high} ORDER BY workflow_id, step"
        );
        let mut statement = connection.prepare(&sql).map_err(unavailable)?;
        let (start, end) = (format_rfc3339_nano(start), format_rfc3339_nano(end));
        let rows = statement
            .query_map(params![start, end], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .map_err(unavailable)?;
        let mut runs = Vec::new();
        for row in rows {
            let (step, outcome, retries, duration_ms) = row.map_err(unavailable)?;
            if let (Some(step), Some(outcome)) =
                (ReviewStep::parse(&step), StepOutcome::parse(&outcome))
            {
                runs.push(StepRun {
                    step,
                    outcome,
                    retries,
                    duration_ms,
                });
            }
        }
        Ok(runs)
    }
}
