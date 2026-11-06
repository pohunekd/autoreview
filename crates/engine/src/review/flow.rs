//! The review workflow: it runs a review, plans its publication and publishes it.
//!
//! Mirrors `cmd/autoreview/review.go:595-776, 857-892`.

// Rust guideline compliant 2026-06-26

use autoreview_policy::{Event, OnClean, Reviewer};
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, UtcOffset};
use tokio_util::sync::CancellationToken;

use super::input::{
    ArtifactOutcome, RenderOutcome, SLOT_STATE_PREFIX, STEP_COMMENTABLE, STEP_DISCUSSION,
    STEP_HUMAN_REVIEW, STEP_PLAN, STEP_PUBLISHED_AT, STEP_RENDERED, SlotState, validate_input,
};
use super::sessions::{Review, ReviewerSlot};
use crate::discovery::poll::live;
use crate::engine::{Clock, Engine, STEP_ARTIFACT, STEP_PUBLICATION};
use crate::error::{BoxError, Error, WORKFLOW_FAILED, error_code};
use crate::integrations::{CommentableLines, GitHubReview, Integrations, SessionRequest};
use crate::messages::published_message;
use crate::model::{
    Artifact, Discussion, Outcome, PublishResult, PublishedRecord, PullRequest, ReviewOutcome,
};
use crate::prior::settle_prior;
use crate::protocol::combine::combine_results;
use crate::protocol::request::build_reviewer_request;
use crate::publish::anchor::anchor_findings;
use crate::publish::decision_text;
use crate::publish::plan::{PublicationPlan, blocking_count, plan_publication, publication_event};
use crate::publish::render::{RenderSettings, human_review_signals, render_review};
use crate::store::{RESULT_SUCCESS, StoredReview};
use crate::verify::{apply_verification, claims_of, verifier_reviewer};

