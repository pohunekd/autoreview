//! Notification backends: the registry by name, and Matrix messages and queue digests.
//!
//! Matrix requests are built by pure functions, so their room, transaction path and body are
//! checked without a homeserver. Sending is one PUT.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::time::Duration;

use autoreview_engine::config::{BackendKind, MatrixBackend, NotifySettings};
use autoreview_engine::error::Error;
use autoreview_engine::error_code;
use autoreview_policy::Level;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Client, StatusCode, Url};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

/// Prefix of the transaction IDs that the engine generates for its messages.
const TRANSACTION_PREFIX: &str = "notify-";

/// Random bytes behind a generated transaction ID, which gives 128 bits.
const TRANSACTION_RANDOM_BYTES: usize = 16;

/// Client API path of room events, placed after the homeserver URL.
const ROOM_EVENTS_PATH: &str = "/_matrix/client/v3/rooms/";

/// Path between the room and the transaction of a message event.
const SEND_MESSAGE_PATH: &str = "/send/m.room.message/";

/// Mark that ends a message cut to the length limit. It takes the last character of the limit.
const TRUNCATION_MARK: char = '…';

/// Message type of an action, digest or log message.
const TEXT_MESSAGE_TYPE: &str = "m.text";

/// Message type of a queue digest, which posts as a notice.
const NOTICE_MESSAGE_TYPE: &str = "m.notice";

/// Format of the HTML body of a queue digest.
const HTML_FORMAT: &str = "org.matrix.custom.html";

/// Cause of a transport failure. The transport's own text can carry the request URL.
const TRANSPORT_CAUSE: &str = "request failed";

/// Cause of a message limit below one character. Go panics on such a limit.
const LIMIT_CAUSE: &str = "matrix_max_characters must be at least 1";

/// Cause of a dot-segment path, which the URL parser would resolve.
const DOT_SEGMENT_CAUSE: &str = "path segment is a dot segment";

/// Code of a backend name that no configured backend has.
const BACKEND_UNKNOWN: &str = "notify_backend_unknown";

/// One configured backend, ready to deliver.
#[derive(Debug)]
pub(crate) enum Backend {
    /// Delivers nothing.
    None,
    /// Posts to one Matrix room.
    Matrix(Matrix),
}

/// The backends of the configuration, by name.
#[derive(Debug)]
pub(crate) struct Backends(BTreeMap<String, Backend>);

impl Backends {
    /// Creates the backends of `notify`; every message is bounded by the two limits.
    pub(crate) fn new(notify: &NotifySettings, max_characters: i64, timeout: Duration) -> Self {
        let backends = notify.backends.iter().map(|backend| {
            let ready = match &backend.kind {
                BackendKind::None => Backend::None,
                BackendKind::Matrix(matrix) => {
                    Backend::Matrix(Matrix::new(matrix, max_characters, timeout))
                }
            };
            (backend.name.clone(), ready)
        });
        Self(backends.collect())
    }

    /// Returns the backend called `name`.
    ///
    /// # Errors
    ///
    /// Returns `notify_backend_unknown` when no backend has that name.
    pub(crate) fn get(&self, name: &str) -> Result<&Backend, Error> {
        (self.0.get(name)).ok_or_else(|| Error::workflow(BACKEND_UNKNOWN, true, None))
    }
}

/// The settings of one Matrix backend.
pub(crate) struct Matrix {
    /// The homeserver URL; its trailing slashes are trimmed before the request path is joined.
    homeserver_url: String,
    /// The access token of the backend. It is sent as a bearer credential and never printed.
    access_token: String,
    /// The user that action messages mention.
    mention_user: String,
    /// The room that receives every message and the review queue digest.
    room: String,
    /// The largest message, in characters.
    max_characters: i64,
    /// The timeout of one request.
    timeout: Duration,
}

impl Matrix {
    /// Creates the settings of `backend` with the message limit and the request timeout.
    pub(crate) fn new(backend: &MatrixBackend, max_characters: i64, timeout: Duration) -> Self {
        Self {
            homeserver_url: backend.homeserver_url.clone(),
            access_token: backend.access_token.clone(),
            mention_user: backend.mention_user.clone(),
            room: backend.room.clone(),
            max_characters,
            timeout,
        }
    }
}

