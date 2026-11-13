//! Reads `config.yaml`: every key of the configuration reference is required and is mapped into
//! the
//! engine configuration or the scheduler policy.

// Rust guideline compliant 2026-06-26

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use autoreview_engine::Error;
use autoreview_engine::config::{
    Config, HostOrder, HostStateChoice, HostStateRules, MatrixRooms, RouteOverride,
};
use autoreview_engine::mcp::{McpClientEntry, McpConfig, McpLimits};
use autoreview_engine::metrics::MetricsConfig;
use autoreview_policy::{EVENTS, Event, Policy, user_login_valid};
use autoreview_schedule::scheduler;
use serde::de::{self, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

use crate::env_file::{self, EnvFile};

/// Error code of a key that is absent or blank.
const CONFIGURATION_MISSING: &str = "configuration_missing";
/// Error code of a key that is present but invalid.
const CONFIGURATION_INVALID: &str = "configuration_invalid";
/// Name of the document in errors that concern the whole file.
const DOCUMENT_NAME: &str = "config";
/// Name of the `--config` flag in errors.
const CONFIG_FLAG: &str = "--config";
/// Environment variable that names the config file when `--config` is not given.
const CONFIG_ENV: &str = "AUTOREVIEW_CONFIG";
/// The only `version` this loader reads.
const CONFIG_VERSION: i64 = 1;
/// Longest debounce the scheduler accepts, in seconds.
const DEBOUNCE_SECONDS_MAX: i64 = 600;
/// Most retry delays the scheduler accepts.
const RETRY_DELAYS_MAX: usize = 5;
/// Longest retry delay the scheduler accepts, in seconds.
const RETRY_DELAY_SECONDS_MAX: i64 = 3600;
/// Longest credential name, in characters.
const CREDENTIAL_NAME_MAX: usize = 32;
/// Seconds in one day, the shortest review retention.
const SECONDS_PER_DAY: u64 = 86_400;
/// Longest discovery lock timeout, in seconds.
const DISCOVERY_LOCK_TIMEOUT_MAX_SECONDS: u64 = 3600;
/// Largest session output buffer, in bytes (16 MiB).
const OUTPUT_MAX_BYTES_LIMIT: i64 = 16 << 20;
/// Longest output wait, in milliseconds.
const OUTPUT_WAIT_MS_MAX: i64 = 8000;
/// Largest value of `limits.min_summary_characters`.
const MIN_SUMMARY_CHARACTERS_MAX: i64 = 500;
/// Divisor of `notify.max_characters` that bounds `limits.excerpt_characters`.
const EXCERPT_DIVISOR: i64 = 4;
/// Largest TCP port that a Matrix homeserver URL may name.
const PORT_MAX: u32 = 65_535;
/// Largest GitHub review body or comment, in bytes.
const GITHUB_BODY_MAX_BYTES_LIMIT: i64 = 65_536;
/// Largest value of `pohunek.host_state.min_battery_percent`.
const BATTERY_PERCENT_MAX: i64 = 100;
/// Largest command time limit that the pohunek daemon honours, in seconds.
///
/// pohunek 0.41.0 lowers a larger `--timeout-ms` to `MAX_COMMAND_TIMEOUT_MS` (60000,
/// `crates/protocol/src/limits.rs:87`).
const DAEMON_COMMAND_TIMEOUT_MAX_SECONDS: u64 = 60;
/// How long the pohunek CLI waits for a command's reply beyond its time limit, in seconds.
///
/// pohunek 0.41.0 waits `min(limit, 60 s) + 1 s`: `DEDICATED_WAIT_TRANSPORT_HEADROOM`
/// (`crates/client/src/transport.rs:94`), added in `dedicated_request_timeout` (`:1725`).
const CLI_REPLY_HEADROOM_SECONDS: u64 = 1;

/// A configuration value read without type coercion, so that each key decides what it accepts and
/// names itself when it refuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Value {
    /// A YAML integer that fits in an `i64`.
    Int(i64),
    /// A YAML string, quoted or plain.
    Text(String),
    /// A YAML sequence; its items are checked by the key that reads it.
    List(Vec<Value>),
    /// A YAML null inside a sequence.
    Null,
    /// A boolean, a float, a mapping, or an integer that does not fit in an `i64`.
    Other,
}

impl<'de> Deserialize<'de> for Value {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(ValueVisitor)
    }
}

/// Accepts every YAML node and keeps the parts a key can use.
struct ValueVisitor;

impl<'de> Visitor<'de> for ValueVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a configuration value")
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Value, E> {
        Ok(Value::Int(value))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Value, E> {
        match i64::try_from(value) {
            Ok(int) => Ok(Value::Int(int)),
            Err(_) => Ok(Value::Other),
        }
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Value, E> {
        Ok(Value::Text(value.to_owned()))
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Value, E> {
        Ok(Value::Other)
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Value, E> {
        Ok(Value::Other)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_none<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element::<Value>()? {
            items.push(item);
        }
        Ok(Value::List(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(Value::Other)
    }
}
/// The root of `config.yaml`. Every field is optional at the serde level so that an absent key
/// is reported with its own name instead of a serde message.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileConfig {
    /// Must be `1`.
    version: Option<Value>,
    /// Absolute path of the policy file.
    policy_file: Option<Value>,
    /// The `github` section.
    github: Option<GithubFile>,
    /// The `pohunek` section.
    pohunek: Option<PohunekFile>,
    /// The `session` section.
    session: Option<SessionFile>,
    /// The `schedule` section.
    schedule: Option<ScheduleFile>,
    /// The `limits` section.
    limits: Option<LimitsFile>,
    /// The `store` section.
    store: Option<StoreFile>,
    /// The `cli` section.
    cli: Option<CliFile>,
    /// The `publish` section.
    publish: Option<PublishFile>,
    /// The `notify` section.
    notify: Option<NotifyFile>,
    /// The `mcp` section; only `autoreview mcp` needs it.
    mcp: Option<McpFile>,
    /// The `metrics` section; without it `serve` runs no metrics endpoint. A null section reads
    /// as a section without keys, so its first key is reported missing.
    #[serde(default, deserialize_with = "present_section")]
    metrics: Option<MetricsFile>,
}

/// The `metrics` section.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct MetricsFile {
    /// Address the metrics endpoint listens on, as `IP:PORT`.
    listen: Option<Value>,
    /// `Host` header values the endpoint accepts.
    allowed_hosts: Option<Value>,
    /// Longest time one connection may stay open, in seconds.
    request_timeout_seconds: Option<Value>,
    /// Longest time a client may take to send the request headers, in seconds.
    header_timeout_seconds: Option<Value>,
    /// Most connections served at once.
    max_connections: Option<Value>,
}

/// The `mcp` section.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpFile {
    /// Address the MCP server listens on, as `IP:PORT`.
    listen: Option<Value>,
    /// `Host` header values the server accepts.
    allowed_hosts: Option<Value>,
    /// The clients of the server.
    clients: Option<Vec<McpClientFile>>,
    /// Longest time one request may take, in seconds.
    request_timeout_seconds: Option<Value>,
    /// Longest time a client may take to send the request headers, in seconds.
    header_timeout_seconds: Option<Value>,
    /// Largest request body in bytes.
    request_max_bytes: Option<Value>,
    /// Most connections served at once.
    max_connections: Option<Value>,
}

/// One entry of `mcp.clients`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpClientFile {
    /// Name of the client in the logs.
    name: Option<Value>,
    /// `read` or `write`.
    scope: Option<Value>,
    /// SHA-256 digest of the client's token, as lowercase hexadecimal.
    token_sha256: Option<Value>,
}

/// The `github` section.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GithubFile {
    /// GitHub host, `github.com` in this release.
    host: Option<Value>,
    /// Absolute path of the GitHub CLI.
    gh_bin: Option<Value>,
    /// Timeout of one `gh` call, in seconds.
    timeout_seconds: Option<Value>,
    /// How often every token is checked, in hours. Parsed, not mapped.
    credential_check_hours: Option<Value>,
    /// Days before a token expires at which a warning is raised. Parsed, not mapped.
    expiry_warning_days: Option<Value>,
    /// One or more credentials, first match wins.
    credentials: Option<Vec<CredentialFile>>,
}

/// One entry of `github.credentials`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialFile {
    /// Unique name of the credential.
    name: Option<Value>,
    /// The account the token belongs to.
    login: Option<Value>,
    /// Environment variable that holds the token.
    token_env: Option<Value>,
    /// Repositories the credential serves: `*`, `owner/*` or `owner/repository`.
    #[serde(rename = "match")]
    matches: Option<Value>,
}

/// The `pohunek` section.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PohunekFile {
    /// Absolute path of the pohunek CLI.
    bin: Option<Value>,
    /// `XDG_STATE_HOME` of the CLI, absolute.
    state_home: Option<Value>,
    /// `XDG_RUNTIME_DIR` of the CLI, absolute.
    runtime_dir: Option<Value>,
    /// Timeout of one pohunek call, in seconds.
    timeout_seconds: Option<Value>,
    /// pohunek host names that may run reviewers; one or more.
    hosts: Option<Value>,
    /// Pins a repository registered on more than one host.
    route_overrides: Option<BTreeMap<String, RouteOverrideFile>>,
    /// Picks review hosts by their state; without it the hosts are tried in configured order.
    /// A null section reads as a section without keys, so its first key is reported missing.
    #[serde(default, deserialize_with = "present_section")]
    host_state: Option<HostStateFile>,
}

/// The `pohunek.host_state` section.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct HostStateFile {
    /// Largest distance of a state's timestamp from now, in seconds.
    max_age_seconds: Option<Value>,
    /// The daemon's time limit for one state read, in seconds.
    read_timeout_seconds: Option<Value>,
    /// `skip` or `eligible`: a host whose state is unknown.
    unknown_state: Option<Value>,
    /// `idle_first` or `configured`: the order of the eligible hosts.
    order: Option<Value>,
    /// `skip` or `eligible`: a host on battery.
    on_battery: Option<Value>,
    /// Lowest battery charge of a host on battery, in percent; 0 sets no limit.
    min_battery_percent: Option<Value>,
    /// Least free memory of a host, in MB; 0 sets no limit.
    min_free_memory_mb: Option<Value>,
    /// Local models that a host must serve.
    required_models: Option<Value>,
    /// `skip` or `eligible`: a host outside its policy window.
    outside_window: Option<Value>,
}

/// One entry of `pohunek.route_overrides`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RouteOverrideFile {
    /// pohunek host that holds the repository.
    host: Option<Value>,
    /// pohunek project identifier on that host.
    project_id: Option<Value>,
}

/// The `session` section.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionFile {
    /// Prefix of the review branches.
    branch_prefix: Option<Value>,
    /// Prefix of the pohunek session names.
    name_prefix: Option<Value>,
    /// pohunek-companion group every review session is placed in.
    group: Option<Value>,
    /// Longest time one reviewer session may run, in seconds.
    timeout_seconds: Option<Value>,
    /// How often a running session's output is read, in seconds.
    output_poll_seconds: Option<Value>,
    /// Session start attempts inside one review.
    start_max_attempts: Option<Value>,
    /// Delay between session start attempts, in seconds.
    start_retry_seconds: Option<Value>,
    /// Session removal attempts.
    cleanup_max_attempts: Option<Value>,
    /// Delay between session removal attempts, in seconds.
    cleanup_retry_seconds: Option<Value>,
    /// Fresh sessions for a reviewer whose output was unusable.
    reviewer_run_max_attempts: Option<Value>,
    /// Delay between fresh reviewer sessions, in seconds.
    reviewer_run_retry_seconds: Option<Value>,
    /// Columns of the PTY of reviewer sessions.
    terminal_cols: Option<Value>,
    /// Rows of the PTY of reviewer sessions.
    terminal_rows: Option<Value>,
    /// Largest session output that is read, in bytes.
    output_max_bytes: Option<Value>,
    /// Size of one output chunk, in bytes.
    output_chunk_bytes: Option<Value>,
    /// Longest wait for output, in milliseconds.
    output_wait_ms: Option<Value>,
}

/// The `schedule` section.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScheduleFile {
    /// Time between discovery passes, in seconds.
    poll_interval_seconds: Option<Value>,
    /// How often the worker checks for reviews to start, in seconds.
    worker_tick_seconds: Option<Value>,
    /// Reviews running at once.
    max_concurrent_reviews: Option<Value>,
    /// Quiet time after a push before a review starts, in seconds.
    debounce_seconds: Option<Value>,
    /// Delays between scheduler attempts of a failed review, in seconds.
    retry_delays_seconds: Option<Value>,
    /// Total time of a review step that keeps failing transiently, in seconds.
    step_retry_seconds: Option<Value>,
    /// First backoff delay of a failing step, in seconds.
    step_retry_initial_seconds: Option<Value>,
    /// Largest backoff delay of a failing step, in seconds.
    step_retry_max_interval_seconds: Option<Value>,
    /// Longest wait for another discovery pass's lock, in seconds.
    discovery_lock_timeout_seconds: Option<Value>,
    /// Interval between lock attempts, in milliseconds.
    discovery_lock_poll_ms: Option<Value>,
    /// GitHub search result limit.
    poll_limit: Option<Value>,
    /// GitHub search page limit.
    search_page_limit: Option<Value>,
    /// Size bound of the discovery state, in bytes.
    scheduler_max_bytes: Option<Value>,
    /// Entry bound of the discovery state.
    state_entry_limit: Option<Value>,
}

/// The `limits` section.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LimitsFile {
    /// Size of one reviewer result, in bytes.
    result_max_bytes: Option<Value>,
    /// Findings in one result.
    max_findings: Option<Value>,
    /// Tests in one result.
    max_tests: Option<Value>,
    /// Shortest acceptable summary, in characters.
    min_summary_characters: Option<Value>,
    /// Longest acceptable summary, in characters.
    max_summary_characters: Option<Value>,
    /// Longest finding title, in characters.
    max_title_characters: Option<Value>,
    /// Longest finding body, in characters.
    max_body_characters: Option<Value>,
    /// Longest discussion handed to a re-review, in characters.
    max_discussion_characters: Option<Value>,
    /// Longest path, in characters.
    max_path_characters: Option<Value>,
    /// Longest test entry, in characters.
    max_test_characters: Option<Value>,
    /// Length of finding excerpts in notifications, in characters.
    excerpt_characters: Option<Value>,
}

