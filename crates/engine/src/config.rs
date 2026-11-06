//! Engine configuration and its validation.

// Rust guideline compliant 2026-06-26

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::net::Ipv6Addr;
use std::time::Duration;

use autoreview_policy::Policy;
use autoreview_schedule::metadata::is_valid_session_group;
use autoreview_schedule::scheduler;
use time::Duration as SignedDuration;

use crate::error::{BoxError, Error};
use crate::model::{RunLimits, repository_valid};

mod symbols;

/// Code of a configuration that fails validation.
const CONFIGURATION_INVALID: &str = "configuration_invalid";

/// Name of the configuration that a validation error reports.
const CONFIG_NAME: &str = "config";

/// Prefix of every variable that a `*_env` key of the configuration names.
pub const SECRET_ENV_PREFIX: &str = "AUTOREVIEW_";

/// Host that the GitHub login and token belong to.
const GITHUB_HOST: &str = "github.com";

/// Longest GitHub login in bytes.
const GITHUB_LOGIN_MAX_LEN: usize = 39;

/// Longest AI signature in bytes.
const AI_SIGNATURE_MAX_BYTES: usize = 200;

/// Text that every AI signature starts with, after its optional leading symbols.
pub(crate) const AI_SIGNATURE_PREFIX: &str = "Written by AI:";

/// Host of the only plain HTTP Matrix homeserver the engine accepts.
const LOOPBACK_HOST: &str = "127.0.0.1";

/// Largest output that the engine keeps, in bytes.
const OUTPUT_MAX_BYTES_LIMIT: i64 = 16 << 20;

/// Largest output chunk that the pohunek protocol returns in one response, in bytes.
const POHUNEK_OUTPUT_CHUNK_MAX_BYTES: i64 = 783_240;

/// Largest schedule payload, in bytes.
const SCHEDULE_MAX_BYTES_LIMIT: i64 = 64 << 20;

/// Longest wait for pohunek output, in milliseconds.
const OUTPUT_WAIT_MAX_MS: i64 = 8000;

/// Largest minimum summary length, in characters.
const MIN_SUMMARY_CHARACTERS_LIMIT: i64 = 500;

/// Largest Matrix message, in characters.
const MATRIX_MAX_CHARACTERS_LIMIT: i64 = 32_000;

/// Divisor of the Matrix message length that bounds an excerpt.
const EXCERPT_DIVISOR: i64 = 4;

/// Largest GitHub review body, in bytes.
const GITHUB_BODY_MAX_BYTES_LIMIT: i64 = 65_536;

/// Smallest terminal width of a pohunek session, in columns.
const TERMINAL_COLS_MIN: i64 = 120;

/// Largest terminal width of a pohunek session, in columns.
const TERMINAL_COLS_MAX: i64 = 400;

/// Smallest terminal height of a pohunek session, in rows.
const TERMINAL_ROWS_MIN: i64 = 100;

/// Largest terminal height of a pohunek session, in rows.
const TERMINAL_ROWS_MAX: i64 = 250;

/// Largest number of attempts to start a session.
const SESSION_START_ATTEMPTS_MAX: i64 = 5;

/// Largest number of attempts to clean up a session.
const SESSION_CLEANUP_ATTEMPTS_MAX: i64 = 10;

/// Largest number of attempts to run a reviewer.
const REVIEWER_RUN_ATTEMPTS_MAX: i64 = 3;

/// Longest delay between two attempts or retries of one step.
const RETRY_DELAY_MAX: Duration = Duration::from_secs(5 * 60);

/// Longest time that one failing step is retried.
const STEP_RETRY_MAX: Duration = Duration::from_secs(60 * 60);

/// Longest interval between two discovery passes.
const POLL_INTERVAL_MAX: Duration = Duration::from_secs(60 * 60);

/// Longest wait for another discovery pass to release its lock.
const DISCOVERY_LOCK_TIMEOUT_MAX: Duration = Duration::from_secs(60 * 60);

/// Shortest time that a completed review stays in the database.
const REVIEW_RETENTION_MIN: Duration = Duration::from_secs(24 * 60 * 60);

/// Largest number of reviews that run at the same time.
const MAX_CONCURRENT_REVIEWS_MAX: i64 = 32;

/// Largest number of requests that one queue message lists.
const QUEUE_MAX_ITEMS_MAX: i64 = 999;

/// Largest duration that Go's `time.Duration` holds, in nanoseconds.
const DURATION_MAX: Duration = Duration::from_nanos(9_223_372_036_854_775_807);

/// Routing override of one repository to a pohunek host and project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteOverride {
    /// The pohunek host that serves the repository.
    pub host: String,
    /// The pohunek project of the repository.
    pub project_id: String,
}

