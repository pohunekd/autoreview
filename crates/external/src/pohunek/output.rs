//! The pohunek calls that read reviewer session output and remove finished sessions.
//!
//! Mirrors `cmd/autoreview/pohunek.go:252-348`. The argument lists and the decoding are pure
//! functions of their inputs; the calls take their runner as a parameter, so both are checked
//! without a child process.

// Rust guideline compliant 2026-06-26

use std::cmp::min;

use autoreview_engine::config::Config;
use autoreview_engine::model::SessionIdentity;
use autoreview_engine::{Error, error_code};
use autoreview_policy::Protocol;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::de::IgnoredAny;
use serde::{Deserialize, Deserializer, Serialize};
use tokio_util::sync::CancellationToken;

use super::sessions::session_list_arguments;
use super::{decode_ok, decode_strict, pohunek_call, response_invalid};

/// Prefix of the line that the pohunek Codex wrapper prints with its run status.
const CODEX_EXIT_PREFIX: &[u8] = b"[pohunek-codex-review exited with status ";

/// Error code of a removal whose session is already absent.
const SESSION_NOT_FOUND: &str = "pohunek_session_not_found";

/// Limits of one session output read, from the output settings.
#[derive(Debug, Clone, Copy)]
pub(crate) struct OutputLimits {
    /// Most bytes the output of one session may hold.
    pub(crate) max_bytes: i64,
    /// Most bytes one `session output` call may return.
    pub(crate) chunk_bytes: i64,
    /// Longest wait of one `session output` call, in milliseconds.
    pub(crate) wait_ms: i64,
}

/// One decoded chunk of a session output.
#[derive(Debug)]
pub(crate) struct OutputChunk {
    /// Bytes of the chunk, in order.
    pub(crate) data: Vec<u8>,
    /// Offset after the chunk, where the next call reads from.
    pub(crate) next: i64,
    /// Whether the call ended at its wait without new output.
    pub(crate) timed_out: bool,
}

/// The `ok` payload of `session output --json`.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct OutputPayload {
    /// Identifier of the session the chunk belongs to.
    session_id: String,
    /// Identifier of the worker instance the chunk belongs to.
    worker_instance_id: String,
    /// Runtime generation the chunk belongs to, as decimal text.
    runtime_generation: String,
    /// Offset of the first byte of history, which must read `0`.
    history_start_offset: String,
    /// Offset the chunk starts at, as decimal text.
    start_offset: String,
    /// Offset after the chunk, as decimal text.
    next_offset: String,
    /// Offset of the end of the runtime history, as decimal text.
    runtime_end_offset: String,
    /// Bytes of the chunk, in standard base64.
    data_base64: String,
    /// Whether history follows the chunk.
    has_more: bool,
    /// Whether the call ended at its wait without new output.
    timed_out: bool,
}

autoreview_go_compat::object_only_serde!(OutputPayload);

/// The envelope of `session output --json`, read for its payload.
#[derive(Debug, Deserialize)]
struct OutputEnvelope {
    /// The payload; `None` when the `ok` value is `null`.
    ok: Option<OutputPayload>,
}

/// The envelope of a call, read only for whether its `ok` key is present.
#[derive(Debug, Deserialize)]
struct PresentOk {
    /// The `ok` value, kept as present when it is `null`.
    #[serde(default, deserialize_with = "present")]
    ok: Option<IgnoredAny>,
}

/// One session of `session list --json`, read for its identifier only.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct ListedSession {
    /// Identifier of the session.
    id: String,
}

autoreview_go_compat::object_only_serde!(ListedSession);

/// Reads any JSON value as present, keeping `null` apart from a missing key.
fn present<'de, D>(deserializer: D) -> Result<Option<IgnoredAny>, D::Error>
where
    D: Deserializer<'de>,
{
    IgnoredAny::deserialize(deserializer).map(Some)
}

/// Reports whether `output` holds an `ok` key, whose value may be `null`.
fn has_ok(output: &str) -> bool {
    decode_strict::<PresentOk>(output).is_some_and(|envelope| envelope.ok.is_some())
}

/// Returns the `review_output_invalid` error.
fn review_output_invalid() -> Error {
    Error::workflow("review_output_invalid", true, None)
}

