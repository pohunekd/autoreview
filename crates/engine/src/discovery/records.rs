//! The discovery records that a review writes: its approval wait and its publication.
//!
//! Mirrors `cmd/autoreview/discovery.go:539-579`.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeSet;

use autoreview_schedule::scheduler::reconcile_completed;
use tokio_util::sync::CancellationToken;

use super::poll::live;
use crate::config::identifier_valid;
use crate::engine::{Clock, Engine};
use crate::error::Error;
use crate::integrations::Integrations;
use crate::model::ids::review_identity_valid;
use crate::model::{AwaitRecord, PublishedRecord};

impl<I: Integrations, C: Clock> Engine<I, C> {
    /// Records that the review of `record` waits for an approval decision.
    ///
    /// # Errors
    ///
    /// Returns `review_identity_invalid` for an invalid record, or the error of the discovery
    /// lock or the store.
    pub(crate) async fn mark_awaiting(
        &self,
        cancel: &CancellationToken,
        record: &AwaitRecord,
    ) -> Result<(), Error> {
        if !review_identity_valid(&record.repository, record.number, &record.head_sha)
            || !identifier_valid(&record.workflow_id)
        {
            return Err(Error::workflow("review_identity_invalid", true, None));
        }
        self.with_discovery(|| async {
            self.store.update_discovery(|state, _| {
                live(cancel)?;
                state.awaiting.insert(record.key(), record.clone());
                Ok(())
            })
        })
        .await
    }

    /// Removes the approval wait of `record`, only while it belongs to the same workflow.
    ///
    /// # Errors
    ///
    /// Returns the error of the discovery lock or the store.
    pub(crate) async fn clear_awaiting(
        &self,
        cancel: &CancellationToken,
        record: &AwaitRecord,
    ) -> Result<(), Error> {
        self.with_discovery(|| async {
            self.store.update_discovery(|state, _| {
                live(cancel)?;
                let key = record.key();
                if state
                    .awaiting
                    .get(&key)
                    .is_some_and(|awaiting| awaiting.workflow_id == record.workflow_id)
                {
                    state.awaiting.remove(&key);
                }
                Ok(())
            })
        })
        .await
    }

    /// Records the publication of `record` and completes its wait and its schedule record.
    ///
    /// # Errors
    ///
    /// Returns `review_identity_invalid` for an invalid record, or the error of the discovery
    /// lock or the store.
    pub(crate) async fn mark_published(
        &self,
        cancel: &CancellationToken,
        record: &PublishedRecord,
    ) -> Result<(), Error> {
        if !review_identity_valid(&record.repository, record.number, &record.head_sha)
            || !identifier_valid(&record.workflow_id)
        {
            return Err(Error::workflow("review_identity_invalid", true, None));
        }
        self.with_discovery(|| async {
            self.store.update_discovery(|state, _| {
                live(cancel)?;
                let key = record.key();
                state.published.insert(key.clone(), record.clone());
                if state
                    .awaiting
                    .get(&key)
                    .is_some_and(|awaiting| awaiting.workflow_id == record.workflow_id)
                {
                    state.awaiting.remove(&key);
                }
                if let Some(scheduled) = state.schedule.remove(&key) {
                    state.schedule.insert(
                        key,
                        reconcile_completed(
                            scheduled,
                            &BTreeSet::from([record.workflow_id.clone()]),
                        ),
                    );
                }
                Ok(())
            })
        })
        .await
    }
}