/// The Matrix rooms that receive the notifications of each level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatrixRooms {
    /// The room of action messages, which mention the user.
    pub action: String,
    /// The room of digest messages.
    pub digest: String,
    /// The room of log messages.
    pub log: String,
}

/// The technical settings of the engine. What is reviewed and how it is published lives in the
/// review policy.
///
/// The struct has no default: every field is set by the loader, which fails on a missing key.
#[derive(Clone)]
pub struct Config {
    /// The parsed review policy, or `None` when the configuration is validated without one.
    pub policy: Option<Policy>,
    /// The debounce and retry timing of review scheduling.
    pub schedule: scheduler::Policy,
    /// The GitHub host; only `github.com` is accepted.
    pub github_host: String,
    /// The GitHub login that publishes the reviews.
    pub github_login: String,
    /// The GitHub token of the engine; it never appears in debug output.
    pub github_token: String,
    /// Absolute path of the GitHub CLI binary.
    pub gh_bin: String,
    /// Absolute path of the pohunek CLI binary.
    pub pohunek_bin: String,
    /// Absolute path of the pohunek state home.
    pub pohunek_state: String,
    /// Absolute path of the pohunek runtime directory.
    pub pohunek_runtime: String,
    /// The pohunek hosts that serve reviews; never empty.
    pub pohunek_hosts: Vec<String>,
    /// Routing overrides keyed by repository.
    pub route_overrides: BTreeMap<String, RouteOverride>,
    /// The Matrix homeserver URL; HTTPS, or plain HTTP on the loopback address.
    pub matrix_homeserver_url: String,
    /// The Matrix access token; it never appears in debug output.
    pub matrix_access_token: String,
    /// Names of the variables that hold the configured secrets.
    ///
    /// No child process inherits them; the `gh` child gets its token as `GH_TOKEN`.
    pub secret_variables: BTreeSet<String>,
    /// The Matrix user that action messages mention.
    pub matrix_mention_user: String,
    /// The Matrix rooms of the three notification levels.
    pub matrix_rooms: MatrixRooms,
    /// The Matrix room that receives the hourly review queue.
    pub queue_matrix_room: String,
    /// The largest number of requests that one queue message lists.
    pub queue_max_items: i64,
    /// The SQLite database of discovery state and reviews; a clean absolute path.
    pub state_db_path: String,
    /// Timeout of GitHub requests.
    pub gh_timeout: Duration,
    /// Timeout of pohunek requests.
    pub pohunek_timeout: Duration,
    /// Timeout of Matrix requests.
    pub matrix_timeout: Duration,
    /// Timeout of one reviewer session.
    pub session_timeout: Duration,
    /// Interval between two polls of reviewer output.
    pub output_poll_interval: Duration,
    /// Delay between two attempts to start a session.
    pub session_start_retry: Duration,
    /// Delay between two attempts to clean up a session.
    pub session_cleanup_retry: Duration,
    /// Delay between two attempts to run a reviewer.
    pub reviewer_run_retry: Duration,
    /// Time that one failing step is retried in total.
    pub step_retry: Duration,
    /// First delay of the step retry backoff.
    pub step_retry_initial: Duration,
    /// Largest delay of the step retry backoff.
    pub step_retry_max_interval: Duration,
    /// Interval between two discovery passes.
    pub poll_interval: Duration,
    /// Interval between two checks for reviews to start, resume or expire.
    pub worker_tick: Duration,
    /// Time that a discovery pass waits for another pass to release its lock.
    pub discovery_lock_timeout: Duration,
    /// Interval between two attempts to take the discovery lock.
    pub discovery_lock_poll: Duration,
    /// Time that a completed review stays in the database.
    pub review_retention: Duration,
    /// Attempts to start a session.
    pub session_start_attempts: i64,
    /// Attempts to clean up a session.
    pub session_cleanup_attempts: i64,
    /// Attempts to run a reviewer.
    pub reviewer_run_attempts: i64,
    /// Number of reviews that run at the same time.
    pub max_concurrent_reviews: i64,
    /// Largest number of reviews that one status command returns.
    pub review_list_limit: i64,
    /// Largest page of one poll request.
    pub poll_limit: i64,
    /// Largest page of one search request.
    pub search_page_limit: i64,
    /// Largest number of entries that one state read returns.
    pub state_entry_limit: i64,
    /// Largest schedule payload, in bytes.
    pub schedule_max_bytes: i64,
    /// Largest output that the engine keeps, in bytes.
    pub output_max_bytes: i64,
    /// Largest output chunk of one pohunek response, in bytes.
    pub output_chunk_bytes: i64,
    /// Longest wait for pohunek output, in milliseconds.
    pub output_wait_ms: i64,
    /// Largest result of one reviewer, in bytes.
    pub result_max_bytes: i64,
    /// Largest number of findings of one review.
    pub max_findings: i64,
    /// Largest number of tests of one review.
    pub max_tests: i64,
    /// Shortest review summary, in characters.
    pub min_summary_characters: i64,
    /// Longest review summary, in characters.
    pub max_summary_characters: i64,
    /// Longest title of a published review, in characters.
    pub max_title_characters: i64,
    /// Longest body of a published review, in characters.
    pub max_body_characters: i64,
    /// Largest discussion that is handed to the reviewers of a review with prior findings.
    pub max_discussion_characters: i64,
    /// Longest file path of a finding, in characters.
    pub max_path_characters: i64,
    /// Longest test name, in characters.
    pub max_test_characters: i64,
    /// Largest GitHub review body, in bytes.
    pub github_body_max_bytes: i64,
    /// Line that ends every published review body and inline comment; it starts with
    /// "Written by AI:" after an optional leading symbol.
    pub ai_signature: String,
    /// Largest Matrix message, in characters.
    pub matrix_max_characters: i64,
    /// Time that the last published decision of a pull request is kept as a baseline.
    pub published_retention: Duration,
    /// Longest excerpt of a review in a Matrix message, in characters.
    pub excerpt_characters: i64,
    /// Width of the pohunek terminal of a review session, in columns.
    pub terminal_cols: i64,
    /// Height of the pohunek terminal of a review session, in rows.
    pub terminal_rows: i64,
    /// The pohunek-companion group that every review session is placed in.
    pub session_group: String,
    /// Prefix of every review session name.
    pub session_name_prefix: String,
    /// Prefix of every reviewer branch on the review host.
    pub session_branch_prefix: String,
}