impl fmt::Debug for Matrix {
    /// Renders the settings without the access token, which must never reach the output.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Matrix")
            .field("homeserver_url", &self.homeserver_url)
            .field("mention_user", &self.mention_user)
            .field("room", &self.room)
            .field("max_characters", &self.max_characters)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

/// One message event: the room, the path of its transaction and the JSON content.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct MatrixRequest {
    /// The room that receives the event.
    pub(crate) room: String,
    /// The request path after the homeserver URL, with the room and the transaction escaped.
    pub(crate) txn_path: String,
    /// The event content as JSON, encoded the way Go's `json.Marshal` encodes it.
    pub(crate) content: Vec<u8>,
}

/// Content of an action, digest or log message. Fields are declared in Go's sorted key order.
#[derive(Serialize)]
struct TextContent<'a> {
    /// The message text, cut to the limit.
    body: &'a str,
    /// The users that the message mentions.
    #[serde(rename = "m.mentions")]
    mentions: Mentions<'a>,
    /// The message type, `m.text`.
    msgtype: &'static str,
}

/// Content of a queue digest. Fields are declared in Go's sorted key order.
#[derive(Serialize)]
struct NoticeContent<'a> {
    /// The plain-text body of the digest.
    body: &'a str,
    /// The format of `formatted_body`.
    format: &'static str,
    /// The HTML body of the digest.
    formatted_body: &'a str,
    /// The users that the digest mentions, which is nobody.
    #[serde(rename = "m.mentions")]
    mentions: Mentions<'a>,
    /// The message type, `m.notice`.
    msgtype: &'static str,
}

/// The `m.mentions` object. Without users it is an empty object, which mentions nobody.
#[derive(Serialize)]
struct Mentions<'a> {
    /// The mentioned users. The key is left out when no user is mentioned.
    #[serde(skip_serializing_if = "Option::is_none")]
    user_ids: Option<[&'a str; 1]>,
}

/// Returns a fresh transaction ID: `notify-` and 16 random bytes in lowercase hex.
///
/// A deterministic ID would make the homeserver drop legitimate repeats.
pub(crate) fn matrix_transaction_id() -> Result<String, Error> {
    let mut random = [0_u8; TRANSACTION_RANDOM_BYTES];
    getrandom::fill(&mut random)
        .map_err(|error| Error::workflow("matrix_request_invalid", true, Some(Box::new(error))))?;
    Ok(format!("{TRANSACTION_PREFIX}{}", hex::encode(random)))
}

/// Builds the request that sends a message of `level` to the room of `matrix`.
///
/// It is built as Go's `sendMatrix` builds it. Only action messages mention the user. A message
/// longer than the limit is cut to the limit minus one character, followed by
/// [`TRUNCATION_MARK`].
pub(crate) fn matrix_request(
    matrix: &Matrix,
    level: Level,
    message: &str,
    transaction: &str,
) -> Result<MatrixRequest, Error> {
    if message.trim().is_empty() {
        return Err(Error::workflow("matrix_message_invalid", true, None));
    }
    let body = truncated(message, matrix.max_characters)?;
    let mentions = Mentions {
        user_ids: if level == Level::Action {
            Some([matrix.mention_user.as_str()])
        } else {
            None
        },
    };
    let content = TextContent {
        body: &body,
        mentions,
        msgtype: TEXT_MESSAGE_TYPE,
    };
    build_request(&matrix.room, transaction, &content)
}

/// Builds the request that posts the review queue digest, as Go's `SendQueueDigest` builds it.
///
/// The digest goes to the room of `matrix` and mentions nobody. The caller chooses `transaction`.
pub(crate) fn queue_digest_request(
    matrix: &Matrix,
    transaction: &str,
    plain: &str,
    formatted: &str,
) -> Result<MatrixRequest, Error> {
    let content = NoticeContent {
        body: plain,
        format: HTML_FORMAT,
        formatted_body: formatted,
        mentions: Mentions { user_ids: None },
        msgtype: NOTICE_MESSAGE_TYPE,
    };
    build_request(&matrix.room, transaction, &content)
}

/// Returns `message` cut to `max_characters` as Go's `sendMatrix` cuts it.
///
/// A limit below one character is refused, because Go panics on it.
fn truncated(message: &str, max_characters: i64) -> Result<String, Error> {
    let limit = usize::try_from(max_characters)
        .ok()
        .filter(|limit| *limit >= 1)
        .ok_or_else(|| {
            Error::workflow(
                "matrix_request_invalid",
                true,
                Some(Box::new(io::Error::other(LIMIT_CAUSE))),
            )
        })?;
    if message.chars().count() <= limit {
        return Ok(message.to_owned());
    }
    let kept: String = message.chars().take(limit - 1).collect();
    Ok(format!("{kept}{TRUNCATION_MARK}"))
}

