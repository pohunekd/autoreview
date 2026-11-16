//! The outcome of every published finding: the `finding_outcomes` table.
//!
//! The table has no foreign key to `reviews`, so pruning a review leaves its outcomes, and an
//! engine that does not know the table opens the file unchanged.

// Rust guideline compliant 2026-06-26

use autoreview_go_compat::go_time::format_rfc3339_nano;
use rusqlite::{Connection, Row, params};
use time::OffsetDateTime;

use super::Store;
use super::reviews::{time_key, unavailable};
use crate::Error;

/// The DDL of the outcomes, run after `ROLLUP_SCHEMA`; a store that predates it gains it.
pub(super) const OUTCOMES_SCHEMA: &str = concat!(
    "\n",
    "CREATE TABLE IF NOT EXISTS finding_outcomes (\n",
    "\trepository       TEXT NOT NULL,\n",
    "\tnumber           INTEGER NOT NULL,\n",
    "\tfinding_key      TEXT NOT NULL,\n",
    "\ttracked_key      TEXT NOT NULL,\n",
    "\tworkflow_id      TEXT NOT NULL,\n",
    "\tfinding_id       TEXT NOT NULL,\n",
    "\thead_sha         TEXT NOT NULL,\n",
    "\treviewer_id      TEXT NOT NULL,\n",
    "\tpipeline         TEXT NOT NULL,\n",
    "\trule             TEXT NOT NULL,\n",
    "\tseverity         TEXT NOT NULL,\n",
    "\tconfidence       TEXT NOT NULL,\n",
    "\tpath             TEXT NOT NULL,\n",
    "\tline             INTEGER NOT NULL,\n",
    "\tblocking         INTEGER NOT NULL,\n",
    "\toutcome          TEXT NOT NULL,\n",
    "\toutcome_head_sha TEXT NOT NULL,\n",
    "\tpublished_at     TEXT NOT NULL,\n",
    "\tupdated_at       TEXT NOT NULL,\n",
    "\tPRIMARY KEY (repository, number, finding_key)\n",
    ");\n",
    "CREATE INDEX IF NOT EXISTS finding_outcomes_tracked ",
    "ON finding_outcomes (repository, number, tracked_key);\n",
);

/// Outcome of a published finding that nothing has settled yet.
pub const OUTCOME_OPEN: &str = "open";

/// Columns of every outcome read, in the order the row reader expects.
const OUTCOME_COLUMNS: &str = "repository, number, finding_key, tracked_key, workflow_id, \
     finding_id, head_sha, reviewer_id, pipeline, rule, severity, confidence, path, line, \
     blocking, outcome, outcome_head_sha, published_at, updated_at";

/// One published finding and what became of it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FindingOutcome {
    /// Repository as `owner/name`.
    pub repository: String,
    /// Pull request number.
    pub number: i64,
    /// Key of the finding: its head cut to 12 characters, a colon and its ID.
    pub finding_key: String,
    /// Key that later reviews of the pull request settle the finding by; it is `finding_key`.
    pub tracked_key: String,
    /// Workflow ID of the review that published the finding first.
    pub workflow_id: String,
    /// Finding ID within its review.
    pub finding_id: String,
    /// Head SHA that the finding was published on.
    pub head_sha: String,
    /// Reviewer ID of the finding; the first reviewer of a merged finding.
    pub reviewer_id: String,
    /// Name of the pipeline of the review.
    pub pipeline: String,
    /// The `match` of the policy rule of the review.
    pub rule: String,
    /// Severity of the finding.
    pub severity: String,
    /// Confidence of the finding.
    pub confidence: String,
    /// File path of the finding.
    pub path: String,
    /// Line of the finding.
    pub line: i64,
    /// Whether the finding held the merge back when it was published.
    pub blocking: bool,
    /// The outcome: `open`, `refuted` or `not_blocking`.
    pub outcome: String,
    /// Head SHA that the outcome was decided at; empty while the outcome is `open`.
    pub outcome_head_sha: String,
    /// Time the finding was published, RFC 3339 in UTC.
    pub published_at: String,
    /// Time the row was last written, RFC 3339 in UTC.
    pub updated_at: String,
}