impl fmt::Debug for Config {
    /// Shows the identifying fields only, so the tokens never reach a log.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("github_host", &self.github_host)
            .field("github_login", &self.github_login)
            .field("pohunek_hosts", &self.pohunek_hosts)
            .field("state_db_path", &self.state_db_path)
            .field("session_group", &self.session_group)
            .finish_non_exhaustive()
    }
}

impl Config {
    /// Validates the configuration.
    ///
    /// The first failing check returns its error.
    ///
    /// # Errors
    ///
    /// Returns a configuration error when a field is invalid.
    pub fn validate(&self) -> Result<(), Error> {
        if self.github_host != GITHUB_HOST
            || !github_login_valid(&self.github_login)
            || self.github_token.contains('\r')
            || self.github_token.contains('\n')
            || self.github_token.contains(' ')
        {
            return Err(invalid("invalid GitHub configuration"));
        }
        for (label, value) in [
            ("GitHub CLI", self.gh_bin.as_str()),
            ("Pohunek CLI", self.pohunek_bin.as_str()),
            ("Pohunek state home", self.pohunek_state.as_str()),
            ("Pohunek runtime directory", self.pohunek_runtime.as_str()),
        ] {
            if !value.starts_with('/') {
                let message = format!("{label} path must be absolute");
                return Err(invalid(message));
            }
        }
        if self.pohunek_hosts.is_empty() {
            return Err(invalid("pohunek hosts must not be empty"));
        }
        for host in &self.pohunek_hosts {
            if !identifier_valid(host) {
                return Err(invalid("invalid Pohunek host"));
            }
        }
        for (repository, route) in &self.route_overrides {
            if !repository_valid(repository)
                || !identifier_valid(&route.host)
                || !identifier_valid(&route.project_id)
            {
                return Err(invalid("invalid route override"));
            }
        }
        if !signature_valid(&self.ai_signature) {
            return Err(invalid("invalid AI signature"));
        }
        if !is_valid_session_group(&self.session_group) {
            return Err(invalid("invalid review session group"));
        }
        self.validate_matrix()?;
        if !state_db_path_valid(&self.state_db_path) {
            return Err(invalid("state database path must be a clean absolute path"));
        }
        if self.limits_out_of_range() {
            return Err(invalid("configured limit is outside the safe range"));
        }
        if let Some(policy) = &self.policy
            && self.retention_too_short(policy)
        {
            return Err(invalid(
                "published retention must exceed the notes hand-off reminder",
            ));
        }
        Ok(())
    }

