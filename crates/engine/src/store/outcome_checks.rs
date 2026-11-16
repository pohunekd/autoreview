//! The state reads of pull requests with open finding outcomes: `finding_outcome_pull_requests`.
//!
//! One row per pull request whose state was read for its outcomes, with its end once it was
//! merged or closed. The table has no foreign key, so an engine that does not know it opens
//! the file unchanged.

// Rust guideline compliant 2026-06-26

use autoreview_go_compat::go_time::format_rfc3339_nano;
use rusqlite::{Connection, params};
use time::OffsetDateTime;

use super::Store;
use super::outcomes::OUTCOME_OPEN;
use super::reviews::{time_key, unavailable};
use crate::Error;

/// The DDL of the outcome checks, run after `OUTCOMES_SCHEMA`; a store without it gains it.
pub(super) const OUTCOME_CHECKS_SCHEMA: &str = concat!(
    "\n",
    "CREATE TABLE IF NOT EXISTS finding_outcome_pull_requests (\n",
    "\trepository   TEXT NOT NULL,\n",
    "\tnumber       INTEGER NOT NULL,\n",
    "\tended        TEXT NOT NULL,\n",
    "\tend_head_sha TEXT NOT NULL,\n",
    "\tchecked_at   TEXT NOT NULL,\n",
    "\tPRIMARY KEY (repository, number)\n",
    ");\n",
);

/// Outcome of a finding that was still open when its pull request was merged.
pub const OUTCOME_OPEN_AT_MERGE: &str = "open_at_merge";

/// Outcome of a finding that was still open when its pull request was closed unmerged.
pub const OUTCOME_OPEN_AT_CLOSE: &str = "open_at_close";

/// The `ended` value of a merged pull request.
const ENDED_MERGED: &str = "merged";

/// The `ended` value of a pull request that was closed unmerged.
const ENDED_CLOSED: &str = "closed";

impl Store {
    /// Returns the pull requests with open outcomes whose state was not read since `cutoff`.
    ///
    /// A pull request whose state was never read is due. The order is repository, number.
    ///
    /// # Errors
    ///
    /// Returns `store_unavailable` when SQLite fails.
    pub fn outcome_checks_due(&self, cutoff: OffsetDateTime) -> Result<Vec<(String, i64)>, Error> {
        let cutoff = format_rfc3339_nano(cutoff);
        let connection = self.lock_connection()?;
        let mut statement = connection
            .prepare(&format!(
                "SELECT DISTINCT o.repository, o.number FROM finding_outcomes o \
                 LEFT JOIN finding_outcome_pull_requests p \
                 ON p.repository = o.repository AND p.number = o.number \
                 WHERE o.outcome = ?1 AND (p.checked_at IS NULL OR {} < {}) \
                 ORDER BY o.repository, o.number",
                time_key("p.checked_at"),
                time_key("?2"),
            ))
            .map_err(unavailable)?;
        let rows = statement
            .query_map(params![OUTCOME_OPEN, cutoff], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .map_err(unavailable)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(unavailable)
    }

    /// Stores that the state of the pull request was read now; its outcomes stay as they are.
    ///
    /// # Errors
    ///
    /// Returns `store_unavailable` when SQLite fails.
    pub fn outcomes_checked(&self, repository: &str, number: i64) -> Result<(), Error> {
        let now = self.timestamp();
        self.in_tx(|transaction| {
            transaction
                .execute(
                    "INSERT INTO finding_outcome_pull_requests \
                     (repository, number, ended, end_head_sha, checked_at) \
                     VALUES (?1, ?2, '', '', ?3) ON CONFLICT (repository, number) \
                     DO UPDATE SET checked_at = excluded.checked_at",
                    params![repository, number, now],
                )
                .map_err(unavailable)?;
            Ok(())
        })
    }

    /// Ends the open outcomes of a merged or closed pull request at its head `head_sha`.
    ///
    /// A decided outcome stays; the pull request row records the end. Returns how many
    /// outcomes ended.
    ///
    /// # Errors
    ///
    /// Returns `store_unavailable` when SQLite fails; then nothing is written.
    pub fn end_outcomes(
        &self,
        repository: &str,
        number: i64,
        merged: bool,
        head_sha: &str,
    ) -> Result<i64, Error> {
        let (outcome, ended) = if merged {
            (OUTCOME_OPEN_AT_MERGE, ENDED_MERGED)
        } else {
            (OUTCOME_OPEN_AT_CLOSE, ENDED_CLOSED)
        };
        let now = self.timestamp();
        self.in_tx(|transaction| {
            let rows = transaction
                .execute(
                    "UPDATE finding_outcomes SET outcome = ?3, outcome_head_sha = ?4, \
                     updated_at = ?5 WHERE repository = ?1 AND number = ?2 AND outcome = ?6",
                    params![repository, number, outcome, head_sha, now, OUTCOME_OPEN],
                )
                .map_err(unavailable)?;
            transaction
                .execute(
                    "INSERT INTO finding_outcome_pull_requests \
                     (repository, number, ended, end_head_sha, checked_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT (repository, number) \
                     DO UPDATE SET ended = excluded.ended, \
                     end_head_sha = excluded.end_head_sha, checked_at = excluded.checked_at",
                    params![repository, number, ended, head_sha, now],
                )
                .map_err(unavailable)?;
            Ok(i64::try_from(rows).unwrap_or(i64::MAX))
        })
    }
}

/// Deletes the pull request rows last read before `cutoff`, an RFC 3339 time.
pub(super) fn prune(connection: &Connection, cutoff: &str) -> Result<(), Error> {
    connection
        .execute(
            &format!(
                "DELETE FROM finding_outcome_pull_requests WHERE {} < {}",
                time_key("checked_at"),
                time_key("?1")
            ),
            [cutoff],
        )
        .map_err(unavailable)?;
    Ok(())
}
