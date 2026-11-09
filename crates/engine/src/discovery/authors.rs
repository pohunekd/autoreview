//! Author classes of the candidates: the trust a rule gives each pull request author.

// Rust guideline compliant 2026-06-26

use autoreview_policy::{AuthorClass, AuthorType, Authors, Permission, Untrusted};
use tokio_util::sync::CancellationToken;

use crate::engine::{Clock, Engine};
use crate::error::Error;
use crate::integrations::Integrations;
use crate::model::{Candidate, CandidateList};

/// Returns the candidate with its author class, or `None` when the rule skips the author.
///
/// An untrusted author's candidate takes the rule of the untrusted section, so every later step
/// reads its base-checkout pipeline and outcomes. `permission` is the author's repository
/// permission, `None` below write or when it was not read.
pub(crate) fn classify_candidate(
    authors: &Authors,
    mut candidate: Candidate,
    permission: Option<Permission>,
) -> Option<Candidate> {
    let pr = &candidate.pull_request;
    let author_type = AuthorType::from_typename(&pr.author_type);
    let class = authors.class(&pr.author, author_type, pr.fork, permission);
    if class == AuthorClass::Trusted {
        candidate.author_class = Some(class);
        return Some(candidate);
    }
    match authors {
        Authors::Restricted {
            untrusted: Untrusted::Review(review),
            ..
        } => {
            candidate.rule = review.rule(&candidate.rule);
            candidate.author_class = Some(AuthorClass::Untrusted);
            Some(candidate)
        }
        // Trusted any never yields an untrusted class; dropping the candidate is the safe side.
        Authors::Restricted {
            untrusted: Untrusted::Skip,
            ..
        }
        | Authors::Any => None,
    }
}

impl<I: Integrations, C: Clock> Engine<I, C> {
    /// Lists the candidates and resolves the author class of each one.
    ///
    /// The permission is read only when it can change the class. A candidate whose rule skips
    /// untrusted authors, or that the policy no longer covers, is dropped with a log line.
    ///
    /// # Errors
    ///
    /// Returns the error of the search or of a permission read, and `configuration_invalid`
    /// when the configuration has no policy.
    pub(crate) async fn list_classified(
        &self,
        cancel: &CancellationToken,
    ) -> Result<CandidateList, Error> {
        let listed = self.integration.list_candidates(cancel).await?;
        let policy = self.policy()?;
        let mut candidates = Vec::new();
        for candidate in listed.candidates {
            let pr = &candidate.pull_request;
            let review_id = pr.review_id();
            let Some(authors) = policy.authors(&pr.repository) else {
                tracing::info!(
                    review_id = %review_id,
                    "candidate is not covered by the policy"
                );
                continue;
            };
            let author_type = AuthorType::from_typename(&pr.author_type);
            let permission = if authors.needs_permission(&pr.author, author_type, pr.fork) {
                self.integration
                    .author_permission(cancel, &pr.repository, &pr.author)
                    .await?
            } else {
                None
            };
            let fork = pr.fork;
            match classify_candidate(authors, candidate, permission) {
                Some(classified) => candidates.push(classified),
                None => tracing::info!(
                    review_id = %review_id,
                    author_class = AuthorClass::Untrusted.as_str(),
                    fork,
                    "untrusted author skipped"
                ),
            }
        }
        Ok(CandidateList {
            candidates,
            rejected: listed.rejected,
        })
    }
}

#[cfg(test)]
mod tests {
    use autoreview_policy::{
        AuthorClass, Authors, Checkout, OnClean, OnFindings, OnNotes, Permission, Pipeline, Rule,
        TrustedAuthors, Untrusted, UntrustedReview,
    };

    use super::classify_candidate;
    use crate::model::{Candidate, PullRequest};

    /// A candidate of an approving rule with a head checkout, by `author`.
    fn candidate(author: &str, fork: bool) -> Candidate {
        Candidate {
            pull_request: PullRequest {
                repository: "public/widget".to_owned(),
                number: 7,
                author: author.to_owned(),
                author_type: "User".to_owned(),
                fork,
                ..PullRequest::default()
            },
            rule: Rule {
                match_glob: "public/*".to_owned(),
                pipeline: Pipeline {
                    name: "full".to_owned(),
                    checkout: Checkout::Head,
                    ..Pipeline::default()
                },
                on_findings: OnFindings::RequestChanges,
                on_clean: OnClean::Approve,
                on_notes: OnNotes::Comment,
                ..Rule::default()
            },
            author_class: None,
        }
    }

    /// Trusts `maintainer` and anyone with maintain; `untrusted` is what everyone else gets.
    fn authors(untrusted: Untrusted) -> Authors {
        Authors::Restricted {
            trusted: TrustedAuthors {
                logins: vec!["maintainer".to_owned()],
                permission: Some(Permission::Maintain),
            },
            untrusted,
        }
    }

    #[test]
    fn untrusted_candidates_take_the_untrusted_rule_or_are_skipped() {
        let light = Pipeline {
            name: "light".to_owned(),
            checkout: Checkout::Base,
            ..Pipeline::default()
        };
        let review = authors(Untrusted::Review(Box::new(UntrustedReview {
            pipeline: light,
            on_findings: OnFindings::Comment,
            on_clean: OnClean::Await,
        })));
        let trusted =
            classify_candidate(&review, candidate("member", false), Some(Permission::Admin))
                .expect("an admin is reviewed");
        assert!(
            trusted.author_class == Some(AuthorClass::Trusted)
                && trusted.rule.on_clean == OnClean::Approve
                && trusted.rule.pipeline.name == "full",
            "a trusted author keeps the rule: {trusted:?}"
        );
        let untrusted = classify_candidate(&review, candidate("member", true), None)
            .expect("an untrusted author is reviewed");
        let rule = &untrusted.rule;
        assert!(
            untrusted.author_class == Some(AuthorClass::Untrusted)
                && rule.match_glob == "public/*"
                && rule.pipeline.name == "light"
                && rule.pipeline.checkout == Checkout::Base
                && rule.on_findings == OnFindings::Comment
                && rule.on_clean == OnClean::Await
                && rule.on_notes == OnNotes::Unset
                && rule.human_review.is_none(),
            "an untrusted author gets the untrusted rule: {untrusted:?}"
        );
        let skip = authors(Untrusted::Skip);
        assert!(
            classify_candidate(&skip, candidate("stranger", true), None).is_none()
                && classify_candidate(&skip, candidate("maintainer", true), None).is_some(),
            "skip drops only untrusted authors"
        );
    }
}
