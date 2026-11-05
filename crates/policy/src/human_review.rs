//! Human review signals: the labels and deleted-test globs that hold a clean review for a person,
//! and the glob matching they use.

// Rust guideline compliant 2026-06-26

use serde::{Deserialize, Serialize};

use crate::Error;
use crate::types::{OnClean, is_label};

/// Most labels and most deleted-test globs a human_review section may list.
const MAX_HUMAN_REVIEW_ENTRIES: usize = 50;

/// Longest accepted glob in bytes.
const MAX_GLOB_BYTES: usize = 256;

/// Byte that separates the segments of a glob and of a repository name.
const SEPARATOR: u8 = b'/';

/// Code point that Go's UTF-8 decoder returns for an invalid byte sequence.
const RUNE_ERROR: u32 = 0xFFFD;

/// Names the signals of a pull request that make a clean review of an auto-approving rule wait for
/// the user. The engine evaluates them from GitHub data, never from reviewer output.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HumanReview {
    /// Holds the review when the pull request carries one of these labels, so an author can ask for
    /// a human.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    /// Path globs; deleting a file that matches one holds the review, because a deleted test may be
    /// the only proof of a contract.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub deleted_tests: Vec<String>,
}

/// The human_review section of a rule as written in the policy file.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct FileHumanReview {
    /// Labels from the file.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    pub(crate) labels: Vec<String>,
    /// Deleted tests from the file.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    pub(crate) deleted_tests: Vec<String>,
}

/// Validates the human_review section of a rule and returns its signals.
///
/// # Errors
///
/// Returns an error when the section sits on a rule whose clean outcome is not approve, sets no
/// entries, lists too many entries, or repeats or malforms a label or glob.
pub(crate) fn parse_human_review(
    raw: Option<&FileHumanReview>,
    on_clean: OnClean,
) -> Result<Option<HumanReview>, Error> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    if on_clean != OnClean::Approve {
        // Every other clean outcome already waits for a human or only comments.
        return Err(Error::HumanReviewNeedsApprove);
    }
    if raw.labels.is_empty() && raw.deleted_tests.is_empty() {
        return Err(Error::HumanReviewEmpty);
    }
    if raw.labels.len() > MAX_HUMAN_REVIEW_ENTRIES
        || raw.deleted_tests.len() > MAX_HUMAN_REVIEW_ENTRIES
    {
        return Err(Error::HumanReviewTooManyEntries {
            limit: MAX_HUMAN_REVIEW_ENTRIES,
        });
    }
    let mut labels = Vec::new();
    for label in &raw.labels {
        if !is_label(label)
            || labels
                .iter()
                .any(|other: &String| other.eq_ignore_ascii_case(label))
        {
            return Err(Error::HumanReviewLabel {
                label: label.clone(),
            });
        }
        labels.push(label.clone());
    }
    let mut globs = Vec::new();
    for glob in &raw.deleted_tests {
        if !valid_glob(glob) || globs.contains(glob) {
            return Err(Error::HumanReviewGlob { glob: glob.clone() });
        }
        globs.push(glob.clone());
    }
    Ok(Some(HumanReview {
        labels,
        deleted_tests: globs,
    }))
}

/// Reports whether glob is a slash-separated relative pattern whose segments are path patterns or
/// **.
fn valid_glob(glob: &str) -> bool {
    if glob.is_empty()
        || glob.len() > MAX_GLOB_BYTES
        || glob.starts_with('/')
        || glob.contains('\\')
    {
        return false;
    }
    for segment in glob.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return false;
        }
        if segment == "**" {
            continue;
        }
        if segment.contains("**") {
            return false;
        }
        if path_match(segment, "").is_err() {
            return false;
        }
    }
    true
}

/// Reports whether a repository path matches a glob of valid_glob: *, ? and classes match within
/// one segment, and ** matches any number of segments, including none.
pub fn match_glob(glob: &str, name: &str) -> bool {
    let pattern: Vec<&str> = glob.split('/').collect();
    let target: Vec<&str> = name.split('/').collect();
    match_segments(&pattern, &target)
}

