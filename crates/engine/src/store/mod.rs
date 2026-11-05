//! The SQLite store: open, schema, the discovery lock and backups.
//!
//! The database file is the one the Go reference writes. Its schema text, pragmas, lock
//! file and error codes are the contract, so a Go-created file opens here unchanged.

// Rust guideline compliant 2026-06-26

use std::fs::{self, DirBuilder, OpenOptions};
use std::future::Future;
use std::io;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use autoreview_go_compat::go_time::format_rfc3339_nano;
use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};
use rustix::fs::{FlockOperation, flock};
use time::OffsetDateTime;

use crate::{BoxError, Error};
use serde::Serialize;

pub mod discovery;
pub mod reviews;

pub use discovery::{
    DiscoveryState, STATE_AWAITING, STATE_PUBLISHED, STATE_REJECTED, STATE_SCHEDULE,
};
pub use reviews::{
    RESULT_FAILURE, RESULT_SUCCESS, REVIEW_AWAITING, REVIEW_COMPLETED, REVIEW_QUEUED,
    REVIEW_RUNNING, ReviewFilter, StoredReview,
};

/// Schema version stored in `PRAGMA user_version`; a newer database is refused.
const STORE_SCHEMA_VERSION: i64 = 1;

/// Busy timeout of every connection, the `busy_timeout` of the Go DSN.
const BUSY_TIMEOUT: Duration = Duration::from_millis(10_000);

/// Mode of the database directory when `open` creates it.
const DIRECTORY_MODE: u32 = 0o700;

/// Mode of the discovery lock file.
const LOCK_FILE_MODE: u32 = 0o600;

/// Suffix appended to the database path to name the discovery lock file.
const DISCOVERY_LOCK_SUFFIX: &str = ".discovery.lock";

/// Message of the cause of a `discovery_busy` failure.
const DISCOVERY_LOCK_TIMEOUT_CAUSE: &str = "discovery lock not acquired in time";

/// Cause of a `store_unavailable` failure when the connection lock is poisoned.
const POISONED_CAUSE: &str = "store connection lock is poisoned";

/// The DDL of the store, kept byte for byte: SQLite stores this text in `sqlite_master`.
const STORE_SCHEMA: &str = concat!(
    "\n",
    "CREATE TABLE IF NOT EXISTS discovery (\n",
    "\tname  TEXT PRIMARY KEY,\n",
    "\tvalue TEXT NOT NULL\n",
    ");\n",
    "CREATE TABLE IF NOT EXISTS reviews (\n",
    "\tworkflow_id       TEXT PRIMARY KEY,\n",
    "\treview_key        TEXT NOT NULL,\n",
    "\tinput             TEXT NOT NULL,\n",
    "\tstatus            TEXT NOT NULL CHECK (status IN ('queued', 'running', ",
    "'awaiting', 'completed')),\n",
    "\tresult            TEXT CHECK (result IS NULL OR result IN ('success', 'failure')),\n",
    "\tfailure           TEXT,\n",
    "\treleased          INTEGER NOT NULL DEFAULT 0,\n",
    "\tcleanup_pending   INTEGER NOT NULL DEFAULT 0,\n",
    "\tapproval          TEXT CHECK (approval IS NULL OR ",
    "approval IN ('approve', 'expired', 'superseded')),\n",
    "\tapproval_deadline TEXT,\n",
    "\toutcome           TEXT,\n",
    "\tcreated_at        TEXT NOT NULL,\n",
    "\tupdated_at        TEXT NOT NULL,\n",
    "\tcompleted_at      TEXT\n",
    ");\n",
    "CREATE INDEX IF NOT EXISTS reviews_status ON reviews (status);\n",
    "CREATE INDEX IF NOT EXISTS reviews_created_at ON reviews (created_at);\n",
    "CREATE TABLE IF NOT EXISTS review_steps (\n",
    "\tworkflow_id TEXT NOT NULL REFERENCES reviews (workflow_id) ON DELETE CASCADE,\n",
    "\tname        TEXT NOT NULL,\n",
    "\toutput      TEXT NOT NULL,\n",
    "\tPRIMARY KEY (workflow_id, name)\n",
    ");\n",
    "CREATE TABLE IF NOT EXISTS review_state (\n",
    "\tworkflow_id TEXT NOT NULL REFERENCES reviews (workflow_id) ON DELETE CASCADE,\n",
    "\tname        TEXT NOT NULL,\n",
    "\tvalue       TEXT NOT NULL,\n",
    "\tPRIMARY KEY (workflow_id, name)\n",
    ");\n",
);

