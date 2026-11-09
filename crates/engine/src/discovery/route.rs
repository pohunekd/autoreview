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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::future::ready;

    use tokio_util::sync::CancellationToken;

    use super::{HostFailures, search_route};
    use crate::config::RouteOverride;
    use crate::error::{Error, error_code};
    use crate::model::ProjectRoute;

    /// Repository that every case routes.
    const REPOSITORY: &str = "acme/widget";

    /// The answer of one host: its project ids, or the code its call fails with.
    type Answer = Result<&'static [&'static str], &'static str>;

    /// The answers of the hosts of one case, keyed by host.
    type Answers = Vec<(&'static str, Answer)>;

    /// The route of a search, or its error code.
    type Outcome = Result<ProjectRoute, String>;

    /// Hosts, their answers, the expected outcome and the expected calls of one case.
    type Case = (
        &'static [&'static str],
        Answers,
        Outcome,
        &'static [&'static str],
    );

    /// Returns the route of `project` on `host`.
    fn route(host: &str, project: &str) -> ProjectRoute {
        ProjectRoute {
            host: host.to_owned(),
            project_id: project.to_owned(),
        }
    }

    /// Searches `hosts` with the given answers and returns the result and the hosts called.
    async fn search(
        hosts: &[&str],
        answers: &[(&str, Answer)],
        overrides: &BTreeMap<String, RouteOverride>,
        failures: &mut HostFailures,
        cancel: &CancellationToken,
    ) -> (Result<ProjectRoute, Error>, Vec<String>) {
        let hosts: Vec<String> = hosts.iter().map(|host| (*host).to_owned()).collect();
        let answers: BTreeMap<&str, Answer> = answers.iter().copied().collect();
        let mut called = Vec::new();
        let result = search_route(&hosts, overrides, REPOSITORY, cancel, failures, |host| {
            called.push(host.to_owned());
            let answer = answers.get(host).copied().unwrap_or(Ok(&[]));
            ready(match answer {
                Ok(projects) => Ok(projects.iter().map(|id| route(host, id)).collect()),
                Err(code) => Err(Error::workflow(code, false, None)),
            })
        })
        .await;
        (result, called)
    }

    /// Returns the route, or the error code, of a search result.
    fn outcome(result: &Result<ProjectRoute, Error>) -> Outcome {
        match result {
            Ok(route) => Ok(route.clone()),
            Err(err) => Err(error_code(err).to_owned()),
        }
    }

    #[tokio::test]
    async fn hosts_are_tried_in_order_and_a_failing_host_is_skipped() {
        let none = BTreeMap::new();
        let cancel = CancellationToken::new();
        // (hosts, answers, expected outcome, expected calls)
        let cases: Vec<Case> = vec![
            (
                &["a", "b"],
                vec![("a", Ok(&["p1"])), ("b", Ok(&["p2"]))],
                Ok(route("a", "p1")),
                &["a"],
            ),
            (
                &["a", "b", "c"],
                vec![("a", Err("pohunek_request_timeout")), ("b", Ok(&["p2"]))],
                Ok(route("b", "p2")),
                &["a", "b"],
            ),
            (
                &["a", "b", "c"],
                vec![
                    ("a", Ok(&[])),
                    ("b", Err("pohunek_request_failed")),
                    ("c", Ok(&["p3"])),
                ],
                Ok(route("c", "p3")),
                &["a", "b", "c"],
            ),
            (
                &["a", "b"],
                vec![("a", Err("pohunek_request_timeout")), ("b", Ok(&[]))],
                Err("pohunek_request_timeout".to_owned()),
                &["a", "b"],
            ),
            (
                &["a", "b"],
                vec![
                    ("a", Err("pohunek_request_timeout")),
                    ("b", Err("pohunek_request_failed")),
                ],
                Err("pohunek_request_timeout".to_owned()),
                &["a", "b"],
            ),
            (
                &["a", "b"],
                vec![],
                Err("project_route_not_found".to_owned()),
                &["a", "b"],
            ),
            (
                &["a", "b"],
                vec![("a", Ok(&["p1", "p2"])), ("b", Ok(&["p3"]))],
                Err("project_route_ambiguous".to_owned()),
                &["a"],
            ),
        ];
        for (hosts, answers, want, want_called) in cases {
            let mut failures = HostFailures::new();
            let (result, called) = search(hosts, &answers, &none, &mut failures, &cancel).await;
            assert_eq!(
                outcome(&result),
                want,
                "hosts={hosts:?} answers={answers:?}"
            );
            assert_eq!(called, want_called, "hosts={hosts:?} answers={answers:?}");
            for (host, answer) in &answers {
                let failed = answer.is_err() && called.iter().any(|item| item == host);
                assert_eq!(
                    failures.contains_key(*host),
                    failed,
                    "failures={failures:?} hosts={hosts:?} answers={answers:?}"
                );
            }
        }
    }

    #[tokio::test]
    async fn host_that_failed_earlier_in_the_pass_is_not_called_again() {
        let none = BTreeMap::new();
        let cancel = CancellationToken::new();
        let mut failures = HostFailures::new();
        failures.insert("a".to_owned(), "pohunek_request_timeout".to_owned());
        let answers = [("a", Ok(&["p1"][..])), ("b", Ok(&["p2"][..]))];
        let (result, called) = search(&["a", "b"], &answers, &none, &mut failures, &cancel).await;
        assert_eq!(outcome(&result), Ok(route("b", "p2")));
        assert_eq!(called, ["b"]);
        let answers = [("a", Ok(&["p1"][..])), ("b", Ok(&[][..]))];
        let (result, called) = search(&["a", "b"], &answers, &none, &mut failures, &cancel).await;
        assert_eq!(outcome(&result), Err("pohunek_request_timeout".to_owned()));
        assert_eq!(called, ["b"]);
    }

    #[tokio::test]
    async fn route_override_asks_only_its_host_and_never_falls_back() {
        let cancel = CancellationToken::new();
        let mut overrides = BTreeMap::new();
        overrides.insert(
            REPOSITORY.to_owned(),
            RouteOverride {
                host: "b".to_owned(),
                project_id: "p2".to_owned(),
            },
        );
        // (answers, expected outcome)
        let cases: Vec<(Answers, Outcome)> = vec![
            (
                vec![("a", Ok(&["p1"])), ("b", Ok(&["p1", "p2"]))],
                Ok(route("b", "p2")),
            ),
            (
                vec![("a", Ok(&["p1"])), ("b", Ok(&["p1"]))],
                Err("project_route_not_found".to_owned()),
            ),
            (
                vec![("a", Ok(&["p2"])), ("b", Err("pohunek_request_failed"))],
                Err("pohunek_request_failed".to_owned()),
            ),
        ];
        for (answers, want) in cases {
            let mut failures = HostFailures::new();
            let (result, called) =
                search(&["a", "b"], &answers, &overrides, &mut failures, &cancel).await;
            assert_eq!(outcome(&result), want, "answers={answers:?}");
            assert_eq!(called, ["b"], "answers={answers:?}");
        }
        let mut failures = HostFailures::new();
        let answers = [("a", Ok(&["p2"][..]))];
        let (result, called) = search(&["a"], &answers, &overrides, &mut failures, &cancel).await;
        assert_eq!(outcome(&result), Err("project_route_not_found".to_owned()));
        assert!(called.is_empty(), "called={called:?}");
    }

    #[tokio::test]
    async fn cancelled_search_returns_the_failure_without_skipping() {
        let none = BTreeMap::new();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut failures = HostFailures::new();
        let answers = [("a", Err("pohunek_request_failed")), ("b", Ok(&["p2"][..]))];
        let (result, called) = search(&["a", "b"], &answers, &none, &mut failures, &cancel).await;
        assert_eq!(outcome(&result), Err("pohunek_request_failed".to_owned()));
        assert_eq!(called, ["a"]);
        assert!(failures.is_empty(), "failures={failures:?}");
    }
}
