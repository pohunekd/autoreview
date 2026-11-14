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