/// Returns the arguments of `session output` for the chunk that starts at `after`.
pub(crate) fn output_arguments(
    session: &SessionIdentity,
    after: i64,
    max_bytes: i64,
    wait_ms: i64,
) -> Vec<String> {
    vec![
        "session".to_owned(),
        "output".to_owned(),
        session.session_id.clone(),
        "--host".to_owned(),
        session.host.clone(),
        "--worker-instance-id".to_owned(),
        session.worker_instance_id.clone(),
        "--runtime-generation".to_owned(),
        session.runtime_generation.clone(),
        "--after-offset".to_owned(),
        after.to_string(),
        "--max-bytes".to_owned(),
        max_bytes.to_string(),
        "--wait-ms".to_owned(),
        wait_ms.to_string(),
        "--json".to_owned(),
    ]
}

/// Returns the arguments of `session rm` for `session`.
///
/// The host follows the session identifier, in the order of `cmd/autoreview/pohunek.go:319`.
pub(crate) fn remove_arguments(session: &SessionIdentity) -> Vec<String> {
    vec![
        "session".to_owned(),
        "rm".to_owned(),
        session.session_id.clone(),
        "--host".to_owned(),
        session.host.clone(),
        "--json".to_owned(),
    ]
}

/// Returns the arguments of the session list that confirms the removal of `session`.
pub(crate) fn list_arguments(session: &SessionIdentity) -> Vec<String> {
    let mut arguments = session_list_arguments(&session.host);
    arguments.push("--filter".to_owned());
    arguments.push(format!("id={}", session.session_id));
    arguments
}

/// Reports whether `output` holds the final frame of a reviewer of `protocol`.
///
/// A Claude reviewer is complete once its output is one JSON value or ends with a newline.
/// Invalid UTF-8 cannot change the verdict, because a replacement character is valid inside a
/// string and invalid outside one, as the raw bytes are.
pub(crate) fn output_complete(protocol: Protocol, output: &[u8]) -> bool {
    match protocol {
        Protocol::ClaudeStructured => {
            let text = String::from_utf8_lossy(output);
            serde_json::from_str::<IgnoredAny>(text.trim()).is_ok() || output.ends_with(b"\n")
        }
        Protocol::CodexEnvelope => output
            .windows(CODEX_EXIT_PREFIX.len())
            .any(|window| window == CODEX_EXIT_PREFIX),
        Protocol::Unset => false,
    }
}

/// Returns the `ok` payload of a `session output` envelope.
///
/// A missing `ok` key is `pohunek_response_invalid`. Any other value that is not a payload, a
/// `null` included, is `review_output_invalid`, as Go reports it.
fn output_payload(output: &str) -> Result<OutputPayload, Error> {
    if !has_ok(output) {
        return Err(response_invalid());
    }
    decode_strict::<OutputEnvelope>(output)
        .and_then(|envelope| envelope.ok)
        .ok_or_else(review_output_invalid)
}

/// Decodes the chunk that a `session output` call returned for the read at `after`.
///
/// A chunk that ends past `max_bytes` is `review_output_too_large`. Every other refusal is
/// `review_output_invalid`, or `pohunek_response_invalid` when the envelope has no `ok` key.
pub(crate) fn decode_output_chunk(
    session: &SessionIdentity,
    after: i64,
    max_bytes: i64,
    output: &str,
) -> Result<OutputChunk, Error> {
    let payload = output_payload(output)?;
    if payload.session_id != session.session_id
        || payload.worker_instance_id != session.worker_instance_id
        || payload.runtime_generation != session.runtime_generation
        || payload.history_start_offset != "0"
    {
        return Err(review_output_invalid());
    }
    let start = payload
        .start_offset
        .parse::<i64>()
        .map_err(|_| review_output_invalid())?;
    let next = payload
        .next_offset
        .parse::<i64>()
        .map_err(|_| review_output_invalid())?;
    let end = payload
        .runtime_end_offset
        .parse::<i64>()
        .map_err(|_| review_output_invalid())?;
    let encoded: Vec<u8> = payload
        .data_base64
        .bytes()
        .filter(|byte| *byte != b'\r' && *byte != b'\n')
        .collect();
    let data = STANDARD
        .decode(encoded)
        .map_err(|_| review_output_invalid())?;
    let span = next.checked_sub(start);
    let length = i64::try_from(data.len()).ok();
    let valid = start == after
        && next >= start
        && end >= next
        && span.is_some()
        && length == span
        && payload.has_more == (next < end);
    if !valid {
        return Err(review_output_invalid());
    }
    if end > max_bytes {
        return Err(Error::workflow("review_output_too_large", true, None));
    }
    Ok(OutputChunk {
        data,
        next,
        timed_out: payload.timed_out,
    })
}

