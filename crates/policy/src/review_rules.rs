//! The repository review rules of a `REVIEW.md`: YAML front matter and the maintainers' prose.

// Rust guideline compliant 2026-11-17

use serde::{Deserialize, Serialize};

use crate::author_limit::present;
use crate::human_review::valid_glob;

/// Most `skip` globs of one file.
const MAX_SKIP_GLOBS: usize = 100;

/// Most `ci_tools` names of one file.
const MAX_CI_TOOLS: usize = 32;

/// Longest `ci_tools` name, in bytes.
const MAX_CI_TOOL_BYTES: usize = 64;

/// Most `**` segments of one `skip` glob.
const MAX_DOUBLE_STAR_SEGMENTS: usize = 4;

/// Longest quoted value in an error reason, in characters.
const MAX_QUOTED_CHARACTERS: usize = 64;

/// The line that opens and closes the front matter.
const FENCE: &str = "---";

/// The rules a repository's `REVIEW.md` sets, read from its base commit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewRules {
    /// Path globs, relative to the repository root, whose findings become notes.
    pub skip: Vec<String>,
    /// Names of the tools that the repository's CI already enforces.
    pub ci_tools: Vec<String>,
    /// The maintainers' prose: everything after the front matter, trimmed.
    pub text: String,
}

/// A `REVIEW.md` that is not a valid rules file.
///
/// The reason names the key or the line and never carries more than a short quoted value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("REVIEW.md {reason}")]
pub struct ReviewRulesError {
    /// What is wrong, as a short phrase.
    pub reason: String,
}

/// The front matter keys; an unknown key is an error.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FrontMatter {
    /// Path globs whose findings become notes; absent means none, null is refused.
    #[serde(deserialize_with = "present")]
    skip: Option<Option<Vec<String>>>,
    /// Tools that CI enforces; absent means none, null is refused.
    #[serde(deserialize_with = "present")]
    ci_tools: Option<Option<Vec<String>>>,
}

/// Returns the error for `reason`.
fn invalid(reason: impl Into<String>) -> ReviewRulesError {
    ReviewRulesError {
        reason: reason.into(),
    }
}

/// Returns `value` cut to a length that an error reason may quote.
fn quoted(value: &str) -> String {
    value.chars().take(MAX_QUOTED_CHARACTERS).collect()
}

/// Parses the text of a `REVIEW.md`.
///
/// A file that does not start with a `---` line is all prose. Otherwise the lines up to the next
/// `---` line are the front matter and the rest is the prose. A leading byte order mark and
/// CRLF line ends are accepted.
///
/// # Errors
///
/// Returns an error for a NUL character, a front matter that is not closed or not YAML of the
/// two known keys, a null list, more than 100 `skip` globs or 32 `ci_tools` names, a glob that
/// is not a relative path glob or has more than four `**` segments, a name outside
/// `[A-Za-z0-9 ._+/-]` of 1 to 64 bytes, or a repeated entry.
pub fn parse_review_rules(raw: &str) -> Result<ReviewRules, ReviewRulesError> {
    if raw.contains('\0') {
        return Err(invalid("contains a NUL character"));
    }
    let text = raw
        .strip_prefix('\u{feff}')
        .unwrap_or(raw)
        .replace("\r\n", "\n");
    let Some(after_fence) = text.strip_prefix(&format!("{FENCE}\n")) else {
        if text == FENCE {
            return Err(invalid("front matter is not closed"));
        }
        return Ok(ReviewRules {
            text: text.trim().to_owned(),
            ..ReviewRules::default()
        });
    };
    let mut offset = 0;
    let mut closed = None;
    for line in after_fence.split_inclusive('\n') {
        if line.trim_end_matches('\n') == FENCE {
            closed = Some((offset, offset + line.len()));
            break;
        }
        offset += line.len();
    }
    let Some((front_end, body_start)) = closed else {
        return Err(invalid("front matter is not closed"));
    };
    let front = after_fence.get(..front_end).unwrap_or_default();
    let body = after_fence.get(body_start..).unwrap_or_default();
    let matter: FrontMatter = if front.trim().is_empty() {
        FrontMatter::default()
    } else {
        serde_saphyr::from_str(front)
            .map_err(|error| invalid(format!("front matter: {}", quoted(&error.to_string()))))?
    };
    Ok(ReviewRules {
        skip: list("skip", matter.skip, MAX_SKIP_GLOBS, check_skip_glob)?,
        ci_tools: list("ci_tools", matter.ci_tools, MAX_CI_TOOLS, check_tool)?,
        text: body.trim().to_owned(),
    })
}

