//! The `session new --meta` arguments that join a review session to its pull request.

// Rust guideline compliant 2026-06-26

use crate::Error;

/// Length in bytes of a full hexadecimal commit SHA.
const HEAD_SHA_LEN: usize = 40;

/// Longest head branch, in bytes, that is passed as `work.link.branch`.
const MAX_BRANCH_LEN: usize = 255;

/// Longest companion group name, in bytes, that pohunek-companion accepts by default.
const MAX_GROUP_LEN: usize = 64;

/// Characters that a pull request URL must not contain.
const URL_FORBIDDEN_CHARS: [char; 5] = [' ', '\t', '\r', '\n', '\0'];

/// Identifies the pull request that a review session belongs to.
///
/// The keys follow the pohunek-work launch contract, so its list joins the session
/// to the pull request row (`work.link.id` is `owner/name#number`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkSession {
    /// The `owner/name` repository of the pull request.
    pub repository: String,
    /// The pull request number.
    pub number: i64,
    /// The GitHub URL of the pull request.
    pub url: String,
    /// The head branch of the pull request.
    pub head_branch: String,
    /// The head commit SHA of the pull request.
    pub head_sha: String,
}

/// Reports whether pohunek-companion shows `group` as a sidebar group name.
///
/// Accepts 1 to 64 ASCII letters, digits, spaces, `.`, `_` and `-`, starting with a
/// letter or digit and not ending with a space. Control and format characters fail.
pub fn is_valid_session_group(group: &str) -> bool {
    let bytes = group.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_GROUP_LEN {
        return false;
    }
    let Some((first, rest)) = bytes.split_first() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    if rest.is_empty() {
        return true;
    }
    let Some((last, _)) = bytes.split_last() else {
        return false;
    };
    if !is_group_byte(*last) {
        return false;
    }
    // Check every byte between first and last.
    let Some((_, without_first)) = bytes.split_first() else {
        return false;
    };
    let Some((_, middle)) = without_first.split_last() else {
        // Length 2 case: no middle bytes.
        return true;
    };
    for byte in middle {
        if !is_group_byte(*byte) && *byte != b' ' {
            return false;
        }
    }
    true
}

/// Returns the `session new --meta` arguments for `session` in a fixed order.
///
/// A head branch that is not a plain ref name is omitted; the link id alone joins the
/// row. The group is written to `companion.group`.
///
/// # Errors
///
/// Returns `Error::InvalidSessionGroup` when `is_valid_session_group` rejects
/// `group`, checked first, and `Error::InvalidSessionIdentity` when the repository,
/// number, head SHA or URL of `session` is malformed.
pub fn metadata_args(session: &WorkSession, group: &str) -> Result<Vec<String>, Error> {
    if !is_valid_session_group(group) {
        return Err(Error::InvalidSessionGroup);
    }
    if !is_metadata_repository(&session.repository)
        || session.number <= 0
        || !is_head_sha(&session.head_sha)
        || !session
            .url
            .starts_with(&format!("https://github.com/{}/pull/", session.repository))
        || session
            .url
            .chars()
            .any(|c| URL_FORBIDDEN_CHARS.contains(&c))
    {
        return Err(Error::InvalidSessionIdentity);
    }
    let mut args = Vec::new();
    let mut push = |key: &str, value: &str| {
        args.push("--meta".to_owned());
        args.push(format!("{key}={value}"));
    };
    push("work.link.provider", "github");
    push("work.link.kind", "pull_request");
    push(
        "work.link.id",
        &format!("{}#{}", session.repository, session.number),
    );
    push("work.link.url", &session.url);
    if is_metadata_branch(&session.head_branch) && !session.head_branch.contains("..") {
        push("work.link.branch", &session.head_branch);
    }
    push("work.role", "review");
    push("work.rev", &session.head_sha);
    push("companion.group", group);
    Ok(args)
}

/// Reports whether `byte` may end a group name.
///
/// Accepts an ASCII letter or digit, `.`, `_` or `-`.
fn is_group_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'_' || byte == b'-'
}

/// Reports whether `sha` is exactly 40 lowercase hexadecimal digits.
fn is_head_sha(sha: &str) -> bool {
    let bytes = sha.as_bytes();
    if bytes.len() != HEAD_SHA_LEN {
        return false;
    }
    for byte in bytes {
        if !(*byte >= b'0' && *byte <= b'9' || *byte >= b'a' && *byte <= b'f') {
            return false;
        }
    }
    true
}

