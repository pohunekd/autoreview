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
use crate::protocol::validate::{CONFIDENCE_LEVELS, count_over};
use crate::publish::render::LABEL_SEPARATOR;

/// Note of a prior finding that no reviewer settled.
const NOT_SETTLED_NOTE: &str = "No reviewer settled this finding.";

/// Combines the reviewer results, given in reviewer order, into one artifact.
///
/// Each finding takes the prefix of its reviewer, so `O001` and `S001` never collide. Findings
/// of different reviewers that `same_finding` matches are merged into one, the artifact keeps
/// the summary of the first reviewer, and the tests form one list without exact duplicates.
///
/// # Errors
///
/// Returns `artifact_invalid` when the results do not match the reviewers one to one, when a
/// source identifier is too short to take a prefix, or when the merged findings exceed the
/// limit.
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
    let mut merged: Vec<MergedFinding> = Vec::new();
    for (reviewer, result) in input.reviewers.iter().zip(results) {
        if result.reviewer_id != reviewer.id {
            return Err(artifact_invalid());
        }
        if artifact.summaries.is_empty() {
            artifact.summaries.push(ReviewerSummary {
                reviewer_id: reviewer.id.clone(),
                label: reviewer.label.clone(),
                summary: result.summary.clone(),
            });
        }
        for source in &result.findings {
            let Some(suffix) = source.source_id.get(1..) else {
                return Err(artifact_invalid());
            };
            let mut finding: Finding = source.clone();
            finding.id = format!("{}{suffix}", reviewer.finding_prefix);
            finding.reviewer_id.clone_from(&reviewer.id);
            finding.reviewer_label.clone_from(&reviewer.label);
            add_finding(&mut merged, finding);
        }
        for test in &result.tests {
            if !artifact.tests.contains(test) {
                artifact.tests.push(test.clone());
            }
        }
    }
    artifact.findings = merged
        .into_iter()
        .map(MergedFinding::into_finding)
        .collect();
    if count_over(artifact.findings.len(), cfg.max_findings) {
        return Err(artifact_invalid());
    }
    artifact.prior_results = combine_prior(&input.prior, results);
    Ok(artifact)
}

/// A finding with the reviewers that reported it, in reviewer order.
struct MergedFinding {
    /// The merged finding, labelled by its first reviewer until `into_finding`.
    finding: Finding,
    /// Identifiers of the reviewers that reported the finding.
    reviewer_ids: Vec<String>,
    /// Labels of the reviewers that reported the finding.
    labels: Vec<String>,
}

impl MergedFinding {
    /// Returns the finding labelled with every reviewer that reported it.
    fn into_finding(self) -> Finding {
        let mut finding = self.finding;
        finding.reviewer_label = self.labels.join(LABEL_SEPARATOR);
        finding
    }
}

/// Merges `finding` into its first duplicate of another reviewer, or adds it.
///
/// The merged finding keeps its identifier and first reviewer, takes the higher confidence and
/// the longer body with its title, and blocks when either finding blocks.
fn add_finding(merged: &mut Vec<MergedFinding>, finding: Finding) {
    let target = merged.iter_mut().find(|candidate| {
        !candidate.reviewer_ids.contains(&finding.reviewer_id)
            && same_finding(&candidate.finding, &finding)
    });
    let Some(target) = target else {
        merged.push(MergedFinding {
            reviewer_ids: vec![finding.reviewer_id.clone()],
            labels: vec![finding.reviewer_label.clone()],
            finding,
        });
        return;
    };
    let kept = &mut target.finding;
    if confidence_rank(&finding.confidence) < confidence_rank(&kept.confidence) {
        kept.confidence.clone_from(&finding.confidence);
    }
    if finding.body.chars().count() > kept.body.chars().count() {
        kept.title.clone_from(&finding.title);
        kept.body.clone_from(&finding.body);
    }
    kept.non_blocking = kept.non_blocking && finding.non_blocking;
    target.reviewer_ids.push(finding.reviewer_id);
    target.labels.push(finding.reviewer_label);
}

/// Reports whether two findings name the same defect: the same path, line and severity.
///
/// A finding names one line, so two line ranges of one line overlap only when they are equal.
fn same_finding(left: &Finding, right: &Finding) -> bool {
    left.path == right.path && left.line == right.line && left.severity == right.severity
}