/// Reads a session output through `call` one chunk at a time.
///
/// Returns `None` while the reviewer still runs. `call` receives the argument list of each
/// `session output` call and returns its stdout; a failure of `call` is returned unchanged.
pub(crate) async fn session_output_with<C, F>(
    call: &C,
    session: &SessionIdentity,
    protocol: Protocol,
    limits: OutputLimits,
) -> Result<Option<Vec<u8>>, Error>
where
    C: Fn(Vec<String>) -> F,
    F: Future<Output = Result<String, Error>>,
{
    let mut collected: Vec<u8> = Vec::new();
    let mut after: i64 = 0;
    loop {
        let collected_length = i64::try_from(collected.len()).unwrap_or(i64::MAX);
        let remaining = limits.max_bytes.saturating_sub(collected_length);
        if remaining <= 0 {
            return Err(Error::workflow("review_output_too_large", true, None));
        }
        let arguments = output_arguments(
            session,
            after,
            min(limits.chunk_bytes, remaining),
            limits.wait_ms,
        );
        let stdout = call(arguments).await?;
        let chunk = decode_output_chunk(session, after, limits.max_bytes, &stdout)?;
        collected.extend(chunk.data);
        if output_complete(protocol, &collected) {
            return Ok(Some(collected));
        }
        if chunk.timed_out {
            return Ok(None);
        }
        after = chunk.next;
    }
}

/// Returns the complete output of the reviewer `session`, or `None` while it runs.
pub(crate) async fn session_output(
    config: &Config,
    cancel: &CancellationToken,
    session: &SessionIdentity,
    protocol: Protocol,
) -> Result<Option<Vec<u8>>, Error> {
    let limits = OutputLimits {
        max_bytes: config.output_max_bytes,
        chunk_bytes: config.output_chunk_bytes,
        wait_ms: config.output_wait_ms,
    };
    let call =
        |arguments: Vec<String>| async move { pohunek_call(config, cancel, &arguments).await };
    session_output_with(&call, session, protocol, limits).await
}

/// Removes `session` through `call`; success means it is gone or was already absent.
///
/// A reply without an `ok` key is `pohunek_response_invalid`.
pub(crate) async fn remove_with<C, F>(call: &C, session: &SessionIdentity) -> Result<(), Error>
where
    C: Fn(Vec<String>) -> F,
    F: Future<Output = Result<String, Error>>,
{
    match call(remove_arguments(session)).await {
        Ok(stdout) => {
            if has_ok(&stdout) {
                Ok(())
            } else {
                Err(response_invalid())
            }
        }
        Err(error) => {
            if error_code(&error) == SESSION_NOT_FOUND {
                Ok(())
            } else {
                Err(error)
            }
        }
    }
}

/// Fails while the session list in `output` still holds `session_id`.
pub(crate) fn cleanup_pending(output: &str, session_id: &str) -> Result<(), Error> {
    let listed: Vec<ListedSession> = decode_ok(output).ok_or_else(response_invalid)?;
    if listed.iter().any(|session| session.id == session_id) {
        return Err(Error::workflow(
            "pohunek_session_cleanup_pending",
            false,
            None,
        ));
    }
    Ok(())
}

/// Removes `session` through `call`, then confirms the removal through `call`.
pub(crate) async fn remove_session_confirmed_with<C, F>(
    call: &C,
    session: &SessionIdentity,
) -> Result<(), Error>
where
    C: Fn(Vec<String>) -> F,
    F: Future<Output = Result<String, Error>>,
{
    remove_with(call, session).await?;
    let output = call(list_arguments(session)).await?;
    cleanup_pending(&output, &session.session_id)
}

/// Removes `session` from its host and fails until the host no longer lists it.
pub(crate) async fn remove_session_confirmed(
    config: &Config,
    cancel: &CancellationToken,
    session: &SessionIdentity,
) -> Result<(), Error> {
    let call =
        |arguments: Vec<String>| async move { pohunek_call(config, cancel, &arguments).await };
    remove_session_confirmed_with(&call, session).await
}
