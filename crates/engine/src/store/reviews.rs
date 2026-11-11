//! Reviews, their steps and their state: the `reviews`, `review_steps` and `review_state` tables.

// Rust guideline compliant 2026-06-26

use autoreview_go_compat::go_time::format_rfc3339_nano;
use autoreview_schedule::scheduler::Run;
use rusqlite::types::ToSql;
use rusqlite::{Connection, Row, Transaction, params};
use serde::Serialize;
use serde::de::DeserializeOwned;
use time::OffsetDateTime;

use super::{Store, encode_go_json, store_error};
use crate::Error;
use crate::model::{ReviewInput, ReviewOutcome};

/// Status of a review that is dispatched and waits to run.
pub const REVIEW_QUEUED: &str = "queued";
/// Status of a review that a worker is running. The engine resumes it after a restart.
pub const REVIEW_RUNNING: &str = "running";
/// Status of a clean review that waits for an approval decision or its deadline.
pub const REVIEW_AWAITING: &str = "awaiting";
/// Status of a review with a final result.
pub const REVIEW_COMPLETED: &str = "completed";
/// Result of a completed review that counts as a success.
pub const RESULT_SUCCESS: &str = "success";
/// Result of a completed review that counts as a failure.
pub const RESULT_FAILURE: &str = "failure";

/// Columns of every review read, in the order the row reader expects.
const REVIEW_COLUMNS: &str = "workflow_id, review_key, input, status, result, failure, \
     released, cleanup_pending, approval, approval_deadline, outcome, created_at, updated_at, \
     completed_at";

/// Which reviews `list_reviews` returns, newest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewFilter {
    /// Every review.
    All,
    /// The reviews of one pull request, named by its `repository#number` key.
    ReviewKey(String),
    /// The awaiting reviews that have no approval decision yet.
    AwaitingUndecided,
    /// The reviews with a status and of a repository; `None` matches every value.
    Matching {
        /// Status that the review has, for example `awaiting`.
        status: Option<String>,
        /// Repository as `owner/name` whose pull requests the review is of.
        repository: Option<String>,
    },
}

/// One review row as the store returns it. The JSON keys are the Go field tags.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct StoredReview {
    /// Workflow identifier of the review.
    pub workflow_id: String,
    /// The `repository#number` key of the pull request.
    pub key: String,
    /// The input the review runs with. Not serialized.
    #[serde(skip)]
    pub input: ReviewInput,
    /// Status: `queued`, `running`, `awaiting` or `completed`.
    pub status: String,
    /// Result of a completed review: `success` or `failure`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub result: String,
    /// Failure code of a failed review.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub failure: String,
    /// Whether the review released its slot.
    pub released: bool,
    /// Whether cleanup of the review is still pending.
    pub cleanup_pending: bool,
    /// Approval decision: `approve`, `expired` or `superseded`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub approval: String,
    /// Deadline of the approval wait, as RFC 3339 text.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub approval_deadline: String,
    /// Raw JSON text of the review outcome.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub outcome: String,
    /// Creation time, as RFC 3339 text.
    pub created_at: String,
    /// Last update time, as RFC 3339 text.
    pub updated_at: String,
    /// Completion time, as RFC 3339 text.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub completed_at: String,
}

impl StoredReview {
    /// Reports whether the review is awaiting with no approval decision yet, the row form of
    /// `ReviewFilter::AwaitingUndecided` (a NULL `approval` decodes as empty).
    pub(crate) fn awaits_decision(&self) -> bool {
        self.status == REVIEW_AWAITING && self.approval.is_empty()
    }
}

impl Store {
    /// Records a dispatched review. A repeated workflow identifier keeps the existing review.
    ///
    /// Call it inside `in_tx`; it does not lock the connection.
    ///
    /// # Errors
    ///
    /// Returns `review_input_invalid` when the input cannot be encoded and `store_unavailable`
    /// when SQLite fails.
    pub fn enqueue_review(
        &self,
        transaction: &Transaction<'_>,
        workflow_id: &str,
        input: &ReviewInput,
    ) -> Result<(), Error> {
        let encoded =
            encode_go_json(input).map_err(|err| store_error("review_input_invalid", true, err))?;
        let now = self.timestamp();
        transaction
            .execute(
                "INSERT INTO reviews (workflow_id, review_key, input, status, created_at, \
                 updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT (workflow_id) DO NOTHING",
                params![
                    workflow_id,
                    input.pull_request.key(),
                    encoded,
                    REVIEW_QUEUED,
                    now,
                    now
                ],
            )
            .map_err(|err| store_error("store_unavailable", false, err))?;
        Ok(())
    }