/// Reports whether `repository` is a lowercase `owner/name` pair.
///
/// Each side is non-empty and holds only ASCII lowercase letters, digits, `_`, `.` or `-`.
fn is_metadata_repository(repository: &str) -> bool {
    let Some((owner, name)) = repository.split_once('/') else {
        return false;
    };
    if owner.is_empty() || name.is_empty() {
        return false;
    }
    for part in [owner, name] {
        for byte in part.as_bytes() {
            let allowed = (*byte >= b'a' && *byte <= b'z')
                || (*byte >= b'0' && *byte <= b'9')
                || *byte == b'_'
                || *byte == b'.'
                || *byte == b'-';
            if !allowed {
                return false;
            }
        }
    }
    true
}

/// Reports whether `branch` is a plain ref name of 1 to 255 bytes.
///
/// The first byte is an ASCII letter, digit or `_`; the rest add `.`, `/` and `-`.
fn is_metadata_branch(branch: &str) -> bool {
    let bytes = branch.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_BRANCH_LEN {
        return false;
    }
    let Some((first, rest)) = bytes.split_first() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() && *first != b'_' {
        return false;
    }
    for byte in rest {
        let allowed = byte.is_ascii_alphanumeric()
            || *byte == b'_'
            || *byte == b'.'
            || *byte == b'/'
            || *byte == b'-';
        if !allowed {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::{WorkSession, metadata_args};

    #[test]
    fn metadata_args_follow_the_pohunek_work_launch_contract() {
        let head = "a".repeat(40);
        let args = metadata_args(
            &WorkSession {
                repository: "example-org/example-repo".to_owned(),
                number: 8605,
                url: "https://github.com/example-org/example-repo/pull/8605".to_owned(),
                head_branch: "example-user/ABC-1/fix".to_owned(),
                head_sha: head.clone(),
            },
            "autoreview",
        )
        .expect("valid session");
        let want = [
            "--meta".to_owned(),
            "work.link.provider=github".to_owned(),
            "--meta".to_owned(),
            "work.link.kind=pull_request".to_owned(),
            "--meta".to_owned(),
            "work.link.id=example-org/example-repo#8605".to_owned(),
            "--meta".to_owned(),
            "work.link.url=https://github.com/example-org/example-repo/pull/8605".to_owned(),
            "--meta".to_owned(),
            "work.link.branch=example-user/ABC-1/fix".to_owned(),
            "--meta".to_owned(),
            "work.role=review".to_owned(),
            "--meta".to_owned(),
            format!("work.rev={head}"),
            "--meta".to_owned(),
            "companion.group=autoreview".to_owned(),
        ];
        assert!(args.join("\n") == want.join("\n"), "args = {args:?}");
    }

    #[test]
    fn metadata_args_omit_unsafe_branch_and_reject_invalid_identity() {
        let head = "a".repeat(40);
        let valid = WorkSession {
            repository: "example-org/example-repo".to_owned(),
            number: 1,
            url: "https://github.com/example-org/example-repo/pull/1".to_owned(),
            head_sha: head,
            ..WorkSession::default()
        };
        for branch in ["", "-option", "a..b", "with space"] {
            let mut session = valid.clone();
            session.head_branch = branch.to_owned();
            let result = metadata_args(&session, "autoreview");
            assert!(
                result
                    .as_ref()
                    .is_ok_and(|args| !args.join(" ").contains("work.link.branch")),
                "branch {branch:?}: {result:?}"
            );
        }
        let mutations: [fn(&mut WorkSession); 5] = [
            |s| s.repository = "Example-Org/Example-Repo".to_owned(),
            |s| s.number = 0,
            |s| s.head_sha = "abc".to_owned(),
            |s| s.url = "https://github.com/other/repo/pull/1".to_owned(),
            |s| s.url.push('\n'),
        ];
        for mutate in mutations {
            let mut session = valid.clone();
            mutate(&mut session);
            assert!(
                metadata_args(&session, "autoreview").is_err(),
                "accepted invalid identity {session:?}"
            );
        }
    }

    #[test]
    fn metadata_args_reject_a_group_the_companion_would_not_show() {
        let session = WorkSession {
            repository: "example-org/example-repo".to_owned(),
            number: 1,
            url: "https://github.com/example-org/example-repo/pull/1".to_owned(),
            head_sha: "a".repeat(40),
            ..WorkSession::default()
        };
        let long_invalid = "a".repeat(65);
        for group in [
            "",
            " autoreview",
            "autoreview ",
            "auto\nreview",
            "auto\u{202e}review",
            "auto\u{200b}review",
            long_invalid.as_str(),
        ] {
            assert!(
                metadata_args(&session, group).is_err(),
                "accepted group {group:?}"
            );
        }
        let long_valid = "a".repeat(64);
        for group in ["autoreview", "Auto review", long_valid.as_str()] {
            let result = metadata_args(&session, group);
            assert!(result.is_ok(), "group {group:?}: {result:?}");
        }
    }
}
