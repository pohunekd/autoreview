//! Publication of a review and its reconciliation by marker.
//!
//! Mirrors `cmd/autoreview/github.go:745-818`. A review is never posted twice: its marker is
//! searched for before the POST and again after a POST that failed.

// Rust guideline compliant 2026-06-26

use autoreview_engine::error::Error;
use autoreview_engine::integrations::{GitHubReview, InlineComment};
use autoreview_engine::model::{Decision, PublishResult, PullRequest};
use serde::{Deserialize, Serialize};

use super::{GitHub, response_invalid};

/// Reviews per page of the reviews API; a short page ends the lookup.
const REVIEWS_PER_PAGE: usize = 100;

/// The author of a review; a deleted account has an empty login.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct ReviewUser {
    /// Login of the author.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    login: String,
}

autoreview_go_compat::object_only_serde!(ReviewUser);

/// One review of the reviews API, with the fields that the reconciliation reads.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct GitHubReviewRecord {
    /// Identifier of the review.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    id: i64,
    /// URL of the review, read from the `html_url` key.
    #[serde(
        rename = "html_url",
        deserialize_with = "autoreview_go_compat::null_default::deserialize"
    )]
    url: String,
    /// Body text of the review, which carries the marker.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    body: String,
    /// Head commit that the review was made on.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    commit_id: String,
    /// GitHub state of the review, such as `APPROVED` or `DISMISSED`.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    state: String,
    /// Author of the review.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    user: ReviewUser,
}

autoreview_go_compat::object_only_serde!(GitHubReviewRecord);

/// Body of the review POST. Its keys are in Go's sorted map order.
#[derive(Debug, Serialize)]
struct ReviewPayload<'a> {
    /// Body text of the review.
    body: &'a str,
    /// Inline comments; absent when the review has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    comments: Option<&'a [InlineComment]>,
    /// Head commit that the review is made on.
    commit_id: &'a str,
    /// GitHub review event; the unset decision is sent as an empty string.
    event: Decision,
}

/// Outcome of one page of the reviews lookup.
#[derive(Debug, PartialEq, Eq)]
enum PageStep {
    /// The page holds the review that is already published.
    Found(PublishResult),
    /// The page is full, so the next page may hold the review.
    Next,
    /// The page is short, so no later page holds the review.
    Done,
}

/// Refuses a review whose body does not carry its marker, before any `gh` call
/// (`github.go:795-797`).
fn check_review_marker(review: &GitHubReview) -> Result<(), Error> {
    if review.marker.is_empty() || !review.body.contains(&review.marker) {
        return Err(Error::workflow("github_review_invalid", true, None));
    }
    Ok(())
}

/// Returns the state that GitHub reports for a published review of `decision`
/// (`github.go:762-768`).
fn expected_state(decision: Decision) -> &'static str {
    match decision {
        Decision::Approve => "APPROVED",
        Decision::RequestChanges => "CHANGES_REQUESTED",
        Decision::Comment | Decision::Unset => "COMMENTED",
    }
}

/// Reports whether the author `author` is the configured `login`, as Go's `strings.EqualFold`.
///
/// The configured login is lower-case ASCII (`github_login_valid` in the engine configuration),
/// so the author matches exactly when each of its characters folds to the login character. Of
/// the non-ASCII characters, only U+212A KELVIN SIGN (folds to `k`) and U+017F LATIN SMALL LETTER
/// LONG S (folds to `s`) fold to an ASCII letter. A missed match would post a second review.
fn login_matches(login: &str, author: &str) -> bool {
    if login.chars().count() != author.chars().count() {
        return false;
    }
    for (left, right) in login.chars().zip(author.chars()) {
        if fold_char(left) != fold_char(right) {
            return false;
        }
    }
    true
}

/// Returns the character that `character` folds to: ASCII lower case, with the two non-ASCII
/// characters that fold to ASCII letters mapped to them.
fn fold_char(character: char) -> char {
    match character {
        '\u{212A}' => 'k',
        '\u{017F}' => 's',
        _ => character.to_ascii_lowercase(),
    }
}

/// Decides what one page of the reviews lookup means (`github.go:766-793`).
///
/// The first record, in listing order, that carries the marker on `head_sha` from `login` is the
/// review. Its state must be the expected state or `DISMISSED`; a later record is never tried.
fn review_page_step(
    output: &str,
    review: &GitHubReview,
    head_sha: &str,
    login: &str,
) -> Result<PageStep, Error> {
    let records: Vec<GitHubReviewRecord> =
        serde_json::from_str(output).map_err(|error| response_invalid(Some(Box::new(error))))?;
    for record in &records {
        let marker_found = record.body.contains(&review.marker);
        let has_id = record.id > 0;
        let has_url = !record.url.is_empty();
        let commit_matches = record.commit_id == head_sha;
        let author_matches = login_matches(login, &record.user.login);
        if marker_found && has_id && has_url && commit_matches && author_matches {
            if record.state != expected_state(review.decision) && record.state != "DISMISSED" {
                return Err(Error::workflow("github_review_state_conflict", true, None));
            }
            return Ok(PageStep::Found(PublishResult {
                decision: review.decision,
                github_review_id: record.id,
                review_url: record.url.clone(),
                reused: true,
            }));
        }
    }
    if records.len() < REVIEWS_PER_PAGE {
        Ok(PageStep::Done)
    } else {
        Ok(PageStep::Next)
    }
}

