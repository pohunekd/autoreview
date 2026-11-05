//! The discovery state: the four entries of the `discovery` table, read and written as one.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeMap;

use autoreview_schedule::scheduler::Record;
use rusqlite::{Transaction, params};
use serde::Serialize;
use serde::de::DeserializeOwned;

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

/// The four discovery maps, each keyed by the `repository#number` key of a pull request.
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
                    state.rejected = read_entry(&name, &value)?;
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
            save_discovery(transaction, &state)
        })
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

/// Writes the four entries in schedule, published, awaiting, rejected order.
fn save_discovery(transaction: &Transaction<'_>, state: &DiscoveryState) -> Result<(), Error> {
    write_entry(transaction, STATE_SCHEDULE, &state.schedule)?;
    write_entry(transaction, STATE_PUBLISHED, &state.published)?;
    write_entry(transaction, STATE_AWAITING, &state.awaiting)?;
    write_entry(transaction, STATE_REJECTED, &state.rejected)?;
    Ok(())
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
