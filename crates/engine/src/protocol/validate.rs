//! Validation of reviewer output: bounded text, the findings of a result and the verdicts on
//! prior findings. A refusal here sends the reviewer to a fresh run, so this module refuses
//! everything that the Go reference refuses.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeSet;

use crate::config::Config;
use crate::error::{Error, error_code};
use crate::model::review::{PRIOR_ANSWERED, PRIOR_FIXED, PRIOR_OPEN};
use crate::model::{Discussion, Finding, PriorFinding, PriorVerdict, ReviewerResult};

/// Severities that a finding may carry, most severe first.
const SEVERITIES: [&str; 5] = ["blocker", "critical", "high", "medium", "low"];

/// Confidence levels that a finding may carry.
const CONFIDENCE_LEVELS: [&str; 3] = ["high", "medium", "low"];

/// Codes of the output failures that a fresh reviewer run may fix.
const RETRYABLE_CODES: [&str; 4] = [
    "review_output_incomplete",
    "review_output_invalid",
    "review_wrapper_invalid",
    "review_result_invalid",
];

/// Sorts findings by severity, then path, then line. Equal findings keep their order.
///
/// A severity outside `SEVERITIES` ranks first, as the zero value of Go's map lookup does.
/// Validation refuses such a finding afterwards.
#[expect(dead_code, reason = "called by S15")]
pub(crate) fn sort_findings(findings: &mut [Finding]) {
    findings.sort_by(|left, right| {
        severity_rank(&left.severity)
            .cmp(&severity_rank(&right.severity))
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.line.cmp(&right.line))
    });
}

/// Returns the position of `severity` in `SEVERITIES`, or 0 when it is not listed.
fn severity_rank(severity: &str) -> usize {
    SEVERITIES
        .iter()
        .position(|candidate| *candidate == severity)
        .unwrap_or(0)
}

/// Reports whether `value` is non-blank text of at most `maximum` characters.
///
/// NUL is refused. Line breaks are allowed only when `multiline` is set. A Rust string is
/// valid UTF-8 already, so no UTF-8 check is repeated here.
#[cfg_attr(not(test), expect(dead_code, reason = "called by S15"))]
pub(crate) fn bounded_text(value: &str, maximum: i64, multiline: bool) -> bool {
    if value.trim().is_empty() {
        return false;
    }
    if count_over(value.chars().count(), maximum) {
        return false;
    }
    if value.contains('\0') {
        return false;
    }
    if !multiline && value.contains(['\r', '\n']) {
        return false;
    }
    true
}

/// Counts the characters of `value`, as Go's `utf8.RuneCountInString` does.
///
/// The count saturates at `i64::MAX`, which no string reaches.
fn char_count(value: &str) -> i64 {
    i64::try_from(value.chars().count()).unwrap_or(i64::MAX)
}

/// Reports whether `len` items exceed `maximum`.
fn count_over(len: usize, maximum: i64) -> bool {
    match i64::try_from(len) {
        Ok(count) => count > maximum,
        Err(_) => true,
    }
}

/// Reports whether `path` is already in the form that Go's `path.Clean` returns, and is relative.
///
/// The current directory `.` is its own cleaned form and passes. Any other empty, `.` or `..`
/// component is refused, so `../x`, `a/../b` and `./a` fail.
fn clean_relative_path(path: &str) -> bool {
    path == "."
        || (!path.is_empty()
            && !path.starts_with('/')
            && path
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != ".."))
}

/// Checks a reviewer result against the configured bounds.
///
/// # Errors
///
/// Returns `artifact_invalid` when the summary, the count of findings or tests, a finding, a
/// source ID or a test is out of bounds.
#[expect(dead_code, reason = "called by S15")]
pub(crate) fn validate_reviewer_result(cfg: &Config, value: &ReviewerResult) -> Result<(), Error> {
    if char_count(value.summary.trim()) < cfg.min_summary_characters
        || !bounded_text(&value.summary, cfg.max_summary_characters, true)
        || count_over(value.findings.len(), cfg.max_findings)
        || count_over(value.tests.len(), cfg.max_tests)
    {
        return Err(artifact_invalid());
    }
    let mut seen = BTreeSet::new();
    for finding in &value.findings {
        if seen.contains(&finding.source_id) || !finding_valid(cfg, finding) {
            return Err(artifact_invalid());
        }
        seen.insert(finding.source_id.clone());
    }
    for test in &value.tests {
        if !bounded_text(test, cfg.max_test_characters, false) {
            return Err(artifact_invalid());
        }
    }
    Ok(())
}

/// Reports whether one finding of a reviewer result is well formed.
fn finding_valid(cfg: &Config, finding: &Finding) -> bool {
    clean_relative_path(&finding.path)
        && finding.line > 0
        && bounded_text(&finding.path, cfg.max_path_characters, false)
        && bounded_text(&finding.title, cfg.max_title_characters, false)
        && bounded_text(&finding.body, cfg.max_body_characters, true)
        && CONFIDENCE_LEVELS.contains(&finding.confidence.as_str())
        && SEVERITIES.contains(&finding.severity.as_str())
}