/// Checks one list: present and not null, at most `max` entries, each valid and listed once.
fn list(
    key: &str,
    value: Option<Option<Vec<String>>>,
    max: usize,
    check: fn(&str) -> Result<(), String>,
) -> Result<Vec<String>, ReviewRulesError> {
    let entries = match value {
        None => return Ok(Vec::new()),
        Some(None) => return Err(invalid(format!("front matter: {key} must be a list"))),
        Some(Some(entries)) => entries,
    };
    if entries.len() > max {
        return Err(invalid(format!(
            "front matter: {key} has more than {max} entries"
        )));
    }
    let mut checked: Vec<String> = Vec::new();
    for entry in entries {
        if let Err(reason) = check(&entry) {
            let shown = quoted(&entry);
            return Err(invalid(format!(
                "front matter: {key} entry {shown:?} {reason}"
            )));
        }
        if checked.contains(&entry) {
            let shown = quoted(&entry);
            return Err(invalid(format!(
                "front matter: {key} lists {shown:?} twice"
            )));
        }
        checked.push(entry);
    }
    Ok(checked)
}

/// Checks a `skip` glob: a relative path glob with at most four `**` segments.
fn check_skip_glob(glob: &str) -> Result<(), String> {
    if !valid_glob(glob) {
        return Err("is not valid".to_owned());
    }
    if glob.split('/').filter(|segment| *segment == "**").count() > MAX_DOUBLE_STAR_SEGMENTS {
        return Err(format!(
            "has more than {MAX_DOUBLE_STAR_SEGMENTS} ** segments"
        ));
    }
    Ok(())
}

