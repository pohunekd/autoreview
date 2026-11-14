//! The pohunek CLI calls of the engine: project routing, the session list and the session start.
//!
//! Every call is an argv list, never a shell. The child gets the [`POHUNEK_INHERITED`] variables
//! of the engine and its own state and runtime paths; a [`local_pohunek_json`] child keeps the
//! caller's paths instead.

// Rust guideline compliant 2026-06-26

pub(crate) mod output;

pub(crate) mod sessions;

pub(crate) mod version;

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::ops::RangeInclusive;
use std::time::Duration;

use autoreview_engine::Error;
use autoreview_engine::config::Config;
use autoreview_engine::error::BoxError;
use autoreview_engine::model::{
    HOST_STATE_UNAVAILABLE, HostCapacity, ProjectRoute, repository_valid,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::process::{POHUNEK_INHERITED, ProcessError, child_environment, run};

/// Variable that points the pohunek child at the state home.
const STATE_HOME_VARIABLE: &str = "XDG_STATE_HOME";

/// Variable that points the pohunek child at the runtime directory.
const RUNTIME_DIR_VARIABLE: &str = "XDG_RUNTIME_DIR";

/// Variable that makes the pohunek child print without a pager.
const PAGER_VARIABLE: &str = "PAGER";

/// Value of [`PAGER_VARIABLE`] that prints straight to the output.
const PAGER_VALUE: &str = "cat";

/// Package of the `moc.host-state` plugin.
const HOST_STATE_PACKAGE: &str = "moc.host-state";

/// Command of the `moc.host-state` plugin that prints one `HostCapacity` document.
const HOST_STATE_COMMAND: &str = "host-state";

/// Option of `host command run` that sets the daemon's time limit of the command.
const TIMEOUT_OPTION: &str = "--timeout-ms";

/// Exit statuses of `host command run` for a `command_*` answer of the daemon.
///
/// pohunek 0.38.1 and later: 40 to 51 name one code each, 52 a code that the CLI does not know.
const COMMAND_EXIT_STATUSES: RangeInclusive<i32> = 40..=52;

/// Text before the code of a daemon error on the stderr of the pohunek CLI.
const DAEMON_ERROR_MARKER: &str = "daemon error: ";

/// Prefix of the daemon codes of a package command that did not run.
const COMMAND_CODE_PREFIX: &str = "command_";

/// Field names of the pohunek JSON, in lower case; a key that only differs in case is refused.
const KNOWN_KEYS: [&str; 30] = [
    "ok",
    "err",
    "code",
    "id",
    "source",
    "repo_root",
    "origin_url",
    "name",
    "agent",
    "project_id",
    "branch",
    "state",
    "is_linked_worktree",
    "warnings",
    "runtime",
    "kind",
    "message",
    "detail",
    "worker_instance_id",
    "runtime_generation",
    "loss_cause",
    "loss_reason",
    "session_id",
    "history_start_offset",
    "start_offset",
    "next_offset",
    "runtime_end_offset",
    "data_base64",
    "has_more",
    "timed_out",
];

/// One project of `pohunek project list --json`.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
pub(crate) struct PohunekProject {
    /// Identifier of the project.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    pub(crate) id: String,
    /// Source the project was registered from.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    pub(crate) source: String,
    /// Absolute path of the repository root of the project.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    pub(crate) repo_root: String,
    /// Origin URL of the repository of the project.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    pub(crate) origin_url: String,
}

autoreview_go_compat::object_only_serde!(PohunekProject);

/// The envelope of a successful pohunek call; a missing or `null` `ok` is refused.
#[derive(Debug, Deserialize)]
struct OkEnvelope<T> {
    /// The result of the call.
    ok: Option<T>,
}

/// The envelope of a failed pohunek call.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct FailureEnvelope {
    /// The error of the call.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    err: FailureDetail,
}

autoreview_go_compat::object_only_serde!(FailureEnvelope);