/// The `store` section.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoreFile {
    /// Absolute path of the SQLite database.
    path: Option<Value>,
    /// How long a finished review stays, in seconds.
    review_retention_seconds: Option<Value>,
    /// How long a published decision is an incremental baseline, in seconds.
    published_retention_seconds: Option<Value>,
}

/// The `cli` section.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CliFile {
    /// Most reviews the review list command shows.
    review_list_limit: Option<Value>,
}

/// The `publish` section.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublishFile {
    /// Last line of every review body and inline comment.
    signature: Option<Value>,
    /// Text that every signature starts with and that marks machine-written comments.
    ai_signed_pattern: Option<Value>,
    /// Bound of one review body or comment, in bytes.
    github_body_max_bytes: Option<Value>,
}

/// The `notify` section.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NotifyFile {
    /// Timeout of one delivery, in seconds.
    timeout_seconds: Option<Value>,
    /// Longer messages are cut, in characters.
    max_characters: Option<Value>,
    /// Named notification backends.
    backends: Option<Vec<BackendFile>>,
    /// Backend names per notification level and per event with its own route.
    routes: Option<BTreeMap<String, Option<Value>>>,
    /// Hourly review queue.
    review_queue: Option<ReviewQueueFile>,
}

/// One entry of `notify.backends`, selected by its `type`.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
enum BackendFile {
    /// Writes nothing but the structured log.
    None {
        /// Name the routes refer to.
        name: Option<Value>,
    },
    /// Posts to a Matrix room.
    Matrix {
        /// Name the routes refer to.
        name: Option<Value>,
        /// Homeserver base URL, HTTPS or loopback HTTP.
        homeserver_url: Option<Value>,
        /// Environment variable that holds the access token.
        access_token_env: Option<Value>,
        /// Room identifier.
        room: Option<Value>,
        /// Matrix user mentioned in action messages.
        mention_user: Option<Value>,
    },
    /// POSTs one body per message.
    Webhook {
        /// Name the routes refer to.
        name: Option<Value>,
        /// Environment variable that holds the URL.
        url_env: Option<Value>,
        /// Body format: `json`, `slack` or `discord`.
        format: Option<Value>,
    },
}

/// The `notify.review_queue` section.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewQueueFile {
    /// Backends that receive the hourly review queue.
    backends: Option<Value>,
    /// Most pull requests listed in one queue message.
    max_items: Option<Value>,
}

/// Chooses the config file: `--config` wins over `AUTOREVIEW_CONFIG`, and neither is an error.
///
/// # Errors
///
/// Returns `configuration_missing` when neither is given and `configuration_invalid` when the
/// chosen path is empty.
pub(crate) fn resolve_path(
    flag: Option<&Path>,
    environment: Option<&OsStr>,
) -> Result<PathBuf, Error> {
    let (name, raw) = match (flag, environment) {
        (Some(path), _) => (CONFIG_FLAG, path.as_os_str()),
        (None, Some(value)) => (CONFIG_ENV, value),
        (None, None) => {
            return Err(Error::config(
                CONFIGURATION_MISSING,
                CONFIG_FLAG,
                Some(Box::from("neither --config nor AUTOREVIEW_CONFIG is set")),
            ));
        }
    };
    if raw.is_empty() {
        return Err(Error::config(
            CONFIGURATION_INVALID,
            name,
            Some(Box::from("must not be empty")),
        ));
    }
    Ok(PathBuf::from(raw))
}

/// Reads the config file at `path` and maps it, see [`parse`].
///
/// A secret comes from `env_file` when the file names its variable, else from the environment.
///
/// # Errors
///
/// Returns the errors of [`parse`], and `configuration_invalid` naming `config` when the file
/// cannot
/// be read.
pub(crate) fn load(
    path: &Path,
    env_file: Option<&EnvFile>,
) -> Result<(Config, Option<McpConfig>, Option<MetricsConfig>), Error> {
    let text = std::fs::read_to_string(path).map_err(|error| {
        Error::config(CONFIGURATION_INVALID, DOCUMENT_NAME, Some(Box::new(error)))
    })?;
    let lookup = |name: &str| env_file::secret(env_file, name, environment_secret);
    let config = parse(&text, lookup, |policy_path| {
        autoreview_policy::load(policy_path).map_err(|error| Box::new(error) as _)
    })?;
    let file = parse_file(&text)?;
    let mcp = mcp_section(&file)?;
    let metrics = metrics_section(&file)?;
    Ok((config, mcp, metrics))
}

/// Reads the `mcp` section; `None` when the configuration has none.
///
/// # Errors
///
/// Returns `configuration_missing` for an absent key of the section, and the errors of
/// [`McpConfig::new`].
pub(crate) fn mcp_section(file: &FileConfig) -> Result<Option<McpConfig>, Error> {
    let Some(mcp) = &file.mcp else {
        return Ok(None);
    };
    let listen = text(&mcp.listen, "mcp.listen")?;
    let allowed_hosts = text_list(&mcp.allowed_hosts, "mcp.allowed_hosts")?;
    let mut entries = Vec::new();
    for (index, client) in section(&mcp.clients, "mcp.clients")?.iter().enumerate() {
        let key = |field: &str| format!("mcp.clients[{index}].{field}");
        entries.push(McpClientEntry {
            name: text(&client.name, &key("name"))?,
            scope: text(&client.scope, &key("scope"))?,
            token_sha256: text(&client.token_sha256, &key("token_sha256"))?,
        });
    }
    let limits = McpLimits {
        request_timeout: seconds(&mcp.request_timeout_seconds, "mcp.request_timeout_seconds")?,
        header_timeout: seconds(&mcp.header_timeout_seconds, "mcp.header_timeout_seconds")?,
        request_max_bytes: count(&mcp.request_max_bytes, "mcp.request_max_bytes")?,
        max_connections: count(&mcp.max_connections, "mcp.max_connections")?,
    };
    McpConfig::new(&listen, allowed_hosts, &entries, limits).map(Some)
}

/// Reads the `metrics` section; `None` when the configuration has no `metrics` key.
///
/// # Errors
///
/// Returns `configuration_missing` for an absent key of the section (`metrics.listen` for a
/// null section), and the errors of [`MetricsConfig::new`].
pub(crate) fn metrics_section(file: &FileConfig) -> Result<Option<MetricsConfig>, Error> {
    let Some(metrics) = &file.metrics else {
        return Ok(None);
    };
    let listen = text(&metrics.listen, "metrics.listen")?;
    let allowed_hosts = text_list(&metrics.allowed_hosts, "metrics.allowed_hosts")?;
    let request_timeout = seconds(
        &metrics.request_timeout_seconds,
        "metrics.request_timeout_seconds",
    )?;
    let header_timeout = seconds(
        &metrics.header_timeout_seconds,
        "metrics.header_timeout_seconds",
    )?;
    let max_connections = count(&metrics.max_connections, "metrics.max_connections")?;
    MetricsConfig::new(
        &listen,
        allowed_hosts,
        request_timeout,
        header_timeout,
        max_connections,
    )
    .map(Some)
}

/// Reads a section that is present when its key is, also when its value is null.
fn present_section<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Option::<T>::deserialize(deserializer).map(|section| Some(section.unwrap_or_default()))
}

/// Reads a positive integer that counts items or bytes.
fn count(value: &Option<Value>, key: &str) -> Result<usize, Error> {
    let number = positive(value, key)?;
    usize::try_from(number).map_err(|_| invalid(key, "must be a positive integer"))
}

