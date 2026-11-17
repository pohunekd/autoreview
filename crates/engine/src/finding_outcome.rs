//! The outcome of each published finding, derived from the reviews that publish and settle it.

// Rust guideline compliant 2026-06-26

use autoreview_go_compat::go_time::format_rfc3339_nano;
use time::OffsetDateTime;

use crate::engine::{Clock, Engine};
use crate::error::error_code;
use crate::integrations::{GitHubReview, Integrations, PullRequestSignals};
use crate::model::review::{PRIOR_ANSWERED, PRIOR_FIXED, PRIOR_OPEN, prior_key};
use crate::model::{Artifact, Decision, PublishResult, ReviewInput};
use crate::store::signals::{SignalIds, SignalRow, SignalUpdate};
use crate::store::{
    FindingCategory, FindingOutcome, OUTCOME_ANSWERED, OUTCOME_FIXED, OUTCOME_OPEN, Settlement,
};
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
            &settlements([workflow_id, origin], artifact),
            &published,
        );
        if let Err(err) = recorded {
            tracing::error!(
                review_id = %input.pull_request.review_id(),
                error_code = error_code(&err),
                error = %err,
                "finding outcomes not recorded"
            );
            return;
        }
        // Without the `guidance` section nothing is counted, so nothing is kept for it.
        if self.config.guidance.is_none() {
            return;
        }
        let categories = published_categories(artifact);
        if categories.is_empty() {
            return;
        }
        let stored =
            (self.store).record_categories(&artifact.repository, artifact.number, &categories);
        if let Err(err) = stored {
            tracing::error!(
                review_id = %input.pull_request.review_id(),
                error_code = error_code(&err),
                error = %err,
                "finding categories not recorded"
            );
        }
    }

    /// Stores the GitHub review ID and the finding IDs of a published review beside the outcome
    /// rows of its findings.
    ///
    /// It runs only with `schedule.signal_check_interval_seconds` and makes no GitHub call: the
    /// publish answer carries the review ID. A store failure is logged and never fails the review.
    pub(crate) fn record_signal_ids(
        &self,
        input: &ReviewInput,
        review: &GitHubReview,
        result: &PublishResult,
        artifact: &Artifact,
    ) {
        if self.config.signal_check_interval.is_none() || result.github_review_id <= 0 {
            return;
        }
        let ids = signal_ids(artifact, review, result);
        let recorded = (self.store).record_signal_ids(&artifact.repository, artifact.number, &ids);
        if let Err(err) = recorded {
            tracing::error!(
                review_id = %input.pull_request.review_id(),
                error_code = error_code(&err),
                error = %err,
                "finding signal IDs not recorded"
            );
        }
    }
}

/// Reports whether the inline comment `body` is the one of finding `id`.
///
/// The heading opens a comment as `**ID `, whatever the severity says after it.
pub(crate) fn heads_comment(body: &str, id: &str) -> bool {
    let rest = body
        .strip_prefix("**")
        .and_then(|rest| rest.strip_prefix(id));
    !id.is_empty() && rest.is_some_and(|rest| rest.starts_with(' '))
}

/// Returns the signal IDs of each finding of `artifact`, by the key that tracks its row.
///
/// A finding keeps its ID only when an inline comment of `review` is headed by it.
fn signal_ids(
    artifact: &Artifact,
    review: &GitHubReview,
    result: &PublishResult,
) -> Vec<SignalIds> {
    let requested_changes = result.decision == Decision::RequestChanges;
    let mut ids = Vec::new();
    for finding in &artifact.findings {
        let commented =
            (review.comments.iter()).any(|comment| heads_comment(&comment.body, &finding.id));
        ids.push(SignalIds {
            tracked_key: prior_key(&artifact.head_sha, &finding.id),
            review_id: result.github_review_id,
            head_sha: artifact.head_sha.clone(),
            finding_id: if commented {
                finding.id.clone()
            } else {
                String::new()
            },
            requested_changes,
        });
    }
    ids
}

/// GitHub state of a review that a person or a branch protection rule dismissed.
const REVIEW_DISMISSED: &str = "DISMISSED";

