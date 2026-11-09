//! Discovery: the pohunek host and project that run the reviews of a repository.
//!
//! The hosts are tried in the order of `pohunek.hosts`; a host that fails is skipped.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeMap;
use std::future::Future;

use tokio_util::sync::CancellationToken;

use crate::config::RouteOverride;
use crate::engine::{Clock, Engine};
use crate::error::{Error, error_code};
use crate::integrations::Integrations;
use crate::model::ProjectRoute;

/// Error code of a repository that no answering host serves.
const PROJECT_ROUTE_NOT_FOUND: &str = "project_route_not_found";

/// Error code of a repository that two projects of one host serve.
const PROJECT_ROUTE_AMBIGUOUS: &str = "project_route_ambiguous";

/// Error codes of the hosts that failed in one discovery pass, keyed by host.
pub(crate) type HostFailures = BTreeMap<String, String>;

impl<I: Integrations, C: Clock> Engine<I, C> {
    /// Returns the pohunek host and project that run the reviews of `repository`.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`search_route`].
    pub(crate) async fn resolve_route(
        &self,
        cancel: &CancellationToken,
        repository: &str,
        sources: &[String],
        failures: &mut HostFailures,
    ) -> Result<ProjectRoute, Error> {
        search_route(
            &self.config.pohunek_hosts,
            &self.config.route_overrides,
            repository,
            cancel,
            failures,
            |host| {
                self.integration
                    .host_routes(cancel, host, repository, sources)
            },
        )
        .await
    }
}

/// Returns the route of the first host, in order, whose projects serve `repository`.
///
/// `list` returns the routes of one host. A host in `failures` is skipped without a call; a host
/// whose call fails is skipped and added to `failures`. Every skip logs one WARN line.
///
/// # Errors
///
/// Returns the error of the first skipped host when no later host serves the repository,
/// `project_route_not_found` when every host answered and none serves it,
/// `project_route_ambiguous` when two projects of the chosen host serve it, and the error of
/// `list` at once when `cancel` is cancelled.
pub(crate) async fn search_route<'a, F, Fut>(
    hosts: &'a [String],
    overrides: &BTreeMap<String, RouteOverride>,
    repository: &str,
    cancel: &CancellationToken,
    failures: &mut HostFailures,
    mut list: F,
) -> Result<ProjectRoute, Error>
where
    F: FnMut(&'a str) -> Fut,
    Fut: Future<Output = Result<Vec<ProjectRoute>, Error>>,
{
    let pinned = overrides.get(repository);
    let mut first_failure = None;
    for host in route_hosts(hosts, pinned) {
        let earlier = failures.get(host).cloned();
        let skipped_after_failure = earlier.is_some();
        let answer = match earlier {
            Some(code) => Err(Error::workflow(code, false, None)),
            None => list(host).await,
        };
        let routes = match answer {
            Ok(routes) => routes,
            Err(err) if cancel.is_cancelled() => return Err(err),
            Err(err) => {
                let code = error_code(&err).to_owned();
                tracing::warn!(
                    host,
                    repository,
                    error_code = code.as_str(),
                    skipped_after_failure,
                    "review host skipped"
                );
                failures.insert(host.to_owned(), code);
                first_failure.get_or_insert(err);
                continue;
            }
        };
        if let Some(route) = pick_route(pinned, routes)? {
            return Ok(route);
        }
    }
    Err(first_failure.unwrap_or_else(|| Error::workflow(PROJECT_ROUTE_NOT_FOUND, true, None)))
}

/// Returns the hosts to try, in order: the pinned host alone, or every configured host.
///
/// A pinned host that is not configured gives no host, so the route is not found.
fn route_hosts<'a>(hosts: &'a [String], pinned: Option<&RouteOverride>) -> Vec<&'a str> {
    hosts
        .iter()
        .filter(|host| pinned.is_none_or(|route| route.host == **host))
        .map(String::as_str)
        .collect()
}

/// Returns the one route of a host's `routes` that the pin allows, or `None` when there is none.
///
/// # Errors
///
/// Returns `project_route_ambiguous` when more than one route is left.
fn pick_route(
    pinned: Option<&RouteOverride>,
    mut routes: Vec<ProjectRoute>,
) -> Result<Option<ProjectRoute>, Error> {
    if let Some(route) = pinned {
        routes.retain(|candidate| {
            candidate.host == route.host && candidate.project_id == route.project_id
        });
    }
    if routes.len() > 1 {
        return Err(Error::workflow(PROJECT_ROUTE_AMBIGUOUS, true, None));
    }
    Ok(routes.pop())
}
