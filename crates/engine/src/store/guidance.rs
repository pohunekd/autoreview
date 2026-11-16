//! The categories of published findings and the guidance suggestions raised from them.
//!
//! Neither table has a foreign key, so an engine that does not know them opens the file
//! unchanged.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeMap;

use autoreview_go_compat::go_time::{format_rfc3339_nano, parse_rfc3339};
use rusqlite::{Connection, params};
use time::OffsetDateTime;

use super::outcomes::OUTCOME_FIXED;
use super::recurrence::{FixedFinding, SuggestedFinding, Suggestion, due_suggestions};
use super::reviews::{time_key, unavailable};
use super::{Store, store_error};
use crate::Error;
use crate::config::GuidanceConfig;

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

    /// Raises the suggestions due in `repository` and returns those that `workflow_id` raised.
    ///
    /// A category is due when `rule.recurrence_threshold` findings of it were fixed within
    /// `rule.window` and after its last suggestion. Raising it records the time, so the same
    /// findings never count again. The count and the record share one transaction, so
    /// concurrent reviews raise a suggestion once. A resumed review gets back the suggestions
    /// it raised before, which it may deliver again.
    ///
    /// # Errors
    ///
    /// Returns `store_unavailable` when SQLite fails and `store_state_invalid` for a stored
    /// time or list that does not read; then nothing is raised.
    pub fn raise_suggestions(
        &self,
        repository: &str,
        workflow_id: &str,
        rule: GuidanceConfig,
    ) -> Result<Vec<Suggestion>, Error> {
        self.in_tx(|transaction| {
            // Read under the write lock, so the time order of suggestions and fixes is the
            // commit order.
            let now = (self.now)();
            let window_start =
                now.saturating_sub(rule.window.try_into().unwrap_or(time::Duration::MAX));
            let last = last_suggestions(transaction, repository)?;
            let fixed = fixed_findings(transaction, repository, window_start)?;
            let due = due_suggestions(&fixed, &last, rule.recurrence_threshold, window_start);
            for suggestion in &due {
                let findings = serde_json::to_string(&suggestion.findings)
                    .map_err(|err| store_error("store_state_invalid", true, err))?;
                transaction
                    .execute(
                        "INSERT INTO guidance_suggestions \
                         (repository, category, workflow_id, suggested_at, findings) \
                         VALUES (?1, ?2, ?3, ?4, ?5) \
                         ON CONFLICT (repository, category) DO UPDATE SET \
                         workflow_id = excluded.workflow_id, \
                         suggested_at = excluded.suggested_at, findings = excluded.findings",
                        params![
                            repository,
                            suggestion.category,
                            workflow_id,
                            format_rfc3339_nano(now),
                            findings
                        ],
                    )
                    .map_err(unavailable)?;
            }
            raised_by(transaction, repository, workflow_id)
        })
    }
}

/// Returns the time of the last suggestion of each category of `repository`.
fn last_suggestions(
    connection: &Connection,
    repository: &str,
) -> Result<BTreeMap<(String, String), OffsetDateTime>, Error> {
    let mut statement = connection
        .prepare("SELECT category, suggested_at FROM guidance_suggestions WHERE repository = ?1")
        .map_err(unavailable)?;
    let rows = statement
        .query_map([repository], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(unavailable)?;
    let mut last = BTreeMap::new();
    for row in rows {
        let (category, text) = row.map_err(unavailable)?;
        let time = read_time(&text)?;
        last.insert((repository.to_owned(), category), time);
    }
    Ok(last)
}

/// Returns the categorized findings of `repository` settled as fixed at or after `since`.
///
/// The outcome row is last written when the finding is settled, and no later statement
/// changes a decided row, so `updated_at` is the time of the fix.
fn fixed_findings(
    connection: &Connection,
    repository: &str,
    since: OffsetDateTime,
) -> Result<Vec<FixedFinding>, Error> {
    let fixed_at = time_key("o.updated_at");
    let mut statement = connection
        .prepare(&format!(
            "SELECT c.category, c.number, c.finding_key, c.body, o.updated_at \
             FROM finding_outcomes o JOIN finding_categories c \
             ON c.repository = o.repository AND c.number = o.number \
             AND c.finding_key = o.finding_key \
             WHERE o.repository = ?1 AND o.outcome = ?2 AND {fixed_at} >= {} \
             ORDER BY {fixed_at}, c.number, c.finding_key",
            time_key("?3")
        ))
        .map_err(unavailable)?;
    let rows = statement
        .query_map(
            params![repository, OUTCOME_FIXED, format_rfc3339_nano(since)],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    SuggestedFinding {
                        number: row.get(1)?,
                        finding_key: row.get(2)?,
                        body: row.get(3)?,
                    },
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .map_err(unavailable)?;
    let mut fixed = Vec::new();
    for row in rows {
        let (category, finding, updated_at) = row.map_err(unavailable)?;
        fixed.push(FixedFinding {
            repository: repository.to_owned(),
            category,
            fixed_at: read_time(&updated_at)?,
            finding,
        });
    }
    Ok(fixed)
}

/// Returns the suggestions of `repository` that review `workflow_id` raised, by category.
fn raised_by(
    connection: &Connection,
    repository: &str,
    workflow_id: &str,
) -> Result<Vec<Suggestion>, Error> {
    let mut statement = connection
        .prepare(
            "SELECT category, findings FROM guidance_suggestions \
             WHERE repository = ?1 AND workflow_id = ?2 ORDER BY category",
        )
        .map_err(unavailable)?;
    let rows = statement
        .query_map(params![repository, workflow_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(unavailable)?;
    let mut raised = Vec::new();
    for row in rows {
        let (category, findings) = row.map_err(unavailable)?;
        raised.push(Suggestion {
            repository: repository.to_owned(),
            category,
            findings: serde_json::from_str(&findings)
                .map_err(|err| store_error("store_state_invalid", true, err))?,
        });
    }
    Ok(raised)
}

/// Reads a stored time.
fn read_time(text: &str) -> Result<OffsetDateTime, Error> {
    parse_rfc3339(text).map_err(|err| store_error("store_state_invalid", true, err))
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