/// Builds the request of `content` in `room` for the transaction `transaction`.
///
/// A dot segment is refused, because the URL parser would resolve it.
fn build_request<T: Serialize>(
    room: &str,
    transaction: &str,
    content: &T,
) -> Result<MatrixRequest, Error> {
    if room == "." || room == ".." || transaction == "." || transaction == ".." {
        return Err(Error::workflow(
            "matrix_request_invalid",
            true,
            Some(Box::new(io::Error::other(DOT_SEGMENT_CAUSE))),
        ));
    }
    let txn_path = format!(
        "{ROOM_EVENTS_PATH}{}{SEND_MESSAGE_PATH}{}",
        path_escape(room),
        path_escape(transaction)
    );
    let content = encode(content)?;
    Ok(MatrixRequest {
        room: room.to_owned(),
        txn_path,
        content,
    })
}

/// Escapes `raw` as one path segment, as Go's `url.PathEscape` does.
///
/// Letters, digits and `-_.~$&+:=@` stay as they are. Every other byte of the UTF-8 text becomes
/// `%XX` with uppercase hex digits.
fn path_escape(raw: &str) -> String {
    let mut escaped = String::new();
    for byte in raw.bytes() {
        let keep = byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'-' | b'_' | b'.' | b'~' | b'$' | b'&' | b'+' | b':' | b'=' | b'@'
            );
        if keep {
            escaped.push(char::from(byte));
        } else {
            escaped.push_str(&format!("%{byte:02X}"));
        }
    }
    escaped
}

/// Encodes `content` as Go's `json.Marshal` does, which escapes `<`, `>`, `&`, U+2028 and U+2029.
///
/// serde writes those characters only inside strings, so escaping the whole text changes nothing
/// else.
fn encode<T: Serialize + ?Sized>(content: &T) -> Result<Vec<u8>, Error> {
    let text = serde_json::to_string(content)
        .map_err(|error| Error::workflow("matrix_request_invalid", true, Some(Box::new(error))))?;
    Ok(text
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
        .into_bytes())
}

/// Sends one message to the room of `matrix`, as Go's `sendMatrix` does.
///
/// Every failure is returned, at every level; [`notify`] drops the muted ones.
pub(crate) async fn send_matrix(
    client: &Client,
    matrix: &Matrix,
    cancel: &CancellationToken,
    level: Level,
    message: &str,
) -> Result<(), Error> {
    let transaction = matrix_transaction_id()?;
    let request = matrix_request(matrix, level, message, &transaction)?;
    put_matrix_message(client, matrix, cancel, request).await
}

/// Sends one event with a PUT and maps the answer to Go's error codes.
///
/// The response body is not read. A transport failure keeps only the fixed cause, because the
/// transport's error text can carry the request URL. A cancelled `cancel` is such a failure:
/// the request is not sent, or it is dropped in flight, as Go's request context does.
async fn put_matrix_message(
    client: &Client,
    matrix: &Matrix,
    cancel: &CancellationToken,
    request: MatrixRequest,
) -> Result<(), Error> {
    let endpoint = format!(
        "{}{}",
        matrix.homeserver_url.trim_end_matches('/'),
        request.txn_path
    );
    let url = Url::parse(&endpoint)
        .map_err(|error| Error::workflow("matrix_request_invalid", true, Some(Box::new(error))))?;
    let sent = client
        .put(url)
        .timeout(matrix.timeout)
        .header(CONTENT_TYPE, "application/json")
        .header(AUTHORIZATION, format!("Bearer {}", matrix.access_token))
        .body(request.content)
        .send();
    let response = cancel
        .run_until_cancelled(sent)
        .await
        .and_then(Result::ok)
        .ok_or_else(|| {
            Error::workflow(
                "matrix_unavailable",
                false,
                Some(Box::new(io::Error::other(TRANSPORT_CAUSE))),
            )
        })?;
    response_result(response.status())
}

/// Maps the status of an answer to the codes of Go's `putMatrixMessage`.
///
/// Too many requests and server errors can succeed later. Every other failure cannot.
fn response_result(status: StatusCode) -> Result<(), Error> {
    if status.is_success() {
        return Ok(());
    }
    if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
        return Err(Error::workflow("matrix_unavailable", false, None));
    }
    Err(Error::workflow("matrix_request_rejected", true, None))
}