/// Store of the discovery state and every review in one SQLite database.
///
/// Writers serialize on the one connection. The discovery state also needs the discovery
/// lock, because a poll reads it before slow GitHub calls and writes it afterwards.
pub struct Store {
    /// The one connection; its lock keeps transactions sequential, as Go's one pool connection.
    connection: Mutex<Connection>,
    /// The discovery lock file, `<database path>.discovery.lock`.
    lock_path: PathBuf,
    /// Serializes discovery sections inside this process; the file lock serializes processes.
    discovery_mutex: tokio::sync::Mutex<()>,
    /// The clock behind the timestamps.
    now: Box<dyn Fn() -> OffsetDateTime + Send + Sync>,
}

impl Store {
    /// Opens the database at the absolute `path` for reading and writing and creates its schema.
    ///
    /// # Errors
    ///
    /// Returns `store_path_invalid` for a relative path, `store_unavailable`
    /// when the directory, the connection or the schema cannot be used, and
    /// `store_version_unsupported` when the database is written by a newer schema.
    pub fn open(
        path: &Path,
        now: Box<dyn Fn() -> OffsetDateTime + Send + Sync>,
    ) -> Result<Self, Error> {
        if !path.is_absolute() {
            return Err(store_error(
                "store_path_invalid",
                true,
                "store path must be absolute",
            ));
        }
        let Some(parent) = path.parent() else {
            return Err(store_error(
                "store_path_invalid",
                true,
                "store path has no directory",
            ));
        };
        let mut builder = DirBuilder::new();
        builder.recursive(true).mode(DIRECTORY_MODE);
        if let Err(err) = builder.create(parent) {
            return Err(store_error("store_unavailable", true, err));
        }
        let connection = Self::open_connection(path, false)?;
        let store = Self {
            connection: Mutex::new(connection),
            lock_path: discovery_lock_path(path),
            discovery_mutex: tokio::sync::Mutex::new(()),
            now,
        };
        store.migrate()?;
        Ok(store)
    }

    /// Opens one connection with the pragmas of the Go DSN, in the Go order.
    ///
    /// Read-only connections skip `journal_mode`, as the Go read-only DSN does. The flags
    /// leave out `SQLITE_OPEN_URI`, so the path is a plain file path.
    fn open_connection(path: &Path, read_only: bool) -> Result<Connection, Error> {
        let flags = if read_only {
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX
        } else {
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
        };
        let connection = Connection::open_with_flags(path, flags)
            .map_err(|err| store_error("store_unavailable", true, err))?;
        connection
            .busy_timeout(BUSY_TIMEOUT)
            .map_err(|err| store_error("store_unavailable", true, err))?;
        connection
            .execute_batch("PRAGMA foreign_keys = 1;")
            .map_err(|err| store_error("store_unavailable", true, err))?;
        if !read_only {
            connection
                .execute_batch("PRAGMA journal_mode = WAL;")
                .map_err(|err| store_error("store_unavailable", true, err))?;
        }
        Ok(connection)
    }

    /// Closes the connection and reports the failure of the close.
    ///
    /// # Errors
    ///
    /// Returns `store_unavailable` when SQLite reports an error while closing.
    pub fn close(self) -> Result<(), Error> {
        let connection = match self.connection.into_inner() {
            Ok(conn) => conn,
            Err(_) => {
                return Err(store_error("store_unavailable", false, POISONED_CAUSE));
            }
        };
        if let Err((_, err)) = connection.close() {
            return Err(store_error("store_unavailable", false, err));
        }
        Ok(())
    }

    /// Creates the schema and stamps the version, after refusing a newer database.
    fn migrate(&self) -> Result<(), Error> {
        let connection = self.lock_connection()?;
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(|err| store_error("store_unavailable", true, err))?;
        if version > STORE_SCHEMA_VERSION {
            return Err(store_error(
                "store_version_unsupported",
                true,
                format!("schema {version} is newer than {STORE_SCHEMA_VERSION}"),
            ));
        }
        connection
            .execute_batch(STORE_SCHEMA)
            .map_err(|err| store_error("store_unavailable", true, err))?;
        connection
            .pragma_update(None, "user_version", STORE_SCHEMA_VERSION)
            .map_err(|err| store_error("store_unavailable", true, err))?;
        Ok(())
    }

