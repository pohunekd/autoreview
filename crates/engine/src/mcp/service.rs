//! The HTTP service of the MCP server: the path, the token and `Host` checks, then rmcp.

// Rust guideline compliant 2026-06-26

use std::convert::Infallible;
use std::fmt::Display;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use http::{HeaderValue, Request, Response, StatusCode};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Empty};
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio_util::sync::CancellationToken;

use super::access::{McpClient, McpConfig, authorize};
use super::host::AllowedHosts;
use super::tools::{Caller, Tools};
use crate::engine::{Clock, Engine};
use crate::integrations::Integrations;

/// The one path the server answers on; every other path is not found.
pub const MCP_PATH: &str = "/mcp";

/// The body of every HTTP response of the server.
pub type ResponseBody = BoxBody<Bytes, Infallible>;

/// The MCP server: the token check in front of the rmcp Streamable HTTP service.
pub struct McpServer<I, C> {
    /// The clients and their token digests.
    clients: Arc<[McpClient]>,
    /// The accepted `Host` values, checked as the metrics endpoint checks them.
    allowed_hosts: AllowedHosts,
    /// The rmcp service that runs the tools.
    inner: StreamableHttpService<Tools<I, C>, NeverSessionManager>,
    /// Longest time one request may take, from reading its body to its answer.
    request_timeout: Duration,
}

impl<I, C> McpServer<I, C>
where
    I: Integrations + Send + Sync + 'static,
    C: Clock + 'static,
{
    /// Creates the server on `engine`; `cancel` stops every request in flight.
    ///
    /// It serves without sessions and answers with plain JSON, accepts only the configured
    /// `Host` values and refuses every request that carries an `Origin` header. rmcp keeps
    /// its own `Host` check behind the shared one, so an rmcp upgrade can only refuse more.
    #[must_use]
    pub fn new(engine: Arc<Engine<I, C>>, config: &McpConfig, cancel: CancellationToken) -> Self {
        let settings = StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .with_json_response(true)
            .with_sse_keep_alive(None)
            .with_allowed_hosts(config.allowed_hosts.clone())
            .enforce_origin_validation()
            .with_max_request_body_bytes(config.limits.request_max_bytes)
            .with_cancellation_token(cancel);
        let inner = StreamableHttpService::new(
            move || Ok(Tools::new(Arc::clone(&engine))),
            Arc::new(NeverSessionManager::default()),
            settings,
        );
        Self {
            clients: config.clients.clone().into(),
            allowed_hosts: AllowedHosts::new(&config.allowed_hosts),
            inner,
            request_timeout: config.limits.request_timeout,
        }
    }

    /// Answers one HTTP request.
    ///
    /// A request to another path gets 404. A request without a known bearer token gets 401
    /// and never reaches a tool. The token is never logged and is removed before rmcp. Then a
    /// missing or malformed `Host` gets 400 and a `Host` that is not allowed gets 403. A
    /// request that takes longer than `mcp.request_timeout_seconds` gets 503; its tool call
    /// is cancelled.
    pub async fn handle<B>(&self, mut request: Request<B>) -> Response<ResponseBody>
    where
        B: http_body::Body + Send + 'static,
        B::Error: Display,
        B::Data: Send + 'static,
    {
        if request.uri().path() != MCP_PATH {
            return empty(StatusCode::NOT_FOUND);
        }
        let header = request
            .headers()
            .get(AUTHORIZATION)
            .map(HeaderValue::as_bytes);
        let client = match authorize(&self.clients, header) {
            Ok(client) => client,
            Err(refusal) => {
                tracing::warn!(reason = refusal.as_str(), "mcp request refused");
                let mut response = empty(StatusCode::UNAUTHORIZED);
                response
                    .headers_mut()
                    .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
                return response;
            }
        };
        if let Err(refusal) = self.allowed_hosts.check(request.uri(), request.headers()) {
            tracing::warn!(reason = refusal.as_str(), "mcp request refused");
            return empty(refusal.status());
        }
        // The tools never need the token, so it does not travel further into rmcp.
        request.headers_mut().remove(AUTHORIZATION);
        request.extensions_mut().insert(Caller {
            name: client.name.clone(),
            scope: client.scope,
        });
        match tokio::time::timeout(self.request_timeout, self.inner.handle(request)).await {
            Ok(response) => response,
            Err(_) => {
                tracing::warn!(client = %client.name, "mcp request timed out");
                empty(StatusCode::SERVICE_UNAVAILABLE)
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