/// Reads a secret from the environment by variable name. Only the value of one variable is read,
/// and the value is never printed.
fn environment_secret(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// Parses the text of `config.yaml` and maps it into the engine configuration.
///
/// `secret` returns the value of the environment variable named by a `*_env` key, and
/// `load_policy` reads the file named by `policy_file`.
///
/// # Errors
///
/// Returns `configuration_missing` for an absent key, `configuration_invalid` for a key whose
/// value
/// is out of range or of the wrong kind or for a review queue without a Matrix backend, and the
/// engine's validation error for a mapped value that the engine rejects.
pub(crate) fn parse(
    text: &str,
    secret: impl Fn(&str) -> Option<String>,
    load_policy: impl FnOnce(&Path) -> Result<Policy, Box<dyn std::error::Error + Send + Sync>>,
) -> Result<Config, Error> {
    let file = parse_file(text)?;
    let mapped = map_file(&file, &secret, load_policy)?;
    // The queue posts only to Matrix until M3. The check follows the mapping, so a test can map
    // a config whose queue names only `none` backends.
    if !mapped.queue_on_matrix {
        return Err(invalid(
            "notify.review_queue.backends",
            "none is not supported for the review queue",
        ));
    }
    mapped.config.validate()?;
    Ok(mapped.config)
}

/// Reads the text of `config.yaml` into the file structs. Serde errors, such as an unknown key,
/// name
/// the key and the line.
///
/// # Errors
///
/// Returns `configuration_invalid` naming `config` when the text is not one YAML document of the
/// file structure.
pub(crate) fn parse_file(text: &str) -> Result<FileConfig, Error> {
    serde_saphyr::from_str::<FileConfig>(text).map_err(|error| {
        Error::config(
            CONFIGURATION_INVALID,
            DOCUMENT_NAME,
            Some(Box::from(error.to_string())),
        )
    })
}

/// Maps the file into the engine configuration without the engine's final validation.
///
/// A review queue without a Matrix backend maps to an empty `queue_matrix_room`. The tests use
/// it to map configs that [`parse`] refuses.
///
/// # Errors
///
/// Returns the errors of [`parse`] except the queue check and the engine's validation error.
#[cfg(test)]
pub(crate) fn to_config(
    file: &FileConfig,
    secret: &impl Fn(&str) -> Option<String>,
    load_policy: impl FnOnce(&Path) -> Result<Policy, Box<dyn std::error::Error + Send + Sync>>,
) -> Result<Config, Error> {
    map_file(file, secret, load_policy).map(|mapped| mapped.config)
}

/// The engine configuration and the loader facts that [`parse`] checks after the mapping.
struct Mapped {
    /// The engine configuration.
    config: Config,
    /// Whether `notify.review_queue.backends` names a Matrix backend.
    queue_on_matrix: bool,
}

/// Maps the file into the engine configuration with the loader facts, without the checks.
///
/// # Errors
///
/// Returns the errors of [`parse`] except the queue check and the engine's validation error.
fn map_file(
    file: &FileConfig,
    secret: &impl Fn(&str) -> Option<String>,
    load_policy: impl FnOnce(&Path) -> Result<Policy, Box<dyn std::error::Error + Send + Sync>>,
) -> Result<Mapped, Error> {
    let version = integer(present(&file.version, "version")?, "version")?;
    if version != CONFIG_VERSION {
        return Err(invalid("version", "must be 1"));
    }
    let policy_path = text(&file.policy_file, "policy_file")?;
    if !policy_path.starts_with('/') {
        return Err(invalid("policy_file", "must be an absolute path"));
    }
    let policy = load_policy(Path::new(policy_path.as_str()))
        .map_err(|error| Error::config(CONFIGURATION_INVALID, "policy_file", Some(error)))?;
    // Every `*_env` value is read through `recorded`, so the names cover each configured secret.
    let secret_variables = RefCell::new(BTreeSet::new());
    let recorded = |name: &str| {
        secret_variables.borrow_mut().insert(name.to_owned());
        secret(name)
    };
    let schedule_file = section(&file.schedule, "schedule")?;
    let schedule = scheduler_policy(
        &schedule_file.debounce_seconds,
        &schedule_file.retry_delays_seconds,
    )?;
    let github = section(&file.github, "github")?;
    let github_host = text(&github.host, "github.host")?;
    let gh_bin = text(&github.gh_bin, "github.gh_bin")?;
    let gh_timeout = seconds(&github.timeout_seconds, "github.timeout_seconds")?;
    let _ = positive(
        &github.credential_check_hours,
        "github.credential_check_hours",
    )?;
    let _ = positive(&github.expiry_warning_days, "github.expiry_warning_days")?;
    let credentials = github_credentials(&github.credentials, &recorded)?;
    let (github_login, github_token) = single_credential(&credentials)?;
    let pohunek = section(&file.pohunek, "pohunek")?;
    let pohunek_bin = text(&pohunek.bin, "pohunek.bin")?;
    let pohunek_state = text(&pohunek.state_home, "pohunek.state_home")?;
    let pohunek_runtime = text(&pohunek.runtime_dir, "pohunek.runtime_dir")?;
    let pohunek_timeout = seconds(&pohunek.timeout_seconds, "pohunek.timeout_seconds")?;
    let pohunek_hosts = text_list(&pohunek.hosts, "pohunek.hosts")?;
    if pohunek_hosts.is_empty() {
        return Err(invalid("pohunek.hosts", "must list at least one host"));
    }
    let route_overrides = match &pohunek.route_overrides {
        Some(overrides) => route_overrides(overrides)?,
        None => return Err(missing("pohunek.route_overrides")),
    };
    let host_state = host_state_rules(&pohunek.host_state, pohunek_timeout)?;
    let session = section(&file.session, "session")?;
    let session_branch_prefix = branch_prefix(&session.branch_prefix)?;
    let session_name_prefix = name_prefix(&session.name_prefix)?;
    let session_group = text(&session.group, "session.group")?;
    let session_timeout = seconds(&session.timeout_seconds, "session.timeout_seconds")?;
    let output_poll_interval =
        seconds(&session.output_poll_seconds, "session.output_poll_seconds")?;
    let session_start_attempts =
        positive(&session.start_max_attempts, "session.start_max_attempts")?;
    let session_start_retry = seconds(&session.start_retry_seconds, "session.start_retry_seconds")?;
    let session_cleanup_attempts = positive(
        &session.cleanup_max_attempts,
        "session.cleanup_max_attempts",
    )?;
    let session_cleanup_retry = seconds(
        &session.cleanup_retry_seconds,
        "session.cleanup_retry_seconds",
    )?;
    let reviewer_run_attempts = positive(
        &session.reviewer_run_max_attempts,
        "session.reviewer_run_max_attempts",
    )?;
    let reviewer_run_retry = seconds(
        &session.reviewer_run_retry_seconds,
        "session.reviewer_run_retry_seconds",
    )?;
    let terminal_cols = positive(&session.terminal_cols, "session.terminal_cols")?;
    let terminal_rows = positive(&session.terminal_rows, "session.terminal_rows")?;
    let output_max_bytes = positive(&session.output_max_bytes, "session.output_max_bytes")?;
    let output_chunk_bytes = positive(&session.output_chunk_bytes, "session.output_chunk_bytes")?;
    let output_wait_ms = positive(&session.output_wait_ms, "session.output_wait_ms")?;
    let poll_interval = seconds(
        &schedule_file.poll_interval_seconds,
        "schedule.poll_interval_seconds",
    )?;
    let worker_tick = seconds(
        &schedule_file.worker_tick_seconds,
        "schedule.worker_tick_seconds",
    )?;
    let max_concurrent_reviews = positive(
        &schedule_file.max_concurrent_reviews,
        "schedule.max_concurrent_reviews",
    )?;
    let step_retry = seconds(
        &schedule_file.step_retry_seconds,
        "schedule.step_retry_seconds",
    )?;
    let step_retry_initial = seconds(
        &schedule_file.step_retry_initial_seconds,
        "schedule.step_retry_initial_seconds",
    )?;
    let step_retry_max_interval = seconds(
        &schedule_file.step_retry_max_interval_seconds,
        "schedule.step_retry_max_interval_seconds",
    )?;
    let discovery_lock_timeout = seconds(
        &schedule_file.discovery_lock_timeout_seconds,
        "schedule.discovery_lock_timeout_seconds",
    )?;
    let discovery_lock_poll = milliseconds(
        &schedule_file.discovery_lock_poll_ms,
        "schedule.discovery_lock_poll_ms",
    )?;
    let poll_limit = positive(&schedule_file.poll_limit, "schedule.poll_limit")?;
    let search_page_limit = positive(
        &schedule_file.search_page_limit,
        "schedule.search_page_limit",
    )?;
    let schedule_max_bytes = positive(
        &schedule_file.scheduler_max_bytes,
        "schedule.scheduler_max_bytes",
    )?;
    let state_entry_limit = positive(
        &schedule_file.state_entry_limit,
        "schedule.state_entry_limit",
    )?;
    let limits = section(&file.limits, "limits")?;
    let result_max_bytes = positive(&limits.result_max_bytes, "limits.result_max_bytes")?;
    let max_findings = positive(&limits.max_findings, "limits.max_findings")?;
    let max_tests = positive(&limits.max_tests, "limits.max_tests")?;
    let min_summary_characters = positive(
        &limits.min_summary_characters,
        "limits.min_summary_characters",
    )?;
    let max_summary_characters = positive(
        &limits.max_summary_characters,
        "limits.max_summary_characters",
    )?;
    let max_title_characters =
        positive(&limits.max_title_characters, "limits.max_title_characters")?;
    let max_body_characters = positive(&limits.max_body_characters, "limits.max_body_characters")?;
    let max_discussion_characters = positive(
        &limits.max_discussion_characters,
        "limits.max_discussion_characters",
    )?;
    let max_path_characters = positive(&limits.max_path_characters, "limits.max_path_characters")?;
    let max_test_characters = positive(&limits.max_test_characters, "limits.max_test_characters")?;
    let excerpt_characters = positive(&limits.excerpt_characters, "limits.excerpt_characters")?;
    let store = section(&file.store, "store")?;
    let state_db_path = text(&store.path, "store.path")?;
    let review_retention = seconds(
        &store.review_retention_seconds,
        "store.review_retention_seconds",
    )?;
    let published_retention = seconds(
        &store.published_retention_seconds,
        "store.published_retention_seconds",
    )?;
    let cli = section(&file.cli, "cli")?;
    let review_list_limit = positive(&cli.review_list_limit, "cli.review_list_limit")?;
    let publish = section(&file.publish, "publish")?;
    let ai_signature = text(&publish.signature, "publish.signature")?;
    let _ = text(&publish.ai_signed_pattern, "publish.ai_signed_pattern")?;
    let github_body_max_bytes = positive(
        &publish.github_body_max_bytes,
        "publish.github_body_max_bytes",
    )?;
    let notify = section(&file.notify, "notify")?;
    let matrix_timeout = seconds(&notify.timeout_seconds, "notify.timeout_seconds")?;
    let matrix_max_characters = positive(&notify.max_characters, "notify.max_characters")?;
    let backends = notify_backends(&notify.backends, &recorded)?;
    let routes = section(&notify.routes, "notify.routes")?;
    let review_queue = section(&notify.review_queue, "notify.review_queue")?;
    let queue_max_items = positive(&review_queue.max_items, "notify.review_queue.max_items")?;
    let queue_backends = text_list(&review_queue.backends, "notify.review_queue.backends")?;
    let shape = matrix_shape(&backends, routes, &queue_backends)?;
    let queue_on_matrix = shape.queue_room.is_some();
    let config = Config {
        policy: Some(policy),
        schedule,
        github_host,
        github_login,
        github_token,
        gh_bin,
        pohunek_bin,
        pohunek_state,
        pohunek_runtime,
        pohunek_hosts,
        route_overrides,
        host_state,
        matrix_homeserver_url: shape.homeserver_url,
        matrix_access_token: shape.access_token,
        secret_variables: secret_variables.take(),
        matrix_mention_user: shape.mention_user,
        matrix_rooms: shape.rooms,
        queue_matrix_room: shape.queue_room.unwrap_or_default(),
        queue_max_items,
        state_db_path,
        gh_timeout,
        pohunek_timeout,
        matrix_timeout,
        session_timeout,
        output_poll_interval,
        session_start_retry,
        session_cleanup_retry,
        reviewer_run_retry,
        step_retry,
        step_retry_initial,
        step_retry_max_interval,
        poll_interval,
        worker_tick,
        discovery_lock_timeout,
        discovery_lock_poll,
        review_retention,
        published_retention,
        session_start_attempts,
        session_cleanup_attempts,
        reviewer_run_attempts,
        max_concurrent_reviews,
        review_list_limit,
        poll_limit,
        search_page_limit,
        state_entry_limit,
        schedule_max_bytes,
        output_max_bytes,
        output_chunk_bytes,
        output_wait_ms,
        result_max_bytes,
        max_findings,
        max_tests,
        min_summary_characters,
        max_summary_characters,
        max_title_characters,
        max_body_characters,
        max_discussion_characters,
        max_path_characters,
        max_test_characters,
        github_body_max_bytes,
        ai_signature,
        matrix_max_characters,
        excerpt_characters,
        terminal_cols,
        terminal_rows,
        session_group,
        session_name_prefix,
        session_branch_prefix,
    };
    cross_key_rules(file, &config)?;
    Ok(Mapped {
        config,
        queue_on_matrix,
    })
}

/// A validated entry of `github.credentials`.
struct Credential {
    /// The account the token belongs to.
    login: String,
    /// Repository patterns the credential serves.
    matches: Vec<String>,
    /// The token, read from the environment variable named by `token_env`.
    token: String,
}

/// Validates every entry of `github.credentials` and reads its token.
///
/// # Errors
///
/// Returns `configuration_missing` for an absent list or token variable and
/// `configuration_invalid` for a malformed entry or a duplicate name.
fn github_credentials(
    list: &Option<Vec<CredentialFile>>,
    secret: &impl Fn(&str) -> Option<String>,
) -> Result<Vec<Credential>, Error> {
    let entries = match list {
        Some(entries) => entries,
        None => return Err(missing("github.credentials")),
    };
    if entries.is_empty() {
        return Err(invalid(
            "github.credentials",
            "must list at least one credential",
        ));
    }
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for entry in entries {
        let name = text(&entry.name, "github.credentials.name")?;
        if !credential_name_valid(name.as_str()) || !seen.insert(name.clone()) {
            return Err(invalid(
                "github.credentials.name",
                "must be unique and match [a-z0-9][a-z0-9_-]{0,31}",
            ));
        }
        let login = text(&entry.login, "github.credentials.login")?;
        if !user_login_valid(login.as_str()) {
            return Err(invalid("github.credentials.login", "is not a GitHub login"));
        }
        let token_env = text(&entry.token_env, "github.credentials.token_env")?;
        let token = secret_value(token_env.as_str(), secret, "github.credentials.token_env")?;
        let matches = text_list(&entry.matches, "github.credentials.match")?;
        if matches.is_empty()
            || matches
                .iter()
                .any(|pattern| !repository_pattern_valid(pattern))
        {
            return Err(invalid(
                "github.credentials.match",
                "must list owner/repository, owner/* or *",
            ));
        }
        out.push(Credential {
            login,
            matches,
            token,
        });
    }
    Ok(out)
}

/// Returns the login and token of the one credential that serves every repository.
///
/// # Errors
///
/// Returns `configuration_invalid` for any other shape, which stays refused until M2.
fn single_credential(credentials: &[Credential]) -> Result<(String, String), Error> {
    if credentials.len() == 1
        && let Some(only) = credentials.first()
        && only.matches == ["*"]
    {
        return Ok((only.login.clone(), only.token.clone()));
    }
    Err(invalid(
        "github.credentials",
        "only one credential with match [\"*\"] is supported until M2",
    ))
}

/// Reports whether a credential name matches `[a-z0-9][a-z0-9_-]{0,31}`.
fn credential_name_valid(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > CREDENTIAL_NAME_MAX {
        return false;
    }
    if !bytes
        .first()
        .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
    {
        return false;
    }
    bytes.iter().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_' || *byte == b'-'
    })
}

/// Reports whether a match entry is `*`, `owner/*` or `owner/repository` in lowercase.
fn repository_pattern_valid(pattern: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    let Some((owner, repository)) = pattern.split_once('/') else {
        return false;
    };
    if owner.is_empty() || repository.is_empty() || repository.contains('/') {
        return false;
    }
    if repository != "*" && repository.contains('*') {
        return false;
    }
    if owner.contains('*') {
        return false;
    }
    pattern.bytes().all(|byte| {
        byte.is_ascii_lowercase()
            || byte.is_ascii_digit()
            || matches!(byte, b'-' | b'_' | b'.' | b'/' | b'*')
    })
}

/// Maps `pohunek.route_overrides`; every key names one repository.
///
/// # Errors
///
/// Returns `configuration_invalid` for a key that is not `owner/repository`.
fn route_overrides(
    overrides: &BTreeMap<String, RouteOverrideFile>,
) -> Result<BTreeMap<String, RouteOverride>, Error> {
    let mut out = BTreeMap::new();
    for (repository, entry) in overrides {
        if *repository == "*" || repository.contains('*') || !repository_pattern_valid(repository) {
            return Err(invalid(
                "pohunek.route_overrides",
                "keys must be owner/repository",
            ));
        }
        let host = text(&entry.host, "pohunek.route_overrides.host")?;
        let project_id = text(&entry.project_id, "pohunek.route_overrides.project_id")?;
        out.insert(repository.clone(), RouteOverride { host, project_id });
    }
    Ok(out)
}

/// Maps `pohunek.host_state`; `None` when the section is absent.
///
/// `pohunek_timeout` is `pohunek.timeout_seconds`: the daemon must stop a hung read first.
///
/// # Errors
///
/// Returns `configuration_missing` for an absent key of the section (`max_age_seconds` for a
/// null section) and `configuration_invalid` for a value outside its choices or range.
fn host_state_rules(
    section: &Option<HostStateFile>,
    pohunek_timeout: Duration,
) -> Result<Option<HostStateRules>, Error> {
    let Some(rules) = section else {
        return Ok(None);
    };
    let max_age = seconds(&rules.max_age_seconds, "pohunek.host_state.max_age_seconds")?;
    let read_key = "pohunek.host_state.read_timeout_seconds";
    let read_timeout = seconds(&rules.read_timeout_seconds, read_key)?;
    // A larger limit would be lowered by the daemon, so the key would not say what applies.
    if read_timeout.as_secs() > DAEMON_COMMAND_TIMEOUT_MAX_SECONDS {
        return Err(invalid(read_key, "must be at most 60"));
    }
    // The CLI must give its answer, or give up, before the call itself is killed.
    let cli_wait = read_timeout.saturating_add(Duration::from_secs(CLI_REPLY_HEADROOM_SECONDS));
    if cli_wait >= pohunek_timeout {
        return Err(invalid(
            read_key,
            "must be at most pohunek.timeout_seconds - 2",
        ));
    }
    let unknown_state = state_choice(&rules.unknown_state, "pohunek.host_state.unknown_state")?;
    let order_key = "pohunek.host_state.order";
    let order = match text(&rules.order, order_key)?.as_str() {
        "idle_first" => HostOrder::IdleFirst,
        "configured" => HostOrder::Configured,
        _ => return Err(invalid(order_key, "must be idle_first or configured")),
    };
    let on_battery = state_choice(&rules.on_battery, "pohunek.host_state.on_battery")?;
    let battery_key = "pohunek.host_state.min_battery_percent";
    let range = "must be from 0 to 100";
    let battery = bounded(
        &rules.min_battery_percent,
        battery_key,
        BATTERY_PERCENT_MAX,
        range,
    )?;
    let min_battery_percent = u8::try_from(battery).map_err(|_| invalid(battery_key, range))?;
    let memory_key = "pohunek.host_state.min_free_memory_mb";
    let negative = "must not be negative";
    let memory = bounded(&rules.min_free_memory_mb, memory_key, i64::MAX, negative)?;
    let min_free_memory_mb = u64::try_from(memory).map_err(|_| invalid(memory_key, negative))?;
    let models_key = "pohunek.host_state.required_models";
    let required_models = text_list(&rules.required_models, models_key)?;
    if required_models.iter().any(|model| model.trim().is_empty()) {
        return Err(invalid(models_key, "must list model names"));
    }
    let outside_window = state_choice(&rules.outside_window, "pohunek.host_state.outside_window")?;
    Ok(Some(HostStateRules {
        max_age,
        read_timeout,
        unknown_state,
        order,
        on_battery,
        min_battery_percent,
        min_free_memory_mb,
        required_models,
        outside_window,
    }))
}

/// Reads `skip` or `eligible`.
fn state_choice(value: &Option<Value>, key: &str) -> Result<HostStateChoice, Error> {
    match text(value, key)?.as_str() {
        "skip" => Ok(HostStateChoice::Skip),
        "eligible" => Ok(HostStateChoice::Eligible),
        _ => Err(invalid(key, "must be skip or eligible")),
    }
}

/// Reads an integer from 0 to `max`; `message` names the range when it is outside.
fn bounded(
    value: &Option<Value>,
    key: &str,
    max: i64,
    message: &'static str,
) -> Result<i64, Error> {
    let number = integer(present(value, key)?, key)?;
    if !(0..=max).contains(&number) {
        return Err(invalid(key, message));
    }
    Ok(number)
}