    /// Validates Matrix fields.
    ///
    /// # Errors
    ///
    /// Returns a configuration error when a Matrix field is invalid.
    fn validate_matrix(&self) -> Result<(), Error> {
        let parsed = match parse_matrix_url(&self.matrix_homeserver_url) {
            Some(value) => value,
            None => return Err(invalid("invalid Matrix URL")),
        };
        if parsed.scheme != "https"
            && !(parsed.scheme == "http" && parsed.hostname == LOOPBACK_HOST)
        {
            return Err(invalid("matrix URL must use HTTPS or loopback HTTP"));
        }
        if !matrix_user_valid(&self.matrix_mention_user)
            || self.matrix_access_token.contains('\r')
            || self.matrix_access_token.contains('\n')
            || self.matrix_access_token.contains(' ')
        {
            return Err(invalid("invalid Matrix identity"));
        }
        for (level, room) in [
            ("action", self.matrix_rooms.action.as_str()),
            ("digest", self.matrix_rooms.digest.as_str()),
            ("log", self.matrix_rooms.log.as_str()),
        ] {
            if !matrix_room_valid(room) {
                let message = format!("invalid Matrix {level} room");
                return Err(invalid(message));
            }
        }
        if !matrix_room_valid(&self.queue_matrix_room) {
            return Err(invalid("invalid Matrix queue room"));
        }
        Ok(())
    }

    /// Reports whether a configured limit is outside the safe range.
    fn limits_out_of_range(&self) -> bool {
        if self.output_max_bytes > OUTPUT_MAX_BYTES_LIMIT {
            return true;
        }
        if self.output_chunk_bytes > POHUNEK_OUTPUT_CHUNK_MAX_BYTES {
            return true;
        }
        if self.output_chunk_bytes > self.output_max_bytes {
            return true;
        }
        if self.output_wait_ms > OUTPUT_WAIT_MAX_MS {
            return true;
        }
        if self.schedule_max_bytes > SCHEDULE_MAX_BYTES_LIMIT {
            return true;
        }
        if self.result_max_bytes > self.output_max_bytes {
            return true;
        }
        if self.min_summary_characters > MIN_SUMMARY_CHARACTERS_LIMIT {
            return true;
        }
        if self.min_summary_characters > self.max_summary_characters {
            return true;
        }
        if self.matrix_max_characters > MATRIX_MAX_CHARACTERS_LIMIT {
            return true;
        }
        if self.excerpt_characters > self.matrix_max_characters / EXCERPT_DIVISOR {
            return true;
        }
        if self.github_body_max_bytes > GITHUB_BODY_MAX_BYTES_LIMIT {
            return true;
        }
        if self.terminal_cols < TERMINAL_COLS_MIN || self.terminal_cols > TERMINAL_COLS_MAX {
            return true;
        }
        if self.terminal_rows < TERMINAL_ROWS_MIN || self.terminal_rows > TERMINAL_ROWS_MAX {
            return true;
        }
        if self.session_start_attempts > SESSION_START_ATTEMPTS_MAX {
            return true;
        }
        if self.session_start_retry > RETRY_DELAY_MAX {
            return true;
        }
        if self.session_cleanup_attempts > SESSION_CLEANUP_ATTEMPTS_MAX {
            return true;
        }
        if self.session_cleanup_retry > RETRY_DELAY_MAX {
            return true;
        }
        if self.reviewer_run_attempts > REVIEWER_RUN_ATTEMPTS_MAX {
            return true;
        }
        if self.reviewer_run_retry > RETRY_DELAY_MAX {
            return true;
        }
        if self.output_poll_interval > self.session_timeout {
            return true;
        }
        if self.step_retry > STEP_RETRY_MAX {
            return true;
        }
        if self.step_retry_max_interval < self.step_retry_initial {
            return true;
        }
        if self.step_retry_max_interval > self.step_retry {
            return true;
        }
        if self.max_concurrent_reviews > MAX_CONCURRENT_REVIEWS_MAX {
            return true;
        }
        if self.review_list_limit > self.state_entry_limit {
            return true;
        }
        if self.poll_interval > POLL_INTERVAL_MAX {
            return true;
        }
        if self.worker_tick > self.poll_interval {
            return true;
        }
        if self.discovery_lock_timeout > DISCOVERY_LOCK_TIMEOUT_MAX {
            return true;
        }
        if self.discovery_lock_poll > self.discovery_lock_timeout {
            return true;
        }
        if self.review_retention < REVIEW_RETENTION_MIN {
            return true;
        }
        if self.queue_max_items > QUEUE_MAX_ITEMS_MAX {
            return true;
        }
        for duration in self.durations() {
            if duration > DURATION_MAX {
                return true;
            }
        }
        false
    }

    /// Reports whether the retention is too short for the policy.
    fn retention_too_short(&self, policy: &Policy) -> bool {
        let Ok(retention) = SignedDuration::try_from(self.published_retention) else {
            return false;
        };
        policy.handoff_reminder() >= retention
    }

