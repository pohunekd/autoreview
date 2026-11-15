//! The daily rollup: counts of pruned reviews and of stops, kept past the review retention.
//!
//! The tables have no foreign key to `reviews`, so pruning leaves them alone, and an engine
//! that does not know them opens the file unchanged.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeMap;

use autoreview_schedule::phase;
use autoreview_schedule::scheduler::Record;
use rusqlite::{Connection, params};

use super::reviews::unavailable;
use crate::Error;

/// The DDL of the rollup, run after `STEP_RUNS_SCHEMA`; a store that predates it gains it.
pub(super) const ROLLUP_SCHEMA: &str = concat!(
    "\n",
    "CREATE TABLE IF NOT EXISTS stats_rollup (\n",
    "\tday       TEXT NOT NULL,\n",
    "\tdimension TEXT NOT NULL,\n",
    "\tkey       TEXT NOT NULL,\n",
    "\tcounter   TEXT NOT NULL,\n",
    "\tvalue     INTEGER NOT NULL,\n",
    "\tPRIMARY KEY (day, dimension, key, counter)\n",
    ");\n",
    "CREATE TABLE IF NOT EXISTS stats_rollup_samples (\n",
    "\tday       TEXT NOT NULL,\n",
    "\tdimension TEXT NOT NULL,\n",
    "\tkey       TEXT NOT NULL,\n",
    "\tsample    INTEGER NOT NULL,\n",
    "\tcount     INTEGER NOT NULL,\n",
    "\tPRIMARY KEY (day, dimension, key, sample)\n",
    ");\n",
);

/// Dimension of the counts of a whole UTC day; its key is empty.
pub(crate) const DIMENSION_DAY: &str = "day";
/// Dimension of the counts per repository.
pub(crate) const DIMENSION_REPOSITORY: &str = "repository";
/// Dimension of the counts per rule `match`.
pub(crate) const DIMENSION_RULE: &str = "rule";
/// Dimension of the counts per reviewer ID.
pub(crate) const DIMENSION_REVIEWER: &str = "reviewer";
/// Dimension of the failed attempts per error code.
pub(crate) const DIMENSION_FAILURE: &str = "failure";
/// Dimension of the runs per review step.
pub(crate) const DIMENSION_STEP: &str = "step";

/// Counter of the pull requests whose schedule record entered `failed`.
pub(crate) const COUNTER_STOPPED: &str = "stopped";

/// One counter of one key of one dimension on one UTC day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollupCount {
    /// UTC day, `YYYY-MM-DD`.
    pub day: String,
    /// What the key names: `day`, `repository`, `rule`, `reviewer`, `failure` or `step`.
    pub dimension: String,
    /// Repository, rule `match`, reviewer ID, error code or step; empty for `day`.
    pub key: String,
    /// Name of the counter, as the statistics document names it.
    pub counter: String,
    /// The count.
    pub value: i64,
}

/// One measured value of one key of one dimension on one UTC day, and how often it occurred.
///
/// A `step` sample is a duration in milliseconds; any other is seconds to publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollupSample {
    /// UTC day, `YYYY-MM-DD`.
    pub day: String,
    /// What the key names, as in `RollupCount`.
    pub dimension: String,
    /// The key, as in `RollupCount`.
    pub key: String,
    /// The measured value.
    pub sample: i64,
    /// How often the value occurred.
    pub count: i64,
}

/// Counts and samples of the rollup.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rollup {
    /// The counters.
    pub counts: Vec<RollupCount>,
    /// The measured values.
    pub samples: Vec<RollupSample>,
}