/// The error object of a failed pohunek call.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct FailureDetail {
    /// The allowlisted error code, or empty when the CLI reported none.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    code: String,
}

autoreview_go_compat::object_only_serde!(FailureDetail);

/// Returns the environment of a pohunek child: the engine's [`POHUNEK_INHERITED`] variables
/// without the secrets, then the configured state and runtime paths and the pager setting.
pub(crate) fn pohunek_environment(
    config: &Config,
    parent: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    child_environment(
        parent,
        &POHUNEK_INHERITED,
        &config.secret_variables,
        [
            (
                OsString::from(STATE_HOME_VARIABLE),
                OsString::from(&config.pohunek_state),
            ),
            (
                OsString::from(RUNTIME_DIR_VARIABLE),
                OsString::from(&config.pohunek_runtime),
            ),
            (OsString::from(PAGER_VARIABLE), OsString::from(PAGER_VALUE)),
        ],
    )
}

/// Returns the environment of a pohunek child that reaches the caller's own daemon.
///
/// It is the [`POHUNEK_INHERITED`] variables of `parent` without the `AUTOREVIEW_` variables,
/// then the pager setting. Every `*_env` key of the configuration names an `AUTOREVIEW_`
/// variable, so no configuration is needed to drop the secrets. The state and runtime
/// directories stay the caller's.
pub(crate) fn local_environment(
    parent: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    child_environment(
        parent,
        &POHUNEK_INHERITED,
        &BTreeSet::new(),
        [(OsString::from(PAGER_VARIABLE), OsString::from(PAGER_VALUE))],
    )
}

/// Runs the pohunek CLI `program` with `arguments` for the caller's own daemon.
///
/// It returns the `ok` value of the answer, decoded as the engine's calls decode it. The child
/// gets the `local_environment` of this process and is killed when `timeout` elapses.
///
/// # Errors
///
/// Returns the cause of a run that cannot start, times out or exits unsuccessfully, inside the
/// pohunek error code of the `err` envelope when its output reports an allowlisted code, and a
/// fixed text when the answer holds no `ok` value of type `T`. No cause holds stdout text.
pub async fn local_pohunek_json<T: DeserializeOwned>(
    program: &str,
    arguments: &[String],
    timeout: Duration,
) -> Result<T, BoxError> {
    let environment = local_environment(std::env::vars_os());
    // No caller cancels a local call; the timeout bounds it.
    let cancel = CancellationToken::new();
    let output = run(&cancel, program, arguments, &[], &environment, timeout)
        .await
        .map_err(|error| -> BoxError {
            let code = failure_code(error.stdout());
            if code.is_empty() {
                Box::new(error)
            } else {
                Box::new(Error::pohunek_code(code, Box::new(error)))
            }
        })?;
    decode_ok(&output).ok_or_else(|| "the answer holds no ok value of the expected shape".into())
}

/// Returns the arguments that address `host` and then run `command`.
pub(crate) fn pohunek_arguments(host: &str, command: &[&str]) -> Vec<String> {
    let mut arguments = Vec::with_capacity(2 + command.len());
    arguments.push("--host".to_owned());
    arguments.push(host.to_owned());
    for item in command {
        arguments.push((*item).to_owned());
    }
    arguments
}

/// Returns the arguments of `project list` on `host`.
pub(crate) fn projects_arguments(host: &str) -> Vec<String> {
    pohunek_arguments(host, &["project", "list", "--json"])
}

/// Runs the pohunek CLI with `args` in the pohunek environment and returns its stdout.
///
/// A failed run is returned as the error of [`pohunek_failure`].
pub(crate) async fn pohunek_call(
    config: &Config,
    cancel: &CancellationToken,
    args: &[String],
) -> Result<String, Error> {
    let environment = pohunek_environment(config, std::env::vars_os());
    match run(
        cancel,
        &config.pohunek_bin,
        args,
        &[],
        &environment,
        config.pohunek_timeout,
    )
    .await
    {
        Ok(stdout) => Ok(stdout),
        Err(error) => {
            let output = error.stdout().to_owned();
            Err(pohunek_failure(&output, Box::new(error)))
        }
    }
}

