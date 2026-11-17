//! Eval cases built from a fix commit: the old-side ranges the fix changed, the cleaned texts,
//! the case file text and its create-only write.

// Rust guideline compliant 2026-11-17

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use autoreview_go_compat::go_time::parse_rfc3339;
use serde::Serialize;

use super::case::{CASE_VERSION, Case, Expect, Expected, parse_case};
use super::case::{MAX_DESCRIPTION_CHARS, MAX_RANGE_LINES, MAX_TITLE_CHARS};
use crate::error::Error;
use crate::integrations::{CommentableLines, CommitFile};
use crate::protocol::reason::is_neutralized;
use crate::store::{FindingOutcome, OUTCOME_OPEN};

/// Permission bits of a written case file.
const CASE_FILE_MODE: u32 = 0o644;

/// Outcomes of a finding that a case can be made from.
const CASE_OUTCOMES: [&str; 2] = ["refuted", "open_at_merge"];

/// Outcomes of a sibling finding that leave the head clean.
const CLEAN_OUTCOMES: [&str; 2] = ["refuted", "not_blocking"];

/// Leading digits of the fix commit in a description.
const DESCRIPTION_SHA_CHARS: usize = 12;

/// A file of the fix that gives no expected range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Skipped {
    /// Path of the file at the reviewed head.
    pub path: String,
    /// Why: `added_by_fix`, `no_patch`, `not_selected` or `not_in_review_range`.
    pub reason: &'static str,
}

/// How the operator narrows or replaces the ranges of a fix.
#[derive(Debug)]
pub struct FixSelection<'a> {
    /// Paths to keep, as named at the reviewed head; empty keeps every eligible file.
    pub paths: &'a [String],
    /// Ranges that replace the computed one, by path.
    pub lines: &'a BTreeMap<String, (i64, i64)>,
}

/// The expected range of one file, on the side of the fix's parent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixRange {
    /// Path of the file at the reviewed head.
    pub path: String,
    /// First line of the range, 1-based.
    pub first_line: i64,
    /// Last line of the range, inclusive.
    pub last_line: i64,
    /// Set when the operator gave the range, so nothing was computed from the patch.
    pub overridden: bool,
}

/// Returns a permanent refusal with `code`; `detail` is the cause that the log line shows.
fn refused(code: &'static str, detail: impl Into<String>) -> Error {
    let detail: String = detail.into();
    Error::workflow(code, true, Some(detail.into()))
}

/// Returns the old-side start line of a hunk header `@@ -<start>[,<count>] +... @@`.
fn old_start(header: &str) -> Option<i64> {
    let rest = header.strip_prefix("@@ -")?;
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest.get(..end)?.parse().ok()
}

/// The change groups of a patch while it is read.
#[derive(Default)]
struct Walk {
    groups: Vec<(i64, i64)>,
    /// Old line of the last context line of the hunk.
    before: Option<i64>,
    /// Old lines of the removed run that is open.
    removed: Option<(i64, i64)>,
    /// Set when the open run adds a line.
    inserted: bool,
}

impl Walk {
    /// Ends the open run; `after` is the old line of the context line that ends it.
    ///
    /// A run that removes lines is those lines. A pure insertion is the context lines on both
    /// sides of the insertion point, or `[1, 1]` in a file with none.
    fn close(&mut self, after: Option<i64>) {
        if let Some(range) = self.removed.take() {
            self.groups.push(range);
        } else if self.inserted {
            let near = [self.before, after].into_iter().flatten();
            let (low, high) = (near.clone().min(), near.max());
            self.groups.push((low.unwrap_or(1), high.unwrap_or(1)));
        }
        self.inserted = false;
    }
}

