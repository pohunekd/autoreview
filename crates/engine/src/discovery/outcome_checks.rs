//! Discovery: the outcome checks that end the open findings of merged and closed pull requests.

// Rust guideline compliant 2026-06-26

use std::collections::{BTreeMap, BTreeSet};

use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

use super::poll::live;
use crate::engine::{Clock, Engine};
use crate::error::{Error, error_code};
use crate::integrations::Integrations;
use crate::model::Candidate;

impl<I: Integrations, C: Clock> Engine<I, C> {
    /// Ends the open finding outcomes of the pull requests that were merged or closed.
    ///
    /// A pull request with open outcomes that is not in `present`, in a searched repository,
    /// gets one state read per `outcome_check_interval` with the credential that serves it.
    /// A merged or closed one ends its open outcomes at its head. An open one, one that
    /// GitHub does not return and a failed read change no outcome; after a failed read, its
    /// credential reads nothing more in this pass. A store failure is logged.
    ///
    /// # Errors
    ///
    /// Returns the read error when `cancel` is cancelled, or `store_unavailable` after it.
    pub(super) async fn check_outcomes(
        &self,
        cancel: &CancellationToken,
        present: &BTreeMap<String, Candidate>,
        failed: &BTreeMap<String, String>,
        now: OffsetDateTime,
    ) -> Result<(), Error> {
        // An interval beyond the range of `time::Duration` reads each pull request once.
        let interval = time::Duration::try_from(self.config.outcome_check_interval)
            .unwrap_or(time::Duration::MAX);
        live(cancel)?;
        let due = match self.store.outcome_checks_due(now.saturating_sub(interval)) {
            Ok(due) => due,
            Err(err) => {
                tracing::error!(
                    error_code = error_code(&err),
                    error = %err,
                    "finding outcome check failed"
                );
                return Ok(());
            }
        };
        let mut unreadable = BTreeSet::new();
        for (repository, number) in due {
            let key = format!("{repository}#{number}");
            if present.contains_key(&key) || self.unsearched(failed, &repository) {
                continue;
            }
            let ended = match self.config.credentials.serving(&repository) {
                Ok(credential) if unreadable.contains(&credential.name) => continue,
                Ok(credential) => {
                    let read = (self.integration)
                        .read_pull_request_state(cancel, credential, &repository, number)
                        .await;
                    match read {
                        Err(err) if cancel.is_cancelled() => return Err(err),
                        Err(err) => {
                            tracing::warn!(
                                repository = %repository,
                                pull_request = number,
                                error_code = error_code(&err),
                                "pull request state of finding outcomes unreadable"
                            );
                            unreadable.insert(credential.name.clone());
                            None
                        }
                        // A pull request that GitHub does not return has no head SHA.
                        Ok(state) => (!state.open && !state.head_sha.is_empty()).then_some(state),
                    }
                }
                // No credential serves the repository; the next check is after the interval.
                Err(_) => None,
            };
            live(cancel)?;
            let stored = match ended {
                Some(state) => (self.store)
                    .end_outcomes(&repository, number, state.merged, &state.head_sha)
                    .map(|outcomes| {
                        tracing::info!(
                            repository = %repository,
                            pull_request = number,
                            merged = state.merged,
                            outcomes,
                            "finding outcomes ended"
                        );
                    }),
                None => self.store.outcomes_checked(&repository, number),
            };
            if let Err(err) = stored {
                tracing::error!(
                    repository = %repository,
                    pull_request = number,
                    error_code = error_code(&err),
                    error = %err,
                    "finding outcome check failed"
                );
            }
        }
        Ok(())
    }
}