/// Returns the error of a failed pohunek call, from the allowlisted code its output reports.
pub(crate) fn pohunek_failure(output: &str, cause: BoxError) -> Error {
    let code = failure_code(output);
    let (name, permanent) = match code.as_str() {
        "session_not_found" => ("pohunek_session_not_found", true),
        "request_timeout" => ("pohunek_request_timeout", false),
        _ => ("pohunek_request_failed", false),
    };
    let cause = if code.is_empty() {
        cause
    } else {
        Box::new(Error::pohunek_code(code, cause))
    };
    Error::workflow(name, permanent, Some(cause))
}

/// Returns the allowlisted error code of a failed call, or an empty string when there is none.
pub(crate) fn failure_code(output: &str) -> String {
    decode_strict::<FailureEnvelope>(output)
        .map(|envelope| envelope.err.code)
        .filter(|code| is_error_code(code))
        .unwrap_or_default()
}

/// Returns the `pohunek_response_invalid` error.
pub(crate) fn response_invalid() -> Error {
    Error::workflow("pohunek_response_invalid", true, None)
}

/// Decodes the `ok` result of a successful call, or `None` when the output does not hold one.
pub(crate) fn decode_ok<T: DeserializeOwned>(output: &str) -> Option<T> {
    decode_strict::<OkEnvelope<T>>(output).and_then(|envelope| envelope.ok)
}

/// Decodes `text` as `T`, which must be a JSON object with no case-variant key.
///
/// A top-level array is refused, because serde would read it into a struct by position.
fn decode_strict<T: DeserializeOwned>(text: &str) -> Option<T> {
    let value: Value = serde_json::from_str(text).ok()?;
    if !value.is_object() || has_case_variant_key(&value) {
        return None;
    }
    serde_json::from_str::<T>(text).ok()
}

/// Reports whether an object key of `value` is a known field name in another case.
fn has_case_variant_key(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if is_case_variant(key) {
                    return true;
                }
                if has_case_variant_key(child) {
                    return true;
                }
            }
            false
        }
        Value::Array(items) => {
            for item in items {
                if has_case_variant_key(item) {
                    return true;
                }
            }
            false
        }
        _ => false,
    }
}

/// Reports whether `key` is not a known field name but equals one under Go's case folding.
fn is_case_variant(key: &str) -> bool {
    if KNOWN_KEYS.contains(&key) {
        return false;
    }
    KNOWN_KEYS.iter().any(|known| folds_to(known, key))
}

/// Reports whether `key` folds to the lower-case `known` name as Go's `encoding/json` folds it.
///
/// The Kelvin sign folds to `k` and the long s to `s`; every other non-ASCII character is kept.
fn folds_to(known: &str, key: &str) -> bool {
    known.chars().eq(key.chars().map(fold_character))
}

/// Maps a key character to the lower-case ASCII letter that Go's case folding equates it with.
fn fold_character(character: char) -> char {
    match character {
        '\u{212A}' => 'k',
        '\u{17F}' => 's',
        _ => character.to_ascii_lowercase(),
    }
}

/// Returns the routes of the projects of one host's `project list` output that serve
/// `repository`, in the order of the output.
pub(crate) fn route_candidates(
    host: &str,
    github_host: &str,
    repository: &str,
    sources: &[String],
    output: &str,
) -> Result<Vec<ProjectRoute>, Error> {
    let entries: Vec<Option<PohunekProject>> = decode_ok(output).ok_or_else(response_invalid)?;
    let projects = autoreview_go_compat::null_entries::zero_entries(entries);
    let mut routes = Vec::new();
    for project in projects {
        if !sources.contains(&project.source) {
            continue;
        }
        if canonical_repository(github_host, &project.origin_url) != repository {
            continue;
        }
        if !is_identifier(&project.id) {
            continue;
        }
        if !project.repo_root.starts_with('/') {
            continue;
        }
        routes.push(ProjectRoute {
            host: host.to_owned(),
            project_id: project.id,
        });
    }
    Ok(routes)
}