impl Store {
    /// Adds the rows of the published findings of one review in one transaction.
    ///
    /// A finding whose key is stored already keeps its row, so a resumed or adopted
    /// publication adds nothing.
    ///
    /// # Errors
    ///
    /// Returns `store_unavailable` when SQLite fails; then nothing is written.
    pub fn record_outcomes(&self, published: &[FindingOutcome]) -> Result<(), Error> {
        let now = self.timestamp();
        self.in_tx(|transaction| {
            for outcome in published {
                insert(transaction, outcome, &now)?;
            }
            Ok(())
        })
    }

    /// Returns the outcomes of `repository` published at or after `since`, oldest first.
    ///
    /// # Errors
    ///
    /// Returns `store_unavailable` when SQLite fails.
    pub fn outcomes_since(
        &self,
        repository: &str,
        since: OffsetDateTime,
    ) -> Result<Vec<FindingOutcome>, Error> {
        let since = format_rfc3339_nano(since);
        let connection = self.lock_connection()?;
        let mut statement = connection
            .prepare(&format!(
                "SELECT {OUTCOME_COLUMNS} FROM finding_outcomes \
                 WHERE repository = ?1 AND {} >= {} \
                 ORDER BY {}, number, finding_key",
                time_key("published_at"),
                time_key("?2"),
                time_key("published_at"),
            ))
            .map_err(unavailable)?;
        let rows = statement
            .query_map(params![repository, since], read_row)
            .map_err(unavailable)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(unavailable)
    }

    /// Deletes the outcomes last written before `cutoff` and returns how many it deleted.
    ///
    /// # Errors
    ///
    /// Returns `store_unavailable` when SQLite fails; then nothing is deleted.
    pub fn prune_outcomes(&self, cutoff: OffsetDateTime) -> Result<i64, Error> {
        let cutoff = format_rfc3339_nano(cutoff);
        self.in_tx(|transaction| {
            let rows = transaction
                .execute(
                    &format!(
                        "DELETE FROM finding_outcomes WHERE {} < {}",
                        time_key("updated_at"),
                        time_key("?1")
                    ),
                    [cutoff],
                )
                .map_err(unavailable)?;
            Ok(i64::try_from(rows).unwrap_or(i64::MAX))
        })
    }
}

/// Adds the row of one published finding unless its key is stored already.
fn insert(connection: &Connection, outcome: &FindingOutcome, now: &str) -> Result<(), Error> {
    connection
        .execute(
            &format!(
                "INSERT INTO finding_outcomes ({OUTCOME_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, \
                 ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19) \
                 ON CONFLICT (repository, number, finding_key) DO NOTHING"
            ),
            params![
                outcome.repository,
                outcome.number,
                outcome.finding_key,
                outcome.tracked_key,
                outcome.workflow_id,
                outcome.finding_id,
                outcome.head_sha,
                outcome.reviewer_id,
                outcome.pipeline,
                outcome.rule,
                outcome.severity,
                outcome.confidence,
                outcome.path,
                outcome.line,
                outcome.blocking,
                outcome.outcome,
                outcome.outcome_head_sha,
                outcome.published_at,
                now
            ],
        )
        .map_err(unavailable)?;
    Ok(())
}

/// Reads one row in the column order of `OUTCOME_COLUMNS`.
fn read_row(row: &Row<'_>) -> rusqlite::Result<FindingOutcome> {
    Ok(FindingOutcome {
        repository: row.get(0)?,
        number: row.get(1)?,
        finding_key: row.get(2)?,
        tracked_key: row.get(3)?,
        workflow_id: row.get(4)?,
        finding_id: row.get(5)?,
        head_sha: row.get(6)?,
        reviewer_id: row.get(7)?,
        pipeline: row.get(8)?,
        rule: row.get(9)?,
        severity: row.get(10)?,
        confidence: row.get(11)?,
        path: row.get(12)?,
        line: row.get(13)?,
        blocking: row.get(14)?,
        outcome: row.get(15)?,
        outcome_head_sha: row.get(16)?,
        published_at: row.get(17)?,
        updated_at: row.get(18)?,
    })
}