/// Returns the old-side line range of every change group of `patch`, in file order.
///
/// A group is a run of consecutive `-` and `+` lines; the no-newline marker is not a line.
fn old_side_groups(patch: &str) -> Result<Vec<(i64, i64)>, Error> {
    let invalid = || Error::workflow("github_response_invalid", true, None);
    let (mut walk, mut old_line, mut in_hunk) = (Walk::default(), 0_i64, false);
    for line in patch.split('\n') {
        if line.starts_with("@@") {
            walk.close(None);
            walk.before = None;
            old_line = old_start(line).ok_or_else(invalid)?;
            in_hunk = true;
            continue;
        }
        match line.as_bytes().first() {
            None | Some(b'\\') => {}
            _ if !in_hunk => return Err(invalid()),
            Some(b' ') => {
                walk.close(Some(old_line));
                walk.before = Some(old_line);
                old_line += 1;
            }
            Some(b'-') => {
                let first = walk.removed.map_or(old_line, |(first, _)| first);
                walk.removed = Some((first, old_line));
                old_line += 1;
            }
            Some(b'+') => walk.inserted = true,
            Some(_) => return Err(invalid()),
        }
    }
    walk.close(None);
    Ok(walk.groups)
}

/// Returns the expected range of each file of the fix, and the files that give none.
///
/// A path is the file's name at the reviewed head (before a rename). Only a modified or removed
/// file that has an added line in the reviewed range can be caught, so any other file is
/// skipped. One range covers all change groups of a file, so a defect is one finding.
///
/// # Errors
///
/// Returns `eval_add_path_unknown` for a selected path that the fix does not change
/// or for a `--lines` path that ends up skipped, `eval_add_range_too_wide` for a range of
/// more than `MAX_RANGE_LINES` lines, `eval_add_path_unsafe` for a path with a control or format
/// character, `eval_add_no_reviewable_path` when no file is left, `eval_add_path_required` when
/// several are left and the operator selected none, and `github_response_invalid` for a
/// malformed patch.
pub fn fix_ranges(
    files: &[CommitFile],
    commentable: &CommentableLines,
    select: &FixSelection<'_>,
) -> Result<(Vec<FixRange>, Vec<Skipped>), Error> {
    let old_path = |file: &CommitFile| file.previous_path.as_ref().unwrap_or(&file.path).clone();
    for file in files {
        let paths = [Some(&file.path), file.previous_path.as_ref()];
        if paths
            .into_iter()
            .flatten()
            .any(|p| p.chars().any(is_neutralized))
        {
            let detail = "a path of the fix has a control or format character";
            return Err(refused("eval_add_path_unsafe", detail));
        }
    }
    for name in select.paths.iter().chain(select.lines.keys()) {
        if !files.iter().any(|file| old_path(file) == *name) {
            return Err(refused("eval_add_path_unknown", name.as_str()));
        }
    }
    let (mut ranges, mut skipped) = (Vec::new(), Vec::new());
    for file in files {
        let path = old_path(file);
        let reason = if file.status == "added" {
            Some("added_by_fix")
        } else if file.patch.is_empty() {
            Some("no_patch")
        } else if !select.paths.is_empty() && !select.paths.contains(&path) {
            Some("not_selected")
        } else if commentable.get(&path).is_none_or(Vec::is_empty) {
            Some("not_in_review_range")
        } else {
            None
        };
        if let Some(reason) = reason {
            skipped.push(Skipped { path, reason });
            continue;
        }
        let groups = old_side_groups(&file.patch)?;
        let (Some(first), Some(last)) = (groups.first(), groups.last()) else {
            skipped.push(Skipped {
                path,
                reason: "no_patch",
            });
            continue;
        };
        let given = select.lines.get(&path).copied();
        let (first_line, last_line) = given.unwrap_or((first.0, last.1));
        if last_line - first_line >= MAX_RANGE_LINES {
            let detail = format!("{path}: {first_line}-{last_line}; give it as --lines");
            return Err(refused("eval_add_range_too_wide", detail));
        }
        let overridden = given.is_some();
        ranges.push(FixRange {
            path,
            first_line,
            last_line,
            overridden,
        });
    }
    if let Some(name) = select
        .lines
        .keys()
        .find(|n| !ranges.iter().any(|r| r.path == **n))
    {
        return Err(refused(
            "eval_add_path_unknown",
            format!("{name} is skipped"),
        ));
    }
    let names = |paths: Vec<&str>| paths.join(", ");
    match ranges.len() {
        0 => {
            let left = skipped.iter().map(|s| format!("{} ({})", s.path, s.reason));
            let detail = left.collect::<Vec<_>>().join(", ");
            Err(refused("eval_add_no_reviewable_path", detail))
        }
        2.. if select.paths.is_empty() => {
            let candidates = names(ranges.iter().map(|range| range.path.as_str()).collect());
            Err(refused("eval_add_path_required", candidates))
        }
        _ => Ok((ranges, skipped)),
    }
}