    /// Returns all durations that have a range check.
    fn durations(&self) -> [Duration; 17] {
        [
            self.gh_timeout,
            self.pohunek_timeout,
            self.matrix_timeout,
            self.session_timeout,
            self.output_poll_interval,
            self.session_start_retry,
            self.session_cleanup_retry,
            self.reviewer_run_retry,
            self.step_retry,
            self.step_retry_initial,
            self.step_retry_max_interval,
            self.poll_interval,
            self.worker_tick,
            self.discovery_lock_timeout,
            self.discovery_lock_poll,
            self.review_retention,
            self.published_retention,
        ]
    }

    /// Returns the run limits of this configuration.
    pub(crate) fn run_limits(&self) -> RunLimits {
        RunLimits {
            session_timeout_ms: millis(self.session_timeout),
            output_poll_ms: millis(self.output_poll_interval),
            session_start_attempts: self.session_start_attempts,
            session_start_retry_ms: millis(self.session_start_retry),
            session_cleanup_attempts: self.session_cleanup_attempts,
            session_cleanup_retry_ms: millis(self.session_cleanup_retry),
            reviewer_run_attempts: self.reviewer_run_attempts,
            reviewer_run_retry_ms: millis(self.reviewer_run_retry),
            step_retry_ms: millis(self.step_retry),
            step_retry_initial_ms: millis(self.step_retry_initial),
            step_retry_max_interval_ms: millis(self.step_retry_max_interval),
        }
    }
}

/// Creates a configuration error with `message`.
fn invalid(message: impl Into<String>) -> Error {
    Error::config(
        CONFIGURATION_INVALID,
        CONFIG_NAME,
        Some(BoxError::from(message.into())),
    )
}

/// Reports whether `login` is a valid GitHub login.
pub(crate) fn github_login_valid(login: &str) -> bool {
    let bytes = login.as_bytes();
    if bytes.is_empty() || bytes.len() > GITHUB_LOGIN_MAX_LEN {
        return false;
    }
    if bytes[0] == b'-' || bytes[bytes.len() - 1] == b'-' {
        return false;
    }
    for &byte in bytes {
        let valid = byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-';
        if !valid {
            return false;
        }
    }
    true
}

/// Reports whether `value` is a valid identifier.
pub fn identifier_valid(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    let first = bytes[0];
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    for &byte in &bytes[1..] {
        let valid = byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'_' || byte == b'-';
        if !valid {
            return false;
        }
    }
    true
}

/// Reports whether `room` is a valid Matrix room.
fn matrix_room_valid(room: &str) -> bool {
    let bytes = room.as_bytes();
    if bytes.is_empty() || bytes[0] != b'!' {
        return false;
    }
    if bytes.len() == 1 {
        return false;
    }
    for &byte in &bytes[1..] {
        let valid = byte.is_ascii_alphanumeric()
            || byte == b'.'
            || byte == b'_'
            || byte == b'~'
            || byte == b'='
            || byte == b'-'
            || byte == b':';
        if !valid {
            return false;
        }
    }
    true
}

/// Reports whether `user` is a valid Matrix user.
fn matrix_user_valid(user: &str) -> bool {
    if !user.starts_with('@') {
        return false;
    }
    let rest = &user[1..];
    let Some(colon) = rest.find(':') else {
        return false;
    };
    let local = &rest[..colon];
    let server = &rest[colon + 1..];
    if local.is_empty() || server.is_empty() {
        return false;
    }
    for byte in local.bytes() {
        let valid = byte.is_ascii_lowercase()
            || byte.is_ascii_digit()
            || byte == b'.'
            || byte == b'_'
            || byte == b'='
            || byte == b'-'
            || byte == b'/'
            || byte == b'+';
        if !valid {
            return false;
        }
    }
    let (host, port) = match server.find(':') {
        Some(pos) => (&server[..pos], Some(&server[pos + 1..])),
        None => (server, None),
    };
    if host.is_empty() {
        return false;
    }
    for byte in host.bytes() {
        let valid = byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-';
        if !valid {
            return false;
        }
    }
    if let Some(port) = port {
        if port.is_empty() {
            return false;
        }
        for byte in port.bytes() {
            if !byte.is_ascii_digit() {
                return false;
            }
        }
    }
    true
}

/// Reports whether `signature` is a valid AI signature.
fn signature_valid(signature: &str) -> bool {
    if signature.trim() != signature {
        return false;
    }
    if signature.len() > AI_SIGNATURE_MAX_BYTES {
        return false;
    }
    if signature.contains('@') || signature.contains('<') || signature.contains('>') {
        return false;
    }
    if signature.chars().any(char::is_control) {
        return false;
    }
    let trimmed = signature.trim_start_matches(is_signature_symbol);
    trimmed.starts_with(AI_SIGNATURE_PREFIX)
}

