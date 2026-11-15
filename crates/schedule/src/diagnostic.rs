//! Allowlisted diagnostic codes and the published explanations of failure codes.

// Rust guideline compliant 2026-06-26

use std::error::Error as StdError;

use crate::scheduler::{AUTHOR_LIMIT_REACHED, HEAD_FETCH_MISMATCH, REVIEWER_USAGE_LIMITED};

/// Lowercase substrings of an error message and the diagnostic code each one maps to.
///
/// The first match wins, so `permission denied (publickey)` precedes `permission denied`.
const DIAGNOSTIC_CODES: [(&str, &str); 16] = [
    ("no space left on device", "disk_full"),
    ("cannot lock ref", "ref_locked"),
    ("authentication failed", "authentication_failed"),
    ("permission denied (publickey)", "authentication_failed"),
    ("could not read username", "authentication_failed"),
    ("couldn't find remote ref", "remote_ref_missing"),
    ("could not resolve host", "dns_failed"),
    ("could not resolve hostname", "dns_failed"),
    ("permission denied", "permission_denied"),
    ("already exists", "resource_exists"),
    ("deadline exceeded", "timeout"),
    ("request_timeout", "timeout"),
    ("timed out", "timeout"),
    ("connection refused", "connection_refused"),
    ("host_unavailable", "host_unavailable"),
    ("agent_not_found", "agent_not_found"),
];

/// Capital I with dot above (U+0130), which is replaced by a plain `i` before lowercasing.
const CAPITAL_I_WITH_DOT: char = '\u{130}';

/// Longest pohunek code that [`SessionLoss`] and [`start_diagnostic`] keep, in bytes.
const LOSS_CODE_MAX_BYTES: usize = 64;

/// Why pohunek reports a reviewer session's runtime as lost, as a checked pohunek code.
///
/// It is the diagnostic of `pohunek_session_lost`; only a code of lowercase letters, digits
/// and underscores is kept, so no raw daemon text reaches a log.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("session runtime lost: {code}")]
pub struct SessionLoss {
    /// The loss cause or loss reason that pohunek reported.
    code: String,
}

impl SessionLoss {
    /// Returns the loss of `cause`, else `reason`, when one is a pohunek code.
    ///
    /// `None` when neither matches `^[a-z][a-z0-9_]{0,63}$`.
    #[must_use]
    pub fn new(cause: &str, reason: &str) -> Option<Self> {
        [cause, reason]
            .into_iter()
            .find(|text| is_loss_code(text))
            .map(|text| Self {
                code: text.to_owned(),
            })
    }

    /// Returns the pohunek code of the loss.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }
}

/// Reports whether `text` matches `^[a-z][a-z0-9_]{0,63}$`, the form of a pohunek code.
fn is_loss_code(text: &str) -> bool {
    let bytes = text.as_bytes();
    let Some((first, rest)) = bytes.split_first() else {
        return false;
    };
    bytes.len() <= LOSS_CODE_MAX_BYTES
        && first.is_ascii_lowercase()
        && rest
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
}

/// Classifies an error into an allowlisted diagnostic code that is safe to log.
///
/// Raw CLI errors can contain tokens, prompt contents and authenticated URLs, so
/// only the code leaves this function. `None` yields `unavailable`. A [`SessionLoss`] in the
/// source chain yields its checked pohunek code.
pub fn diagnostic<'a>(err: Option<&'a (dyn StdError + Send + Sync + 'static)>) -> &'a str {
    let Some(err) = err else {
        return "unavailable";
    };
    if let Some(code) = session_loss(err) {
        return code;
    }
    classify(&err.to_string())
}

/// Classifies a failed session start into a diagnostic code that is safe to log.
///
/// A [`SessionLoss`] in the source chain wins, then `code`, the error code pohunek reported,
/// when it matches `^[a-z][a-z0-9_]{0,63}$`; otherwise the result of [`diagnostic`] for `err`.
pub fn start_diagnostic<'a>(
    code: Option<&'a str>,
    err: Option<&'a (dyn StdError + Send + Sync + 'static)>,
) -> &'a str {
    if let Some(loss) = err.and_then(|err| session_loss(err)) {
        return loss;
    }
    match code {
        Some(code) if is_loss_code(code) => code,
        _ => diagnostic(err),
    }
}

/// Returns the code of the first [`SessionLoss`] in the source chain of `err`.
fn session_loss<'a>(err: &'a (dyn StdError + 'static)) -> Option<&'a str> {
    std::iter::successors(Some(err), |&error| error.source())
        .find_map(|error| error.downcast_ref::<SessionLoss>())
        .map(SessionLoss::code)
}

/// Maps an error message to the code of its first matching entry in `DIAGNOSTIC_CODES`.
fn classify(message: &str) -> &'static str {
    let lower = go_lowercase(message);
    for (needle, code) in DIAGNOSTIC_CODES {
        if lower.contains(needle) {
            return code;
        }
    }
    "unclassified_cli_failure"
}

