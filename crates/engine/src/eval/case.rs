//! The eval case file: one pull request head and what a review of it must find.
//!
//! A case is one YAML file named `<id>.yaml`. Validation collects every error of every file, so
//! one run names everything to fix.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::Path;

use serde::de::value::MapAccessDeserializer;
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use crate::model::ids::{branch_valid, repository_valid, sha_valid};

/// The only case format version.
pub const CASE_VERSION: i64 = 1;

/// Largest case file, in bytes.
pub const MAX_CASE_BYTES: u64 = 64 * 1024;

/// Most expected findings of one case.
pub const MAX_EXPECTED: usize = 20;

/// Most lines of one expected range; a wider range would let any finding match.
pub const MAX_RANGE_LINES: i64 = 200;

/// Longest repository path of an expected finding, in bytes.
const MAX_PATH_BYTES: usize = 4096;

/// Longest description of an expected finding, in characters.
const MAX_DESCRIPTION_CHARS: usize = 500;

/// Longest pull request title, in characters.
const MAX_TITLE_CHARS: usize = 200;

/// Longest case or pipeline identifier, in characters.
const MAX_ID_CHARS: usize = 64;

/// Longest YAML error message kept in a case error, in characters.
const MAX_YAML_MESSAGE_CHARS: usize = 300;

/// Extension of a case file.
const CASE_EXTENSION: &str = "yaml";

/// Extension that a case file must not have, so no case is skipped silently.
const REFUSED_EXTENSION: &str = "yml";

/// One validated case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Case {
    /// Identifier of the case; it is the file name without `.yaml`.
    pub id: String,
    /// Repository as lowercase `owner/name`.
    pub repository: String,
    /// Pull request number.
    pub pull_request: i64,
    /// Base branch the reviewer worktree starts from.
    pub base_branch: String,
    /// Ref the session fetches the head from: `refs/pull/<pull_request>/head` or
    /// `refs/heads/<branch>`; the fetched ref must peel to `head_sha`.
    pub head_ref: String,
    /// Base commit of the reviewed diff.
    pub base_sha: String,
    /// Head commit of the reviewed diff.
    pub head_sha: String,
    /// Pipelines that run the case; empty means every pipeline.
    pub pipelines: Vec<String>,
    /// Pull request title the reviewers see; empty when the case has none.
    pub title: String,
    /// What a review of the case must report.
    pub expect: Expect,
}

impl Case {
    /// Reports whether the case expects blocking findings.
    #[must_use]
    pub fn is_defect(&self) -> bool {
        matches!(self.expect, Expect::Blocking(_))
    }

    /// Reports whether the case runs under the pipeline `name`.
    #[must_use]
    pub fn runs_under(&self, name: &str) -> bool {
        self.pipelines.is_empty() || self.pipelines.iter().any(|pipeline| pipeline == name)
    }
}

/// What a review of a case must report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expect {
    /// No blocking finding.
    Clean,
    /// Each of these defects, as a blocking finding in its path and line range.
    Blocking(Vec<Expected>),
}

/// One defect that a review must report as a blocking finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expected {
    /// Repository path of the file, as the pull request's file list names it.
    pub path: String,
    /// First line of the range, 1-based and inclusive, on the right side of the head.
    pub first_line: i64,
    /// Last line of the range, inclusive.
    pub last_line: i64,
    /// What is wrong, for the human who reads the report.
    pub description: String,
}

/// One problem of one case file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CaseError {
    /// File name, or the cases directory for a directory problem.
    pub file: String,
    /// Stable code, `eval_case_` and a reason.
    pub code: &'static str,
    /// The key at fault, such as `expect.blocking[0].lines`; empty for a whole file. For
    /// `eval_case_key_unknown` it is the name of the unknown key alone.
    pub key: String,
    /// What is wrong.
    pub message: String,
}

/// Case file as it is read, before any value is checked.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCase {
    version: Option<i64>,
    id: String,
    repository: String,
    pull_request: i64,
    base_branch: String,
    head_ref: Option<String>,
    base_sha: String,
    head_sha: String,
    pipelines: Option<Vec<String>>,
    title: Option<String>,
    expect: Option<RawExpect>,
}

/// Expected finding as it is read.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFinding {
    path: String,
    lines: Vec<i64>,
    description: String,
}

