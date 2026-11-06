//! Combination of the reviewer results into one artifact.

// Rust guideline compliant 2026-06-26

use autoreview_go_compat::go_time::format_rfc3339_seconds;
use time::{OffsetDateTime, UtcOffset};

use crate::config::Config;
use crate::error::Error;
use crate::model::review::{PRIOR_ANSWERED, PRIOR_FIXED, PRIOR_OPEN};
use crate::model::{
    Artifact, Finding, PriorFinding, PriorResult, PriorVerdict, ReviewInput, ReviewerResult,
    ReviewerSummary,
};
use crate::protocol::validate::count_over;

/// Note of a prior finding that no reviewer settled.
const NOT_SETTLED_NOTE: &str = "No reviewer settled this finding.";

/// Combines the reviewer results, given in reviewer order, into one artifact.
///
/// Each finding takes the prefix of its reviewer, so `O001` and `S001` never collide.
///
/// # Errors
///
/// Returns `artifact_invalid` when the results do not match the reviewers one to one, when a
/// source identifier is too short to take a prefix, or when the findings exceed the limit.
pub(crate) fn combine_results(
    cfg: &Config,
    input: &ReviewInput,
    results: &[ReviewerResult],
    reviewed_at: OffsetDateTime,
) -> Result<Artifact, Error> {
    if results.len() != input.reviewers.len() || results.is_empty() {
        return Err(artifact_invalid());
    }
    let mut artifact = Artifact {
        repository: input.pull_request.repository.clone(),
        number: input.pull_request.number,
        head_sha: input.pull_request.head_sha.clone(),
        scope: input.scope.clone(),
        reviewed_at: format_rfc3339_seconds(reviewed_at.to_offset(UtcOffset::UTC)),
        ..Artifact::default()
    };
    for (reviewer, result) in input.reviewers.iter().zip(results) {
        if result.reviewer_id != reviewer.id {
            return Err(artifact_invalid());
        }
        artifact.summaries.push(ReviewerSummary {
            reviewer_id: reviewer.id.clone(),
            label: reviewer.label.clone(),
            summary: result.summary.clone(),
        });
        for source in &result.findings {
            let Some(suffix) = source.source_id.get(1..) else {
                return Err(artifact_invalid());
            };
            let mut finding: Finding = source.clone();
            finding.id = format!("{}{suffix}", reviewer.finding_prefix);
            finding.reviewer_id.clone_from(&reviewer.id);
            finding.reviewer_label.clone_from(&reviewer.label);
            artifact.findings.push(finding);
        }
        for test in &result.tests {
            artifact.tests.push(format!("{}: {test}", reviewer.label));
        }
    }
    if count_over(artifact.findings.len(), cfg.max_findings) {
        return Err(artifact_invalid());
    }
    artifact.prior_results = combine_prior(&input.prior, results);
    Ok(artifact)
}

/// Settles each prior finding on the most cautious verdict that a reviewer gave it.
///
/// A prior finding that no reviewer settled is open. Among equally cautious verdicts the first
/// one given wins.
pub(crate) fn combine_prior(
    prior: &[PriorFinding],
    results: &[ReviewerResult],
) -> Vec<PriorResult> {
    let mut out = Vec::new();
    for finding in prior {
        let mut chosen: Option<PriorVerdict> = None;
        let mut chosen_rank = 255u8;
        for result in results {
            for verdict in &result.prior {
                if verdict.key == finding.key {
                    let rank = prior_rank(&verdict.status);
                    if chosen.is_none() || rank < chosen_rank {
                        chosen = Some(verdict.clone());
                        chosen_rank = rank;
                    }
                }
            }
        }
        let verdict = chosen.unwrap_or(PriorVerdict {
            key: finding.key.clone(),
            status: PRIOR_OPEN.to_owned(),
            note: NOT_SETTLED_NOTE.to_owned(),
            relies_on: String::new(),
        });
        out.push(PriorResult {
            finding: finding.clone(),
            verdict,
            ..PriorResult::default()
        });
    }
    out
}

/// Rank of a verdict status, the lower the more cautious. An unknown status ranks like an open
/// one, as the zero value of Go's rank map does.
fn prior_rank(status: &str) -> u8 {
    match status {
        s if s == PRIOR_FIXED => 2,
        s if s == PRIOR_ANSWERED => 1,
        _ => 0,
    }
}

/// The artifact that a combination refuses.
fn artifact_invalid() -> Error {
    Error::workflow("artifact_invalid", true, None)
}

#[cfg(test)]
mod tests {
    use time::OffsetDateTime;

    use crate::model::{Finding, ReviewerResult};
    use crate::protocol::fixtures::{opus, test_input, test_pull_request};
    use crate::protocol::validate::tests::test_config;

    use super::combine_results;

    #[test]
    fn combine_results_prefixes_findings_per_reviewer() {
        let cfg = test_config();
        let mut sonnet = opus();
        sonnet.id = "sonnet".to_owned();
        sonnet.label = "Sonnet".to_owned();
        sonnet.finding_prefix = "S".to_owned();
        let mut input = test_input(&opus(), test_pull_request(4));
        input.reviewers = vec![opus(), sonnet];
        let results = vec![
            ReviewerResult {
                reviewer_id: "opus".to_owned(),
                summary: "opus summary".to_owned(),
                findings: vec![Finding {
                    source_id: "C001".to_owned(),
                    title: "a".to_owned(),
                    ..Finding::default()
                }],
                ..ReviewerResult::default()
            },
            ReviewerResult {
                reviewer_id: "sonnet".to_owned(),
                summary: "sonnet summary".to_owned(),
                findings: vec![Finding {
                    source_id: "C001".to_owned(),
                    title: "b".to_owned(),
                    ..Finding::default()
                }],
                ..ReviewerResult::default()
            },
        ];
        let artifact = combine_results(&cfg, &input, &results, OffsetDateTime::UNIX_EPOCH)
            .expect("results in reviewer order combine");
        assert_eq!(artifact.findings.len(), 2);
        assert_eq!(artifact.findings[0].id, "O001");
        assert_eq!(artifact.findings[1].id, "S001");
        assert_eq!(artifact.findings[1].reviewer_label, "Sonnet");
        assert_eq!(artifact.summaries.len(), 2);

        let reversed = vec![results[1].clone(), results[0].clone()];
        assert!(combine_results(&cfg, &input, &reversed, OffsetDateTime::UNIX_EPOCH).is_err());
    }
}