/// Lowercases `text` for every ASCII substring match.
///
/// Rust maps U+0130 to `i` plus a combining dot, so it is replaced by `i` first.
fn go_lowercase(text: &str) -> String {
    text.replace(CAPITAL_I_WITH_DOT, "i").to_lowercase()
}

/// Classifies untrusted fetch warning text before it enters durable state.
///
/// Returns the failure code and whether the failure is permanent.
pub fn fetch_failure(detail: &str) -> (&'static str, bool) {
    let code = classify(detail);
    match code {
        "timeout" => ("pohunek_base_fetch_timeout", false),
        "authentication_failed" => ("pohunek_base_fetch_authentication_failed", true),
        "permission_denied" => ("pohunek_base_fetch_permission_denied", true),
        "remote_ref_missing" => ("pohunek_base_fetch_ref_missing", true),
        "disk_full" => ("pohunek_base_fetch_disk_full", true),
        "dns_failed" => ("pohunek_base_fetch_dns_failed", false),
        "ref_locked" => ("pohunek_base_fetch_ref_locked", false),
        _ => ("pohunek_base_fetch_failed", false),
    }
}

/// Classifies the error code of a `session new` that failed its pull request head fetch.
///
/// Returns the failure code and whether the failure is permanent, or `None` for any other
/// code. A fetched head that is not the trusted one is `pohunek_head_fetch_mismatch`, which the
/// scheduler starts again until the retry budget is spent.
pub fn head_fetch_failure(code: &str) -> Option<(&'static str, bool)> {
    match code {
        "fetch_ref_not_found" => Some(("pohunek_head_fetch_ref_missing", true)),
        "fetch_ref_failed" => Some(("pohunek_head_fetch_failed", false)),
        "fetch_ref_commit_mismatch" => Some((HEAD_FETCH_MISMATCH, true)),
        _ => None,
    }
}

