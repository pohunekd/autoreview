//! Changed files of a pull request: the paged file list, the commentable lines and the deleted
//! files.
//!
//! Mirrors `cmd/autoreview/github.go:628-748`. Every file is visited once, in listing order, and
//! the first error stops the read, as Go's visit callback does.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeSet;

use autoreview_engine::error::Error;
use autoreview_engine::integrations::CommentableLines;
use autoreview_engine::model::PullRequest;
use serde::{Deserialize, Serialize};

use super::{GitHub, response_invalid};

/// Files per page of the changed-files API; a short page ends the listing.
#[cfg_attr(not(test), expect(dead_code, reason = "called by E09"))]
const FILES_PER_PAGE: usize = 100;

/// Prefix of the marker line that GitHub writes after a file without a final newline.
const NO_NEWLINE_MARKER: &str = "\\ No newline at end of file";

/// One file of the changed-files API, with the fields that the engine reads.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct ChangedFileRecord {
    /// Path of the file, relative to the repository root.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    filename: String,
    /// Change status, such as `added`, `modified`, `renamed` or `removed`.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    status: String,
    /// Unified diff of the file; empty when GitHub omits it, as for binary files.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    patch: String,
}

autoreview_go_compat::object_only_serde!(ChangedFileRecord);

/// Returns Go's `path.Clean` of `path`, computed over bytes as Go computes it.
///
/// The empty path cleans to `.`. A path that Go leaves unchanged cleans to itself.
#[cfg_attr(not(test), expect(dead_code, reason = "called by E09"))]
fn go_path_clean(path: &[u8]) -> Vec<u8> {
    if path.is_empty() {
        return vec![b'.'];
    }
    let rooted = path.first().is_some_and(|byte| *byte == b'/');
    let length = path.len();
    let mut out: Vec<u8> = Vec::with_capacity(length);
    let mut read_index: usize = 0;
    let mut dotdot: usize = 0;
    if rooted {
        out.push(b'/');
        read_index = 1;
        dotdot = 1;
    }
    while read_index < length {
        let Some(current) = path.get(read_index).copied() else {
            break;
        };
        if current == b'/' {
            read_index += 1;
            continue;
        }
        let is_dot = current == b'.' && {
            let next_is_end = read_index + 1 == length;
            let next_is_slash = path.get(read_index + 1).is_some_and(|byte| *byte == b'/');
            next_is_end || next_is_slash
        };
        if is_dot {
            read_index += 1;
            continue;
        }
        let is_dotdot =
            current == b'.' && path.get(read_index + 1).is_some_and(|byte| *byte == b'.') && {
                let after_is_end = read_index + 2 == length;
                let after_is_slash = path.get(read_index + 2).is_some_and(|byte| *byte == b'/');
                after_is_end || after_is_slash
            };
        if is_dotdot {
            read_index += 2;
            if out.len() > dotdot {
                let mut last_slash: Option<usize> = None;
                for index in dotdot..out.len() {
                    let is_slash = out.get(index).is_some_and(|byte| *byte == b'/');
                    if is_slash {
                        last_slash = Some(index);
                    }
                }
                if let Some(position) = last_slash {
                    out.truncate(position);
                } else {
                    out.truncate(dotdot);
                }
            } else if !rooted {
                if !out.is_empty() {
                    out.push(b'/');
                }
                out.push(b'.');
                out.push(b'.');
                dotdot = out.len();
            }
            continue;
        }
        // Real path element: add slash when needed.
        let need_slash = if rooted {
            out.len() != 1
        } else {
            !out.is_empty()
        };
        if need_slash {
            out.push(b'/');
        }
        while read_index < length {
            let Some(byte) = path.get(read_index).copied() else {
                break;
            };
            if byte == b'/' {
                break;
            }
            out.push(byte);
            read_index += 1;
        }
    }
    if out.is_empty() {
        return vec![b'.'];
    }
    out
}

/// Reports whether a changed file name passes the path checks of `listFiles`
/// (`github.go:700-702`).
///
/// The name must be unchanged by `path.Clean`, must not be absolute and must not start with `../`.
/// The names `.` and `..` pass, as they do in Go.
#[cfg_attr(not(test), expect(dead_code, reason = "called by E09"))]
fn valid_changed_path(name: &str) -> bool {
    if name.starts_with('/') || name.starts_with("../") {
        return false;
    }
    go_path_clean(name.as_bytes()) == name.as_bytes()
}

/// Splits the leading ASCII digits off `text`, or returns `None` when there is no digit.
fn take_digits(text: &str) -> Option<(&str, &str)> {
    let bytes = text.as_bytes();
    let mut end: usize = 0;
    while let Some(byte) = bytes.get(end).copied() {
        if byte.is_ascii_digit() {
            end += 1;
        } else {
            break;
        }
    }
    if end == 0 {
        return None;
    }
    let (head, rest) = text.split_at_checked(end)?;
    Some((head, rest))
}

/// Parses a hunk header the way `hunkPattern` (`github.go:42`) matches it.
///
/// The line must start with `@@ -<digits>[,<digits>] +<digits>[,<digits>] @@`; the text after
/// the closing `@@` is not read. Returns the new-side start and count, where a missing count is 1.
/// A number that does not fit in `i64` returns `None`, as Go's `strconv.Atoi` refuses it.
#[cfg_attr(not(test), expect(dead_code, reason = "called by E09"))]
fn parse_hunk_header(line: &str) -> Option<(i64, i64)> {
    let after_at = line.strip_prefix("@@ -")?;
    let (_, after_old_start) = take_digits(after_at)?;
    let after_old = if let Some(rest) = after_old_start.strip_prefix(',') {
        let (_, after_old_count) = take_digits(rest)?;
        after_old_count
    } else {
        after_old_start
    };
    let after_plus = after_old.strip_prefix(" +")?;
    let (start_text, after_start) = take_digits(after_plus)?;
    let (count_text, after_count) = if let Some(rest) = after_start.strip_prefix(',') {
        let (digits, remaining) = take_digits(rest)?;
        (digits, remaining)
    } else {
        ("1", after_start)
    };
    let _ = after_count.strip_prefix(" @@")?;
    let start = start_text.parse::<i64>().ok()?;
    let count = count_text.parse::<i64>().ok()?;
    Some((start, count))
}