/// Returns `text` without controls, format characters and line separators.
fn cleaned(text: &str) -> String {
    text.chars().filter(|c| !is_neutralized(*c)).collect()
}

/// Returns the pull request title as a case holds it: cleaned, trimmed and cut to the limit.
///
/// An empty result means the case has no title.
#[must_use]
pub fn clean_title(title: &str) -> String {
    cleaned(title)
        .trim()
        .chars()
        .take(MAX_TITLE_CHARS)
        .collect()
}

/// Returns the description of a defect found by a fix commit.
///
/// The subject is text from outside, so it is cleaned and the whole text is cut to the limit.
#[must_use]
pub fn fix_description(repository: &str, fix_sha: &str, subject: &str) -> String {
    let sha: String = fix_sha.chars().take(DESCRIPTION_SHA_CHARS).collect();
    let text = format!("Fixed by {sha} ({repository}): {}", cleaned(subject).trim());
    text.chars().take(MAX_DESCRIPTION_CHARS).collect()
}

/// What an operator judges a published finding to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The finding is wrong: a review of the head must block nothing.
    Wrong,
    /// The finding is right: a review must block on its lines.
    Right,
}

/// Reports whether `row` is a blocking finding that still stood when the head of `finding`
/// was reviewed.
///
/// A row of that head counts unless it is `refuted` or `not_blocking`. A row of another head
/// counts too, except one published after `finding` and one whose outcome was decided at
/// another head before `finding` was published. A time that does not parse counts the row.
fn stood_with(row: &FindingOutcome, finding: &FindingOutcome) -> bool {
    let blocking = row.blocking && !CLEAN_OUTCOMES.contains(&row.outcome.as_str());
    if row.finding_key == finding.finding_key || !blocking {
        return false;
    }
    if row.head_sha == finding.head_sha {
        return true;
    }
    let time = |text: &str| parse_rfc3339(text).ok();
    let (row_published, published) = (time(&row.published_at), time(&finding.published_at));
    let later = row_published
        .zip(published)
        .is_some_and(|(row, own)| row > own);
    let decided_before = row.outcome != OUTCOME_OPEN
        && row.outcome_head_sha != finding.head_sha
        && time(&row.updated_at)
            .zip(published)
            .is_some_and(|(updated, own)| updated < own);
    !later && !decided_before
}

