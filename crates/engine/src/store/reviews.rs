//! Reviews, their steps and their state: the `reviews`, `review_steps` and `review_state` tables.

// Rust guideline compliant 2026-06-26

use std::collections::{BTreeMap, BTreeSet};

use autoreview_go_compat::go_time::{format_rfc3339_nano, parse_rfc3339};
use autoreview_policy::AuthorClass;
use autoreview_schedule::scheduler::Run;
use rusqlite::types::ToSql;
use rusqlite::{Connection, Row, Transaction, TransactionBehavior, params};
use serde::Serialize;
use serde::de::DeserializeOwned;
use time::OffsetDateTime;

use super::discovery::DiscoveryState;
use super::rollup::{Rollup, add_rollup, pruned_requests, read_rollup};
use super::steps::{EndedStepRun, StepRun, collect_step_runs};
use super::{Store, encode_go_json, store_error};
use crate::Error;
use crate::engine::STEP_COMBINED;
use crate::model::pull_request::is_unset_class;
use crate::model::{ReviewInput, ReviewOutcome};
use crate::review::input::{STEP_PLAN, STEP_VERIFICATION};
use crate::stats::{first_publications, rollup};

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

/// One review row as the store returns it.
///
/// The JSON keys are the stored field tags; `author_class` is omitted when unset.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct StoredReview {
    /// Workflow identifier of the review.
    pub workflow_id: String,
    /// The `repository#number` key of the pull request.
    pub key: String,
    /// The author class stored in the input; omitted for a review stored before classes.
    #[serde(skip_serializing_if = "is_unset_class")]
    pub author_class: AuthorClass,
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

/// What `Store::stats_snapshot` reads in one transaction.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatsSnapshot {
    /// The review rows of `stats_reviews`.
    pub reviews: Vec<StoredReview>,
    /// The discovery state.
    pub discovery: DiscoveryState,
    /// The step runs that ended inside the range; empty without one.
    pub steps: Vec<EndedStepRun>,
    /// The rollup of the days of the range; empty without one.
    pub rollup: Rollup,
    /// The stored plans of the reviews completed inside the range, by workflow ID, undecoded;
    /// empty without a range.
    pub plans: BTreeMap<String, String>,
    /// The verifier's steps of the reviews completed inside the range; empty without a range.
    pub verifier: VerifierSteps,
    /// The review requests whose first publication was pruned, as `repository#number#request
    /// ID`; empty without a range.
    pub pruned_requests: BTreeSet<String>,
}