/// Reports whether `symbol` may start an AI signature.
pub(crate) fn is_signature_symbol(symbol: char) -> bool {
    symbol == ' ' || symbol == '\u{FE0F}' || symbols::is_so(symbol)
}

/// Reports whether `path` is a clean absolute path.
fn state_db_path_valid(path: &str) -> bool {
    if !path.starts_with('/') {
        return false;
    }
    if path.contains('?') || path.contains('#') {
        return false;
    }
    if path == "/" {
        return true;
    }
    if path.ends_with('/') {
        return false;
    }
    let rest = &path[1..];
    for segment in rest.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return false;
        }
    }
    true
}

/// Converts `duration` to milliseconds, saturating on overflow.
fn millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

/// A parsed Matrix homeserver URL.
struct MatrixUrl {
    /// Lowercase scheme.
    scheme: String,
    /// Hostname without a port suffix.
    hostname: String,
}

/// Parses `raw` as a Matrix homeserver URL.
fn parse_matrix_url(raw: &str) -> Option<MatrixUrl> {
    // Any control or DEL byte fails.
    for byte in raw.bytes() {
        if byte < 0x20 || byte == 0x7f {
            return None;
        }
    }
    let before = match raw.find('#') {
        Some(pos) => {
            let after = &raw[pos + 1..];
            if !after.is_empty() {
                return None;
            }
            &raw[..pos]
        }
        None => raw,
    };
    let (scheme_opt, mut rest) = split_scheme(before)?;
    let scheme = scheme_opt.unwrap_or_default();
    if let Some(pos) = rest.find('?') {
        if pos + 1 == rest.len() {
            rest = rest[..pos].to_string();
        } else {
            return None;
        }
    }
    if !rest.starts_with('/') {
        return None;
    }
    let authority = if rest.starts_with("//") && (!scheme.is_empty() || !rest.starts_with("///")) {
        let after = &rest[2..];
        match after.find('/') {
            Some(pos) => &after[..pos],
            None => after,
        }
    } else {
        return None;
    };
    let path_start = 2 + authority.len();
    let path = if path_start < rest.len() {
        &rest[path_start..]
    } else {
        ""
    };
    if authority.contains('@') {
        return None;
    }
    let path_bytes = path.as_bytes();
    let mut idx = 0;
    while idx < path_bytes.len() {
        if path_bytes[idx] == b'%' {
            hex_pair(path_bytes, idx)?;
            idx += 3;
        } else {
            idx += 1;
        }
    }
    let hostname = host_name(&scheme, authority)?;
    if hostname.is_empty() {
        return None;
    }
    Some(MatrixUrl { scheme, hostname })
}

/// Splits `input` into an optional scheme and the rest.
fn split_scheme(input: &str) -> Option<(Option<String>, String)> {
    let bytes = input.as_bytes();
    for (idx, &byte) in bytes.iter().enumerate() {
        if byte.is_ascii_alphabetic() {
            continue;
        }
        if byte.is_ascii_digit() || byte == b'+' || byte == b'-' || byte == b'.' {
            if idx == 0 {
                return Some((None, input.to_string()));
            }
            continue;
        }
        if byte == b':' {
            if idx == 0 {
                return None;
            }
            let scheme = input[..idx].to_ascii_lowercase();
            let rest = input[idx + 1..].to_string();
            return Some((Some(scheme), rest));
        }
        return Some((None, input.to_string()));
    }
    Some((None, input.to_string()))
}

/// Returns the hostname of `authority`.
fn host_name(scheme: &str, authority: &str) -> Option<String> {
    if let Some(pos) = authority.find('[') {
        if pos == 0 {
            return ip_literal_name(authority);
        }
        return None;
    }
    if authority.contains(':') {
        let first = authority.find(':')?;
        let last = authority.rfind(':')?;
        let port_start = if last != first && (scheme == "http" || scheme == "https") {
            first
        } else {
            last
        };
        let port_slice = &authority[port_start..];
        if !valid_optional_port(port_slice) {
            return None;
        }
    }
    if !host_escapes_valid(authority) {
        return None;
    }
    let hostname = if let Some(last) = authority.rfind(':') {
        let candidate = &authority[last..];
        if valid_optional_port(candidate) && candidate.starts_with(':') {
            &authority[..last]
        } else {
            authority
        }
    } else {
        authority
    };
    Some(hostname.to_string())
}