/// Returns the published explanation of a failure code.
///
/// The text never interpolates raw CLI output or credentials.
pub fn failure_explanation(code: &str) -> &'static str {
    match code {
        "pohunek_base_fetch_timeout" => {
            "Git timed out while fetching the review base branch on the review host. Check GitHub connectivity, whether the SSH agent hangs on signing, and whether the Git credential helper is waiting for a locked keyring."
        }
        "pohunek_base_fetch_authentication_failed" => {
            "Git could not authenticate to fetch the review base branch. Restore GitHub credentials or SSH access on the review host, and check that its SSH agent is unlocked and signs with the key; retrying cannot repair credentials."
        }
        "pohunek_base_fetch_permission_denied" => {
            "Git was denied access while fetching the review base branch. Correct repository file permissions on the review host before retrying."
        }
        "pohunek_base_fetch_ref_missing" => {
            "The review base branch was not found on the Git remote. Verify the PR base branch and the review host repository remote before retrying."
        }
        "pohunek_base_fetch_disk_full" => {
            "The review host ran out of disk space while fetching the review base branch. Free disk space before retrying."
        }
        "pohunek_base_fetch_dns_failed" => {
            "The review host could not resolve the Git remote hostname. Check its DNS and network connection."
        }
        "pohunek_base_fetch_ref_locked" => {
            "Another Git fetch in the same repository on the review host held a ref lock while the review base branch was fetched. The lock is released when that fetch ends, so the next attempt fetches again."
        }
        "pohunek_head_fetch_ref_missing" => {
            "The review host's project repository has no `origin`, or `origin` has no `refs/pull/<n>/head` for this pull request, so the session could not fetch its head. Check the project repository's remote on the review host, then retry this PR."
        }
        "pohunek_head_fetch_failed" => {
            "Git could not fetch the pull request head on the review host: a network, authentication or timeout failure, or Git older than 2.29 there. Check GitHub access and the Git version on the review host."
        }
        HEAD_FETCH_MISMATCH => {
            "At each of the latest starts of the review that a usage limit did not end, `refs/pull/<n>/head` fetched on the review host was at another commit than the head GitHub reports for the pull request, so GitHub did not update the ref. Check the pull request on GitHub, then retry this PR or push a new commit."
        }
        "pohunek_session_warning" => {
            "The reviewer session reported a warning other than a failed base fetch, for example an existing branch checked out instead of the review base, or a failed setup script or hook. The session was removed; check the project's setup scripts and hooks and stale branches under `session.branch_prefix` on the review host before retrying."
        }
        "pohunek_base_fetch_failed" => {
            "Git could not fetch the review base branch on the review host. Check the Pohunek session fetch warning and GitHub access; the retained error does not identify a more specific cause."
        }
        "pohunek_session_lost" => {
            "The review host lost the reviewer session's worker while the session started. Check the review host's resources (inotify watches, memory) and the Pohunek daemon journal; the next attempt starts a fresh session."
        }
        "pohunek_session_start_failed" => {
            "Pohunek could not start the reviewer process. Check the review host, agent configuration, and service logs."
        }
        "pohunek_request_failed" => {
            "The request to the Pohunek review host failed. Check that the host and its daemon are reachable."
        }
        "pohunek_request_timeout" => {
            "The Pohunek review host did not answer within the CLI request timeout. The host was reachable; check its network latency and the size of its session list."
        }
        "review_head_unavailable" => {
            "The pull request head commit is not in the review host's repository, so the reviewer on a base checkout could not read the change and its adapter stopped the run. Pohunek fetches the head at session start only when the pohunek CLI and the review host's daemon are 0.43.0 or later. On 0.43.0 or later, the service log shows `head fetch capability unknown, starting without the fetch` when autoreview could not read that capability; retry this PR. With an older pohunek, upgrade it, or on the review host run `git -C <project repository> fetch origin refs/pull/<n>/head`, then retry this PR."
        }
        "github_request_failed" => {
            "A GitHub API request failed. Check GitHub availability, authentication, and rate limits."
        }
        AUTHOR_LIMIT_REACHED => {
            "The pull request author started as many reviews under this rule as its \
             `author_limit` allows in its window, and the rule skips reviews over the bound, so \
             this head is not reviewed. A later commit is reviewed if it comes after the window \
             frees a slot; retry this PR to review this head now."
        }
        REVIEWER_USAGE_LIMITED => {
            "The reviewer's model account reached its rate or usage limit: Claude Code ended the \
             run with HTTP status 429. The review is tried again on the delays of \
             `schedule.usage_limit_retry_delays_seconds`, which use none of the other retries; \
             when they are spent, retry this PR after the limit resets."
        }
        "credential_invalid" => {
            "GitHub refused the token of the review's credential: revoked, expired or never valid. Replace the token in the variable that its `token_env` names and restart autoreview, then retry this PR; it is not retried automatically."
        }
        _ => {
            "The review could not complete. Inspect the review service logs using the diagnostic code below."
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{failure_explanation, fetch_failure, head_fetch_failure};
    use crate::scheduler::{retryable, superseded};

    #[test]
    fn fetch_failures_classify_without_retaining_secrets() {
        let cases = [
            (
                "git command timed out after 30s",
                "pohunek_base_fetch_timeout",
                false,
            ),
            (
                "Authentication failed for https://token:secret@github.com/private",
                "pohunek_base_fetch_authentication_failed",
                true,
            ),
            (
                "git@ssh.github.com: Permission denied (publickey).",
                "pohunek_base_fetch_authentication_failed",
                true,
            ),
            (
                "fatal: couldn't find remote ref deleted",
                "pohunek_base_fetch_ref_missing",
                true,
            ),
            (
                "No space left on device",
                "pohunek_base_fetch_disk_full",
                true,
            ),
            (
                "cannot open FETCH_HEAD: Permission denied",
                "pohunek_base_fetch_permission_denied",
                true,
            ),
            (
                "Could not resolve host: github.com",
                "pohunek_base_fetch_dns_failed",
                false,
            ),
            (
                "error: cannot lock ref 'refs/remotes/origin/main': is at 49a1b61 but expected 9ff9c5d",
                "pohunek_base_fetch_ref_locked",
                false,
            ),
            (
                "token=secret prompt=private",
                "pohunek_base_fetch_failed",
                false,
            ),
            ("", "pohunek_base_fetch_failed", false),
        ];
        for (detail, want_code, want_permanent) in cases {
            let (code, permanent) = fetch_failure(detail);
            assert!(
                code == want_code && permanent == want_permanent,
                "{detail:?}: classification = {code}, {permanent}"
            );
            assert!(
                retryable(code) != permanent,
                "incorrect scheduling retry policy for {code}"
            );
            let explanation = failure_explanation(code);
            assert!(
                !explanation.contains("secret") && !explanation.contains("private"),
                "raw diagnostic leaked"
            );
        }
        // A failed head fetch of `session new` is classified by the pohunek error code (#255).
        let generic = failure_explanation("");
        let head_cases = [
            (
                "fetch_ref_not_found",
                Some(("pohunek_head_fetch_ref_missing", true)),
            ),
            (
                "fetch_ref_failed",
                Some(("pohunek_head_fetch_failed", false)),
            ),
            (
                "fetch_ref_commit_mismatch",
                Some(("pohunek_head_fetch_mismatch", true)),
            ),
            ("fetch_ref_outcome_missing", None),
            ("session_new_fetch_ref_unsupported", None),
            ("branch_fetch_failed", None),
            ("", None),
        ];
        for (pohunek_code, want) in head_cases {
            let got = head_fetch_failure(pohunek_code);
            assert_eq!(got, want, "{pohunek_code:?}");
            let Some((code, permanent)) = got else {
                continue;
            };
            assert!(
                retryable(code) != permanent,
                "incorrect scheduling retry policy for {code}"
            );
            assert!(
                superseded(code) || failure_explanation(code) != generic,
                "{code} has no explanation"
            );
        }
    }
}