/// Returns the routes of the projects of `host` that serve `repository`.
///
/// One `project list` call; the engine decides which host and route a review uses.
pub(crate) async fn host_routes(
    config: &Config,
    cancel: &CancellationToken,
    host: &str,
    repository: &str,
    sources: &[String],
) -> Result<Vec<ProjectRoute>, Error> {
    let output = pohunek_call(config, cancel, &projects_arguments(host)).await?;
    route_candidates(host, &config.github_host, repository, sources, &output)
}

/// Returns the arguments of the host-state command on `host`, limited to `timeout`.
///
/// `host command run` names the host itself, so the arguments carry no `--host`. The daemon
/// stops the command after `timeout` and answers with `command_timed_out`.
fn host_state_arguments(host: &str, timeout: Duration) -> Vec<String> {
    let millis = timeout.as_millis().to_string();
    [
        "host",
        "command",
        "run",
        host,
        HOST_STATE_PACKAGE,
        HOST_STATE_COMMAND,
        TIMEOUT_OPTION,
        millis.as_str(),
    ]
    .map(str::to_owned)
    .to_vec()
}

/// Returns the state that the `moc.host-state` plugin reports for `host`.
///
/// One `host command run` call, which the daemon stops after `timeout`; its standard output is
/// the document. A daemon that answers with a `command_*` code (the plugin is not installed,
/// the command is unsupported, not exposed, busy or timed out) gives `host_state_unavailable`:
/// the host answers, its state is unknown. Any other failure is the pohunek failure of the
/// call, as for a project list.
pub(crate) async fn host_capacity(
    config: &Config,
    cancel: &CancellationToken,
    host: &str,
    timeout: Duration,
) -> Result<HostCapacity, Error> {
    let environment = pohunek_environment(config, std::env::vars_os());
    let arguments = host_state_arguments(host, timeout);
    match run(
        cancel,
        &config.pohunek_bin,
        &arguments,
        &[],
        &environment,
        config.pohunek_timeout,
    )
    .await
    {
        Ok(output) => HostCapacity::parse(&output),
        Err(error) => {
            // `Some` for a daemon answer, holding the `command_*` code that stderr names.
            let answer = match &error {
                ProcessError::Exit { code, stderr, .. }
                    if !cancel.is_cancelled() && daemon_answered(*code, stderr) =>
                {
                    Some(daemon_command_code(stderr))
                }
                _ => None,
            };
            match answer {
                Some(command) => {
                    let cause: BoxError = match command {
                        Some(code) => Box::new(Error::pohunek_code(code, Box::new(error))),
                        None => Box::new(error),
                    };
                    Err(Error::workflow(HOST_STATE_UNAVAILABLE, false, Some(cause)))
                }
                None => {
                    let output = error.stdout().to_owned();
                    Err(pohunek_failure(&output, Box::new(error)))
                }
            }
        }
    }
}

/// Returns whether a failed `host command run` is a `command_*` answer of the daemon.
///
/// An exit status from 40 to 52 is one, and so is a `command_*` code on stderr. Exit 1 without
/// such a code (an unreachable host, a transport error), a usage error and a signal are not.
fn daemon_answered(code: Option<i32>, stderr: &str) -> bool {
    code.is_some_and(|code| COMMAND_EXIT_STATUSES.contains(&code))
        || daemon_command_code(stderr).is_some()
}

