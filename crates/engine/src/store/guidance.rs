//! The categories of published findings and the guidance suggestions raised from them.
//!
//! Neither table has a foreign key, so an engine that does not know them opens the file
//! unchanged.

// Rust guideline compliant 2026-06-26

use rusqlite::{Connection, params};

use super::Store;
use super::reviews::{time_key, unavailable};
use crate::Error;

/// The DDL of the categories and suggestions, run after `OUTCOMES_SCHEMA`; a store that
/// predates it gains it.
pub(super) const GUIDANCE_SCHEMA: &str = concat!(
    "\n",
    "CREATE TABLE IF NOT EXISTS finding_categories (\n",
    "\trepository  TEXT NOT NULL,\n",
    "\tnumber      INTEGER NOT NULL,\n",
    "\tfinding_key TEXT NOT NULL,\n",
    "\tcategory    TEXT NOT NULL,\n",
    "\tbody        TEXT NOT NULL,\n",
    "\tPRIMARY KEY (repository, number, finding_key)\n",
    ");\n",
    "CREATE TABLE IF NOT EXISTS guidance_suggestions (\n",
    "\trepository   TEXT NOT NULL,\n",
    "\tcategory     TEXT NOT NULL,\n",
    "\tworkflow_id  TEXT NOT NULL,\n",
    "\tsuggested_at TEXT NOT NULL,\n",
    "\tfindings     TEXT NOT NULL,\n",
    "\tPRIMARY KEY (repository, category)\n",
    ");\n",
);

/// Category and text of one published finding, stored next to its outcome row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingCategory {
    /// Key of the finding, the `finding_key` of its outcome row.
    pub finding_key: String,
    /// Category slug the reviewer gave the finding.
    pub category: String,
    /// Body of the finding, as the reviewer wrote it.
    pub body: String,
}

impl Store {
    /// Stores the category and body of the findings that a review published on `number`.
    ///
    /// A finding whose key is stored already keeps its row, so a resumed publication changes
    /// nothing.
    ///
    /// # Errors
    ///
    /// Returns `store_unavailable` when SQLite fails; then nothing is written.
    pub fn record_categories(
        &self,
        repository: &str,
        number: i64,
        categories: &[FindingCategory],
    ) -> Result<(), Error> {
        self.in_tx(|transaction| {
            for item in categories {
                transaction
                    .execute(
                        "INSERT INTO finding_categories \
                         (repository, number, finding_key, category, body) \
                         VALUES (?1, ?2, ?3, ?4, ?5) \
                         ON CONFLICT (repository, number, finding_key) DO NOTHING",
                        params![
                            repository,
                            number,
                            item.finding_key,
                            item.category,
                            item.body
                        ],
                    )
                    .map_err(unavailable)?;
            }
            Ok(())
        })
    }
}

/// Deletes the categories whose outcome row is gone and the suggestions made before `cutoff`.
///
/// `cutoff` is an RFC 3339 time. Every outcome older than a suggestion that is older than
/// `cutoff` is pruned too, so no count needs the suggestion any more.
pub(super) fn prune(connection: &Connection, cutoff: &str) -> Result<(), Error> {
    connection
        .execute(
            "DELETE FROM finding_categories WHERE NOT EXISTS (SELECT 1 FROM finding_outcomes o \
             WHERE o.repository = finding_categories.repository \
             AND o.number = finding_categories.number \
             AND o.finding_key = finding_categories.finding_key)",
            [],
        )
        .map_err(unavailable)?;
    connection
        .execute(
            &format!(
                "DELETE FROM guidance_suggestions WHERE {} < {}",
                time_key("suggested_at"),
                time_key("?1")
            ),
            [cutoff],
        )
        .map_err(unavailable)?;
    Ok(())
}
