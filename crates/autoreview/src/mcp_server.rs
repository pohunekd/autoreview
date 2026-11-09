//! The listener of `autoreview mcp`: HTTP/1.1 connections served by the engine's MCP server.

// Rust guideline compliant 2026-06-26

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use autoreview_engine::mcp::{McpConfig, McpServer};
use autoreview_engine::{BoxError, Engine, Error, SystemClock, WORKFLOW_FAILED};
use autoreview_external::External;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// Error code of a listen address that cannot be bound.
const MCP_LISTEN_FAILED: &str = "mcp_listen_failed";

/// Pause after a failed accept, so that a full descriptor table does not spin the loop.
const ACCEPT_RETRY_PAUSE: Duration = Duration::from_millis(100);

/// Serves the MCP tools on `config.listen` until `cancel` is cancelled.
///
/// At most `mcp.max_connections` connections run at once, each in its own task. On cancel
/// the listener stops, each connection finishes its request in flight, and the server waits
/// for them up to `mcp.request_timeout_seconds` before it cancels what is left.
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
    // Requests in flight are cancelled only after the drain, not at the signal.
    let requests = CancellationToken::new();
    let server = Arc::new(McpServer::new(engine, config, requests.clone()));
    let slots = Arc::new(Semaphore::new(config.limits.max_connections));
    let connections = TaskTracker::new();
    let header_timeout = config.limits.header_timeout;
    loop {
        let permit = tokio::select! {
            () = cancel.cancelled() => break,
            permit = Arc::clone(&slots).acquire_owned() => permit.map_err(failed)?,
        };
        let accepted = tokio::select! {
            () = cancel.cancelled() => break,
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
        connections.spawn(async move {
            let _permit = permit;
            let service = service_fn(move |request| {
                let server = Arc::clone(&server);
                async move { Ok::<_, Infallible>(server.handle(request).await) }
            });
            let mut builder = http1::Builder::new();
            builder
                .timer(TokioTimer::new())
                .header_read_timeout(header_timeout);
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
    connections.close();
    let drain = tokio::time::timeout(config.limits.request_timeout, connections.wait()).await;
    if drain.is_err() {
        tracing::warn!(
            open = connections.len(),
            "mcp connections still open after the drain"
        );
    }
    requests.cancel();
    Ok(())
}

/// Wraps the bind error of the listen address.
fn listen_failed(cause: BoxError) -> Error {
    Error::workflow(MCP_LISTEN_FAILED, true, Some(cause))
}

/// Wraps a failure that carries no engine code, as the other commands report it.
fn failed(cause: impl Into<BoxError>) -> Error {
    Error::workflow(WORKFLOW_FAILED, false, Some(cause.into()))
}