/// Checks a `ci_tools` name: 1 to 64 bytes of letters, digits, space, `.`, `_`, `+`, `-` or `/`.
fn check_tool(name: &str) -> Result<(), String> {
    let valid = !name.is_empty()
        && name.len() <= MAX_CI_TOOL_BYTES
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b" ._+-/".contains(&byte));
    if valid {
        Ok(())
    } else {
        Err("is not valid".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::{ReviewRules, parse_review_rules};

    /// Returns the rules of `skip`, `ci_tools` and `text`.
    fn rules(skip: &[&str], ci_tools: &[&str], text: &str) -> ReviewRules {
        ReviewRules {
            skip: skip.iter().map(|glob| (*glob).to_owned()).collect(),
            ci_tools: ci_tools.iter().map(|tool| (*tool).to_owned()).collect(),
            text: text.to_owned(),
        }
    }

    /// Each row is a file and its rules, or a part of the reason that refuses it.
    #[test]
    fn review_rules_parse_and_every_invalid_file_names_its_reason() {
        let many_globs = format!("---\nskip: [{}]\n---\n", vec!["a"; 101].join(", "));
        let many_tools = (0..33)
            .map(|n| format!("t{n}"))
            .collect::<Vec<_>>()
            .join(", ");
        let cases: Vec<(&str, String, Result<ReviewRules, &str>)> = vec![
            (
                "front matter and prose",
                "---\nskip:\n  - \"docs/archive/**\"\n  - \"**/CHANGELOG.md\"\nci_tools: [rustfmt, clippy]\n---\nBe strict.\n".to_owned(),
                Ok(rules(&["docs/archive/**", "**/CHANGELOG.md"], &["rustfmt", "clippy"], "Be strict.")),
            ),
            ("no front matter", "Plain guidance.\n".to_owned(), Ok(rules(&[], &[], "Plain guidance."))),
            ("empty file", String::new(), Ok(rules(&[], &[], ""))),
            ("empty front matter and body", "---\n---\n".to_owned(), Ok(rules(&[], &[], ""))),
            ("closing fence at the end", "---\nskip: [a]\n---".to_owned(), Ok(rules(&["a"], &[], ""))),
            (
                "byte order mark and CRLF",
                "\u{feff}---\r\nskip: [a/**]\r\n---\r\nText.\r\n".to_owned(),
                Ok(rules(&["a/**"], &[], "Text.")),
            ),
            ("fence later in the prose", "Intro\n---\nskip: [a]\n---\n".to_owned(), Ok(rules(&[], &[], "Intro\n---\nskip: [a]\n---"))),
            ("empty lists", "---\nskip: []\nci_tools: []\n---\n".to_owned(), Ok(rules(&[], &[], ""))),
            ("not closed", "---\nskip: [a]\n".to_owned(), Err("front matter is not closed")),
            ("only an opening fence", "---".to_owned(), Err("front matter is not closed")),
            ("NUL", "a\0b".to_owned(), Err("NUL")),
            ("YAML error", "---\nskip: [a\n---\n".to_owned(), Err("front matter:")),
            ("unknown key", "---\nskp: [a]\n---\n".to_owned(), Err("skp")),
            ("wrong type", "---\nskip: docs\n---\n".to_owned(), Err("front matter:")),
            ("null skip", "---\nskip: null\n---\n".to_owned(), Err("skip must be a list")),
            ("null ci_tools", "---\nci_tools:\n---\n".to_owned(), Err("ci_tools must be a list")),
            ("absolute glob", "---\nskip: [/docs/**]\n---\n".to_owned(), Err("skip entry \"/docs/**\"")),
            ("parent segment", "---\nskip: [../docs]\n---\n".to_owned(), Err("skip entry")),
            ("partial double star", "---\nskip: [\"a/**.rs\"]\n---\n".to_owned(), Err("skip entry")),
            ("backslash glob", "---\nskip: ['a\\b']\n---\n".to_owned(), Err("skip entry")),
            ("empty glob", "---\nskip: [\"\"]\n---\n".to_owned(), Err("skip entry")),
            ("repeated glob", "---\nskip: [a, a]\n---\n".to_owned(), Err("skip lists \"a\" twice")),
            ("four double stars", "---\nskip: [\"**/**/**/**/z\"]\n---\n".to_owned(), Ok(rules(&["**/**/**/**/z"], &[], ""))),
            ("five double stars", "---\nskip: [\"**/a/**/**/**/**\"]\n---\n".to_owned(), Err("skip entry \"**/a/**/**/**/**\" has more than 4 ** segments")),
            ("too many globs", many_globs, Err("skip has more than 100 entries")),
            ("too many tools", format!("---\nci_tools: [{many_tools}]\n---\n"), Err("ci_tools has more than 32 entries")),
            ("tool with a quote", "---\nci_tools: ['a\"b']\n---\n".to_owned(), Err("ci_tools entry")),
            ("long tool name", format!("---\nci_tools: [{}]\n---\n", "t".repeat(65)), Err("ci_tools entry")),
            ("repeated tool", "---\nci_tools: [clippy, clippy]\n---\n".to_owned(), Err("ci_tools lists \"clippy\" twice")),
        ];
        for (name, text, want) in cases {
            let got = parse_review_rules(&text);
            match (got, want) {
                (Ok(got), Ok(want)) => assert_eq!(got, want, "{name}"),
                (Err(got), Err(part)) => {
                    assert!(got.to_string().starts_with("REVIEW.md "), "{name}: {got}");
                    assert!(got.to_string().contains(part), "{name}: {got}");
                }
                (got, want) => panic!("{name}: got {got:?}, want {want:?}"),
            }
        }
    }
}
