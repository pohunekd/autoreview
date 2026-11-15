//! Discovery: the pohunek host and project that run the reviews of a repository.
//!
//! The hosts are tried in the order of `pohunek.hosts`, or in the order of the host-state rules;
//! a host that fails is skipped.

// Rust guideline compliant 2026-06-26

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;

use autoreview_schedule::scheduler::retryable;
use tokio_util::sync::CancellationToken;

use super::host_state::{HOST_STATE_INELIGIBLE, HostVerdicts, state_hosts};
use crate::config::RouteOverride;
use crate::engine::{Clock, Engine, is_permanent};
use crate::error::{Error, error_code};
use crate::integrations::Integrations;
use crate::model::ProjectRoute;

/// Error code of a repository that no answering host serves.
const PROJECT_ROUTE_NOT_FOUND: &str = "project_route_not_found";

/// Error code of a repository that two projects of one host serve.
const PROJECT_ROUTE_AMBIGUOUS: &str = "project_route_ambiguous";

/// Error codes after which a review waits for a host: the codes of a failed pohunek call, of a
/// project list that does not read, and of a review that the state of no host allows.
const HOST_FAILURE_CODES: [&str; 5] = [
    "pohunek_request_failed",
    "pohunek_request_timeout",
    "pohunek_session_not_found",
    "pohunek_response_invalid",
    HOST_STATE_INELIGIBLE,
];

/// Reports whether a route error comes from a host that failed or that the state rules refused.
///
/// After such an error the review never started on a host, so it waits for one instead of
/// using its retries. Any other code, such as `project_route_not_found`, follows the
/// scheduler's retry policy, so an unknown code never waits without end.
pub(crate) fn host_unavailable(err: &Error) -> bool {
    HOST_FAILURE_CODES.contains(&error_code(err))
}

/// The failure of one host's project list, kept for the rest of a discovery pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostFailure {
    /// Error code of the failure.
    pub(crate) code: String,
    /// Whether the failure was marked permanent.
    pub(crate) permanent: bool,
}

/// The hosts that failed in one discovery pass, keyed by host.
pub(crate) type HostFailures = BTreeMap<String, HostFailure>;

/// The hosts whose project list succeeded in one discovery pass.
pub(crate) type HostAnswers = BTreeSet<String>;

/// What one discovery pass learned about the review hosts.
#[derive(Debug, Default)]
pub(crate) struct HostPass {
    /// The hosts whose project list failed.
    pub(crate) failures: HostFailures,
    /// The hosts whose project list succeeded.
    pub(crate) answers: HostAnswers,
    /// The verdict of the host-state rules on each host whose state was read.
    pub(crate) verdicts: HostVerdicts,
}

impl<I: Integrations, C: Clock> Engine<I, C> {
    /// Returns the pohunek host and project that run the reviews of `repository`.
    ///
    /// With host-state rules, a repository without a route override tries only the hosts the
    /// rules allow, in their order, and asks the refused hosts only when none of them serves
    /// the repository; each refused host logs one WARN line.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`state_hosts`] and [`search_state_route`].
    pub(crate) async fn resolve_route(
        &self,
        cancel: &CancellationToken,
        repository: &str,
        sources: &[String],
        pass: &mut HostPass,
    ) -> Result<ProjectRoute, Error> {
        let configured = &self.config.pohunek_hosts;
        let rules = self.config.host_state.as_ref();
        let (hosts, refused) = match rules {
            Some(rules) if !self.config.route_overrides.contains_key(repository) => {
                let read_timeout = rules.read_timeout;
                let checked = state_hosts(
                    configured,
                    rules,
                    self.clock.wall_now(),
                    cancel,
                    &mut pass.verdicts,
                    &mut pass.failures,
                    |host| self.integration.host_capacity(cancel, host, read_timeout),
                )
                .await?;
                for (host, rule) in &checked.refused {
                    tracing::warn!(
                        host = host.as_str(),
                        repository,
                        error_code = HOST_STATE_INELIGIBLE,
                        rule,
                        "review host skipped"
                    );
                }
                let refused = checked.refused.into_iter().map(|(host, _)| host).collect();
                (checked.eligible, refused)
            }
            _ => (configured.clone(), Vec::new()),
        };
        search_state_route(
            &hosts,
            &refused,
            &self.config.route_overrides,
            repository,
            cancel,
            pass,
            |host| {
                self.integration
                    .host_routes(cancel, host, repository, sources)
            },
        )
        .await
    }

    /// Asks each stored outage host that this pass has not asked.
    ///
    /// A stored outage ends only on a pass in which its host answers, and a host is asked only
    /// by a due review, so a host that recovers while no review waits is asked here. With no
    /// project source no project matches, so the call only checks that the host answers. A host
    /// that answers is added to `answers`; a host that fails logs one WARN line.
    ///
    /// # Errors
    ///
    /// Returns the error of a call at once when `cancel` is cancelled.
    pub(crate) async fn probe_outage_hosts(
        &self,
        cancel: &CancellationToken,
        outages: &BTreeMap<String, String>,
        failures: &HostFailures,
        answers: &mut HostAnswers,
    ) -> Result<(), Error> {
        for host in outages.keys() {
            if !self.config.pohunek_hosts.contains(host)
                || failures.contains_key(host)
                || answers.contains(host)
            {
                continue;
            }
            match self.integration.host_routes(cancel, host, "", &[]).await {
                Ok(_) => {
                    tracing::info!(host = host.as_str(), "review host answers again");
                    answers.insert(host.clone());
                }
                Err(err) if cancel.is_cancelled() => return Err(err),
                Err(err) => {
                    tracing::warn!(
                        host = host.as_str(),
                        error_code = error_code(&err),
                        "review host still unavailable"
                    );
                }
            }
        }
        Ok(())
    }