/// Returns what a review must do about a published finding, from the operator's verdict.
///
/// `pull_rows` are the rows of the finding's pull request at every head; `lines` and
/// `description` are the operator's, which `Right` needs and `Wrong` refuses; `commentable`
/// is the reviewed range and is read for `Right` only.
///
/// # Errors
///
/// Returns `eval_add_outcome_unsupported` for an outcome other than `refuted` and
/// `open_at_merge`, `eval_add_path_unsafe` for a finding path with a control or format
/// character, `eval_add_verdict_input_invalid` for a `lines` or `description` the verdict
/// does not take or lacks, `eval_add_head_has_other_blocking` when `Wrong` meets another
/// blocking finding that stood at the head, `eval_add_no_reviewable_path` for a path with no
/// added line in the range, and `eval_add_range_too_wide` for more than `MAX_RANGE_LINES`
/// lines.
pub fn finding_expect(
    finding: &FindingOutcome,
    pull_rows: &[FindingOutcome],
    verdict: Verdict,
    lines: &[(String, (i64, i64))],
    description: Option<&str>,
    commentable: &CommentableLines,
) -> Result<Expect, Error> {
    if !CASE_OUTCOMES.contains(&finding.outcome.as_str()) {
        let detail = format!("{} is {}", finding.finding_key, finding.outcome);
        return Err(refused("eval_add_outcome_unsupported", detail));
    }
    if finding.path.chars().any(is_neutralized) {
        let detail = "the path of the finding has a control or format character";
        return Err(refused("eval_add_path_unsafe", detail));
    }
    let text: Option<String> = description.map(|text| {
        cleaned(text)
            .trim()
            .chars()
            .take(MAX_DESCRIPTION_CHARS)
            .collect()
    });
    match (verdict, lines, text) {
        (Verdict::Wrong, [], None) => {
            let others = pull_rows.iter().filter(|row| stood_with(row, finding));
            let keys: Vec<&str> = others.map(|row| row.finding_key.as_str()).collect();
            if keys.is_empty() {
                return Ok(Expect::Clean);
            }
            let detail = format!("{}; write the case by hand", keys.join(", "));
            Err(refused("eval_add_head_has_other_blocking", detail))
        }
        (Verdict::Right, [(path, (first, last))], Some(description))
            if *path == finding.path && !description.is_empty() =>
        {
            if commentable.get(path).is_none_or(Vec::is_empty) {
                return Err(refused("eval_add_no_reviewable_path", path.as_str()));
            }
            if last - first >= MAX_RANGE_LINES {
                let detail =
                    format!("{path}: {first}-{last}; the range is over {MAX_RANGE_LINES} lines");
                return Err(refused("eval_add_range_too_wide", detail));
            }
            Ok(Expect::Blocking(vec![Expected {
                path: path.clone(),
                first_line: *first,
                last_line: *last,
                description,
            }]))
        }
        _ => {
            let detail = "right needs --description and one --lines of the finding's path; \
                          wrong takes neither";
            Err(refused("eval_add_verdict_input_invalid", detail))
        }
    }
}

/// Returns the text of a case file; every string is a double-quoted scalar.
///
/// `source` is one comment line that names where the case comes from.
#[must_use]
pub fn render_case(case: &Case, source: &str) -> String {
    let quoted = |text: &str| serde_json::Value::String(text.to_owned()).to_string();
    let mut text = format!("# {source}\nversion: {CASE_VERSION}\n");
    text += &format!(
        "id: {}\nrepository: {}\n",
        quoted(&case.id),
        quoted(&case.repository)
    );
    text += &format!("pull_request: {}\n", case.pull_request);
    text += &format!("base_branch: {}\n", quoted(&case.base_branch));
    if case.head_ref != format!("refs/pull/{}/head", case.pull_request) {
        text += &format!("head_ref: {}\n", quoted(&case.head_ref));
    }
    text += &format!(
        "base_sha: {}\nhead_sha: {}\n",
        quoted(&case.base_sha),
        quoted(&case.head_sha)
    );
    if !case.pipelines.is_empty() {
        let names: Vec<String> = case.pipelines.iter().map(|name| quoted(name)).collect();
        text += &format!("pipelines: [{}]\n", names.join(", "));
    }
    if !case.title.is_empty() {
        text += &format!("title: {}\n", quoted(&case.title));
    }
    match &case.expect {
        Expect::Clean => text += "expect: clean\n",
        Expect::Blocking(findings) => {
            text += "expect:\n  blocking:\n";
            for found in findings {
                text += &format!("    - path: {}\n", quoted(&found.path));
                text += &format!("      lines: [{}, {}]\n", found.first_line, found.last_line);
                text += &format!("      description: {}\n", quoted(&found.description));
            }
        }
    }
    text
}

