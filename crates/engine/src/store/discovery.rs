//! The discovery state: the four entries of the `discovery` table, read and written as one.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeMap;

use autoreview_schedule::scheduler::Record;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Serialize;
use serde::de::DeserializeOwned;
use time::UtcOffset;

use super::reviews::unavailable;
use super::rollup::{add_rollup, stops};
use super::{Store, encode_go_json, store_error};
use crate::Error;
use crate::model::{AwaitRecord, PublishedRecord};

/// Discovery entry that holds the schedule records.
pub const STATE_SCHEDULE: &str = "schedule";
/// Discovery entry that holds the last published review of each pull request.
pub const STATE_PUBLISHED: &str = "published";
/// Discovery entry that holds the clean reviews that wait for approval.
pub const STATE_AWAITING: &str = "awaiting";
/// Discovery entry that holds the code last reported for each rejected pull request.
pub const STATE_REJECTED: &str = "rejected";

/// Key prefix of an announced host outage in the `rejected` entry.
///
/// The outages share the `rejected` entry: an unknown entry name is refused with
/// `store_state_invalid`, so a new entry would not open on an earlier release.
/// No `owner/name#N` key starts with this prefix, because an owner has no `:`.
const HOST_OUTAGE_PREFIX: &str = "host:";

/// Key prefix of a pull request that the last pass skipped, in the `rejected` entry.
const SKIPPED_PREFIX: &str = "skipped:";

/// Key of the last weekly report send in the `rejected` entry, which has no `owner/name#N`
/// key with a `:` before its `/`.
const WEEKLY_REPORT_SENT_KEY: &str = "report:weekly_report_sent";

/// The discovery maps; all but `host_outages` are keyed by the `repository#number` key of a
/// pull request.
///
/// A missing or `null` entry in the database reads as an empty map.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscoveryState {
    /// Schedule record of each review identity.
    pub schedule: BTreeMap<String, Record>,
    /// Last published review of each pull request.
    pub published: BTreeMap<String, PublishedRecord>,
    /// Clean reviews that wait for an approval decision.
    pub awaiting: BTreeMap<String, AwaitRecord>,
    /// Code last reported for each rejected pull request, so each rejection is announced once.
    pub rejected: BTreeMap<String, String>,
    /// Error code of each review host whose outage was announced, keyed by host, so each
    /// outage is announced once. Stored in the `rejected` entry under [`HOST_OUTAGE_PREFIX`].
    pub host_outages: BTreeMap<String, String>,
    /// Reason of each pull request that the last pass skipped, such as `untrusted_author`.
    /// Stored in the `rejected` entry under [`SKIPPED_PREFIX`].
    pub skipped: BTreeMap<String, String>,
    /// When the review queue last sent the weekly report, RFC 3339 as stored.
    /// Stored in the `rejected` entry under [`WEEKLY_REPORT_SENT_KEY`].
    pub weekly_report_sent: Option<String>,
}

impl Store {
    /// Reads the four discovery entries.
    ///
    /// # Errors
    ///
    /// Returns `store_state_invalid` for an unknown entry name or an entry whose JSON does not
    /// read, and `store_unavailable` when SQLite fails.
    pub fn load_discovery(&self) -> Result<DiscoveryState, Error> {
        let connection = self.lock_connection()?;
        Self::read_discovery(&connection)
    }

    /// Reads the four discovery entries through `connection`, as `load_discovery` does.
    ///
    /// # Errors
    ///
    /// The errors of `load_discovery`.
    pub(super) fn read_discovery(connection: &Connection) -> Result<DiscoveryState, Error> {
        let mut statement = connection
            .prepare("SELECT name, value FROM discovery")
            .map_err(|err| store_error("store_unavailable", false, err))?;
        let rows = statement
            .query_map([], |row| {
                let name: String = row.get(0)?;
                let value: String = row.get(1)?;
                Ok((name, value))
            })
            .map_err(|err| store_error("store_unavailable", false, err))?;
        let mut state = DiscoveryState::default();
        for row in rows {
            let (name, value) = row.map_err(|err| store_error("store_unavailable", false, err))?;
            match name.as_str() {
                STATE_SCHEDULE => {
                    state.schedule = read_entry(&name, &value)?;
                }
                STATE_PUBLISHED => {
                    state.published = read_entry(&name, &value)?;
                }
                STATE_AWAITING => {
                    state.awaiting = read_entry(&name, &value)?;
                }
                STATE_REJECTED => {
                    let entries: BTreeMap<String, String> = read_entry(&name, &value)?;
                    for (key, code) in entries {
                        if key == WEEKLY_REPORT_SENT_KEY {
                            state.weekly_report_sent = Some(code);
                        } else if let Some(host) = key.strip_prefix(HOST_OUTAGE_PREFIX) {
                            state.host_outages.insert(host.to_owned(), code);
                        } else if let Some(skipped) = key.strip_prefix(SKIPPED_PREFIX) {
                            state.skipped.insert(skipped.to_owned(), code);
                        } else {
                            state.rejected.insert(key, code);
                        }
                    }
                }
                _ => {
                    return Err(store_error(
                        "store_state_invalid",
                        true,
                        format!("unknown discovery entry {name:?}"),
                    ));
                }
            }
        }
        Ok(state)
    }

