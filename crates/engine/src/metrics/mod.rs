//! The Prometheus endpoint of `autoreview serve`: the review counters of the process and the
//! current state, read from the store at each scrape.
//!
//! The binary binds the listener; this module counts the reviews and answers each request.

// Rust guideline compliant 2026-06-26

pub(crate) mod counts;
pub(crate) mod exposition;
pub(crate) mod server;

use std::sync::{Mutex, MutexGuard, PoisonError};

use tokio_util::sync::CancellationToken;

use self::counts::{Counts, Gauges};
use self::exposition::render;
use crate::discovery::poll::live;
use crate::engine::{Clock, Engine};
use crate::error::{Error, error_code};
use crate::integrations::Integrations;
use crate::store::{REVIEW_COMPLETED, StoredReview};

#[doc(inline)]
pub use server::{METRICS_PATH, MetricsConfig, MetricsServer};

/// The review counters of one `serve` process; they start at zero with the process.
#[derive(Debug, Default)]
pub(crate) struct Metrics {
    /// The counts, shared by the workers and the scrapes.
    counts: Mutex<Counts>,
}

impl Metrics {
    /// Locks the counts; a worker that panicked leaves counts that are still valid.
    fn lock(&self) -> MutexGuard<'_, Counts> {
        self.counts.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl<I: Integrations, C: Clock> Engine<I, C> {
    /// Returns the engine with the review counters of the metrics endpoint.
    #[must_use]
    pub fn with_metrics(mut self) -> Self {
        self.metrics = Some(Metrics::default());
        self
    }

    /// Counts a review that a worker took from the queue; without counters it does nothing.
    pub(crate) fn count_started(&self, review: &StoredReview) {
        if let Some(metrics) = &self.metrics {
            metrics.lock().start(review);
        }
    }

    /// Counts the review `workflow_id` when its run completed it.
    ///
    /// Without counters it does nothing. A row that cannot be read is logged and not counted.
    pub(crate) fn count_completed(&self, workflow_id: &str) {
        let Some(metrics) = &self.metrics else {
            return;
        };
        let counted = self
            .store
            .get_review(workflow_id)
            .and_then(|row| match row {
                Some(review) if review.status == REVIEW_COMPLETED => {
                    metrics.lock().complete(&review)
                }
                _ => Ok(()),
            });
        if let Err(err) = counted {
            tracing::warn!(
                workflow_id,
                error_code = error_code(&err),
                "review not counted in the metrics"
            );
        }
    }

    /// Returns the text exposition: the current state of the store and the review counters.
    ///
    /// The gauges come from one `Store::stats_snapshot` without a range, the indexed read of
    /// the reviews that are not completed. Without counters the counter families are empty.
    ///
    /// # Errors
    ///
    /// Returns the shutdown error after a cancel, the errors of `Store::stats_snapshot`, and
    /// `store_state_invalid` for a schedule key without `#`.
    pub fn metrics_text(&self, cancel: &CancellationToken) -> Result<String, Error> {
        live(cancel)?;
        let (reviews, discovery) = self.store.stats_snapshot(None)?;
        let gauges = Gauges::read(&reviews, &discovery.schedule)?;
        let none = Counts::default();
        let guard = self.metrics.as_ref().map(Metrics::lock);
        Ok(render(&gauges, guard.as_deref().unwrap_or(&none)))
    }
}