    /// Returns the current time as RFC 3339 text with nanoseconds, in UTC, as Go writes it.
    pub fn timestamp(&self) -> String {
        format_rfc3339_nano((self.now)())
    }

    /// Runs `run` while this process and every other process holds the discovery lock.
    ///
    /// # Errors
    ///
    /// Returns `discovery_busy` when the lock stays held for `timeout`, `store_unavailable`
    /// when the lock file cannot be opened or locked, and any error of `run`.
    pub async fn with_discovery_lock<T, F, Fut>(
        &self,
        timeout: Duration,
        poll: Duration,
        run: F,
    ) -> Result<T, Error>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, Error>>,
    {
        let _in_process = self.discovery_mutex.lock().await;
        let lock_file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .mode(LOCK_FILE_MODE)
            .open(&self.lock_path)
            .map_err(|err| store_error("store_unavailable", true, err))?;
        let deadline = Instant::now() + timeout;
        loop {
            match flock(&lock_file, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => break,
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(store_error(
                            "discovery_busy",
                            false,
                            io::Error::other(DISCOVERY_LOCK_TIMEOUT_CAUSE),
                        ));
                    }
                    tokio::time::sleep(poll).await;
                }
                Err(err) => {
                    return Err(store_error("store_unavailable", true, err));
                }
            }
        }
        // Dropping `lock_file` closes the descriptor, which releases the flock.
        let result = run().await;
        drop(lock_file);
        drop(_in_process);
        result
    }

    /// Runs `change` in one `BEGIN IMMEDIATE` transaction and commits it when `change` succeeds.
    ///
    /// # Errors
    ///
    /// Returns `store_unavailable` when the transaction cannot begin or commit, and any
    /// error of `change`.
    ///
    /// A failing `change` rolls the transaction back and its error is returned unchanged.
    pub fn in_tx<T>(
        &self,
        change: impl FnOnce(&Transaction<'_>) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let mut connection = self.lock_connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|err| store_error("store_unavailable", false, err))?;
        let value = change(&transaction)?;
        transaction
            .commit()
            .map_err(|err| store_error("store_unavailable", false, err))?;
        Ok(value)
    }

    /// Writes a consistent copy of the database to the absolute `path`.
    ///
    /// # Errors
    ///
    /// Returns `backup_path_invalid` for a relative or non-UTF-8 path, `backup_path_exists`
    /// when anything already sits at `path`, and `backup_failed` when SQLite cannot write it.
    pub fn backup_to(&self, path: &Path) -> Result<(), Error> {
        if !path.is_absolute() {
            return Err(Error::workflow("backup_path_invalid", true, None));
        }
        if fs::metadata(path).is_ok() {
            return Err(Error::workflow("backup_path_exists", true, None));
        }
        let Some(target) = path.to_str() else {
            return Err(store_error(
                "backup_path_invalid",
                true,
                "backup path must be valid UTF-8",
            ));
        };
        let connection = self.lock_connection()?;
        connection
            .execute("VACUUM INTO ?1", [target])
            .map_err(|err| store_error("backup_failed", true, err))?;
        Ok(())
    }

    /// Returns the connection guard, or `store_unavailable` when an earlier holder panicked.
    fn lock_connection(&self) -> Result<MutexGuard<'_, Connection>, Error> {
        match self.connection.lock() {
            Ok(guard) => Ok(guard),
            Err(_) => Err(store_error("store_unavailable", false, POISONED_CAUSE)),
        }
    }
}

/// Returns the discovery lock path: the database path with `.discovery.lock` appended.
fn discovery_lock_path(path: &Path) -> PathBuf {
    let mut os_string = path.as_os_str().to_owned();
    os_string.push(DISCOVERY_LOCK_SUFFIX);
    PathBuf::from(os_string)
}

/// Builds a workflow error from a stable code and the error that caused it.
fn store_error(code: &str, permanent: bool, cause: impl Into<BoxError>) -> Error {
    Error::workflow(code, permanent, Some(cause.into()))
}

