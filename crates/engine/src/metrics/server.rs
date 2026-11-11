//! The HTTP side of the metrics endpoint: its settings and `GET /metrics`.

// Rust guideline compliant 2026-06-26

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::header::{ALLOW, CONTENT_TYPE, ORIGIN};
use http::{HeaderValue, Method, Request, Response, StatusCode};
use http_body_util::{BodyExt, Empty, Full};
use tokio_util::sync::CancellationToken;

use crate::engine::{Clock, Engine};
use crate::error::{Error, error_code};
use crate::integrations::Integrations;
use crate::mcp::ResponseBody;
use crate::mcp::access::{MAX_CONNECTIONS_LIMIT, TIMEOUT_MAX, host_valid, invalid};
use crate::mcp::host::AllowedHosts;

/// The one path the endpoint answers on; every other path is not found.
pub const METRICS_PATH: &str = "/metrics";

/// Content type of the Prometheus text exposition format 0.0.4.
const CONTENT_TYPE_TEXT: &str = "text/plain; version=0.0.4; charset=utf-8";

/// Settings of the metrics endpoint of `autoreview serve`, the `metrics` section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricsConfig {
    /// Address the endpoint listens on.
    pub listen: SocketAddr,
    /// `Host` header values the endpoint accepts.
    pub allowed_hosts: Vec<String>,
    /// Longest time one connection may stay open; it serves one request.
    pub request_timeout: Duration,
    /// Longest time a client may take to send the request headers.
    pub header_timeout: Duration,
    /// Most connections served at once; further connections wait to be accepted.
    pub max_connections: usize,
}

impl MetricsConfig {
    /// Validates the `metrics` section with the rules of the `mcp` section.
    ///
    /// # Errors
    ///
    /// Returns `configuration_invalid` naming `metrics.listen` for an address that does not
    /// parse, `metrics.allowed_hosts` for an empty list or an entry that is not a bare host or
    /// `host:port`, and the key of a limit that is zero or above its bound.
    pub fn new(
        listen: &str,
        allowed_hosts: Vec<String>,
        request_timeout: Duration,
        header_timeout: Duration,
        max_connections: usize,
    ) -> Result<Self, Error> {
        let listen = listen
            .parse::<SocketAddr>()
            .map_err(|_| invalid("metrics.listen", "must be an IP address and port"))?;
        if allowed_hosts.is_empty() || !allowed_hosts.iter().all(|host| host_valid(host)) {
            return Err(invalid(
                "metrics.allowed_hosts",
                "must list hosts or host:port, without scheme, path or user",
            ));
        }
        let timeouts = [
            ("metrics.request_timeout_seconds", request_timeout),
            ("metrics.header_timeout_seconds", header_timeout),
        ];
        for (key, timeout) in timeouts {
            if timeout.is_zero() || timeout > TIMEOUT_MAX {
                return Err(invalid(key, "must be from 1 to 86400 seconds"));
            }
        }
        if !(1..=MAX_CONNECTIONS_LIMIT).contains(&max_connections) {
            return Err(invalid("metrics.max_connections", "must be from 1 to 4096"));
        }
        Ok(Self {
            listen,
            allowed_hosts,
            request_timeout,
            header_timeout,
            max_connections,
        })
    }
}

/// The metrics endpoint: the `Host` check in front of the exposition of the engine.
pub struct MetricsServer<I, C> {
    /// The engine whose store and counters the endpoint reads.
    engine: Arc<Engine<I, C>>,
    /// The accepted `Host` values, checked as the MCP server checks them.
    allowed_hosts: AllowedHosts,
    /// Stops the store reads of a scrape when the service shuts down.
    cancel: CancellationToken,
}

impl<I, C> MetricsServer<I, C>
where
    I: Integrations + Send + Sync + 'static,
    C: Clock + 'static,
{
    /// Creates the endpoint on `engine`; `cancel` stops the store reads of a scrape.
    #[must_use]
    pub fn new(
        engine: Arc<Engine<I, C>>,
        config: &MetricsConfig,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            engine,
            allowed_hosts: AllowedHosts::new(&config.allowed_hosts),
            cancel,
        }
    }

    /// Answers one HTTP request; only `GET /metrics` with an accepted `Host` gets the text.
    ///
    /// A missing or malformed `Host` gets 400 and a `Host` that is not allowed gets 403, as the
    /// MCP server answers. A request with an `Origin` header gets 403, so a browser page cannot
    /// read the endpoint. Another path gets 404 and another method 405. The store is read on
    /// a blocking thread, never on a runtime thread. A failed read gets 500 and is logged.
    pub async fn handle<B>(&self, request: Request<B>) -> Response<ResponseBody> {
        if let Err(refusal) = self.allowed_hosts.check(request.uri(), request.headers()) {
            tracing::warn!(reason = refusal.as_str(), "metrics request refused");
            return empty(refusal.status());
        }
        if request.headers().contains_key(ORIGIN) {
            tracing::warn!(reason = "origin_present", "metrics request refused");
            return empty(StatusCode::FORBIDDEN);
        }
        if request.uri().path() != METRICS_PATH {
            return empty(StatusCode::NOT_FOUND);
        }
        if request.method() != Method::GET {
            let mut response = empty(StatusCode::METHOD_NOT_ALLOWED);
            response
                .headers_mut()
                .insert(ALLOW, HeaderValue::from_static("GET"));
            return response;
        }
        let engine = Arc::clone(&self.engine);
        let cancel = self.cancel.clone();
        let read = tokio::task::spawn_blocking(move || engine.metrics_text(&cancel)).await;
        match read {
            Ok(Ok(text)) => {
                let mut response = Response::new(Full::new(Bytes::from(text)).boxed());
                response
                    .headers_mut()
                    .insert(CONTENT_TYPE, HeaderValue::from_static(CONTENT_TYPE_TEXT));
                response
            }
            Ok(Err(err)) => {
                tracing::error!(error_code = error_code(&err), "metrics not read");
                empty(StatusCode::INTERNAL_SERVER_ERROR)
            }
            Err(err) => {
                tracing::error!(error = %err, "metrics read did not finish");
                empty(StatusCode::INTERNAL_SERVER_ERROR)
            }
        }
    }
}

/// Returns an empty response with `status`.
fn empty(status: StatusCode) -> Response<ResponseBody> {
    let mut response = Response::new(Empty::<Bytes>::new().boxed());
    *response.status_mut() = status;
    response
}