    /// Reads the review with the workflow identifier, or `None` when there is none.
    ///
    /// # Errors
    ///
    /// Returns `store_state_invalid` when the stored input or a flag does not decode, and
    /// `store_unavailable` when SQLite fails.
    pub fn get_review(&self, workflow_id: &str) -> Result<Option<StoredReview>, Error> {
        let connection = self.lock_connection()?;
        let sql = format!("SELECT {REVIEW_COLUMNS} FROM reviews WHERE workflow_id = ?1");
        let mut statement = connection
            .prepare(&sql)
            .map_err(|err| store_error("store_unavailable", false, err))?;
        let row: Option<ReviewRow> = statement
            .query_row(params![workflow_id], read_review_row)
            .map(Some)
            .or_else(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(store_error("store_unavailable", false, other)),
            })?;
        row.map(decode_review_row).transpose()
    }

    /// Lists the reviews that match `filter`, newest first, at most `limit` of them.
    ///
    /// # Errors
    ///
    /// Returns `store_state_invalid` when a stored row does not decode, and `store_unavailable`
    /// when SQLite fails.
    pub fn list_reviews(
        &self,
        filter: &ReviewFilter,
        limit: i64,
    ) -> Result<Vec<StoredReview>, Error> {
        let connection = self.lock_connection()?;
        let created = time_key("created_at");
        match filter {
            ReviewFilter::All => {
                let sql = format!(
                    "SELECT {REVIEW_COLUMNS} FROM reviews \
                     ORDER BY {created} DESC, workflow_id LIMIT ?1"
                );
                collect_reviews(&connection, &sql, &[&limit])
            }
            ReviewFilter::ReviewKey(key) => {
                let sql = format!(
                    "SELECT {REVIEW_COLUMNS} FROM reviews WHERE review_key = ?1 \
                     ORDER BY {created} DESC, workflow_id LIMIT ?2"
                );
                collect_reviews(&connection, &sql, &[key, &limit])
            }
            ReviewFilter::AwaitingUndecided => {
                let sql = format!(
                    "SELECT {REVIEW_COLUMNS} FROM reviews WHERE status = ?1 AND approval IS NULL \
                     ORDER BY {created} DESC, workflow_id LIMIT ?2"
                );
                collect_reviews(&connection, &sql, &[&REVIEW_AWAITING, &limit])
            }
            ReviewFilter::Matching { status, repository } => {
                // The key is `repository#number`, so the repository is the text before `#`.
                let sql = format!(
                    "SELECT {REVIEW_COLUMNS} FROM reviews WHERE (?1 IS NULL OR status = ?1) \
                     AND (?2 IS NULL OR substr(review_key, 1, length(?2) + 1) = ?2 || '#') \
                     ORDER BY {created} DESC, workflow_id LIMIT ?3"
                );
                collect_reviews(&connection, &sql, &[status, repository, &limit])
            }
        }
    }

    /// Returns the newest reviews in the shape the scheduler plans with.
    ///
    /// A history larger than `limit` fails closed, so the scheduler never dispatches over a
    /// review it cannot see.
    ///
    /// # Errors
    ///
    /// Returns `review_history_limit_exceeded` when the history is larger than `limit`, and the
    /// errors of `list_reviews`.
    pub fn run_history(&self, limit: i64) -> Result<Vec<Run>, Error> {
        let reviews = self.list_reviews(&ReviewFilter::All, limit.wrapping_add(1))?;
        let count = i64::try_from(reviews.len()).unwrap_or(i64::MAX);
        if count > limit {
            return Err(Error::workflow("review_history_limit_exceeded", true, None));
        }
        Ok(reviews
            .into_iter()
            .map(|review| {
                let status = if review.status == REVIEW_COMPLETED {
                    REVIEW_COMPLETED.to_owned()
                } else {
                    REVIEW_RUNNING.to_owned()
                };
                Run {
                    target: review.input.pull_request.target(),
                    workflow_id: review.workflow_id,
                    attempt: review.input.attempt,
                    status,
                    result: review.result,
                    failure: review.failure,
                    released: review.released,
                    cleanup_pending: review.cleanup_pending,
                }
            })
            .collect())
    }

    /// Lists the reviews that the statistics count, oldest first.
    ///
    /// These are the reviews that are not completed and, with `range` (its first instant and
    /// the first instant after it), those created or completed inside it.
    ///
    /// # Errors
    ///
    /// Returns `store_state_invalid` when a stored row does not decode, and `store_unavailable`
    /// when SQLite fails.
    pub fn stats_reviews(
        &self,
        range: Option<(OffsetDateTime, OffsetDateTime)>,
    ) -> Result<Vec<StoredReview>, Error> {
        let connection = self.lock_connection()?;
        let created = time_key("created_at");
        let Some((start, end)) = range else {
            let sql = format!(
                "SELECT {REVIEW_COLUMNS} FROM reviews WHERE status != ?1 \
                 ORDER BY {created}, workflow_id"
            );
            return collect_reviews(&connection, &sql, &[&REVIEW_COMPLETED]);
        };
        let completed = time_key("completed_at");
        let (low, high) = (time_key("?2"), time_key("?3"));
        let sql = format!(
            "SELECT {REVIEW_COLUMNS} FROM reviews WHERE status != ?1 \
             OR ({created} >= {low} AND {created} < {high}) \
             OR (completed_at IS NOT NULL AND {completed} >= {low} AND {completed} < {high}) \
             ORDER BY {created}, workflow_id"
        );
        let (start, end) = (format_rfc3339_nano(start), format_rfc3339_nano(end));
        collect_reviews(&connection, &sql, &[&REVIEW_COMPLETED, &start, &end])
    }

    /// Lists the reviews that can make progress, oldest first: queued and running reviews, and
    /// awaiting reviews that have a decision.
    ///
    /// # Errors
    ///
    /// Returns `store_state_invalid` when a stored row does not decode, and `store_unavailable`
    /// when SQLite fails.
    pub fn ready_reviews(&self) -> Result<Vec<StoredReview>, Error> {
        let connection = self.lock_connection()?;
        let created = time_key("created_at");
        let sql = format!(
            "SELECT {REVIEW_COLUMNS} FROM reviews \
             WHERE status IN (?1, ?2) OR (status = ?3 AND approval IS NOT NULL) \
             ORDER BY {created}, workflow_id"
        );
        collect_reviews(
            &connection,
            &sql,
            &[&REVIEW_QUEUED, &REVIEW_RUNNING, &REVIEW_AWAITING],
        )
    }
}