/// Returns the `command_*` code of a daemon error that the CLI printed on stderr.
///
/// The CLI prints `pohunek: daemon error: <area>/<code>: <text>` without `--json`. Returns `None`
/// when the text holds no daemon error or when its code is not a `command_*` error code.
fn daemon_command_code(stderr: &str) -> Option<String> {
    let (_, rest) = stderr.split_once(DAEMON_ERROR_MARKER)?;
    let qualified = rest.split([':', ' ', '\n']).next()?;
    let code = qualified.rsplit('/').next()?.trim();
    (code.starts_with(COMMAND_CODE_PREFIX) && is_error_code(code)).then(|| code.to_owned())
}

/// Returns the `owner/name` that a project origin URL names on `host`, or an empty string.
///
/// Every refusal is an empty string.
pub(crate) fn canonical_repository(host: &str, raw: &str) -> String {
    let mut current = raw.to_owned();
    if current.contains('@') && !current.contains("://") {
        let Some((prefix, path)) = current.split_once(':') else {
            return String::new();
        };
        let suffix = "@".to_owned() + host;
        if !prefix.ends_with(suffix.as_str()) {
            return String::new();
        }
        current = format!("https://{host}/{path}");
    }
    let Some((hostname, path)) = parse_url(&current) else {
        return String::new();
    };
    if go_lowercase(&hostname) != host {
        return String::new();
    }
    let trimmed = path.strip_prefix('/').unwrap_or(path.as_str());
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    let value = go_lowercase(trimmed);
    if repository_valid(&value) {
        value
    } else {
        String::new()
    }
}

/// Returns the decoded hostname and path of `raw` when Go's `url.Parse` accepts it and the
/// canonical check reads nothing but those two.
///
/// It refuses, as the canonical check does, a fragment, a query, userinfo and an IP literal.
fn parse_url(raw: &str) -> Option<(String, String)> {
    let (before_fragment, fragment) = match raw.split_once('#') {
        Some(pair) => pair,
        None => (raw, ""),
    };
    if !fragment.is_empty() {
        return None;
    }
    for byte in before_fragment.bytes() {
        if byte < 0x20 || byte == 0x7f {
            return None;
        }
    }
    let (scheme, rest) = get_scheme(before_fragment)?;
    let (rest, query) = match rest.split_once('?') {
        Some(pair) => pair,
        None => (rest, ""),
    };
    if !query.is_empty() {
        return None;
    }
    if !rest.starts_with('/') {
        if !scheme.is_empty() {
            return Some((String::new(), String::new()));
        }
        let before_slash = match rest.find('/') {
            Some(pos) => {
                let (head, _) = rest.split_at(pos);
                head
            }
            None => rest,
        };
        if before_slash.contains(':') {
            return None;
        }
    }
    let (hostname, path_text) =
        if rest.starts_with("//") && (!scheme.is_empty() || !rest.starts_with("///")) {
            let after = rest.strip_prefix("//").unwrap_or(rest);
            let (authority, path_text) = match after.find('/') {
                Some(pos) => after.split_at(pos),
                None => (after, ""),
            };
            let host = parse_authority(&scheme, authority)?;
            let hostname = go_hostname(&host);
            (hostname, path_text)
        } else {
            (String::new(), rest)
        };
    let path = unescape(path_text, Mode::Path)?;
    Some((hostname, String::from_utf8_lossy(&path).into_owned()))
}

/// Splits the scheme off `raw` as Go's `getScheme` does, lowercasing it.
///
/// `None` is an error, which is a scheme that starts with a colon.
fn get_scheme(raw: &str) -> Option<(String, &str)> {
    let bytes = raw.as_bytes();
    for (index, &byte) in bytes.iter().enumerate() {
        if byte.is_ascii_alphabetic() {
            continue;
        }
        if byte.is_ascii_digit() || byte == b'+' || byte == b'-' || byte == b'.' {
            if index == 0 {
                return Some((String::new(), raw));
            }
            continue;
        }
        if byte == b':' {
            if index == 0 {
                return None;
            }
            let (scheme, rest) = raw.split_at(index);
            return Some((scheme.to_ascii_lowercase(), rest.strip_prefix(':')?));
        }
        return Some((String::new(), raw));
    }
    Some((String::new(), raw))
}

