//! Discovery: the discovery lock, notifications and the rejected pull requests.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeMap;
use std::future::Future;

use autoreview_policy::{Event, Level, Policy};
use autoreview_schedule::phase;
use autoreview_schedule::scheduler::Record;
use tokio_util::sync::CancellationToken;

use crate::config::{Credential, NotifyRoute};
use crate::engine::{Clock, Engine, retry_step};
use crate::error::{Error, error_code};
use crate::integrations::Integrations;
use crate::model::RejectedCandidate;
use crate::store::DiscoveryState;

pub(crate) mod authors;
pub(crate) mod commands;
pub(crate) mod host_state;
pub(crate) mod plan;
pub(crate) mod poll;
pub(crate) mod records;
pub(crate) mod route;

/// Error code of an engine whose configuration lacks a usable policy.
const CONFIGURATION_INVALID: &str = "configuration_invalid";

/// Configuration key named by the error of a missing policy.
const POLICY_KEY: &str = "policy_file";

/// Error code logged for a notification that a shutdown left unsent.
pub(crate) const SHUTDOWN: &str = "shutdown";

/// A notification of a discovery pass, sent once the pass has saved its state.
#[derive(Debug)]
pub(crate) struct Notice {
    /// Event that the notification reports.
    pub(crate) event: Event,
    /// Key `repository#number` of the pull request that the notification is about.
    pub(crate) review_key: String,
    /// Level of the notification, which decides its mention.
    pub(crate) level: Level,
    /// Route whose backends receive the notification.
    pub(crate) route: NotifyRoute,
    /// Text of the notification.
    pub(crate) message: String,
}

impl<I: Integrations, C: Clock> Engine<I, C> {
    /// Runs `run` while this engine holds the discovery lock.
    ///
    /// The timeout and the poll interval come from the configuration.
    ///
    /// # Errors
    ///
    /// Returns `discovery_busy`, `store_unavailable` or the error of `run`.
    pub(crate) async fn with_discovery<T, F, Fut>(&self, run: F) -> Result<T, Error>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, Error>>,
    {
        self.store
            .with_discovery_lock(
                self.config.discovery_lock_timeout,
                self.config.discovery_lock_poll,
                run,
            )
            .await
    }

