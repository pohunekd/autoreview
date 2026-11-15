//! Publication of a review: anchoring, the publication plan, the marker and neutralization.

// Rust guideline compliant 2026-06-26

pub(crate) mod anchor;
pub(crate) mod neutralize;
pub(crate) mod plan;
pub(crate) mod render;

use sha2::{Digest, Sha256};

use crate::model::{Decision, Finding, PriorResult, PullRequest};

/// Name that the hidden markers of a publication carry, set by `publish.marker`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerName {
    /// `autoreview`, the name of the markers of new deployments.
    Autoreview,
    /// `hermes-pr-review`, the name that existing merge gates read.
    HermesPrReview,
}

impl MarkerName {
    /// Every marker name; a lost acknowledgement is reconciled by each of them.
    pub const ALL: [Self; 2] = [Self::Autoreview, Self::HermesPrReview];

    /// Returns the name as the markers and `publish.marker` write it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Autoreview => "autoreview",
            Self::HermesPrReview => "hermes-pr-review",
        }
    }

    /// Returns the marker name that `text` writes, or `None` for any other text.
    #[must_use]
    pub fn from_name(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|name| name.as_str() == text)
    }
}

/// Returns the opening of the review marker of `name`, `<!-- NAME:v1:`.
fn marker_opening(name: MarkerName) -> String {
    format!("<!-- {}:v1:", name.as_str())
}

/// Returns the opening of the base marker of `name`, `<!-- NAME-base:v1:`.
fn base_opening(name: MarkerName) -> String {
    format!("<!-- {}-base:v1:", name.as_str())
}

/// Returns `marker` and, after it, the same marker under each other name.
///
/// A publication marker that starts with no known name is returned alone. The forms differ in
/// the name only, so a review published under one name is found after `publish.marker` changes.
#[must_use]
pub fn marker_forms(marker: &str) -> Vec<String> {
    let mut forms = vec![marker.to_owned()];
    let found = MarkerName::ALL
        .into_iter()
        .find_map(|name| Some((name, marker.strip_prefix(&marker_opening(name))?)));
    let Some((name, rest)) = found else {
        return forms;
    };
    for other in MarkerName::ALL {
        if other != name {
            let rest = rest.replacen(&base_opening(name), &base_opening(other), 1);
            forms.push(format!("{}{rest}", marker_opening(other)));
        }
    }
    forms
}

/// Review marker of one publication, which a merge gate reads without parsing the body.
///
/// It carries the decision and the number of findings the caller passes, so a clean review
/// differs from one with blocking findings even when GitHub forces COMMENT. The caller passes
/// only the blocking findings. An open prior finding is counted under its key.
pub(crate) fn review_marker(
    name: MarkerName,
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
        "{}{}#{}@{}:decision={}:findings={}:digest={} -->",
        marker_opening(name),
        pr.repository,
        pr.number,
        pr.head_sha,
        decision_text(decision),
        ids.len(),
        hex_digest
    )
}

/// Marker of one publication: the review marker, then the marker of the base branch.
///
/// A lookup by this marker never finds a review published for another base branch. A reader
/// of the review marker, such as a merge gate, still finds it unchanged.
pub(crate) fn publication_marker(
    name: MarkerName,
    pr: &PullRequest,
    decision: &Decision,
    findings: &[Finding],
    open: &[PriorResult],
) -> String {
    let review = review_marker(name, pr, decision, findings, open);
    format!("{review}{}", base_marker(name, &pr.base_branch))
}

/// Hidden marker that names the base branch of a publication by its SHA-256 digest.
///
/// The digest keeps the branch name out of the body and gives the marker a fixed length.
pub(crate) fn base_marker(name: MarkerName, base_branch: &str) -> String {
    let digest = hex::encode(Sha256::digest(base_branch.as_bytes()));
    format!("{}{digest} -->", base_opening(name))
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
