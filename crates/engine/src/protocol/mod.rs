//! Reviewer requests, the validation of reviewer output, and the parsing and combination of the
//! Claude and Codex outputs.

// Rust guideline compliant 2026-06-26

use autoreview_policy::{Protocol, Reviewer};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::Value;

use crate::config::Config;
use crate::error::Error;
use crate::model::{Discussion, ReviewInput, ReviewerResult};

pub(crate) mod claude;
pub(crate) mod codex;
pub(crate) mod combine;
mod reason;
pub(crate) mod request;
pub(crate) mod validate;

/// Replacement character that Go writes for each invalid byte.
const REPLACEMENT: char = '\u{FFFD}';

/// Parses and validates the complete output of one reviewer session.
///
/// The output of either protocol is checked against the verdicts on the prior findings before
/// the result is validated. The reviewer id is set from `reviewer` only after the output parsed.
///
/// # Errors
///
/// Returns the code of the first refusal of the parser of the reviewer's protocol, of the
/// verdict check, or of the validation of the result.
pub(crate) fn parse_reviewer_output(
    cfg: &Config,
    input: &ReviewInput,
    discussion: &Discussion,
    reviewer: &Reviewer,
    raw: &str,
) -> Result<ReviewerResult, Error> {
    let mut result = match reviewer.protocol {
        Protocol::ClaudeStructured => {
            let parsed = claude::parse_claude_output(
                &input.pull_request,
                &input.rule.pipeline.checkout,
                raw,
            )?;
            validate::validate_prior_verdicts(cfg, &input.prior, discussion, &parsed.prior)?;
            parsed
        }
        Protocol::CodexEnvelope => {
            let parsed = codex::parse_codex_output(cfg, &input.pull_request, raw)?;
            validate::validate_prior_verdicts(cfg, &input.prior, discussion, &parsed.prior)?;
            parsed
        }
        Protocol::Unset | Protocol::Other(_) => {
            return Err(Error::workflow("review_protocol_invalid", true, None));
        }
    };
    result.reviewer_id.clone_from(&reviewer.id);
    validate::validate_reviewer_result(cfg, &result)?;
    Ok(result)
}

/// Decodes standard base64 as Go's `StdEncoding.Strict()` does: line breaks are skipped, padding
/// is required and the trailing bits must be zero.
pub(crate) fn decode_go_base64(text: &str) -> Option<Vec<u8>> {
    let filtered: String = text.chars().filter(|c| *c != '\r' && *c != '\n').collect();
    STANDARD.decode(filtered).ok()
}

/// Decodes bytes as UTF-8 with the replacement of Go's `encoding/json`: every byte that starts no
/// valid sequence becomes one U+FFFD. `String::from_utf8_lossy` replaces a whole invalid subpart
/// with one U+FFFD, which gives fewer replacements for a truncated sequence.
pub(crate) fn go_utf8_lossy(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut rest = bytes;
    while let Err(error) = std::str::from_utf8(rest) {
        let valid_up_to = error.valid_up_to();
        if let Some(prefix) = rest.get(..valid_up_to)
            && let Ok(text) = std::str::from_utf8(prefix)
        {
            out.push_str(text);
        }
        out.push(REPLACEMENT);
        rest = rest.get(valid_up_to + 1..).unwrap_or_default();
    }
    if let Ok(text) = std::str::from_utf8(rest) {
        out.push_str(text);
    }
    out
}

/// Refuses text whose JSON objects hold two keys that differ only in ASCII case.
///
/// Go matches keys case-insensitively and keeps the value of the last key, so such a pair decides
/// the value by position. Text that is not JSON is left to the typed decode, which refuses it.
pub(crate) fn refuse_case_variant_keys(text: &str, code: &str) -> Result<(), Error> {
    if let Ok(value) = serde_json::from_str::<Value>(text)
        && has_case_variant_keys(&value)
    {
        return Err(Error::workflow(code, true, None));
    }
    Ok(())
}

/// Reports whether an object anywhere in `value` holds two keys equal under ASCII case folding.
fn has_case_variant_keys(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<String> = map.keys().map(|k| k.to_ascii_lowercase()).collect();
            keys.sort();
            let duplicate = keys
                .windows(2)
                .any(|pair| matches!(pair, [left, right] if left == right));
            if duplicate {
                return true;
            }
            map.values().any(has_case_variant_keys)
        }
        Value::Array(items) => items.iter().any(has_case_variant_keys),
        _ => false,
    }
}

