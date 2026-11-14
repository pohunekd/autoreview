//! Webhook backends: the body of each format and one POST per message.
//!
//! Bodies are built by a pure function, so each format is checked without a service. The URL
//! is a secret: it is never part of an error, a log line or the `Debug` output.

// Rust guideline compliant 2026-11-14

use std::fmt;
use std::io;
use std::time::Duration;

use autoreview_engine::config::{WebhookBackend, WebhookFormat};
use autoreview_engine::error::Error;
use reqwest::header::CONTENT_TYPE;
use reqwest::{Client, StatusCode, Url};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

/// Code of a body or URL that cannot be sent.
const REQUEST_INVALID: &str = "webhook_request_invalid";

/// Code of a service that is unreachable, rate limited or failing; it can succeed later.
pub(crate) const UNAVAILABLE: &str = "webhook_unavailable";

/// Code of a message that the service refused.
pub(crate) const REJECTED: &str = "webhook_request_rejected";

/// Cause of a transport failure. The transport's own text can carry the URL.
const TRANSPORT_CAUSE: &str = "request failed";

/// Cause of a message limit below one character.
const LIMIT_CAUSE: &str = "max_characters must be at least 1";

/// Cause of an empty or blank message.
const EMPTY_CAUSE: &str = "message is empty";

/// Largest Discord message content, in UTF-16 code units.
const DISCORD_CONTENT_MAX: usize = 2000;

/// Mark that ends a message cut to the length limit. It takes the last character of the limit.
const TRUNCATION_MARK: char = '…';

/// The settings of one webhook backend.
pub(crate) struct Webhook {
    /// The webhook URL; it is sent only as the request target and never printed.
    url: String,
    /// The body format.
    format: WebhookFormat,
    /// The largest message, in characters.
    max_characters: i64,
    /// The timeout of one request.
    timeout: Duration,
}

impl Webhook {
    /// Creates the settings of `backend` with the message limit and the request timeout.
    pub(crate) fn new(backend: &WebhookBackend, max_characters: i64, timeout: Duration) -> Self {
        Self {
            url: backend.url.clone(),
            format: backend.format,
            max_characters,
            timeout,
        }
    }
}

impl fmt::Debug for Webhook {
    /// Renders the settings without the URL.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Webhook")
            .field("format", &self.format)
            .field("max_characters", &self.max_characters)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

/// The body of the `json` format.
#[derive(Serialize)]
struct JsonBody<'a> {
    /// The event name, or `review_queue` for the queue.
    event: &'a str,
    /// The level of the event; left out for the queue, which has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    level: Option<&'a str>,
    /// The plain text, cut to the limit.
    text: &'a str,
}

/// The body of the `slack` format.
#[derive(Serialize)]
struct SlackBody<'a> {
    /// The text with `&`, `<` and `>` escaped, so it cannot form a mention or a link.
    text: &'a str,
}

/// The body of the `discord` format.
#[derive(Serialize)]
struct DiscordBody<'a> {
    /// The text, cut to the limit and to Discord's content limit.
    content: &'a str,
    /// The mentions that Discord may resolve, which are none.
    allowed_mentions: AllowedMentions,
}