/// Returns the hostname of an IP literal authority.
fn ip_literal_name(authority: &str) -> Option<String> {
    let close = authority.rfind(']')?;
    let after = &authority[close + 1..];
    if !valid_optional_port(after) {
        return None;
    }
    let inner = &authority[1..close];
    let (address, zone) = match inner.find("%25") {
        Some(pos) => (&inner[..pos], Some(&inner[pos + 3..])),
        None => (inner, None),
    };
    if address.contains('%') {
        return None;
    }
    if address.parse::<Ipv6Addr>().is_err() {
        return None;
    }
    if let Some(zone) = zone
        && (zone.is_empty() || !zone_escapes_valid(zone))
    {
        return None;
    }
    Some(inner.to_string())
}

/// Reports whether `port` is a valid optional port.
fn valid_optional_port(port: &str) -> bool {
    if let Some(stripped) = port.strip_prefix(':') {
        for byte in stripped.bytes() {
            if !byte.is_ascii_digit() {
                return false;
            }
        }
        true
    } else {
        port.is_empty()
    }
}

/// Reports whether `host` has valid escapes.
fn host_escapes_valid(host: &str) -> bool {
    let bytes = host.as_bytes();
    let mut idx = 0;
    while idx < bytes.len() {
        let byte = bytes[idx];
        if byte == b'%' {
            let Some(value) = hex_pair(bytes, idx) else {
                return false;
            };
            let is_percent_encoded =
                idx + 2 < bytes.len() && bytes[idx + 1] == b'2' && bytes[idx + 2] == b'5';
            if value < 0x80 && !is_percent_encoded {
                return false;
            }
            idx += 3;
            continue;
        }
        if byte < 0x80 && !host_byte_allowed(u32::from(byte)) {
            return false;
        }
        idx += 1;
    }
    true
}

/// Reports whether `zone` has valid escapes.
fn zone_escapes_valid(zone: &str) -> bool {
    let bytes = zone.as_bytes();
    let mut idx = 0;
    while idx < bytes.len() {
        let byte = bytes[idx];
        if byte == b'%' {
            let Some(value) = hex_pair(bytes, idx) else {
                return false;
            };
            let is_percent_encoded =
                idx + 2 < bytes.len() && bytes[idx + 1] == b'2' && bytes[idx + 2] == b'5';
            let allowed = value == 0x20 || (value < 0x80 && host_byte_allowed(value));
            if !is_percent_encoded && !allowed {
                return false;
            }
            idx += 3;
            continue;
        }
        if byte < 0x80 && !host_byte_allowed(u32::from(byte)) {
            return false;
        }
        idx += 1;
    }
    true
}

/// Parses the hex pair at `percent`.
fn hex_pair(bytes: &[u8], percent: usize) -> Option<u32> {
    if percent + 2 >= bytes.len() {
        return None;
    }
    let high = bytes[percent + 1];
    let low = bytes[percent + 2];
    let high_val = hex_value(high)?;
    let low_val = hex_value(low)?;
    Some(u32::from(high_val) * 16 + u32::from(low_val))
}

