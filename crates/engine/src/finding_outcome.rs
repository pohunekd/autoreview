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
        // An adopted hand-off publishes the findings of the review that wrote them.
        let origin = match &input.handoff {
            Some(handoff) if handoff.adopt => &handoff.workflow_id,
            _ => workflow_id,
        };
        let recorded = self.store.record_outcomes(
            &artifact.repository,
            artifact.number,
            &artifact.head_sha,
            &settlements(origin, artifact),
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
/// A new finding whose key no earlier finding of `artifact` has untracks the stored rows of
/// that key that a review other than `origin` published: they belong to another finding.
fn settlements(origin: &str, artifact: &Artifact) -> Vec<Settlement> {
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
    for finding in &artifact.findings {
        let key = prior_key(&artifact.head_sha, &finding.id);
        let earlier = (artifact.prior_results)
            .iter()
            .any(|result| result.finding.key == key || result.republished == finding.id);
        if !earlier {
            let origin = origin.to_owned();
            settlements.push(Settlement::Untracked { key, origin });
        }
    }
    settlements
}

#[cfg(test)]
mod tests {
    use crate::model::review::{PRIOR_ANSWERED, PRIOR_FIXED, PRIOR_OPEN, prior_key};
    use crate::model::{
        Artifact, Finding, PriorCheck, PriorFinding, PriorResult, PriorVerdict, Verification,
    };
    use crate::protocol::fixtures::sha;
    use crate::store::Settlement;
    use crate::verify::{VERDICT_REOPENED, VERDICT_UPHELD};

    use super::settlements;

    #[test]
    fn prior_verdicts_settle_open_findings_as_the_publication_does() {
        let key = prior_key(&sha("a"), "O001");
        // One earlier finding with its final status, its new ID, and the verifier's check.
        let artifact = |status: &str, republished: &str, check: Option<(&str, &str)>| Artifact {
            head_sha: sha("c"),
            prior_results: vec![PriorResult {
                finding: PriorFinding {
                    key: key.clone(),
                    ..PriorFinding::default()
                },
                verdict: PriorVerdict {
                    key: key.clone(),
                    status: status.to_owned(),
                    ..PriorVerdict::default()
                },
                republished: republished.to_owned(),
                reopened: check.is_some_and(|(verdict, _)| verdict == VERDICT_REOPENED),
            }],
            verification: check.map(|(verdict, error_code)| Verification {
                prior: vec![PriorCheck {
                    key: key.clone(),
                    verdict: verdict.to_owned(),
                    note: String::new(),
                }],
                error_code: error_code.to_owned(),
                ..Verification::default()
            }),
            ..Artifact::default()
        };
        let (upheld, reopened) = (Some((VERDICT_UPHELD, "")), Some((VERDICT_REOPENED, "")));
        let failed = Some((VERDICT_UPHELD, "verifier_failed"));
        // The last column is the outcome, `moved` for a new tracked key, empty for no change.
        let cases = [
            ("fixed", PRIOR_FIXED, "", None, "fixed"),
            ("upheld answer", PRIOR_ANSWERED, "", upheld, "fixed"),
            ("no verifier", PRIOR_ANSWERED, "", None, "answered"),
            ("failed check", PRIOR_ANSWERED, "", failed, "answered"),
            ("reopened", PRIOR_OPEN, "", reopened, ""),
            ("reopened, published", PRIOR_OPEN, "P001", reopened, "moved"),
            ("published again", PRIOR_OPEN, "P001", None, "moved"),
            ("left open", PRIOR_OPEN, "", None, ""),
        ];
        for (name, status, republished, check, want) in cases {
            let key = key.clone();
            let want = match want {
                "" => Vec::new(),
                "moved" => {
                    let republished = prior_key(&sha("c"), "P001");
                    vec![Settlement::Republished { key, republished }]
                }
                outcome => vec![Settlement::Decided {
                    key,
                    outcome: outcome.to_owned(),
                }],
            };
            let settled = settlements("w1", &artifact(status, republished, check));
            assert_eq!(settled, want, "{name}");
        }

        // Only O002 is a new key here: O001 is the earlier finding, P001 publishes it again.
        let mut reused = artifact(PRIOR_OPEN, "P001", None);
        reused.prior_results[0].finding.key = prior_key(&sha("c"), "O001");
        let finding = |id: &str| Finding {
            id: id.to_owned(),
            ..Finding::default()
        };
        reused.findings = vec![finding("O001"), finding("O002"), finding("P001")];
        let want = vec![
            Settlement::Republished {
                key: prior_key(&sha("c"), "O001"),
                republished: prior_key(&sha("c"), "P001"),
            },
            Settlement::Untracked {
                key: prior_key(&sha("c"), "O002"),
                origin: "w1".to_owned(),
            },
        ];
        assert_eq!(settlements("w1", &reused), want, "a reused key");
    }
}