    /// Runs `search` once per credential, each with the step retries of `step`.
    ///
    /// A credential whose search fails is logged with its name and error code and left out, so
    /// the others go on; the returned map holds its code by its name.
    ///
    /// # Errors
    ///
    /// Returns the first error when every credential fails, or the error after a cancel.
    pub(crate) async fn search_each<'a, T, Fut>(
        &'a self,
        step: &str,
        cancel: &CancellationToken,
        mut search: impl FnMut(&'a Credential) -> Fut,
    ) -> Result<(Vec<(&'a Credential, T)>, BTreeMap<String, String>), Error>
    where
        Fut: Future<Output = Result<T, Error>>,
    {
        let mut found = Vec::new();
        let mut failed = BTreeMap::new();
        let mut first = None;
        for credential in self.config.credentials.iter() {
            let policy = self.step_policy();
            let searched = retry_step(step, policy, &self.clock, cancel, || search(credential));
            match searched.await {
                Ok(value) => found.push((credential, value)),
                Err(err) if cancel.is_cancelled() => return Err(err),
                Err(err) => {
                    tracing::error!(
                        credential = %credential.name,
                        error_code = error_code(&err),
                        step,
                        "search of a credential failed, its repositories wait for the next pass"
                    );
                    failed.insert(credential.name.clone(), error_code(&err).to_owned());
                    first.get_or_insert(err);
                }
            }
        }
        match first {
            Some(err) if found.is_empty() => Err(err),
            _ => Ok((found, failed)),
        }
    }

    /// Reports whether a credential whose search failed in this pass serves `repository`.
    pub(crate) fn unsearched(&self, failed: &BTreeMap<String, String>, repository: &str) -> bool {
        !failed.is_empty()
            && (self.config.credentials.serving(repository))
                .is_ok_and(|credential| failed.contains_key(&credential.name))
    }

    /// Sends each notice once, after the discovery lock is released.
    ///
    /// A notice gets one attempt, bounded by the notifier's request timeout, and a failure is
    /// seen at every level. After a failure, the later notices of the same route get no
    /// attempt, because they reach the same backends. After `cancel`, no notice gets an
    /// attempt. Every dropped notice is logged, so a notifier outage never fails a pass.
    pub(crate) async fn send_notices(&self, cancel: &CancellationToken, notices: Vec<Notice>) {
        // The route and error code of each route whose notice failed in this batch.
        let mut failed: Vec<(NotifyRoute, String)> = Vec::new();
        for notice in notices {
            let earlier = failed
                .iter()
                .find(|(route, _)| *route == notice.route)
                .map(|(_, code)| code.clone());
            let (code, skipped) = match earlier {
                Some(code) => (code, true),
                // A shutdown is no notifier failure, so the notifier is not blamed for it.
                None if cancel.is_cancelled() => (SHUTDOWN.to_owned(), false),
                None => {
                    let sent = self
                        .integration
                        .notify_unmuted(
                            cancel,
                            notice.route.clone(),
                            notice.level.clone(),
                            &notice.message,
                        )
                        .await;
                    let Err(err) = sent else {
                        continue;
                    };
                    let code = error_code(&err).to_owned();
                    failed.push((notice.route.clone(), code.clone()));
                    (code, false)
                }
            };
            tracing::warn!(
                event = notice.event.as_str(),
                review_key = %notice.review_key,
                level = notice.level.as_str(),
                error_code = %code,
                skipped_after_failure = skipped,
                "discovery notification dropped"
            );
        }
    }

    /// Returns the notice of `event` about `review_key`, at the level of the policy.
    ///
    /// The route follows the configuration, see
    /// [`Config::notify_route`](crate::config::Config::notify_route).
    ///
    /// # Errors
    ///
    /// Returns `configuration_invalid` when the policy names no level for `event`.
    pub(crate) fn notice(
        &self,
        event: Event,
        review_key: &str,
        message: String,
    ) -> Result<Notice, Error> {
        let level = self.level(event.clone())?;
        let route = self.config.notify_route(&event, level.clone());
        Ok(Notice {
            event,
            review_key: review_key.to_owned(),
            level,
            route,
            message,
        })
    }

    /// Announces each refused pull request once per refusal code.
    ///
    /// The user has to look at it, because it is not reviewed. The notices go to `notices`, and
    /// the rejected map of `state` is replaced by the current refusals.
    ///
    /// # Errors
    ///
    /// Returns `configuration_invalid` without a policy level.
    pub(crate) fn report_rejected(
        &self,
        state: &mut DiscoveryState,
        rejected: &[RejectedCandidate],
        notices: &mut Vec<Notice>,
    ) -> Result<(), Error> {
        let mut current = BTreeMap::new();
        for item in rejected {
            let key = item.key();
            current.insert(key.clone(), item.code.clone());
            let reported = state.rejected.get(&key).map_or("", String::as_str);
            if reported == item.code {
                continue;
            }
            let message = match &item.permission_login {
                Some(login) => permission_message(&key, &item.code, &item.repository, login),
                None => rejected_message(&key, &item.code),
            };
            notices.push(self.notice(Event::ReviewStopped, &key, message)?);
        }
        state.rejected = current;
        Ok(())
    }

    /// Returns the review policy of the configuration.
    ///
    /// # Errors
    ///
    /// Returns `configuration_invalid` naming `policy_file` when the configuration has none.
    pub(crate) fn policy(&self) -> Result<&Policy, Error> {
        self.config
            .policy
            .as_ref()
            .ok_or_else(|| Error::config(CONFIGURATION_INVALID, POLICY_KEY, None))
    }

    /// Returns the notification level of `event` in the policy.
    ///
    /// # Errors
    ///
    /// Returns `configuration_invalid` naming `policy_file` without a policy or a level.
    pub(crate) fn level(&self, event: Event) -> Result<Level, Error> {
        self.policy()?
            .level(event)
            .ok_or_else(|| Error::config(CONFIGURATION_INVALID, POLICY_KEY, None))
    }
}

/// Returns the notification event of a schedule record.
///
/// A record that waits for a retry is `retry_scheduled`; any other is `review_stopped`.
pub(crate) fn record_event(record: &Record) -> Event {
    if record.phase == phase::RETRY_WAIT {
        Event::RetryScheduled
    } else {
        Event::ReviewStopped
    }
}

/// Returns the span of one notification, so the notifier's own log lines name it.
pub(crate) fn notification_span(event: &Event, review_key: &str, level: &Level) -> tracing::Span {
    tracing::info_span!(
        "notification",
        event = event.as_str(),
        review_key,
        level = level.as_str()
    )
}

/// Returns the notification of a pull request whose data the engine refuses.
fn rejected_message(key: &str, code: &str) -> String {
    format!(
        "Review for {key} is not started: GitHub returned data the review service refuses \
         ({code}). Check the pull request; it is reviewed once the data is valid."
    )
}

/// Returns the stop notice of a pull request whose author's permission could not be read.
///
/// It names the read to check with the engine's credential and holds no `gh` output.
fn permission_message(key: &str, code: &str, repository: &str, login: &str) -> String {
    format!(
        "Review for {key} is not started: reading the repository permission of its author \
         {login} failed ({code}). Check that the engine's GitHub token can read \
         gh api repos/{repository}/collaborators/{login}/permission; the permission is read \
         again on the next pass."
    )
}