/// The verifier's steps of the reviews that a statistics read counts, by workflow ID, undecoded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VerifierSteps {
    /// The stored verification of each review that ran the verifier.
    pub verifications: BTreeMap<String, String>,
    /// The combined artifact of each of those reviews; it names each finding's reviewer.
    pub combined: BTreeMap<String, String>,
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
    /// review it cannot see. A creation time that does not parse leaves `started_at` unset.
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
                    author: review.input.pull_request.author,
                    rule: review.input.rule.match_glob,
                    started_at: parse_rfc3339(&review.created_at).ok(),
                }
            })
            .collect())
    }

    /// Lists the reviews that the statistics count, oldest first.
    ///
    /// These are the reviews that are not completed and, with `range` (its first instant and
    /// the first instant after it), those created or completed inside it and every review of a
    /// pull request with a review completed inside it, so the first publication of a review
    /// request is known. Without `range` the read goes through the status index, so it does
    /// not grow with the completed reviews.
    ///
    /// # Errors
    ///
    /// Returns `store_state_invalid` when a stored row does not decode, and `store_unavailable`
    /// when SQLite fails.
    fn stats_reviews(
        connection: &Connection,
        range: Option<(OffsetDateTime, OffsetDateTime)>,
    ) -> Result<Vec<StoredReview>, Error> {
        let created = time_key("created_at");
        let Some((start, end)) = range else {
            let sql = format!(
                "SELECT {REVIEW_COLUMNS} FROM reviews WHERE status IN (?1, ?2, ?3) \
                 ORDER BY {created}, workflow_id"
            );
            let active: [&dyn ToSql; 3] = [&REVIEW_QUEUED, &REVIEW_RUNNING, &REVIEW_AWAITING];
            return collect_reviews(connection, &sql, &active);
        };
        let completed = time_key("completed_at");
        let (low, high) = (time_key("?2"), time_key("?3"));
        let sql = format!(
            "SELECT {REVIEW_COLUMNS} FROM reviews WHERE status != ?1 \
             OR ({created} >= {low} AND {created} < {high}) \
             OR (completed_at IS NOT NULL AND {completed} >= {low} AND {completed} < {high}) \
             OR review_key IN (SELECT review_key FROM reviews WHERE completed_at IS NOT NULL \
             AND {completed} >= {low} AND {completed} < {high}) \
             ORDER BY {created}, workflow_id"
        );
        let (start, end) = (format_rfc3339_nano(start), format_rfc3339_nano(end));
        collect_reviews(connection, &sql, &[&REVIEW_COMPLETED, &start, &end])
    }

    /// Reads the reviews of `stats_reviews`, the discovery state and, with `range`, the step
    /// runs that ended inside it, the rollup of its days, the plans and the verifier's steps of
    /// the reviews completed inside it and the pruned review requests, as one snapshot.
    ///
    /// A deferred transaction takes no write lock; in WAL mode its first read fixes the
    /// snapshot, so a write of another process between the reads is not seen, and a pruned
    /// review is either a row or in the rollup. Without `range` the step runs, the rollup, the
    /// plans, the verifier's steps and the pruned requests are empty and not read.
    ///
    /// # Errors
    ///
    /// The errors of `stats_reviews`, `load_discovery`, `stats_step_runs`, `read_rollup`,
    /// `stored_steps` and `pruned_requests`.
    pub fn stats_snapshot(
        &self,
        range: Option<(OffsetDateTime, OffsetDateTime)>,
    ) -> Result<StatsSnapshot, Error> {
        let mut connection = self.lock_connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(unavailable)?;
        let reviews = Self::stats_reviews(&transaction, range)?;
        let discovery = Self::read_discovery(&transaction)?;
        let (steps, rollup, (plans, verifier), pruned_requests) = match range {
            Some((start, end)) => (
                Self::stats_step_runs(&transaction, (start, end))?,
                read_rollup(
                    &transaction,
                    &start.date().to_string(),
                    &end.date().to_string(),
                )?,
                completed_steps(&transaction, (start, end))?,
                pruned_requests(&transaction)?,
            ),
            None => Default::default(),
        };
        transaction.commit().map_err(unavailable)?;
        Ok(StatsSnapshot {
            reviews,
            discovery,
            steps,
            rollup,
            plans,
            verifier,
            pruned_requests,
        })
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
pub(super) fn time_key(operand: &str) -> String {
    format!(
        "(CASE WHEN instr({operand}, '.') > 0 \
         THEN substr({operand}, 1, instr({operand}, '.') - 1) \
         || substr(substr({operand}, instr({operand}, '.') + 1, \
         length({operand}) - instr({operand}, '.') - 1) || '000000000', 1, 9) \
         ELSE substr({operand}, 1, length({operand}) - 1) || '000000000' END)"
    )
}

/// Maps a SQLite failure to `store_unavailable`.
pub(super) fn unavailable(err: rusqlite::Error) -> Error {
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
        author_class: input.author_class.clone(),
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

/// Returns the stored plan and the verifier's steps of each review completed inside `range`.
///
/// These are the completed rows of `stats_reviews` whose completion `compute` counts.
///
/// # Errors
///
/// Returns `store_unavailable` when SQLite fails.
fn completed_steps(
    connection: &Connection,
    (start, end): (OffsetDateTime, OffsetDateTime),
) -> Result<(BTreeMap<String, String>, VerifierSteps), Error> {
    let completed = time_key("completed_at");
    let (low, high) = (time_key("?2"), time_key("?3"));
    let filter = format!(
        "status = ?1 AND completed_at IS NOT NULL \
         AND {completed} >= {low} AND {completed} < {high}"
    );
    let (start, end) = (format_rfc3339_nano(start), format_rfc3339_nano(end));
    let bound: [&dyn ToSql; 3] = [&REVIEW_COMPLETED, &start, &end];
    let plans = stored_steps(connection, &filter, &bound, STEP_PLAN)?;
    Ok((plans, verifier_steps(connection, &filter, &bound)?))
}

/// Returns the verification and the combined artifact of each review matched by `filter`.
///
/// The combined artifact, the larger row, is read only for a review with a verification.
///
/// # Errors
///
/// Returns `store_unavailable` when SQLite fails.
fn verifier_steps(
    connection: &Connection,
    filter: &str,
    bound: &[&dyn ToSql],
) -> Result<VerifierSteps, Error> {
    let verified = format!(
        "{filter} AND workflow_id IN \
         (SELECT workflow_id FROM review_steps WHERE name = '{STEP_VERIFICATION}')"
    );
    Ok(VerifierSteps {
        verifications: stored_steps(connection, filter, bound, STEP_VERIFICATION)?,
        combined: stored_steps(connection, &verified, bound, STEP_COMBINED)?,
    })
}

/// Returns the stored step `name` of each review matched by `filter`, by workflow ID,
/// undecoded.
///
/// `filter` uses the parameters `bound`; the step name is the parameter after them.
///
/// # Errors
///
/// Returns `store_unavailable` when SQLite fails.
fn stored_steps(
    connection: &Connection,
    filter: &str,
    bound: &[&dyn ToSql],
    name: &str,
) -> Result<BTreeMap<String, String>, Error> {
    let sql = format!(
        "SELECT workflow_id, output FROM review_steps WHERE name = ?{} AND workflow_id IN \
         (SELECT workflow_id FROM reviews WHERE {filter})",
        bound.len() + 1
    );
    let mut values = bound.to_vec();
    values.push(&name);
    let mut statement = connection.prepare(&sql).map_err(unavailable)?;
    let rows = statement
        .query_map(values.as_slice(), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(unavailable)?;
    rows.collect::<Result<BTreeMap<_, _>, _>>()
        .map_err(unavailable)
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
        let connection = self.lock_connection()?;
        self.write_completion(&connection, workflow_id, result, failure, encoded)
    }

    /// Records the final result of a review as `complete_review` does, and the run `run` of
    /// the step that ended it, in one transaction.
    ///
    /// Returns whether the run was inserted: `false` when the step already has its record.
    ///
    /// # Errors
    ///
    /// Returns `store_state_invalid` when the outcome cannot be encoded and `store_unavailable`
    /// when SQLite fails; then neither is written.
    pub(crate) fn complete_review_recorded(
        &self,
        workflow_id: &str,
        result: &str,
        failure: &str,
        outcome: Option<&ReviewOutcome>,
        run: &StepRun,
    ) -> Result<bool, Error> {
        let encoded = outcome
            .map(encode_go_json)
            .transpose()
            .map_err(|err| store_error("store_state_invalid", true, err))?;
        self.in_tx(|transaction| {
            self.write_completion(transaction, workflow_id, result, failure, encoded)?;
            self.insert_step_run(transaction, workflow_id, run)
        })
    }

    /// Writes the final result of a review on `connection`. An empty `failure` is no failure.
    fn write_completion(
        &self,
        connection: &Connection,
        workflow_id: &str,
        result: &str,
        failure: &str,
        encoded: Option<String>,
    ) -> Result<(), Error> {
        let failure_value = if failure.is_empty() {
            None
        } else {
            Some(failure.to_owned())
        };
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

    /// Deletes the completed reviews that were last updated before `cutoff`, and adds their
    /// counts to the rollup in the same transaction.
    ///
    /// Returns the number of deleted reviews.
    ///
    /// # Errors
    ///
    /// Returns `store_state_invalid` when a deleted row, its step runs, its plan, its
    /// verifier's steps or another review of its pull request does not decode, and
    /// `store_unavailable` when SQLite fails;
    /// then nothing is deleted.
    pub fn prune_reviews(&self, cutoff: OffsetDateTime) -> Result<i64, Error> {
        let filter = format!(
            "status = ?1 AND {} < {}",
            time_key("updated_at"),
            time_key("?2")
        );
        let cutoff = format_rfc3339_nano(cutoff);
        let bound: [&dyn ToSql; 2] = [&REVIEW_COMPLETED, &cutoff];
        self.in_tx(|transaction| {
            let reviews = collect_reviews(
                transaction,
                &format!("SELECT {REVIEW_COLUMNS} FROM reviews WHERE {filter}"),
                &bound,
            )?;
            if reviews.is_empty() {
                return Ok(0);
            }
            let runs = collect_step_runs(
                transaction,
                &format!(
                    "SELECT workflow_id, step, outcome, retries, duration_ms, finished_at \
                     FROM review_step_runs WHERE workflow_id IN \
                     (SELECT workflow_id FROM reviews WHERE {filter})"
                ),
                &bound,
            )?;
            let plans = stored_steps(transaction, &filter, &bound, STEP_PLAN)?;
            let verifier = verifier_steps(transaction, &filter, &bound)?;
            // The first publication of a request is decided over every review of its pull
            // request: a later attempt can be pruned before an earlier one.
            let related = collect_reviews(
                transaction,
                &format!(
                    "SELECT {REVIEW_COLUMNS} FROM reviews WHERE review_key IN \
                     (SELECT review_key FROM reviews WHERE {filter})"
                ),
                &bound,
            )?;
            let firsts = first_publications(&related, &pruned_requests(transaction)?)?;
            let pruned = rollup(&reviews, &runs, &plans, &verifier, &firsts)?;
            add_rollup(transaction, &pruned)?;
            let rows = transaction
                .execute(&format!("DELETE FROM reviews WHERE {filter}"), bound)
                .map_err(unavailable)?;
            Ok(i64::try_from(rows).unwrap_or(i64::MAX))
        })
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
        write_step(&connection, workflow_id, name, &encoded)
    }

    /// Writes the output of a review step as `save_step` does, and the run `run` of the step,
    /// in one transaction.
    ///
    /// Returns whether the run was inserted: `false` when the step already has its record.
    ///
    /// # Errors
    ///
    /// Returns `store_state_invalid` when the value cannot be encoded and `store_unavailable`
    /// when SQLite fails; then neither is written.
    pub(crate) fn save_step_recorded<T: Serialize + ?Sized>(
        &self,
        workflow_id: &str,
        name: &str,
        value: &T,
        run: &StepRun,
    ) -> Result<bool, Error> {
        let encoded =
            encode_go_json(value).map_err(|err| store_error("store_state_invalid", true, err))?;
        self.in_tx(|transaction| {
            write_step(transaction, workflow_id, name, &encoded)?;
            self.insert_step_run(transaction, workflow_id, run)
        })
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
        write_state(&connection, workflow_id, name, &encoded)
    }

    /// Writes a review state value as `set_state` does, and the run `run` of a step, in one
    /// transaction.
    ///
    /// Returns whether the run was inserted: `false` when the step already has its record.
    ///
    /// # Errors
    ///
    /// Returns `store_state_invalid` when the value cannot be encoded and `store_unavailable`
    /// when SQLite fails; then neither is written.
    pub(crate) fn set_state_recorded<T: Serialize + ?Sized>(
        &self,
        workflow_id: &str,
        name: &str,
        value: &T,
        run: &StepRun,
    ) -> Result<bool, Error> {
        let encoded =
            encode_go_json(value).map_err(|err| store_error("store_state_invalid", true, err))?;
        self.in_tx(|transaction| {
            write_state(transaction, workflow_id, name, &encoded)?;
            self.insert_step_run(transaction, workflow_id, run)
        })
    }
}

/// Writes the encoded output of a review step on `connection`, replacing a previous one.
fn write_step(
    connection: &Connection,
    workflow_id: &str,
    name: &str,
    encoded: &str,
) -> Result<(), Error> {
    connection
        .execute(
            "INSERT INTO review_steps (workflow_id, name, output) VALUES (?1, ?2, ?3) \
             ON CONFLICT (workflow_id, name) DO UPDATE SET output = excluded.output",
            params![workflow_id, name, encoded],
        )
        .map_err(unavailable)?;
    Ok(())
}

/// Writes an encoded review state value on `connection`, replacing a previous one.
fn write_state(
    connection: &Connection,
    workflow_id: &str,
    name: &str,
    encoded: &str,
) -> Result<(), Error> {
    connection
        .execute(
            "INSERT INTO review_state (workflow_id, name, value) VALUES (?1, ?2, ?3) \
             ON CONFLICT (workflow_id, name) DO UPDATE SET value = excluded.value",
            params![workflow_id, name, encoded],
        )
        .map_err(unavailable)?;
    Ok(())
}
