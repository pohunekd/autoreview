//! Discovery: the discovery lock, notifications and the rejected pull requests.
//!
//! Mirrors `cmd/autoreview/discovery.go:15-64, 197-202`.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeMap;
use std::future::Future;

use autoreview_policy::{Event, Level, Policy};
use autoreview_schedule::scheduler::Record;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::engine::{Clock, Engine};
use crate::error::{Error, error_code};
use crate::integrations::Integrations;
use crate::model::RejectedCandidate;
use crate::store::DiscoveryState;

pub(crate) mod commands;
pub(crate) mod plan;
pub(crate) mod poll;
pub(crate) mod records;

/// Error code of an engine whose configuration lacks a usable policy.
const CONFIGURATION_INVALID: &str = "configuration_invalid";

/// Configuration key named by the error of a missing policy.
const POLICY_KEY: &str = "policy_file";

/// Schedule phase of a review that waits for its next retry.
const PHASE_RETRY_WAIT: &str = "retry_wait";

/// A notification of a discovery pass, sent once the pass has saved its state.
#[derive(Debug)]
pub(crate) struct Notice {
    /// Event that the notification reports.
    pub(crate) event: Event,
    /// Key `repository#number` of the pull request that the notification is about.
    pub(crate) review_key: String,
    /// Level whose room receives the notification.
    pub(crate) level: Level,
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

    /// Sends each notice once, after the discovery lock is released.
    ///
    /// A notice gets one attempt, bounded by the notifier's request timeout. After a failure,
    /// the later notices of the same level get no attempt, because the notifier routes by
    /// level. Every dropped notice is logged, so a notifier outage never fails a pass.
    pub(crate) async fn send_notices(&self, cancel: &CancellationToken, notices: Vec<Notice>) {
        // The level and error code of each level whose notice failed in this batch.
        let mut failed: Vec<(Level, String)> = Vec::new();
        for notice in notices {
            let earlier = failed
                .iter()
                .find(|(level, _)| *level == notice.level)
                .map(|(_, code)| code.clone());
            let (code, skipped) = match earlier {
                Some(code) => (code, true),
                None => {
                    let span = notification_span(&notice.event, &notice.review_key, &notice.level);
                    let sent = self
                        .integration
                        .notify(cancel, notice.level.clone(), &notice.message)
                        .instrument(span)
                        .await;
                    let Err(err) = sent else {
                        continue;
                    };
                    let code = error_code(&err).to_owned();
                    failed.push((notice.level.clone(), code.clone()));
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
        Ok(Notice {
            event,
            review_key: review_key.to_owned(),
            level,
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
            let message = rejected_message(&key, &item.code);
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
    if record.phase == PHASE_RETRY_WAIT {
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
