//! Parsing of the envelope of a finished Codex session.

// Rust guideline compliant 2026-06-26

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::error::Error;
use crate::model::{Finding, PriorVerdict, PullRequest, ReviewerResult};
use crate::protocol::validate::{count_over, review_head_unavailable, sort_findings};
use crate::protocol::{decode_go_base64, go_utf8_lossy, refuse_case_variant_keys};

/// Opens the base64 report of a Codex envelope.
const ENVELOPE_MARKER: &str = "[pohunek-codex-review-result-v1:";

/// Exit line of a Codex session that finished with status 0.
const EXIT_LINE: &str = "[pohunek-codex-review exited with status 0]";

/// Exit line of a Codex session that the adapter refused.
const REFUSED_EXIT_LINE: &str = "[pohunek-codex-review exited with status 64]";

/// Line of the adapter, right before its exit line, when the pull request head is missing.
const HEAD_UNAVAILABLE_MARKER: &str = "[pohunek-codex-review-refused-v1:head_unavailable]";

/// Line breaks that may separate the report from the exit line.
const SEPARATORS: [&str; 6] = ["\n", "\r\n", "\n\n", "\n\r\n", "\r\n\n", "\r\n\r\n"];

/// Line endings that may follow the exit line at the end of the output.
const ENDINGS: [&str; 4] = ["\r\n", "\n", "\r", ""];

/// Severities that hold the merge back when a finding flags itself as blocking.
const BLOCKING_SEVERITIES: [&str; 3] = ["blocker", "critical", "high"];

/// Findings; absent or `null` is refused.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, remote = "Self")]
pub(crate) struct CodexReport {
    /// Findings; absent or `null` is refused.
    pub(crate) findings: Option<Vec<CodexFinding>>,
    /// Summary of the review.
    pub(crate) summary: String,
    /// Tests that the reviewer ran; absent or `null` is refused.
    pub(crate) tests: Option<Vec<String>>,
    /// Status of the review; only `completed` is accepted.
    pub(crate) status: String,
    /// Head SHA that the review covers.
    pub(crate) reviewed_head_sha: String,
    /// Verdicts on the prior findings; absent or `null` is refused.
    pub(crate) prior_findings: Option<Vec<CodexVerdict>>,
}

autoreview_go_compat::object_only_serde!(CodexReport);

/// Severity of the finding.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, remote = "Self")]
pub(crate) struct CodexFinding {
    /// Severity of the finding.
    pub(crate) severity: String,
    /// Confidence of the finding.
    pub(crate) confidence: String,
    /// Whether the reviewer marks the finding as blocking; absent or `null` is refused.
    pub(crate) blocking: Option<bool>,
    /// Path of the finding.
    pub(crate) path: String,
    /// Line of the finding.
    pub(crate) line: i64,
    /// Title of the finding.
    pub(crate) title: String,
    /// Body of the finding.
    pub(crate) body: String,
}

autoreview_go_compat::object_only_serde!(CodexFinding);

impl CodexFinding {
    /// Reports whether the finding is a note: not blocking, or blocking below the severities
    /// that hold the merge back.
    fn non_blocking(&self) -> bool {
        match self.blocking {
            Some(blocking) => !blocking || !BLOCKING_SEVERITIES.contains(&self.severity.as_str()),
            None => false,
        }
    }
}

/// Verdict of a Codex reviewer on one prior finding.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, remote = "Self")]
pub(crate) struct CodexVerdict {
    /// Key of the prior finding.
    pub(crate) key: String,
    /// Outcome of the verdict.
    pub(crate) status: String,
    /// Note explaining the verdict.
    pub(crate) note: String,
    /// Login of the comment that an answered verdict rests on, empty when the code alone decides.
    pub(crate) relies_on: String,
}

autoreview_go_compat::object_only_serde!(CodexVerdict);

