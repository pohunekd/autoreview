//! The outcome of each published finding, derived from the review that publishes it.

// Rust guideline compliant 2026-06-26

use autoreview_go_compat::go_time::format_rfc3339_nano;
use time::OffsetDateTime;

use crate::engine::{Clock, Engine};
use crate::error::error_code;
use crate::integrations::Integrations;
use crate::model::review::prior_key;
use crate::model::{Artifact, ReviewInput};
use crate::store::{FindingOutcome, OUTCOME_OPEN};
use crate::verify::demoted;

impl<I: Integrations, C: Clock> Engine<I, C> {
    /// Records one outcome for each finding that `artifact` publishes.
    ///
    /// A store failure is logged and never fails the review: outcomes only record a review.
    pub(crate) fn record_finding_outcomes(
        &self,
        workflow_id: &str,
        input: &ReviewInput,
        artifact: &Artifact,
        published_at: OffsetDateTime,
    ) {
        let published_at = format_rfc3339_nano(published_at);
        let published = published_outcomes(workflow_id, input, artifact, &published_at);
        if let Err(err) = self.store.record_outcomes(&published) {
            tracing::error!(
                review_id = %input.pull_request.review_id(),
                error_code = error_code(&err),
                error = %err,
                "finding outcomes not recorded"
            );
        }
    }
}

/// Returns one outcome per finding of `artifact`, except those that publish a prior one again.
///
/// A finding that the verifier demoted has its verdict as the outcome; any other is `open`.
fn published_outcomes(
    workflow_id: &str,
    input: &ReviewInput,
    artifact: &Artifact,
    published_at: &str,
) -> Vec<FindingOutcome> {
    let republished = |id: &str| {
        (artifact.prior_results)
            .iter()
            .any(|result| !result.republished.is_empty() && result.republished == id)
    };
    let mut outcomes = Vec::new();
    for finding in &artifact.findings {
        if republished(&finding.id) {
            continue;
        }
        let key = prior_key(&artifact.head_sha, &finding.id);
        let (outcome, outcome_head_sha) = if demoted(finding) {
            (finding.verdict.clone(), artifact.head_sha.clone())
        } else {
            (OUTCOME_OPEN.to_owned(), String::new())
        };
        outcomes.push(FindingOutcome {
            repository: artifact.repository.clone(),
            number: artifact.number,
            finding_key: key.clone(),
            tracked_key: key,
            workflow_id: workflow_id.to_owned(),
            finding_id: finding.id.clone(),
            head_sha: artifact.head_sha.clone(),
            reviewer_id: finding.reviewer_id.clone(),
            pipeline: input.rule.pipeline.name.clone(),
            rule: input.rule.match_glob.clone(),
            severity: finding.severity.clone(),
            confidence: finding.confidence.clone(),
            path: finding.path.clone(),
            line: finding.line,
            blocking: input.blocks(finding),
            outcome,
            outcome_head_sha,
            published_at: published_at.to_owned(),
            updated_at: String::new(),
        });
    }
    outcomes
}