/// Returns the decoded host of `authority`, or `None` when it has userinfo or Go refuses it.
fn parse_authority(scheme: &str, authority: &str) -> Option<String> {
    if authority.contains('@') {
        return None;
    }
    parse_host(scheme, authority)
}

/// Returns the decoded host of `host`, checking its port as Go's `parseHost` does.
///
/// A host with `[` is refused: an IP literal never equals the GitHub host, which has no colon.
fn parse_host(scheme: &str, host: &str) -> Option<String> {
    if host.contains('[') {
        return None;
    }
    if host.contains(':') {
        let first = host.find(':')?;
        let last = host.rfind(':')?;
        let is_web = scheme == "http" || scheme == "https";
        let colon = if last != first && !is_web {
            last
        } else {
            first
        };
        let port = host.split_at(colon).1;
        if !valid_optional_port(port) {
            return None;
        }
        let decoded = unescape(host, Mode::Host)?;
        return Some(String::from_utf8_lossy(&decoded).into_owned());
    }
    let decoded = unescape(host, Mode::Host)?;
    Some(String::from_utf8_lossy(&decoded).into_owned())
}

/// Returns the hostname of a decoded host: the host without a valid port, as Go's `Hostname`.
fn go_hostname(host: &str) -> String {
    if let Some(colon) = host.rfind(':') {
        let port = host.split_at(colon).1;
        if valid_optional_port(port) {
            let (head, _) = host.split_at(colon);
            return head.to_owned();
        }
    }
    host.to_owned()
}

/// Reports whether `port` is empty or a colon followed by decimal digits, as Go's
/// `validOptionalPort`.
fn valid_optional_port(port: &str) -> bool {
    if port.is_empty() {
        return true;
    }
    if let Some(digits) = port.strip_prefix(':') {
        for byte in digits.bytes() {
            if !byte.is_ascii_digit() {
                return false;
            }
        }
        return true;
    }
    false
}

/// The part of a URL that [`unescape`] decodes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// A host, where only escapes of non-ASCII bytes are allowed.
    Host,
    /// A path, where every escape decodes.
    Path,
}

/// Decodes the percent escapes of `text` as Go's `unescape` does for `mode`.
///
/// `None` is an error: a malformed escape, or in host mode an escape of an ASCII byte or an ASCII
/// byte that a host may not hold.
fn unescape(text: &str, mode: Mode) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while let Some(&byte) = bytes.get(index) {
        if byte == b'%' {
            let high = bytes.get(index + 1).copied().and_then(hex_value)?;
            let low = bytes.get(index + 2).copied().and_then(hex_value)?;
            if mode == Mode::Host {
                let value = (high << 4) | low;
                if high < 8 && !(high == 2 && low == 5) {
                    return None;
                }
                out.push(value);
            } else {
                out.push((high << 4) | low);
            }
            index += 3;
        } else {
            if mode == Mode::Host && byte < 0x80 && !is_host_byte(byte) {
                return None;
            }
            out.push(byte);
            index += 1;
        }
    }
    Some(out)
}

/// Returns the value of an ASCII hexadecimal digit.
fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Reports whether an ASCII byte may appear unescaped in a host, as Go's host table allows.
fn is_host_byte(byte: u8) -> bool {
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

/// Lowercases `text` as Go's `strings.ToLower` does: U+0130 becomes `i`.
fn go_lowercase(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        if character == '\u{130}' {
            out.push('i');
        } else {
            let mut lower = character.to_lowercase();
            if let Some(first) = lower.next() {
                out.push(first);
                for rest in lower {
                    out.push(rest);
                }
            } else {
                out.push(character);
            }
        }
    }
    out
}

/// Reports whether `text` matches Go's `^[A-Za-z0-9][A-Za-z0-9._-]*$`.
pub(crate) fn is_identifier(text: &str) -> bool {
    let bytes = text.as_bytes();
    let Some((first, rest)) = bytes.split_first() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    for &byte in rest {
        let valid = byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'_' || byte == b'-';
        if !valid {
            return false;
        }
    }
    true
}