/// Returns SQL that maps stored time text `operand` to a time-ordered key.
///
/// Both engines write UTC text with the fraction's trailing zeros trimmed, so as text
/// `...:05.5Z` sorts before `...:05Z`. The key is the text up to the seconds followed by the
/// fraction padded to nine digits.
fn time_key(operand: &str) -> String {
    format!(
        "(CASE WHEN instr({operand}, '.') > 0 \
         THEN substr({operand}, 1, instr({operand}, '.') - 1) \
         || substr(substr({operand}, instr({operand}, '.') + 1, \
         length({operand}) - instr({operand}, '.') - 1) || '000000000', 1, 9) \
         ELSE substr({operand}, 1, length({operand}) - 1) || '000000000' END)"
    )
}

/// Maps a SQLite failure to `store_unavailable`.
fn unavailable(err: rusqlite::Error) -> Error {
    store_error("store_unavailable", false, err)
}

/// One review row in the column order of `REVIEW_COLUMNS`.
struct ReviewRow {
    /// Workflow identifier of the review.
    workflow_id: String,
    /// Review key of the pull request.
    key: String,
    /// Encoded review input.
    input: String,
    /// Status of the review.
    status: String,
    /// Result of a completed review.
    result: Option<String>,
    /// Failure code of a failed review.
    failure: Option<String>,
    /// Release flag as stored: 0 or 1.
    released: i64,
    /// Cleanup flag as stored: 0 or 1.
    cleanup_pending: i64,
    /// Approval decision of the review.
    approval: Option<String>,
    /// Deadline of the approval wait.
    approval_deadline: Option<String>,
    /// Encoded review outcome.
    outcome: Option<String>,
    /// Creation time.
    created_at: String,
    /// Last update time.
    updated_at: String,
    /// Completion time.
    completed_at: Option<String>,
}

