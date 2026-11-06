//! The review workflow: it runs a review, plans its publication and publishes it.
//!
//! Mirrors `cmd/autoreview/review.go:857-892`.

// Rust guideline compliant 2026-06-26

use autoreview_policy::Event;
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, UtcOffset};

use super::input::{RenderOutcome, STEP_PUBLISHED_AT, STEP_RENDERED};
use super::sessions::Review;
use crate::discovery::poll::live;
use crate::engine::{Clock, STEP_PUBLICATION};
use crate::error::{Error, error_code};
use crate::integrations::{GitHubReview, Integrations};
use crate::messages::published_message;
use crate::model::{Artifact, Outcome, PublishResult, PublishedRecord, PullRequest, ReviewOutcome};
use crate::publish::decision_text;
use crate::publish::plan::{PublicationPlan, blocking_count, publication_event};
use crate::publish::render::{RenderSettings, render_review};
use crate::store::RESULT_SUCCESS;

/// The stored time of a publication, written as Go writes a `time.Time`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
struct PublishedAt(#[serde(with = "autoreview_go_compat::go_time")] OffsetDateTime);

impl<I: Integrations, C: Clock> Review<'_, I, C> {
    /// Renders and publishes the review once, records it and completes the review.
    ///
    /// # Errors
    ///
    /// Returns the shutdown error, or the error of the store or of the discovery record. A
    /// failed render or publication completes the review as a failure and returns `Ok`.
    pub(crate) async fn publish(
        &mut self,
        current: &PullRequest,
        artifact: &Artifact,
        plan: &PublicationPlan,
    ) -> Result<(), Error> {
        let settings = RenderSettings {
            ai_signature: self.engine.config.ai_signature.clone(),
            github_body_max_bytes: self.engine.config.github_body_max_bytes,
            excerpt_characters: self.engine.config.excerpt_characters,
        };
        let rendered = self
            .once::<RenderOutcome, _>(STEP_RENDERED, || {
                let (settings, input) = (&settings, &self.input);
                async move {
                    match render_review(settings, input, artifact, plan) {
                        Ok(review) => Ok(RenderOutcome {
                            review,
                            error_code: String::new(),
                        }),
                        Err(err) => Ok(RenderOutcome {
                            review: GitHubReview::default(),
                            error_code: error_code(&err).to_owned(),
                        }),
                    }
                }
            })
            .await;
        let rendered = match rendered {
            Ok(rendered) => rendered,
            Err(err) => return self.fail(err).await,
        };
        if !rendered.error_code.is_empty() {
            return self
                .fail(Error::workflow(rendered.error_code, true, None))
                .await;
        }
        let result = self
            .once::<PublishResult, _>(STEP_PUBLICATION, || {
                let integration = &*self.engine.integration;
                let review = &rendered.review;
                async move { integration.publish_review(current, review).await }
            })
            .await;
        let result = match result {
            Ok(result) => result,
            Err(err) => return self.fail(err).await,
        };
        let published_at = self
            .once::<PublishedAt, _>(STEP_PUBLISHED_AT, || async {
                Ok(PublishedAt(
                    self.engine.clock.wall_now().to_offset(UtcOffset::UTC),
                ))
            })
            .await?;
        let record = PublishedRecord {
            repository: current.repository.clone(),
            number: current.number,
            head_sha: current.head_sha.clone(),
            review_request_id: current.review_request_id,
            decision: plan.decision,
            workflow_id: self.workflow_id.clone(),
            published_at: published_at.0,
            awaits_author: plan.notes_to_author,
            reminded: false,
        };
        if let Err(err) = self.engine.mark_published(&self.cancel, &record).await {
            return Err(self.interrupted(err));
        }
        let blocking = blocking_count(&self.input, artifact);
        let event: Event = publication_event(*plan, blocking);
        let message = published_message(
            self.engine.config.excerpt_characters,
            &self.input,
            artifact,
            plan,
            &result,
        );
        self.notify("published", event, &message).await?;
        tracing::info!(
            review_id = %current.review_id(),
            decision = %decision_text(plan.decision),
            findings = artifact.findings.len(),
            blocking_findings = blocking,
            review_url = %result.review_url,
            reused = result.reused,
            policy_digest = %self.input.policy_digest,
            "review published"
        );
        live(&self.cancel)?;
        self.engine.store.complete_review(
            &self.workflow_id,
            RESULT_SUCCESS,
            "",
            Some(&ReviewOutcome {
                outcome: Outcome::Published,
                publication: Some(result),
            }),
        )
    }
}