/// The stored time of a publication, written as Go writes a `time.Time`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
struct PublishedAt(#[serde(with = "autoreview_go_compat::go_time")] OffsetDateTime);

/// Returns the progress of a slot with no stored state: run 1, nothing started.
fn first_state() -> SlotState {
    SlotState {
        run: 1,
        start: 0,
        session: None,
        result: None,
        verification: None,
    }
}

impl<I: Integrations, C: Clock> Engine<I, C> {
    /// Runs or resumes `stored` until it completes or waits for an approval.
    ///
    /// # Errors
    ///
    /// Returns the shutdown error when the engine stops the review midway, or the error of
    /// the store. A failed review is completed as a failure and returns `Ok`.
    #[expect(dead_code, reason = "called by S30")]
    pub(crate) async fn run_review(
        &self,
        cancel: &CancellationToken,
        stored: StoredReview,
    ) -> Result<(), Error> {
        let mut review = Review {
            engine: self,
            cancel: cancel.clone(),
            workflow_id: stored.workflow_id,
            input: stored.input,
            slots: Vec::new(),
            verifier: None,
            discussion: Discussion::default(),
            approved: false,
        };
        if let Err(err) = validate_input(&review.input) {
            return review
                .fail(Error::workflow("review_input_invalid", true, Some(err)))
                .await;
        }
        if !review.input.prior.is_empty() {
            let settled = review
                .once::<Discussion, _>(STEP_DISCUSSION, || {
                    let integration = &*self.integration;
                    let cancel = &review.cancel;
                    let input = &review.input;
                    async move {
                        integration
                            .list_discussion(cancel, &input.pull_request, &input.prior)
                            .await
                    }
                })
                .await;
            match settled {
                Ok(discussion) => review.discussion = discussion,
                Err(err) => return review.fail(err).await,
            }
        }
        for reviewer in review.input.reviewers.clone() {
            let prompt = match build_reviewer_request(&review.input, &reviewer, &review.discussion)
            {
                Ok(prompt) => prompt,
                Err(err) => return review.fail(err).await,
            };
            let slot = match review.new_slot(reviewer, prompt) {
                Ok(slot) => slot,
                Err(err) => return Err(err),
            };
            review.slots.push(slot);
        }
        if review.input.verifies() {
            let Some(reviewer) = verifier_reviewer(&review.input) else {
                return Err(Error::workflow(WORKFLOW_FAILED, true, None));
            };
            match review.new_slot(reviewer, String::new()) {
                Ok(slot) => review.verifier = Some(slot),
                Err(err) => return Err(err),
            }
        }
        live(&review.cancel)?;
        let plan = match self
            .store
            .load_step::<PublicationPlan>(&review.workflow_id, STEP_PLAN)?
        {
            Some(plan) => plan,
            None => {
                if review.input.adopts_handoff() {
                    return review.adopt_handoff().await;
                }
                return review.review().await;
            }
        };
        let loaded = live(&review.cancel).and_then(|()| {
            self.store
                .load_step::<Artifact>(&review.workflow_id, STEP_ARTIFACT)
        });
        let artifact = match loaded {
            Ok(Some(artifact)) => artifact,
            Ok(None) => {
                return review
                    .fail(Error::workflow("review_incomplete", true, None))
                    .await;
            }
            Err(err) => {
                let source: BoxError = Box::new(err);
                return review
                    .fail(Error::workflow("review_incomplete", true, Some(source)))
                    .await;
            }
        };
        if !plan.await_approval {
            let current = match review.current_pull_request().await {
                Ok(current) => current,
                Err(err) => return review.fail(err).await,
            };
            return review.publish(&current, &artifact, &plan).await;
        }
        review
            .continue_after_approval(&stored.approval, artifact, plan)
            .await
    }
}

impl<I: Integrations, C: Clock> Review<'_, I, C> {
    /// Returns the slot of `reviewer` with its stored progress, or its first progress.
    ///
    /// The prompt of the verifier is empty here: it depends on the combined findings.
    fn new_slot(&self, reviewer: Reviewer, prompt: String) -> Result<ReviewerSlot, Error> {
        let request = SessionRequest {
            pull_request: self.input.pull_request.clone(),
            route: self.input.route.clone(),
            reviewer: reviewer.clone(),
            checkout: self.input.rule.pipeline.checkout,
            workflow_id: self.workflow_id.clone(),
            run: 0,
            start: 0,
            prompt,
        };
        let key = format!("{SLOT_STATE_PREFIX}{}", reviewer.id);
        live(&self.cancel)?;
        let state = self
            .engine
            .store
            .get_state::<SlotState>(&self.workflow_id, &key)?
            .unwrap_or_else(first_state);
        Ok(ReviewerSlot {
            reviewer,
            request,
            state,
        })
    }

    /// Runs the reviewers, combines and anchors their findings and plans the publication.
    async fn review(&mut self) -> Result<(), Error> {
        if let Err(err) = self.current_pull_request().await {
            return self.fail(err).await;
        }
        if let Err(err) = self.collect().await {
            return self.fail(err).await;
        }
        let mut results = Vec::with_capacity(self.slots.len());
        for slot in &self.slots {
            let Some(result) = slot.state.result.clone() else {
                return Err(Error::workflow(WORKFLOW_FAILED, true, None));
            };
            results.push(result);
        }
        let name = format!("{STEP_ARTIFACT}/combined");
        let combined = self
            .once::<ArtifactOutcome, _>(&name, || {
                let (engine, input, results) = (self.engine, &self.input, &results);
                async move {
                    let reviewed_at = engine.clock.wall_now();
                    match combine_results(&engine.config, input, results, reviewed_at) {
                        Ok(artifact) => Ok(ArtifactOutcome {
                            artifact,
                            error_code: String::new(),
                        }),
                        Err(err) => Ok(ArtifactOutcome {
                            artifact: Artifact::default(),
                            error_code: error_code(&err).to_owned(),
                        }),
                    }
                }
            })
            .await;
        let combined = match combined {
            Ok(combined) => combined,
            Err(err) => return self.fail(err).await,
        };
        if !combined.error_code.is_empty() {
            return self
                .fail(Error::workflow(combined.error_code, true, None))
                .await;
        }
        let current = match self.current_pull_request().await {
            Ok(current) => current,
            Err(err) => return self.fail(err).await,
        };
        let artifact = combined.artifact;
        if artifact.findings.is_empty() && artifact.prior_results.is_empty() {
            return self.plan(&current, artifact).await;
        }
        let commentable = self
            .once::<CommentableLines, _>(STEP_COMMENTABLE, || {
                let integration = &*self.engine.integration;
                let cancel = &self.cancel;
                let current = &current;
                async move { integration.list_commentable_lines(cancel, current).await }
            })
            .await;
        let commentable = match commentable {
            Ok(commentable) => commentable,
            Err(err) => return self.fail(err).await,
        };
        // An unanchored finding becomes a note first, so the verifier never checks it.
        // A reopened prior finding is published again, so the verifier runs first.
        let mut artifact = anchor_findings(artifact, &commentable);
        let claims = claims_of(&self.input, &artifact);
        if self.input.verifies() && !claims.is_empty() {
            let verification = match self.verify(&claims).await {
                Ok(verification) => verification,
                Err(err) => return self.fail(err).await,
            };
            artifact = apply_verification(artifact, verification);
        }
        let settled = settle_prior(&self.input, &self.discussion, artifact);
        let artifact = anchor_findings(settled, &commentable);
        self.plan(&current, artifact).await
    }

    /// Plans the stored result of the review that handed the notes to the author.
    ///
    /// The head is unchanged, so running the reviewers again would only sample the same code.
    async fn adopt_handoff(&mut self) -> Result<(), Error> {
        let current = match self.current_pull_request().await {
            Ok(current) => current,
            Err(err) => return self.fail(err).await,
        };
        let Some(handoff) = self.input.handoff.as_ref().map(|h| h.workflow_id.clone()) else {
            return Err(Error::workflow(WORKFLOW_FAILED, true, None));
        };
        let loaded = live(&self.cancel).and_then(|()| {
            self.engine
                .store
                .load_step::<Artifact>(&handoff, STEP_ARTIFACT)
        });
        let artifact = match loaded {
            Ok(Some(artifact)) => artifact,
            Ok(None) => {
                return self
                    .fail(Error::workflow("handoff_review_not_stored", true, None))
                    .await;
            }
            Err(err) => {
                let source: BoxError = Box::new(err);
                return self
                    .fail(Error::workflow(
                        "handoff_review_not_stored",
                        true,
                        Some(source),
                    ))
                    .await;
            }
        };
        if artifact.repository != current.repository
            || artifact.number != current.number
            || artifact.head_sha != current.head_sha
        {
            return self
                .fail(Error::workflow("handoff_review_mismatch", true, None))
                .await;
        }
        self.plan(&current, artifact).await
    }

    /// Stores the final artifact with its publication plan, then publishes it or waits.
    async fn plan(&mut self, current: &PullRequest, mut artifact: Artifact) -> Result<(), Error> {
        let signals = match self.human_review(current, &artifact).await {
            Ok(signals) => signals,
            Err(err) => return self.fail(err).await,
        };
        artifact.human_review = signals;
        let plan = plan_publication(&self.input, &artifact, &self.input.publisher_login);
        live(&self.cancel)?;
        self.engine
            .store
            .save_step(&self.workflow_id, STEP_ARTIFACT, &artifact)?;
        live(&self.cancel)?;
        self.engine
            .store
            .save_step(&self.workflow_id, STEP_PLAN, &plan)?;
        if plan.await_approval {
            return self.await_approval(&artifact).await;
        }
        self.publish(current, &artifact, &plan).await
    }

    /// Evaluates the human review signals of the rule once per review.
    ///
    /// Only a clean review that its rule approves without the user needs them.
    async fn human_review(
        &self,
        current: &PullRequest,
        artifact: &Artifact,
    ) -> Result<Vec<String>, Error> {
        let Some(signals) = self.input.rule.human_review.as_ref() else {
            return Ok(Vec::new());
        };
        let blocking = blocking_count(&self.input, artifact);
        if self.input.rule.on_clean != OnClean::Approve || blocking > 0 {
            return Ok(Vec::new());
        }
        self.once::<Vec<String>, _>(STEP_HUMAN_REVIEW, || {
            let integration = &*self.engine.integration;
            let cancel = &self.cancel;
            async move {
                let mut deleted = Vec::new();
                if !signals.deleted_tests.is_empty() {
                    deleted = integration.list_deleted_files(cancel, current).await?;
                }
                Ok(human_review_signals(signals, &current.labels, &deleted))
            }
        })
        .await
    }

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
                let cancel = &self.cancel;
                let review = &rendered.review;
                async move { integration.publish_review(cancel, current, review).await }
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
