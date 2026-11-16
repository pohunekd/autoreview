//! The outcome of every published finding: the `finding_outcomes` table.
//!
//! The table has no foreign key to `reviews`, so pruning a review leaves its outcomes, and an
//! engine that does not know the table opens the file unchanged.

// Rust guideline compliant 2026-06-26

use autoreview_go_compat::go_time::format_rfc3339_nano;
use rusqlite::{Connection, Row, params};
use time::OffsetDateTime;

use super::reviews::{time_key, unavailable};
use super::{Store, outcome_checks};
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

/// Outcome of a finding that a later review settled as fixed, or as answered and upheld.
pub const OUTCOME_FIXED: &str = "fixed";

/// Outcome of a finding that a later review settled as answered without an upheld check.
pub const OUTCOME_ANSWERED: &str = "answered";

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
    /// Key that later reviews of the pull request settle the finding by.
    ///
    /// It starts as `finding_key` and becomes the key of the new finding when the finding
    /// is published again. It is empty once a finding of another review reuses the key.
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
    /// The outcome: `open`, `refuted`, `not_blocking`, `fixed`, `answered`, `open_at_merge`
    /// or `open_at_close`.
    pub outcome: String,
    /// Head SHA that the outcome was decided at; empty while the outcome is `open`.
    pub outcome_head_sha: String,
    /// Time the finding was published, RFC 3339 in UTC.
    pub published_at: String,
    /// Time the row was last written, RFC 3339 in UTC.
    pub updated_at: String,
}

/// What a published review decided about one earlier finding of its pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settlement {
    /// The open finding tracked as `key` gets `outcome` at the head of the review.
    Decided {
        /// Tracked key of the earlier finding.
        key: String,
        /// The new outcome.
        outcome: String,
    },
    /// The open finding tracked as `key` was published again and is tracked as `republished`.
    ///
    /// When an open row that no settlement of the review picks has `republished` already, the
    /// finding is untracked instead.
    Republished {
        /// Tracked key of the earlier finding.
        key: String,
        /// Key of the new finding that publishes it again.
        republished: String,
    },
    /// A new finding reuses `key`, so an open row of another review stops being tracked by it.
    Untracked {
        /// The tracked key that the row loses.
        key: String,
        /// Workflow IDs whose rows stay: the publishing review and the hand-off it adopts.
        kept: [String; 2],
    },
}

impl Store {
    /// Applies the settlements of one review, then adds the rows of its published findings.
    ///
    /// Only an `open` row changes, and a finding whose key is stored already keeps its row,
    /// so a resumed or adopted publication adds no row and changes no decided outcome.
    /// Everything runs in one transaction.
    ///
    /// # Errors
    ///
    /// Returns `store_unavailable` when SQLite fails; then nothing is written.
    pub fn record_outcomes(
        &self,
        repository: &str,
        number: i64,
        head_sha: &str,
        settlements: &[Settlement],
        published: &[FindingOutcome],
    ) -> Result<(), Error> {
        let now = self.timestamp();
        self.in_tx(|transaction| {
            // Every settlement picks its rows before any change: a key that one settlement
            // moves a row to can be the key that another one of the same review names.
            let mut picked = Vec::new();
            for settlement in settlements {
                let (Settlement::Decided { key, .. }
                | Settlement::Republished { key, .. }
                | Settlement::Untracked { key, .. }) = settlement;
                picked.push((open_rows(transaction, repository, number, key)?, settlement));
            }
            // A move onto a key that an open row no settlement here picks still has would put
            // two findings under one key, so the moving row is untracked instead.
            let chosen: Vec<i64> = picked.iter().flat_map(|(rows, _)| rows.clone()).collect();
            let mut taken = Vec::new();
            for (_, settlement) in &picked {
                let target = match settlement {
                    Settlement::Republished { republished, .. } => {
                        open_rows(transaction, repository, number, republished)?
                    }
                    _ => Vec::new(),
                };
                taken.push(target.iter().any(|row| !chosen.contains(row)));
            }
            for ((rows, settlement), taken) in picked.into_iter().zip(taken) {
                for row in rows {
                    if taken {
                        untrack(transaction, row, &now)?;
                    } else {
                        settle(transaction, row, head_sha, &now, settlement)?;
                    }
                }
            }
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
    /// The state reads of their pull requests last made before `cutoff` go in the same
    /// transaction.
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
                    [&cutoff],
                )
                .map_err(unavailable)?;
            outcome_checks::prune(transaction, &cutoff)?;
            Ok(i64::try_from(rows).unwrap_or(i64::MAX))
        })
    }
}