/// Returns the result of a posted review, which needs a positive id and a URL
/// (`github.go:812-815`).
fn posted_result(output: &str, decision: Decision) -> Result<PublishResult, Error> {
    let record: GitHubReviewRecord =
        serde_json::from_str(output).map_err(|error| response_invalid(Some(Box::new(error))))?;
    if record.id <= 0 || record.url.is_empty() {
        return Err(response_invalid(None));
    }
    Ok(PublishResult {
        decision,
        github_review_id: record.id,
        review_url: record.url,
        reused: false,
    })
}

/// Returns the argv of one page of the reviews lookup (`github.go:766-770`).
fn reviews_list_args(repository: &str, number: i64, page: i64) -> Vec<String> {
    vec![
        "api".to_owned(),
        format!("repos/{repository}/pulls/{number}/reviews?per_page=100&page={page}"),
    ]
}

/// Returns the argv of the review POST, whose body is read from stdin (`github.go:803`).
fn reviews_post_args(repository: &str, number: i64) -> Vec<String> {
    vec![
        "api".to_owned(),
        "--method".to_owned(),
        "POST".to_owned(),
        format!("repos/{repository}/pulls/{number}/reviews"),
        "--input".to_owned(),
        "-".to_owned(),
    ]
}
impl GitHub {
    /// Publishes `review` unless its marker is already on the head (`github.go:794-817`).
    pub(crate) async fn publish_review(
        &self,
        pr: &PullRequest,
        review: &GitHubReview,
    ) -> Result<PublishResult, Error> {
        check_review_marker(review)?;
        if let Some(existing) = self.find_published_review(pr, review).await? {
            return Ok(existing);
        }
        let comments = if review.comments.is_empty() {
            None
        } else {
            Some(review.comments.as_slice())
        };
        let payload = ReviewPayload {
            body: &review.body,
            comments,
            commit_id: &pr.head_sha,
            event: review.decision,
        };
        let arguments = reviews_post_args(&pr.repository, pr.number);
        let output = match self.github(&arguments, Some(&payload)).await {
            Ok(value) => value,
            Err(post_error) => {
                let second = self.find_published_review(pr, review).await;
                if let Ok(Some(existing)) = second {
                    return Ok(existing);
                }
                return Err(post_error);
            }
        };
        posted_result(&output, review.decision)
    }

    /// Returns the published review that carries the marker, reading every page
    /// (`github.go:766-793`).
    async fn find_published_review(
        &self,
        pr: &PullRequest,
        review: &GitHubReview,
    ) -> Result<Option<PublishResult>, Error> {
        for page in 1..=self.search_page_limit {
            let arguments = reviews_list_args(&pr.repository, pr.number, page);
            let output = self.github::<()>(&arguments, None).await?;
            match review_page_step(&output, review, &pr.head_sha, &self.login)? {
                PageStep::Found(result) => return Ok(Some(result)),
                PageStep::Done => return Ok(None),
                PageStep::Next => {
                    if page == self.search_page_limit {
                        return Err(Error::workflow("github_search_limit_exceeded", true, None));
                    }
                }
            }
        }
        Err(Error::workflow("github_search_limit_exceeded", true, None))
    }
}

#[cfg(test)]
mod tests {
    use autoreview_engine::integrations::GitHubReview;
    use autoreview_engine::model::{Decision, PublishResult};

    use super::{PageStep, check_review_marker, review_page_step};

    /// Marker of the review under test.
    const MARKER: &str = "<!-- marker:v1 -->";

    /// Head of the pull request under test.
    const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    /// Head that the pull request had before its last push.
    const OLD_HEAD: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    /// Login of the reviewing account, lower-case ASCII as the configuration requires.
    const LOGIN: &str = "keeper";

    /// Returns a review whose body carries the marker.
    fn review(decision: Decision) -> GitHubReview {
        GitHubReview {
            decision,
            body: format!("Findings. {MARKER}"),
            comments: Vec::new(),
            marker: MARKER.to_owned(),
        }
    }

    /// Returns one review record as the reviews API writes it.
    fn record(id: i64, commit: &str, login: &str, state: &str) -> String {
        format!(r#"{{"id":{id},"html_url":"https://example.org/acme/widget/pull/3#review-{id}","#)
            + &format!(r#""body":"Findings. {MARKER}","commit_id":"{commit}","state":"{state}","#)
            + &format!(r#""user":{{"login":"{login}"}}}}"#)
    }

    #[test]
    fn review_marker_is_required_before_any_call() {
        assert!(check_review_marker(&review(Decision::Comment)).is_ok());
        let mut missing = review(Decision::Comment);
        missing.body = "Findings without the marker.".to_owned();
        assert!(check_review_marker(&missing).is_err());
        let mut empty = review(Decision::Comment);
        empty.marker = String::new();
        assert!(check_review_marker(&empty).is_err());
    }

    #[test]
    fn first_matching_review_wins_in_listing_order() {
        let records = [
            record(10, HEAD, "colleague", "COMMENTED"),
            record(11, OLD_HEAD, LOGIN, "COMMENTED"),
            record(12, HEAD, "\u{212a}eeper", "COMMENTED"),
            record(13, HEAD, "KEEPER", "COMMENTED"),
        ];
        let output = format!("[{}]", records.join(","));
        let step = review_page_step(&output, &review(Decision::Comment), HEAD, LOGIN)
            .expect("page decodes");
        assert_eq!(
            step,
            PageStep::Found(PublishResult {
                decision: Decision::Comment,
                github_review_id: 12,
                review_url: "https://example.org/acme/widget/pull/3#review-12".to_owned(),
                reused: true,
            })
        );
    }
}