/// Reads one review row by column index, in the order of `REVIEW_COLUMNS`.
fn read_review_row(row: &Row<'_>) -> rusqlite::Result<ReviewRow> {
    Ok(ReviewRow {
        workflow_id: row.get(0)?,
        key: row.get(1)?,
        input: row.get(2)?,
        status: row.get(3)?,
        result: row.get(4)?,
        failure: row.get(5)?,
        released: row.get(6)?,
        cleanup_pending: row.get(7)?,
        approval: row.get(8)?,
        approval_deadline: row.get(9)?,
        outcome: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
        completed_at: row.get(13)?,
    })
}

/// Decodes one review row into the stored review.
fn decode_review_row(row: ReviewRow) -> Result<StoredReview, Error> {
    let input: ReviewInput = serde_json::from_str(&row.input).map_err(|err| {
        store_error(
            "store_state_invalid",
            true,
            format!("review {} input: {err}", row.workflow_id),
        )
    })?;
    Ok(StoredReview {
        workflow_id: row.workflow_id.clone(),
        key: row.key,
        input,
        status: row.status,
        result: row.result.unwrap_or_default(),
        failure: row.failure.unwrap_or_default(),
        released: decode_flag(row.released, &row.workflow_id, "released")?,
        cleanup_pending: decode_flag(row.cleanup_pending, &row.workflow_id, "cleanup_pending")?,
        approval: row.approval.unwrap_or_default(),
        approval_deadline: row.approval_deadline.unwrap_or_default(),
        outcome: row.outcome.unwrap_or_default(),
        created_at: row.created_at,
        updated_at: row.updated_at,
        completed_at: row.completed_at.unwrap_or_default(),
    })
}

/// Decodes a 0 or 1 flag column; any other value is invalid.
fn decode_flag(value: i64, workflow_id: &str, column: &str) -> Result<bool, Error> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(store_error(
            "store_state_invalid",
            true,
            format!("review {workflow_id} {column} is {value}, not 0 or 1"),
        )),
    }
}

/// Runs a review select and decodes every row in query order.
fn collect_reviews(
    connection: &Connection,
    sql: &str,
    params: &[&dyn ToSql],
) -> Result<Vec<StoredReview>, Error> {
    let mut statement = connection.prepare(sql).map_err(unavailable)?;
    let rows = statement
        .query_map(params, read_review_row)
        .map_err(unavailable)?;
    let mut reviews = Vec::new();
    for row in rows {
        let row = row.map_err(unavailable)?;
        reviews.push(decode_review_row(row)?);
    }
    Ok(reviews)
}

/// Reads one text column by workflow and name; no row gives `None`.
fn load_text(
    connection: &Connection,
    sql: &str,
    workflow_id: &str,
    name: &str,
) -> Result<Option<String>, Error> {
    let mut statement = connection.prepare(sql).map_err(unavailable)?;
    statement
        .query_row(params![workflow_id, name], |row| row.get::<_, String>(0))
        .map(Some)
        .or_else(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(unavailable(other)),
        })
}

/// Decodes optional JSON text; `None` stays `None`.
fn decode_text<T: DeserializeOwned>(
    kind: &str,
    name: &str,
    text: Option<String>,
) -> Result<Option<T>, Error> {
    text.map(|value| {
        serde_json::from_str(&value).map_err(|err| {
            store_error("store_state_invalid", true, format!("{kind} {name}: {err}"))
        })
    })
    .transpose()
}

impl Store {
    /// Moves a review from one non-final status to another.
    ///
    /// Returns `false` when the review does not have the status `from`.
    ///
    /// # Errors
    ///
    /// Returns `store_unavailable` when SQLite fails.
    pub fn set_review_status(
        &self,
        workflow_id: &str,
        from: &str,
        to: &str,
    ) -> Result<bool, Error> {
        let connection = self.lock_connection()?;
        let rows = connection
            .execute(
                "UPDATE reviews SET status = ?1, updated_at = ?2 \
                 WHERE workflow_id = ?3 AND status = ?4",
                params![to, self.timestamp(), workflow_id, from],
            )
            .map_err(unavailable)?;
        Ok(rows == 1)
    }