/// Returns the IDs of the open rows of the pull request tracked as `key`.
fn open_rows(
    connection: &Connection,
    repository: &str,
    number: i64,
    key: &str,
) -> Result<Vec<i64>, Error> {
    let mut statement = connection
        .prepare(
            "SELECT rowid FROM finding_outcomes \
             WHERE repository = ?1 AND number = ?2 AND tracked_key = ?3 AND outcome = ?4",
        )
        .map_err(unavailable)?;
    let rows = statement
        .query_map(params![repository, number, key, OUTCOME_OPEN], |row| {
            row.get(0)
        })
        .map_err(unavailable)?;
    rows.collect::<Result<Vec<i64>, _>>().map_err(unavailable)
}

/// Clears the tracked key of the row with the ID `row`, so no later settlement matches it.
fn untrack(connection: &Connection, row: i64, now: &str) -> Result<(), Error> {
    connection
        .execute(
            "UPDATE finding_outcomes SET tracked_key = '', updated_at = ?2 WHERE rowid = ?1",
            params![row, now],
        )
        .map_err(unavailable)?;
    Ok(())
}

/// Applies one settlement to the row with the ID `row`.
///
/// An untracked row keeps its outcome; an empty tracked key matches no later settlement.
fn settle(
    connection: &Connection,
    row: i64,
    head_sha: &str,
    now: &str,
    settlement: &Settlement,
) -> Result<(), Error> {
    match settlement {
        Settlement::Decided { outcome, .. } => connection.execute(
            "UPDATE finding_outcomes SET outcome = ?2, outcome_head_sha = ?3, updated_at = ?4 \
             WHERE rowid = ?1",
            params![row, outcome, head_sha, now],
        ),
        Settlement::Republished { republished, .. } => connection.execute(
            "UPDATE finding_outcomes SET tracked_key = ?2, updated_at = ?3 WHERE rowid = ?1",
            params![row, republished, now],
        ),
        Settlement::Untracked {
            kept: [publisher, origin],
            ..
        } => connection.execute(
            "UPDATE finding_outcomes SET tracked_key = '', updated_at = ?4 \
             WHERE rowid = ?1 AND workflow_id NOT IN (?2, ?3)",
            params![row, publisher, origin, now],
        ),
    }
    .map_err(unavailable)?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use time::OffsetDateTime;

    use super::{FindingOutcome, OUTCOME_OPEN, Settlement};
    use crate::store::Store;

    #[test]
    fn a_move_onto_a_key_an_open_row_keeps_untracks_the_moving_row() {
        let dir = tempfile::TempDir::new().expect("create the store directory");
        let path = dir.path().join("state.db");
        let store = Store::open(&path, Box::new(OffsetDateTime::now_utc)).expect("open the store");
        let row = |number: i64, key: &str, tracked: &str| FindingOutcome {
            repository: "acme/repo".to_owned(),
            number,
            finding_key: key.to_owned(),
            tracked_key: tracked.to_owned(),
            outcome: OUTCOME_OPEN.to_owned(),
            published_at: "2026-01-02T03:04:05Z".to_owned(),
            ..FindingOutcome::default()
        };
        let rows = [1, 2].map(|n| [row(n, "a:O001", "a:O001"), row(n, "a:X001", "c:P001")]);
        let moved = |key: &str, republished: &str| Settlement::Republished {
            key: key.to_owned(),
            republished: republished.to_owned(),
        };
        // In pull request 1 no settlement picks the row at c:P001 (the prior list was cut
        // before it); in pull request 2 that row moves on, so its key is free.
        let settled = [
            vec![moved("a:O001", "c:P001")],
            vec![moved("a:O001", "c:P001"), moved("c:P001", "c:P002")],
        ];
        for (number, (rows, settled)) in (1..).zip(rows.iter().zip(&settled)) {
            let record = |settled: &[Settlement]| {
                let recorded = store.record_outcomes("acme/repo", number, "c", settled, rows);
                recorded.expect("record the outcomes");
            };
            record(&[]);
            record(settled);
        }
        // A resumed publication of pull request 1 changes nothing.
        let resumed = store.record_outcomes("acme/repo", 1, "c", &settled[0], &rows[0]);
        resumed.expect("record the outcomes again");
        let stored = store.outcomes_since("acme/repo", OffsetDateTime::UNIX_EPOCH);
        let stored = stored.expect("read the outcomes");
        let tracked = stored.iter().map(|row| (row.number, &*row.tracked_key));
        let want = [(1, ""), (1, "c:P001"), (2, "c:P001"), (2, "c:P002")];
        assert_eq!(tracked.collect::<Vec<_>>(), want);
    }
}
