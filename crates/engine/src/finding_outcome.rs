//! The outcome of each published finding, derived from the reviews that publish and settle it.

// Rust guideline compliant 2026-06-26

use autoreview_go_compat::go_time::format_rfc3339_nano;
use time::OffsetDateTime;

use crate::engine::{Clock, Engine};
use crate::error::error_code;
use crate::integrations::Integrations;
use crate::model::review::{PRIOR_ANSWERED, PRIOR_FIXED, PRIOR_OPEN, prior_key};
use crate::model::{Artifact, ReviewInput};
use crate::store::{FindingOutcome, OUTCOME_ANSWERED, OUTCOME_FIXED, OUTCOME_OPEN, Settlement};
use crate::verify::{VERDICT_UPHELD, demoted};

impl<I: Integrations, C: Clock> Engine<I, C> {
    /// Records the findings that `artifact` publishes and what it settles about earlier ones.
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
        let recorded = self.store.record_outcomes(
            &artifact.repository,
            artifact.number,
            &artifact.head_sha,
            &settlements(artifact),
            &published,
        );
        if let Err(err) = recorded {
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

/// Returns what `artifact` settles about its earlier findings, in their order.
///
/// `fixed` is fixed; `answered` is fixed when the verifier upheld it and `answered` otherwise.
/// An open one published again moves to the key of its new finding; any other stays open.
fn settlements(artifact: &Artifact) -> Vec<Settlement> {
    let upheld = |key: &str| {
        artifact.verification.as_ref().is_some_and(|verification| {
            verification.error_code.is_empty()
                && (verification.prior)
                    .iter()
                    .any(|check| check.key == key && check.verdict == VERDICT_UPHELD)
        })
    };
    let mut settlements = Vec::new();
    for result in &artifact.prior_results {
        let key = result.finding.key.clone();
        let status = result.verdict.status.as_str();
        if status == PRIOR_FIXED || (status == PRIOR_ANSWERED && upheld(&key)) {
            let outcome = OUTCOME_FIXED.to_owned();
            settlements.push(Settlement::Decided { key, outcome });
        } else if status == PRIOR_ANSWERED {
            let outcome = OUTCOME_ANSWERED.to_owned();
            settlements.push(Settlement::Decided { key, outcome });
        } else if status == PRIOR_OPEN && !result.republished.is_empty() {
            let republished = prior_key(&artifact.head_sha, &result.republished);
            settlements.push(Settlement::Republished { key, republished });
        }
    }
    settlements
}