/// A validated entry of `notify.backends`.
enum Backend {
    /// The `none` type, which writes nothing but the structured log.
    None {
        /// Name the routes refer to.
        name: String,
    },
    /// The `matrix` type.
    Matrix {
        /// Name the routes refer to.
        name: String,
        /// Homeserver base URL.
        homeserver_url: String,
        /// Environment variable that holds the access token.
        access_token_env: String,
        /// The access token, read from the environment variable named by the key.
        access_token: String,
        /// Room identifier.
        room: String,
        /// Matrix user mentioned in action messages.
        mention_user: String,
    },
    /// The `webhook` type; parsed and validated, but routes to it are refused until M3.
    Webhook {
        /// Name the routes refer to.
        name: String,
    },
}

impl Backend {
    /// Returns the name the routes refer to.
    fn name(&self) -> &str {
        match self {
            Backend::None { name } => name,
            Backend::Matrix { name, .. } => name,
            Backend::Webhook { name } => name,
        }
    }

    /// Returns the homeserver, token variable and mentioned user of a Matrix backend.
    fn matrix_identity(&self) -> Option<(&str, &str, &str)> {
        match self {
            Backend::Matrix {
                homeserver_url,
                access_token_env,
                mention_user,
                ..
            } => Some((homeserver_url, access_token_env, mention_user)),
            Backend::None { .. } | Backend::Webhook { .. } => None,
        }
    }
}

/// Validates `notify.backends` and reads the Matrix access token.
///
/// # Errors
///
/// Returns `configuration_missing` for an absent list or token variable and
/// `configuration_invalid` for a malformed or duplicate backend.
fn notify_backends(
    list: &Option<Vec<BackendFile>>,
    secret: &impl Fn(&str) -> Option<String>,
) -> Result<Vec<Backend>, Error> {
    let entries = match list {
        Some(entries) => entries,
        None => return Err(missing("notify.backends")),
    };
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for entry in entries {
        match entry {
            BackendFile::None { name } => {
                let name = text(name, "notify.backends.name")?;
                if !credential_name_valid(name.as_str()) || !seen.insert(name.clone()) {
                    return Err(invalid(
                        "notify.backends.name",
                        "must be unique and match [a-z0-9][a-z0-9_-]{0,31}",
                    ));
                }
                out.push(Backend::None { name });
            }
            BackendFile::Matrix {
                name,
                homeserver_url: url,
                access_token_env,
                room,
                mention_user,
            } => {
                let name = text(name, "notify.backends.name")?;
                if !credential_name_valid(name.as_str()) || !seen.insert(name.clone()) {
                    return Err(invalid(
                        "notify.backends.name",
                        "must be unique and match [a-z0-9][a-z0-9_-]{0,31}",
                    ));
                }
                let homeserver_url = homeserver_url(url)?;
                let variable = text(access_token_env, "notify.backends.access_token_env")?;
                let access_token = secret_value(
                    variable.as_str(),
                    secret,
                    "notify.backends.access_token_env",
                )?;
                let room = text(room, "notify.backends.room")?;
                let mention_user = text(mention_user, "notify.backends.mention_user")?;
                out.push(Backend::Matrix {
                    name,
                    homeserver_url,
                    access_token_env: variable,
                    access_token,
                    room,
                    mention_user,
                });
            }
            BackendFile::Webhook {
                name,
                url_env,
                format,
            } => {
                let name = text(name, "notify.backends.name")?;
                if !credential_name_valid(name.as_str()) || !seen.insert(name.clone()) {
                    return Err(invalid(
                        "notify.backends.name",
                        "must be unique and match [a-z0-9][a-z0-9_-]{0,31}",
                    ));
                }
                let variable = text(url_env, "notify.backends.url_env")?;
                let _ = secret_value(variable.as_str(), secret, "notify.backends.url_env")?;
                let format = text(format, "notify.backends.format")?;
                if format != "json" && format != "slack" && format != "discord" {
                    return Err(invalid(
                        "notify.backends.format",
                        "must be json, slack or discord",
                    ));
                }
                out.push(Backend::Webhook { name });
            }
        }
    }
    Ok(out)
}

/// Reads the Matrix homeserver URL. When it names a port, the port must be at most 65535.
///
/// # Errors
///
/// Returns `configuration_missing` for an absent key and `configuration_invalid` for a port
/// that is empty, not decimal, or above 65535.
fn homeserver_url(value: &Option<Value>) -> Result<String, Error> {
    let url = text(value, "notify.backends.homeserver_url")?;
    let after_scheme = match url.split_once("://") {
        Some((_, rest)) => rest,
        None => url.as_str(),
    };
    let authority = match after_scheme.split_once('/') {
        Some((authority, _)) => authority,
        None => after_scheme,
    };
    let without_userinfo = match authority.rsplit_once('@') {
        Some((_, host)) => host,
        None => authority,
    };
    let port = if without_userinfo.starts_with('[') {
        without_userinfo.split_once("]:").map(|(_, port)| port)
    } else {
        without_userinfo.rsplit_once(':').map(|(_, port)| port)
    };
    if let Some(port) = port {
        let valid = !port.is_empty()
            && port.bytes().all(|byte| byte.is_ascii_digit())
            && port.parse::<u32>().is_ok_and(|number| number <= PORT_MAX);
        if !valid {
            return Err(invalid(
                "notify.backends.homeserver_url",
                "port must be at most 65535",
            ));
        }
    }
    Ok(url)
}

/// Reads the secret named by `variable`, which must match `AUTOREVIEW_[A-Z0-9_]+`.
///
/// # Errors
///
/// Returns `configuration_invalid` naming `key` for any other name and
/// `configuration_missing` for an unset or blank variable. The value never appears in an error.
fn secret_value(
    variable: &str,
    secret: &impl Fn(&str) -> Option<String>,
    key: &str,
) -> Result<String, Error> {
    if !env_file::name_valid(variable) {
        return Err(invalid(key, "must match AUTOREVIEW_[A-Z0-9_]+"));
    }
    match secret(variable) {
        Some(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(missing(key)),
    }
}

/// The Matrix values of the engine configuration.
struct MatrixShape {
    /// Homeserver base URL, empty when no route and no queue reaches Matrix.
    homeserver_url: String,
    /// Access token, empty when no route and no queue reaches Matrix.
    access_token: String,
    /// Mentioned user, empty when no route and no queue reaches Matrix.
    mention_user: String,
    /// The level and event rooms; empty for a `none` backend.
    rooms: MatrixRooms,
    /// The review queue room; `None` when the queue names no Matrix backend.
    queue_room: Option<String>,
}

/// Maps the routes and the review queue onto Matrix backends that share one identity.
///
/// The levels are checked first, then the event routes in key order, then the queue.
///
/// # Errors
///
/// Returns `configuration_invalid` when a route key is neither a level nor an event, when a
/// route names other than one backend, names a webhook, or names a Matrix backend with another
/// identity, or when the queue names two Matrix backends.
fn matrix_shape(
    backends: &[Backend],
    routes: &BTreeMap<String, Option<Value>>,
    queue_backends: &[String],
) -> Result<MatrixShape, Error> {
    let mut matrix: Option<&Backend> = None;
    let action = level_room(
        backends,
        &routes.get("action").cloned().flatten(),
        "notify.routes.action",
        &mut matrix,
    )?;
    let digest = level_room(
        backends,
        &routes.get("digest").cloned().flatten(),
        "notify.routes.digest",
        &mut matrix,
    )?;
    let log = level_room(
        backends,
        &routes.get("log").cloned().flatten(),
        "notify.routes.log",
        &mut matrix,
    )?;
    let events = event_rooms(backends, routes, &mut matrix)?;
    let queue_room = queue_room(backends, queue_backends, &mut matrix)?;
    let (homeserver_url, access_token, mention_user) = match matrix {
        Some(Backend::Matrix {
            homeserver_url,
            access_token,
            mention_user,
            ..
        }) => (
            homeserver_url.clone(),
            access_token.clone(),
            mention_user.clone(),
        ),
        _ => (String::new(), String::new(), String::new()),
    };
    Ok(MatrixShape {
        homeserver_url,
        access_token,
        mention_user,
        rooms: MatrixRooms {
            action,
            digest,
            log,
            events,
        },
        queue_room,
    })
}

/// The keys of `notify.routes` that name a level; every other key names an event.
const LEVEL_ROUTES: [&str; 3] = ["action", "digest", "log"];

/// Resolves every event route of `notify.routes` to its room, in key order.
///
/// # Errors
///
/// Returns `configuration_invalid` naming `notify.routes.<key>` when the key names no event,
/// and the errors of [`level_room`] for its backends.
fn event_rooms<'a>(
    backends: &'a [Backend],
    routes: &BTreeMap<String, Option<Value>>,
    matrix: &mut Option<&'a Backend>,
) -> Result<BTreeMap<Event, String>, Error> {
    let mut rooms = BTreeMap::new();
    for (name, names) in routes {
        if LEVEL_ROUTES.contains(&name.as_str()) {
            continue;
        }
        let key = format!("notify.routes.{name}");
        let Some(event) = EVENTS.iter().find(|event| event.as_str() == name) else {
            return Err(invalid(&key, "names no notification event"));
        };
        let room = level_room(backends, names, &key, matrix)?;
        rooms.insert(event.clone(), room);
    }
    Ok(rooms)
}

/// Resolves one level or event route to its room and checks its backend.
///
/// # Errors
///
/// Returns `configuration_invalid` when the route does not name exactly one backend, the
/// backend is a webhook, or its identity differs from the Matrix backend chosen before.
fn level_room<'a>(
    backends: &'a [Backend],
    names: &Option<Value>,
    key: &str,
    matrix: &mut Option<&'a Backend>,
) -> Result<String, Error> {
    let names = text_list(names, key)?;
    let [name] = names.as_slice() else {
        return Err(invalid(key, "must name exactly one backend until M3"));
    };
    let backend = find_backend(backends, name, key)?;
    match backend {
        Backend::None { .. } => Ok(String::new()),
        Backend::Matrix { room, .. } => {
            share_identity(backend, key, matrix)?;
            Ok(room.clone())
        }
        Backend::Webhook { .. } => Err(invalid(key, "webhook backends are not routed until M3")),
    }
}

/// Resolves the review queue to its Matrix room, or to `None` without a Matrix backend.
///
/// # Errors
///
/// Returns `configuration_invalid` naming `notify.review_queue.backends` for an empty list, a
/// webhook, a second Matrix backend, or a Matrix backend whose identity differs from the routes.
fn queue_room<'a>(
    backends: &'a [Backend],
    names: &[String],
    matrix: &mut Option<&'a Backend>,
) -> Result<Option<String>, Error> {
    let key = "notify.review_queue.backends";
    if names.is_empty() {
        return Err(invalid(key, "must name at least one backend"));
    }
    let mut queue: Option<&Backend> = None;
    for name in names {
        let backend = find_backend(backends, name, key)?;
        match backend {
            Backend::None { .. } => {}
            Backend::Matrix { .. } => {
                if queue.is_some_and(|chosen| !std::ptr::eq(chosen, backend)) {
                    return Err(invalid(
                        key,
                        "must name at most one Matrix backend until M3",
                    ));
                }
                share_identity(backend, key, matrix)?;
                queue = Some(backend);
            }
            Backend::Webhook { .. } => {
                return Err(invalid(key, "webhook backends are not routed until M3"));
            }
        }
    }
    match queue {
        Some(Backend::Matrix { room, .. }) => Ok(Some(room.clone())),
        _ => Ok(None),
    }
}

/// Records `backend` as the Matrix identity, or checks it against the identity chosen before.
///
/// # Errors
///
/// Returns `configuration_invalid` naming `key` when `homeserver_url`, `access_token_env` or
/// `mention_user` differs from the Matrix backend chosen before.
fn share_identity<'a>(
    backend: &'a Backend,
    key: &str,
    matrix: &mut Option<&'a Backend>,
) -> Result<(), Error> {
    match matrix {
        Some(chosen) => {
            if chosen.matrix_identity() != backend.matrix_identity() {
                return Err(invalid(
                    key,
                    "Matrix backends must share homeserver_url, access_token_env and \
                     mention_user until M3",
                ));
            }
        }
        None => *matrix = Some(backend),
    }
    Ok(())
}

/// Finds the backend called `name`.
///
/// # Errors
///
/// Returns `configuration_invalid` naming `key` when no backend has that name.
fn find_backend<'a>(backends: &'a [Backend], name: &str, key: &str) -> Result<&'a Backend, Error> {
    for backend in backends {
        if backend.name() == name {
            return Ok(backend);
        }
    }
    Err(invalid(key, "names no configured backend"))
}

/// Ports Go's `schedule.LoadPolicy`: the debounce is checked before the retry delays.
///
/// # Errors
///
/// Returns `configuration_missing` for an absent key and `configuration_invalid` for a value
/// outside
/// the bounds, with the Go message as cause.
pub(crate) fn scheduler_policy(
    debounce: &Option<Value>,
    retry_delays: &Option<Value>,
) -> Result<scheduler::Policy, Error> {
    let debounce = debounce_seconds(present(debounce, DEBOUNCE_KEY)?)?;
    let retry_delays = retry_delay_seconds(present(retry_delays, RETRY_DELAYS_KEY)?)?;
    Ok(scheduler::Policy {
        debounce,
        retry_delays,
    })
}

/// Name of the debounce key in errors.
const DEBOUNCE_KEY: &str = "schedule.debounce_seconds";
/// Name of the retry delays key in errors.
const RETRY_DELAYS_KEY: &str = "schedule.retry_delays_seconds";

/// Accepts an integer from 1 to [`DEBOUNCE_SECONDS_MAX`].
fn debounce_seconds(value: &Value) -> Result<time::Duration, Error> {
    match value {
        Value::Int(number) if (1..=DEBOUNCE_SECONDS_MAX).contains(number) => {
            Ok(time::Duration::seconds(*number))
        }
        _ => Err(Error::config(
            CONFIGURATION_INVALID,
            DEBOUNCE_KEY,
            Some(Box::from("invalid or missing configuration")),
        )),
    }
}