/// Checks the verdicts on the prior findings.
///
/// Each prior finding gets exactly one verdict with a known key, a known status and a
/// non-blank note. A `relies_on` login must have commented on that finding's thread or on
/// the pull request.
///
/// # Errors
///
/// Returns `review_output_invalid` when a verdict breaks one of these rules, or when the
/// number of verdicts differs from the number of prior findings.
#[cfg_attr(not(test), expect(dead_code, reason = "called by S15"))]
pub(crate) fn validate_prior_verdicts(
    cfg: &Config,
    prior: &[PriorFinding],
    discussion: &Discussion,
    verdicts: &[PriorVerdict],
) -> Result<(), Error> {
    if verdicts.len() != prior.len() {
        return Err(output_invalid());
    }
    let mut expected = BTreeSet::new();
    for finding in prior {
        expected.insert(finding.key.clone());
    }
    for verdict in verdicts {
        if !expected.contains(&verdict.key)
            || !bounded_text(&verdict.note, cfg.max_body_characters, true)
            || (!verdict.relies_on.is_empty()
                && discussion
                    .comments_by(&verdict.key, &verdict.relies_on)
                    .is_empty())
        {
            return Err(output_invalid());
        }
        match verdict.status.as_str() {
            PRIOR_FIXED | PRIOR_OPEN | PRIOR_ANSWERED => {}
            _ => return Err(output_invalid()),
        }
        expected.remove(&verdict.key);
    }
    Ok(())
}

/// Reports whether a fresh reviewer run may fix the output that `err` describes.
#[cfg_attr(not(test), expect(dead_code, reason = "called by S26"))]
pub(crate) fn reviewer_output_retryable(err: &Error) -> bool {
    RETRYABLE_CODES.contains(&error_code(err))
}

/// The error of a reviewer result that the engine refuses. It is permanent.
fn artifact_invalid() -> Error {
    Error::workflow("artifact_invalid", true, None)
}