/// Parses the envelope of a finished Codex session into a reviewer result.
///
/// Source identifiers are assigned after the findings are sorted, as `C001`, `C002` and so on.
/// A finding blocks when it flags itself as blocking at severity blocker, critical or high.
///
/// # Errors
///
/// Returns `review_result_invalid` for an output, report or head that the engine refuses,
/// `review_adapter_outdated` for a report without `prior_findings` or a finding without
/// `blocking`, and `review_head_unavailable` when the adapter refused a missing head.
pub(crate) fn parse_codex_output(
    cfg: &Config,
    pr: &PullRequest,
    raw: &str,
) -> Result<ReviewerResult, Error> {
    if refused_for_missing_head(raw) {
        return Err(review_head_unavailable());
    }
    let encoded = envelope_payload(raw).ok_or_else(result_invalid)?;
    let decoded = decode_go_base64(encoded)
        .filter(|v| !v.is_empty())
        .ok_or_else(result_invalid)?;
    if count_over(decoded.len(), cfg.result_max_bytes) {
        return Err(result_invalid());
    }
    let text = go_utf8_lossy(&decoded);
    refuse_case_variant_keys(&text, "review_result_invalid")?;
    let report: CodexReport = serde_json::from_str(&text).map_err(|_| result_invalid())?;
    if report.findings.is_none()
        || report.tests.is_none()
        || report.status != "completed"
        || report.reviewed_head_sha != pr.head_sha
    {
        return Err(result_invalid());
    }
    let findings = report.findings.unwrap_or_default();
    let tests = report.tests.unwrap_or_default();
    let Some(verdicts) = report.prior_findings else {
        return Err(adapter_outdated());
    };
    if findings.iter().any(|finding| finding.blocking.is_none()) {
        return Err(adapter_outdated());
    }
    let mut mapped: Vec<Finding> = findings
        .into_iter()
        .map(|source| {
            let non_blocking = source.non_blocking();
            Finding {
                severity: source.severity,
                confidence: source.confidence,
                path: source.path,
                line: source.line,
                title: source.title,
                body: source.body,
                non_blocking,
                ..Finding::default()
            }
        })
        .collect();
    sort_findings(&mut mapped);
    for (index, finding) in mapped.iter_mut().enumerate() {
        finding.source_id = format!("C{:03}", index + 1);
    }
    let prior = verdicts
        .into_iter()
        .map(|verdict| PriorVerdict {
            key: verdict.key,
            status: verdict.status,
            note: verdict.note,
            relies_on: verdict.relies_on,
        })
        .collect();
    Ok(ReviewerResult {
        summary: report.summary,
        findings: mapped,
        tests,
        prior,
        ..ReviewerResult::default()
    })
}

/// Reports whether `raw` ends with the adapter's marker of a missing head and the exit line
/// with status 64.
///
/// The marker must come right before the exit line, separated by one of `SEPARATORS`, as the
/// adapter writes both at once. A run could print the marker too, but a run that exits with
/// status 64 is a refusal anyway, so it stops the review and never publishes.
fn refused_for_missing_head(raw: &str) -> bool {
    ENDINGS
        .iter()
        .filter_map(|ending| raw.strip_suffix(ending))
        .filter_map(|rest| rest.strip_suffix(REFUSED_EXIT_LINE))
        .any(|head| {
            SEPARATORS
                .iter()
                .filter_map(|separator| head.strip_suffix(separator))
                .any(|before| before.ends_with(HEAD_UNAVAILABLE_MARKER))
        })
}

/// Returns the base64 text of the envelope that ends `raw`, or `None` when Go's envelope pattern
/// does not match.
///
/// The output ends with the exit line followed by one of `ENDINGS`. Before the exit line comes one
/// of `SEPARATORS`, after the `]` that closes the report. The base64 text runs back from that `]`
/// to the marker. It holds no bracket and no line break, so only one envelope can match.
fn envelope_payload(raw: &str) -> Option<&str> {
    let mut ending = None;
    for e in ENDINGS {
        if let Some(rest) = raw.strip_suffix(e)
            && rest.ends_with(EXIT_LINE)
        {
            ending = Some(rest);
            break;
        }
    }
    let ending = ending?;
    let head = ending.strip_suffix(EXIT_LINE)?;
    let close = head.rfind(']')?;
    let sep = head.get(close + 1..)?;
    if !SEPARATORS.contains(&sep) {
        return None;
    }
    let before = head.get(..close)?;
    let mut run_start = 0;
    let mut found_non_base64 = false;
    for (index, character) in before.char_indices().rev() {
        if !is_base64_char(character) {
            run_start = index + character.len_utf8();
            found_non_base64 = true;
            break;
        }
    }
    if !found_non_base64 {
        run_start = 0;
    }
    let payload = before.get(run_start..)?;
    let marker_part = before.get(..run_start)?;
    if payload.is_empty() || !marker_part.ends_with(ENVELOPE_MARKER) {
        return None;
    }
    Some(payload)
}

/// Reports whether `character` belongs to the base64 alphabet that the envelope accepts.
fn is_base64_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '+' || character == '/' || character == '='
}

/// The envelope of a Codex session that the engine refuses.
fn result_invalid() -> Error {
    Error::workflow("review_result_invalid", true, None)
}

