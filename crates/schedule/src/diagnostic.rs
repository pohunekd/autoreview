//! Allowlisted diagnostic codes and the published explanations of failure codes.

// Rust guideline compliant 2026-06-26

use std::error::Error as StdError;

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

/// Capital I with dot above (U+0130), which Go's `strings.ToLower` maps to a plain `i`.
const CAPITAL_I_WITH_DOT: char = '\u{130}';

/// Longest pohunek code that [`SessionLoss`] keeps, in bytes.
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
    let mut current: Option<&'a (dyn StdError + 'static)> = Some(err);
    while let Some(error) = current {
        if let Some(loss) = error.downcast_ref::<SessionLoss>() {
            return loss.code();
        }
        current = error.source();
    }
    classify(&err.to_string())
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

/// Lowercases `text` as Go's `strings.ToLower` does for every ASCII substring match.
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
        "github_request_failed" => {
            "A GitHub API request failed. Check GitHub availability, authentication, and rate limits."
        }
        _ => {
            "The review could not complete. Inspect the review service logs using the diagnostic code below."
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{failure_explanation, fetch_failure};
    use crate::scheduler::retryable;

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
    }
}
