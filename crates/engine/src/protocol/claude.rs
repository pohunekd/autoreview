//! Parsing of the structured output of a finished Claude session.

// Rust guideline compliant 2026-06-26

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Error;
use crate::model::{Finding, PriorVerdict, PullRequest, ReviewerResult};
use crate::protocol::validate::{review_head_unavailable, reviewer_output_invalid};
use crate::protocol::{decode_go_base64, go_utf8_lossy, refuse_case_variant_keys};

/// Protocol name of the wrapper around a finished structured Claude session.
pub(crate) const CLAUDE_RESULT_PROTOCOL: &str = "claude-otel-noninteractive/v2";

/// Severities that hold the merge back when a finding flags itself as blocking.
const BLOCKING_SEVERITIES: [&str; 2] = ["critical", "high"];

/// Refusal of the adapter when the pull request head is not in the review checkout.
const REFUSAL_HEAD_UNAVAILABLE: &str = "head_unavailable";

/// Exit code of the wrapper when the adapter refuses the run.
const EXIT_REFUSED: i64 = 64;

/// Protocol name of the wrapper.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub(crate) struct ClaudeWrapper {
    /// Protocol name of the wrapper.
    pub(crate) protocol: String,
    /// Output mode of the session, `structured` for schema-enforced output.
    pub(crate) mode: String,
    /// Exit code of the session.
    pub(crate) exit_code: i64,
    /// Base64 of the stdout bytes of the session.
    pub(crate) stdout_base64: String,
    /// Base64 of the stderr bytes of the session.
    pub(crate) stderr_base64: String,
    /// Reason the adapter refused the run; empty when Claude Code ran.
    pub(crate) refusal: String,
}

autoreview_go_compat::object_only_serde!(ClaudeWrapper);

/// Kind of the message, `result` for the final one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct ClaudeOuter<T> {
    /// Kind of the message, `result` for the final one.
    #[serde(rename = "type")]
    pub(crate) r#type: String,
    /// Subtype of the result, `success` when the session finished.
    pub(crate) subtype: String,
    /// Whether the session reported an error.
    pub(crate) is_error: bool,
    /// Prose result of the session.
    pub(crate) result: String,
    /// Schema-enforced output of the session.
    pub(crate) structured_output: T,
}

/// Repository that the review covers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub(crate) struct ClaudeArtifact {
    /// Repository that the review covers.
    pub(crate) repository: String,
    /// Number of the pull request that the review covers.
    pub(crate) number: i64,
    /// Head SHA that the review covers.
    pub(crate) head_sha: String,
    /// Summary of the review.
    pub(crate) summary: String,
    /// Findings; absent or `null` is refused.
    pub(crate) findings: Option<Vec<ClaudeFinding>>,
    /// Verdicts on the prior findings; absent or `null` means none.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    pub(crate) prior_findings: Vec<PriorVerdict>,
}

autoreview_go_compat::object_only_serde!(ClaudeArtifact);

/// Source identifier: `C` followed by at least three digits.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub(crate) struct ClaudeFinding {
    /// Source identifier: `C` followed by at least three digits.
    pub(crate) id: String,
    /// Severity of the finding.
    pub(crate) severity: String,
    /// Confidence of the finding.
    pub(crate) confidence: String,
    /// Path of the finding.
    pub(crate) path: String,
    /// Line of the finding.
    pub(crate) line: i64,
    /// Side of the diff; only `RIGHT` is accepted.
    pub(crate) side: String,
    /// Title of the finding.
    pub(crate) title: String,
    /// Body of the finding.
    pub(crate) body: String,
    /// Whether the reviewer marks the finding as blocking; absent means the finding blocks.
    pub(crate) blocking: Option<bool>,
}

autoreview_go_compat::object_only_serde!(ClaudeFinding);

impl ClaudeFinding {
    /// Reports whether the finding is a note: not blocking, or blocking below the severities
    /// that hold the merge back.
    fn non_blocking(&self) -> bool {
        match self.blocking {
            Some(blocking) => !blocking || !BLOCKING_SEVERITIES.contains(&self.severity.as_str()),
            None => false,
        }
    }
}