fn match_segments(pattern: &[&str], name: &[&str]) -> bool {
    let mut pattern = pattern;
    let mut name = name;
    while !pattern.is_empty() {
        let first = pattern[0];
        if first == "**" {
            let rest = &pattern[1..];
            for skip in 0..=name.len() {
                if match_segments(rest, &name[skip..]) {
                    return true;
                }
            }
            return false;
        }
        if name.is_empty() {
            return false;
        }
        let head = name[0];
        match path_match(first, head) {
            Ok(true) => {}
            _ => return false,
        }
        pattern = &pattern[1..];
        name = &name[1..];
    }
    name.is_empty()
}

/// Matches name against pattern with the rules of Go's path.Match. The error is BadPattern when the
/// pattern is malformed, which Go reports even when the name does not match.
fn path_match(pattern: &str, name: &str) -> Result<bool, Error> {
    match_bytes(pattern.as_bytes(), name.as_bytes())
}

/// Runs the path.Match loop over bytes, because Go's star search steps one byte at a time.
fn match_bytes(mut pattern: &[u8], mut name: &[u8]) -> Result<bool, Error> {
    'Pattern: while !pattern.is_empty() {
        let (star, chunk, rest) = scan_chunk(pattern);
        pattern = rest;
        if star && chunk.is_empty() {
            return Ok(!name.contains(&SEPARATOR));
        }
        if let Some(tail) = match_chunk(chunk, name)?
            && (tail.is_empty() || !pattern.is_empty())
        {
            name = tail;
            continue;
        }
        if star {
            for index in 0..name.len() {
                if name[index] == SEPARATOR {
                    break;
                }
                let tail = &name[index + 1..];
                match match_chunk(chunk, tail)? {
                    Some(tail2) => {
                        if pattern.is_empty() && !tail2.is_empty() {
                            continue;
                        }
                        name = tail2;
                        continue 'Pattern;
                    }
                    None => continue,
                }
            }
        }
        // Malformed-rest check: report pattern errors even after mismatch.
        while !pattern.is_empty() {
            let (_, chunk, rest) = scan_chunk(pattern);
            pattern = rest;
            match_chunk(chunk, &[])?;
        }
        return Ok(false);
    }
    Ok(name.is_empty())
}

/// Splits the next chunk off a path.Match pattern: whether leading stars were seen, the chunk up to
/// the next star outside a class, and the rest of the pattern.
fn scan_chunk(mut pattern: &[u8]) -> (bool, &[u8], &[u8]) {
    let mut star = false;
    while !pattern.is_empty() && pattern[0] == b'*' {
        pattern = &pattern[1..];
        star = true;
    }
    let mut in_class = false;
    let mut index = 0;
    while index < pattern.len() {
        let byte = pattern[index];
        if byte == b'\\' {
            if index + 1 < pattern.len() {
                index += 2;
                continue;
            }
            index += 1;
            continue;
        }
        if byte == b'[' {
            in_class = true;
        } else if byte == b']' {
            in_class = false;
        } else if byte == b'*' && !in_class {
            return (star, &pattern[..index], &pattern[index..]);
        }
        index += 1;
    }
    (star, pattern, &[])
}