    /// Sets the `released` and `cleanup_pending` flags that are `Some`, in that order.
    ///
    /// # Errors
    ///
    /// Returns `store_unavailable` when SQLite fails.
    pub fn set_review_flags(
        &self,
        workflow_id: &str,
        released: Option<bool>,
        cleanup_pending: Option<bool>,
    ) -> Result<(), Error> {
        let connection = self.lock_connection()?;
        if let Some(released) = released {
            connection
                .execute(
                    "UPDATE reviews SET released = ?1, updated_at = ?2 \
                     WHERE workflow_id = ?3",
                    params![released, self.timestamp(), workflow_id],
                )
                .map_err(unavailable)?;
        }
        if let Some(cleanup_pending) = cleanup_pending {
            connection
                .execute(
                    "UPDATE reviews SET cleanup_pending = ?1, updated_at = ?2 \
                     WHERE workflow_id = ?3",
                    params![cleanup_pending, self.timestamp(), workflow_id],
                )
                .map_err(unavailable)?;
        }
        Ok(())
    }

    /// Marks a review as awaiting approval and records its deadline once.
    ///
    /// A resumed review keeps its first deadline.
    ///
    /// # Errors
    ///
    /// Returns `store_unavailable` when SQLite fails.
    #[rustfmt::skip]
    pub fn start_awaiting(
        &self,
        workflow_id: &str,
        deadline: OffsetDateTime,
    ) -> Result<(), Error> {
        let connection = self.lock_connection()?;
        connection
            .execute(
                "UPDATE reviews SET status = ?1, released = 1, \
                 approval_deadline = COALESCE(approval_deadline, ?2), updated_at = ?3 \
                 WHERE workflow_id = ?4",
                params![
                    REVIEW_AWAITING,
                    format_rfc3339_nano(deadline),
                    self.timestamp(),
                    workflow_id
                ],
            )
            .map_err(unavailable)?;
        Ok(())
    }

    /// Sets the approval decision unless one is already set, and returns the decision in effect.
    ///
    /// # Errors
    ///
    /// Returns `store_unavailable` when the transaction fails.
    pub fn decide_approval(&self, workflow_id: &str, decision: &str) -> Result<String, Error> {
        self.in_tx(|transaction| self.decide_approval_tx(transaction, workflow_id, decision))
    }