/// The mapping form of `expect`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBlocking {
    blocking: Vec<RawFinding>,
}

/// `expect` as it is read: the word `clean` or a mapping with `blocking`.
enum RawExpect {
    Clean,
    Blocking(Vec<RawFinding>),
}

impl<'de> Deserialize<'de> for RawExpect {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(ExpectVisitor)
    }
}

/// Reads `expect` in either form and keeps the unknown-key error of the mapping.
struct ExpectVisitor;

impl<'de> Visitor<'de> for ExpectVisitor {
    type Value = RawExpect;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("`clean` or a mapping with `blocking`")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<RawExpect, E> {
        if value == "clean" {
            Ok(RawExpect::Clean)
        } else {
            Err(E::invalid_value(de::Unexpected::Str(value), &self))
        }
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<RawExpect, A::Error> {
        let raw = RawBlocking::deserialize(MapAccessDeserializer::new(map))?;
        Ok(RawExpect::Blocking(raw.blocking))
    }
}

/// Collects the errors of one file.
struct Collector {
    file: String,
    errors: Vec<CaseError>,
}

impl Collector {
    fn add(&mut self, code: &'static str, key: &str, message: &str) {
        self.errors.push(CaseError {
            file: self.file.clone(),
            code,
            key: key.to_owned(),
            message: message.to_owned(),
        });
    }
}

/// Reports whether `value` is an identifier: lowercase letters, digits, `.`, `_` and `-`.
fn identifier_valid(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
        && value.chars().count() <= MAX_ID_CHARS
        && chars
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

/// Reports whether `branch` is a branch name that git accepts in `refs/heads/<branch>`.
fn head_branch_valid(branch: &str) -> bool {
    branch_valid(branch)
        && !branch.starts_with('-')
        && branch
            .split('/')
            .all(|part| !part.is_empty() && !part.starts_with('.') && !part.ends_with(".lock"))
}

/// Reports whether `path` is a relative repository path that a file list can name.
fn path_valid(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= MAX_PATH_BYTES
        && !path.starts_with('/')
        && !path.contains(['\\', '\0'])
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// Reports whether `text` is non-empty, short enough and free of control characters.
fn text_valid(text: &str, max_chars: usize) -> bool {
    !text.trim().is_empty()
        && text.chars().count() <= max_chars
        && !text.chars().any(char::is_control)
}

/// Checks one expected finding and returns it when it is valid.
fn check_finding(raw: &RawFinding, index: usize, out: &mut Collector) -> Option<Expected> {
    let key = |name: &str| format!("expect.blocking[{index}].{name}");
    let before = out.errors.len();
    if !path_valid(&raw.path) {
        let message = format!(
            "must be a relative path of at most {MAX_PATH_BYTES} bytes without `.`, `..`, \
             empty parts, backslashes or NUL"
        );
        out.add("eval_case_finding_path_invalid", &key("path"), &message);
    }
    let (first_line, last_line) = match raw.lines.as_slice() {
        &[first, last] if first >= 1 && last >= first && last - first < MAX_RANGE_LINES => {
            (first, last)
        }
        _ => {
            let message = format!(
                "must be [first, last] with 1 <= first <= last, at most {MAX_RANGE_LINES} lines"
            );
            out.add("eval_case_finding_lines_invalid", &key("lines"), &message);
            (0, 0)
        }
    };
    if !text_valid(&raw.description, MAX_DESCRIPTION_CHARS) {
        let message =
            format!("must be 1 to {MAX_DESCRIPTION_CHARS} characters without control characters");
        out.add(
            "eval_case_finding_description_invalid",
            &key("description"),
            &message,
        );
    }
    (out.errors.len() == before).then(|| Expected {
        path: raw.path.clone(),
        first_line,
        last_line,
        description: raw.description.clone(),
    })
}

/// Checks `expect` and returns it when it is valid.
fn check_expect(raw: Option<RawExpect>, out: &mut Collector) -> Option<Expect> {
    let findings = match raw {
        None => {
            out.add("eval_case_expect_invalid", "expect", "is required");
            return None;
        }
        Some(RawExpect::Clean) => return Some(Expect::Clean),
        Some(RawExpect::Blocking(findings)) => findings,
    };
    if findings.is_empty() || findings.len() > MAX_EXPECTED {
        let message = format!("blocking must list 1 to {MAX_EXPECTED} findings");
        out.add("eval_case_expect_invalid", "expect.blocking", &message);
        return None;
    }
    let mut seen = BTreeSet::new();
    let mut expected = Vec::new();
    let mut valid = true;
    for (index, raw) in findings.iter().enumerate() {
        match check_finding(raw, index, out) {
            Some(finding) => {
                let key = (finding.path.clone(), finding.first_line, finding.last_line);
                if seen.insert(key) {
                    expected.push(finding);
                } else {
                    let name = format!("expect.blocking[{index}]");
                    out.add(
                        "eval_case_finding_duplicate",
                        &name,
                        "repeats an earlier finding",
                    );
                    valid = false;
                }
            }
            None => valid = false,
        }
    }
    valid.then_some(Expect::Blocking(expected))
}

/// Maps a YAML error to a case error; an unknown key has its own code.
fn yaml_error(error: &serde_saphyr::Error, out: &mut Collector) {
    let plain = error.without_snippet();
    let (code, key) = match plain {
        serde_saphyr::Error::SerdeUnknownField { field, .. } => {
            ("eval_case_key_unknown", field.to_string())
        }
        _ => ("eval_case_yaml_invalid", String::new()),
    };
    let message: String = (plain.to_string().chars())
        .take(MAX_YAML_MESSAGE_CHARS)
        .collect();
    out.add(code, &key, &message);
}

/// Parses and validates one case file.
///
/// `file` is the file name, which must be `<id>.yaml`.
///
/// # Errors
///
/// Returns every problem of the file; the file is never accepted in part.
pub fn parse_case(file: &str, text: &str) -> Result<Case, Vec<CaseError>> {
    let mut out = Collector {
        file: file.to_owned(),
        errors: Vec::new(),
    };
    let raw = match serde_saphyr::from_str::<RawCase>(text) {
        Ok(raw) => raw,
        Err(error) => {
            yaml_error(&error, &mut out);
            return Err(out.errors);
        }
    };
    if raw.version != Some(CASE_VERSION) {
        out.add("eval_case_version_unsupported", "version", "must be 1");
    }
    if !identifier_valid(&raw.id) {
        let message = format!("must be 1 to {MAX_ID_CHARS} of a-z, 0-9, `.`, `_`, `-`");
        out.add("eval_case_id_invalid", "id", &message);
    }
    if file.strip_suffix(&format!(".{CASE_EXTENSION}")) != Some(raw.id.as_str()) {
        out.add(
            "eval_case_id_mismatch",
            "id",
            "must equal the file name without `.yaml`",
        );
    }
    if !repository_valid(&raw.repository) {
        out.add(
            "eval_case_repository_invalid",
            "repository",
            "must be lowercase owner/name",
        );
    }
    if raw.pull_request < 1 {
        out.add(
            "eval_case_pull_request_invalid",
            "pull_request",
            "must be at least 1",
        );
    }
    if !branch_valid(&raw.base_branch) {
        out.add(
            "eval_case_base_branch_invalid",
            "base_branch",
            "is not a branch name",
        );
    }
    let default_ref = format!("refs/pull/{}/head", raw.pull_request);
    let head_ref = raw.head_ref.unwrap_or_else(|| default_ref.clone());
    let branch_ref = head_ref
        .strip_prefix("refs/heads/")
        .is_some_and(head_branch_valid);
    if head_ref != default_ref && !branch_ref {
        out.add(
            "eval_case_head_ref_invalid",
            "head_ref",
            "must be refs/pull/<pull_request>/head or refs/heads/<branch>",
        );
    }
    for (key, value) in [("base_sha", &raw.base_sha), ("head_sha", &raw.head_sha)] {
        if !sha_valid(value) {
            out.add(
                "eval_case_sha_invalid",
                key,
                "must be 40 lowercase hex digits",
            );
        }
    }
    if raw.base_sha == raw.head_sha {
        out.add(
            "eval_case_sha_equal",
            "head_sha",
            "must differ from base_sha",
        );
    }
    let listed = raw.pipelines.is_some();
    let pipelines = raw.pipelines.unwrap_or_default();
    let distinct: BTreeSet<&String> = pipelines.iter().collect();
    let identifiers = pipelines.iter().all(|name| identifier_valid(name));
    if (listed && pipelines.is_empty()) || distinct.len() != pipelines.len() || !identifiers {
        let message = "must list distinct pipeline names, or be left out";
        out.add("eval_case_pipelines_invalid", "pipelines", message);
    }
    let titled = raw.title.is_some();
    let title = raw.title.unwrap_or_default();
    if titled && !text_valid(&title, MAX_TITLE_CHARS) {
        let message =
            format!("must be 1 to {MAX_TITLE_CHARS} characters without control characters");
        out.add("eval_case_title_invalid", "title", &message);
    }
    let expect = check_expect(raw.expect, &mut out);
    let Some(expect) = expect.filter(|_| out.errors.is_empty()) else {
        return Err(out.errors);
    };
    Ok(Case {
        id: raw.id,
        repository: raw.repository,
        pull_request: raw.pull_request,
        base_branch: raw.base_branch,
        head_ref,
        base_sha: raw.base_sha,
        head_sha: raw.head_sha,
        pipelines,
        title,
        expect,
    })
}

/// Reads every case of the directory `dir`, not recursively.
///
/// A `.yml` file and a file without extension are errors; any other extension and any name that
/// starts with a dot is ignored. Extensions compare ignoring ASCII case. A symbolic link or any
/// other entry that is not a regular file, named like a case file, is an error, so no case is
/// read from outside the directory and no pipe is opened; an entry that cannot be read is an
/// error of its own
/// while the rest of the directory is still checked. Two cases of one repository, pull request
/// and head are the same review identity.
///
/// # Errors
///
/// Returns the errors of every file and of the directory.
pub fn load_cases(dir: &Path) -> Result<Vec<Case>, Vec<CaseError>> {
    let name = dir.display().to_string();
    let unreadable = |file: &str, cause: &dyn fmt::Display| CaseError {
        file: file.to_owned(),
        code: "eval_case_unreadable",
        key: String::new(),
        message: cause.to_string(),
    };
    let entries = fs::read_dir(dir).map_err(|cause| vec![unreadable(&name, &cause)])?;
    let (mut cases, mut errors): (Vec<Case>, Vec<CaseError>) = (Vec::new(), Vec::new());
    let mut files: Vec<(String, std::path::PathBuf)> = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => files.push((
                entry.file_name().to_string_lossy().into_owned(),
                entry.path(),
            )),
            Err(cause) => errors.push(unreadable(&name, &cause)),
        }
    }
    files.sort();
    let mut identities = BTreeSet::new();
    for (file, path) in files {
        if file.starts_with('.') {
            continue;
        }
        let extension =
            (path.extension().and_then(|value| value.to_str())).map(str::to_ascii_lowercase);
        let extension = extension.as_deref();
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(cause) => {
                errors.push(unreadable(&file, &cause));
                continue;
            }
        };
        if metadata.is_dir() {
            continue;
        }
        if extension.is_none() || extension == Some(REFUSED_EXTENSION) {
            let message = "a case file is named <id>.yaml";
            errors.push(CaseError {
                file,
                code: "eval_case_extension",
                key: String::new(),
                message: message.to_owned(),
            });
            continue;
        }
        if extension != Some(CASE_EXTENSION) {
            continue;
        }
        if metadata.file_type().is_symlink() {
            errors.push(unreadable(&file, &"a case file is not a symbolic link"));
            continue;
        }
        if !metadata.is_file() {
            errors.push(unreadable(&file, &"a case file is a regular file"));
            continue;
        }
        if metadata.len() > MAX_CASE_BYTES {
            let message = format!("is larger than {MAX_CASE_BYTES} bytes");
            errors.push(CaseError {
                file,
                code: "eval_case_too_large",
                key: String::new(),
                message,
            });
            continue;
        }
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(cause) => {
                errors.push(unreadable(&file, &cause));
                continue;
            }
        };
        match parse_case(&file, &text) {
            Ok(case) => {
                let identity = (
                    case.repository.clone(),
                    case.pull_request,
                    case.head_sha.clone(),
                );
                if identities.insert(identity) {
                    cases.push(case);
                } else {
                    let message =
                        "repeats the repository, pull request and head of an earlier case";
                    errors.push(CaseError {
                        file,
                        code: "eval_case_identity_duplicate",
                        key: String::new(),
                        message: message.to_owned(),
                    });
                }
            }
            Err(found) => errors.extend(found),
        }
    }
    if errors.is_empty() {
        Ok(cases)
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::{Case, CaseError, Expect, load_cases, parse_case};

    const HEADER: &str = "version: 1\nid: widgets-cache-race\nrepository: acme/widgets\n\
pull_request: 214\nbase_branch: main\n\
base_sha: 0123456789abcdef0123456789abcdef01234567\n\
head_sha: 89abcdef0123456789abcdef0123456789abcdef\npipelines: [default]\ntitle: Cache race\n";

    const ITEM: &str = "    - path: src/cache.rs\n      lines: [40, 52]\n      \
description: Lock released before the entry is inserted.\n";

    const BLOCKING: &str = "expect:\n  blocking:\n    - path: src/cache.rs\n      \
lines: [40, 52]\n      description: Lock released before the entry is inserted.\n";

    fn codes(errors: &[CaseError]) -> Vec<&'static str> {
        errors.iter().map(|error| error.code).collect()
    }

    fn parse(text: &str) -> Result<Case, Vec<CaseError>> {
        parse_case("widgets-cache-race.yaml", text)
    }

    /// The case format refuses each malformed value with its own code, and a directory
    /// reports every problem of every file at once.
    #[test]
    fn case_files_are_validated_with_one_code_per_problem() {
        let valid = format!("{HEADER}{BLOCKING}");
        let case = parse(&valid).expect("the base case is valid");
        assert!(case.is_defect() && case.runs_under("default") && !case.runs_under("other"));
        let clean = parse(&format!("{HEADER}expect: clean\n")).expect("a clean case is valid");
        assert_eq!(clean.expect, Expect::Clean);
        let head_ref = |value: &str| {
            valid.replace(
                "base_branch: main",
                &format!("base_branch: main\nhead_ref: {value}"),
            )
        };
        // The head ref defaults to the pull request ref; both accepted forms are kept as written.
        assert_eq!(case.head_ref, "refs/pull/214/head");
        for want in ["refs/pull/214/head", "refs/heads/eval-cases/cache-race"] {
            let parsed = parse(&head_ref(want)).expect("a valid head ref");
            assert_eq!(parsed.head_ref, want);
        }
        let rows = [
            (head_ref("refs/pull/215/head"), "eval_case_head_ref_invalid"),
            (
                head_ref("refs/pull/214/merge"),
                "eval_case_head_ref_invalid",
            ),
            (head_ref("refs/heads/a..b"), "eval_case_head_ref_invalid"),
            (head_ref("refs/heads/"), "eval_case_head_ref_invalid"),
            (head_ref("refs/heads/x/"), "eval_case_head_ref_invalid"),
            (head_ref("refs/heads/a//b"), "eval_case_head_ref_invalid"),
            (head_ref("refs/heads/x.lock"), "eval_case_head_ref_invalid"),
            (
                head_ref("refs/heads/a.lock/b"),
                "eval_case_head_ref_invalid",
            ),
            (head_ref("refs/heads/.x"), "eval_case_head_ref_invalid"),
            (head_ref("refs/heads/a/.x"), "eval_case_head_ref_invalid"),
            (head_ref("refs/heads/-x"), "eval_case_head_ref_invalid"),
            (head_ref("refs/tags/v1"), "eval_case_head_ref_invalid"),
            (head_ref("main"), "eval_case_head_ref_invalid"),
            (head_ref("\"\""), "eval_case_head_ref_invalid"),
            (
                valid.replace("version: 1", "version: 2"),
                "eval_case_version_unsupported",
            ),
            (
                valid.replace("version: 1\n", ""),
                "eval_case_version_unsupported",
            ),
            (
                valid.replace("title: Cache race", "title: \"\""),
                "eval_case_title_invalid",
            ),
            (
                valid.replace("id: widgets-cache-race", "id: other"),
                "eval_case_id_mismatch",
            ),
            (
                valid.replace("acme/widgets", "Acme/Widgets"),
                "eval_case_repository_invalid",
            ),
            (
                valid.replace("pull_request: 214", "pull_request: 0"),
                "eval_case_pull_request_invalid",
            ),
            (
                valid.replace("base_branch: main", "base_branch: a..b"),
                "eval_case_base_branch_invalid",
            ),
            (
                valid.replace("0123456789abcdef0123456789abcdef01234567", "abc"),
                "eval_case_sha_invalid",
            ),
            (
                valid.replace(
                    "0123456789abcdef0123456789abcdef01234567",
                    "89abcdef0123456789abcdef0123456789abcdef",
                ),
                "eval_case_sha_equal",
            ),
            (
                valid.replace("[default]", "[a, a]"),
                "eval_case_pipelines_invalid",
            ),
            (
                valid.replace("title: Cache race", "title: \"a\\u0007b\""),
                "eval_case_title_invalid",
            ),
            (
                valid.replace("title: Cache race", "title: Cache race\nnote: x"),
                "eval_case_key_unknown",
            ),
            (
                valid.replace("lines: [40, 52]", "lines: [40, 52]\n      extra: 1"),
                "eval_case_key_unknown",
            ),
            (
                valid.replace("pull_request: 214", "pull_request: abc"),
                "eval_case_yaml_invalid",
            ),
            (
                valid.replace("id: widgets-cache-race", "id: a\nid: b"),
                "eval_case_yaml_invalid",
            ),
            (
                valid.replace("[40, 52]", "[52, 40]"),
                "eval_case_finding_lines_invalid",
            ),
            (
                valid.replace("[40, 52]", "[1, 300]"),
                "eval_case_finding_lines_invalid",
            ),
            (
                valid.replace("[40, 52]", "[40]"),
                "eval_case_finding_lines_invalid",
            ),
            (
                valid.replace("src/cache.rs", "/src/cache.rs"),
                "eval_case_finding_path_invalid",
            ),
            (
                valid.replace("src/cache.rs", "src/../cache.rs"),
                "eval_case_finding_path_invalid",
            ),
            (
                valid.replace("Lock released before the entry is inserted.", "\"  \""),
                "eval_case_finding_description_invalid",
            ),
            (format!("{valid}{ITEM}"), "eval_case_finding_duplicate"),
            (
                valid.replace("[default]", "[]"),
                "eval_case_pipelines_invalid",
            ),
            (HEADER.to_owned(), "eval_case_expect_invalid"),
            (
                format!("{HEADER}expect:\n  blocking: []\n"),
                "eval_case_expect_invalid",
            ),
            (format!("{HEADER}expect: dirty\n"), "eval_case_yaml_invalid"),
            (
                format!("{HEADER}expect:\n  clean: true\n"),
                "eval_case_key_unknown",
            ),
        ];
        for (text, want) in rows {
            let errors = parse(&text).expect_err("the row is invalid");
            assert_eq!(codes(&errors), [want], "{errors:?}");
        }
        // A bad id is also not the file name.
        let errors = parse(&valid.replace("id: widgets-cache-race", "id: Widgets"))
            .expect_err("an upper-case id is invalid");
        assert_eq!(
            codes(&errors),
            ["eval_case_id_invalid", "eval_case_id_mismatch"]
        );
        let dir = tempfile::tempdir().expect("create the cases directory");
        let write = |name: &str, text: &str| {
            std::fs::write(dir.path().join(name), text).expect("write a file")
        };
        write("widgets-cache-race.yaml", &valid);
        write(
            "widgets-copy.yaml",
            &valid.replace("widgets-cache-race", "widgets-copy"),
        );
        write("widgets-old.yml", &valid);
        write("widgets-upper.YML", &valid);
        write("widgets-upper.YAML", &valid);
        write("widgets-big.yaml", &"#".repeat(70_000));
        write("README.md", "ignored");
        write(".gitkeep", "");
        write(".hidden.yml", &valid);
        // A link to a case file in the directory is refused, and so is a dangling one.
        let link = |target: &str, name: &str| {
            std::os::unix::fs::symlink(dir.path().join(target), dir.path().join(name))
                .expect("create a symbolic link")
        };
        link("widgets-cache-race.yaml", "widgets-link.yaml");
        link("missing.yaml", "widgets-dangling.yaml");
        let errors = load_cases(dir.path()).expect_err("the directory has problems");
        let mut found = codes(&errors);
        found.sort_unstable();
        assert_eq!(
            found,
            [
                "eval_case_extension",
                "eval_case_extension",
                "eval_case_id_mismatch",
                "eval_case_identity_duplicate",
                "eval_case_too_large",
                "eval_case_unreadable",
                "eval_case_unreadable"
            ]
        );
    }
}