/// Returns the RIGHT-side line numbers that accept a comment, in patch order
/// (`github.go:628-683`).
///
/// Lines before the first hunk, empty lines and the no-newline marker are skipped. Every other
/// line must start with `+`, ` `, `-` or `@@`, or the patch is refused. The line number wraps at
/// `i64` overflow, as Go's `int` does.
#[cfg_attr(not(test), expect(dead_code, reason = "called by E09"))]
fn commentable_lines_from_patch(patch: &str) -> Result<Vec<i64>, Error> {
    let mut result: Vec<i64> = Vec::new();
    let mut line_number: i64 = 0;
    let mut in_hunk = false;
    let mut empty_new_range = false;
    for line in patch.split('\n') {
        if line.starts_with("@@") {
            let (start, count) = parse_hunk_header(line).ok_or_else(|| response_invalid(None))?;
            if start == 0 && count != 0 {
                return Err(response_invalid(None));
            }
            empty_new_range = count == 0;
            line_number = start;
            in_hunk = true;
            continue;
        }
        if !in_hunk || line.is_empty() || line.starts_with(NO_NEWLINE_MARKER) {
            continue;
        }
        let first = line.as_bytes().first().copied();
        if empty_new_range && first.is_some_and(|byte| byte == b'+' || byte == b' ') {
            return Err(response_invalid(None));
        }
        match first {
            Some(b'+') => {
                result.push(line_number);
                line_number = line_number.wrapping_add(1);
            }
            Some(b' ') => {
                line_number = line_number.wrapping_add(1);
            }
            Some(b'-') => {}
            _ => {
                return Err(response_invalid(None));
            }
        }
    }
    Ok(result)
}
/// Decodes one page of the changed-files API; `null` is refused and an empty list is accepted.
#[cfg_attr(not(test), expect(dead_code, reason = "called by E09"))]
fn decode_files_page(output: &str) -> Result<Vec<ChangedFileRecord>, Error> {
    serde_json::from_str(output).map_err(|error| response_invalid(Some(Box::new(error))))
}

/// Returns the argv of one page of the changed-files API (`github.go:685-700`).
#[cfg_attr(not(test), expect(dead_code, reason = "called by E09"))]
fn files_args(repository: &str, number: i64, page: i64) -> Vec<String> {
    vec![
        "api".to_owned(),
        format!("repos/{repository}/pulls/{number}/files?per_page=100&page={page}"),
    ]
}

impl GitHub {
    /// Returns the commentable lines of every changed file, keyed by path (`github.go:715-728`).
    #[expect(dead_code, reason = "called by E09")]
    pub(crate) async fn commentable_lines(
        &self,
        pr: &PullRequest,
    ) -> Result<CommentableLines, Error> {
        let mut result: CommentableLines = CommentableLines::new();
        self.visit_changed_files(pr, |file| {
            let lines = commentable_lines_from_patch(&file.patch)?;
            result.insert(file.filename.clone(), lines);
            Ok(())
        })
        .await?;
        Ok(result)
    }

    /// Returns the paths of the files that the pull request removes (`github.go:731-747`).
    #[expect(dead_code, reason = "called by E09")]
    pub(crate) async fn deleted_files(&self, pr: &PullRequest) -> Result<Vec<String>, Error> {
        let mut removed: Vec<String> = Vec::new();
        self.visit_changed_files(pr, |file| {
            if file.status == "removed" {
                removed.push(file.filename.clone());
            }
            Ok(())
        })
        .await?;
        Ok(removed)
    }

    /// Visits every changed file once, in listing order, with a validated path
    /// (`github.go:685-712`).
    ///
    /// A page that is short ends the listing. A file whose name is invalid or repeated fails the
    /// read before `visit` sees it.
    #[expect(dead_code, reason = "called by E09")]
    async fn visit_changed_files(
        &self,
        pr: &PullRequest,
        mut visit: impl FnMut(&ChangedFileRecord) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for page in 1..=self.search_page_limit {
            let arguments = files_args(&pr.repository, pr.number, page);
            let output = self.github::<()>(&arguments, None).await?;
            let files = decode_files_page(&output)?;
            for file in &files {
                if !valid_changed_path(&file.filename) || seen.contains(&file.filename) {
                    return Err(response_invalid(None));
                }
                seen.insert(file.filename.clone());
                visit(file)?;
            }
            if files.len() < FILES_PER_PAGE {
                return Ok(());
            }
            if page == self.search_page_limit {
                return Err(Error::workflow("github_search_limit_exceeded", true, None));
            }
        }
        Err(Error::workflow("github_search_limit_exceeded", true, None))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn commentable_lines_from_patch() {
        let lines = super::commentable_lines_from_patch(
            "@@ -1,3 +1,4 @@\n context\n-removed\n+added\n+added too\n context\n\\ No newline \
at end of file",
        )
        .expect("patch parses");
        assert_eq!(lines, vec![2, 3]);
        let deleted = super::commentable_lines_from_patch("@@ -1,2 +0,0 @@\n-a\n-b")
            .expect("a deleted file parses");
        assert!(deleted.is_empty());
        assert!(super::commentable_lines_from_patch("@@ broken @@").is_err());
    }
}
