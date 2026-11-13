//! Parsing of the structured output of a finished Claude session.

// Rust guideline compliant 2026-06-26

use autoreview_policy::Checkout;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{BoxError, Error};
use crate::model::{Finding, PriorVerdict, PullRequest, ReviewerResult};
use crate::protocol::reason::trailing_reason;
use crate::protocol::request::checkout_name;
use crate::protocol::validate::{
    review_adapter_outdated, review_head_unavailable, reviewer_output_invalid,
};
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
    /// Checkout that the adapter checked before and after the run, `base` or `head`; an older
    /// adapter leaves it out.
    pub(crate) checked_checkout: String,
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

/// Parses the structured output of a finished Claude session on `checkout` into a reviewer
/// result.
///
/// # Errors
///
/// Returns `review_output_incomplete` for empty output, `review_wrapper_invalid`,
/// `claude_result_invalid` and `claude_result_mismatch` for a wrapper, outer message or prose
/// result that the engine refuses, the codes of `decode_structured` for a refusal or an older
/// adapter, and `review_output_invalid` with a reason for an artifact that names another pull
/// request or head, has no findings list or has a finding the engine refuses.
pub(crate) fn parse_claude_output(
    pr: &PullRequest,
    checkout: &Checkout,
    raw: &str,
) -> Result<ReviewerResult, Error> {
    let artifact: ClaudeArtifact = decode_structured(raw, checkout)?;
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

/// Reads the schema-enforced structured output of a finished Claude session on `checkout`.
///
/// The prose result is checked against the structured output by `check_prose_agreement`. A
/// refusal of the adapter is `review_head_unavailable` for a missing head, and
/// `review_result_invalid` otherwise. A wrapper of the protocol whose `checked_checkout` is not
/// the name of `checkout` is `review_adapter_outdated`, whatever its exit code: only an adapter
/// that checked the review checkout writes it, outside the base64 output of the run. A
/// `review_wrapper_invalid` of a run whose exit code is not 0 carries the end of its stderr.
/// A `review_result_invalid` of a refusal carries the adapter's message from its stderr.
pub(crate) fn decode_structured<T>(raw: &str, checkout: &Checkout) -> Result<T, Error>
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
    if wrapper.protocol == CLAUDE_RESULT_PROTOCOL
        && wrapper.checked_checkout != checkout_name(checkout)
    {
        return Err(review_adapter_outdated());
    }
    let stderr = decode_go_base64(&wrapper.stderr_base64);
    if stderr.is_none()
        || stderr.as_ref().is_some_and(|v| !v.is_empty())
        || wrapper.protocol != CLAUDE_RESULT_PROTOCOL
        || wrapper.mode != "structured"
        || wrapper.exit_code != 0
    {
        return Err(wrapper_refused(wrapper.exit_code, stderr.as_deref()));
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
/// a run that a fresh session may repeat. Its cause is the end of the decoded stderr, where
/// the adapter writes its message; the `refusal` field holds only the kind. The stderr is
/// untrusted, so it is bounded and neutralized, and it only goes into the log line of the
/// refused output.
fn adapter_refusal(wrapper: &ClaudeWrapper) -> Error {
    if wrapper.protocol == CLAUDE_RESULT_PROTOCOL
        && wrapper.exit_code == EXIT_REFUSED
        && wrapper.stdout_base64.is_empty()
        && wrapper.refusal == REFUSAL_HEAD_UNAVAILABLE
    {
        return review_head_unavailable();
    }
    let reason = decode_go_base64(&wrapper.stderr_base64)
        .map(|stderr| trailing_reason(&stderr))
        .unwrap_or_default();
    if reason.is_empty() {
        return Error::workflow("review_result_invalid", true, None);
    }
    Error::workflow(
        "review_result_invalid",
        true,
        Some(BoxError::from(format!(
            "Claude adapter refused the run: {reason}"
        ))),
    )
}

/// Error of a refused wrapper, with the end of the stderr of a failed run.
///
/// The end of the decoded `stderr` of a run whose `exit_code` is not 0 is the cause. It quotes
/// Claude Code's output, which is untrusted, so it is bounded and neutralized. It only goes
/// into the log line of the refused output: the recorded failure keeps only the code.
fn wrapper_refused(exit_code: i64, stderr: Option<&[u8]>) -> Error {
    let reason = match stderr {
        Some(bytes) if exit_code != 0 => trailing_reason(bytes),
        _ => String::new(),
    };
    if reason.is_empty() {
        return wrapper_invalid();
    }
    Error::workflow(
        "review_wrapper_invalid",
        true,
        Some(BoxError::from(format!(
            "Claude Code exited with status {exit_code}: {reason}"
        ))),
    )
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
    use autoreview_policy::Checkout;
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use serde_json::{Value, json};

    use crate::error::error_code;
    use crate::protocol::fixtures::{
        claude_output, claude_session, claude_session_raw, sha, test_pull_request,
    };
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
        let result = parse_claude_output(&pr, &Checkout::Base, &output).expect("the output parses");
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
        let err = parse_claude_output(&pr, &Checkout::Base, &claude_session(&structured))
            .expect_err("two keys that differ only in case are refused");
        assert_eq!(error_code(&err), "claude_result_invalid");
    }

    // One row per adapter stdout. The prose object rows reach the object branch of
    // `check_prose_agreement`; the repeated key row is refused.
    #[test]
    fn prose_and_stdout_rows_follow_the_protocol() {
        let pr = test_pull_request(5);
        let structured = json!({
            "repository": "acme/widget",
            "number": 5,
            "head_sha": sha("a"),
            "summary": "Reviewed the change thoroughly and found the listed issues.",
            "findings": []
        });
        let other = json!({
            "repository": "acme/widget",
            "number": 5,
            "head_sha": sha("a"),
            "summary": "Another summary.",
            "findings": []
        });
        let session = |result: Value, output: &Value| {
            json!({
                "type": "result",
                "subtype": "success",
                "is_error": false,
                "result": result,
                "structured_output": output
            })
            .to_string()
        };
        let duplicate = format!(
            r#"{{"type":"result","subtype":"success","is_error":false,"result":"Prose summary.","structured_output":{{"repository":"acme/widget","number":5,"head_sha":"{}","summary":"Reviewed the change thoroughly and found the listed issues.","findings":[],"findings":[{{"id":"C001","severity":"high","confidence":"high","path":"a.go","line":4,"side":"RIGHT","title":"Bug","body":"Details."}}]}}}}"#,
            sha("a")
        );
        let rows: Vec<(&str, String, Option<&str>)> = vec![
            (
                "prose null",
                session(json!("null"), &structured),
                Some("claude_result_mismatch"),
            ),
            (
                "prose object that differs",
                session(json!(other.to_string()), &structured),
                Some("claude_result_mismatch"),
            ),
            (
                "prose object that matches",
                session(json!(structured.to_string()), &structured),
                None,
            ),
            (
                "stdout null",
                "null".to_owned(),
                Some("claude_result_invalid"),
            ),
            ("repeated key", duplicate, Some("claude_result_invalid")),
        ];
        for (name, stdout, expected) in rows {
            let raw = claude_session_raw(&stdout);
            let outcome = parse_claude_output(&pr, &Checkout::Base, &raw);
            match expected {
                Some(code) => assert_eq!(error_code(&outcome.expect_err(name)), code, "{name}"),
                None => {
                    outcome.expect(name);
                }
            }
        }
    }

    // A refusal envelope of the adapter names its reason. A head that is not in the review
    // checkout stops the review; a fresh run on the same repository cannot find it either (#253).
    // A result without the checkout that the adapter checked comes from an adapter that may not
    // have checked it, and a fresh run of that adapter would write the same result. Any other
    // refusal gets the adapter's message from its stderr, bounded and neutralized, as the
    // cause of `review_result_invalid` (#323).
    #[test]
    fn adapter_refusal_of_a_missing_head_is_not_retried() {
        let pr = test_pull_request(3);
        let review = claude_output(&pr, json!([]), None);
        let mut success: serde_json::Value =
            serde_json::from_str(&review).expect("the fixture is JSON");
        let mut unchecked = success.clone();
        unchecked
            .as_object_mut()
            .expect("the fixture is an object")
            .remove("checked_checkout");
        let mut other_checkout = success.clone();
        other_checkout["checked_checkout"] = json!("head");
        success["refusal"] = json!("head_unavailable");
        let refusal = |reason: &str, exit_code: i64, message: &str| {
            let stderr = STANDARD.encode(format!("claude-adapter: {message}\n"));
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
        let missing = "the head is unavailable";
        let refused = "Claude adapter refused the run: claude-adapter: the head is unavailable";
        let cases = [
            (
                "head unavailable",
                refusal("head_unavailable", 64, missing),
                "review_head_unavailable",
                None,
            ),
            (
                "other refusal",
                refusal("checkout_invalid", 64, missing),
                "review_result_invalid",
                Some(refused),
            ),
            (
                "exit status 0",
                refusal("head_unavailable", 0, missing),
                "review_result_invalid",
                Some(refused),
            ),
            (
                "refusal with a review",
                format!("{success}\n"),
                "review_result_invalid",
                None,
            ),
            (
                "no checked checkout",
                format!("{unchecked}\n"),
                "review_adapter_outdated",
                None,
            ),
            (
                "other checked checkout",
                format!("{other_checkout}\n"),
                "review_adapter_outdated",
                None,
            ),
            (
                "refusal message with control and format characters",
                refusal("checkout_invalid", 64, "a\u{1b}[2J\u{202e}b\nc"),
                "review_result_invalid",
                Some("Claude adapter refused the run: claude-adapter: a?[2J?b?c"),
            ),
        ];
        for (name, output, code, cause) in cases {
            let err = parse_claude_output(&pr, &Checkout::Base, &output).expect_err(name);
            assert_eq!(error_code(&err), code, "{name}");
            assert_eq!(
                reviewer_output_retryable(&err),
                code == "review_result_invalid",
                "{name}"
            );
            let message = match cause {
                Some(cause) => format!("{code}: {cause}"),
                None => code.to_owned(),
            };
            assert_eq!(err.to_string(), message, "{name}");
        }
        parse_claude_output(&pr, &Checkout::Head, &format!("{other_checkout}\n"))
            .expect("a head result is read on a head checkout");
    }

    // A Claude Code run that exits with a status other than 0 gets the end of its stderr,
    // bounded and neutralized, as the cause of `review_wrapper_invalid`; any other wrapper
    // that the engine refuses keeps the bare code (#304).
    #[test]
    fn failed_claude_run_carries_the_bounded_end_of_its_stderr() {
        let pr = test_pull_request(3);
        let wrapper = |exit_code: i64, stderr: String| {
            format!(
                "{}\n",
                json!({
                    "protocol": CLAUDE_RESULT_PROTOCOL,
                    "mode": "structured",
                    "exit_code": exit_code,
                    "stdout_base64": "",
                    "stderr_base64": stderr,
                    "checked_checkout": "base"
                })
            )
        };
        let failed = |stderr: &[u8]| wrapper(1, STANDARD.encode(stderr));
        let long = format!("{}\nError: last", "x".repeat(600));
        let cut = ["\u{e9}".as_bytes(), &b"y".repeat(511)].concat();
        let cases = [
            (
                "last error line",
                failed(b"Warning: slow network\nError: Invalid API key\n"),
                Some("status 1: Warning: slow network?Error: Invalid API key".to_owned()),
            ),
            (
                "status of a signal",
                wrapper(-9, STANDARD.encode("Killed\n")),
                Some("status -9: Killed".to_owned()),
            ),
            (
                "control and format characters",
                failed("a\u{1b}[31mb\u{202e}c\u{85}d\te\u{200b}f\u{2028}g".as_bytes()),
                Some("status 1: a?[31mb?c?d?e?f?g".to_owned()),
            ),
            (
                "invalid bytes",
                failed(b"Error: \xff\xfe x"),
                Some("status 1: Error: \u{FFFD}\u{FFFD} x".to_owned()),
            ),
            (
                "end of a long stderr",
                failed(long.as_bytes()),
                Some(format!("status 1: {}?Error: last", "x".repeat(500))),
            ),
            (
                "cut inside a character",
                failed(&cut),
                Some(format!("status 1: {}", "y".repeat(511))),
            ),
            (
                "replacement characters over the bound",
                failed(&[0xFF; 200]),
                Some(format!("status 1: {}", "\u{FFFD}".repeat(170))),
            ),
            ("empty stderr", failed(b""), None),
            ("blank stderr", failed(b" \n\t\r\n"), None),
            (
                "stderr of a run with status 0",
                wrapper(0, STANDARD.encode("Error: x\n")),
                None,
            ),
            ("invalid base64", wrapper(1, "%%".to_owned()), None),
        ];
        for (name, output, reason) in cases {
            let err = parse_claude_output(&pr, &Checkout::Base, &output).expect_err(name);
            assert_eq!(error_code(&err), "review_wrapper_invalid", "{name}");
            assert!(reviewer_output_retryable(&err), "{name}");
            let message = match reason {
                Some(reason) => {
                    format!("review_wrapper_invalid: Claude Code exited with {reason}")
                }
                None => "review_wrapper_invalid".to_owned(),
            };
            assert_eq!(err.to_string(), message, "{name}");
        }
    }
}