/// Encodes `value` as Go's `encoding/json` does, which escapes `<`, `>`, `&` and the two line
/// separators, so the stored text is byte-identical to the Go reference.
fn encode_go_json<T: Serialize + ?Sized>(value: &T) -> Result<String, serde_json::Error> {
    let text = serde_json::to_string(value)?;
    Ok(text
        .replace('<', "<u003c")
        .replace('>', "<u003e")
        .replace('&', "<u0026")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029"))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use autoreview_schedule::scheduler::Run;
    use rusqlite::types::Value as SqlValue;
    use serde::Serialize;
    use serde_json::{Value, json};
    use time::OffsetDateTime;

    use super::{ReviewFilter, Store};

    /// Store golden written by the Go reference, shared with the Go reference tests.
    const STORE_GOLDEN: &str = include_str!("../../../../testdata/parity/store-v1.json");

    /// Page size of the list reads in `go_store_golden_opens`; the golden holds fewer reviews.
    const GOLDEN_LIST_LIMIT: i64 = 100;

    fn test_now() -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH
    }

    fn open_store(path: &Path) -> Store {
        Store::open(path, Box::new(test_now)).expect("open store")
    }

    #[test]
    fn backup_writes_a_readable_copy_and_refuses_an_existing_path() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = open_store(&dir.path().join("live.db"));
        store
            .in_tx(|tx| {
                tx.execute(
                    "INSERT INTO reviews (workflow_id, review_key, input, status, created_at, \
                     updated_at) VALUES ('prr-v2-backup', 'key', '{}', 'queued', 'now', 'now')",
                    [],
                )
                .expect("insert review row");
                Ok(())
            })
            .expect("commit insert");
        let target = dir.path().join("backup.db");
        store.backup_to(&target).expect("backup");
        let copied = open_store(&target);
        let copied_input: String = copied
            .connection
            .lock()
            .expect("lock copy")
            .query_row(
                "SELECT input FROM reviews WHERE workflow_id = 'prr-v2-backup'",
                [],
                |row| row.get(0),
            )
            .expect("read copied review");
        assert_eq!(copied_input, "{}");
        copied.close().expect("close copy");
        let existing = store.backup_to(&target).expect_err("existing path refused");
        assert_eq!(crate::error_code(&existing), "backup_path_exists");
        let relative = store
            .backup_to(Path::new("relative/backup.db"))
            .expect_err("relative path refused");
        assert_eq!(crate::error_code(&relative), "backup_path_invalid");
    }

    #[test]
    fn newer_schema_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("newer.db");
        let connection = rusqlite::Connection::open(&path).expect("create file");
        connection
            .pragma_update(None, "user_version", 2)
            .expect("set version");
        drop(connection);
        let err = Store::open(&path, Box::new(test_now))
            .err()
            .expect("newer schema refused");
        assert_eq!(crate::error_code(&err), "store_version_unsupported");
    }

    /// Converts one golden cell into the SQLite value it was stored as.
    fn sql_value(cell: &Value) -> SqlValue {
        match cell {
            Value::Null => SqlValue::Null,
            Value::String(text) => SqlValue::Text(text.clone()),
            Value::Number(number) => SqlValue::Integer(number.as_i64().expect("golden integer")),
            other => panic!("golden cell {other} is not a scalar the store holds"),
        }
    }

    /// Writes the golden rows of one table with SQL, one column per golden key.
    fn insert_golden_rows(connection: &rusqlite::Connection, table: &str, rows: &[Value]) {
        for row in rows {
            let object = row.as_object().expect("golden row is an object");
            let columns: Vec<&str> = object.keys().map(String::as_str).collect();
            let values: Vec<SqlValue> = object.values().map(sql_value).collect();
            let placeholders = vec!["?"; columns.len()].join(", ");
            let sql = format!(
                "INSERT INTO {table} ({}) VALUES ({placeholders})",
                columns.join(", ")
            );
            connection
                .execute(&sql, rusqlite::params_from_iter(values))
                .expect("golden row inserts");
        }
    }

    /// Serializes a value for comparison with the golden JSON.
    fn to_value<T: Serialize>(value: &T) -> Value {
        serde_json::to_value(value).expect("the value serializes")
    }

    /// Writes the Go run-history shape of one run, which keeps Go's field names.
    fn run_value(run: &Run) -> Value {
        json!({
            "Target": {
                "repository": run.target.repository,
                "number": run.target.number,
                "head_sha": run.target.head_sha,
                "review_request_id": run.target.request_id,
            },
            "WorkflowID": run.workflow_id,
            "Attempt": run.attempt,
            "Status": run.status,
            "Result": run.result,
            "Failure": run.failure,
            "Released": run.released,
            "CleanupPending": run.cleanup_pending,
        })
    }

    /// Opens a store holding the rows of the Go golden and reads every part the store owns.
    ///
    /// The engine's `prior_findings` section is not checked here; it belongs to the engine.
    #[test]
    fn go_store_golden_opens() {
        let golden: Value = serde_json::from_str(STORE_GOLDEN).expect("golden is JSON");
        let dir = tempfile::tempdir().expect("temp dir");
        let store = open_store(&dir.path().join("golden.db"));
        store
            .in_tx(|transaction| {
                for table in ["discovery", "reviews", "review_steps", "review_state"] {
                    let rows = golden[table].as_array().expect("golden table is an array");
                    insert_golden_rows(transaction, table, rows);
                }
                Ok(())
            })
            .expect("golden rows insert");

        {
            let connection = store.lock_connection().expect("connection locks");
            let version: i64 = connection
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .expect("version reads");
            assert_eq!(Some(version), golden["user_version"].as_i64());
            let mut statement = connection
                .prepare(
                    "SELECT type, name, tbl_name, sql FROM sqlite_master \
                     WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
                )
                .expect("schema query prepares");
            let schema: Vec<Value> = statement
                .query_map([], |row| {
                    Ok(json!({
                        "type": row.get::<_, String>(0)?,
                        "name": row.get::<_, String>(1)?,
                        "tbl_name": row.get::<_, String>(2)?,
                        "sql": row.get::<_, String>(3)?,
                    }))
                })
                .expect("schema rows query")
                .map(|row| row.expect("schema row reads"))
                .collect();
            assert_eq!(Value::Array(schema), golden["schema"]);
        }

        let state = store.load_discovery().expect("discovery reads");
        assert_eq!(
            to_value(&state.schedule),
            golden["api"]["discovery"]["schedule"]
        );
        assert_eq!(
            to_value(&state.published),
            golden["api"]["discovery"]["published"]
        );
        assert_eq!(
            to_value(&state.awaiting),
            golden["api"]["discovery"]["awaiting"]
        );
        assert_eq!(
            to_value(&state.rejected),
            golden["api"]["discovery"]["rejected"]
        );

        let reviews = store
            .list_reviews(&ReviewFilter::All, GOLDEN_LIST_LIMIT)
            .expect("reviews read");
        assert_eq!(to_value(&reviews), golden["api"]["list_reviews"]);
        let runs = store
            .run_history(GOLDEN_LIST_LIMIT)
            .expect("run history reads");
        let run_values: Vec<Value> = runs.iter().map(run_value).collect();
        assert_eq!(Value::Array(run_values), golden["api"]["run_history"]);
        assert!(
            store
                .ready_reviews()
                .expect("ready reviews read")
                .is_empty()
        );

        for step in golden["review_steps"].as_array().expect("golden steps") {
            let workflow_id = step["workflow_id"].as_str().expect("workflow id");
            let name = step["name"].as_str().expect("step name");
            let output: Value = store
                .load_step(workflow_id, name)
                .expect("step reads")
                .expect("golden step exists");
            assert_eq!(
                output,
                serde_json::from_str::<Value>(step["output"].as_str().expect("output"))
                    .expect("golden output is JSON")
            );
        }
        for state_row in golden["review_state"].as_array().expect("golden state") {
            let workflow_id = state_row["workflow_id"].as_str().expect("workflow id");
            let name = state_row["name"].as_str().expect("state name");
            let value: Value = store
                .get_state(workflow_id, name)
                .expect("state reads")
                .expect("golden state exists");
            assert_eq!(
                value,
                serde_json::from_str::<Value>(state_row["value"].as_str().expect("value"))
                    .expect("golden value is JSON")
            );
        }
    }
}