/// The report of a Codex adapter that sends no blocking flag or no verdicts.
///
/// It is not retried: a fresh run of the same adapter leaves out the same fields.
fn adapter_outdated() -> Error {
    Error::workflow("review_adapter_outdated", true, None)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::error::error_code;
    use crate::protocol::fixtures::{codex_report, test_pull_request};
    use crate::protocol::validate::reviewer_output_retryable;
    use crate::protocol::validate::tests::test_config;

    use super::parse_codex_output;

    // A Codex note does not block: a finding blocks only when it says so at a severity that
    // holds the merge back (#23).
    #[test]
    fn codex_finding_blocks_only_when_it_says_so() {
        let pr = test_pull_request(3);
        let output = codex_report(
            &pr,
            json!({
                "findings": [
                    {"severity": "high", "confidence": "high", "blocking": true,
                     "path": "a.go", "line": 1, "title": "Bug", "body": "Details."},
                    {"severity": "low", "confidence": "high", "blocking": false,
                     "path": "a.go", "line": 2, "title": "Note", "body": "Details."},
                    {"severity": "medium", "confidence": "high", "blocking": true,
                     "path": "a.go", "line": 3, "title": "Below the floor", "body": "Details."},
                    {"severity": "blocker", "confidence": "high", "blocking": true,
                     "path": "a.go", "line": 4, "title": "Blocker", "body": "Details."},
                    {"severity": "critical", "confidence": "high", "blocking": false,
                     "path": "a.go", "line": 5, "title": "Accepted", "body": "Details."}
                ],
                "prior_findings": []
            }),
        );
        let result = parse_codex_output(&test_config(), &pr, &output).expect("the report parses");
        let blocking: Vec<(&str, bool)> = result
            .findings
            .iter()
            .map(|finding| (finding.title.as_str(), !finding.non_blocking))
            .collect();
        assert_eq!(
            blocking,
            [
                ("Blocker", true),
                ("Accepted", false),
                ("Bug", true),
                ("Below the floor", false),
                ("Note", false),
            ]
        );
    }

    // The report of an adapter without the blocking flag or the verdicts is refused for good: a
    // fresh run of the same adapter cannot add them (#23).
    #[test]
    fn codex_report_without_blocking_or_prior_findings_is_refused() {
        let pr = test_pull_request(3);
        let finding = json!({"severity": "high", "confidence": "high", "path": "a.go", "line": 1,
                             "title": "Bug", "body": "Details."});
        let reports = [
            ("no prior_findings", json!({"findings": []})),
            (
                "no blocking flag",
                json!({"findings": [finding], "prior_findings": []}),
            ),
        ];
        for (name, report) in reports {
            let err = parse_codex_output(&test_config(), &pr, &codex_report(&pr, report))
                .expect_err(name);
            assert_eq!(error_code(&err), "review_adapter_outdated", "{name}");
            assert!(!reviewer_output_retryable(&err), "{name}");
        }
    }

    // The adapter's marker right before the exit line with status 64 means the head is not in
    // the review checkout; a fresh run on the same repository cannot find it either (#253).
    #[test]
    fn adapter_refusal_of_a_missing_head_is_not_retried() {
        let pr = test_pull_request(3);
        let marker = "[pohunek-codex-review-refused-v1:head_unavailable]";
        let message = "codex-adapter: trusted pull request head is unavailable in the review \
checkout";
        let refused = "[pohunek-codex-review exited with status 64]";
        let cases = [
            (
                "refusal on the terminal",
                format!("{message}\r\n\r\n{marker}\r\n{refused}\r\n"),
                "review_head_unavailable",
            ),
            (
                "refusal",
                format!("{marker}\n{refused}"),
                "review_head_unavailable",
            ),
            (
                "text after the marker",
                format!("{marker}\nmore\n{refused}\n"),
                "review_result_invalid",
            ),
            (
                "marker before status 0",
                format!("{marker}\n[pohunek-codex-review exited with status 0]\n"),
                "review_result_invalid",
            ),
            (
                "other refusal",
                format!("codex-adapter: review checkout is not clean\n\n{refused}\n"),
                "review_result_invalid",
            ),
        ];
        for (name, output, code) in cases {
            let err = parse_codex_output(&test_config(), &pr, &output).expect_err(name);
            assert_eq!(error_code(&err), code, "{name}");
            assert_eq!(
                reviewer_output_retryable(&err),
                code == "review_result_invalid",
                "{name}"
            );
        }
    }
}
