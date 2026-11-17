//! The GitHub signals of published findings: the `finding_signals` table.
//!
//! A signal row sits beside the outcome row of the same key. The table has no foreign key, so
//! an engine that does not know it opens the file unchanged.

// Rust guideline compliant 2026-06-26

use rusqlite::{Connection, params};

use super::Store;
use super::reviews::unavailable;
use crate::Error;

/// The DDL of the signals, run after `OUTCOME_CHECKS_SCHEMA`; a store without it gains it.
///
/// `checked_at` is empty until the signals of the row were read once.
pub(super) const SIGNALS_SCHEMA: &str = concat!(
    "\n",
    "CREATE TABLE IF NOT EXISTS finding_signals (\n",
    "\trepository        TEXT NOT NULL,\n",
    "\tnumber            INTEGER NOT NULL,\n",
    "\tfinding_key       TEXT NOT NULL,\n",
    "\treview_id         INTEGER NOT NULL,\n",
    "\thead_sha          TEXT NOT NULL,\n",
    "\tfinding_id        TEXT NOT NULL,\n",
    "\trequested_changes INTEGER NOT NULL,\n",
    "\tauthor_reactions  TEXT NOT NULL,\n",
    "\tchecked_at        TEXT NOT NULL,\n",
    "\tPRIMARY KEY (repository, number, finding_key)\n",
    ");\n",
);

/// What one published review says about one of its findings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalIds {
    /// Key of the finding in the review that published it; the row tracked by it gets the IDs.
    pub tracked_key: String,
    /// GitHub ID of the review that published the finding.
    pub review_id: i64,
    /// Head SHA that the review was made on.
    pub head_sha: String,
    /// ID of the finding in that review; empty when the finding has no inline comment.
    pub finding_id: String,
    /// Whether the review requested changes.
    pub requested_changes: bool,
}

/// One finding with its outcome and the GitHub signals stored for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalRow {
    /// Key of the outcome row.
    pub finding_key: String,
    /// Outcome of the finding now.
    pub outcome: String,
    /// GitHub ID of the review that published the finding last.
    pub review_id: i64,
    /// ID of the finding in that review; empty when it has no inline comment.
    pub finding_id: String,
    /// Whether that review requested changes.
    pub requested_changes: bool,
    /// Reactions of the pull request author to the comment, sorted, as GitHub names them.
    pub author_reactions: Vec<String>,
}

impl Store {
    /// Returns the findings of one pull request that have signal IDs, ordered by key.
    ///
    /// # Errors
    ///
    /// Returns `store_unavailable` when SQLite fails.
    pub fn signal_rows(&self, repository: &str, number: i64) -> Result<Vec<SignalRow>, Error> {
        let connection = self.lock_connection()?;
        let mut statement = connection
            .prepare(
                "SELECT o.finding_key, o.outcome, s.review_id, s.finding_id, \
                 s.requested_changes, s.author_reactions \
                 FROM finding_signals s JOIN finding_outcomes o \
                 ON o.repository = s.repository AND o.number = s.number \
                 AND o.finding_key = s.finding_key \
                 WHERE s.repository = ?1 AND s.number = ?2 ORDER BY o.finding_key",
            )
            .map_err(unavailable)?;
        let rows = statement
            .query_map(params![repository, number], |row| {
                let reactions: String = row.get(5)?;
                Ok(SignalRow {
                    finding_key: row.get(0)?,
                    outcome: row.get(1)?,
                    review_id: row.get(2)?,
                    finding_id: row.get(3)?,
                    requested_changes: row.get(4)?,
                    author_reactions: split_reactions(&reactions),
                })
            })
            .map_err(unavailable)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(unavailable)
    }

