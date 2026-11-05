//! Settlement of prior findings: what the reviewers' verdicts cannot decide alone.

// Rust guideline compliant 2026-06-26

use autoreview_policy::OnClean;

use crate::model::review::{PRIOR_ANSWERED, PRIOR_OPEN};
use crate::model::{Artifact, Discussion, Finding, PriorResult, ReviewInput};

/// Prefix of the identifiers of republished prior findings.
///
/// It keeps them apart from the findings of the reviewers.
const REPUBLISHED_PREFIX: &str = "P";

/// Characters of the head SHA that a republished finding names.
const REPUBLISHED_HEAD_CHARS: usize = 12;

/// Settles the prior findings of `artifact` with the discussion.
///
/// An answered verdict stays answered only on a rule that waits for the user, or when the
/// publishing account wrote the comment it relies on. Otherwise it becomes an open verdict.
/// An open verdict without an unresolved thread is published again as a new finding.
#[cfg_attr(not(test), expect(dead_code, reason = "called by S28"))]
pub(crate) fn settle_prior(
    input: &ReviewInput,
    discussion: &Discussion,
    mut artifact: Artifact,
) -> Artifact {
    if artifact.prior_results.is_empty() {
        return artifact;
    }
    let mut results = artifact.prior_results.clone();
    let mut findings = artifact.findings.clone();
    let mut republished = 0;
    for result in &mut results {
        if result.verdict.status == PRIOR_ANSWERED
            && input.rule.on_clean == OnClean::Approve
            && !decided_by_reviewer(discussion, result, &input.publisher_login)
        {
            result.verdict.status = PRIOR_OPEN.to_owned();
            let prefix = concat!(
                "Settled in the discussion, but a rule that approves ",
                "without the user needs a comment of the reviewing account itself: "
            );
            let mut note = String::new();
            note.push_str(prefix);
            note.push_str(&result.verdict.note);
            result.verdict.note = note;
        }
        if result.verdict.status != PRIOR_OPEN {
            continue;
        }
        if discussion.open_thread(&result.finding.key) {
            continue;
        }
        republished += 1;
        result.republished = format!("{}{:03}", REPUBLISHED_PREFIX, republished);
        let head_short: String = result
            .finding
            .head_sha
            .chars()
            .take(REPUBLISHED_HEAD_CHARS)
            .collect();
        let body = format!(
            concat!(
                "Still open from {} of the review of `{}`, ",
                "whose thread is resolved or missing.\n\n{}\n\n{}"
            ),
            result.finding.id, head_short, result.verdict.note, result.finding.body
        );
        findings.push(Finding {
            id: result.republished.clone(),
            source_id: result.republished.clone(),
            reviewer_label: "earlier review".to_owned(),
            severity: result.finding.severity.clone(),
            confidence: "high".to_owned(),
            path: result.finding.path.clone(),
            line: result.finding.line,
            title: result.finding.title.clone(),
            body,
            ..Finding::default()
        });
    }
    artifact.prior_results = results;
    artifact.findings = findings;
    artifact
}

/// Reports whether the verdict relies on the reviewer's own comment.
fn decided_by_reviewer(discussion: &Discussion, result: &PriorResult, publisher: &str) -> bool {
    if result.verdict.relies_on.is_empty() || result.verdict.relies_on != publisher {
        return false;
    }
    for comment in discussion.comments_by(&result.finding.key, publisher) {
        if !comment.ai_signed {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use autoreview_policy::{OnClean, Rule};

    use crate::discussion::{DISCUSSION_ROLE_AUTHOR, DISCUSSION_ROLE_REVIEWER};
    use crate::model::review::{PRIOR_ANSWERED, PRIOR_OPEN, prior_key};
    use crate::model::{
        Artifact, Discussion, DiscussionComment, DiscussionThread, PriorFinding, PriorResult,
        PriorVerdict, ReviewInput,
    };

    use super::settle_prior;

    /// Review input that carries only the fields that settlement reads.
    fn input_for(on_clean: OnClean) -> ReviewInput {
        ReviewInput {
            publisher_login: "reviewer".to_owned(),
            rule: Rule {
                on_clean,
                ..Rule::default()
            },
            ..ReviewInput::default()
        }
    }

    #[test]
    fn answered_closes_a_finding_without_the_user_only_on_the_reviewers_own_comment() {
        let head = "c".repeat(40);
        let key = prior_key(&head, "O001");
        let answered = |login: &str| Artifact {
            findings: Vec::new(),
            prior_results: vec![PriorResult {
                finding: PriorFinding {
                    key: key.clone(),
                    id: "O001".to_owned(),
                    head_sha: head.clone(),
                    ..PriorFinding::default()
                },
                verdict: PriorVerdict {
                    key: key.clone(),
                    status: PRIOR_ANSWERED.to_owned(),
                    note: "Fine.".to_owned(),
                    relies_on: login.to_owned(),
                },
                ..PriorResult::default()
            }],
            ..Artifact::default()
        };
        let discussion = |signed: bool| Discussion {
            threads: vec![DiscussionThread {
                key: key.clone(),
                resolved: false,
                replies: vec![
                    DiscussionComment {
                        author: "colleague".to_owned(),
                        role: DISCUSSION_ROLE_AUTHOR.to_owned(),
                        ai_signed: false,
                        body: String::new(),
                    },
                    DiscussionComment {
                        author: "reviewer".to_owned(),
                        role: DISCUSSION_ROLE_REVIEWER.to_owned(),
                        ai_signed: signed,
                        body: String::new(),
                    },
                ],
            }],
            comments: Vec::new(),
        };
        let approving = input_for(OnClean::Approve);
        let awaiting = input_for(OnClean::Await);
        let cases = [
            (
                "the author's reply on an approving rule",
                &approving,
                "colleague",
                false,
                PRIOR_OPEN,
            ),
            (
                "an AI-signed comment of the reviewing account",
                &approving,
                "reviewer",
                true,
                PRIOR_OPEN,
            ),
            (
                "the reviewing account's own comment",
                &approving,
                "reviewer",
                false,
                PRIOR_ANSWERED,
            ),
            (
                "the author's reply when the user approves",
                &awaiting,
                "colleague",
                false,
                PRIOR_ANSWERED,
            ),
        ];
        for (name, input, login, signed, want) in cases {
            let settled = settle_prior(input, &discussion(signed), answered(login));
            let status = settled.prior_results[0].verdict.status.as_str();
            assert!(
                status == want && settled.findings.is_empty(),
                "{name}: status = {status}, findings = {}",
                settled.findings.len()
            );
        }
    }
}
