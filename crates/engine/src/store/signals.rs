//! The GitHub signals of published findings: the `finding_signals` table.
//!
//! A signal row sits beside the outcome row of the same key. The table has no foreign key, so
//! an engine that does not know it opens the file unchanged.

// Rust guideline compliant 2026-06-26

use autoreview_go_compat::go_time::format_rfc3339_nano;
use rusqlite::{Connection, params};
use time::OffsetDateTime;

use super::Store;
use super::outcomes::OUTCOME_OPEN;
use super::reviews::{time_key, unavailable};
use crate::Error;

/// Outcome of an open finding whose review, which requested changes, was dismissed on GitHub.
pub const OUTCOME_DISMISSED: &str = "dismissed";

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

/// What one signal read changes for one finding, as read for one review and finding ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalUpdate {
    /// Key of the outcome row.
    pub finding_key: String,
    /// GitHub ID of the review that the read was made for; the row must still point to it.
    pub review_id: i64,
    /// ID of the finding in that review; the row must still hold it.
    pub finding_id: String,
    /// Whether the finding, if still open, gets the outcome `dismissed`.
    pub dismissed: bool,
    /// The reactions of the pull request author now, sorted; they replace the stored ones.
    pub reactions: Vec<String>,
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
    /// Returns the pull requests with an open finding that can produce a signal and whose
    /// signals were not read since `cutoff`.
    ///
    /// A finding can produce one when its review requested changes or it has an inline comment.
    /// A pull request is due by its oldest read among those findings, so one that was never read
    /// comes first and one whose read keeps failing cannot hold back the rest.
    ///
    /// # Errors
    ///
    /// Returns `store_unavailable` when SQLite fails.
    pub fn signal_checks_due(&self, cutoff: OffsetDateTime) -> Result<Vec<(String, i64)>, Error> {
        let cutoff = format_rfc3339_nano(cutoff);
        let connection = self.lock_connection()?;
        let mut statement = connection
            .prepare(&format!(
                "SELECT s.repository, s.number FROM finding_signals s \
                 JOIN finding_outcomes o ON o.repository = s.repository \
                 AND o.number = s.number AND o.finding_key = s.finding_key \
                 WHERE o.outcome = ?1 AND (s.requested_changes = 1 OR s.finding_id <> '') \
                 GROUP BY s.repository, s.number HAVING MIN({0}) < {1} \
                 ORDER BY MIN({0}), s.repository, s.number",
                time_key("s.checked_at"),
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

    /// Stores that the signals of the pull request were read now, changing nothing else.
    ///
    /// # Errors
    ///
    /// Returns `store_unavailable` when SQLite fails.
    pub fn signals_checked(&self, repository: &str, number: i64) -> Result<(), Error> {
        self.apply_signals(repository, number, &[]).map(|_| ())
    }

    /// Applies one successful signal read of the pull request and stores that it was made.
    ///
    /// Each write holds only while the row still points to the review that was read: a finding
    /// with `dismissed` that is still `open` gets the outcome `dismissed` at the head of that
    /// review, and the reactions replace the stored ones only for the same review and finding
    /// ID. An outcome already decided is never rewritten. Returns how many outcomes became
    /// `dismissed`.
    ///
    /// # Errors
    ///
    /// Returns `store_unavailable` when SQLite fails; then nothing is written.
    pub fn apply_signals(
        &self,
        repository: &str,
        number: i64,
        updates: &[SignalUpdate],
    ) -> Result<i64, Error> {
        self.in_tx(|transaction| {
            // Read under the write lock, so `updated_at` follows the commit order.
            let now = self.timestamp();
            let mut dismissed = 0_i64;
            for update in updates {
                if update.dismissed {
                    let rows = transaction
                        .execute(
                            "UPDATE finding_outcomes SET outcome = ?4, updated_at = ?5, \
                             outcome_head_sha = (SELECT s.head_sha FROM finding_signals s \
                             WHERE s.repository = ?1 AND s.number = ?2 AND s.finding_key = ?3 \
                             AND s.review_id = ?6) \
                             WHERE repository = ?1 AND number = ?2 AND finding_key = ?3 \
                             AND outcome = ?7 AND EXISTS (SELECT 1 FROM finding_signals s \
                             WHERE s.repository = ?1 AND s.number = ?2 AND s.finding_key = ?3 \
                             AND s.review_id = ?6)",
                            params![
                                repository,
                                number,
                                update.finding_key,
                                OUTCOME_DISMISSED,
                                now,
                                update.review_id,
                                OUTCOME_OPEN
                            ],
                        )
                        .map_err(unavailable)?;
                    dismissed += i64::try_from(rows).unwrap_or(i64::MAX);
                }
                transaction
                    .execute(
                        "UPDATE finding_signals SET author_reactions = ?4 \
                         WHERE repository = ?1 AND number = ?2 AND finding_key = ?3 \
                         AND review_id = ?5 AND finding_id = ?6",
                        params![
                            repository,
                            number,
                            update.finding_key,
                            update.reactions.join(","),
                            update.review_id,
                            update.finding_id
                        ],
                    )
                    .map_err(unavailable)?;
            }
            transaction
                .execute(
                    "UPDATE finding_signals SET checked_at = ?3 \
                     WHERE repository = ?1 AND number = ?2",
                    params![repository, number, now],
                )
                .map_err(unavailable)?;
            Ok(dismissed)
        })
    }

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
    use time::{Duration, OffsetDateTime};

    use super::{SignalIds, SignalUpdate};
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

    /// A write holds only for the review and finding ID that were read, a dismissal only for an
    /// open row, and a write stamps the read so the pull request is not due again at once.
    #[test]
    fn a_signal_write_changes_only_the_row_that_was_read() {
        let dir = tempfile::TempDir::new().expect("create the store directory");
        let path = dir.path().join("state.db");
        let store = Store::open(&path, Box::new(OffsetDateTime::now_utc)).expect("open the store");
        let outcome = FindingOutcome {
            repository: "acme/widgets".to_owned(),
            number: 1,
            finding_key: "a:O001".to_owned(),
            tracked_key: "a:O001".to_owned(),
            outcome: OUTCOME_OPEN.to_owned(),
            published_at: "2026-01-02T03:04:05Z".to_owned(),
            ..FindingOutcome::default()
        };
        let recorded = store.record_outcomes("acme/widgets", 1, "a", &[], &[outcome]);
        recorded.expect("record the outcomes");
        let ids = SignalIds {
            tracked_key: "a:O001".to_owned(),
            review_id: 7,
            head_sha: "c".to_owned(),
            finding_id: "O001".to_owned(),
            requested_changes: true,
        };
        store
            .record_signal_ids("acme/widgets", 1, &[ids])
            .expect("store");
        let now = OffsetDateTime::now_utc();
        let due = |cutoff: OffsetDateTime| store.signal_checks_due(cutoff).expect("due");
        let pull_request = vec![("acme/widgets".to_owned(), 1)];
        assert_eq!(due(now - Duration::hours(1)), pull_request, "never read");
        let update = |review_id: i64| SignalUpdate {
            finding_key: "a:O001".to_owned(),
            review_id,
            finding_id: "O001".to_owned(),
            dismissed: true,
            reactions: vec!["+1".to_owned()],
        };
        let apply = |update: SignalUpdate| store.apply_signals("acme/widgets", 1, &[update]);
        let state = || {
            let rows = store.signal_rows("acme/widgets", 1).expect("read the rows");
            (rows[0].outcome.clone(), rows[0].author_reactions.clone())
        };
        // A read of another review changes nothing but the time of the read.
        assert_eq!(apply(update(8)).expect("apply"), 0);
        assert_eq!(state(), ("open".to_owned(), Vec::<String>::new()));
        assert!(due(now - Duration::hours(1)).is_empty(), "read just now");
        assert_eq!(
            due(now + Duration::hours(1)),
            pull_request,
            "read before the cutoff"
        );
        assert_eq!(apply(update(7)).expect("apply"), 1);
        assert_eq!(state(), ("dismissed".to_owned(), vec!["+1".to_owned()]));
        let listed = store.list_outcomes("acme/widgets", OffsetDateTime::UNIX_EPOCH);
        assert_eq!(listed.expect("list")[0].finding.outcome_head_sha, "c");
        // A decided outcome is never rewritten, and a decided finding is not due.
        assert_eq!(apply(update(7)).expect("apply"), 0);
        assert!(due(now + Duration::hours(1)).is_empty());
    }
}