    /// Returns the hosts that `repository` may use and that are in `failures`, in order.
    pub(crate) fn failed_route_hosts(
        &self,
        repository: &str,
        failures: &HostFailures,
    ) -> Vec<String> {
        route_hosts(
            &self.config.pohunek_hosts,
            self.config.route_overrides.get(repository),
        )
        .into_iter()
        .filter(|host| failures.contains_key(*host))
        .map(str::to_owned)
        .collect()
    }
}

/// Returns the route of the first host, in order, whose projects serve `repository`.
///
/// `list` returns the routes of one host. A host in the pass's failures is skipped without a
/// call; a host whose call fails is skipped and added to them, and a host whose call succeeds is
/// added to the pass's answers. Every skip logs one WARN line.
///
/// # Errors
///
/// When no later host serves the repository, returns the first skipped host's failure whose
/// code the scheduler retries, else the first skipped host's failure; returns
/// `project_route_not_found` when every host answered and none serves it,
/// `project_route_ambiguous` when two projects of the chosen host serve it, and the error of
/// `list` at once when `cancel` is cancelled.
pub(crate) async fn search_route<'a, F, Fut>(
    hosts: &'a [String],
    overrides: &BTreeMap<String, RouteOverride>,
    repository: &str,
    cancel: &CancellationToken,
    pass: &mut HostPass,
    mut list: F,
) -> Result<ProjectRoute, Error>
where
    F: FnMut(&'a str) -> Fut,
    Fut: Future<Output = Result<Vec<ProjectRoute>, Error>>,
{
    let pinned = overrides.get(repository);
    let mut first_failure = None;
    let mut first_retryable = None;
    for host in route_hosts(hosts, pinned) {
        let earlier = pass.failures.get(host).cloned();
        let skipped_after_failure = earlier.is_some();
        let answer = match earlier {
            Some(failure) => Err(Error::workflow(failure.code, failure.permanent, None)),
            None => list(host).await,
        };
        let routes = match answer {
            Ok(routes) => {
                pass.answers.insert(host.to_owned());
                routes
            }
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
                let failure = HostFailure {
                    code,
                    permanent: is_permanent(&err),
                };
                pass.failures.insert(host.to_owned(), failure);
                if first_retryable.is_none() && retryable(error_code(&err)) {
                    first_retryable = Some(err);
                } else {
                    first_failure.get_or_insert(err);
                }
                continue;
            }
        };
        if let Some(route) = pick_route(pinned, routes)? {
            return Ok(route);
        }
    }
    Err(first_retryable
        .or(first_failure)
        .unwrap_or_else(|| Error::workflow(PROJECT_ROUTE_NOT_FOUND, true, None)))
}

/// Returns the route of the first allowed host, in order, whose projects serve `repository`.
///
/// `hosts` are the hosts that the host-state rules allow and `refused` the hosts they held
/// back. When no allowed host serves the repository, the refused hosts are asked for their
/// projects the same way, so a repository that no host serves is not found.
///
/// # Errors
///
/// Returns `host_state_ineligible` when only a refused host serves `repository`, else the
/// errors of [`search_route`] over `hosts`, then over `refused` when no allowed host failed.
pub(crate) async fn search_state_route<'a, F, Fut>(
    hosts: &'a [String],
    refused: &'a [String],
    overrides: &BTreeMap<String, RouteOverride>,
    repository: &str,
    cancel: &CancellationToken,
    pass: &mut HostPass,
    mut list: F,
) -> Result<ProjectRoute, Error>
where
    F: FnMut(&'a str) -> Fut,
    Fut: Future<Output = Result<Vec<ProjectRoute>, Error>>,
{
    match search_route(hosts, overrides, repository, cancel, pass, &mut list).await {
        Err(err) if !refused.is_empty() && error_code(&err) == PROJECT_ROUTE_NOT_FOUND => {
            match search_route(refused, overrides, repository, cancel, pass, &mut list).await {
                // The review waits until the state of a host that serves it allows it.
                Ok(_) => Err(Error::workflow(HOST_STATE_INELIGIBLE, false, None)),
                Err(err) => Err(err),
            }
        }
        found => found,
    }
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

#[cfg(test)]
mod tests {
    use autoreview_schedule::scheduler::retryable;

    use super::host_unavailable;
    use crate::error::Error;

    #[test]
    fn only_a_host_failure_makes_a_review_wait_for_a_host() {
        // (error code, waits for a host)
        let cases = [
            ("pohunek_request_failed", true),
            ("pohunek_request_timeout", true),
            ("pohunek_session_not_found", true),
            ("pohunek_response_invalid", true),
            ("host_state_ineligible", true),
            ("project_route_not_found", false),
            ("project_route_ambiguous", false),
            ("pohunek_session_lost", false),
            ("workflow_failed", false),
        ];
        for (code, want) in cases {
            let err = Error::workflow(code, !retryable(code), None);
            assert_eq!(host_unavailable(&err), want, "code={code}");
        }
    }
}
