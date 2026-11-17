//! Discovery: the signal reads that dismiss findings of dismissed reviews and keep the
//! reactions of the pull request author.

// Rust guideline compliant 2026-06-26

use std::collections::{BTreeMap, BTreeSet};

use time::UtcOffset;
use tokio_util::sync::CancellationToken;

use crate::engine::{Clock, Engine};
use crate::error::error_code;
use crate::finding_outcome::decide_signals;
use crate::integrations::Integrations;

impl<I: Integrations, C: Clock> Engine<I, C> {
    /// Reads the signals of the pull requests with open findings, at most once per
    /// `signal_check_interval` each, and stores what they change.
    ///
    /// It does nothing without `signal_check_interval`. A pull request in a repository whose
    /// search failed in the pass, with the credentials in `failed`, is not read; after a failed
    /// read its credential reads nothing more. A failed or partial read changes no row and is
    /// read again after the interval. A store failure is logged. A cancellation ends the loop.
    ///
    /// It runs after the discovery pass is saved and its notices are sent, without the discovery
    /// lock: no review decision waits for it, and the writes only hold for the review they read.
    pub(super) async fn check_signals(
        &self,
        cancel: &CancellationToken,
        failed: &BTreeMap<String, String>,
    ) {
        let Some(interval) = self.config.signal_check_interval else {
            return;
        };
        // An interval beyond the range of `time::Duration` reads each pull request once.
        let interval = time::Duration::try_from(interval).unwrap_or(time::Duration::MAX);
        let now = self.clock.wall_now().to_offset(UtcOffset::UTC);
        let due = match self.store.signal_checks_due(now.saturating_sub(interval)) {
            Ok(due) => due,
            Err(err) => {
                tracing::error!(
                    error_code = error_code(&err),
                    error = %err,
                    "finding signal check failed"
                );
                return;
            }
        };
        let mut unreadable = BTreeSet::new();
        for (repository, number) in due {
            if cancel.is_cancelled() {
                return;
            }
            if self.unsearched(failed, &repository) {
                continue;
            }
            let Ok(credential) = self.config.credentials.serving(&repository) else {
                continue;
            };
            if unreadable.contains(&credential.name) {
                continue;
            }
            let read = (self.integration)
                .read_pull_request_signals(cancel, credential, &repository, number)
                .await;
            if cancel.is_cancelled() {
                return;
            }
            let stored = match read {
                Ok(signals) => (self.store)
                    .signal_rows(&repository, number)
                    .and_then(|rows| {
                        let updates = decide_signals(&rows, &signals);
                        self.store.apply_signals(&repository, number, &updates)
                    }),
                Err(err) => {
                    tracing::warn!(
                        repository = %repository,
                        pull_request = number,
                        error_code = error_code(&err),
                        "signals of finding outcomes unreadable"
                    );
                    unreadable.insert(credential.name.clone());
                    self.store.signals_checked(&repository, number).map(|()| 0)
                }
            };
            match stored {
                Ok(0) => {}
                Ok(dismissed) => tracing::info!(
                    repository = %repository,
                    pull_request = number,
                    outcomes = dismissed,
                    "finding outcomes dismissed"
                ),
                Err(err) => tracing::error!(
                    repository = %repository,
                    pull_request = number,
                    error_code = error_code(&err),
                    error = %err,
                    "finding signal check failed"
                ),
            }
        }
    }
}