/// Writes `case` as `<dir>/<id>.yaml` and returns the file text; an existing file stays as it is.
///
/// `existing` are the cases already in `dir`. The text is parsed back and must equal `case`
/// before anything is written.
///
/// # Errors
///
/// Returns `eval_add_case_exists` when a case has the same repository, pull request and head,
/// `eval_add_case_invalid` when the text does not parse back to `case`, `eval_add_file_exists`
/// when the file exists, and `eval_add_write_failed` when it cannot be written.
pub fn write_case(
    dir: &Path,
    existing: &[Case],
    case: &Case,
    source: &str,
) -> Result<String, Error> {
    let identity = |c: &Case| (c.repository.clone(), c.pull_request, c.head_sha.clone());
    if let Some(same) = existing.iter().find(|c| identity(c) == identity(case)) {
        return Err(refused("eval_add_case_exists", format!("{}.yaml", same.id)));
    }
    let (file, text) = (format!("{}.yaml", case.id), render_case(case, source));
    match parse_case(&file, &text) {
        Ok(parsed) if parsed == *case => {}
        Ok(_) => {
            return Err(refused(
                "eval_add_case_invalid",
                "the text parses to another case",
            ));
        }
        Err(errors) => {
            let first = errors.first().map(|e| format!("{}: {}", e.code, e.message));
            return Err(refused("eval_add_case_invalid", first.unwrap_or_default()));
        }
    }
    let path = dir.join(&file);
    let mut handle = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(CASE_FILE_MODE)
        .open(&path)
        .map_err(|error| match error.kind() {
            ErrorKind::AlreadyExists => refused("eval_add_file_exists", file.as_str()),
            _ => Error::workflow("eval_add_write_failed", false, Some(Box::new(error))),
        })?;
    if let Err(error) = handle
        .write_all(text.as_bytes())
        .and_then(|()| handle.sync_all())
    {
        let left = match fs::remove_file(&path) {
            Ok(()) => String::new(),
            Err(removal) => format!("; the partial file {file} stays: {removal}"),
        };
        let cause = format!("{error}{left}");
        return Err(Error::workflow(
            "eval_add_write_failed",
            false,
            Some(cause.into()),
        ));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{FixSelection, Verdict, clean_title, finding_expect, fix_description};
    use super::{fix_ranges, write_case};
    use crate::error::error_code;
    use crate::eval::case::{Case, Expect, Expected, load_cases};
    use crate::integrations::{CommentableLines, CommitFile};
    use crate::store::FindingOutcome;

    /// A table row of `finding_expect`: name, path and outcome of the finding, its siblings as
    /// `(outcome, blocking)`, the verdict, `--lines`, `--description` and the result. An
    /// outcome ends in `@earlier` for a sibling of an earlier head that is decided later,
    /// `@decided` for one decided at a head reviewed before the finding's, and `@later` for
    /// one published after the finding; any other sibling is of the finding's head.
    type Judged<'a> = (
        &'a str,
        (&'a str, &'a str),
        &'a [(&'a str, bool)],
        Verdict,
        &'a [(&'a str, i64, i64)],
        Option<&'a str>,
        &'a str,
    );

    /// A table row: name, fix files, `--path`, `--lines` and the expected outcome.
    type Row<'a> = (
        &'a str,
        Vec<CommitFile>,
        &'a [&'a str],
        &'a [(&'a str, i64, i64)],
        &'a str,
    );

    fn file(path: &str, status: &str, patch: &str) -> CommitFile {
        CommitFile {
            path: path.to_owned(),
            status: status.to_owned(),
            previous_path: None,
            patch: patch.to_owned(),
        }
    }

    /// Runs `fix_ranges` over a review range where `a.rs`, `b.rs`, `old.rs` and `gone.rs` have
    /// an added line, `deleted.rs` has none and `outside.rs` is not in it.
    fn run(files: &[CommitFile], paths: &[&str], lines: &[(&str, i64, i64)]) -> String {
        let mut commentable = CommentableLines::new();
        for name in ["a.rs", "b.rs", "old.rs", "gone.rs"] {
            commentable.insert(name.to_owned(), vec![1]);
        }
        commentable.insert("deleted.rs".to_owned(), Vec::new());
        let paths: Vec<String> = paths.iter().map(|path| (*path).to_owned()).collect();
        let lines: BTreeMap<String, (i64, i64)> = lines
            .iter()
            .map(|(path, first, last)| ((*path).to_owned(), (*first, *last)))
            .collect();
        let select = FixSelection {
            paths: &paths,
            lines: &lines,
        };
        match fix_ranges(files, &commentable, &select) {
            Ok((ranges, skipped)) => (ranges.iter())
                .map(|r| {
                    let given = if r.overridden { " given" } else { "" };
                    format!("{} {}-{}{given}", r.path, r.first_line, r.last_line)
                })
                .chain(skipped.iter().map(|s| format!("{} !{}", s.path, s.reason)))
                .collect::<Vec<_>>()
                .join("; "),
            Err(error) => format!("err {}", error_code(&error)),
        }
    }

    /// Each row is a fix commit (one modified file, then a table of files), the operator's
    /// `--path` and `--lines`, and the ranges and skipped files that come out, or the code.
    #[test]
    #[rustfmt::skip]
    fn fix_commit_patches_become_old_side_ranges() {
        let wide = |at: i64| format!("@@ -1,1 +1,1 @@\n-a\n+b\n@@ -{at},1 +{at},1 @@\n-c\n+d");
        let patches = [
            ("one modified line", "@@ -3,5 +3,5 @@\n a\n b\n-old\n+new\n c\n d", "a.rs 5-5"),
            ("removed block", "@@ -4,4 +4,2 @@\n a\n-b\n-c\n d", "a.rs 5-6"),
            ("two groups", "@@ -1,1 +1,1 @@\n-a\n+b\n@@ -20,2 +20,2 @@\n c\n-d", "a.rs 1-21"),
            ("insertion in the middle", "@@ -10,4 +10,5 @@\n a\n b\n+new\n c\n d", "a.rs 11-12"),
            ("insertion at line 1", "@@ -1,2 +1,3 @@\n+new\n a\n b", "a.rs 1-1"),
            ("insertion at the end", "@@ -8,2 +8,3 @@\n a\n b\n+new", "a.rs 9-9"),
            ("no newline marker", "@@ -1,1 +1,1 @@\n-a\n\\ No newline at end of file", "a.rs 1-1"),
            ("span of 200 lines", &wide(200), "a.rs 1-200"),
            ("span of 201 lines", &wide(201), "err eval_add_range_too_wide"),
            ("binary file", "", "err eval_add_no_reviewable_path"),
            ("malformed patch", "@@ x @@\n-a", "err github_response_invalid"),
        ];
        for (name, patch, want) in patches {
            let files = [file("a.rs", "modified", patch)];
            assert_eq!(run(&files, &[], &[]), want, "{name}");
        }
        let changed = "@@ -3,5 +3,5 @@\n a\n b\n-old\n+new\n c\n d";
        let m = |path: &str| file(path, "modified", changed);
        let renamed = CommitFile {
            previous_path: Some("old.rs".to_owned()),
            ..file("new.rs", "renamed", changed)
        };
        let removed = file("gone.rs", "removed", "@@ -1,3 +0,0 @@\n-a\n-b\n-c");
        let added = file("new.rs", "added", "@@ -0,0 +1,1 @@\n+x");
        let (a, b, ab) = (m("a.rs"), m("b.rs"), vec![m("a.rs"), m("b.rs")]);
        let skipped = vec![a.clone(), added, m("outside.rs"), m("deleted.rs")];
        let rows: Vec<Row<'_>> = vec![
            ("removed file", vec![removed], &[], &[], "gone.rs 1-3"),
            ("renamed file keeps its old name", vec![renamed], &[], &[], "old.rs 5-5"),
            ("files that give no range", skipped, &[], &[],
                "a.rs 5-5; new.rs !added_by_fix; outside.rs !not_in_review_range; \
                 deleted.rs !not_in_review_range"),
            ("no file left", vec![m("outside.rs")], &[], &[], "err eval_add_no_reviewable_path"),
            ("two files without --path", ab.clone(), &[], &[], "err eval_add_path_required"),
            ("two files with --path", ab.clone(), &["a.rs", "b.rs"], &[], "a.rs 5-5; b.rs 5-5"),
            ("one file selected", ab, &["b.rs"], &[], "b.rs 5-5; a.rs !not_selected"),
            ("--lines replaces", vec![a.clone()], &[], &[("a.rs", 7, 9)], "a.rs 7-9 given"),
            ("--lines unknown path", vec![b], &[], &[("c.rs", 1, 2)], "err eval_add_path_unknown"),
            ("--lines of a skipped file", vec![a.clone(), m("outside.rs")], &[],
                &[("outside.rs", 1, 2)], "err eval_add_path_unknown"),
            ("path with a bidi character", vec![m("a\u{202e}.rs")], &[], &[],
                "err eval_add_path_unsafe"),
        ];
        for (name, files, paths, lines, want) in rows {
            assert_eq!(run(&files, paths, lines), want, "{name}");
        }
    }

    /// Outside text loses controls and bidi marks and is cut to the limits of the case format.
    #[test]
    fn outside_text_is_cleaned_and_cut() {
        assert_eq!(clean_title(&"t".repeat(256)), "t".repeat(200));
        assert_eq!(clean_title("a\u{202e}b\tc\u{2028}d\n"), "abcd");
        assert_eq!(clean_title(" \u{200b} "), "");
        let long = fix_description("acme/widgets", &"a".repeat(40), &"s".repeat(900));
        assert_eq!(long.chars().count(), 500);
        let plain = fix_description("acme/widgets", &"a".repeat(40), "Fix\u{202e} the race\r");
        assert_eq!(plain, "Fixed by aaaaaaaaaaaa (acme/widgets): Fix the race");
    }

    /// Each row is a published finding with the rows of its head, the operator's verdict,
    /// `--lines` and `--description`, and the case or the refusal that comes out.
    #[test]
    #[rustfmt::skip]
    fn a_verdict_on_a_finding_becomes_a_case_or_a_refusal() {
        let row = |key: &str, path: &str, outcome: &str, blocking: bool| FindingOutcome {
            finding_key: key.to_owned(),
            path: path.to_owned(),
            outcome: outcome.to_owned(),
            blocking,
            ..FindingOutcome::default()
        };
        let mut commentable = CommentableLines::new();
        commentable.insert("a.rs".to_owned(), vec![1]);
        commentable.insert("d.rs".to_owned(), Vec::new());
        let (w, r, text) = (Verdict::Wrong, Verdict::Right, Some("Lock released.\u{202e}"));
        let (a, d) = (("a.rs", "refuted"), ("d.rs", "open_at_merge"));
        let merged = ("a.rs", "open_at_merge");
        let bad = "err eval_add_verdict_input_invalid";
        let other = "err eval_add_head_has_other_blocking";
        let (five, wide) = ([("a.rs", 5, 9)], "err eval_add_range_too_wide");
        let rows: Vec<Judged<'_>> = vec![
            ("wrong", a, &[], w, &[], None, "clean"),
            ("wrong, the finding is no sibling of itself", merged, &[], w, &[], None, "clean"),
            ("wrong, siblings that leave the head clean", a,
                &[("refuted", true), ("not_blocking", true), ("open", false)],
                w, &[], None, "clean"),
            ("wrong, a sibling that is open", a, &[("open", true)], w, &[], None, other),
            ("wrong, a sibling that is fixed", a, &[("fixed", true)], w, &[], None, other),
            ("wrong, a sibling of an earlier head that is open", a, &[("open@earlier", true)],
                w, &[], None, other),
            ("wrong, a sibling of an earlier head that is fixed later", a,
                &[("fixed@earlier", true)], w, &[], None, other),
            ("wrong, a sibling decided before this head", a, &[("fixed@decided", true)],
                w, &[], None, "clean"),
            ("wrong, a sibling published after the finding", a, &[("open@later", true)],
                w, &[], None, "clean"),
            ("fixed finding", ("a.rs", "fixed"), &[], w, &[], None,
                "err eval_add_outcome_unsupported"),
            ("wrong, path with a bidi character", ("a\u{202e}.rs", "refuted"), &[], w, &[], None,
                "err eval_add_path_unsafe"),
            ("wrong with --lines", a, &[], w, &five, None, bad),
            ("wrong with --description", a, &[], w, &[], text, bad),
            ("right", a, &[], r, &five, text, "blocking 5-9"),
            ("right without --lines", a, &[], r, &[], text, bad),
            ("right without --description", a, &[], r, &five, None, bad),
            ("right, description empty once cleaned", a, &[], r, &five, Some("\u{202e}"), bad),
            ("right, --lines of another path", a, &[], r, &[("b.rs", 1, 2)], text, bad),
            ("right, path without an added line", d, &[], r, &[("d.rs", 1, 2)], text,
                "err eval_add_no_reviewable_path"),
            ("right, 200 lines", a, &[], r, &[("a.rs", 1, 200)], text, "blocking 1-200"),
            ("right, 201 lines", a, &[], r, &[("a.rs", 1, 201)], text, wide),
        ];
        for (name, (path, outcome), siblings, verdict, lines, description, want) in rows {
            let finding = FindingOutcome {
                head_sha: "h2".to_owned(),
                published_at: "2026-01-02T00:00:00Z".to_owned(),
                ..row("t:O001", path, outcome, true)
            };
            let mut pull_rows = vec![finding.clone()];
            for (at, (spec, blocking)) in siblings.iter().enumerate() {
                let (outcome, when) = spec.split_once('@').unwrap_or((spec, "same"));
                let (head, published, updated, decided) = match when {
                    "earlier" => ("h1", "2026-01-01T00:00:00Z", "2026-01-03T00:00:00Z", "h3"),
                    "decided" => ("h1", "2026-01-01T00:00:00Z", "2026-01-01T12:00:00Z", "h1b"),
                    "later" => ("h3", "2026-01-03T00:00:00Z", "2026-01-03T00:00:00Z", ""),
                    _ => ("h2", "2026-01-02T00:00:00Z", "2026-01-02T00:00:01Z", "h2"),
                };
                let decided = if outcome == "open" { "" } else { decided };
                pull_rows.push(FindingOutcome {
                    head_sha: head.to_owned(),
                    published_at: published.to_owned(),
                    updated_at: updated.to_owned(),
                    outcome_head_sha: decided.to_owned(),
                    ..row(&format!("s:O00{at}"), "z.rs", outcome, *blocking)
                });
            }
            let lines: Vec<(String, (i64, i64))> =
                lines.iter().map(|(p, f, l)| ((*p).to_owned(), (*f, *l))).collect();
            let expect = finding_expect(
                &finding, &pull_rows, verdict, &lines, description, &commentable,
            );
            let got = match expect {
                Ok(Expect::Clean) => "clean".to_owned(),
                Ok(Expect::Blocking(found)) => {
                    assert_eq!(found[0].description, "Lock released.", "{name}");
                    format!("blocking {}-{}", found[0].first_line, found[0].last_line)
                }
                Err(error) => format!("err {}", error_code(&error)),
            };
            assert_eq!(got, want, "{name}");
        }
    }

    /// A case built from plain values is written beside another case, loads, and a second run
    /// for the same head is refused.
    #[test]
    fn a_written_case_loads_and_a_second_run_is_refused() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let other = "version: 1\nid: other\nrepository: acme/widgets\npull_request: 1\n\
                     base_branch: main\nbase_sha: 0123456789abcdef0123456789abcdef01234567\n\
                     head_sha: 89abcdef0123456789abcdef0123456789abcdef\nexpect: clean\n";
        std::fs::write(dir.path().join("other.yaml"), other).expect("other case");
        let case = Case {
            id: "widgets-cache-race".to_owned(),
            repository: "acme/widgets".to_owned(),
            pull_request: 214,
            base_branch: "main".to_owned(),
            head_ref: "refs/heads/eval-cases/cache-race".to_owned(),
            base_sha: "1".repeat(40),
            head_sha: "2".repeat(40),
            pipelines: Vec::new(),
            title: "Cache \"race\" \\ ü".to_owned(),
            expect: Expect::Blocking(vec![Expected {
                path: "src/cache.rs".to_owned(),
                first_line: 40,
                last_line: 52,
                description: fix_description("acme/widgets", &"3".repeat(40), "Fix: \"lock\" #1"),
            }]),
        };
        let existing = load_cases(dir.path()).expect("the directory loads");
        let text = write_case(dir.path(), &existing, &case, "acme/widgets#214").expect("written");
        assert!(
            text.starts_with("# acme/widgets#214\nversion: 1\n"),
            "{text}"
        );
        let loaded = load_cases(dir.path()).expect("both cases load");
        assert!(loaded.contains(&case), "{loaded:?}");
        let again = write_case(dir.path(), &loaded, &case, "acme/widgets#214");
        let refusal = again.expect_err("the head has a case");
        assert_eq!(error_code(&refusal), "eval_add_case_exists");
    }
}
