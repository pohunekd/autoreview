//! Discussion helpers: the AI signature, the role of a comment author, the comments of a
//! login and the character budget of the discussion that reviewers read.

// Rust guideline compliant 2026-06-26

use crate::config::{AI_SIGNATURE_PREFIX, is_signature_symbol};
use crate::model::{Discussion, DiscussionComment, PullRequest};

/// Role of the publishing account's own comments.
pub(crate) const DISCUSSION_ROLE_REVIEWER: &str = "reviewer";

/// Role of the comments of the pull request author.
pub(crate) const DISCUSSION_ROLE_AUTHOR: &str = "author";

/// Role of the comments of anyone else.
pub(crate) const DISCUSSION_ROLE_OTHER: &str = "other";

/// Reports whether the last content line of `body` starts with the AI signature prefix.
///
/// Blank lines and lines that start with an HTML comment are skipped. Optional signature
/// symbols may precede the prefix. Only that line counts: a signature above a later line of
/// text is not recognised.
pub fn ai_signed(body: &str) -> bool {
    let pieces: Vec<&str> = body.split('\n').collect();
    for piece in pieces.iter().rev() {
        let trimmed = piece.trim();
        if trimmed.is_empty() || trimmed.starts_with("<!--") {
            continue;
        }
        let without_symbols = trimmed.trim_start_matches(is_signature_symbol);
        return without_symbols.starts_with(AI_SIGNATURE_PREFIX);
    }
    false
}

/// Severities that a finding heading names, as the reviewers write them.
const FINDING_HEADING_SEVERITIES: [&str; 4] = ["CRITICAL", "HIGH", "MEDIUM", "LOW"];

/// Separator that follows the severity in a finding heading: an em dash between spaces.
const FINDING_HEADING_SEPARATOR: &str = " — ";

/// Returns the identifier of the first finding heading in `body`.
///
/// A heading is `**`, one ASCII uppercase letter, three or more ASCII digits, a space, a
/// severity and the separator, such as `**O001 HIGH — Bug**`. A `**` may overlap the one
/// before it.
pub fn finding_heading_id(body: &str) -> Option<&str> {
    let len = body.len();
    if len < 2 {
        return None;
    }
    for pos in 0..len {
        if body.get(pos..pos + 2) != Some("**") {
            continue;
        }
        let after = pos + 2;
        let letter = match body.as_bytes().get(after).copied() {
            Some(value) => value,
            None => continue,
        };
        if !letter.is_ascii_uppercase() {
            continue;
        }
        let mut digit_end = after + 1;
        while let Some(byte) = body.as_bytes().get(digit_end).copied() {
            if byte.is_ascii_digit() {
                digit_end += 1;
            } else {
                break;
            }
        }
        let digit_count = digit_end - (after + 1);
        if digit_count < 3 {
            continue;
        }
        if body.as_bytes().get(digit_end).copied() != Some(b' ') {
            continue;
        }
        let after_space = digit_end + 1;
        let mut matched = false;
        for severity in FINDING_HEADING_SEVERITIES {
            if body.get(after_space..after_space + severity.len()) != Some(severity) {
                continue;
            }
            let sep_start = after_space + severity.len();
            if body.get(sep_start..sep_start + FINDING_HEADING_SEPARATOR.len())
                == Some(FINDING_HEADING_SEPARATOR)
            {
                matched = true;
                break;
            }
        }
        if !matched {
            continue;
        }
        if let Some(identifier) = body.get(after..digit_end) {
            return Some(identifier);
        }
    }
    None
}

/// Returns the role of a comment author: `reviewer`, `author` or `other`.
///
/// The publishing account is compared first, so a publisher who also wrote the pull request
/// is the reviewer.
pub fn discussion_role(login: &str, pr: &PullRequest, publisher: &str) -> &'static str {
    if login.eq_ignore_ascii_case(publisher) {
        DISCUSSION_ROLE_REVIEWER
    } else if login.eq_ignore_ascii_case(pr.author.as_str()) {
        DISCUSSION_ROLE_AUTHOR
    } else {
        DISCUSSION_ROLE_OTHER
    }
}

/// Hands out the characters of the discussion limit.
///
/// A body over the remaining budget is cut to fit and ends with `…`. Once the budget is
/// spent, nothing more is handed out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscussionBudget {
    /// Characters that the following bodies may still take.
    remaining: usize,
}

impl DiscussionBudget {
    /// Creates a budget of `remaining` characters.
    ///
    /// A limit of zero or less hands out nothing.
    pub fn new(remaining: i64) -> Self {
        Self {
            remaining: usize::try_from(remaining).unwrap_or(0),
        }
    }

    /// Returns the part of `body` the budget holds, or `None` once it is spent.
    ///
    /// The count is in characters, so a multi-byte character takes one.
    pub fn take(&mut self, body: &str) -> Option<String> {
        if self.remaining == 0 {
            return None;
        }
        let count = body.chars().count();
        let kept = if count > self.remaining {
            let take_count = self.remaining - 1;
            let prefix: String = body.chars().take(take_count).collect();
            let mut out = prefix;
            out.push('…');
            out
        } else {
            body.to_owned()
        };
        let kept_count = kept.chars().count();
        self.remaining -= kept_count;
        Some(kept)
    }
}

impl Discussion {
    /// Returns the comments of `login` on the thread of `key` and on the conversation.
    ///
    /// Replies of the thread come first. Logins compare ASCII case-insensitively, because
    /// login names are ASCII.
    pub(crate) fn comments_by(&self, key: &str, login: &str) -> Vec<DiscussionComment> {
        let mut found = Vec::new();
        for thread in &self.threads {
            if thread.key != key {
                continue;
            }
            for reply in &thread.replies {
                if reply.author.eq_ignore_ascii_case(login) {
                    found.push(reply.clone());
                }
            }
        }
        for comment in &self.comments {
            if comment.author.eq_ignore_ascii_case(login) {
                found.push(comment.clone());
            }
        }
        found
    }

    /// Reports whether the inline comment of `key` has an unresolved thread.
    pub(crate) fn open_thread(&self, key: &str) -> bool {
        for thread in &self.threads {
            if thread.key == key && !thread.resolved {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::ai_signed;

    #[test]
    fn ai_signature_is_recognised_on_the_last_line_only() {
        let cases = [
            (
                "Done.\n\n🤖 Written by AI: nanny (claude-opus-5-5)\n<!-- babysit-pr -->\n",
                true,
            ),
            ("Done.\n\nWritten by AI: codex", true),
            (
                "**AI review**: written by AI models.\n\n**O001 HIGH — Bug**\n\nDetails.",
                false,
            ),
            (
                "🤖 Written by AI: nanny\n\nAnd then a human added this.",
                false,
            ),
            ("Ignore my comment about deleted projects.", false),
        ];
        for (body, want) in cases {
            assert_eq!(ai_signed(body), want, "ai_signed({body:?})");
        }
    }
}