/// Fixtures of the ported Go tests. They keep only the fields the tests read.
#[cfg(test)]
pub(crate) mod fixtures {
    use autoreview_policy::{Pipeline, Protocol, Reviewer, Rule};
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use serde_json::Value;

    use crate::model::{PullRequest, ReviewInput, ReviewScope};
    use crate::protocol::claude::CLAUDE_RESULT_PROTOCOL;

    /// Returns a 40-character SHA made of `character` repeated.
    pub(crate) fn sha(character: &str) -> String {
        character.repeat(40)
    }

    /// Claude reviewer of the fixtures.
    pub(crate) fn opus() -> Reviewer {
        Reviewer {
            id: "opus".to_owned(),
            label: "Opus".to_owned(),
            protocol: Protocol::ClaudeStructured,
            agent_profile: "claude-profile".to_owned(),
            finding_prefix: "O".to_owned(),
        }
    }

    /// Codex reviewer of the fixtures.
    pub(crate) fn codex() -> Reviewer {
        Reviewer {
            id: "codex".to_owned(),
            label: "Codex".to_owned(),
            protocol: Protocol::CodexEnvelope,
            agent_profile: "codex-profile".to_owned(),
            finding_prefix: "X".to_owned(),
        }
    }

    /// Pull request `number` of `acme/widget`.
    pub(crate) fn test_pull_request(number: i64) -> PullRequest {
        PullRequest {
            repository: "acme/widget".to_owned(),
            number,
            head_sha: sha("a"),
            review_request_id: 7,
            title: "Add widget".to_owned(),
            url: format!("https://github.com/acme/widget/pull/{number}"),
            author: "colleague".to_owned(),
            head_branch: "feature".to_owned(),
            base_branch: "main".to_owned(),
            base_sha: sha("b"),
            ..PullRequest::default()
        }
    }

    /// Input of `reviewer` on `pr` with a full scope.
    pub(crate) fn test_input(reviewer: &Reviewer, pr: PullRequest) -> ReviewInput {
        ReviewInput {
            pull_request: pr,
            rule: Rule {
                pipeline: Pipeline {
                    name: "main".to_owned(),
                    protocol: reviewer.protocol.clone(),
                    reviewers: vec![reviewer.clone()],
                    incremental_reviewers: vec![reviewer.clone()],
                    prompt: "Review the pull request.".to_owned(),
                    ..Pipeline::default()
                },
                ..Rule::default()
            },
            scope: ReviewScope {
                mode: "full".to_owned(),
                ..ReviewScope::default()
            },
            ..ReviewInput::default()
        }
    }

    /// Output of a finished Claude session on a base checkout whose structured output is
    /// `structured`.
    pub(crate) fn claude_session(structured: &Value) -> String {
        let outer = serde_json::json!({
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "result": "Prose summary.",
            "structured_output": structured
        });
        let text = outer.to_string();
        let encoded = STANDARD.encode(text.as_bytes());
        let wrapper = serde_json::json!({
            "protocol": CLAUDE_RESULT_PROTOCOL,
            "mode": "structured",
            "exit_code": 0,
            "stdout_base64": encoded,
            "stderr_base64": "",
            "checked_checkout": "base"
        });
        format!("{wrapper}\n")
    }

    /// Output of a Claude session for `pr` with `findings`; `prior` is omitted when `None`.
    pub(crate) fn claude_output(pr: &PullRequest, items: Value, prior: Option<Value>) -> String {
        let mut structured = serde_json::json!({
            "repository": pr.repository,
            "number": pr.number,
            "head_sha": pr.head_sha,
            "summary": "Reviewed the change thoroughly and found the listed issues.",
            "findings": items
        });
        if let Some(prior_value) = prior
            && let Some(map) = structured.as_object_mut()
        {
            map.insert("prior_findings".to_owned(), prior_value);
        }
        claude_session(&structured)
    }

    /// Envelope of a finished Codex session for `pr` with `findings`, no verdicts and `status`.
    pub(crate) fn codex_output(pr: &PullRequest, findings: Value, status: i64) -> String {
        let report = serde_json::json!({
            "findings": findings,
            "summary": "Full review of the pull request diff; details follow.",
            "tests": ["go test ./..."],
            "status": "completed",
            "reviewed_head_sha": pr.head_sha,
            "prior_findings": []
        });
        let encoded = STANDARD.encode(report.to_string().as_bytes());
        let header = "[pohunek-codex-review-result-v1:";
        let footer = format!("[pohunek-codex-review exited with status {status}]");
        format!("{header}{encoded}]\n{footer}\n")
    }

