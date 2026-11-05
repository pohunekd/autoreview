//! Parsing of the structured output of a finished Claude session.

// Rust guideline compliant 2026-06-26

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Error;
use crate::model::{Finding, PriorVerdict, PullRequest, ReviewerResult};
use crate::protocol::{decode_go_base64, go_utf8_lossy, refuse_case_variant_keys};

/// Protocol name of the wrapper around a finished structured Claude session.
pub(crate) const CLAUDE_RESULT_PROTOCOL: &str = "claude-otel-noninteractive/v2";

/// Severities that hold the merge back when a finding flags itself as blocking.
const BLOCKING_SEVERITIES: [&str; 2] = ["critical", "high"];

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
}

autoreview_go_compat::object_only_serde!(ClaudeWrapper);

/// Kind of the message, `result` for the final one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub(crate) struct ClaudeOuter {
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
    pub(crate) structured_output: ClaudeArtifact,
}

autoreview_go_compat::object_only_serde!(ClaudeOuter);

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
/// result that the engine refuses, and `artifact_invalid` for an artifact that does not match
/// the pull request or that has a finding the engine refuses.
#[cfg_attr(not(test), expect(dead_code, reason = "called by S26"))]
pub(crate) fn parse_claude_output(pr: &PullRequest, raw: &str) -> Result<ReviewerResult, Error> {
    let artifact = decode_structured(raw)?;
    if artifact.repository != pr.repository
        || artifact.number != pr.number
        || artifact.head_sha != pr.head_sha
        || artifact.findings.is_none()
    {
        return Err(artifact_invalid());
    }
    let Some(findings) = artifact.findings else {
        return Err(artifact_invalid());
    };
    let mut result_findings = Vec::new();
    for finding in findings {
        if finding.side != "RIGHT" || !is_source_id(&finding.id) || finding.severity == "blocker" {
            return Err(artifact_invalid());
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
/// The prose result is checked against the structured output by `check_prose_agreement`.
fn decode_structured(raw: &str) -> Result<ClaudeArtifact, Error> {
    if raw.trim().is_empty() {
        return Err(Error::workflow("review_output_incomplete", false, None));
    }
    refuse_case_variant_keys(raw, "review_wrapper_invalid")?;
    let wrapper: ClaudeWrapper = serde_json::from_str(raw).map_err(|_| wrapper_invalid())?;
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
    let outer: ClaudeOuter = serde_json::from_str(&text).map_err(|_| result_invalid())?;
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
fn check_prose_agreement(prose: &str, structured: &ClaudeArtifact) -> Result<(), Error> {
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
    let parsed: ClaudeArtifact = match serde_json::from_str(prose) {
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

/// The wrapper of a Claude session that the engine refuses.
fn wrapper_invalid() -> Error {
    Error::workflow("review_wrapper_invalid", true, None)
}

/// The outer message of a Claude session that the engine refuses.
fn result_invalid() -> Error {
    Error::workflow("claude_result_invalid", true, None)
}

/// The artifact of a Claude session that the engine refuses.
fn artifact_invalid() -> Error {
    Error::workflow("artifact_invalid", true, None)
}

/// The prose of a Claude session that does not match its structured output.
fn claude_mismatch() -> Error {
    Error::workflow("claude_result_mismatch", true, None)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::error::error_code;
    use crate::protocol::fixtures::{claude_output, claude_session, sha, test_pull_request};

    use super::parse_claude_output;

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
}