/// The error of reviewer verdicts that the engine refuses. It is permanent.
fn output_invalid() -> Error {
    Error::workflow("review_output_invalid", true, None)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use autoreview_schedule::scheduler;

    use crate::config::{Config, MatrixRooms};
    use crate::discussion::{DISCUSSION_ROLE_AUTHOR, DISCUSSION_ROLE_REVIEWER};
    use crate::error::error_code;
    use crate::model::review::{PRIOR_ANSWERED, PRIOR_FIXED, PRIOR_OPEN, prior_key};
    use crate::model::{
        Discussion, DiscussionComment, DiscussionThread, PriorFinding, PriorVerdict,
    };
    use crate::protocol::fixtures::sha;

    use super::{reviewer_output_retryable, validate_prior_verdicts};

    /// A configuration that passes validation; every value is synthetic.
    fn valid_config() -> Config {
        Config {
            policy: None,
            schedule: scheduler::Policy {
                debounce: time::Duration::seconds(120),
                retry_delays: vec![time::Duration::seconds(60)],
            },
            github_host: "github.com".to_owned(),
            github_login: "example-user".to_owned(),
            github_token: "placeholder-token".to_owned(),
            gh_bin: "/usr/bin/gh".to_owned(),
            pohunek_bin: "/usr/bin/pohunek".to_owned(),
            pohunek_state: "/var/lib/pohunek".to_owned(),
            pohunek_runtime: "/run/pohunek".to_owned(),
            pohunek_hosts: vec!["example-host".to_owned()],
            route_overrides: std::collections::BTreeMap::new(),
            matrix_homeserver_url: "https://matrix.example.org".to_owned(),
            matrix_access_token: "placeholder-token".to_owned(),
            matrix_mention_user: "@example-user:example.org".to_owned(),
            matrix_rooms: MatrixRooms {
                action: "!action:example.org".to_owned(),
                digest: "!digest:example.org".to_owned(),
                log: "!log:example.org".to_owned(),
            },
            queue_matrix_room: "!queue:example.org".to_owned(),
            queue_max_items: 10,
            state_db_path: "/var/lib/autoreview/state.db".to_owned(),
            gh_timeout: Duration::from_secs(30),
            pohunek_timeout: Duration::from_secs(60),
            matrix_timeout: Duration::from_secs(30),
            session_timeout: Duration::from_secs(30 * 60),
            output_poll_interval: Duration::from_secs(15),
            session_start_retry: Duration::from_secs(30),
            session_cleanup_retry: Duration::from_secs(30),
            reviewer_run_retry: Duration::from_secs(30),
            step_retry: Duration::from_secs(10 * 60),
            step_retry_initial: Duration::from_secs(30),
            step_retry_max_interval: Duration::from_secs(5 * 60),
            poll_interval: Duration::from_secs(5 * 60),
            worker_tick: Duration::from_secs(30),
            discovery_lock_timeout: Duration::from_secs(60),
            discovery_lock_poll: Duration::from_secs(1),
            review_retention: Duration::from_secs(30 * 24 * 60 * 60),
            session_start_attempts: 3,
            session_cleanup_attempts: 3,
            reviewer_run_attempts: 2,
            max_concurrent_reviews: 4,
            review_list_limit: 20,
            poll_limit: 100,
            search_page_limit: 100,
            state_entry_limit: 1000,
            schedule_max_bytes: 1 << 20,
            output_max_bytes: 1 << 20,
            output_chunk_bytes: 64 << 10,
            output_wait_ms: 1000,
            result_max_bytes: 512 << 10,
            max_findings: 20,
            max_tests: 20,
            min_summary_characters: 10,
            max_summary_characters: 1000,
            max_title_characters: 256,
            max_body_characters: 4000,
            max_discussion_characters: 8000,
            max_path_characters: 512,
            max_test_characters: 256,
            github_body_max_bytes: 65_536,
            ai_signature: "Written by AI: example-agent".to_owned(),
            matrix_max_characters: 4000,
            published_retention: Duration::from_secs(7 * 24 * 60 * 60),
            excerpt_characters: 1000,
            terminal_cols: 120,
            terminal_rows: 100,
            session_group: "autoreview".to_owned(),
        }
    }

    fn test_config() -> Config {
        let mut cfg = valid_config();
        cfg.github_host = "github.com".to_owned();
        cfg.github_login = "reviewer".to_owned();
        cfg.github_body_max_bytes = 60_000;
        cfg.ai_signature = "🤖 Written by AI: test reviewer".to_owned();
        cfg.max_findings = 50;
        cfg.max_tests = 20;
        cfg.min_summary_characters = 10;
        cfg.max_summary_characters = 20_000;
        cfg.max_title_characters = 512;
        cfg.max_body_characters = 20_000;
        cfg.max_discussion_characters = 40_000;
        cfg.max_path_characters = 4_096;
        cfg.max_test_characters = 1_024;
        cfg.result_max_bytes = 1 << 20;
        cfg.excerpt_characters = 200;
        cfg.matrix_max_characters = 8_000;
        cfg.session_group = "autoreview".to_owned();
        cfg
    }

    /// A prior finding with `key` and nothing else set.
    fn prior_finding(key: String) -> PriorFinding {
        PriorFinding {
            key,
            ..PriorFinding::default()
        }
    }

    /// A verdict on `key` with `status`, the Go test note and no `relies_on`.
    fn verdict(key: &str, status: &str) -> PriorVerdict {
        PriorVerdict {
            key: key.to_owned(),
            status: status.to_owned(),
            note: "Checked.".to_owned(),
            relies_on: String::new(),
        }
    }

    #[test]
    fn every_prior_finding_needs_exactly_one_verdict() {
        let cfg = test_config();
        let prior = vec![
            prior_finding(prior_key(&sha("c"), "O001")),
            prior_finding(prior_key(&sha("c"), "S001")),
        ];
        let cases = [
            ("missing", vec![verdict(&prior[0].key, PRIOR_FIXED)]),
            (
                "duplicate",
                vec![
                    verdict(&prior[0].key, PRIOR_FIXED),
                    verdict(&prior[0].key, PRIOR_OPEN),
                ],
            ),
            (
                "invented",
                vec![
                    verdict(&prior[0].key, PRIOR_FIXED),
                    verdict("cccccccccccc:X001", PRIOR_FIXED),
                ],
            ),
            ("absent", Vec::new()),
        ];
        for (name, verdicts) in cases {
            let err = validate_prior_verdicts(&cfg, &prior, &Discussion::default(), &verdicts)
                .expect_err(name);
            assert_eq!(error_code(&err), "review_output_invalid", "{name}");
            assert!(
                reviewer_output_retryable(&err),
                "{name} must get a fresh run"
            );
        }
        let verdicts = [
            verdict(&prior[1].key, PRIOR_ANSWERED),
            verdict(&prior[0].key, PRIOR_FIXED),
        ];
        assert!(validate_prior_verdicts(&cfg, &prior, &Discussion::default(), &verdicts).is_ok());
    }

    #[test]
    fn relies_on_must_name_a_commenter_of_the_finding() {
        let cfg = test_config();
        let key = prior_key(&sha("c"), "O001");
        let prior = vec![prior_finding(key.clone())];
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
        let answered = |login: &str| PriorVerdict {
            key: key.clone(),
            status: PRIOR_ANSWERED.to_owned(),
            note: "Settled.".to_owned(),
            relies_on: login.to_owned(),
        };
        for login in ["colleague", "reviewer", ""] {
            let verdicts = [answered(login)];
            assert!(
                validate_prior_verdicts(&cfg, &prior, &discussion, &verdicts).is_ok(),
                "relies_on {login:?} must be accepted"
            );
        }
        let verdicts = [answered("stranger")];
        let err = validate_prior_verdicts(&cfg, &prior, &discussion, &verdicts)
            .expect_err("a relies_on that names nobody must be refused");
        assert_eq!(error_code(&err), "review_output_invalid");
    }
}