/// Parses the structured output of a finished Claude session into a reviewer result.
///
/// # Errors
///
/// Returns `review_output_incomplete` for empty output, `review_wrapper_invalid`,
/// `claude_result_invalid` and `claude_result_mismatch` for a wrapper, outer message or prose
/// result that the engine refuses, and `review_output_invalid` with a reason for an artifact that
/// names another pull request or head, has no findings list or has a finding the engine refuses.
pub(crate) fn parse_claude_output(pr: &PullRequest, raw: &str) -> Result<ReviewerResult, Error> {
    let artifact: ClaudeArtifact = decode_structured(raw)?;
    if artifact.repository != pr.repository || artifact.number != pr.number {
        return Err(reviewer_output_invalid(
            "the output names another pull request".to_owned(),
        ));
    }
    if artifact.head_sha != pr.head_sha {
        return Err(reviewer_output_invalid(
            "the output names another head commit".to_owned(),
        ));
    }
    let Some(findings) = artifact.findings else {
        return Err(reviewer_output_invalid(
            "the output has no findings list".to_owned(),
        ));
    };
    let mut result_findings = Vec::new();
    for (index, finding) in findings.into_iter().enumerate() {
        if finding.side != "RIGHT" || !is_source_id(&finding.id) || finding.severity == "blocker" {
            return Err(reviewer_output_invalid(format!(
                "finding {} has an invalid id, side or severity",
                index + 1
            )));
        }
        let non_blocking = finding.non_blocking();
        result_findings.push(Finding {
            source_id: finding.id,
            severity: finding.severity,
            confidence: finding.confidence,
            path: finding.path,
            line: finding.line,
            title: finding.title,
            body: finding.body,
            non_blocking,
            ..Finding::default()
        });
    }
    Ok(ReviewerResult {
        summary: artifact.summary,
        prior: artifact.prior_findings,
        findings: result_findings,
        ..ReviewerResult::default()
    })
}

/// Reads the schema-enforced structured output of a finished Claude session.
///
/// The prose result is checked against the structured output by `check_prose_agreement`. A
/// refusal of the adapter is `review_head_unavailable` for a missing head, and
/// `review_result_invalid` otherwise.
pub(crate) fn decode_structured<T>(raw: &str) -> Result<T, Error>
where
    T: DeserializeOwned + PartialEq + Default,
{
    if raw.trim().is_empty() {
        return Err(Error::workflow("review_output_incomplete", false, None));
    }
    refuse_case_variant_keys(raw, "review_wrapper_invalid")?;
    let wrapper: ClaudeWrapper = serde_json::from_str(raw).map_err(|_| wrapper_invalid())?;
    if !wrapper.refusal.is_empty() {
        return Err(adapter_refusal(&wrapper));
    }
    let stderr = decode_go_base64(&wrapper.stderr_base64);
    if stderr.is_none()
        || stderr.as_ref().is_some_and(|v| !v.is_empty())
        || wrapper.protocol != CLAUDE_RESULT_PROTOCOL
        || wrapper.mode != "structured"
        || wrapper.exit_code != 0
    {
        return Err(wrapper_invalid());
    }
    let stdout = decode_go_base64(&wrapper.stdout_base64).ok_or_else(wrapper_invalid)?;
    let text = go_utf8_lossy(&stdout);
    refuse_case_variant_keys(&text, "claude_result_invalid")?;
    if !text
        .trim_start_matches([' ', '\t', '\n', '\r'])
        .starts_with('{')
    {
        return Err(result_invalid());
    }
    let outer: ClaudeOuter<T> = serde_json::from_str(&text).map_err(|_| result_invalid())?;
    if outer.r#type != "result" || outer.subtype != "success" || outer.is_error {
        return Err(result_invalid());
    }
    check_prose_agreement(&outer.result, &outer.structured_output)?;
    Ok(outer.structured_output)
}

/// Checks the prose result against the structured output.
///
/// Prose that is not a JSON object or `null` is not compared. A JSON object or `null` must read
/// as the same artifact, so a duplicate key, a null field or a type error in it is refused.
fn check_prose_agreement<T>(prose: &str, structured: &T) -> Result<(), Error>
where
    T: DeserializeOwned + PartialEq,
{
    let value: Value = match serde_json::from_str(prose) {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };
    if !value.is_object() && !value.is_null() {
        return Ok(());
    }
    if value.is_object() {
        refuse_case_variant_keys(prose, "claude_result_mismatch")?;
    }
    let parsed: T = match serde_json::from_str(prose) {
        Ok(v) => v,
        Err(_) => return Err(claude_mismatch()),
    };
    if parsed == *structured {
        Ok(())
    } else {
        Err(claude_mismatch())
    }
}

/// Reports whether `id` is `C` and at least three ASCII digits, as Go's `^C[0-9]{3,}$`.
fn is_source_id(id: &str) -> bool {
    if let Some(digits) = id.strip_prefix('C') {
        digits.len() >= 3 && digits.bytes().all(|b| b.is_ascii_digit())
    } else {
        false
    }
}

