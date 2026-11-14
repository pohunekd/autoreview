//! Notification backends: the registry by name, delivery by type, and Matrix messages.
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
use autoreview_engine::notify::Notification;
use autoreview_policy::Level;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Client, StatusCode, Url};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use self::webhook::{Webhook, post_webhook, webhook_body, webhook_client};

mod webhook;

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

/// Cause of a message limit below one character.
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
    /// Posts to one incoming webhook.
    Webhook(Webhook),
}

/// The backends of the configuration, by name.
#[derive(Debug)]
pub(crate) struct Backends(BTreeMap<String, Backend>);

impl Backends {
    /// Creates the backends of `notify`; every message is bounded by the two limits.
    ///
    /// Every webhook shares one client from [`webhook_client`]; Matrix uses the caller's.
    ///
    /// # Errors
    ///
    /// Returns `webhook_unavailable` when the webhook client cannot be built.
    pub(crate) fn new(
        notify: &NotifySettings,
        max_characters: i64,
        timeout: Duration,
    ) -> Result<Self, Error> {
        let client = webhook_client(timeout)?;
        let backends = notify.backends.iter().map(|backend| {
            let ready = match &backend.kind {
                BackendKind::None => Backend::None,
                BackendKind::Matrix(matrix) => {
                    Backend::Matrix(Matrix::new(matrix, max_characters, timeout))
                }
                BackendKind::Webhook(webhook) => {
                    let client = client.clone();
                    Backend::Webhook(Webhook::new(webhook, max_characters, timeout, client))
                }
            };
            (backend.name.clone(), ready)
        });
        Ok(Self(backends.collect()))
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
    /// The event content as JSON, with `<`, `>`, `&`, U+2028 and U+2029 written as `\uXXXX`.
    pub(crate) content: Vec<u8>,
}

/// Content of an action, digest or log message. Keys serialize in alphabetical order.
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

/// Content of a queue digest. Keys serialize in alphabetical order.
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
/// Only action messages mention the user. A message
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

/// Builds the request that posts the review queue digest.
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

/// Returns `message` unchanged when it fits in `max_characters`, otherwise cut short with a
/// truncation mark.
///
/// A limit below one character is refused.
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

/// Escapes `raw` as one path segment.
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

/// Encodes `content` as JSON, writing `<`, `>`, `&`, U+2028 and U+2029 as `\uXXXX` escapes.
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

/// Sends one message to the room of `matrix`.
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

/// Sends one event with a PUT and maps the answer to error codes.
///
/// The response body is not read. A transport failure keeps only the fixed cause, because the
/// transport's error text can carry the request URL. A cancelled `cancel` is such a failure:
/// the request is not sent, or it is dropped in flight.
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

/// Maps the status of an answer to `matrix_unavailable` or `matrix_request_rejected`.
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

/// Notifies the room of `matrix` at `level`.
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

/// Posts the review queue digest under `transaction`.
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

/// Event name of the review queue in a webhook body.
const QUEUE_EVENT: &str = "review_queue";

/// Delivers `notification` to `backend`, which the routes call `name`.
///
/// A `none` backend only logs the message's event and level. With `muted`, a backend drops a
/// failure of a level other than action and logs the drop, as [`notify`] does for Matrix.
pub(crate) async fn deliver(
    client: &Client,
    name: &str,
    backend: &Backend,
    cancel: &CancellationToken,
    notification: &Notification,
    muted: bool,
) -> Result<(), Error> {
    let (level, text) = (notification.level.clone(), notification.text.as_str());
    match backend {
        Backend::None => {
            tracing::info!(
                backend = name,
                event = notification.event.as_str(),
                level = level_name(&level),
                "notification logged only"
            );
            Ok(())
        }
        Backend::Matrix(matrix) if muted => notify(client, matrix, cancel, level, text).await,
        Backend::Matrix(matrix) => send_matrix(client, matrix, cancel, level, text).await,
        Backend::Webhook(webhook) => {
            let event = notification.event.as_str();
            let body = webhook_body(webhook, event, Some(level_name(&level)), text)?;
            let sent = post_webhook(webhook, cancel, body).await;
            match sent {
                Err(error) if muted => drop_webhook_if_muted(level, error),
                sent => sent,
            }
        }
    }
}

/// Posts the review queue digest to `backend`; a `none` backend posts nothing.
///
/// Matrix takes both bodies under `transaction`; a webhook takes the plain body.
pub(crate) async fn deliver_queue(
    client: &Client,
    backend: &Backend,
    cancel: &CancellationToken,
    transaction: &str,
    plain: &str,
    formatted: &str,
) -> Result<(), Error> {
    match backend {
        Backend::None => Ok(()),
        Backend::Matrix(matrix) => {
            send_queue_digest(client, matrix, cancel, transaction, plain, formatted).await
        }
        Backend::Webhook(webhook) => {
            let body = webhook_body(webhook, QUEUE_EVENT, None, plain)?;
            post_webhook(webhook, cancel, body).await
        }
    }
}

/// Drops a webhook failure of a level other than action, as [`drop_if_muted`] does for Matrix.
fn drop_webhook_if_muted(level: Level, error: Error) -> Result<(), Error> {
    let code = error_code(&error);
    let muted = code == webhook::UNAVAILABLE || code == webhook::REJECTED;
    if level == Level::Action || !muted {
        return Err(error);
    }
    tracing::warn!(
        level = level_name(&level),
        error_code = code,
        "webhook notification dropped"
    );
    Ok(())
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

    use std::collections::BTreeMap;
    use std::io;
    use std::sync::{Arc, Mutex, PoisonError};

    use autoreview_engine::config::{
        BackendKind, MatrixBackend, NotifyBackend, NotifySettings, WebhookBackend, WebhookFormat,
    };
    use autoreview_engine::error_code;
    use autoreview_engine::notify::Notification;
    use autoreview_policy::{Event, Level};
    use reqwest::Client;
    use serde_json::{Value, json};
    use tokio_util::sync::CancellationToken;
    use tracing_subscriber::fmt::MakeWriter;

    use super::{Backends, Matrix, deliver, deliver_queue, matrix_request, queue_digest_request};

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
        let action = matrix_request(&matrix, Level::Action, "action", "notify-test")
            .expect("action request");
        let digest = matrix_request(&matrix, Level::Digest, "digest", "notify-test")
            .expect("digest request");
        assert_eq!(action.room, "!queue:example.org");
        assert_eq!(digest.room, "!queue:example.org");
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

    /// A log writer that keeps every line in memory.
    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Captured {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let mut lines = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            lines.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Captured {
        type Writer = Captured;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// Returns the error's text, its debug form and every cause.
    fn chain(error: &(dyn std::error::Error + 'static)) -> String {
        let mut text = format!("{error} {error:?}");
        let mut cause = error.source();
        while let Some(next) = cause {
            text.push_str(&format!(" {next} {next:?}"));
            cause = next.source();
        }
        text
    }

    // A redirect could carry the secret URL (`Referer`) or the body to another host;
    // reqwest's client `Debug` shows both settings.
    #[test]
    fn webhook_requests_follow_no_redirect_and_send_no_referer() {
        let notify = NotifySettings {
            backends: vec![NotifyBackend {
                name: "hook".to_owned(),
                kind: BackendKind::Webhook(WebhookBackend {
                    url: "https://hooks.example.org/placeholder".to_owned(),
                    format: WebhookFormat::Json,
                }),
            }],
            action: Vec::new(),
            digest: Vec::new(),
            log: Vec::new(),
            events: BTreeMap::new(),
            queue: Vec::new(),
        };
        let backends = Backends::new(&notify, 32_000, Duration::from_secs(30));
        let settings = format!("{:?}", backends.expect("the backends build"));
        assert!(
            settings.contains(r#"redirect_policy: "Policy(None)""#),
            "{settings}"
        );
        assert!(!settings.contains("referer"), "{settings}");
    }

    #[tokio::test]
    async fn webhook_url_never_reaches_errors_logs_or_debug() {
        const SECRET: &str = "secret-path-marker";
        let webhook = |name: &str, url: String, format| NotifyBackend {
            name: name.to_owned(),
            kind: BackendKind::Webhook(WebhookBackend { url, format }),
        };
        let notify = NotifySettings {
            backends: vec![
                webhook(
                    "hook",
                    format!("https://hooks.example.org/{SECRET}"),
                    WebhookFormat::Slack,
                ),
                webhook("broken", format!("{SECRET} is no URL"), WebhookFormat::Json),
                webhook(
                    "refused",
                    format!("https://127.0.0.1:1/{SECRET}"),
                    WebhookFormat::Discord,
                ),
                NotifyBackend {
                    name: "quiet".to_owned(),
                    kind: BackendKind::None,
                },
            ],
            action: Vec::new(),
            digest: Vec::new(),
            log: Vec::new(),
            events: BTreeMap::new(),
            queue: Vec::new(),
        };
        let backends = Backends::new(&notify, 32_000, Duration::from_secs(30));
        let backends = backends.expect("the backends build");
        let captured = Captured::default();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_writer(captured.clone())
            .finish();
        let _logs = tracing::subscriber::set_default(subscriber);
        // A cancelled token stops every request before it is sent, so nothing leaves the host.
        let (client, cancel) = (Client::new(), CancellationToken::new());
        cancel.cancel();
        let mut texts = vec![format!("{backends:?} {notify:?}")];
        let mut codes = Vec::new();
        let sends = [
            ("hook", Level::Action),
            ("hook", Level::Digest),
            ("broken", Level::Action),
            ("quiet", Level::Log),
        ];
        for (name, level) in sends {
            let backend = backends.get(name).expect("a configured backend");
            let notification = Notification {
                event: Event::Approved,
                level,
                text: "Approved".to_owned(),
            };
            let sent = deliver(&client, name, backend, &cancel, &notification, true).await;
            if let Err(error) = sent {
                codes.push(error_code(&error).to_owned());
                texts.push(chain(&error));
            }
        }
        let hook = backends.get("hook").expect("the hook backend");
        let queue = deliver_queue(&client, hook, &cancel, "queue-txn", "plain", "<p>plain</p>");
        let queue = queue.await.expect_err("the queue post fails");
        codes.push(error_code(&queue).to_owned());
        texts.push(chain(&queue));
        // A live token reaches the transport: port 1 on the host refuses the connection, so the
        // request fails in reqwest without leaving the host.
        let refused = backends.get("refused").expect("the refused backend");
        let notification = Notification {
            event: Event::Approved,
            level: Level::Action,
            text: "Approved".to_owned(),
        };
        let live = CancellationToken::new();
        let sent = deliver(&client, "refused", refused, &live, &notification, true).await;
        let sent = sent.expect_err("the connection is refused");
        codes.push(error_code(&sent).to_owned());
        texts.push(chain(&sent));
        let logs = captured
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let logs = String::from_utf8(logs).expect("UTF-8 logs");
        assert_eq!(
            codes,
            [
                "webhook_unavailable",
                "webhook_request_invalid",
                "webhook_unavailable",
                "webhook_unavailable"
            ]
        );
        assert!(logs.contains("webhook notification dropped"), "{logs}");
        assert!(logs.contains("notification logged only"), "{logs}");
        texts.push(logs);
        for text in texts {
            assert!(!text.contains(SECRET), "the URL leaked: {text}");
        }
    }
}
