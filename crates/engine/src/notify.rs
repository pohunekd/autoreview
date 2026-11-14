//! The channel-neutral notification and its delivery to the backends of a route.

// Rust guideline compliant 2026-11-14

use autoreview_policy::{Event, Level};
use tokio_util::sync::CancellationToken;

use crate::config::NotifyRoute;
use crate::engine::{Clock, Engine};
use crate::error::Error;
use crate::integrations::Integrations;

/// Code of a route that names no backend.
const ROUTE_INVALID: &str = "notify_route_invalid";

/// One notification, the same for every backend; each backend decides how to show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    /// The event that the notification reports.
    pub event: Event,
    /// The level of the event, which decides the mention and whether a failure is dropped.
    pub level: Level,
    /// The plain text that every backend shows; its first line is the title.
    pub text: String,
}

impl<I: Integrations, C: Clock> Engine<I, C> {
    /// Sends `notification` to the backends of `route`, in route order.
    ///
    /// The first failure stops the delivery and is returned. With `muted`, each backend drops
    /// the failures that its level allows, see [`Integrations::notify`].
    ///
    /// # Errors
    ///
    /// Returns `notify_route_invalid` when the route names no backend, and the error of the
    /// backend that failed.
    pub(crate) async fn deliver(
        &self,
        cancel: &CancellationToken,
        route: &NotifyRoute,
        notification: &Notification,
        muted: bool,
    ) -> Result<(), Error> {
        let backends = self.config.notify.backends_of(route);
        if backends.is_empty() {
            return Err(Error::workflow(ROUTE_INVALID, true, None));
        }
        for backend in backends {
            if muted {
                self.integration
                    .notify(cancel, backend, notification)
                    .await?;
            } else {
                let sent = self
                    .integration
                    .notify_unmuted(cancel, backend, notification);
                sent.await?;
            }
        }
        Ok(())
    }
}