    /// Stores the GitHub IDs of the findings that one published review carries.
    ///
    /// Each entry goes to the outcome rows tracked by its key, so a finding that publishes an
    /// earlier one again moves that row to the newest review, and a key that no row tracks gets
    /// nothing. A row keeps its reactions while its review stays the same. Everything runs in
    /// one transaction.
    ///
    /// # Errors
    ///
    /// Returns `store_unavailable` when SQLite fails; then nothing is written.
    pub fn record_signal_ids(
        &self,
        repository: &str,
        number: i64,
        ids: &[SignalIds],
    ) -> Result<(), Error> {
        self.in_tx(|transaction| {
            for entry in ids {
                transaction
                    .execute(
                        "INSERT INTO finding_signals (repository, number, finding_key, \
                         review_id, head_sha, finding_id, requested_changes, author_reactions, \
                         checked_at) \
                         SELECT repository, number, finding_key, ?4, ?5, ?6, ?7, '', '' \
                         FROM finding_outcomes \
                         WHERE repository = ?1 AND number = ?2 AND tracked_key = ?3 \
                         ON CONFLICT (repository, number, finding_key) DO UPDATE SET \
                         author_reactions = CASE WHEN excluded.review_id = \
                         finding_signals.review_id THEN finding_signals.author_reactions \
                         ELSE '' END, \
                         review_id = excluded.review_id, head_sha = excluded.head_sha, \
                         finding_id = excluded.finding_id, \
                         requested_changes = excluded.requested_changes",
                        params![
                            repository,
                            number,
                            entry.tracked_key,
                            entry.review_id,
                            entry.head_sha,
                            entry.finding_id,
                            entry.requested_changes
                        ],
                    )
                    .map_err(unavailable)?;
            }
            Ok(())
        })
    }
}

/// Splits the stored reaction set, a comma-joined text, into its names.
pub(super) fn split_reactions(stored: &str) -> Vec<String> {
    let names = stored.split(',').filter(|name| !name.is_empty());
    names.map(str::to_owned).collect()
}

/// Deletes the signal rows whose outcome row is gone.
pub(super) fn prune(connection: &Connection) -> Result<(), Error> {
    connection
        .execute(
            "DELETE FROM finding_signals WHERE NOT EXISTS (SELECT 1 FROM finding_outcomes o \
             WHERE o.repository = finding_signals.repository \
             AND o.number = finding_signals.number \
             AND o.finding_key = finding_signals.finding_key)",
            [],
        )
        .map_err(unavailable)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use time::OffsetDateTime;

    use super::SignalIds;
    use crate::store::{FindingOutcome, OUTCOME_OPEN, Store};

    /// The IDs follow the row that the key tracks, and a newer review owns the row.
    #[test]
    fn ids_follow_the_tracked_row_and_the_newest_review() {
        let dir = tempfile::TempDir::new().expect("create the store directory");
        let path = dir.path().join("state.db");
        let store = Store::open(&path, Box::new(OffsetDateTime::now_utc)).expect("open the store");
        let row = |key: &str, tracked: &str| FindingOutcome {
            repository: "acme/widgets".to_owned(),
            number: 1,
            finding_key: key.to_owned(),
            tracked_key: tracked.to_owned(),
            outcome: OUTCOME_OPEN.to_owned(),
            published_at: "2026-01-02T03:04:05Z".to_owned(),
            ..FindingOutcome::default()
        };
        let rows = [row("a:O001", "a:O001"), row("a:O002", "")];
        let recorded = store.record_outcomes("acme/widgets", 1, "a", &[], &rows);
        recorded.expect("record the outcomes");
        let ids = |key: &str, review: i64, finding: &str| SignalIds {
            tracked_key: key.to_owned(),
            review_id: review,
            head_sha: "c".to_owned(),
            finding_id: finding.to_owned(),
            requested_changes: true,
        };
        let stored = |store: &Store| {
            let rows = store.signal_rows("acme/widgets", 1).expect("read the rows");
            let rows = rows.into_iter();
            rows.map(|row| (row.finding_key, row.review_id, row.finding_id))
                .collect::<Vec<_>>()
        };
        // A key that no row tracks, as the untracked a:O002 and an unknown one, gets nothing.
        let first = [
            ids("a:O001", 7, "O001"),
            ids("a:O002", 7, "O002"),
            ids("c:P009", 7, "P009"),
        ];
        let write = |ids: &[SignalIds]| store.record_signal_ids("acme/widgets", 1, ids);
        write(&first).expect("store");
        assert_eq!(
            stored(&store),
            [("a:O001".to_owned(), 7, "O001".to_owned())]
        );
        // The same review again changes nothing.
        write(&first).expect("store");
        assert_eq!(
            stored(&store),
            [("a:O001".to_owned(), 7, "O001".to_owned())]
        );
        // A newer review that publishes the finding again, without a comment, owns the row.
        write(&[ids("a:O001", 8, "")]).expect("store");
        assert_eq!(stored(&store), [("a:O001".to_owned(), 8, String::new())]);
    }
}
