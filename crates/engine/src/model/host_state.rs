//! The `HostCapacity` v1 document that the `moc.host-state` plugin reports for one host.
//!
//! Every field of the schema is required and no other field is accepted, at every level.

// Rust guideline compliant 2026-06-26

use serde::{Deserialize, Deserializer};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::error::Error;

/// Error code of a host state document that does not read as `HostCapacity` v1.
pub(crate) const HOST_STATE_INVALID: &str = "host_state_invalid";

/// The only `schemaVersion` this reader accepts.
const SCHEMA_VERSION: &str = "v1";

/// Largest `batteryPercent` of the schema.
const BATTERY_PERCENT_MAX: u8 = 100;

/// The state of one review host: idle, power, memory, local models and policy window.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostCapacity {
    /// Version of the document; always `v1`.
    pub schema_version: String,
    /// The plugin's own name of the host.
    pub host_alias: String,
    /// When the plugin read the state, in UTC.
    #[serde(deserialize_with = "utc_timestamp")]
    pub timestamp: OffsetDateTime,
    /// Whether the plugin judges the host idle.
    pub idle: bool,
    /// Why the host is idle or not, in words.
    pub idle_reason: String,
    /// Seconds since the last user input.
    pub seconds_since_input: u64,
    /// Whether the host runs on mains power.
    pub ac_power: bool,
    /// Battery charge from 0 to 100, or `None` without a battery.
    #[serde(deserialize_with = "nullable")]
    pub battery_percent: Option<u8>,
    /// Processor cores and load.
    pub cpu: HostCpu,
    /// Total and free memory.
    pub memory: HostMemory,
    /// The graphics card, when there is one.
    pub gpu: HostGpu,
    /// The local model server and its models.
    pub local_models: HostModels,
    /// The host's own policy window.
    pub policy: HostPolicy,
}

/// Processor cores and load of a host.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostCpu {
    /// Number of cores.
    pub cores: u64,
    /// Load average over one minute.
    pub load1m: f64,
}

/// Total and free memory of a host, in MB.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostMemory {
    /// Total memory, in MB.
    pub total_mb: u64,
    /// Free memory, in MB.
    pub free_mb: u64,
}

/// The graphics card of a host.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostGpu {
    /// Whether the host has a graphics card.
    pub present: bool,
    /// Name of the card, or `None` without one.
    #[serde(deserialize_with = "nullable")]
    pub name: Option<String>,
    /// Free graphics memory in MB, or `None` when unknown.
    #[serde(deserialize_with = "nullable")]
    pub free_mb: Option<u64>,
}

/// The local model server of a host.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostModels {
    /// Whether the model server answered.
    pub reachable: bool,
    /// Names of the models it serves.
    pub models: Vec<String>,
}

/// The policy window that the host's own configuration sets.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostPolicy {
    /// Start and end of the window, in host local time.
    pub allowed_window: HostWindow,
    /// Whether the host is inside its window now.
    pub inside_window: bool,
    /// Most agents the host runs at once.
    pub max_concurrent_agents: u64,
}

/// Start and end of a policy window, as `HH:MM`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostWindow {
    /// Start of the window.
    pub start: String,
    /// End of the window.
    pub end: String,
}

impl HostCapacity {
    /// Reads one `HostCapacity` v1 document.
    ///
    /// # Errors
    ///
    /// Returns `host_state_invalid` for a document that is not v1 JSON of this shape, with a
    /// timestamp that is not RFC 3339 ending in `Z`, or with a battery above 100.
    pub fn parse(text: &str) -> Result<Self, Error> {
        let capacity: Self = serde_json::from_str(text)
            .map_err(|err| Error::workflow(HOST_STATE_INVALID, false, Some(Box::new(err))))?;
        if capacity.schema_version != SCHEMA_VERSION
            || capacity
                .battery_percent
                .is_some_and(|percent| percent > BATTERY_PERCENT_MAX)
        {
            return Err(Error::workflow(HOST_STATE_INVALID, false, None));
        }
        Ok(capacity)
    }
}

/// Reads an RFC 3339 timestamp that ends in `Z`; fractional seconds are accepted.
fn utc_timestamp<'de, D: Deserializer<'de>>(deserializer: D) -> Result<OffsetDateTime, D::Error> {
    let text = String::deserialize(deserializer)?;
    if !text.ends_with('Z') {
        return Err(serde::de::Error::custom("the timestamp does not end in Z"));
    }
    OffsetDateTime::parse(&text, &Rfc3339).map_err(serde::de::Error::custom)
}

/// Reads a value that may be null but must be present.
fn nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}
