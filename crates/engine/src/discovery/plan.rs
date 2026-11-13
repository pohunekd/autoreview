//! Review planning: the scope, the prior findings, the notes handoff and the review input.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeSet;

use autoreview_policy::{OnClean, Rule};
use tokio_util::sync::CancellationToken;

use crate::engine::{Clock, Engine, STEP_ARTIFACT, STEP_PUBLICATION};
use crate::error::{Error, error_code};
use crate::integrations::Integrations;
use crate::model::ids::{INPUT_VERSION, WORKFLOW_ID_PREFIX};
use crate::model::pull_request::{SCOPE_FULL, SCOPE_INCREMENTAL};
use crate::model::{
    Artifact, Candidate, Decision, NotesHandoff, PriorFinding, ProjectRoute, PublishResult,
    PublishedRecord, PullRequest, ReviewInput, ReviewScope, prior_key, sha_valid,
};

/// Returns the scope of the next review of a pull request head.
///
/// The scope is incremental only when the last published decision leaves nothing open that a
/// review without a human could approve: an approval, a rule that never approves or waits for
/// the user's approval, or a pull request the reviewing account authored. A rule that approves
/// on its own gets a full review after a request for changes.
pub(crate) fn review_scope(
    rule: &Rule,
    published: Option<&PublishedRecord>,
    head: &str,
    own_pull_request: bool,
) -> ReviewScope {
    let Some(published) = published else {
        return full_scope();
    };
    if !sha_valid(&published.head_sha) || published.head_sha == head {
        return full_scope();
    }
    if published.decision == Decision::Approve
        || !rule.approves_clean()
        || rule.on_clean == OnClean::Await
        || own_pull_request
    {
        return ReviewScope {
            mode: SCOPE_INCREMENTAL.to_owned(),
            previous_head_sha: published.head_sha.clone(),
        };
    }
    full_scope()
}

/// Returns the scope of a review that reads the whole pull request.
fn full_scope() -> ReviewScope {
    ReviewScope {
        mode: SCOPE_FULL.to_owned(),
        previous_head_sha: String::new(),
    }
}

impl<I: Integrations, C: Clock> Engine<I, C> {
    /// Decides the scope and the prior findings of the review of `candidate`.
    ///
    /// The prior findings are the blocking findings of the last published review and those it
    /// left open, for a pipeline of either protocol. An incremental scope also needs an unchanged
    /// base branch, that review's stored result and a head that extends the reviewed one
    /// linearly; otherwise the review is full and keeps the prior findings.
    ///
    /// # Errors
    ///
    /// Returns a store error, or the history error when `cancel` is cancelled.
    pub(crate) async fn plan_review(
        &self,
        cancel: &CancellationToken,
        candidate: &Candidate,
        published: Option<&PublishedRecord>,
    ) -> Result<(ReviewScope, Vec<PriorFinding>), Error> {
        let pr = &candidate.pull_request;
        let own = pr.author == self.config.github_login;
        let scope = review_scope(&candidate.rule, published, &pr.head_sha, own);
        let Some(published) = published else {
            return Ok((scope, Vec::new()));
        };
        // A retarget of the published head keeps its findings: the full review settles them.
        if published.head_sha == pr.head_sha && !published.retargeted(pr) {
            return Ok((scope, Vec::new()));
        }
        let stored = self.prior_findings(published)?;
        if !scope.incremental() {
            return Ok((scope, stored.unwrap_or_default()));
        }
        if published.retargeted(pr) {
            // Commits that joined the diff through the new base were never reviewed.
            tracing::info!(
                review_id = %pr.review_id(),
                reason = "base_branch_changed",
                previous_base_branch = %published.base_branch,
                base_branch = %pr.base_branch,
                "review widened to full"
            );
            return Ok((full_scope(), stored.unwrap_or_default()));
        }
        let Some(prior) = stored else {
            tracing::info!(
                review_id = %pr.review_id(),
                reason = "previous_review_not_stored",
                "review widened to full"
            );
            return Ok((full_scope(), Vec::new()));
        };
        let scope = self.linear_scope(cancel, pr, published, scope).await?;
        Ok((scope, prior))
    }