    /// Runs `change` on the discovery state and writes the state back in one transaction.
    ///
    /// `change` may write other rows through the same transaction. The state is read before the
    /// transaction begins. The caller holds the discovery lock.
    ///
    /// # Errors
    ///
    /// Returns the error of the read, of `change` or of the write. A failing `change` rolls the
    /// transaction back.
    pub fn update_discovery(
        &self,
        change: impl FnOnce(&mut DiscoveryState, &Transaction<'_>) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let mut state = self.load_discovery()?;
        self.in_tx(|transaction| {
            change(&mut state, transaction)?;
            self.save_discovery(transaction, &state)
        })
    }

    /// Writes the four entries in schedule, published, awaiting, rejected order.
    ///
    /// The `rejected` entry holds the host outages, the skipped pull requests and the last
    /// weekly report send as well. A
    /// schedule record that enters `failed` adds one stop to the rollup of the current UTC day,
    /// in the same transaction, so a restart never counts it again.
    ///
    /// # Errors
    ///
    /// Returns `store_state_invalid` when the stored schedule does not read or an entry does
    /// not encode, and `store_unavailable` when SQLite fails.
    pub(crate) fn save_discovery(
        &self,
        transaction: &Transaction<'_>,
        state: &DiscoveryState,
    ) -> Result<(), Error> {
        let stored: Option<String> = transaction
            .query_row(
                "SELECT value FROM discovery WHERE name = ?1",
                [STATE_SCHEDULE],
                |row| row.get(0),
            )
            .optional()
            .map_err(unavailable)?;
        let stored: BTreeMap<String, Record> = match stored {
            Some(value) => read_entry(STATE_SCHEDULE, &value)?,
            None => BTreeMap::new(),
        };
        let day = (self.now)().to_offset(UtcOffset::UTC).date().to_string();
        add_rollup(transaction, &stops(&day, &stored, &state.schedule))?;
        write_entry(transaction, STATE_SCHEDULE, &state.schedule)?;
        write_entry(transaction, STATE_PUBLISHED, &state.published)?;
        write_entry(transaction, STATE_AWAITING, &state.awaiting)?;
        let mut rejected = state.rejected.clone();
        for (host, code) in &state.host_outages {
            rejected.insert(format!("{HOST_OUTAGE_PREFIX}{host}"), code.clone());
        }
        for (key, reason) in &state.skipped {
            rejected.insert(format!("{SKIPPED_PREFIX}{key}"), reason.clone());
        }
        if let Some(sent) = &state.weekly_report_sent {
            rejected.insert(WEEKLY_REPORT_SENT_KEY.to_owned(), sent.clone());
        }
        write_entry(transaction, STATE_REJECTED, &rejected)?;
        Ok(())
    }
}

/// Reads one discovery entry; JSON `null` gives an empty map.
fn read_entry<T: DeserializeOwned + Default>(name: &str, value: &str) -> Result<T, Error> {
    serde_json::from_str::<Option<T>>(value)
        .map(Option::unwrap_or_default)
        .map_err(|err| {
            store_error(
                "store_state_invalid",
                true,
                format!("discovery entry {name}: {err}"),
            )
        })
}

/// Encodes one entry and upserts it into the `discovery` table.
fn write_entry<T: Serialize>(
    transaction: &Transaction<'_>,
    name: &str,
    value: &T,
) -> Result<(), Error> {
    let encoded =
        encode_go_json(value).map_err(|err| store_error("store_state_invalid", true, err))?;
    transaction
        .execute(
            "INSERT INTO discovery (name, value) VALUES (?1, ?2) \
             ON CONFLICT (name) DO UPDATE SET value = excluded.value",
            params![name, encoded],
        )
        .map_err(|err| store_error("store_unavailable", false, err))?;
    Ok(())
}