/// Rank of a confidence level, the lower the more confident. An unknown level ranks last.
fn confidence_rank(confidence: &str) -> usize {
    CONFIDENCE_LEVELS
        .iter()
        .position(|candidate| *candidate == confidence)
        .unwrap_or(CONFIDENCE_LEVELS.len())
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

    use crate::model::{Finding, ReviewInput, ReviewerResult};
    use crate::protocol::fixtures::{opus, test_input, test_pull_request};
    use crate::protocol::validate::tests::test_config;

    use super::combine_results;

    /// Input of a rule that runs Opus and then Sonnet.
    fn two_reviewers() -> ReviewInput {
        let mut sonnet = opus();
        sonnet.id = "sonnet".to_owned();
        sonnet.label = "Sonnet".to_owned();
        sonnet.finding_prefix = "S".to_owned();
        let mut input = test_input(&opus(), test_pull_request(4));
        input.reviewers = vec![opus(), sonnet];
        input
    }

    /// Result of `reviewer` with `findings` and `tests`.
    fn result(reviewer: &str, findings: Vec<Finding>, tests: &[&str]) -> ReviewerResult {
        ReviewerResult {
            reviewer_id: reviewer.to_owned(),
            summary: format!("{reviewer} summary"),
            findings,
            tests: tests.iter().map(|test| (*test).to_owned()).collect(),
            ..ReviewerResult::default()
        }
    }

    /// Finding `source_id` at `path:line` with `severity`, confidence and body.
    fn finding(source_id: &str, path: &str, line: i64, severity: &str) -> Finding {
        Finding {
            source_id: source_id.to_owned(),
            path: path.to_owned(),
            line,
            severity: severity.to_owned(),
            confidence: "medium".to_owned(),
            title: format!("{source_id} title"),
            body: format!("{source_id} body"),
            ..Finding::default()
        }
    }

    #[test]
    fn combine_results_prefixes_findings_per_reviewer() {
        let cfg = test_config();
        let input = two_reviewers();
        let results = vec![
            result("opus", vec![finding("C001", "a.go", 1, "high")], &[]),
            result("sonnet", vec![finding("C001", "a.go", 2, "high")], &[]),
        ];
        let artifact = combine_results(&cfg, &input, &results, OffsetDateTime::UNIX_EPOCH)
            .expect("results in reviewer order combine");
        assert_eq!(artifact.findings.len(), 2);
        assert_eq!(artifact.findings[0].id, "O001");
        assert_eq!(artifact.findings[1].id, "S001");
        assert_eq!(artifact.findings[1].reviewer_label, "Sonnet");

        let reversed = vec![results[1].clone(), results[0].clone()];
        assert!(combine_results(&cfg, &input, &reversed, OffsetDateTime::UNIX_EPOCH).is_err());
    }

    #[test]
    fn duplicate_findings_merge_into_the_first_reviewers_finding() {
        let cfg = test_config();
        let mut first = finding("C002", "a.go", 7, "high");
        first.confidence = "low".to_owned();
        first.non_blocking = true;
        let mut second = finding("C001", "a.go", 7, "high");
        second.confidence = "high".to_owned();
        second.title = "Longer".to_owned();
        second.body = "A longer body.".to_owned();
        let results = vec![
            result("opus", vec![first], &[]),
            result("sonnet", vec![second], &[]),
        ];
        let artifact =
            combine_results(&cfg, &two_reviewers(), &results, OffsetDateTime::UNIX_EPOCH)
                .expect("duplicate findings combine");
        let want = Finding {
            id: "O002".to_owned(),
            source_id: "C002".to_owned(),
            reviewer_id: "opus".to_owned(),
            reviewer_label: "Opus + Sonnet".to_owned(),
            severity: "high".to_owned(),
            confidence: "high".to_owned(),
            path: "a.go".to_owned(),
            line: 7,
            title: "Longer".to_owned(),
            body: "A longer body.".to_owned(),
            non_blocking: false,
            ..Finding::default()
        };
        assert_eq!(artifact.findings, vec![want]);
    }

    #[test]
    fn findings_that_differ_in_path_line_or_severity_stay_apart() {
        let cfg = test_config();
        let cases = [
            ("b.go", 7, "high"),
            ("a.go", 8, "high"),
            ("a.go", 7, "medium"),
        ];
        for (path, line, severity) in cases {
            let results = vec![
                result("opus", vec![finding("C001", "a.go", 7, "high")], &[]),
                result("sonnet", vec![finding("C001", path, line, severity)], &[]),
            ];
            let artifact =
                combine_results(&cfg, &two_reviewers(), &results, OffsetDateTime::UNIX_EPOCH)
                    .expect("distinct findings combine");
            let ids: Vec<&str> = artifact.findings.iter().map(|f| f.id.as_str()).collect();
            assert_eq!(ids, ["O001", "S001"], "{path}:{line} {severity}");
        }
    }

    #[test]
    fn findings_of_one_reviewer_never_merge_with_each_other() {
        let cfg = test_config();
        let results = vec![
            result(
                "opus",
                vec![
                    finding("C001", "a.go", 7, "high"),
                    finding("C002", "a.go", 7, "high"),
                ],
                &[],
            ),
            result(
                "sonnet",
                vec![
                    finding("C001", "a.go", 7, "high"),
                    finding("C002", "a.go", 7, "high"),
                ],
                &[],
            ),
        ];
        let artifact =
            combine_results(&cfg, &two_reviewers(), &results, OffsetDateTime::UNIX_EPOCH)
                .expect("findings of one reviewer combine");
        let got: Vec<(&str, &str)> = artifact
            .findings
            .iter()
            .map(|f| (f.id.as_str(), f.reviewer_label.as_str()))
            .collect();
        assert_eq!(got, [("O001", "Opus + Sonnet"), ("O002", "Opus + Sonnet")]);
    }

    #[test]
    fn body_tie_keeps_the_first_text_and_length_counts_characters() {
        let cfg = test_config();
        let mut tie = finding("C001", "a.go", 7, "high");
        tie.body = "same".to_owned();
        let mut other = finding("C001", "a.go", 7, "high");
        other.title = "Other".to_owned();
        other.body = "four".to_owned();
        let mut wide = finding("C002", "a.go", 9, "high");
        wide.body = "ééé".to_owned();
        let mut narrow = finding("C002", "a.go", 9, "high");
        narrow.body = "abcd".to_owned();
        let results = vec![
            result("opus", vec![tie, wide], &[]),
            result("sonnet", vec![other, narrow], &[]),
        ];
        let artifact =
            combine_results(&cfg, &two_reviewers(), &results, OffsetDateTime::UNIX_EPOCH)
                .expect("tied findings combine");
        assert_eq!(artifact.findings[0].title, "C001 title");
        assert_eq!(artifact.findings[0].body, "same");
        assert_eq!(artifact.findings[1].body, "abcd");
    }

    #[test]
    fn one_summary_and_one_list_of_tests_without_duplicates() {
        let cfg = test_config();
        let results = vec![
            result("opus", Vec::new(), &["go test ./...", "go vet ./..."]),
            result("sonnet", Vec::new(), &["go vet ./...", "Go test ./..."]),
        ];
        let artifact =
            combine_results(&cfg, &two_reviewers(), &results, OffsetDateTime::UNIX_EPOCH)
                .expect("clean results combine");
        assert_eq!(artifact.summaries.len(), 1);
        assert_eq!(artifact.summaries[0].label, "Opus");
        assert_eq!(artifact.summaries[0].summary, "opus summary");
        assert_eq!(
            artifact.tests,
            ["go test ./...", "go vet ./...", "Go test ./..."]
        );
    }

    #[test]
    fn finding_limit_counts_the_merged_findings() {
        let mut cfg = test_config();
        cfg.max_findings = 1;
        let results = vec![
            result("opus", vec![finding("C001", "a.go", 7, "high")], &[]),
            result("sonnet", vec![finding("C001", "a.go", 7, "high")], &[]),
        ];
        let input = two_reviewers();
        let artifact = combine_results(&cfg, &input, &results, OffsetDateTime::UNIX_EPOCH)
            .expect("one merged finding is within the limit");
        assert_eq!(artifact.findings.len(), 1);

        let distinct = vec![
            result("opus", vec![finding("C001", "a.go", 7, "high")], &[]),
            result("sonnet", vec![finding("C001", "a.go", 8, "high")], &[]),
        ];
        assert!(combine_results(&cfg, &input, &distinct, OffsetDateTime::UNIX_EPOCH).is_err());
    }
}
