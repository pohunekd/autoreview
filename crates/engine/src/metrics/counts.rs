//! The review counters of one process and the gauges of one scrape.
//!
//! A completed review is classified with the function that the statistics document uses, so
//! a counter and its document field count the same attempts.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeMap;

use autoreview_schedule::scheduler::Record;

use crate::error::Error;
use crate::stats::{Ending, Published, StateCounts, completed_ending, counts_now, stopped};
use crate::store::StoredReview;

/// Upper bounds of the publication time buckets in seconds, from one minute to three days.
pub(crate) const PUBLICATION_BUCKETS: [u64; 9] =
    [60, 300, 900, 1800, 3600, 14_400, 43_200, 86_400, 259_200];

/// The counts of one repository and rule since the process started.
#[derive(Debug, Default)]
pub(crate) struct ReviewCounts {
    /// Attempts that a worker took from the queue.
    pub(crate) started: u64,
    /// Attempts completed, whatever their result.
    pub(crate) completed: u64,
    /// Published attempts per `decision` label.
    pub(crate) published: BTreeMap<&'static str, u64>,
    /// Attempts that a newer state of the pull request replaced.
    pub(crate) superseded: u64,
    /// Approval waits that ended at their deadline.
    pub(crate) approval_expired: u64,
    /// Failed attempts per stored error code.
    pub(crate) failed: BTreeMap<String, u64>,
    /// Published attempts with a duration at most each bound of `PUBLICATION_BUCKETS`.
    pub(crate) buckets: [u64; PUBLICATION_BUCKETS.len()],
    /// Sum of the seconds of the published attempts with a duration.
    pub(crate) seconds_sum: u64,
    /// Published attempts with a duration.
    pub(crate) seconds_count: u64,
}

/// The review counters of this process, per repository and rule `match`.
#[derive(Debug, Default)]
pub(crate) struct Counts {
    /// The counts keyed by repository, then rule.
    pub(crate) reviews: BTreeMap<(String, String), ReviewCounts>,
}

impl Counts {
    /// Returns the counts of the repository and the rule of `review`.
    fn entry(&mut self, review: &StoredReview) -> &mut ReviewCounts {
        let input = &review.input;
        let key = (
            input.pull_request.repository.clone(),
            input.rule.match_glob.clone(),
        );
        self.reviews.entry(key).or_default()
    }

    /// Counts one attempt that a worker took from the queue.
    pub(crate) fn start(&mut self, review: &StoredReview) {
        self.entry(review).started += 1;
    }

    /// Counts one completed attempt by how it ended, as the statistics document counts it.
    ///
    /// # Errors
    ///
    /// Returns the `store_state_invalid` error of `completed_ending`; nothing is counted then.
    pub(crate) fn complete(&mut self, review: &StoredReview) -> Result<(), Error> {
        let ending = completed_ending(review)?;
        let counts = self.entry(review);
        counts.completed += 1;
        match ending {
            Ending::Published { decision, seconds } => {
                *counts
                    .published
                    .entry(decision_label(decision))
                    .or_default() += 1;
                if let Some(seconds) = seconds.and_then(|value| u64::try_from(value).ok()) {
                    for (bucket, bound) in counts.buckets.iter_mut().zip(PUBLICATION_BUCKETS) {
                        if seconds <= bound {
                            *bucket += 1;
                        }
                    }
                    counts.seconds_sum = counts.seconds_sum.saturating_add(seconds);
                    counts.seconds_count += 1;
                }
            }
            Ending::Superseded => counts.superseded += 1,
            Ending::Expired => counts.approval_expired += 1,
            Ending::Failed => *counts.failed.entry(review.failure.clone()).or_default() += 1,
            Ending::Unknown => {}
        }
        Ok(())
    }
}

/// Returns the `decision` label of a published attempt: the name of its document counter.
fn decision_label(decision: Published) -> &'static str {
    match decision {
        Published::ApprovedWithoutHuman => "approved_without_human",
        Published::ApprovedAfterWait => "approved_after_wait",
        Published::ChangesRequested => "changes_requested",
        Published::Commented => "commented",
        Published::Other => "other",
    }
}

/// The current state at the time of a scrape.
#[derive(Debug, Default)]
pub(crate) struct Gauges {
    /// Queued, running and undecided awaiting reviews per repository and review host.
    pub(crate) reviews: BTreeMap<(String, String), StateCounts>,
    /// Pull requests stopped after their retries per repository.
    pub(crate) stopped: BTreeMap<String, i64>,
}

impl Gauges {
    /// Counts the current state as the statistics document counts it.
    ///
    /// # Errors
    ///
    /// Returns `store_state_invalid` for a schedule key without `#`.
    pub(crate) fn read(
        reviews: &[StoredReview],
        schedule: &BTreeMap<String, Record>,
    ) -> Result<Self, Error> {
        let mut counts: BTreeMap<(String, String), StateCounts> = BTreeMap::new();
        for review in reviews.iter().filter(|review| counts_now(review)) {
            let input = &review.input;
            let key = (
                input.pull_request.repository.clone(),
                input.route.host.clone(),
            );
            counts.entry(key).or_default().add(&review.status);
        }
        Ok(Self {
            reviews: counts,
            stopped: stopped(schedule)?,
        })
    }
}