/// Adds every count and sample of `rollup` to the stored ones on `connection`.
///
/// Call it inside a transaction, so the addition commits with the change it records.
///
/// # Errors
///
/// Returns `store_unavailable` when SQLite fails.
pub(super) fn add_rollup(connection: &Connection, rollup: &Rollup) -> Result<(), Error> {
    for count in &rollup.counts {
        connection
            .execute(
                "INSERT INTO stats_rollup (day, dimension, key, counter, value) \
                 VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT (day, dimension, key, counter) \
                 DO UPDATE SET value = value + excluded.value",
                params![
                    count.day,
                    count.dimension,
                    count.key,
                    count.counter,
                    count.value
                ],
            )
            .map_err(unavailable)?;
    }
    for sample in &rollup.samples {
        connection
            .execute(
                "INSERT INTO stats_rollup_samples (day, dimension, key, sample, count) \
                 VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT (day, dimension, key, sample) \
                 DO UPDATE SET count = count + excluded.count",
                params![
                    sample.day,
                    sample.dimension,
                    sample.key,
                    sample.sample,
                    sample.count
                ],
            )
            .map_err(unavailable)?;
    }
    Ok(())
}

/// Reads the counts and samples of the days from `since` up to, not including, `before`.
///
/// # Errors
///
/// Returns `store_unavailable` when SQLite fails.
pub(super) fn read_rollup(
    connection: &Connection,
    since: &str,
    before: &str,
) -> Result<Rollup, Error> {
    let mut statement = connection
        .prepare(
            "SELECT day, dimension, key, counter, value FROM stats_rollup \
             WHERE day >= ?1 AND day < ?2 ORDER BY day, dimension, key, counter",
        )
        .map_err(unavailable)?;
    let rows = statement
        .query_map(params![since, before], |row| {
            Ok(RollupCount {
                day: row.get(0)?,
                dimension: row.get(1)?,
                key: row.get(2)?,
                counter: row.get(3)?,
                value: row.get(4)?,
            })
        })
        .map_err(unavailable)?;
    let counts = rows.collect::<Result<Vec<_>, _>>().map_err(unavailable)?;
    let mut statement = connection
        .prepare(
            "SELECT day, dimension, key, sample, count FROM stats_rollup_samples \
             WHERE day >= ?1 AND day < ?2 ORDER BY day, dimension, key, sample",
        )
        .map_err(unavailable)?;
    let rows = statement
        .query_map(params![since, before], |row| {
            Ok(RollupSample {
                day: row.get(0)?,
                dimension: row.get(1)?,
                key: row.get(2)?,
                sample: row.get(3)?,
                count: row.get(4)?,
            })
        })
        .map_err(unavailable)?;
    let samples = rows.collect::<Result<Vec<_>, _>>().map_err(unavailable)?;
    Ok(Rollup { counts, samples })
}

/// Returns the stops of `day`: the records of `schedule` that are `failed` and were not before.
///
/// A record was `failed` before when `stored` holds it in phase `failed` with the same
/// identity; other fields, such as the handled failure, change on a record that stays
/// stopped. Each stop counts in the day and under its repository, the key up to its last `#`
/// (the whole key without one).
pub(super) fn stops(
    day: &str,
    stored: &BTreeMap<String, Record>,
    schedule: &BTreeMap<String, Record>,
) -> Rollup {
    let mut repositories: BTreeMap<&str, i64> = BTreeMap::new();
    for (key, record) in schedule {
        let was_failed = stored.get(key).is_some_and(|before| {
            before.phase == phase::FAILED && before.identity == record.identity
        });
        if record.phase != phase::FAILED || was_failed {
            continue;
        }
        let repository = key.rsplit_once('#').map_or(key.as_str(), |(name, _)| name);
        *repositories.entry(repository).or_default() += 1;
    }
    let total: i64 = repositories.values().sum();
    if total == 0 {
        return Rollup::default();
    }
    let count = |dimension: &str, key: &str, value: i64| RollupCount {
        day: day.to_owned(),
        dimension: dimension.to_owned(),
        key: key.to_owned(),
        counter: COUNTER_STOPPED.to_owned(),
        value,
    };
    let mut counts = vec![count(DIMENSION_DAY, "", total)];
    for (repository, value) in repositories {
        counts.push(count(DIMENSION_REPOSITORY, repository, value));
    }
    Rollup {
        counts,
        samples: Vec::new(),
    }
}
