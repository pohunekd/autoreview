//! The listener of the metrics endpoint that `autoreview serve` runs beside the engine.

// Rust guideline compliant 2026-06-26

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use autoreview_engine::metrics::{MetricsConfig, MetricsServer};
use autoreview_engine::{BoxError, Engine, Error, SystemClock};
use autoreview_external::External;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// Error code of a metrics listen address that cannot be bound.
const METRICS_LISTEN_FAILED: &str = "metrics_listen_failed";

/// Pause after a failed accept, so that a full descriptor table does not spin the loop.
const ACCEPT_RETRY_PAUSE: Duration = Duration::from_millis(100);

/// Binds the listen address of the endpoint.
///
/// # Errors
///
/// Returns `metrics_listen_failed` when the address cannot be bound.
pub(crate) async fn bind(config: &MetricsConfig) -> Result<TcpListener, Error> {
    let listener = TcpListener::bind(config.listen).await.map_err(|error| {
        let cause: BoxError = error.into();
        Error::workflow(METRICS_LISTEN_FAILED, true, Some(cause))
    })?;
    tracing::info!(listen = %config.listen, "metrics endpoint listening");
    Ok(listener)
}

/// Serves `GET /metrics` on `listener` until `cancel` is cancelled.
///
/// At most `metrics.max_connections` connections run at once, each in its own task. On cancel
/// the listener stops and each connection finishes the request it serves; a scrape answers
/// from memory and one indexed store read, so the wait is short.
pub(crate) async fn run(
    listener: TcpListener,
    engine: Arc<Engine<External, SystemClock>>,
    config: MetricsConfig,
    cancel: CancellationToken,
) {
    let server = Arc::new(MetricsServer::new(engine, &config, cancel.clone()));
    let slots = Arc::new(Semaphore::new(config.max_connections));
    let connections = TaskTracker::new();
    loop {
        let permit = tokio::select! {
            () = cancel.cancelled() => break,
            permit = Arc::clone(&slots).acquire_owned() => permit,
        };
        // Nothing closes the semaphore, so this ends the loop only on a programmer error.
        let Ok(permit) = permit else {
            tracing::error!("metrics connection slots closed");
            break;
        };
        let accepted = tokio::select! {
            () = cancel.cancelled() => break,
            accepted = listener.accept() => accepted,
        };
        let stream = match accepted {
            Ok((stream, _)) => stream,
            Err(error) => {
                tracing::warn!(error = %error, "metrics connection not accepted");
                tokio::time::sleep(ACCEPT_RETRY_PAUSE).await;
                continue;
            }
        };
        let server = Arc::clone(&server);
        let cancel = cancel.clone();
        let header_timeout = config.header_timeout;
        connections.spawn(async move {
            let _permit = permit;
            let service = service_fn(move |request| {
                let server = Arc::clone(&server);
                async move { Ok::<_, Infallible>(server.handle(&request)) }
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
                tracing::debug!(error = %error, "metrics connection ended with an error");
            }
        });
    }
    connections.close();
    connections.wait().await;
}