    /// Keeps an incremental `scope` only when the head extends the published head linearly.
    ///
    /// A history that is not linear or cannot be read widens the review to full.
    ///
    /// # Errors
    ///
    /// Returns the history error when `cancel` is cancelled.
    async fn linear_scope(
        &self,
        cancel: &CancellationToken,
        pr: &PullRequest,
        published: &PublishedRecord,
        scope: ReviewScope,
    ) -> Result<ReviewScope, Error> {
        match self
            .integration
            .linear_history(cancel, &pr.repository, &published.head_sha, &pr.head_sha)
            .await
        {
            Err(err) if cancel.is_cancelled() => Err(err),
            Err(err) => {
                // Without the comparison the diff since the reviewed head is unknown.
                tracing::warn!(
                    review_id = %pr.review_id(),
                    reason = "history_unknown",
                    error_code = error_code(&err),
                    "review widened to full"
                );
                Ok(full_scope())
            }
            Ok(false) => {
                tracing::info!(
                    review_id = %pr.review_id(),
                    reason = "history_not_linear",
                    "review widened to full"
                );
                Ok(full_scope())
            }
            Ok(true) => Ok(scope),
        }
    }

    /// Reads the blocking findings of a published review and those it left open.
    ///
    /// Returns `None` when that review's result is gone, such as after retention or for a
    /// review this engine did not make.
    fn prior_findings(
        &self,
        published: &PublishedRecord,
    ) -> Result<Option<Vec<PriorFinding>>, Error> {
        let id = &published.workflow_id;
        if !id.starts_with(WORKFLOW_ID_PREFIX) {
            return Ok(None);
        }
        let Some(review) = self.store.get_review(id)? else {
            return Ok(None);
        };
        let Some(artifact) = self.store.load_step::<Artifact>(id, STEP_ARTIFACT)? else {
            return Ok(None);
        };
        let Some(publication) = self
            .store
            .load_step::<PublishResult>(id, STEP_PUBLICATION)?
        else {
            return Ok(None);
        };
        let limit = usize::try_from(self.config.max_findings).unwrap_or(0);
        let mut seen = BTreeSet::new();
        let mut prior = Vec::new();
        let mut add = |finding: PriorFinding| {
            if seen.insert(finding.key.clone()) && prior.len() < limit {
                prior.push(finding);
            }
        };
        for open in artifact.open_prior() {
            add(open.finding);
        }
        let (blocking, _) = review.input.split_findings(&artifact.findings);
        for finding in blocking {
            add(PriorFinding {
                key: prior_key(&artifact.head_sha, &finding.id),
                id: finding.id.clone(),
                head_sha: artifact.head_sha.clone(),
                github_review_id: publication.github_review_id,
                severity: finding.severity.clone(),
                path: finding.path.clone(),
                line: finding.line,
                title: finding.title.clone(),
                body: finding.body.clone(),
            });
        }
        Ok(Some(prior))
    }

    /// Returns the published review that handed non-blocking findings to the author.
    ///
    /// At an unchanged head and base branch the review adopts that result while it is stored;
    /// otherwise the reviewers run and the clean outcome waits for the user.
    ///
    /// # Errors
    ///
    /// Returns the store error of the artifact read.
    pub(crate) fn notes_handoff(
        &self,
        published: Option<&PublishedRecord>,
        pr: &PullRequest,
    ) -> Result<Option<NotesHandoff>, Error> {
        let Some(published) = published else {
            return Ok(None);
        };
        if !published.awaits_author {
            return Ok(None);
        }
        let mut handoff = NotesHandoff {
            workflow_id: published.workflow_id.clone(),
            head_sha: published.head_sha.clone(),
            adopt: false,
        };
        if published.head_sha != pr.head_sha || published.retargeted(pr) {
            return Ok(Some(handoff));
        }
        let stored = self
            .store
            .load_step::<Artifact>(&published.workflow_id, STEP_ARTIFACT)?
            .is_some();
        if !stored {
            tracing::info!(
                review_id = %pr.review_id(),
                reason = "handoff_review_not_stored",
                "notes hand-off reviewed again"
            );
        }
        handoff.adopt = stored;
        Ok(Some(handoff))
    }

    /// Builds the input of one review attempt of `candidate`.
    ///
    /// An incremental scope runs the incremental reviewers of the pipeline.
    ///
    /// # Errors
    ///
    /// Returns `configuration_invalid` when the configuration has no policy, and the permanent
    /// `review_input_invalid` for a candidate whose author class is not resolved.
    pub(crate) fn build_input(
        &self,
        candidate: &Candidate,
        route: ProjectRoute,
        scope: ReviewScope,
        prior: Vec<PriorFinding>,
        handoff: Option<NotesHandoff>,
        attempt: i64,
    ) -> Result<ReviewInput, Error> {
        let policy = self.policy()?;
        let Some(author_class) = candidate.author_class.clone() else {
            // No review runs with a trust that nobody decided.
            return Err(Error::workflow("review_input_invalid", true, None));
        };
        let pipeline = &candidate.rule.pipeline;
        Ok(ReviewInput {
            version: INPUT_VERSION,
            pull_request: candidate.pull_request.clone(),
            attempt,
            rule: candidate.rule.clone(),
            author_class,
            reviewers: if scope.incremental() {
                pipeline.incremental_reviewers.clone()
            } else {
                pipeline.reviewers.clone()
            },
            scope,
            route,
            notifications: policy.notifications().clone(),
            approval_timeout_seconds: i64::try_from(policy.approval_timeout().as_secs())
                .unwrap_or(i64::MAX),
            policy_digest: policy.digest().to_owned(),
            publisher_login: self.config.github_login.clone(),
            limits: self.config.run_limits(),
            prior,
            handoff,
        })
    }
}

