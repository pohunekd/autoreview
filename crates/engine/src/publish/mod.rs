//! Publication of a review: anchoring, the publication plan, the marker and neutralization.

// Rust guideline compliant 2026-06-26

pub(crate) mod anchor;
pub(crate) mod neutralize;
pub(crate) mod plan;
pub(crate) mod render;

use sha2::{Digest, Sha256};

use crate::model::{Decision, Finding, PriorResult, PullRequest};

/// Review marker of one publication, which a merge gate reads without parsing the body.
///
/// It carries the decision and the number of findings the caller passes, so a clean review
/// differs from one with blocking findings even when GitHub forces COMMENT. The caller passes
/// only the blocking findings. An open prior finding is counted under its key.
pub(crate) fn review_marker(
    pr: &PullRequest,
    decision: &Decision,
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

/// Marker of one publication: the legacy marker, then the marker of the base branch.
///
/// A lookup by this marker never finds a review published for another base branch. A reader
/// of the legacy marker, such as the Go engine or a merge gate, still finds it unchanged.
pub(crate) fn publication_marker(
    pr: &PullRequest,
    decision: &Decision,
    findings: &[Finding],
    open: &[PriorResult],
) -> String {
    let legacy = review_marker(pr, decision, findings, open);
    format!("{legacy}{}", base_marker(&pr.base_branch))
}

/// Hidden marker that names the base branch of a publication by its SHA-256 digest.
///
/// The digest keeps the branch name out of the body and gives the marker a fixed length.
pub(crate) fn base_marker(base_branch: &str) -> String {
    let digest = hex::encode(Sha256::digest(base_branch.as_bytes()));
    format!("<!-- hermes-pr-review-base:v1:{digest} -->")
}

/// Text of a decision as the marker and the GitHub review carry it.
pub(crate) fn decision_text(decision: &Decision) -> &str {
    match decision {
        Decision::Approve => "APPROVE",
        Decision::RequestChanges => "REQUEST_CHANGES",
        Decision::Comment => "COMMENT",
        Decision::Unset => "",
        Decision::Other(text) => text,
    }
}

#[cfg(test)]
mod tests {
    use super::{base_marker, publication_marker, review_marker};
    use crate::model::{Decision, Finding, PullRequest};

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
        let approve = review_marker(&pr, &Decision::Approve, &[], &[]);
        let changes = review_marker(&pr, &Decision::RequestChanges, &[finding("O001")], &[]);
        assert!(
            approve != changes && approve.starts_with("<!-- hermes-pr-review:v1:acme/widget#5@"),
            "markers {approve:?} and {changes:?}"
        );
        assert!(
            approve.contains(":decision=APPROVE:findings=0:digest=")
                && changes.contains(":decision=REQUEST_CHANGES:findings=1:digest="),
            "markers must carry the decision and the finding count: {approve:?} and {changes:?}"
        );
        let clean_comment = review_marker(&pr, &Decision::Comment, &[], &[]);
        let comment_with_findings = review_marker(
            &pr,
            &Decision::Comment,
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

    /// Each row is a base branch and the SHA-256 digest that its marker carries (#297).
    #[test]
    fn publication_marker_names_the_base_branch_by_its_digest() {
        let cases: [(&str, &str); 5] = [
            (
                "main",
                "0d6e4079e36703ebd37c00722f5891d28b0e2811dc114b129215123adcce3605",
            ),
            (
                "release",
                "a4d451ec23463726f72c43d64c710968f6b602cd653b4de8adee1b556240a829",
            ),
            (
                "release/2.0",
                "8b092cafaff1f6fa8ea25b5a660c1d66f608e26530fb7389cd83992b251503f4",
            ),
            (
                "",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                "x --> <!-- @octocat",
                "b81178c3fd7d34af561aa3ff97a66425cd9618bd6a1ea92dad7fde6b9a136f26",
            ),
        ];
        let mut markers = Vec::new();
        for (base, digest) in cases {
            let mut pr = test_pull_request(5);
            pr.base_branch = base.to_owned();
            let legacy = review_marker(&pr, &Decision::Approve, &[], &[]);
            let marker = publication_marker(&pr, &Decision::Approve, &[], &[]);
            assert_eq!(
                (marker.clone(), base_marker(base).len()),
                (
                    format!("{legacy}<!-- hermes-pr-review-base:v1:{digest} -->"),
                    98
                ),
                "the legacy marker must stay first and the base must be a digest: {base:?}"
            );
            markers.push(marker);
        }
        for (index, marker) in markers.iter().enumerate() {
            for (other, found) in markers.iter().enumerate() {
                assert_eq!(
                    found.contains(marker.as_str()),
                    index == other,
                    "a marker must find only its own base branch: {marker} in {found}"
                );
            }
        }
    }
}