/// Accepts 1 to [`RETRY_DELAYS_MAX`] integers, strictly increasing, each at most
/// [`RETRY_DELAY_SECONDS_MAX`].
fn retry_delay_seconds(value: &Value) -> Result<Vec<time::Duration>, Error> {
    let invalid_list = || {
        Error::config(
            CONFIGURATION_INVALID,
            RETRY_DELAYS_KEY,
            Some(Box::from("invalid or missing configuration")),
        )
    };
    let items = match value {
        Value::List(items) => items,
        _ => return Err(invalid_list()),
    };
    if items.is_empty() || items.len() > RETRY_DELAYS_MAX {
        return Err(invalid_list());
    }
    let mut delays = Vec::new();
    for item in items {
        match item {
            Value::Int(number) => delays.push(*number),
            Value::Null => delays.push(0),
            _ => return Err(invalid_list()),
        }
    }
    let mut out = Vec::new();
    let mut previous = 0;
    for delay in delays {
        if delay <= previous || delay > RETRY_DELAY_SECONDS_MAX {
            return Err(Error::config(
                CONFIGURATION_INVALID,
                RETRY_DELAYS_KEY,
                Some(Box::from("invalid retry delay")),
            ));
        }
        out.push(time::Duration::seconds(delay));
        previous = delay;
    }
    Ok(out)
}

/// Reads `session.branch_prefix`, which starts every reviewer branch name.
///
/// # Errors
///
/// Returns `configuration_invalid` for text that cannot be part of a git ref name: whitespace,
/// a control character, `..`, `@{`, one of `~^:?*[\`, or a leading `-` or `/`.
fn branch_prefix(value: &Option<Value>) -> Result<String, Error> {
    let key = "session.branch_prefix";
    let prefix = text(value, key)?;
    let refused = prefix.starts_with(['-', '/'])
        || prefix.contains("..")
        || prefix.contains("@{")
        || prefix
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || "~^:?*[\\".contains(c));
    if refused {
        return Err(invalid(key, "must be usable in a git ref name"));
    }
    Ok(prefix)
}

/// Reads `session.name_prefix`, which starts every review session name.
///
/// # Errors
///
/// Returns `configuration_invalid` for text with a control character.
fn name_prefix(value: &Option<Value>) -> Result<String, Error> {
    let key = "session.name_prefix";
    let prefix = text(value, key)?;
    if prefix.chars().any(char::is_control) {
        return Err(invalid(key, "must not contain a control character"));
    }
    Ok(prefix)
}

/// Builds a `configuration_invalid` error that names `key`.
fn invalid(key: &str, message: &'static str) -> Error {
    Error::config(CONFIGURATION_INVALID, key, Some(Box::from(message)))
}

/// Builds a `configuration_missing` error that names `key`.
fn missing(key: &str) -> Error {
    Error::config(CONFIGURATION_MISSING, key, None)
}

/// Returns the value of `key`, which must be present and not blank. Go's `required` treats
/// whitespace-only text as missing, and so does this.
fn present<'a>(value: &'a Option<Value>, key: &str) -> Result<&'a Value, Error> {
    match value {
        Some(Value::Text(text)) if text.trim().is_empty() => Err(missing(key)),
        Some(value) => Ok(value),
        None => Err(missing(key)),
    }
}

/// Returns the section `key`, which must be present.
fn section<'a, T>(value: &'a Option<T>, key: &str) -> Result<&'a T, Error> {
    match value {
        Some(section) => Ok(section),
        None => Err(missing(key)),
    }
}

/// Reads an integer.
fn integer(value: &Value, key: &str) -> Result<i64, Error> {
    match value {
        Value::Int(number) => Ok(*number),
        _ => Err(invalid(key, "must be an integer")),
    }
}

/// Reads a positive integer, as Go's `positive` does.
fn positive(value: &Option<Value>, key: &str) -> Result<i64, Error> {
    let number = integer(present(value, key)?, key)?;
    if number <= 0 {
        return Err(invalid(key, "must be a positive integer"));
    }
    Ok(number)
}

/// Reads a positive number of seconds.
fn seconds(value: &Option<Value>, key: &str) -> Result<Duration, Error> {
    let number = positive(value, key)?;
    let unsigned = u64::try_from(number).map_err(|_| invalid(key, "must be a positive integer"))?;
    Ok(Duration::from_secs(unsigned))
}

/// Reads a positive number of milliseconds.
fn milliseconds(value: &Option<Value>, key: &str) -> Result<Duration, Error> {
    let number = positive(value, key)?;
    let unsigned = u64::try_from(number).map_err(|_| invalid(key, "must be a positive integer"))?;
    Ok(Duration::from_millis(unsigned))
}

/// Reads text.
fn text(value: &Option<Value>, key: &str) -> Result<String, Error> {
    match present(value, key)? {
        Value::Text(text) => Ok(text.clone()),
        _ => Err(invalid(key, "must be text")),
    }
}

/// Reads a list of text.
fn text_list(value: &Option<Value>, key: &str) -> Result<Vec<String>, Error> {
    match present(value, key)? {
        Value::List(items) => {
            let mut texts = Vec::new();
            for item in items {
                match item {
                    Value::Text(text) => texts.push(text.clone()),
                    _ => return Err(invalid(key, "must be a list of text")),
                }
            }
            Ok(texts)
        }
        _ => Err(invalid(key, "must be a list")),
    }
}

/// Returns `configuration_invalid` naming the key whose value breaks a rule.
fn cross_key_rules(file: &FileConfig, config: &Config) -> Result<(), Error> {
    if !clean_absolute(config.state_db_path.as_str()) {
        return Err(invalid("store.path", "must be a clean absolute path"));
    }
    if config.review_retention < Duration::from_secs(SECONDS_PER_DAY) {
        return Err(invalid(
            "store.review_retention_seconds",
            "must be at least one day",
        ));
    }
    if let Some(policy) = config.policy.as_ref() {
        let retention = time::Duration::try_from(config.published_retention)
            .map_err(|_| invalid("store.published_retention_seconds", "is out of range"))?;
        if retention <= policy.handoff_reminder() {
            return Err(invalid(
                "store.published_retention_seconds",
                "must exceed notes_handoff.reminder_hours of the policy",
            ));
        }
    }
    if config.discovery_lock_timeout > Duration::from_secs(DISCOVERY_LOCK_TIMEOUT_MAX_SECONDS) {
        return Err(invalid(
            "schedule.discovery_lock_timeout_seconds",
            "must be at most one hour",
        ));
    }
    if config.output_max_bytes > OUTPUT_MAX_BYTES_LIMIT {
        return Err(invalid(
            "session.output_max_bytes",
            "must be at most 16 MiB",
        ));
    }
    if config.output_chunk_bytes > config.output_max_bytes {
        return Err(invalid(
            "session.output_chunk_bytes",
            "must not exceed session.output_max_bytes",
        ));
    }
    if config.result_max_bytes > config.output_max_bytes {
        return Err(invalid(
            "limits.result_max_bytes",
            "must not exceed session.output_max_bytes",
        ));
    }
    if config.output_wait_ms > OUTPUT_WAIT_MS_MAX {
        return Err(invalid("session.output_wait_ms", "must be at most 8000"));
    }
    if config.min_summary_characters > MIN_SUMMARY_CHARACTERS_MAX {
        return Err(invalid(
            "limits.min_summary_characters",
            "must be at most 500",
        ));
    }
    if config.min_summary_characters > config.max_summary_characters {
        return Err(invalid(
            "limits.min_summary_characters",
            "must not exceed limits.max_summary_characters",
        ));
    }
    if config.excerpt_characters > config.matrix_max_characters / EXCERPT_DIVISOR {
        return Err(invalid(
            "limits.excerpt_characters",
            "must be at most a quarter of notify.max_characters",
        ));
    }
    if config.github_body_max_bytes > GITHUB_BODY_MAX_BYTES_LIMIT {
        return Err(invalid(
            "publish.github_body_max_bytes",
            "must be at most 65536",
        ));
    }
    let publish = section(&file.publish, "publish")?;
    let signature = text(&publish.signature, "publish.signature")?;
    let pattern = text(&publish.ai_signed_pattern, "publish.ai_signed_pattern")?;
    if !signature_valid(signature.as_str(), pattern.as_str()) {
        return Err(invalid(
            "publish.signature",
            "must be one line that starts with publish.ai_signed_pattern and mentions no one",
        ));
    }
    Ok(())
}

/// Reports whether `path` is absolute, has no empty, `.` or `..` part, and has no query or
/// fragment.
pub(crate) fn clean_absolute(path: &str) -> bool {
    if path.contains('?') || path.contains('#') {
        return false;
    }
    let Some(rest) = path.strip_prefix('/') else {
        return false;
    };
    if rest.is_empty() {
        return false;
    }
    for part in rest.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return false;
        }
    }
    true
}

