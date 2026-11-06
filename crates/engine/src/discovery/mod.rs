//! Discovery: the discovery lock, notifications and the rejected pull requests.
//!
//! Mirrors `cmd/autoreview/discovery.go:15-64, 197-202`.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeMap;
use std::future::Future;

use autoreview_policy::{Event, Level, Policy};
use autoreview_schedule::scheduler::Record;
use tokio_util::sync::CancellationToken;

use crate::engine::{Clock, Engine, retry_step};
use crate::error::Error;
use crate::integrations::Integrations;
use crate::model::RejectedCandidate;
use crate::store::DiscoveryState;

pub(crate) mod commands;
pub(crate) mod plan;
pub(crate) mod poll;

/// Error code of an engine whose configuration lacks a usable policy.
const CONFIGURATION_INVALID: &str = "configuration_invalid";

/// Configuration key named by the error of a missing policy.
const POLICY_KEY: &str = "policy_file";

/// Schedule phase of a review that waits for its next retry.
const PHASE_RETRY_WAIT: &str = "retry_wait";

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

    /// Sends `message` to the room of `level`, retried under the step policy.
    ///
    /// # Errors
    ///
    /// Returns the last error of the notification when the retries end.
    pub(crate) async fn notify_now(
        &self,
        cancel: &CancellationToken,
        level: Level,
        message: &str,
    ) -> Result<(), Error> {
        retry_step(self.step_policy(), &self.clock, cancel, || {
            self.integration.notify(level, message)
        })
        .await
    }

    /// Announces each refused pull request once per refusal code.
    ///
    /// The user has to look at it, because it is not reviewed. The rejected map of `state` is
    /// replaced by the current refusals only after every notification is sent.
    ///
    /// # Errors
    ///
    /// Returns `configuration_invalid` without a policy level, or the error of a notification.
    pub(crate) async fn report_rejected(
        &self,
        cancel: &CancellationToken,
        state: &mut DiscoveryState,
        rejected: &[RejectedCandidate],
    ) -> Result<(), Error> {
        let mut current = BTreeMap::new();
        for item in rejected {
            let key = item.key();
            current.insert(key.clone(), item.code.clone());
            let reported = state.rejected.get(&key).map_or("", String::as_str);
            if reported == item.code {
                continue;
            }
            let message = rejected_message(&key, &item.code);
            let level = self.level(Event::ReviewStopped)?;
            self.notify_now(cancel, level, &message).await?;
        }
        state.rejected = current;
        Ok(())
    }

    /// Returns the notification level of a schedule record.
    ///
    /// A record that waits for a retry is `retry_scheduled`; any other is `review_stopped`.
    ///
    /// # Errors
    ///
    /// Returns `configuration_invalid` when the policy names no level for the event.
    pub(crate) fn notify_level(&self, record: &Record) -> Result<Level, Error> {
        if record.phase == PHASE_RETRY_WAIT {
            self.level(Event::RetryScheduled)
        } else {
            self.level(Event::ReviewStopped)
        }
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

/// Returns the notification of a pull request whose data the engine refuses.
fn rejected_message(key: &str, code: &str) -> String {
    format!(
        "Review for {key} is not started: GitHub returned data the review service refuses \
         ({code}). Check the pull request; it is reviewed once the data is valid."
    )
}