/// The error of a wrapper with a refusal, which never carries a review.
///
/// Only the adapter's exact refusal of a missing head stops the review; any other refusal is
/// a run that a fresh session may repeat.
fn adapter_refusal(wrapper: &ClaudeWrapper) -> Error {
    if wrapper.protocol == CLAUDE_RESULT_PROTOCOL
        && wrapper.exit_code == EXIT_REFUSED
        && wrapper.stdout_base64.is_empty()
        && wrapper.refusal == REFUSAL_HEAD_UNAVAILABLE
    {
        review_head_unavailable()
    } else {
        Error::workflow("review_result_invalid", true, None)
    }
}

/// The wrapper of a Claude session that the engine refuses.
fn wrapper_invalid() -> Error {
    Error::workflow("review_wrapper_invalid", true, None)
}

/// The outer message of a Claude session that the engine refuses.
fn result_invalid() -> Error {
    Error::workflow("claude_result_invalid", true, None)
}

/// The prose of a Claude session that does not match its structured output.
fn claude_mismatch() -> Error {
    Error::workflow("claude_result_mismatch", true, None)
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use serde_json::json;

    use crate::error::error_code;
    use crate::protocol::fixtures::{claude_output, claude_session, sha, test_pull_request};
    use crate::protocol::validate::reviewer_output_retryable;

    use super::{CLAUDE_RESULT_PROTOCOL, parse_claude_output};

    #[test]
    fn claude_finding_blocks_unless_it_says_otherwise() {
        let pr = test_pull_request(3);
        let output = claude_output(
            &pr,
            json!([
                {"id": "C001", "severity": "high", "confidence": "high", "blocking": true,
                 "path": "a.go", "line": 1, "side": "RIGHT", "title": "Bug", "body": "Details."},
                {"id": "C002", "severity": "low", "confidence": "high", "blocking": false,
                 "path": "a.go", "line": 2, "side": "RIGHT", "title": "Note", "body": "Details."},
                {"id": "C003", "severity": "medium", "confidence": "high", "path": "a.go",
                 "line": 3, "side": "RIGHT", "title": "Unflagged", "body": "Details."},
                {"id": "C004", "severity": "medium", "confidence": "high", "blocking": true,
                 "path": "a.go", "line": 4, "side": "RIGHT", "title": "Below the floor",
                 "body": "Details."}
            ]),
            None,
        );
        let result = parse_claude_output(&pr, &output).expect("the output parses");
        let blocking: Vec<bool> = result
            .findings
            .iter()
            .map(|finding| !finding.non_blocking)
            .collect();
        assert_eq!(blocking, vec![true, false, true, false]);
    }

    #[test]
    fn case_variant_keys_are_refused() {
        let pr = test_pull_request(3);
        let structured = json!({
            "repository": "acme/widget",
            "REPOSITORY": "acme/widget",
            "number": 3,
            "head_sha": sha("a"),
            "summary": "Reviewed the change thoroughly.",
            "findings": []
        });
        let err = parse_claude_output(&pr, &claude_session(&structured))
            .expect_err("two keys that differ only in case are refused");
        assert_eq!(error_code(&err), "claude_result_invalid");
    }

    // A refusal envelope of the adapter names its reason. A head that is not in the review
    // checkout stops the review; a fresh run on the same repository cannot find it either (#253).
    #[test]
    fn adapter_refusal_of_a_missing_head_is_not_retried() {
        let pr = test_pull_request(3);
        let review = claude_output(&pr, json!([]), None);
        let mut success: serde_json::Value =
            serde_json::from_str(&review).expect("the fixture is JSON");
        success["refusal"] = json!("head_unavailable");
        let refusal = |reason: &str, exit_code: i64| {
            let stderr = STANDARD.encode("claude-adapter: the head is unavailable\n");
            format!(
                "{}\n",
                json!({
                    "protocol": CLAUDE_RESULT_PROTOCOL,
                    "mode": "structured",
                    "exit_code": exit_code,
                    "stdout_base64": "",
                    "stderr_base64": stderr,
                    "refusal": reason
                })
            )
        };
        let cases = [
            (
                "head unavailable",
                refusal("head_unavailable", 64),
                "review_head_unavailable",
            ),
            (
                "other refusal",
                refusal("checkout_invalid", 64),
                "review_result_invalid",
            ),
            (
                "exit status 0",
                refusal("head_unavailable", 0),
                "review_result_invalid",
            ),
            (
                "refusal with a review",
                format!("{success}\n"),
                "review_result_invalid",
            ),
        ];
        for (name, output, code) in cases {
            let err = parse_claude_output(&pr, &output).expect_err(name);
            assert_eq!(error_code(&err), code, "{name}");
            assert_eq!(
                reviewer_output_retryable(&err),
                code == "review_result_invalid",
                "{name}"
            );
        }
    }
}