/// Reports whether a signature is one line that starts with `pattern` and mentions no one or
/// embeds
/// no HTML.
fn signature_valid(signature: &str, pattern: &str) -> bool {
    if !signature.starts_with(pattern) {
        return false;
    }
    for cell in signature.chars() {
        if cell.is_control() || cell == '@' || cell == '<' || cell == '>' {
            return false;
        }
    }
    true
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use autoreview_engine::config::NotifyRoute;
    use autoreview_engine::go_json;
    use autoreview_policy::Level;

    /// The example config, read by the loader tests so that they cover the shipped file.
    const EXAMPLE: &str = include_str!("../../../examples/config.yaml");

    /// Wraps one YAML scalar or flow sequence under a key, so that tests read values as the loader
    /// does.
    #[derive(Deserialize)]
    struct Wrapped {
        /// The value under test.
        v: Option<Value>,
    }

    /// Reads `text` as the value of a key.
    fn scalar(text: &str) -> Option<Value> {
        serde_saphyr::from_str::<Wrapped>(&format!("v: {text}"))
            .expect("test scalar is valid YAML")
            .v
    }

    /// Secret lookup for the tests; only the token variables they use are set.
    pub(crate) fn secret(name: &str) -> Option<String> {
        match name {
            "AUTOREVIEW_GH_TOKEN" => Some("synthetic-token".to_owned()),
            "AUTOREVIEW_MATRIX_TOKEN" | "AUTOREVIEW_MATRIX_TOKEN_OTHER" => {
                Some("synthetic-matrix-token".to_owned())
            }
            "AUTOREVIEW_WEBHOOK_URL" => Some("https://hooks.example.org/synthetic".to_owned()),
            _ => None,
        }
    }

    /// Parses a policy document, reading its prompts from `testdata`, in place of the file named
    /// by
    /// `policy_file`. The document is the engine's test policy, which the policy crate accepts.
    fn policy_from(document: &str) -> Result<Policy, Box<dyn std::error::Error + Send + Sync>> {
        autoreview_policy::parse(document.as_bytes(), |relative| {
            std::fs::read(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../testdata")
                    .join(relative),
            )
        })
        .map_err(|error| Box::new(error) as _)
    }

    /// Loads the engine's test policy for every test that does not change it.
    fn example_policy(_: &Path) -> Result<Policy, Box<dyn std::error::Error + Send + Sync>> {
        policy_from(&deployed_policy_document())
    }

    /// Returns `testdata/review-policy.yaml` with `trusted: any` on every rule.
    ///
    /// @TODO remove after #111 and write the sections into the fixture.
    fn deployed_policy_document() -> String {
        const FLOW_RULE_END: &str = "deleted_tests: [\"**/tests/**\"]}}";
        const BLOCK_RULE_LINE: &str = "    on_clean: approve\n";
        let document = include_str!("../../../testdata/review-policy.yaml");
        assert!(
            document.contains(FLOW_RULE_END) && document.contains(BLOCK_RULE_LINE),
            "the policy fixture changed; update the authors injection"
        );
        document
            .replacen(
                FLOW_RULE_END,
                "deleted_tests: [\"**/tests/**\"]}, authors: {trusted: any}}",
                1,
            )
            .replace(
                BLOCK_RULE_LINE,
                "    on_clean: approve\n    authors:\n      trusted: any\n",
            )
    }

    /// Maps `text` through the same path as [`parse`], without the engine's final validation.
    fn mapped(text: &str) -> Result<Config, Error> {
        to_config(&parse_file(text)?, &secret, example_policy)
    }

    /// Returns the text of `result` when it failed.
    fn error_text(result: Result<Config, Error>) -> String {
        result
            .expect_err("the loader refuses this config")
            .to_string()
    }

    /// Returns the example with `old` replaced by `new`; `old` must occur in the example.
    fn mutated(old: &str, new: &str) -> String {
        assert!(EXAMPLE.contains(old), "example lacks {old:?}");
        EXAMPLE.replacen(old, new, 1)
    }

    /// Lists the dotted path of every key of the example that is not inside a list.
    fn example_key_paths() -> Vec<(String, usize, usize)> {
        let lines: Vec<&str> = EXAMPLE.lines().collect();
        let mut stack: Vec<(usize, String)> = Vec::new();
        let mut paths = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            let indent = line.len() - line.trim_start().len();
            let trimmed = line.trim_start();
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("- ") {
                continue;
            }
            let Some((key, _)) = trimmed.split_once(':') else {
                continue;
            };
            while stack.last().is_some_and(|(depth, _)| *depth >= indent) {
                stack.pop();
            }
            let mut path: Vec<&str> = stack.iter().map(|(_, name)| name.as_str()).collect();
            path.push(key);
            let joined = path.join(".");
            let grouped = ["github.credentials", "notify.backends"];
            if !grouped.iter().any(|prefix| joined.starts_with(prefix)) {
                paths.push((joined, index, indent));
            }
            stack.push((indent, key.to_owned()));
        }
        paths
    }

    /// Removes the key on `index` with its block: every following line indented deeper.
    fn without_block(index: usize, indent: usize) -> String {
        let lines: Vec<&str> = EXAMPLE.lines().collect();
        let mut end = index + 1;
        while lines.get(end).is_some_and(|line| {
            let width = line.len() - line.trim_start().len();
            line.trim().is_empty() || width > indent
        }) {
            end += 1;
        }
        lines
            .iter()
            .enumerate()
            .filter(|(line, _)| *line < index || *line >= end)
            .map(|(_, line)| *line)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn example_maps_to_the_engine_configuration() {
        let config = mapped(EXAMPLE).expect("the example maps");
        assert_eq!(config.session_group, "autoreview");
        assert_eq!(config.session_name_prefix, "github-review-");
        assert_eq!(config.session_branch_prefix, "autoreview/review/");
        assert_eq!(config.github_login, "review-bot");
        assert_eq!(config.github_token, "synthetic-token");
        assert_eq!(config.state_db_path, "/var/lib/autoreview/autoreview.db");
    }

    #[test]
    fn example_passes_the_loader_and_the_engine_validation() {
        let config = parse(EXAMPLE, secret, example_policy).expect("the example loads");
        assert_eq!(config.matrix_rooms.action, "!roomid:example.org");
        assert_eq!(config.matrix_rooms.log, "!roomid:example.org");
        assert_eq!(config.queue_matrix_room, "!roomid:example.org");
        assert_eq!(config.matrix_homeserver_url, "https://matrix.example.org");
        assert_eq!(config.matrix_access_token, "synthetic-matrix-token");
        assert_eq!(config.matrix_mention_user, "@you:example.org");
        let file = parse_file(EXAMPLE).expect("the example parses");
        assert!(
            mcp_section(&file).is_ok_and(|mcp| mcp.is_none()),
            "the example keeps the MCP server off"
        );
        assert!(
            metrics_section(&file).is_ok_and(|metrics| metrics.is_none()),
            "the example keeps the metrics endpoint off"
        );
        // An Enterprise Managed User login is `handle_shortcode`.
        for login in ["mona-cat_octo", "octo_admin"] {
            let text = mutated(
                "      login: review-bot\n",
                &format!("      login: {login}\n"),
            );
            let config = parse(&text, secret, example_policy)
                .unwrap_or_else(|error| panic!("{login}: {error}"));
            assert_eq!(config.github_login, login);
        }
    }

    /// Reads a prompt of the example in `directory` as its README installs it.
    ///
    /// A prompt comes from the example's own `prompts/`, or is one of the two default prompts
    /// of the repository's `prompts/`, which every README copies.
    fn example_prompt(directory: &Path, relative: &str) -> std::io::Result<Vec<u8>> {
        let own = directory.join(relative);
        if own.is_file() {
            return std::fs::read(own);
        }
        if relative == "prompts/generic.md" || relative == "prompts/verifier.md" {
            return std::fs::read(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../..")
                    .join(relative),
            );
        }
        Err(std::io::Error::from(std::io::ErrorKind::NotFound))
    }

    #[test]
    fn every_example_policy_validates_with_the_example_config() {
        let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
        let mut directories: Vec<PathBuf> = std::fs::read_dir(&examples)
            .expect("the examples directory is readable")
            .map(|entry| entry.expect("an examples entry").path())
            .filter(|path| path.join("policy.yaml").is_file())
            .collect();
        directories.sort();
        assert!(
            directories.len() >= 8,
            "found only {} example policies",
            directories.len()
        );
        for directory in &directories {
            let document =
                std::fs::read(directory.join("policy.yaml")).expect("the policy is readable");
            let loaded = parse(EXAMPLE, secret, |path| {
                assert_eq!(path, Path::new("/etc/autoreview/policy.yaml"));
                autoreview_policy::parse(&document, |relative| example_prompt(directory, relative))
                    .map_err(|error| Box::new(error) as _)
            });
            if let Err(error) = loaded {
                panic!("{}: {error}", directory.display());
            }
        }
    }

    /// The notify backends, routes and queue backends of the example.
    const EXAMPLE_NOTIFY: &str = concat!(
        "  backends:\n",
        "    - name: matrix\n",
        "      type: matrix\n",
        "      homeserver_url: https://matrix.example.org\n",
        "      access_token_env: AUTOREVIEW_MATRIX_TOKEN\n",
        "      room: \"!roomid:example.org\"\n",
        "      mention_user: \"@you:example.org\"\n",
        "  routes:\n",
        "    action: [matrix]\n",
        "    digest: [matrix]\n",
        "    log: [matrix]\n",
        "  review_queue:\n",
        "    backends: [matrix]\n",
    );

    /// Returns one `matrix` entry of `notify.backends`.
    fn matrix_backend(name: &str, homeserver: &str, token_env: &str, mention: &str) -> String {
        format!(
            concat!(
                "    - name: {name}\n",
                "      type: matrix\n",
                "      homeserver_url: {homeserver}\n",
                "      access_token_env: {token_env}\n",
                "      room: \"!{name}:example.org\"\n",
                "      mention_user: \"{mention}\"\n",
            ),
            name = name,
            homeserver = homeserver,
            token_env = token_env,
            mention = mention,
        )
    }

    /// Returns the example routed to the backends `action`, `digest`, `log` and `queue`.
    fn four_backends(entries: [String; 4], queue_names: &str) -> String {
        let [action, digest, log, queue] = entries;
        let notify = format!(
            concat!(
                "  backends:\n{action}{digest}{log}{queue}",
                "  routes:\n",
                "    action: [action]\n",
                "    digest: [digest]\n",
                "    log: [log]\n",
                "  review_queue:\n",
                "    backends: {queue_names}\n",
            ),
            action = action,
            digest = digest,
            log = log,
            queue = queue,
            queue_names = queue_names,
        );
        mutated(EXAMPLE_NOTIFY, &notify)
    }

    /// Returns the four backends that share one identity.
    fn shared_backends() -> [String; 4] {
        ["action", "digest", "log", "queue"].map(|name| {
            matrix_backend(
                name,
                "https://matrix.example.org",
                "AUTOREVIEW_MATRIX_TOKEN",
                "@you:example.org",
            )
        })
    }

    #[test]
    fn four_matrix_backends_route_each_level_to_its_room() {
        let text = four_backends(shared_backends(), "[queue]");
        let config = parse(&text, secret, example_policy)
            .expect("four Matrix backends with one identity pass validation");
        assert_eq!(config.matrix_rooms.action, "!action:example.org");
        assert_eq!(config.matrix_rooms.digest, "!digest:example.org");
        assert_eq!(config.matrix_rooms.log, "!log:example.org");
        assert_eq!(config.queue_matrix_room, "!queue:example.org");
        assert_eq!(config.matrix_homeserver_url, "https://matrix.example.org");
        assert_eq!(config.matrix_access_token, "synthetic-matrix-token");
        assert_eq!(config.matrix_mention_user, "@you:example.org");
    }

    #[test]
    fn secret_variables_name_every_env_key() {
        let mut entries = shared_backends();
        entries[3].push_str(concat!(
            "    - name: hook\n",
            "      type: webhook\n",
            "      url_env: AUTOREVIEW_WEBHOOK_URL\n",
            "      format: json\n",
        ));
        let text = four_backends(entries, "[queue]");
        let config = parse(&text, secret, example_policy).expect("a webhook config loads");
        let names: Vec<&str> = config.secret_variables.iter().map(String::as_str).collect();
        assert_eq!(
            names,
            [
                "AUTOREVIEW_GH_TOKEN",
                "AUTOREVIEW_MATRIX_TOKEN",
                "AUTOREVIEW_WEBHOOK_URL"
            ]
        );
    }

    #[test]
    fn a_queue_routed_only_to_none_is_refused() {
        let mut entries = shared_backends();
        entries[3].push_str("    - name: quiet\n      type: none\n");
        let text = four_backends(entries.clone(), "[quiet]");
        assert_eq!(
            error_text(parse(&text, secret, example_policy)),
            concat!(
                "configuration_invalid: notify.review_queue.backends: ",
                "none is not supported for the review queue"
            )
        );
        let text = four_backends(entries, "[quiet, queue]");
        let config = parse(&text, secret, example_policy).expect("none next to Matrix loads");
        assert_eq!(config.queue_matrix_room, "!queue:example.org");
        let text = four_backends(shared_backends(), "[queue]")
            .replace("room: \"!queue:example.org\"", "room: \"\"");
        assert_eq!(
            error_text(parse(&text, secret, example_policy)),
            "configuration_missing: notify.backends.room"
        );
    }

    #[test]
    fn a_queue_without_backends_is_refused() {
        let text = four_backends(shared_backends(), "[]");
        assert_eq!(
            error_text(parse(&text, secret, example_policy)),
            "configuration_invalid: notify.review_queue.backends: must name at least one backend"
        );
    }

    #[test]
    fn a_level_routed_to_none_is_refused() {
        for (index, level) in ["action", "digest", "log"].into_iter().enumerate() {
            let mut entries = shared_backends();
            if let Some(entry) = entries.get_mut(index) {
                *entry = format!("    - name: {level}\n      type: none\n");
            }
            let text = four_backends(entries, "[queue]");
            assert_eq!(
                error_text(parse(&text, secret, example_policy)),
                format!("configuration_invalid: config: invalid Matrix {level} room"),
                "{level} routed to none"
            );
        }
    }

    #[test]
    fn a_matrix_backend_with_another_identity_names_its_route_key() {
        let shared = concat!(
            "Matrix backends must share homeserver_url, access_token_env and ",
            "mention_user until M3"
        );
        let cases = [
            (
                0,
                "https://other.example.org",
                "AUTOREVIEW_MATRIX_TOKEN",
                "@you:example.org",
                "notify.routes.digest",
            ),
            (
                1,
                "https://other.example.org",
                "AUTOREVIEW_MATRIX_TOKEN",
                "@you:example.org",
                "notify.routes.digest",
            ),
            (
                2,
                "https://matrix.example.org",
                "AUTOREVIEW_MATRIX_TOKEN_OTHER",
                "@you:example.org",
                "notify.routes.log",
            ),
            (
                1,
                "https://matrix.example.org",
                "AUTOREVIEW_MATRIX_TOKEN",
                "@other:example.org",
                "notify.routes.digest",
            ),
            (
                3,
                "https://other.example.org",
                "AUTOREVIEW_MATRIX_TOKEN",
                "@you:example.org",
                "notify.review_queue.backends",
            ),
        ];
        for (index, homeserver, token_env, mention, key) in cases {
            let mut entries = shared_backends();
            let names = ["action", "digest", "log", "queue"];
            if let (Some(entry), Some(name)) = (entries.get_mut(index), names.get(index)) {
                *entry = matrix_backend(name, homeserver, token_env, mention);
            }
            let text = four_backends(entries, "[queue]");
            assert_eq!(
                error_text(mapped(&text)),
                format!("configuration_invalid: {key}: {shared}"),
                "{key} with {homeserver}, {token_env}, {mention}"
            );
        }
        let text = four_backends(shared_backends(), "[queue, log]");
        assert_eq!(
            error_text(mapped(&text)),
            concat!(
                "configuration_invalid: notify.review_queue.backends: ",
                "must name at most one Matrix backend until M3"
            )
        );
    }

    /// Returns the four shared backends plus `approved`, with `route` added to
    /// `notify.routes`.
    fn with_event_route(route: &str) -> String {
        let mut entries = shared_backends();
        entries[3].push_str(&matrix_backend(
            "approved",
            "https://matrix.example.org",
            "AUTOREVIEW_MATRIX_TOKEN",
            "@you:example.org",
        ));
        four_backends(entries, "[queue]").replacen(
            "    log: [log]\n",
            &format!("    log: [log]\n{route}"),
            1,
        )
    }

    #[test]
    fn an_event_route_takes_its_event_and_leaves_the_others_on_their_levels() {
        let config = parse(
            &four_backends(shared_backends(), "[queue]"),
            secret,
            example_policy,
        )
        .expect("level routes alone load");
        assert!(config.matrix_rooms.events.is_empty());
        assert_eq!(
            config.notify_route(&Event::Approved, Level::Digest),
            NotifyRoute::Level(Level::Digest)
        );
        let text = with_event_route("    approved: [approved]\n");
        let config = parse(&text, secret, example_policy).expect("an event route loads");
        assert_eq!(
            config.matrix_rooms.events,
            BTreeMap::from([(Event::Approved, "!approved:example.org".to_owned())])
        );
        assert_eq!(config.matrix_rooms.digest, "!digest:example.org");
        assert_eq!(
            config.notify_route(&Event::Approved, Level::Digest),
            NotifyRoute::Event(Event::Approved)
        );
        assert_eq!(
            config.notify_route(&Event::ChangesRequested, Level::Digest),
            NotifyRoute::Level(Level::Digest)
        );
    }

    #[test]
    fn an_event_route_naming_an_unknown_event_or_backend_names_its_key() {
        let cases = [
            (
                "    aproved: [approved]\n",
                "configuration_invalid: notify.routes.aproved: names no notification event",
            ),
            (
                "    approved: [missing]\n",
                "configuration_invalid: notify.routes.approved: names no configured backend",
            ),
            (
                "    approved: [approved, digest]\n",
                concat!(
                    "configuration_invalid: notify.routes.approved: ",
                    "must name exactly one backend until M3"
                ),
            ),
            (
                "    approved: [quiet]\n",
                "configuration_invalid: config: invalid Matrix approved room",
            ),
        ];
        for (route, expected) in cases {
            let text = with_event_route(route).replacen(
                "  routes:\n",
                "    - name: quiet\n      type: none\n  routes:\n",
                1,
            );
            assert_eq!(
                error_text(parse(&text, secret, example_policy)),
                expected,
                "{route}"
            );
        }
    }

    #[test]
    fn removing_any_key_of_the_example_names_it() {
        let paths = example_key_paths();
        assert!(!paths.is_empty());
        for (path, index, indent) in &paths {
            let text = without_block(*index, *indent);
            // `cli` holds one key, so removing it leaves `cli:` null, which reads as the section.
            let named = if path == "cli.review_list_limit" {
                "cli"
            } else {
                path.as_str()
            };
            let expected = format!("configuration_missing: {named}");
            assert_eq!(error_text(mapped(&text)), expected, "removing {path}");
        }
    }

    #[test]
    fn removing_a_credential_or_backend_key_names_it() {
        let cases = [
            ("      login: review-bot\n", "github.credentials.login"),
            (
                "      token_env: AUTOREVIEW_GH_TOKEN\n",
                "github.credentials.token_env",
            ),
            ("      match: [\"*\"]\n", "github.credentials.match"),
            (
                "      room: \"!roomid:example.org\"\n",
                "notify.backends.room",
            ),
        ];
        for (old, key) in cases {
            let text = mutated(old, "");
            let result = error_text(mapped(&text));
            assert!(result.contains(key), "{result} lacks {key}");
        }
    }

    #[test]
    fn invalid_values_name_their_key() {
        let cases = [
            (
                "  max_characters: 8000",
                "  max_characters: 0",
                "configuration_invalid: notify.max_characters: must be a positive integer",
            ),
            (
                "      homeserver_url: https://matrix.example.org\n",
                "      homeserver_url: https://matrix.example.org:65536\n",
                concat!(
                    "configuration_invalid: notify.backends.homeserver_url: ",
                    "port must be at most 65535"
                ),
            ),
            (
                "version: 1",
                "version: 2",
                "configuration_invalid: version: must be 1",
            ),
            (
                "      login: review-bot\n",
                "      login: \"review-bot[bot]\"\n",
                "configuration_invalid: github.credentials.login: is not a GitHub login",
            ),
            (
                "      login: review-bot\n",
                "      login: mona_cat_octo\n",
                "configuration_invalid: github.credentials.login: is not a GitHub login",
            ),
            (
                "policy_file: /etc/autoreview/policy.yaml",
                "policy_file: policy.yaml",
                "configuration_invalid: policy_file: must be an absolute path",
            ),
            (
                "  credential_check_hours: 24",
                "  credential_check_hours: 0",
                "configuration_invalid: github.credential_check_hours: must be a positive integer",
            ),
            (
                "  expiry_warning_days: 14",
                "  expiry_warning_days: \"14\"",
                "configuration_invalid: github.expiry_warning_days: must be an integer",
            ),
            (
                "  timeout_seconds: 45",
                "  timeout_seconds: 1.5",
                "configuration_invalid: pohunek.timeout_seconds: must be an integer",
            ),
            (
                "  hosts: [review-host]",
                "  hosts: []",
                "configuration_invalid: pohunek.hosts: must list at least one host",
            ),
            (
                "  discovery_lock_poll_ms: 500",
                "  discovery_lock_poll_ms: 0",
                concat!(
                    "configuration_invalid: schedule.discovery_lock_poll_ms: must ",
                    "be a positive integer"
                ),
            ),
            (
                "  poll_limit: 100",
                "  poll_limit: -3",
                "configuration_invalid: schedule.poll_limit: must be a positive integer",
            ),
            (
                "  debounce_seconds: 120",
                "  debounce_seconds: 601",
                concat!(
                    "configuration_invalid: schedule.debounce_seconds: invalid or ",
                    "missing configuration"
                ),
            ),
            (
                "  retry_delays_seconds: [60, 300, 900]",
                "  retry_delays_seconds: [60, 300, 3601]",
                "configuration_invalid: schedule.retry_delays_seconds: invalid retry delay",
            ),
            (
                "  path: /var/lib/autoreview/autoreview.db",
                "  path: /var/lib/autoreview/autoreview.db\n  extra: 1",
                "unknown field `extra`",
            ),
            (
                "  branch_prefix: autoreview/review/",
                "  branch_prefix: autoreview/review..",
                concat!(
                    "configuration_invalid: session.branch_prefix: ",
                    "must be usable in a git ref name"
                ),
            ),
            (
                "  name_prefix: github-review-",
                "  name_prefix: \"github-review-\\t\"",
                concat!(
                    "configuration_invalid: session.name_prefix: ",
                    "must not contain a control character"
                ),
            ),
        ];
        for (old, new, expected) in cases {
            let result = error_text(mapped(&mutated(old, new)));
            assert!(result.contains(expected), "{result} lacks {expected}");
        }
    }

    #[test]
    fn go_load_policy_bounds_are_ported_to_file_keys() {
        let policy = scheduler_policy(&scalar("120"), &scalar("[60, 300, 900]"))
            .expect("the Go test's valid values load");
        assert_eq!(policy.debounce, time::Duration::seconds(120));
        assert_eq!(
            policy.retry_delays,
            vec![
                time::Duration::seconds(60),
                time::Duration::seconds(300),
                time::Duration::seconds(900),
            ]
        );
        for bad in ["\"\"", "[]", "[0]", "[300, 60]", "[3601]"] {
            assert!(
                scheduler_policy(&scalar("120"), &scalar(bad)).is_err(),
                "accepted {bad}"
            );
        }
    }

    #[test]
    fn debounce_and_retry_delays_have_the_go_messages() {
        let cases = [
            ("1", true),
            ("600", true),
            ("0", false),
            ("601", false),
            ("-1", false),
            ("~", false),
            ("\"120\"", false),
            ("120.0", false),
        ];
        for (text, accepted) in cases {
            let result = scheduler_policy(&scalar(text), &scalar("[60]"));
            assert_eq!(result.is_ok(), accepted, "debounce {text}");
        }
        let cases = [
            ("[1, 2, 3, 4, 5]", None),
            (
                "[1, 2, 3, 4, 5, 6]",
                Some("invalid or missing configuration"),
            ),
            ("[]", Some("invalid or missing configuration")),
            ("[60, 60]", Some("invalid retry delay")),
            ("[60, 59]", Some("invalid retry delay")),
            ("[0]", Some("invalid retry delay")),
            ("[null]", Some("invalid retry delay")),
            ("[1.5]", Some("invalid or missing configuration")),
            ("[\"60\"]", Some("invalid or missing configuration")),
            ("[3600]", None),
            ("[3601]", Some("invalid retry delay")),
        ];
        for (text, message) in cases {
            let result = scheduler_policy(&scalar("120"), &scalar(text));
            match message {
                None => assert!(result.is_ok(), "rejected {text}"),
                Some(message) => {
                    let text_of = result.expect_err("the value is refused").to_string();
                    assert!(text_of.ends_with(message), "{text_of} for {text}");
                }
            }
        }
    }

    #[test]
    fn resolve_path_prefers_the_flag() {
        let flag = Path::new("/etc/a.yaml");
        let env = OsStr::new("/etc/b.yaml");
        assert_eq!(
            resolve_path(Some(flag), Some(env)).expect("flag wins"),
            PathBuf::from("/etc/a.yaml")
        );
        assert_eq!(
            resolve_path(None, Some(env)).expect("env used"),
            PathBuf::from("/etc/b.yaml")
        );
        assert_eq!(
            error_text_path(resolve_path(None, None)),
            "configuration_missing: --config: neither --config nor AUTOREVIEW_CONFIG is set"
        );
        assert_eq!(
            error_text_path(resolve_path(None, Some(OsStr::new("")))),
            "configuration_invalid: AUTOREVIEW_CONFIG: must not be empty"
        );
    }

    /// Returns the text of a failed path resolution.
    fn error_text_path(result: Result<PathBuf, Error>) -> String {
        result.expect_err("the path is refused").to_string()
    }

    #[test]
    fn cross_key_rules_name_both_keys() {
        let cases = [
            (
                "  path: /var/lib/autoreview/autoreview.db",
                "  path: /var/lib/../autoreview.db",
                "configuration_invalid: store.path: must be a clean absolute path",
            ),
            (
                "review_retention_seconds: 2592000",
                "review_retention_seconds: 3600",
                "configuration_invalid: store.review_retention_seconds: must be at least one day",
            ),
            (
                "  discovery_lock_timeout_seconds: 300",
                "  discovery_lock_timeout_seconds: 3601",
                concat!(
                    "configuration_invalid: schedule.discovery_lock_timeout_seconds: ",
                    "must be at most one hour"
                ),
            ),
            (
                "  output_max_bytes: 8388608",
                "  output_max_bytes: 16777217",
                "configuration_invalid: session.output_max_bytes: must be at most 16 MiB",
            ),
            (
                "  output_chunk_bytes: 65536",
                "  output_chunk_bytes: 9000000",
                concat!(
                    "configuration_invalid: session.output_chunk_bytes: must not ",
                    "exceed session.output_max_bytes"
                ),
            ),
            (
                "  result_max_bytes: 1048576",
                "  result_max_bytes: 9000000",
                concat!(
                    "configuration_invalid: limits.result_max_bytes: must not exceed ",
                    "session.output_max_bytes"
                ),
            ),
            (
                "  output_wait_ms: 8000",
                "  output_wait_ms: 8001",
                "configuration_invalid: session.output_wait_ms: must be at most 8000",
            ),
            (
                "  min_summary_characters: 40",
                "  min_summary_characters: 501",
                "configuration_invalid: limits.min_summary_characters: must be at most 500",
            ),
            (
                "  max_summary_characters: 1500",
                "  max_summary_characters: 10",
                concat!(
                    "configuration_invalid: limits.min_summary_characters: must not ",
                    "exceed limits.max_summary_characters"
                ),
            ),
            (
                "  excerpt_characters: 360",
                "  excerpt_characters: 2001",
                concat!(
                    "configuration_invalid: limits.excerpt_characters: must be at most a ",
                    "quarter of notify.max_characters"
                ),
            ),
            (
                "  github_body_max_bytes: 60000",
                "  github_body_max_bytes: 70000",
                "configuration_invalid: publish.github_body_max_bytes: must be at most 65536",
            ),
            (
                "  ai_signed_pattern: \"Written by AI:\"",
                "  ai_signed_pattern: \"Generated:\"",
                concat!(
                    "configuration_invalid: publish.signature: must be one line that starts with ",
                    "publish.ai_signed_pattern and mentions no one"
                ),
            ),
        ];
        for (old, new, expected) in cases {
            let result = error_text(mapped(&mutated(old, new)));
            assert_eq!(result, expected, "rule for {old:?}");
        }
    }
    #[test]
    fn published_retention_must_exceed_the_handoff_reminder() {
        let text = mutated(
            "published_retention_seconds: 7776000",
            "published_retention_seconds: 3600",
        );
        let file = parse_file(&text).expect("valid YAML");
        let document = deployed_policy_document().replacen(
            "    on_clean: approve\n",
            "    on_clean: await\n    on_notes: comment\n",
            1,
        ) + "notes_handoff:\n  reminder_hours: 24\n";
        let result = to_config(&file, &secret, |_| policy_from(&document));
        assert_eq!(
            error_text(result),
            concat!(
                "configuration_invalid: store.published_retention_seconds: must exceed ",
                "notes_handoff.reminder_hours of the policy"
            )
        );
    }

    #[test]
    fn mcp_section_is_optional_and_every_key_is_required() {
        let file = parse_file(EXAMPLE).expect("the example parses");
        assert_eq!(mcp_section(&file).expect("no section is valid"), None);
        let client = concat!(
            "    - name: hermes\n",
            "      scope: write\n",
            "      token_sha256: ",
            "\"2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae\"\n",
        );
        let section = format!(
            "mcp:\n  listen: \"127.0.0.1:8787\"\n  allowed_hosts: [mcp.example.com]\n  \
             request_timeout_seconds: 120\n  header_timeout_seconds: 10\n  \
             request_max_bytes: 65536\n  max_connections: 16\n  clients:\n{client}"
        );
        let full = format!("{EXAMPLE}{section}");
        let loaded = mcp_section(&parse_file(&full).expect("valid YAML"))
            .expect("the section is valid")
            .expect("the section is present");
        assert_eq!(loaded.clients.len(), 1);
        let cases = [
            (
                "  listen: \"127.0.0.1:8787\"\n",
                "configuration_missing: mcp.listen",
            ),
            (
                "  allowed_hosts: [mcp.example.com]\n",
                "configuration_missing: mcp.allowed_hosts",
            ),
            (
                "  max_connections: 16\n",
                "configuration_missing: mcp.max_connections",
            ),
            (
                "      scope: write\n",
                "configuration_missing: mcp.clients[0].scope",
            ),
            (
                "  request_max_bytes: 65536\n",
                "configuration_missing: mcp.request_max_bytes",
            ),
        ];
        for (removed, expected) in cases {
            // The example holds a commented copy of the section, so only the section changes.
            let text = format!("{EXAMPLE}{}", section.replacen(removed, "", 1));
            let result = mcp_section(&parse_file(&text).expect("valid YAML"));
            assert_eq!(
                result.expect_err("a key is missing").to_string(),
                expected,
                "without {removed:?}"
            );
        }
    }

    #[test]
    fn metrics_section_is_optional_and_every_key_is_required() {
        let section = concat!(
            "metrics:\n  listen: \"127.0.0.1:9464\"\n  allowed_hosts: [metrics.example.com]\n",
            "  request_timeout_seconds: 10\n  header_timeout_seconds: 10\n  max_connections: 4\n",
        );
        let full = format!("{EXAMPLE}{section}");
        let loaded = metrics_section(&parse_file(&full).expect("valid YAML"))
            .expect("the section is valid")
            .expect("the section is present");
        assert_eq!(loaded.allowed_hosts, ["metrics.example.com"]);
        let host = "configuration_invalid: metrics.allowed_hosts: must list hosts or host:port, \
                    without scheme, path or user";
        let cases = [
            (
                section,
                "metrics:\n",
                "configuration_missing: metrics.listen",
            ),
            (
                "  listen: \"127.0.0.1:9464\"\n",
                "",
                "configuration_missing: metrics.listen",
            ),
            (
                "  allowed_hosts: [metrics.example.com]\n",
                "",
                "configuration_missing: metrics.allowed_hosts",
            ),
            (
                "  request_timeout_seconds: 10\n",
                "",
                "configuration_missing: metrics.request_timeout_seconds",
            ),
            (
                "  header_timeout_seconds: 10\n",
                "",
                "configuration_missing: metrics.header_timeout_seconds",
            ),
            (
                "  max_connections: 4\n",
                "",
                "configuration_missing: metrics.max_connections",
            ),
            (
                "[metrics.example.com]",
                "[\"https://metrics.example.com\"]",
                host,
            ),
            ("[metrics.example.com]", "[]", host),
            (
                "\"127.0.0.1:9464\"",
                "\"localhost:9464\"",
                "configuration_invalid: metrics.listen: must be an IP address and port",
            ),
            (
                "request_timeout_seconds: 10",
                "request_timeout_seconds: 86401",
                concat!(
                    "configuration_invalid: metrics.request_timeout_seconds: ",
                    "must be from 1 to 86400 seconds"
                ),
            ),
            (
                "header_timeout_seconds: 10",
                "header_timeout_seconds: 86401",
                concat!(
                    "configuration_invalid: metrics.header_timeout_seconds: ",
                    "must be from 1 to 86400 seconds"
                ),
            ),
            (
                "max_connections: 4",
                "max_connections: 4097",
                "configuration_invalid: metrics.max_connections: must be from 1 to 4096",
            ),
        ];
        for (old, new, expected) in cases {
            // The example holds a commented copy of the section, so only the section changes.
            let text = format!("{EXAMPLE}{}", section.replacen(old, new, 1));
            let result = metrics_section(&parse_file(&text).expect("valid YAML"));
            assert_eq!(
                result.expect_err("the section is refused").to_string(),
                expected,
                "{old:?} -> {new:?}"
            );
        }
        let unknown = format!("{full}  bearer_token_env: AUTOREVIEW_METRICS_TOKEN\n");
        assert!(
            parse_file(&unknown).is_err(),
            "the section refuses a key it does not know"
        );
    }

    #[test]
    fn host_state_section_is_optional_and_every_key_is_required() {
        let config = parse(EXAMPLE, secret, example_policy).expect("the example loads");
        assert_eq!(config.host_state, None, "the example reads no host state");
        let anchor = "  route_overrides: {}\n";
        let section = concat!(
            "  host_state:\n",
            "    max_age_seconds: 120\n",
            "    read_timeout_seconds: 20\n",
            "    unknown_state: skip\n",
            "    order: idle_first\n",
            "    on_battery: eligible\n",
            "    min_battery_percent: 30\n",
            "    min_free_memory_mb: 0\n",
            "    required_models: [model-a]\n",
            "    outside_window: skip\n",
        );
        let with = |section: &str| mutated(anchor, &format!("{anchor}{section}"));
        let config = parse(&with(section), secret, example_policy).expect("the section loads");
        let rules = HostStateRules {
            max_age: Duration::from_secs(120),
            read_timeout: Duration::from_secs(20),
            unknown_state: HostStateChoice::Skip,
            order: HostOrder::IdleFirst,
            on_battery: HostStateChoice::Eligible,
            min_battery_percent: 30,
            min_free_memory_mb: 0,
            required_models: vec!["model-a".to_owned()],
            outside_window: HostStateChoice::Skip,
        };
        assert_eq!(config.host_state, Some(rules));
        let missing = "configuration_missing: pohunek.host_state.";
        let invalid = "configuration_invalid: pohunek.host_state.";
        // (old, new, expected error after the key prefix)
        let cases = [
            (section, "  host_state:\n", "max_age_seconds"),
            ("    max_age_seconds: 120\n", "", "max_age_seconds"),
            ("    read_timeout_seconds: 20\n", "", "read_timeout_seconds"),
            ("    unknown_state: skip\n", "", "unknown_state"),
            ("    order: idle_first\n", "", "order"),
            ("    on_battery: eligible\n", "", "on_battery"),
            ("    min_battery_percent: 30\n", "", "min_battery_percent"),
            ("    min_free_memory_mb: 0\n", "", "min_free_memory_mb"),
            ("    required_models: [model-a]\n", "", "required_models"),
            ("    outside_window: skip\n", "", "outside_window"),
            (
                "min_battery_percent: 30",
                "min_battery_percent: null",
                "min_battery_percent",
            ),
        ];
        for (old, new, key) in cases {
            let text = with(&section.replacen(old, new, 1));
            let expected = format!("{missing}{key}");
            assert_eq!(error_text(parse(&text, secret, example_policy)), expected);
        }
        let cases = [
            (
                "max_age_seconds: 120",
                "max_age_seconds: 0",
                "max_age_seconds: must be a positive integer",
            ),
            (
                "read_timeout_seconds: 20",
                "read_timeout_seconds: 0",
                "read_timeout_seconds: must be a positive integer",
            ),
            (
                "read_timeout_seconds: 20",
                "read_timeout_seconds: 61",
                "read_timeout_seconds: must be at most 60",
            ),
            // The example's `pohunek.timeout_seconds` is 45; the CLI waits 1 s longer.
            (
                "read_timeout_seconds: 20",
                "read_timeout_seconds: 44",
                "read_timeout_seconds: must be at most pohunek.timeout_seconds - 2",
            ),
            (
                "unknown_state: skip",
                "unknown_state: ignore",
                "unknown_state: must be skip or eligible",
            ),
            (
                "order: idle_first",
                "order: random",
                "order: must be idle_first or configured",
            ),
            (
                "on_battery: eligible",
                "on_battery: true",
                "on_battery: must be text",
            ),
            (
                "min_battery_percent: 30",
                "min_battery_percent: 101",
                "min_battery_percent: must be from 0 to 100",
            ),
            (
                "min_free_memory_mb: 0",
                "min_free_memory_mb: -1",
                "min_free_memory_mb: must not be negative",
            ),
            (
                "required_models: [model-a]",
                "required_models: [\" \"]",
                "required_models: must list model names",
            ),
            (
                "outside_window: skip",
                "outside_window: allow",
                "outside_window: must be skip or eligible",
            ),
        ];
        for (old, new, expected) in cases {
            let text = with(&section.replacen(old, new, 1));
            let expected = format!("{invalid}{expected}");
            assert_eq!(error_text(parse(&text, secret, example_policy)), expected);
        }
        // 43 s plus the CLI's 1 s wait still ends before the 45 s call timeout.
        let longest = section.replacen("read_timeout_seconds: 20", "read_timeout_seconds: 43", 1);
        let config = parse(&with(&longest), secret, example_policy).expect("43 loads");
        let read_timeout = config.host_state.map(|rules| rules.read_timeout);
        assert_eq!(read_timeout, Some(Duration::from_secs(43)));
        let unknown = with(&format!("{section}    prefer_idle: true\n"));
        assert!(
            parse(&unknown, secret, example_policy).is_err(),
            "the section refuses a key it does not know"
        );
    }

    /// A synthetic secret value that no error text or debug output may contain.
    const MARKER: &str = "synthetic-marker-6d1e";

    /// What the env file test puts at the path of the file.
    enum EnvSetup {
        /// A regular file with this mode.
        Mode(u32),
        /// A regular file of mode 0600, read as another user.
        Foreign,
        /// A symlink to a regular file with this mode.
        Link(u32),
        /// A directory.
        Directory,
        /// A FIFO of mode 0600 without a writer.
        Fifo,
        /// Nothing.
        Missing,
    }

    /// How long the env file test waits for one read of the file.
    const READ_BOUND: Duration = Duration::from_secs(10);

    /// Reads the env file at `path` as `euid` on a thread; panics after [`READ_BOUND`].
    ///
    /// An open of a FIFO without `O_NONBLOCK` waits for a writer; the test then fails, not hangs.
    fn read_bounded(path: &Path, euid: u32) -> Result<EnvFile, Error> {
        let (sender, receiver) = std::sync::mpsc::channel();
        let opened = path.to_owned();
        let reader =
            std::thread::spawn(move || sender.send(crate::env_file::read_as(&opened, euid)));
        if let Ok(result) = receiver.recv_timeout(READ_BOUND) {
            return result;
        }
        // A writer ends the wait of a blocked open; closing it before the join gives a read EOF,
        // so the thread ends before the panic.
        let flags = rustix::fs::OFlags::WRONLY | rustix::fs::OFlags::NONBLOCK;
        let writer = rustix::fs::open(path, flags, rustix::fs::Mode::empty());
        if let Ok(writer) = writer {
            drop(writer);
            let _ = reader.join();
        }
        panic!("reading {} took longer than {READ_BOUND:?}", path.display());
    }

    /// D1-D3 of #6: the env file checks, the precedence of its values and that no error, log
    /// text or debug output shows a value (`{m}` is the marker, `{path}` the file's path).
    /// #351 and #353: the `*_env` name rule and a FIFO, which must fail and never block.
    #[test]
    fn env_file_values_win_and_never_appear_in_errors() {
        use EnvSetup::{Directory, Fifo, Foreign, Link, Missing, Mode};
        use std::os::unix::fs::{PermissionsExt, symlink};
        const GH: &str = "AUTOREVIEW_GH_TOKEN={m}\n";
        const TOKEN_ENV: &str = "      token_env: AUTOREVIEW_GH_TOKEN\n";
        const MATRIX_ENV: &str = "      access_token_env: AUTOREVIEW_MATRIX_TOKEN\n";
        const MENTION: &str = "      mention_user: \"@you:example.org\"\n";
        // A webhook backend after the Matrix backend, with a name that breaks the rule.
        const WEBHOOK: &str = concat!(
            "      mention_user: \"@you:example.org\"\n",
            "    - name: hook\n",
            "      type: webhook\n",
            "      url_env: AUTOREVIEW_{m}\n",
            "      format: json\n",
        );
        // Setup, file text, config line replaced ("" for none), the environment's GitHub token,
        // and the token expected (Ok) or a text of the error (Err).
        let row = |setup, text, config, environment, expected| {
            (setup, text, config, environment, expected)
        };
        // A row on the example config without a GitHub token in the environment.
        let file = |setup, text, expected| row(setup, text, ("", ""), None, expected);
        // A row of the GitHub token file on a changed config, without the environment's token.
        let named = |config, expected| row(Mode(0o600), GH, config, None, expected);
        let cases = [
            row(Mode(0o600), GH, ("", ""), Some("environment"), Ok("{m}")),
            file(
                Mode(0o400),
                "# c\n\nAUTOREVIEW_GH_TOKEN={m}=x\r\n",
                Ok("{m}=x"),
            ),
            row(
                Mode(0o600),
                "AUTOREVIEW_A=x\n",
                ("", ""),
                Some("{m}"),
                Ok("{m}"),
            ),
            file(Link(0o600), GH, Ok("{m}")),
            row(
                Mode(0o600),
                "AUTOREVIEW_GH_TOKEN=\n",
                ("", ""),
                Some("{m}"),
                Err("configuration_missing: github.credentials.token_env"),
            ),
            file(Mode(0o640), GH, Err("{path} has mode 0640;")),
            file(Mode(0o604), GH, Err("{path} has mode 0604;")),
            file(Link(0o610), GH, Err("{path} has mode 0610;")),
            file(Foreign, GH, Err("{path} is owned by uid")),
            file(Directory, "", Err("{path} is not a regular file")),
            file(Missing, "", Err("{path} cannot be read")),
            file(Mode(0o600), "{m}\n", Err("{path} line 1 has no '='")),
            file(Mode(0o600), "\n{m}=x\n", Err("{path} line 2 names no")),
            file(
                Mode(0o600),
                "export AUTOREVIEW_A={m}\n",
                Err("{path} line 1 names"),
            ),
            file(Mode(0o600), "{GH}{GH}", Err("{path} line 2 repeats")),
            file(
                Mode(0o600),
                "AUTOREVIEW_GH_TOKEN=a {m}\n",
                Err("configuration_invalid: config: invalid GitHub configuration"),
            ),
            row(
                Mode(0o600),
                GH,
                ("version: 1", "version: 2"),
                None,
                Err("configuration_invalid: version: must be 1"),
            ),
            file(Fifo, "", Err("{path} is not a regular file")),
            named(
                (TOKEN_ENV, "      token_env: {m}\n"),
                Err("github.credentials.token_env: must match AUTOREVIEW_[A-Z0-9_]+"),
            ),
            named(
                (TOKEN_ENV, "      token_env: AUTOREVIEW_\n"),
                Err("github.credentials.token_env: must match AUTOREVIEW_[A-Z0-9_]+"),
            ),
            named(
                (TOKEN_ENV, "      token_env: AUTOREVIEW_{m}\n"),
                Err("github.credentials.token_env: must match AUTOREVIEW_[A-Z0-9_]+"),
            ),
            named(
                (TOKEN_ENV, "      token_env: \"AUTOREVIEW_GH_TOKEN \"\n"),
                Err("github.credentials.token_env: must match AUTOREVIEW_[A-Z0-9_]+"),
            ),
            row(
                Mode(0o600),
                "AUTOREVIEW_GH_2={m}\n",
                (TOKEN_ENV, "      token_env: AUTOREVIEW_GH_2\n"),
                None,
                Ok("{m}"),
            ),
            named(
                (MATRIX_ENV, "      access_token_env: GITHUB_TOKEN\n"),
                Err("notify.backends.access_token_env: must match AUTOREVIEW_[A-Z0-9_]+"),
            ),
            named(
                (
                    MATRIX_ENV,
                    "      access_token_env: AUTOREVIEW_MATRIX_TOKÉN\n",
                ),
                Err("notify.backends.access_token_env: must match AUTOREVIEW_[A-Z0-9_]+"),
            ),
            named(
                (MENTION, WEBHOOK),
                Err("notify.backends.url_env: must match AUTOREVIEW_[A-Z0-9_]+"),
            ),
        ];
        let euid = rustix::process::geteuid().as_raw();
        for (index, (setup, text, (old, new), environment, expected)) in
            cases.into_iter().enumerate()
        {
            let directory = tempfile::tempdir().expect("temporary directory");
            let path = directory.path().join("secrets.env");
            let target = directory.path().join("target.env");
            let text = text.replace("{GH}", GH).replace("{m}", MARKER);
            let mut reader = euid;
            match setup {
                Mode(mode) | Link(mode) => {
                    let file = if matches!(setup, Link(_)) {
                        &target
                    } else {
                        &path
                    };
                    std::fs::write(file, &text).expect("env file written");
                    let permissions = std::fs::Permissions::from_mode(mode);
                    std::fs::set_permissions(file, permissions).expect("mode set");
                    if matches!(setup, Link(_)) {
                        symlink(&target, &path).expect("symlink created");
                    }
                }
                Foreign => {
                    std::fs::write(&path, &text).expect("env file written");
                    let permissions = std::fs::Permissions::from_mode(0o600);
                    std::fs::set_permissions(&path, permissions).expect("mode set");
                    reader = euid.wrapping_add(1);
                }
                Directory => std::fs::create_dir(&path).expect("directory created"),
                Fifo => {
                    let mode = rustix::fs::Mode::from_raw_mode(0o600);
                    let fifo = rustix::fs::FileType::Fifo;
                    rustix::fs::mknodat(rustix::fs::CWD, &path, fifo, mode, 0)
                        .expect("FIFO created");
                }
                Missing => {}
            }
            let config = if old.is_empty() {
                EXAMPLE.to_owned()
            } else {
                mutated(old, &new.replace("{m}", MARKER))
            };
            let environment = environment.map(|value| value.replace("{m}", MARKER));
            let lookup = |name: &str| match name {
                "AUTOREVIEW_GH_TOKEN" => environment.clone(),
                other => secret(other),
            };
            let result = read_bounded(&path, reader).and_then(|file| {
                let found = |name: &str| crate::env_file::secret(Some(&file), name, lookup);
                let config = parse(&config, found, example_policy)?;
                Ok((format!("{file:?}"), config))
            });
            let shown = path.display().to_string();
            match (result, expected) {
                (Ok((file, config)), Ok(token)) => {
                    assert_eq!(
                        config.github_token,
                        token.replace("{m}", MARKER),
                        "row {index}"
                    );
                    for output in [file, format!("{config:?}")] {
                        assert!(!output.contains(MARKER), "row {index}: {output}");
                    }
                }
                (Err(error), Err(part)) => {
                    let part = part.replace("{path}", &shown);
                    let json = go_json(&crate::error_response::error_response(&error))
                        .expect("error response encodes");
                    let display = error.to_string();
                    assert!(
                        display.contains(&part),
                        "row {index}: {display} lacks {part}"
                    );
                    if part.contains(&shown) {
                        assert!(json.contains(&part), "row {index}: {json} lacks {part}");
                    }
                    for output in [display, format!("{error:?}"), json] {
                        assert!(!output.contains(MARKER), "row {index}: {output}");
                    }
                }
                (Ok(_), Err(part)) => panic!("row {index} loads; expected {part}"),
                (Err(error), Ok(_)) => panic!("row {index} fails: {error}"),
            }
        }
    }
}