/// Discord's `allowed_mentions` object; an empty `parse` list resolves no mention.
#[derive(Serialize)]
struct AllowedMentions {
    /// The mention types to resolve, always empty.
    parse: [&'static str; 0],
}

/// Returns the body of one message of `event` at `level` in the format of `webhook`.
///
/// The text is cut to `max_characters` first, then to the format's own limit. No format
/// mentions anyone.
///
/// # Errors
///
/// Returns `webhook_request_invalid` for a blank text or a limit below one character.
pub(crate) fn webhook_body(
    webhook: &Webhook,
    event: &str,
    level: Option<&str>,
    text: &str,
) -> Result<Vec<u8>, Error> {
    if text.trim().is_empty() {
        return Err(invalid(EMPTY_CAUSE));
    }
    let limit = usize::try_from(webhook.max_characters)
        .ok()
        .filter(|limit| *limit >= 1)
        .ok_or_else(|| invalid(LIMIT_CAUSE))?;
    let text = cut(text, limit, |_| 1);
    let encoded = match webhook.format {
        WebhookFormat::Json => serde_json::to_vec(&JsonBody {
            event,
            level,
            text: &text,
        }),
        WebhookFormat::Slack => serde_json::to_vec(&SlackBody {
            text: &slack_escaped(&text),
        }),
        WebhookFormat::Discord => serde_json::to_vec(&DiscordBody {
            content: &cut(&text, DISCORD_CONTENT_MAX, char::len_utf16),
            allowed_mentions: AllowedMentions { parse: [] },
        }),
    };
    encoded.map_err(|error| Error::workflow(REQUEST_INVALID, true, Some(Box::new(error))))
}

/// Returns `text` cut to `limit` units, where `units` sizes each character.
///
/// A cut text keeps what fits with [`TRUNCATION_MARK`] and ends in it.
fn cut(text: &str, limit: usize, units: fn(char) -> usize) -> String {
    if text.chars().map(units).sum::<usize>() <= limit {
        return text.to_owned();
    }
    let mut kept = String::new();
    let mut used = units(TRUNCATION_MARK);
    for character in text.chars() {
        used += units(character);
        if used > limit {
            break;
        }
        kept.push(character);
    }
    kept.push(TRUNCATION_MARK);
    kept
}

/// Escapes the three characters that Slack reads as markup: `&`, `<` and `>`.
fn slack_escaped(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Returns `webhook_request_invalid` with a fixed cause.
fn invalid(cause: &'static str) -> Error {
    Error::workflow(
        REQUEST_INVALID,
        true,
        Some(Box::new(io::Error::other(cause))),
    )
}

/// Posts `body` to the URL of `webhook` and maps the answer to an error code.
///
/// The response body is not read. A transport failure keeps only a fixed cause, and a URL that
/// does not parse has no cause, so the URL never reaches an error. A cancelled `cancel` is a
/// transport failure: the request is not sent, or it is dropped in flight.
pub(crate) async fn post_webhook(
    client: &Client,
    webhook: &Webhook,
    cancel: &CancellationToken,
    body: Vec<u8>,
) -> Result<(), Error> {
    let url = Url::parse(&webhook.url).map_err(|_| Error::workflow(REQUEST_INVALID, true, None))?;
    let sent = client
        .post(url)
        .timeout(webhook.timeout)
        .header(CONTENT_TYPE, "application/json")
        .body(body)
        .send();
    let response = cancel
        .run_until_cancelled(sent)
        .await
        .and_then(Result::ok)
        .ok_or_else(|| {
            Error::workflow(
                UNAVAILABLE,
                false,
                Some(Box::new(io::Error::other(TRANSPORT_CAUSE))),
            )
        })?;
    response_result(response.status())
}

/// Maps the status of an answer: success, a failure that can pass, or a refusal.
///
/// Too many requests and server errors can succeed later. Every other failure cannot.
fn response_result(status: StatusCode) -> Result<(), Error> {
    if status.is_success() {
        return Ok(());
    }
    if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
        return Err(Error::workflow(UNAVAILABLE, false, None));
    }
    Err(Error::workflow(REJECTED, true, None))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use autoreview_engine::config::{WebhookBackend, WebhookFormat};
    use autoreview_engine::error_code;
    use serde_json::{Value, json};

    use super::{Webhook, webhook_body};

    /// A webhook of `format` with a synthetic URL and a message limit of `max_characters`.
    fn webhook(format: WebhookFormat, max_characters: i64) -> Webhook {
        let backend = WebhookBackend {
            url: "https://hooks.example.org/placeholder".to_owned(),
            format,
        };
        Webhook::new(&backend, max_characters, Duration::from_secs(30))
    }

    #[test]
    fn webhook_bodies_follow_their_format() {
        use WebhookFormat::{Discord, Json, Slack};
        let long = "a".repeat(2500);
        let wide = "\u{1F600}".repeat(1500);
        let cases = [
            (
                Json,
                32_000,
                "approved",
                Some("digest"),
                "Approved <a&b>",
                json!({
                    "event": "approved", "level": "digest", "text": "Approved <a&b>"
                }),
            ),
            (
                Json,
                32_000,
                "review_queue",
                None,
                "queue",
                json!({
                    "event": "review_queue", "text": "queue"
                }),
            ),
            (
                Json,
                5,
                "started",
                Some("log"),
                "abcdefgh",
                json!({
                    "event": "started", "level": "log", "text": "abcd\u{2026}"
                }),
            ),
            (
                Slack,
                32_000,
                "approved",
                Some("action"),
                "<!channel> & <@U1> > x",
                json!({
                    "text": "&lt;!channel&gt; &amp; &lt;@U1&gt; &gt; x"
                }),
            ),
            (
                Slack,
                4,
                "approved",
                Some("action"),
                "<<<<<",
                json!({
                    "text": "&lt;&lt;&lt;\u{2026}"
                }),
            ),
            (
                Discord,
                32_000,
                "approved",
                Some("action"),
                "@everyone <@1> hi",
                json!({
                    "content": "@everyone <@1> hi", "allowed_mentions": {"parse": []}
                }),
            ),
            (
                Discord,
                32_000,
                "approved",
                Some("action"),
                long.as_str(),
                json!({
                    "content": format!("{}\u{2026}", "a".repeat(1999)),
                    "allowed_mentions": {"parse": []}
                }),
            ),
            (
                Discord,
                32_000,
                "approved",
                Some("action"),
                wide.as_str(),
                json!({
                    "content": format!("{}\u{2026}", "\u{1F600}".repeat(999)),
                    "allowed_mentions": {"parse": []}
                }),
            ),
        ];
        for (format, limit, event, level, text, expected) in cases {
            let body = webhook_body(&webhook(format, limit), event, level, text).expect("a body");
            let body: Value = serde_json::from_slice(&body).expect("a JSON body");
            assert_eq!(body, expected, "{format:?} {limit} {text:.20}");
        }
        for (text, limit) in [(" \n", 32_000), ("text", 0)] {
            let refused = webhook_body(&webhook(Json, limit), "approved", None, text)
                .expect_err("the body is refused");
            assert_eq!(
                error_code(&refused),
                "webhook_request_invalid",
                "{text:?} {limit}"
            );
        }
    }
}
