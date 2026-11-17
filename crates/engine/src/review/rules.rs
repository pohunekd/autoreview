//! The read of the repository review rules: `REVIEW.md` at the base commit of the pull request.

// Rust guideline compliant 2026-11-17

use autoreview_policy::{ReviewRules, parse_review_rules};

use super::input::{STEP_CHANGED_FILES, STEP_REVIEW_RULES};
use super::sessions::Review;
use crate::engine::Clock;
use crate::error::{BoxError, Error};
use crate::integrations::{ChangedFiles, Integrations, RepositoryFile};
use crate::model::{PullRequest, ReviewRulesOutcome};

/// Path of the rules file, relative to the repository root.
pub(crate) const REVIEW_RULES_PATH: &str = "REVIEW.md";

/// Code of a rules file over `limits.max_review_rules_bytes`.
const RULES_TOO_LARGE: &str = "review_rules_too_large";

/// Code of a rules file that is not valid.
const RULES_INVALID: &str = "review_rules_invalid";

/// Code of a rules file that could not be read.
const RULES_UNREADABLE: &str = "review_rules_unreadable";

/// Returns the failure of an unusable base file: its code and the cause for the logs.
fn unusable(file: RepositoryFile) -> Result<Option<ReviewRules>, (&'static str, Option<BoxError>)> {
    match file {
        RepositoryFile::Absent => Ok(None),
        RepositoryFile::TooLarge => Err((RULES_TOO_LARGE, None)),
        RepositoryFile::NotText => Err((RULES_INVALID, Some("REVIEW.md is not UTF-8 text".into()))),
        RepositoryFile::Text(text) => parse_review_rules(&text)
            .map(Some)
            .map_err(|error| (RULES_INVALID, Some(Box::new(error) as BoxError))),
    }
}

/// Returns the failure of a read: a permanent error becomes `review_rules_unreadable`.
///
/// Any other error keeps its own code, so the scheduler retries a transient one
/// (`github_request_failed`) and a shutdown stays a shutdown.
fn unreadable(error: Error) -> Error {
    let permanent = matches!(
        &error,
        Error::Workflow {
            permanent: true,
            ..
        }
    );
    if permanent {
        Error::workflow(RULES_UNREADABLE, true, Some(Box::new(error)))
    } else {
        error
    }
}

impl<I: Integrations, C: Clock> Review<'_, I, C> {
    /// Reads `REVIEW.md` at the base commit once per review and stores the outcome.
    ///
    /// An invalid or too large file fails the review, except for a pull request that changes
    /// `REVIEW.md`: that one is reviewed without rules and the outcome records the code.
    ///
    /// # Errors
    ///
    /// Returns `review_rules_too_large`, `review_rules_invalid` or `review_rules_unreadable`,
    /// the shutdown error, or the error of the store.
    pub(crate) async fn review_rules(&self) -> Result<ReviewRulesOutcome, Error> {
        self.once(STEP_REVIEW_RULES, || self.read_rules()).await
    }

    /// Lists the changed files of `pull_request` once per review and stores the listing.
    ///
    /// The rules step and the human review signals read the same listing.
    pub(crate) async fn changed_files(
        &self,
        pull_request: &PullRequest,
    ) -> Result<ChangedFiles, Error> {
        self.once(STEP_CHANGED_FILES, || async {
            let integration = &*self.engine.integration;
            let credential = self.credential()?;
            integration
                .list_changed_files(&self.cancel, credential, pull_request)
                .await
        })
        .await
    }

    /// Reads the base file and the changed files of the pull request.
    async fn read_rules(&self) -> Result<ReviewRulesOutcome, Error> {
        let integration = &*self.engine.integration;
        let credential = self.credential()?;
        let pull_request = &self.input.pull_request;
        let max_bytes = self.engine.config.max_review_rules_bytes.unsigned_abs();
        let file = integration
            .read_repository_file(
                &self.cancel,
                credential,
                &pull_request.repository,
                &pull_request.base_sha,
                REVIEW_RULES_PATH,
                max_bytes,
            )
            .await
            .map_err(unreadable)?;
        let changed = self.changed_files(pull_request).await?;
        let changed_by_pull_request = changed.files.iter().any(|file| {
            file.path == REVIEW_RULES_PATH
                || file.previous_path.as_deref() == Some(REVIEW_RULES_PATH)
        });
        let outcome = |rules, invalid_code: &str| ReviewRulesOutcome {
            base_sha: pull_request.base_sha.clone(),
            rules,
            invalid_code: invalid_code.to_owned(),
            changed_by_pull_request,
        };
        match unusable(file) {
            Ok(rules) => Ok(outcome(rules, "")),
            Err((code, cause)) => {
                // The cause names the key or the line; it goes to the log, never to the PR.
                tracing::warn!(
                    review_id = %pull_request.review_id(),
                    base_sha = %pull_request.base_sha,
                    error_code = code,
                    cause = ?cause.as_ref().map(ToString::to_string),
                    changed_by_pull_request,
                    "REVIEW.md of the base commit is unusable"
                );
                if changed_by_pull_request {
                    Ok(outcome(None, code))
                } else {
                    Err(Error::workflow(code, true, cause))
                }
            }
        }
    }
}