/// Returns what one signal read changes for `rows`, the findings of its pull request.
///
/// An open finding of a review that requested changes becomes `dismissed` when GitHub reports
/// that review dismissed, whatever the head: GitHub dismisses stale approvals on a push, never
/// a request for changes. A finding's reactions are those of the pull request author to the one
/// thread of its review whose comment its heading opens, sorted; without exactly one such
/// thread the stored ones stay. A review the read does not name changes nothing.
pub(crate) fn decide_signals(
    rows: &[SignalRow],
    signals: &PullRequestSignals,
) -> Vec<SignalUpdate> {
    let mut updates = Vec::new();
    for row in rows {
        let state = signals.reviews.get(&row.review_id);
        let dismissed = row.outcome == OUTCOME_OPEN
            && row.requested_changes
            && state.is_some_and(|state| state == REVIEW_DISMISSED);
        let mut threads = (signals.threads.iter()).filter(|thread| {
            thread.review_id == row.review_id && heads_comment(&thread.body, &row.finding_id)
        });
        let reactions = match (threads.next(), threads.next()) {
            (Some(thread), None) if !signals.author.is_empty() => {
                let own = (thread.reactions.iter())
                    .filter(|reaction| reaction.login.eq_ignore_ascii_case(&signals.author));
                let mut own: Vec<String> = own.map(|reaction| reaction.content.clone()).collect();
                own.sort();
                own.dedup();
                own
            }
            _ => row.author_reactions.clone(),
        };
        if dismissed || reactions != row.author_reactions {
            updates.push(SignalUpdate {
                finding_key: row.finding_key.clone(),
                review_id: row.review_id,
                finding_id: row.finding_id.clone(),
                dismissed,
                reactions,
            });
        }
    }
    updates
}

/// Reports whether finding `id` of `artifact` publishes a prior finding again.
fn republished(artifact: &Artifact, id: &str) -> bool {
    (artifact.prior_results)
        .iter()
        .any(|result| !result.republished.is_empty() && result.republished == id)
}