    /// Envelope of a finished Codex session for `pr` whose report also holds `fields`.
    ///
    /// The report has a summary, one test, the status `completed` and the head of `pr`.
    pub(crate) fn codex_report(pr: &PullRequest, fields: Value) -> String {
        let mut report = serde_json::json!({
            "summary": "Full review of the pull request diff; details follow.",
            "tests": ["go test ./..."],
            "status": "completed",
            "reviewed_head_sha": pr.head_sha
        });
        if let (Some(map), Some(extra)) = (report.as_object_mut(), fields.as_object()) {
            for (key, value) in extra {
                map.insert(key.clone(), value.clone());
            }
        }
        let encoded = STANDARD.encode(report.to_string().as_bytes());
        format!(
            "[pohunek-codex-review-result-v1:{encoded}]\n\
[pohunek-codex-review exited with status 0]\n"
        )
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use crate::discussion::{DISCUSSION_ROLE_AUTHOR, DISCUSSION_ROLE_REVIEWER};
    use crate::error::error_code;
    use crate::model::review::{PRIOR_ANSWERED, PRIOR_FIXED, PRIOR_OPEN, prior_key};
    use crate::model::{
        Discussion, DiscussionComment, DiscussionThread, PriorFinding, PriorVerdict, ReviewInput,
        ReviewerResult,
    };
    use crate::protocol::combine::combine_prior;
    use crate::protocol::fixtures::{
        claude_output, claude_session, codex, codex_output, codex_report, opus, sha,
        test_pull_request,
    };
    use crate::protocol::validate::reviewer_output_retryable;
    use crate::protocol::validate::tests::test_config;

    use super::parse_reviewer_output;

    /// Verdict of `status` on `key` as Claude writes it, with the Go test note.
    fn verdict_json(key: &str, status: &str) -> Value {
        json!({"key": key, "status": status, "note": "Checked.", "relies_on": ""})
    }

    /// Verdict of `status` on `key` with the Go test note and no `relies_on`.
    fn verdict(key: &str, status: &str) -> PriorVerdict {
        PriorVerdict {
            key: key.to_owned(),
            status: status.to_owned(),
            note: "Checked.".to_owned(),
            relies_on: String::new(),
        }
    }

    #[test]
    fn parse_reviewer_output_normalizes_both_protocols() {
        let cfg = test_config();
        let pr = test_pull_request(3);
        let input = ReviewInput {
            pull_request: pr.clone(),
            ..ReviewInput::default()
        };
        let claude = parse_reviewer_output(
            &cfg,
            &input,
            &Discussion::default(),
            &opus(),
            &claude_output(
                &pr,
                json!([{
                    "id": "C001", "severity": "high", "confidence": "high", "path": "a.go",
                    "line": 4, "side": "RIGHT", "title": "Bug", "body": "Details."
                }]),
                None,
            ),
        )
        .expect("a Claude result parses");
        assert_eq!(claude.reviewer_id, "opus");
        assert_eq!(claude.findings.len(), 1);
        assert_eq!(claude.findings[0].source_id, "C001");

        let codex_result = parse_reviewer_output(
            &cfg,
            &input,
            &Discussion::default(),
            &codex(),
            &codex_output(
                &pr,
                json!([
                    {"severity": "low", "confidence": "low", "blocking": false, "path": "z.go",
                    "line": 1, "title": "Minor", "body": "Minor detail."},
                    {"severity": "blocker", "confidence": "high", "blocking": true,
                    "path": "a.go", "line": 2, "title": "Blocker", "body": "Breaks the build."}
                ]),
                0,
            ),
        )
        .expect("a Codex result parses");
        assert_eq!(codex_result.findings.len(), 2);
        assert_eq!(codex_result.findings[0].severity, "blocker");
        assert_eq!(codex_result.findings[0].source_id, "C001");
        assert_eq!(codex_result.findings[1].source_id, "C002");
        assert_eq!(codex_result.tests.len(), 1);

        let failed = parse_reviewer_output(
            &cfg,
            &input,
            &Discussion::default(),
            &codex(),
            &codex_output(&pr, json!([]), 1),
        )
        .expect_err("a failed Codex run is refused");
        assert!(reviewer_output_retryable(&failed));

        let mut other_pr = pr.clone();
        other_pr.head_sha = sha("e");
        let other = ReviewInput {
            pull_request: other_pr,
            ..ReviewInput::default()
        };
        let err = parse_reviewer_output(
            &cfg,
            &other,
            &Discussion::default(),
            &opus(),
            &claude_output(&pr, json!([]), None),
        )
        .expect_err("a result for another head gets a fresh run");
        assert_eq!(error_code(&err), "review_output_invalid");
        assert!(reviewer_output_retryable(&err));

        let err = parse_reviewer_output(
            &cfg,
            &input,
            &Discussion::default(),
            &opus(),
            &claude_output(
                &pr,
                json!([{
                    "id": "C001", "severity": "high", "confidence": "high",
                    "path": "../etc/passwd",
                    "line": 1, "side": "RIGHT", "title": "x", "body": "y"
                }]),
                None,
            ),
        )
        .expect_err("a traversing path gets a fresh run");
        assert_eq!(error_code(&err), "review_output_invalid");
        assert!(reviewer_output_retryable(&err));
    }

    /// Each row is named by the attack that its reviewer output models (#35).
    #[test]
    fn reviewer_output_for_another_pull_request_is_refused_and_no_decision_is_read() {
        let cfg = test_config();
        let pr = test_pull_request(3);
        let input = ReviewInput {
            pull_request: pr.clone(),
            ..ReviewInput::default()
        };
        let mut other_repository = pr.clone();
        other_repository.repository = "acme/other".to_owned();
        let mut other_number = pr.clone();
        other_number.number = 4;
        let mut other_head = pr.clone();
        other_head.head_sha = sha("e");
        let cases = [
            (
                "claude_result_for_another_repository",
                opus(),
                claude_output(&other_repository, json!([]), None),
                "review_output_invalid",
            ),
            (
                "claude_result_for_another_number",
                opus(),
                claude_output(&other_number, json!([]), None),
                "review_output_invalid",
            ),
            (
                "codex_result_for_another_head",
                codex(),
                codex_output(&other_head, json!([]), 0),
                "review_result_invalid",
            ),
            (
                "codex_result_asking_for_approval",
                codex(),
                codex_report(
                    &pr,
                    json!({"findings": [], "prior_findings": [], "decision": "APPROVE"}),
                ),
                "review_result_invalid",
            ),
        ];
        for (name, reviewer, raw, code) in cases {
            let err = parse_reviewer_output(&cfg, &input, &Discussion::default(), &reviewer, &raw)
                .expect_err(name);
            assert_eq!(error_code(&err), code, "{name}");
            assert!(reviewer_output_retryable(&err), "{name}");
        }

        // The Claude artifact has no decision: a decision the model adds is dropped unread.
        let asking = claude_session(&json!({
            "repository": pr.repository,
            "number": pr.number,
            "head_sha": pr.head_sha,
            "summary": "Reviewed the change thoroughly and found the listed issues.",
            "findings": [],
            "decision": "APPROVE",
            "approve": true
        }));
        let parse = |raw: &str| {
            parse_reviewer_output(&cfg, &input, &Discussion::default(), &opus(), raw)
                .expect("a Claude result parses")
        };
        assert_eq!(
            parse(&asking),
            parse(&claude_output(&pr, json!([]), None)),
            "claude_result_asking_for_approval"
        );
    }

    #[test]
    fn every_prior_finding_needs_exactly_one_verdict() {
        let cfg = test_config();
        let pr = test_pull_request(3);
        let prior = vec![
            PriorFinding {
                key: prior_key(&sha("c"), "O001"),
                ..PriorFinding::default()
            },
            PriorFinding {
                key: prior_key(&sha("c"), "S001"),
                ..PriorFinding::default()
            },
        ];
        let input = ReviewInput {
            pull_request: pr.clone(),
            prior: prior.clone(),
            ..ReviewInput::default()
        };
        let cases: [(&str, Option<Value>); 4] = [
            (
                "missing",
                Some(json!([verdict_json(&prior[0].key, PRIOR_FIXED)])),
            ),
            (
                "duplicate",
                Some(json!([
                    verdict_json(&prior[0].key, PRIOR_FIXED),
                    verdict_json(&prior[0].key, PRIOR_OPEN)
                ])),
            ),
            (
                "invented",
                Some(json!([
                    verdict_json(&prior[0].key, PRIOR_FIXED),
                    verdict_json("cccccccccccc:X001", PRIOR_FIXED)
                ])),
            ),
            ("absent", None),
        ];
        for (name, verdicts) in cases {
            let err = parse_reviewer_output(
                &cfg,
                &input,
                &Discussion::default(),
                &opus(),
                &claude_output(&pr, json!([]), verdicts),
            )
            .expect_err(name);
            assert_eq!(error_code(&err), "review_output_invalid", "{name}");
            assert!(
                reviewer_output_retryable(&err),
                "{name} must get a fresh run"
            );
        }
        let complete = claude_output(
            &pr,
            json!([]),
            Some(json!([
                verdict_json(&prior[1].key, PRIOR_ANSWERED),
                verdict_json(&prior[0].key, PRIOR_FIXED)
            ])),
        );
        let result =
            parse_reviewer_output(&cfg, &input, &Discussion::default(), &opus(), &complete)
                .expect("a complete verdict list parses");
        assert_eq!(result.prior.len(), 2);

        let combined = combine_prior(
            &prior,
            &[
                ReviewerResult {
                    prior: vec![
                        verdict(&prior[0].key, PRIOR_FIXED),
                        verdict(&prior[1].key, PRIOR_ANSWERED),
                    ],
                    ..ReviewerResult::default()
                },
                ReviewerResult {
                    prior: vec![
                        verdict(&prior[0].key, PRIOR_OPEN),
                        verdict(&prior[1].key, PRIOR_FIXED),
                    ],
                    ..ReviewerResult::default()
                },
            ],
        );
        assert_eq!(combined[0].verdict.status, PRIOR_OPEN);
        assert_eq!(combined[1].verdict.status, PRIOR_ANSWERED);
    }

    #[test]
    fn relies_on_must_name_a_commenter_of_the_finding() {
        let cfg = test_config();
        let pr = test_pull_request(3);
        let key = prior_key(&sha("c"), "O001");
        let input = ReviewInput {
            pull_request: pr.clone(),
            prior: vec![PriorFinding {
                key: key.clone(),
                ..PriorFinding::default()
            }],
            ..ReviewInput::default()
        };
        let discussion = Discussion {
            threads: vec![DiscussionThread {
                key: key.clone(),
                resolved: false,
                replies: vec![DiscussionComment {
                    author: "colleague".to_owned(),
                    role: DISCUSSION_ROLE_AUTHOR.to_owned(),
                    ..DiscussionComment::default()
                }],
            }],
            comments: vec![DiscussionComment {
                author: "reviewer".to_owned(),
                role: DISCUSSION_ROLE_REVIEWER.to_owned(),
                ..DiscussionComment::default()
            }],
        };
        let output = |login: &str| {
            claude_output(
                &pr,
                json!([]),
                Some(json!([{
                    "key": key, "status": PRIOR_ANSWERED, "note": "Settled.",
                    "relies_on": login
                }])),
            )
        };
        for login in ["colleague", "reviewer", ""] {
            parse_reviewer_output(&cfg, &input, &discussion, &opus(), &output(login))
                .expect("a relies_on that names a commenter, or nobody, parses");
        }
        let err = parse_reviewer_output(&cfg, &input, &discussion, &opus(), &output("stranger"))
            .expect_err("a relies_on that names nobody in the discussion is refused");
        assert_eq!(error_code(&err), "review_output_invalid");
    }

    // A Codex re-review settles the prior findings as a Claude one does: each needs one verdict,
    // and the verdicts decide the combined outcome (#23).
    #[test]
    fn codex_verdicts_settle_the_prior_findings() {
        let cfg = test_config();
        let pr = test_pull_request(3);
        let prior = vec![
            PriorFinding {
                key: prior_key(&sha("c"), "X001"),
                ..PriorFinding::default()
            },
            PriorFinding {
                key: prior_key(&sha("c"), "X002"),
                ..PriorFinding::default()
            },
        ];
        let input = ReviewInput {
            pull_request: pr.clone(),
            prior: prior.clone(),
            ..ReviewInput::default()
        };
        let output = |verdicts: Value| {
            codex_report(&pr, json!({"findings": [], "prior_findings": verdicts}))
        };
        let settling = output(json!([
            verdict_json(&prior[0].key, PRIOR_FIXED),
            verdict_json(&prior[1].key, PRIOR_OPEN)
        ]));
        let result =
            parse_reviewer_output(&cfg, &input, &Discussion::default(), &codex(), &settling)
                .expect("a Codex result that settles every prior finding parses");
        let settled: Vec<String> = combine_prior(&prior, &[result])
            .into_iter()
            .map(|settled| settled.verdict.status)
            .collect();
        assert_eq!(settled, [PRIOR_FIXED, PRIOR_OPEN]);
        let partial = output(json!([verdict_json(&prior[0].key, PRIOR_FIXED)]));
        let err = parse_reviewer_output(&cfg, &input, &Discussion::default(), &codex(), &partial)
            .expect_err("a Codex result that leaves a prior finding unsettled is refused");
        assert_eq!(error_code(&err), "review_output_invalid");
    }
}