/// Notifies the room of `matrix` at `level`, as Go's `Notify` does.
///
/// A level other than action drops a message the homeserver cannot take, whatever its route.
/// The drop is logged, so a muted notification never fails a review.
pub(crate) async fn notify(
    client: &Client,
    matrix: &Matrix,
    cancel: &CancellationToken,
    level: Level,
    message: &str,
) -> Result<(), Error> {
    match send_matrix(client, matrix, cancel, level.clone(), message).await {
        Ok(()) => Ok(()),
        Err(error) => drop_if_muted(level, error),
    }
}

/// Drops a message of a level other than action that the homeserver cannot take.
///
/// The drop is logged with its level and code; the caller's `notification` span adds the event
/// and the review key. Every other error is returned unchanged.
fn drop_if_muted(level: Level, error: Error) -> Result<(), Error> {
    let code = error_code(&error);
    let muted = code == "matrix_unavailable" || code == "matrix_request_rejected";
    if level == Level::Action || !muted {
        return Err(error);
    }
    tracing::warn!(
        level = level_name(&level),
        error_code = code,
        "matrix notification dropped"
    );
    Ok(())
}

/// Posts the review queue digest under `transaction`, as Go's `SendQueueDigest` does.
pub(crate) async fn send_queue_digest(
    client: &Client,
    matrix: &Matrix,
    cancel: &CancellationToken,
    transaction: &str,
    plain: &str,
    formatted: &str,
) -> Result<(), Error> {
    let request = queue_digest_request(matrix, transaction, plain, formatted)?;
    put_matrix_message(client, matrix, cancel, request).await
}

/// Returns the name of `level` as the log line carries it.
fn level_name(level: &Level) -> &str {
    match level {
        Level::Action => "action",
        Level::Digest => "digest",
        Level::Log => "log",
        Level::Other(text) => text,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use autoreview_engine::config::MatrixBackend;
    use autoreview_policy::Level;
    use serde_json::{Value, json};

    use super::{Matrix, matrix_request, queue_digest_request};

    /// Settings of one backend with synthetic values only.
    fn matrix() -> Matrix {
        let backend = MatrixBackend {
            homeserver_url: "https://matrix.example.org".to_owned(),
            access_token: "placeholder-token".to_owned(),
            room: "!queue:example.org".to_owned(),
            mention_user: "@example-user:example.org".to_owned(),
        };
        Matrix::new(&backend, 32_000, Duration::from_secs(30))
    }

    #[test]
    fn send_queue_digest_posts_without_mentions_on_its_own_room() {
        let request =
            queue_digest_request(&matrix(), "queue-2026-10-19T14", "plain", "<p>html</p>")
                .expect("digest request");
        assert_eq!(request.room, "!queue:example.org");
        assert_eq!(
            request.txn_path,
            "/_matrix/client/v3/rooms/%21queue:example.org/send/m.room.message/queue-2026-10-19T14"
        );
        let payload: Value = serde_json::from_slice(&request.content).expect("JSON content");
        assert_eq!(payload["m.mentions"], json!({}));
        assert_eq!(payload["msgtype"], "m.notice");
        assert_eq!(payload["format"], "org.matrix.custom.html");
        assert_eq!(payload["formatted_body"], "<p>html</p>");
        assert_eq!(payload["body"], "plain");
    }

    #[test]
    fn send_matrix_still_mentions_only_on_action_level() {
        let matrix = matrix();
        let action_route = NotifyRoute::Level(Level::Action);
        let digest_route = NotifyRoute::Level(Level::Digest);
        let action = matrix_request(
            &matrix,
            &action_route,
            Level::Action,
            "action",
            "notify-test",
        )
        .expect("action request");
        let digest = matrix_request(
            &matrix,
            &digest_route,
            Level::Digest,
            "digest",
            "notify-test",
        )
        .expect("digest request");
        assert_eq!(action.room, "!action:example.org");
        assert_eq!(digest.room, "!digest:example.org");
        let action_payload: Value =
            serde_json::from_slice(&action.content).expect("action JSON content");
        let digest_payload: Value =
            serde_json::from_slice(&digest.content).expect("digest JSON content");
        assert_eq!(
            action_payload["m.mentions"]["user_ids"],
            json!(["@example-user:example.org"])
        );
        assert_eq!(digest_payload["m.mentions"], json!({}));
    }
}