/// Returns the hex value of `byte`.
fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Reports whether `value` is allowed in a host.
fn host_byte_allowed(value: u32) -> bool {
    if value > 255 {
        return false;
    }
    let byte = value as u8;
    if byte.is_ascii_alphanumeric() {
        return true;
    }
    matches!(
        byte,
        b'!' | b'$'
            | b'&'
            | b'\''
            | b'('
            | b')'
            | b'*'
            | b'+'
            | b','
            | b';'
            | b'='
            | b':'
            | b'['
            | b']'
            | b'<'
            | b'>'
            | b'"'
            | b'-'
            | b'.'
            | b'_'
            | b'~'
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use autoreview_schedule::scheduler;

    use super::{Config, MatrixRooms};

    use crate::error::error_code;

    /// A configuration that passes validation; every value is synthetic.
    fn valid_config() -> Config {
        Config {
            policy: None,
            schedule: scheduler::Policy {
                debounce: time::Duration::seconds(120),
                retry_delays: vec![time::Duration::seconds(60)],
            },
            github_host: "github.com".to_owned(),
            github_login: "example-user".to_owned(),
            github_token: "placeholder-token".to_owned(),
            gh_bin: "/usr/bin/gh".to_owned(),
            pohunek_bin: "/usr/bin/pohunek".to_owned(),
            pohunek_state: "/var/lib/pohunek".to_owned(),
            pohunek_runtime: "/run/pohunek".to_owned(),
            pohunek_hosts: vec!["example-host".to_owned()],
            route_overrides: std::collections::BTreeMap::new(),
            matrix_homeserver_url: "https://matrix.example.org".to_owned(),
            matrix_access_token: "placeholder-token".to_owned(),
            secret_variables: std::collections::BTreeSet::new(),
            matrix_mention_user: "@example-user:example.org".to_owned(),
            matrix_rooms: MatrixRooms {
                action: "!action:example.org".to_owned(),
                digest: "!digest:example.org".to_owned(),
                log: "!log:example.org".to_owned(),
            },
            queue_matrix_room: "!queue:example.org".to_owned(),
            queue_max_items: 10,
            state_db_path: "/var/lib/autoreview/state.db".to_owned(),
            gh_timeout: Duration::from_secs(30),
            pohunek_timeout: Duration::from_secs(60),
            matrix_timeout: Duration::from_secs(30),
            session_timeout: Duration::from_secs(30 * 60),
            output_poll_interval: Duration::from_secs(15),
            session_start_retry: Duration::from_secs(30),
            session_cleanup_retry: Duration::from_secs(30),
            reviewer_run_retry: Duration::from_secs(30),
            step_retry: Duration::from_secs(10 * 60),
            step_retry_initial: Duration::from_secs(30),
            step_retry_max_interval: Duration::from_secs(5 * 60),
            poll_interval: Duration::from_secs(5 * 60),
            worker_tick: Duration::from_secs(30),
            discovery_lock_timeout: Duration::from_secs(60),
            discovery_lock_poll: Duration::from_secs(1),
            review_retention: Duration::from_secs(30 * 24 * 60 * 60),
            session_start_attempts: 3,
            session_cleanup_attempts: 3,
            reviewer_run_attempts: 2,
            max_concurrent_reviews: 4,
            review_list_limit: 20,
            poll_limit: 100,
            search_page_limit: 100,
            state_entry_limit: 1000,
            schedule_max_bytes: 1 << 20,
            output_max_bytes: 1 << 20,
            output_chunk_bytes: 64 << 10,
            output_wait_ms: 1000,
            result_max_bytes: 512 << 10,
            max_findings: 20,
            max_tests: 20,
            min_summary_characters: 10,
            max_summary_characters: 1000,
            max_title_characters: 256,
            max_body_characters: 4000,
            max_discussion_characters: 8000,
            max_path_characters: 512,
            max_test_characters: 256,
            github_body_max_bytes: 65_536,
            ai_signature: "Written by AI: example-agent".to_owned(),
            matrix_max_characters: 4000,
            published_retention: Duration::from_secs(7 * 24 * 60 * 60),
            excerpt_characters: 1000,
            terminal_cols: 120,
            terminal_rows: 100,
            session_group: "autoreview".to_owned(),
            session_name_prefix: "github-review-".to_owned(),
            session_branch_prefix: "autoreview/review/".to_owned(),
        }
    }

    #[test]
    fn missing_or_unsafe_configuration_fails_fast() {
        valid_config()
            .validate()
            .expect("the base configuration is valid");
        let too_long = "a".repeat(201);
        let signatures = [
            "  ",
            "AI review\nby @example-user",
            "AI review by @example-user",
            "AI <!-- review -->",
            " AI review",
            too_long.as_str(),
            "**AI review**: machine-written",
            "🤖 AI review: example-agent",
        ];
        for signature in signatures {
            let mut config = valid_config();
            config.ai_signature = signature.to_owned();
            let error = config.validate().expect_err("the signature is refused");
            assert_eq!(
                error.to_string(),
                "configuration_invalid: config: invalid AI signature"
            );
            assert_eq!(error_code(&error), "configuration_invalid");
        }
        let mut config = valid_config();
        config.matrix_rooms.action = String::new();
        let error = config
            .validate()
            .expect_err("an empty action room is refused");
        assert_eq!(
            error.to_string(),
            "configuration_invalid: config: invalid Matrix action room"
        );
    }

    #[test]
    fn published_retention_must_exceed_the_notes_handoff_reminder() {
        let document = include_str!("../../../testdata/review-policy.yaml").replacen(
            "    on_clean: approve\n",
            "    on_clean: await\n    on_notes: comment\n",
            1,
        ) + "notes_handoff:\n  reminder_hours: 24\n";
        let loaded = autoreview_policy::parse(document.as_bytes(), |relative| {
            std::fs::read(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../testdata")
                    .join(relative),
            )
        })
        .expect("the policy with a notes hand-off parses");
        let mut config = valid_config();
        config.policy = Some(loaded);
        config.published_retention = Duration::from_secs(3600);
        let error = config
            .validate()
            .expect_err("a retention shorter than the reminder is refused");
        assert_eq!(
            error.to_string(),
            concat!(
                "configuration_invalid: config: ",
                "published retention must exceed the notes hand-off reminder"
            )
        );
    }
}