/// Reports whether `text` matches Go's `^[a-z][a-z0-9_]{0,63}$`.
fn is_error_code(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() > 64 {
        return false;
    }
    let Some((first, rest)) = bytes.split_first() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }
    for &byte in rest {
        let valid = byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_';
        if !valid {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::{daemon_answered, daemon_command_code, local_environment};

    fn pairs(items: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        items
            .iter()
            .map(|(name, value)| (OsString::from(name), OsString::from(value)))
            .collect()
    }

    #[test]
    fn local_environment_keeps_the_daemon_paths_and_drops_identity_and_secrets() {
        let parent = pairs(&[
            ("PATH", "/usr/bin"),
            ("POHUNEK_SESSION_ID", "session-1"),
            ("POHUNEK_DAEMON_ID", "daemon-1"),
            ("XDG_STATE_HOME", "/home/operator/.local/state"),
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
            ("GH_TOKEN", "synthetic-gh"),
            ("MATRIX_ACCESS_TOKEN", "synthetic-matrix"),
            ("AUTOREVIEW_GH_TOKEN", "synthetic-autoreview"),
            ("SHARED_TOOL_TOKEN", "synthetic-shared"),
            ("GITHUB_TOKEN", "synthetic-github"),
            ("HTTPS_PROXY", "http://proxy.example:3128"),
            ("PAGER", "less"),
        ]);

        assert_eq!(
            local_environment(parent),
            pairs(&[
                ("PATH", "/usr/bin"),
                ("XDG_STATE_HOME", "/home/operator/.local/state"),
                ("XDG_RUNTIME_DIR", "/run/user/1000"),
                ("PAGER", "cat"),
            ])
        );
    }

    #[test]
    fn only_a_command_code_of_the_daemon_leaves_the_host_state_unknown() {
        // (exit status, stderr of a failed `host command run`, unknown state, expected code)
        let cases = [
            (
                Some(41),
                "pohunek: daemon error: runtime/command_package_not_installed: the package is \
                 not installed\nhint: run pohunek plugin enable",
                true,
                Some("command_package_not_installed"),
            ),
            (
                Some(40),
                "pohunek: daemon error: command_unsupported: the daemon has no command.run",
                true,
                Some("command_unsupported"),
            ),
            (
                Some(46),
                "pohunek: daemon error: runtime/command_busy: one run per command",
                true,
                Some("command_busy"),
            ),
            (
                Some(48),
                "pohunek: daemon error: runtime/command_timed_out: the command did not finish \
                 within its time limit and was stopped\nhint: retry, or raise the timeout",
                true,
                Some("command_timed_out"),
            ),
            (Some(48), "", true, None),
            (Some(51), "pohunek: unexpected text", true, None),
            (Some(52), "", true, None),
            (
                Some(1),
                "pohunek: host 'review-host-b' was not found in any configured overlay\nhint: x",
                false,
                None,
            ),
            (
                Some(1),
                "pohunek: daemon error: overlay/peer_unreachable: no route",
                false,
                None,
            ),
            (
                Some(1),
                "pohunek: daemon error: runtime/Command_Busy: mixed case",
                false,
                None,
            ),
            (
                Some(1),
                "pohunek: daemon error: runtime/command_busy",
                true,
                Some("command_busy"),
            ),
            (
                Some(2),
                "error: unexpected argument '--timeout-ms' found",
                false,
                None,
            ),
            (Some(39), "", false, None),
            (Some(53), "", false, None),
            (None, "", false, None),
        ];
        for (status, stderr, unknown, want) in cases {
            assert_eq!(
                daemon_answered(status, stderr),
                unknown,
                "{status:?} {stderr:?}"
            );
            assert_eq!(daemon_command_code(stderr).as_deref(), want, "{stderr:?}");
        }
    }
}
