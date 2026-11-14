//! Notification backends and the routes from levels and events to them.

// Rust guideline compliant 2026-11-14

use std::collections::BTreeMap;
use std::fmt;

use autoreview_policy::{Event, Level};

/// One named entry of `notify.backends`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifyBackend {
    /// The name that the routes and the review queue refer to.
    pub name: String,
    /// The type of the backend with its settings.
    pub kind: BackendKind,
}

/// The type of a backend, which decides how a message is delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendKind {
    /// Delivers nothing; the structured log is the only record.
    None,
    /// Posts to one Matrix room.
    Matrix(MatrixBackend),
}

/// The settings of a `matrix` backend.
#[derive(Clone, PartialEq, Eq)]
pub struct MatrixBackend {
    /// The homeserver URL; HTTPS, or plain HTTP on the loopback address.
    pub homeserver_url: String,
    /// The access token; it never appears in debug output.
    pub access_token: String,
    /// The room that receives the messages.
    pub room: String,
    /// The Matrix user that action messages mention.
    pub mention_user: String,
}

impl fmt::Debug for MatrixBackend {
    /// Renders the settings without the access token.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MatrixBackend")
            .field("homeserver_url", &self.homeserver_url)
            .field("room", &self.room)
            .field("mention_user", &self.mention_user)
            .finish_non_exhaustive()
    }
}

/// The route that carries one notification to its backends.
///
/// The level of a notification still decides its mention and whether a failure is dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotifyRoute {
    /// The route of the level, `notify.routes.<level>`.
    Level(Level),
    /// The event's own route, `notify.routes.<event>`.
    Event(Event),
}

/// The notification backends, the routes to them and the backends of the review queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifySettings {
    /// Every configured backend, in file order.
    pub backends: Vec<NotifyBackend>,
    /// Backend names of the `action` level.
    pub action: Vec<String>,
    /// Backend names of the `digest` level.
    pub digest: Vec<String>,
    /// Backend names of the `log` level.
    pub log: Vec<String>,
    /// Backend names of each event with its own route; empty without event routes.
    pub events: BTreeMap<Event, Vec<String>>,
    /// Backend names that receive the review queue.
    pub queue: Vec<String>,
}

impl NotifySettings {
    /// Returns the backend names of `route`.
    ///
    /// A level other than the three and an event without its own route have none.
    pub fn backends_of(&self, route: &NotifyRoute) -> &[String] {
        match route {
            NotifyRoute::Level(Level::Action) => &self.action,
            NotifyRoute::Level(Level::Digest) => &self.digest,
            NotifyRoute::Level(Level::Log) => &self.log,
            NotifyRoute::Level(Level::Other(_)) => &[],
            NotifyRoute::Event(event) => self.events.get(event).map_or(&[], Vec::as_slice),
        }
    }

    /// Returns the backend called `name`, or `None` when no backend has that name.
    pub fn backend(&self, name: &str) -> Option<&NotifyBackend> {
        self.backends.iter().find(|backend| backend.name == name)
    }

    /// Returns one Matrix backend `matrix` that every level and the queue name.
    #[cfg(test)]
    pub(crate) fn single_for_tests() -> Self {
        let name = vec!["matrix".to_owned()];
        Self {
            backends: vec![NotifyBackend {
                name: "matrix".to_owned(),
                kind: BackendKind::Matrix(MatrixBackend {
                    homeserver_url: "https://matrix.example.org".to_owned(),
                    access_token: "placeholder-token".to_owned(),
                    room: "!room:example.org".to_owned(),
                    mention_user: "@example-user:example.org".to_owned(),
                }),
            }],
            action: name.clone(),
            digest: name.clone(),
            log: name.clone(),
            events: BTreeMap::new(),
            queue: name,
        }
    }
}
