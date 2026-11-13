//! Identifiers of a review: keys, review IDs, workflow IDs and their validation.
//!
//! The texts are stored and published, so they match the existing rows and markers byte for
//! byte.

// Rust guideline compliant 2026-06-26

use autoreview_policy::user_login_valid;
use autoreview_schedule::scheduler::Target;
use sha2::{Digest, Sha256};

use super::pull_request::{PullRequest, RejectedCandidate};

/// Prefix of the workflow IDs of this engine; pohunek session names derive from them.
pub(crate) const WORKFLOW_ID_PREFIX: &str = "prr-v2-";

/// Contract version of the review input that discovery writes.
pub(crate) const INPUT_VERSION: i64 = 5;

/// Oldest input version a review accepts, so a stored review resumes after an upgrade.
pub(crate) const MIN_INPUT_VERSION: i64 = 2;

/// First input version whose findings may be non-blocking; before it every finding blocks.
pub(crate) const BLOCKING_GATE_VERSION: i64 = 3;

/// First input version that may carry the blocking findings of earlier reviews.
pub(crate) const PRIOR_FINDINGS_VERSION: i64 = 4;

/// First input version whose blocking findings the verifier checks before publication.
pub(crate) const VERIFIER_VERSION: i64 = 5;

/// Characters a review ID keeps of the head SHA.
const REVIEW_ID_HEAD_CHARS: usize = 12;

/// Length of a full commit SHA in hex digits.
pub const SHA_LEN: usize = 40;

/// Shortest head SHA prefix a command accepts.
pub const HEAD_PREFIX_MIN_LEN: usize = 12;

/// Longest repository name.
const NAME_MAX_LEN: usize = 100;

impl PullRequest {
    /// Returns the `repository#number` key shared by every head of the pull request.
    #[must_use]
    pub fn key(&self) -> String {
        format!("{}#{}", self.repository, self.number)
    }

    /// Returns the scheduler target of this pull request head.
    #[must_use]
    pub fn target(&self) -> Target {
        Target {
            repository: self.repository.clone(),
            number: self.number,
            head_sha: self.head_sha.clone(),
            request_id: self.review_request_id,
        }
    }

    /// Returns the notification and command handle of the review head.
    ///
    /// The head SHA is cut to its first 12 characters, as Go's `%.12s` does.
    #[must_use]
    pub fn review_id(&self) -> String {
        let head: String = self.head_sha.chars().take(REVIEW_ID_HEAD_CHARS).collect();
        format!("{}#PR{}@{}", self.repository, self.number, head)
    }

    /// Returns the workflow ID of one attempt of the review identity.
    ///
    /// It is deterministic, so a repeated dispatch of the same attempt reaches the same workflow.
    #[must_use]
    pub fn workflow_id(&self, attempt: i64) -> String {
        let text = format!(
            "pr-review-v2\0{}\0{}\0{}\0{}\0{}",
            self.repository, self.number, self.head_sha, self.review_request_id, attempt
        );
        let digest = Sha256::digest(text.as_bytes());
        format!("{}{}", WORKFLOW_ID_PREFIX, hex::encode(digest))
    }
}

impl RejectedCandidate {
    /// Returns the `repository#number` key of the refused pull request.
    #[must_use]
    pub fn key(&self) -> String {
        format!("{}#{}", self.repository, self.number)
    }
}

/// Reports whether a repository is a lowercase `owner/name` that GitHub allows.
///
/// The owner is a user or organization login, a managed user's `handle_shortcode` included
/// ([`user_login_valid`]); the name matches `[a-z0-9_.-]{1,100}`.
#[must_use]
pub fn repository_valid(repository: &str) -> bool {
    let Some((owner, name)) = repository.split_once('/') else {
        return false;
    };
    if !user_login_valid(owner) {
        return false;
    }
    if name.is_empty() || name.len() > NAME_MAX_LEN {
        return false;
    }
    for &byte in name.as_bytes() {
        if !(byte.is_ascii_lowercase()
            || byte.is_ascii_digit()
            || byte == b'_'
            || byte == b'.'
            || byte == b'-')
        {
            return false;
        }
    }
    true
}

/// Reports whether a value is a full lowercase hex commit SHA.
///
/// It matches Go's `^[0-9a-f]{40}$`.
#[must_use]
pub fn sha_valid(value: &str) -> bool {
    value.len() == SHA_LEN && lower_hex(value)
}

/// Reports whether a value is a lowercase hex SHA prefix of 12 to 40 digits.
#[must_use]
pub fn head_prefix_valid(value: &str) -> bool {
    let len = value.len();
    (HEAD_PREFIX_MIN_LEN..=SHA_LEN).contains(&len) && lower_hex(value)
}

/// Reports whether a repository, number and head name one valid review head.
pub(crate) fn review_identity_valid(repository: &str, number: i64, head: &str) -> bool {
    repository_valid(repository) && number > 0 && sha_valid(head)
}

/// Reports whether every byte of a value is a lowercase hex digit.
fn lower_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::repository_valid;

    #[test]
    fn repository_owner_follows_the_github_login_grammar() {
        // A managed user owns repositories as `handle_shortcode`, 39 bytes at most.
        let longest_owner = format!("{}/widget", "a".repeat(39));
        let too_long_owner = format!("{}/widget", "a".repeat(40));
        let longest_managed = format!("{}_octo/widget", "a".repeat(34));
        let too_long_managed = format!("{}_octo/widget", "a".repeat(35));
        let longest_name = format!("acme/{}", "a".repeat(100));
        let too_long_name = format!("acme/{}", "a".repeat(101));
        let accepted = [
            "acme/widget",
            "0/x",
            "a-/x",
            "a--b/x",
            "acme/my_repo.v2-x",
            longest_owner.as_str(),
            longest_name.as_str(),
            "mona-cat_octo/widget",
            "octo_admin/repo.name",
            "2abvd19d_admin/x",
            longest_managed.as_str(),
        ];
        let refused = [
            "",
            "acme",
            "/widget",
            "acme/",
            "acme/widget/x",
            "Acme/widget",
            "acme/Widget",
            "-acme/x",
            "acme[bot]/x",
            too_long_owner.as_str(),
            too_long_name.as_str(),
            "_octo/x",
            "mona_/x",
            "mona__octo/x",
            "mona_cat_octo/x",
            "-mona_octo/x",
            "mona-_octo/x",
            "mona--cat_octo/x",
            "mona_oc-to/x",
            "mona.cat_octo/x",
            "mona cat_octo/x",
            "mona_\u{f6}cto/x",
            "mona_octo[bot]/x",
            "Mona-Cat_octo/x",
            too_long_managed.as_str(),
        ];
        for repository in accepted {
            assert!(
                repository_valid(repository),
                "{repository:?} must be accepted"
            );
        }
        for repository in refused {
            assert!(
                !repository_valid(repository),
                "{repository:?} must be refused"
            );
        }
    }
}
