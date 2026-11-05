//! Publication of a review: anchoring, the publication plan, the marker and neutralization.

// Rust guideline compliant 2026-06-26

pub(crate) mod anchor;
pub(crate) mod neutralize;
pub(crate) mod plan;

use sha2::{Digest, Sha256};

use crate::model::{Decision, Finding, PriorResult, PullRequest};

/// Review marker of one publication, which a merge gate reads without parsing the body.
///
/// It carries the decision and the number of findings the caller passes, so a clean review
/// differs from one with blocking findings even when GitHub forces COMMENT. The caller passes
/// only the blocking findings. An open prior finding is counted under its key.
#[cfg_attr(not(test), expect(dead_code, reason = "called by S12"))]
pub(crate) fn review_marker(
    pr: &PullRequest,
    decision: Decision,
    findings: &[Finding],
    open: &[PriorResult],
) -> String {
    let mut ids: Vec<String> = Vec::with_capacity(findings.len() + open.len());
    for finding in findings {
        ids.push(finding.id.clone());
    }
    for result in open {
        ids.push(result.finding.key.clone());
    }
    ids.sort_unstable();
    let text = format!(
        "{}\n{}\n{}\n{}\n{}",
        pr.repository,
        pr.number,
        pr.head_sha,
        decision_text(decision),
        ids.join(",")
    );
    let digest = Sha256::digest(text.as_bytes());
    let hex_digest = hex::encode(digest);
    format!(
        "<!-- hermes-pr-review:v1:{}#{}@{}:decision={}:findings={}:digest={} -->",
        pr.repository,
        pr.number,
        pr.head_sha,
        decision_text(decision),
        ids.len(),
        hex_digest
    )
}

/// Text of a decision as the marker and the GitHub review carry it.
fn decision_text(decision: Decision) -> &'static str {
    match decision {
        Decision::Approve => "APPROVE",
        Decision::RequestChanges => "REQUEST_CHANGES",
        Decision::Comment => "COMMENT",
        Decision::Unset => "",
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::review_marker;
    use crate::model::{Decision, Finding, PriorResult, PullRequest};

    /// Go-written markers, shared with the Go reference tests.
    const GOLDEN: &str = include_str!("../../../../testdata/parity/marker.json");

    /// One golden case: a named input and the Go output.
    #[derive(Debug, Deserialize)]
    struct Case {
        name: String,
        input: Input,
        output: String,
    }

    /// Input of a golden case; a Go nil slice is `null`.
    #[derive(Debug, Deserialize)]
    struct Input {
        pull_request: PullRequest,
        decision: Decision,
        findings: Option<Vec<Finding>>,
        open_prior: Option<Vec<PriorResult>>,
    }

    fn sha(character: char) -> String {
        character.to_string().repeat(40)
    }

    fn test_pull_request(number: i64) -> PullRequest {
        PullRequest {
            repository: "acme/widget".to_owned(),
            number,
            head_sha: sha('a'),
            review_request_id: 7,
            title: "Add widget".to_owned(),
            url: format!("https://example.invalid/acme/widget/pull/{number}"),
            author: "colleague".to_owned(),
            head_branch: "feature".to_owned(),
            base_branch: "main".to_owned(),
            base_sha: sha('b'),
            ..PullRequest::default()
        }
    }

    fn finding(id: &str) -> Finding {
        Finding {
            id: id.to_owned(),
            ..Finding::default()
        }
    }

    #[test]
    fn review_marker_depends_on_decision_and_findings() {
        let pr = test_pull_request(5);
        let approve = review_marker(&pr, Decision::Approve, &[], &[]);
        let changes = review_marker(&pr, Decision::RequestChanges, &[finding("O001")], &[]);
        assert!(
            approve != changes && approve.starts_with("<!-- hermes-pr-review:v1:acme/widget#5@"),
            "markers {approve:?} and {changes:?}"
        );
        assert!(
            approve.contains(":decision=APPROVE:findings=0:digest=")
                && changes.contains(":decision=REQUEST_CHANGES:findings=1:digest="),
            "markers must carry the decision and the finding count: {approve:?} and {changes:?}"
        );
        let clean_comment = review_marker(&pr, Decision::Comment, &[], &[]);
        let comment_with_findings = review_marker(
            &pr,
            Decision::Comment,
            &[finding("O001"), finding("S001")],
            &[],
        );
        assert!(
            clean_comment.contains(":decision=COMMENT:findings=0:")
                && comment_with_findings.contains(":decision=COMMENT:findings=2:"),
            "a COMMENT on an own pull request must still tell clean from findings: \
             {clean_comment:?} and {comment_with_findings:?}"
        );
    }

    #[test]
    fn marker_matches_go_golden() {
        let cases: Vec<Case> = serde_json::from_str(GOLDEN).expect("golden is JSON");
        assert_eq!(cases.len(), 7, "golden has the seven marker cases");
        for case in &cases {
            let marker = review_marker(
                &case.input.pull_request,
                case.input.decision,
                case.input.findings.as_deref().unwrap_or_default(),
                case.input.open_prior.as_deref().unwrap_or_default(),
            );
            assert_eq!(marker, case.output, "case {}", case.name);
        }
    }
}