    /// Runs `decide_approval` inside the caller's transaction.
    ///
    /// Returns an empty string when the review does not exist. Do not call a `Store` method
    /// that locks the connection from inside the transaction.
    ///
    /// # Errors
    ///
    /// Returns `store_unavailable` when SQLite fails.
    pub fn decide_approval_tx(
        &self,
        transaction: &Transaction<'_>,
        workflow_id: &str,
        decision: &str,
    ) -> Result<String, Error> {
        transaction
            .execute(
                "UPDATE reviews SET approval = ?1, updated_at = ?2 \
                 WHERE workflow_id = ?3 AND approval IS NULL",
                params![decision, self.timestamp(), workflow_id],
            )
            .map_err(unavailable)?;
        let mut statement = transaction
            .prepare("SELECT approval FROM reviews WHERE workflow_id = ?1")
            .map_err(unavailable)?;
        statement
            .query_row(params![workflow_id], |row| row.get::<_, Option<String>>(0))
            .map(|approval| approval.unwrap_or_default())
            .or_else(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => Ok(String::new()),
                other => Err(unavailable(other)),
            })
    }

    /// Records the final result of a review. An empty `failure` is stored as no failure.
    ///
    /// # Errors
    ///
    /// Returns `store_state_invalid` when the outcome cannot be encoded and `store_unavailable`
    /// when SQLite fails.
    pub fn complete_review(
        &self,
        workflow_id: &str,
        result: &str,
        failure: &str,
        outcome: Option<&ReviewOutcome>,
    ) -> Result<(), Error> {
        let encoded = outcome
            .map(encode_go_json)
            .transpose()
            .map_err(|err| store_error("store_state_invalid", true, err))?;
        let failure_value = if failure.is_empty() {
            None
        } else {
            Some(failure.to_owned())
        };
        let connection = self.lock_connection()?;
        let now = self.timestamp();
        connection
            .execute(
                "UPDATE reviews SET status = ?1, result = ?2, failure = ?3, outcome = ?4, \
                 completed_at = ?5, updated_at = ?6 WHERE workflow_id = ?7",
                params![
                    REVIEW_COMPLETED,
                    result,
                    failure_value,
                    encoded,
                    now,
                    now,
                    workflow_id
                ],
            )
            .map_err(unavailable)?;
        Ok(())
    }

    /// Deletes the completed reviews that were last updated before `cutoff`.
    ///
    /// Returns the number of deleted reviews.
    ///
    /// # Errors
    ///
    /// Returns `store_unavailable` when SQLite fails.
    pub fn prune_reviews(&self, cutoff: OffsetDateTime) -> Result<i64, Error> {
        let connection = self.lock_connection()?;
        let sql = format!(
            "DELETE FROM reviews WHERE status = ?1 AND {} < {}",
            time_key("updated_at"),
            time_key("?2")
        );
        let rows = connection
            .execute(&sql, params![REVIEW_COMPLETED, format_rfc3339_nano(cutoff)])
            .map_err(unavailable)?;
        Ok(i64::try_from(rows).unwrap_or(i64::MAX))
    }

    /// Reads the output of a review step, or `None` when the step has not been saved.
    ///
    /// Call it outside `in_tx`.
    ///
    /// # Errors
    ///
    /// Returns `store_state_invalid` when the stored JSON does not read as `T`, and
    /// `store_unavailable` when SQLite fails.
    pub fn load_step<T: DeserializeOwned>(
        &self,
        workflow_id: &str,
        name: &str,
    ) -> Result<Option<T>, Error> {
        let connection = self.lock_connection()?;
        let text = load_text(
            &connection,
            "SELECT output FROM review_steps WHERE workflow_id = ?1 AND name = ?2",
            workflow_id,
            name,
        )?;
        decode_text("step", name, text)
    }

    /// Writes the output of a review step. A saved step replaces the previous output.
    ///
    /// # Errors
    ///
    /// Returns `store_state_invalid` when the value cannot be encoded and `store_unavailable`
    /// when SQLite fails.
    pub fn save_step<T: Serialize + ?Sized>(
        &self,
        workflow_id: &str,
        name: &str,
        value: &T,
    ) -> Result<(), Error> {
        let encoded =
            encode_go_json(value).map_err(|err| store_error("store_state_invalid", true, err))?;
        let connection = self.lock_connection()?;
        connection
            .execute(
                "INSERT INTO review_steps (workflow_id, name, output) VALUES (?1, ?2, ?3) \
                 ON CONFLICT (workflow_id, name) DO UPDATE SET output = excluded.output",
                params![workflow_id, name, encoded],
            )
            .map_err(unavailable)?;
        Ok(())
    }

    /// Reads a review state value, or `None` when it has not been set.
    ///
    /// # Errors
    ///
    /// Returns `store_state_invalid` when the stored JSON does not read as `T`, and
    /// `store_unavailable` when SQLite fails.
    pub fn get_state<T: DeserializeOwned>(
        &self,
        workflow_id: &str,
        name: &str,
    ) -> Result<Option<T>, Error> {
        let connection = self.lock_connection()?;
        let text = load_text(
            &connection,
            "SELECT value FROM review_state WHERE workflow_id = ?1 AND name = ?2",
            workflow_id,
            name,
        )?;
        decode_text("state", name, text)
    }

    /// Writes a review state value. A set value replaces the previous one.
    ///
    /// # Errors
    ///
    /// Returns `store_state_invalid` when the value cannot be encoded and `store_unavailable`
    /// when SQLite fails.
    pub fn set_state<T: Serialize + ?Sized>(
        &self,
        workflow_id: &str,
        name: &str,
        value: &T,
    ) -> Result<(), Error> {
        let encoded =
            encode_go_json(value).map_err(|err| store_error("store_state_invalid", true, err))?;
        let connection = self.lock_connection()?;
        connection
            .execute(
                "INSERT INTO review_state (workflow_id, name, value) VALUES (?1, ?2, ?3) \
                 ON CONFLICT (workflow_id, name) DO UPDATE SET value = excluded.value",
                params![workflow_id, name, encoded],
            )
            .map_err(unavailable)?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, PoisonError};

    use autoreview_policy::{
        Checkout, OnClean, OnFindings, Pipeline, Protocol, Reviewer, Rule, Trigger, TriggerKind,
    };
    use time::OffsetDateTime;

    use crate::Error;
    use crate::model::PullRequest;
    use crate::model::ReviewInput;
    use crate::model::ids::INPUT_VERSION;
    use crate::store::Store;

    use super::{RESULT_SUCCESS, ReviewFilter, StoredReview};

    /// Approval decision that approves a review, the Go `approvalKindApprove`.
    const APPROVAL_APPROVE: &str = "approve";

    /// Approval decision that supersedes a review, the Go `approvalKindSuperseded`.
    const APPROVAL_SUPERSEDED: &str = "superseded";

    /// Approval decision that expires a wait, the Go `approvalKindExpired`.
    const APPROVAL_EXPIRED: &str = "expired";

    /// Opens a store in a new database file under `dir`.
    fn open_store(dir: &std::path::Path) -> Store {
        Store::open(
            &dir.join("state.db"),
            Box::new(|| OffsetDateTime::UNIX_EPOCH),
        )
        .expect("open store")
    }

    /// The reviewer of the Go test helpers.
    fn test_reviewer() -> Reviewer {
        Reviewer {
            id: "opus".to_owned(),
            label: "Opus".to_owned(),
            protocol: Protocol::ClaudeStructured,
            agent_profile: "claude-profile".to_owned(),
            finding_prefix: "O".to_owned(),
        }
    }

    /// A rule for `acme/*` with one reviewer on the base checkout, as the Go `testRule` builds.
    fn test_rule(on_findings: OnFindings, on_clean: OnClean) -> Rule {
        let reviewer = test_reviewer();
        Rule {
            match_glob: "acme/*".to_owned(),
            trigger: Trigger {
                kind: TriggerKind::ReviewRequested,
                label: String::new(),
            },
            on_findings,
            on_clean,
            pipeline: Pipeline {
                name: "main".to_owned(),
                protocol: reviewer.protocol.clone(),
                reviewers: vec![reviewer.clone()],
                incremental_reviewers: vec![reviewer],
                prompt: "Review the pull request.".to_owned(),
                checkout: Checkout::Base,
                project_sources: vec!["manual".to_owned()],
                verifier: None,
            },
            ..Rule::default()
        }
    }

    /// A 40-character SHA made of one repeated character.
    fn sha(character: char) -> String {
        character.to_string().repeat(40)
    }

    /// The pull request of the Go `testPullRequest`.
    fn test_pull_request(number: i64) -> PullRequest {
        PullRequest {
            repository: "acme/widget".to_owned(),
            number,
            head_sha: sha('a'),
            review_request_id: 7,
            title: "Add widget".to_owned(),
            url: format!("https://example.invalid/acme/widget/pull/{number}"),
            author: "colleague".to_owned(),
            head_branch: "feature".to_owned(),
            base_branch: "main".to_owned(),
            base_sha: sha('b'),
            ..PullRequest::default()
        }
    }

    /// The review input of the Go `testInput`, limited to the fields these tests read.
    fn test_input(rule: Rule, pull_request: PullRequest) -> ReviewInput {
        ReviewInput {
            version: INPUT_VERSION,
            reviewers: rule.pipeline.reviewers.clone(),
            pull_request,
            rule,
            ..ReviewInput::default()
        }
    }

    /// Records a review in its own transaction, as the Go `enqueueTestReview` does.
    fn enqueue_test_review(store: &Store, workflow_id: &str, input: &ReviewInput) {
        store
            .in_tx(|transaction| store.enqueue_review(transaction, workflow_id, input))
            .expect("enqueue commits");
    }

    /// Approve, supersede and expiry all race for one wait. Whichever writer commits first sets
    /// the decision, and every writer reports that decision.
    #[test]
    fn approval_decision_is_set_once_and_every_writer_reports_the_winner() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = open_store(dir.path());
        let input = test_input(
            test_rule(OnFindings::RequestChanges, OnClean::Await),
            test_pull_request(4),
        );
        for round in 0..20 {
            let workflow_id = format!("prr-v2-race-{round}");
            enqueue_test_review(&store, &workflow_id, &input);
            let decisions = [APPROVAL_APPROVE, APPROVAL_SUPERSEDED, APPROVAL_EXPIRED];
            let results: Vec<Result<String, Error>> = std::thread::scope(|scope| {
                let handles: Vec<_> = decisions
                    .into_iter()
                    .map(|decision| {
                        let store = &store;
                        let workflow_id = workflow_id.as_str();
                        scope.spawn(move || store.decide_approval(workflow_id, decision))
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|handle| handle.join().expect("writer thread finishes"))
                    .collect()
            });
            let stored = store
                .get_review(&workflow_id)
                .expect("review reads")
                .expect("review exists");
            for (index, result) in results.into_iter().enumerate() {
                let decision = result
                    .unwrap_or_else(|err| panic!("round {round}: writer {index} failed: {err}"));
                assert_eq!(
                    decision, stored.approval,
                    "round {round}: writer {index} reported a different decision"
                );
            }
        }
    }

    /// The first approval decision stays; later writers report it.
    #[test]
    fn approval_decision_keeps_the_first_writer() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = open_store(dir.path());
        let input = test_input(
            test_rule(OnFindings::RequestChanges, OnClean::Await),
            test_pull_request(5),
        );
        enqueue_test_review(&store, "prr-v2-supersede-first", &input);
        assert_eq!(
            store
                .decide_approval("prr-v2-supersede-first", APPROVAL_SUPERSEDED)
                .expect("first decision"),
            APPROVAL_SUPERSEDED
        );
        assert_eq!(
            store
                .decide_approval("prr-v2-supersede-first", APPROVAL_APPROVE)
                .expect("approval after a supersede"),
            APPROVAL_SUPERSEDED
        );
        enqueue_test_review(&store, "prr-v2-approve-first", &input);
        assert_eq!(
            store
                .decide_approval("prr-v2-approve-first", APPROVAL_APPROVE)
                .expect("first decision"),
            APPROVAL_APPROVE
        );
        assert_eq!(
            store
                .decide_approval("prr-v2-approve-first", APPROVAL_EXPIRED)
                .expect("expiry after an approval"),
            APPROVAL_APPROVE
        );
    }

    /// The run history is the only view the scheduler plans with, so it must expose the attempt
    /// and target stored in the review input.
    #[test]
    fn run_history_exposes_the_stored_review_to_the_planner() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = open_store(dir.path());
        let pull_request = test_pull_request(77);
        let mut input = test_input(
            test_rule(OnFindings::RequestChanges, OnClean::Approve),
            pull_request.clone(),
        );
        input.attempt = 2;
        enqueue_test_review(&store, "prr-v2-history", &input);
        let runs = store.run_history(10).expect("run history reads");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].target, pull_request.target());
        assert_eq!(runs[0].attempt, 2);
        assert_eq!(runs[0].workflow_id, "prr-v2-history");
        assert!(runs[0].active());
        let err = store
            .run_history(0)
            .expect_err("a history above the limit must fail closed");
        assert_eq!(crate::error_code(&err), "review_history_limit_exceeded");
    }

    /// Two instants in one second order and prune by time, not by their text (#120).
    #[test]
    fn reviews_in_one_second_order_and_prune_by_time() {
        let dir = tempfile::tempdir().expect("temp dir");
        let whole = OffsetDateTime::UNIX_EPOCH;
        let half = whole + time::Duration::milliseconds(500);
        let clock = Arc::new(Mutex::new(whole));
        let now = Arc::clone(&clock);
        let store = Store::open(
            &dir.path().join("state.db"),
            Box::new(move || *now.lock().unwrap_or_else(PoisonError::into_inner)),
        )
        .expect("open store");
        let set_clock = |at: OffsetDateTime| {
            *clock.lock().unwrap_or_else(PoisonError::into_inner) = at;
        };
        let input = test_input(
            test_rule(OnFindings::RequestChanges, OnClean::Approve),
            test_pull_request(5),
        );
        enqueue_test_review(&store, "review-whole", &input);
        set_clock(half);
        enqueue_test_review(&store, "review-half", &input);
        let ids = |reviews: Vec<StoredReview>| -> Vec<String> {
            reviews
                .into_iter()
                .map(|review| review.workflow_id)
                .collect()
        };
        let listed = store.list_reviews(&ReviewFilter::All, 10).expect("list");
        assert_eq!(listed[0].created_at, "1970-01-01T00:00:00.5Z");
        assert_eq!(listed[1].created_at, "1970-01-01T00:00:00Z");
        assert_eq!(ids(listed), ["review-half", "review-whole"]);
        let ready = store.ready_reviews().expect("ready reviews");
        assert_eq!(ids(ready), ["review-whole", "review-half"]);
        set_clock(whole);
        store
            .complete_review("review-whole", RESULT_SUCCESS, "", None)
            .expect("complete");
        set_clock(half);
        store
            .complete_review("review-half", RESULT_SUCCESS, "", None)
            .expect("complete");
        assert_eq!(store.prune_reviews(whole).expect("prune"), 0);
        assert_eq!(store.prune_reviews(half).expect("prune"), 1);
        let left = store.list_reviews(&ReviewFilter::All, 10).expect("list");
        assert_eq!(ids(left), ["review-half"]);
    }
}