/// Returns the category and body of each finding of `artifact` that has a category, except those
/// that publish a prior one again.
fn published_categories(artifact: &Artifact) -> Vec<FindingCategory> {
    (artifact.findings)
        .iter()
        .filter(|finding| !finding.category.is_empty() && !republished(artifact, &finding.id))
        .map(|finding| FindingCategory {
            finding_key: prior_key(&artifact.head_sha, &finding.id),
            category: finding.category.clone(),
            body: finding.body.clone(),
        })
        .collect()
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
    let mut outcomes = Vec::new();
    for finding in &artifact.findings {
        if republished(artifact, &finding.id) {
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
/// that key that no review in `kept` published: they belong to another finding.
fn settlements(kept: [&str; 2], artifact: &Artifact) -> Vec<Settlement> {
    let kept = kept.map(str::to_owned);
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
            let kept = kept.clone();
            settlements.push(Settlement::Untracked { key, kept });
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

    use super::{decide_signals, settlements, signal_ids};
    use crate::integrations::{
        CommentReaction, GitHubReview, InlineComment, PullRequestSignals, ThreadComment,
    };
    use crate::model::{Decision, PublishResult};
    use crate::store::signals::{SignalRow, SignalUpdate};

    /// A signal read dismisses an open finding only for a dismissed request for changes, and
    /// keeps the reactions of the pull request author to the one thread that its heading opens.
    #[test]
    fn a_signal_read_dismisses_open_findings_and_keeps_the_authors_reactions() {
        let row = |outcome: &str, requested: bool, reactions: &[&str]| SignalRow {
            finding_key: "a:O001".to_owned(),
            outcome: outcome.to_owned(),
            review_id: 7,
            finding_id: "O001".to_owned(),
            requested_changes: requested,
            author_reactions: reactions.iter().map(|text| (*text).to_owned()).collect(),
        };
        let reaction = |login: &str, content: &str| CommentReaction {
            login: login.to_owned(),
            content: content.to_owned(),
        };
        let thread = |review: i64, heading: &str, reactions: &[CommentReaction]| ThreadComment {
            review_id: review,
            body: format!("**{heading} — Bug** (confidence: high)\n\nBody."),
            reactions: reactions.to_vec(),
        };
        let read = |state: &str, threads: &[ThreadComment]| PullRequestSignals {
            author: "Dev".to_owned(),
            reviews: [(7, state.to_owned())].into(),
            threads: threads.to_vec(),
        };
        let update = |dismissed: bool, reactions: &[&str]| SignalUpdate {
            finding_key: "a:O001".to_owned(),
            review_id: 7,
            finding_id: "O001".to_owned(),
            dismissed,
            reactions: reactions.iter().map(|text| (*text).to_owned()).collect(),
        };
        let (gone, standing) = (read("DISMISSED", &[]), read("CHANGES_REQUESTED", &[]));
        let both = [
            reaction("dev", "-1"),
            reaction("other", "+1"),
            reaction("DEV", "eyes"),
            reaction("dev", "-1"),
        ];
        let own = [thread(7, "O001 BLOCKER", &both)];
        let cases = [
            (
                "dismissed request",
                row("open", true, &[]),
                gone.clone(),
                vec![update(true, &[])],
            ),
            (
                "comment-only review",
                row("open", false, &[]),
                gone.clone(),
                vec![],
            ),
            ("decided finding", row("fixed", true, &[]), gone, vec![]),
            (
                "request still standing",
                row("open", true, &[]),
                standing,
                vec![],
            ),
            (
                "unknown review",
                row("open", true, &[]),
                PullRequestSignals {
                    reviews: [(8, "DISMISSED".to_owned())].into(),
                    ..PullRequestSignals::default()
                },
                vec![],
            ),
            (
                "author and other reactions",
                row("open", true, &[]),
                read("CHANGES_REQUESTED", &own),
                vec![update(false, &["-1", "eyes"])],
            ),
            (
                "removed reaction",
                row("open", true, &["-1"]),
                read(
                    "CHANGES_REQUESTED",
                    &[thread(7, "O001 HIGH", &[reaction("other", "+1")])],
                ),
                vec![update(false, &[])],
            ),
            (
                "thread of another review or finding",
                row("open", true, &["-1"]),
                read(
                    "CHANGES_REQUESTED",
                    &[thread(8, "O001 HIGH", &[]), thread(7, "O010 HIGH", &[])],
                ),
                vec![],
            ),
            (
                "two threads for one finding",
                row("open", true, &["-1"]),
                read("CHANGES_REQUESTED", &[own[0].clone(), own[0].clone()]),
                vec![],
            ),
            (
                "reaction and dismissal",
                row("open", true, &[]),
                read(
                    "DISMISSED",
                    &[thread(7, "O001 HIGH", &[reaction("dev", "confused")])],
                ),
                vec![update(true, &["confused"])],
            ),
        ];
        for (name, row, signals, want) in cases {
            assert_eq!(decide_signals(&[row], &signals), want, "{name}");
        }
    }

    /// A finding keeps its ID only for the inline comment its heading opens, whatever the
    /// severity says; a longer ID that starts the same does not match.
    #[test]
    fn a_finding_keeps_its_id_only_for_the_comment_its_heading_opens() {
        let finding = |id: &str| Finding {
            id: id.to_owned(),
            ..Finding::default()
        };
        let artifact = Artifact {
            head_sha: sha("c"),
            findings: ["O001", "O002", "O003", "O01"].map(finding).to_vec(),
            ..Artifact::default()
        };
        let comment = |heading: &str| InlineComment {
            body: format!("**{heading} — Bug** (confidence: high)\n\nBody."),
            ..InlineComment::default()
        };
        let review = GitHubReview {
            comments: ["O001 HIGH", "O002 BLOCKER", "O010 LOW"]
                .map(comment)
                .to_vec(),
            ..GitHubReview::default()
        };
        for (decision, requested) in [(Decision::RequestChanges, true), (Decision::Comment, false)]
        {
            let result = PublishResult {
                decision,
                github_review_id: 7,
                ..PublishResult::default()
            };
            let ids = signal_ids(&artifact, &review, &result);
            let found: Vec<_> = ids
                .iter()
                .map(|ids| (ids.finding_id.as_str(), ids.requested_changes))
                .collect();
            let want = [("O001", requested), ("O002", requested)];
            assert_eq!(found[..2], want, "{requested}");
            assert_eq!(found[2..], [("", requested), ("", requested)]);
            assert!(ids.iter().all(|ids| ids.review_id == 7));
            assert_eq!(ids[0].tracked_key, prior_key(&sha("c"), "O001"));
        }
    }

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
            let settled = settlements(["w1", "w0"], &artifact(status, republished, check));
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
                kept: ["w1".to_owned(), "w0".to_owned()],
            },
        ];
        assert_eq!(settlements(["w1", "w0"], &reused), want, "a reused key");
    }
}
