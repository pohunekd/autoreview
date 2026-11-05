//! Identifiers of a review: keys, review IDs, workflow IDs and their validation.
//!
//! The texts are stored and published, so they match the Go reference byte for byte.

// Rust guideline compliant 2026-06-26

use autoreview_schedule::scheduler::Target;
use sha2::{Digest, Sha256};

use super::pull_request::{PullRequest, RejectedCandidate};

/// Prefix of the workflow IDs of this engine; pohunek session names derive from them.
pub(crate) const WORKFLOW_ID_PREFIX: &str = "prr-v2-";

/// Contract version of the review input that discovery writes.
#[expect(dead_code, reason = "called by S22")]
pub(crate) const INPUT_VERSION: i64 = 5;

/// Oldest input version a review accepts, so a stored review resumes after an upgrade.
#[expect(dead_code, reason = "called by S25")]
pub(crate) const MIN_INPUT_VERSION: i64 = 2;

/// First input version whose findings may be non-blocking; before it every finding blocks.
pub(crate) const BLOCKING_GATE_VERSION: i64 = 3;

/// First input version that may carry the blocking findings of earlier reviews.
#[expect(dead_code, reason = "called by S25")]
pub(crate) const PRIOR_FINDINGS_VERSION: i64 = 4;

/// First input version whose blocking findings the verifier checks before publication.
#[expect(dead_code, reason = "called by S16")]
pub(crate) const VERIFIER_VERSION: i64 = 5;

/// Characters a review ID keeps of the head SHA.
const REVIEW_ID_HEAD_CHARS: usize = 12;

/// Length of a full commit SHA in hex digits.
const SHA_LEN: usize = 40;

/// Shortest head SHA prefix a command accepts.
const HEAD_PREFIX_MIN_LEN: usize = 12;

/// Longest repository owner name.
const OWNER_MAX_LEN: usize = 39;

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
/// It matches Go's `^[a-z0-9][a-z0-9-]{0,38}/[a-z0-9_.-]{1,100}$`.
#[must_use]
pub fn repository_valid(repository: &str) -> bool {
    let Some((owner, name)) = repository.split_once('/') else {
        return false;
    };
    if owner.is_empty() || owner.len() > OWNER_MAX_LEN {
        return false;
    }
    if name.is_empty() || name.len() > NAME_MAX_LEN {
        return false;
    }
    let owner_bytes = owner.as_bytes();
    let first = owner_bytes[0];
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    for &byte in &owner_bytes[1..] {
        if !(byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-') {
            return false;
        }
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
#[expect(dead_code, reason = "called by S24")]
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
    use serde::Deserialize;
    use serde_json::Value;

    use super::PullRequest;
    use crate::model::review::{AwaitRecord, PublishedRecord, prior_key};

    /// Go-written identifiers, shared with the Go reference tests.
    const GOLDEN: &str = include_str!("../../../../testdata/parity/ids.json");

    /// Golden cases of identifiers that later slices port, so this test skips them.
    const LATER_CASES: &[&str] = &[
        "session_run1_base",
        "session_run1_start2",
        "session_run2_head_checkout",
    ];

    /// One golden case: a named input and the Go output.
    #[derive(Debug, Deserialize)]
    struct Case {
        name: String,
        input: Value,
        output: Value,
    }

    /// The input of a workflow ID case.
    #[derive(Debug, Deserialize)]
    struct WorkflowInput {
        pull_request: PullRequest,
        attempt: i64,
    }

    /// The input of a prior key case.
    #[derive(Debug, Deserialize)]
    struct PriorKeyInput {
        head: String,
        id: String,
    }

    /// Reads a golden input as a pull request.
    fn pull_request(case: &Case) -> PullRequest {
        serde_json::from_value(case.input.clone())
            .unwrap_or_else(|error| panic!("{}: the input is a pull request: {error}", case.name))
    }

    #[test]
    fn ids_match_go_golden() {
        let cases: Vec<Case> = serde_json::from_str(GOLDEN).expect("golden is JSON");
        let mut checked = 0;
        for case in &cases {
            let actual = match case.name.as_str() {
                "pull_request_key" => Value::from(pull_request(case).key()),
                "review_id" => Value::from(pull_request(case).review_id()),
                "workflow_id_attempt_1" | "workflow_id_attempt_2" => {
                    let input: WorkflowInput = serde_json::from_value(case.input.clone())
                        .unwrap_or_else(|error| panic!("{}: workflow input: {error}", case.name));
                    Value::from(input.pull_request.workflow_id(input.attempt))
                }
                "prior_key_full_sha" | "prior_key_short_head" => {
                    let input: PriorKeyInput = serde_json::from_value(case.input.clone())
                        .unwrap_or_else(|error| panic!("{}: prior key input: {error}", case.name));
                    Value::from(prior_key(&input.head, &input.id))
                }
                "published_record_key" => {
                    let record: PublishedRecord = serde_json::from_value(case.input.clone())
                        .unwrap_or_else(|error| panic!("{}: record input: {error}", case.name));
                    Value::from(record.key())
                }
                "await_record_key" => {
                    let record: AwaitRecord = serde_json::from_value(case.input.clone())
                        .unwrap_or_else(|error| panic!("{}: record input: {error}", case.name));
                    Value::from(record.key())
                }
                "target_key_and_identity" => {
                    let target = pull_request(case).target();
                    serde_json::json!({"key": target.key(), "identity": target.identity()})
                }
                name => {
                    assert!(
                        LATER_CASES.contains(&name),
                        "golden case {name} is not ported"
                    );
                    continue;
                }
            };
            assert_eq!(actual, case.output, "case {}", case.name);
            checked += 1;
        }
        assert_eq!(
            checked, 9,
            "golden has the nine identifier cases of this module"
        );
    }
}