/// Matches one chunk at the start of name. It returns the rest of the name on a match, None on a
/// mismatch, and an error when the chunk is malformed, which is reported even after a mismatch.
fn match_chunk<'a>(mut chunk: &[u8], mut name: &'a [u8]) -> Result<Option<&'a [u8]>, Error> {
    let mut failed = false;
    let mut rune: u32 = 0;
    while !chunk.is_empty() {
        failed = failed || name.is_empty();
        let first = chunk[0];
        if first == b'[' {
            if !failed {
                let (decoded, width) = decode_rune(name);
                rune = decoded;
                name = &name[width..];
            }
            chunk = &chunk[1..];
            let mut negated = false;
            if !chunk.is_empty() && chunk[0] == b'^' {
                negated = true;
                chunk = &chunk[1..];
            }
            let mut matched = false;
            let mut ranges = 0;
            loop {
                if !chunk.is_empty() && chunk[0] == b']' && ranges > 0 {
                    chunk = &chunk[1..];
                    break;
                }
                let (low, rest) = escaped_rune(chunk)?;
                chunk = rest;
                let mut high = low;
                if !chunk.is_empty() && chunk[0] == b'-' {
                    let (value, rest2) = escaped_rune(&chunk[1..])?;
                    high = value;
                    chunk = rest2;
                }
                matched = matched || (low <= rune && rune <= high);
                ranges += 1;
            }
            failed = failed || (matched == negated);
        } else if first == b'?' {
            if !failed {
                failed = name[0] == SEPARATOR;
                let (_, width) = decode_rune(name);
                name = &name[width..];
            }
            chunk = &chunk[1..];
        } else {
            let mut literal = first;
            chunk = &chunk[1..];
            if first == b'\\' {
                if chunk.is_empty() {
                    return Err(Error::BadPattern);
                }
                literal = chunk[0];
                chunk = &chunk[1..];
            }
            if !failed {
                failed = literal != name[0];
                name = &name[1..];
            }
        }
    }
    if failed { Ok(None) } else { Ok(Some(name)) }
}

/// Reads one class bound from chunk, which may be escaped with a backslash. It returns the code
/// point and the rest of the chunk.
fn escaped_rune(chunk: &[u8]) -> Result<(u32, &[u8]), Error> {
    if chunk.is_empty() || chunk[0] == b'-' || chunk[0] == b']' {
        return Err(Error::BadPattern);
    }
    let mut rest = chunk;
    if rest[0] == b'\\' {
        rest = &rest[1..];
        if rest.is_empty() {
            return Err(Error::BadPattern);
        }
    }
    let (value, width) = decode_rune(rest);
    if value == RUNE_ERROR && width == 1 {
        return Err(Error::BadPattern);
    }
    let remaining = &rest[width..];
    if remaining.is_empty() {
        return Err(Error::BadPattern);
    }
    Ok((value, remaining))
}

/// Decodes the first UTF-8 code point of bytes as Go's utf8.DecodeRuneInString does: an invalid or
/// truncated sequence, including a continuation byte, yields RUNE_ERROR with width 1.
fn decode_rune(bytes: &[u8]) -> (u32, usize) {
    if bytes.is_empty() {
        return (RUNE_ERROR, 0);
    }
    let first = bytes[0];
    if first < 0x80 {
        return (u32::from(first), 1);
    }
    let width = if (0xC2..=0xDF).contains(&first) {
        2
    } else if (0xE0..=0xEF).contains(&first) {
        3
    } else if (0xF0..=0xF4).contains(&first) {
        4
    } else {
        return (RUNE_ERROR, 1);
    };
    if bytes.len() < width {
        return (RUNE_ERROR, 1);
    }
    let slice = &bytes[..width];
    match std::str::from_utf8(slice) {
        Ok(text) => {
            if let Some(character) = text.chars().next() {
                (character as u32, width)
            } else {
                (RUNE_ERROR, 1)
            }
        }
        Err(_) => (RUNE_ERROR, 1),
    }
}

#[cfg(test)]
mod tests {
    use super::match_glob;

    #[test]
    fn match_glob_spans_segments_only_with_double_star() {
        let cases = [
            ("tests/**", "tests/a.php", true),
            ("tests/**", "tests/unit/deep/a.php", true),
            ("tests/**", "src/tests/a.php", false),
            ("**/tests/**", "src/tests/a.php", true),
            ("**/tests/**", "tests/a.php", true),
            ("**/tests/**", "src/testsuite/a.php", false),
            ("src/*.go", "src/a.go", true),
            ("src/*.go", "src/sub/a.go", false),
            ("**/*Test.php", "a/b/OwnerTest.php", true),
            ("**", "anything/at/all", true),
        ];
        for (glob, name, want) in cases {
            assert_eq!(
                match_glob(glob, name),
                want,
                "match_glob({glob:?}, {name:?})"
            );
        }
    }
}
