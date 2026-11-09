//! The listener of `autoreview mcp`: HTTP/1.1 connections served by the engine's MCP server.

// Rust guideline compliant 2026-06-26

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use autoreview_engine::mcp::{McpConfig, McpServer};
use autoreview_engine::{BoxError, Engine, Error, SystemClock};
use autoreview_external::External;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

/// Error code of a listen address that cannot be bound.
const MCP_LISTEN_FAILED: &str = "mcp_listen_failed";

/// Time a client has to send the request headers, so an idle connection cannot hold a task.
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Pause after a failed accept, so that a full descriptor table does not spin the loop.
const ACCEPT_RETRY_PAUSE: Duration = Duration::from_millis(100);

/// Serves the MCP tools on `config.listen` until `cancel` is cancelled.
///
/// Each connection runs in its own task. On cancel the listener stops at once and rmcp
/// cancels the tool calls in flight; a store write is one transaction, so none is half done.
///
/// # Errors
///
/// Returns `mcp_listen_failed` when the address cannot be bound.
pub(crate) async fn serve(
    engine: Arc<Engine<External, SystemClock>>,
    config: &McpConfig,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    let listener = TcpListener::bind(config.listen)
        .await
        .map_err(|error| listen_failed(error.into()))?;
    tracing::info!(
        listen = %config.listen,
        clients = config.clients.len(),
        "mcp server listening"
    );
    let server = Arc::new(McpServer::new(engine, config, cancel.child_token()));
    loop {
        let accepted = tokio::select! {
            () = cancel.cancelled() => return Ok(()),
            accepted = listener.accept() => accepted,
        };
        let stream = match accepted {
            Ok((stream, _)) => stream,
            Err(error) => {
                tracing::warn!(error = %error, "mcp connection not accepted");
                tokio::time::sleep(ACCEPT_RETRY_PAUSE).await;
                continue;
            }
        };
        let server = Arc::clone(&server);
        let cancel = cancel.clone();
        tokio::spawn(async move {
            let service = service_fn(move |request| {
                let server = Arc::clone(&server);
                async move { Ok::<_, Infallible>(server.handle(request).await) }
            });
            let mut builder = http1::Builder::new();
            builder
                .timer(TokioTimer::new())
                .header_read_timeout(HEADER_READ_TIMEOUT);
            let connection = builder.serve_connection(TokioIo::new(stream), service);
            tokio::pin!(connection);
            let result = tokio::select! {
                result = connection.as_mut() => result,
                () = cancel.cancelled() => {
                    connection.as_mut().graceful_shutdown();
                    connection.await
                }
            };
            if let Err(error) = result {
                tracing::debug!(error = %error, "mcp connection ended with an error");
            }
        });
    }
}

/// Wraps the bind error of the listen address.
fn listen_failed(cause: BoxError) -> Error {
    Error::workflow(MCP_LISTEN_FAILED, true, Some(cause))
}