#[cfg(test)]
mod tests {
    use autoreview_policy::{
        Checkout, OnClean, OnFindings, Pipeline, Protocol, Reviewer, Rule, Trigger, TriggerKind,
    };

    use super::review_scope;
    use crate::model::{Decision, PublishedRecord, ReviewScope};
    use crate::protocol::fixtures::{codex, opus, sha};

    fn test_rule(reviewer: Reviewer, on_findings: OnFindings, on_clean: OnClean) -> Rule {
        let (checkout, trigger) = if reviewer.protocol == Protocol::CodexEnvelope {
            (
                Checkout::Head,
                Trigger {
                    kind: TriggerKind::Label,
                    label: "ai:review".to_owned(),
                },
            )
        } else {
            (
                Checkout::Base,
                Trigger {
                    kind: TriggerKind::ReviewRequested,
                    label: String::new(),
                },
            )
        };
        Rule {
            match_glob: "acme/*".to_owned(),
            trigger,
            on_findings,
            on_clean,
            pipeline: Pipeline {
                name: "main".to_owned(),
                protocol: reviewer.protocol.clone(),
                reviewers: vec![reviewer.clone()],
                incremental_reviewers: vec![reviewer],
                prompt: "Review the pull request.".to_owned(),
                checkout,
                project_sources: vec!["manual".to_owned()],
                verifier: None,
            },
            ..Rule::default()
        }
    }

    struct Case<'a> {
        name: &'a str,
        rule: &'a Rule,
        published: Option<PublishedRecord>,
        head: String,
        own: bool,
        want: ReviewScope,
    }

    // An approval without the user must rest on a full review or on an approved baseline: after
    // a request for changes the next review is full again, unless the user approves.
    #[test]
    fn review_scope_is_incremental_only_after_a_decision_that_leaves_nothing_open() {
        let approving = test_rule(opus(), OnFindings::RequestChanges, OnClean::Approve);
        let awaiting = test_rule(opus(), OnFindings::RequestChanges, OnClean::Await);
        let commenting = test_rule(codex(), OnFindings::Comment, OnClean::Comment);
        let previous = |decision: Decision| PublishedRecord {
            head_sha: sha("c"),
            decision,
            ..PublishedRecord::default()
        };
        let full = || ReviewScope {
            mode: "full".to_owned(),
            previous_head_sha: String::new(),
        };
        let incremental = || ReviewScope {
            mode: "incremental".to_owned(),
            previous_head_sha: sha("c"),
        };
        let cases = [
            Case {
                name: "first review",
                rule: &approving,
                published: None,
                head: sha("a"),
                own: false,
                want: full(),
            },
            Case {
                name: "after approval",
                rule: &approving,
                published: Some(previous(Decision::Approve)),
                head: sha("a"),
                own: false,
                want: incremental(),
            },
            Case {
                name: "after request changes",
                rule: &approving,
                published: Some(previous(Decision::RequestChanges)),
                head: sha("a"),
                own: false,
                want: full(),
            },
            Case {
                name: "after request changes on a rule the user approves",
                rule: &awaiting,
                published: Some(previous(Decision::RequestChanges)),
                head: sha("a"),
                own: false,
                want: incremental(),
            },
            Case {
                name: "after comment on an approving rule",
                rule: &approving,
                published: Some(previous(Decision::Comment)),
                head: sha("a"),
                own: false,
                want: full(),
            },
            Case {
                name: "own pull request is never approved",
                rule: &approving,
                published: Some(previous(Decision::Comment)),
                head: sha("a"),
                own: true,
                want: incremental(),
            },
            Case {
                name: "comment-only rule",
                rule: &commenting,
                published: Some(previous(Decision::Comment)),
                head: sha("a"),
                own: false,
                want: incremental(),
            },
            Case {
                name: "same head",
                rule: &approving,
                published: Some(previous(Decision::Approve)),
                head: sha("c"),
                own: false,
                want: full(),
            },
        ];
        for case in cases {
            let got = review_scope(case.rule, case.published.as_ref(), &case.head, case.own);
            assert_eq!(got, case.want, "{}", case.name);
        }
    }
}
