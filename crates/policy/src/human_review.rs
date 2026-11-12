//! Human review signals: the labels, globs, size limits, note limit and authors that hold a clean
//! review for a person, and the glob matching they use.

// Rust guideline compliant 2026-06-26

use serde::{Deserialize, Serialize};

use crate::Error;
use crate::authors::login_valid;
use crate::types::{OnClean, is_label};

/// Most entries of each list a human_review section may hold.
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
#[serde(default, remote = "Self")]
pub struct HumanReview {
    /// Holds the review when the pull request carries one of these labels, so an author can ask for
    /// a human.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    /// Path globs; deleting a file that matches one holds the review, because a deleted test may be
    /// the only proof of a contract.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub deleted_tests: Vec<String>,
    /// Path globs; a changed file that matches one holds the review.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    /// Holds the review when the diff changes more lines than this (additions plus deletions).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_changed_lines: Option<i64>,
    /// Holds the review when the pull request changes more files than this.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_changed_files: Option<i64>,
    /// Holds the review when the finished review has more notes than this.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_notes: Option<i64>,
    /// Logins, matched case-insensitively, and author classes (`trusted`, `untrusted`) whose pull
    /// requests always wait.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<String>,
}

autoreview_go_compat::object_only_serde!(HumanReview);

impl HumanReview {
    /// Reports whether a signal reads the changed files of the pull request.
    #[must_use]
    pub fn needs_changed_files(&self) -> bool {
        !self.deleted_tests.is_empty()
            || !self.paths.is_empty()
            || self.max_changed_lines.is_some()
            || self.max_changed_files.is_some()
    }

    /// Reports whether a signal beyond labels and deleted tests is set; the digest hashes those
    /// only then, so policies that use neither keep their digest.
    pub(crate) fn has_extended_signals(&self) -> bool {
        !self.paths.is_empty()
            || self.max_changed_lines.is_some()
            || self.max_changed_files.is_some()
            || self.max_notes.is_some()
            || !self.authors.is_empty()
    }
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
    /// Paths from the file.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    pub(crate) paths: Vec<String>,
    /// Changed-line limit from the file; null is the same as absent.
    pub(crate) max_changed_lines: Option<i64>,
    /// Changed-file limit from the file; null is the same as absent.
    pub(crate) max_changed_files: Option<i64>,
    /// Note limit from the file; null is the same as absent.
    pub(crate) max_notes: Option<i64>,
    /// Authors from the file.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    pub(crate) authors: Vec<String>,
}

/// Validates the human_review section of a rule and returns its signals.
///
/// # Errors
///
/// Returns an error when the section sits on a rule whose clean outcome is not approve, sets no
/// signal, lists too many entries, repeats or malforms a label, glob or author, or sets a limit
/// below 1.
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
    let empty = raw.labels.is_empty()
        && raw.deleted_tests.is_empty()
        && raw.paths.is_empty()
        && raw.authors.is_empty()
        && raw.max_changed_lines.is_none()
        && raw.max_changed_files.is_none()
        && raw.max_notes.is_none();
    if empty {
        return Err(Error::HumanReviewEmpty);
    }
    let lists = [
        raw.labels.len(),
        raw.deleted_tests.len(),
        raw.paths.len(),
        raw.authors.len(),
    ];
    if lists.iter().any(|len| *len > MAX_HUMAN_REVIEW_ENTRIES) {
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
    Ok(Some(HumanReview {
        labels,
        deleted_tests: parse_globs("deleted_tests", &raw.deleted_tests)?,
        paths: parse_globs("paths", &raw.paths)?,
        max_changed_lines: positive("max_changed_lines", raw.max_changed_lines)?,
        max_changed_files: positive("max_changed_files", raw.max_changed_files)?,
        max_notes: positive("max_notes", raw.max_notes)?,
        authors: parse_authors(&raw.authors)?,
    }))
}

/// Validates the globs of one list, each a valid relative glob listed once.
fn parse_globs(key: &'static str, globs: &[String]) -> Result<Vec<String>, Error> {
    let mut parsed: Vec<String> = Vec::new();
    for glob in globs {
        if !valid_glob(glob) || parsed.contains(glob) {
            return Err(Error::HumanReviewGlob {
                key,
                glob: glob.clone(),
            });
        }
        parsed.push(glob.clone());
    }
    Ok(parsed)
}

/// Refuses a limit below 1; an absent limit stays absent.
fn positive(key: &'static str, value: Option<i64>) -> Result<Option<i64>, Error> {
    match value {
        Some(number) if number < 1 => Err(Error::HumanReviewNumber { key }),
        other => Ok(other),
    }
}

/// Validates the authors: a login or an author class, each listed once whatever its case.
fn parse_authors(entries: &[String]) -> Result<Vec<String>, Error> {
    let mut parsed: Vec<String> = Vec::new();
    for entry in entries {
        let known = is_author_class(entry) || login_valid(entry);
        if !known || parsed.iter().any(|other| other.eq_ignore_ascii_case(entry)) {
            return Err(Error::HumanReviewAuthor {
                author: entry.clone(),
            });
        }
        parsed.push(entry.clone());
    }
    Ok(parsed)
}

/// Reports whether value is one of the author classes that the policy decides for a pull request.
fn is_author_class(value: &str) -> bool {
    matches!(value, "trusted" | "untrusted")
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
