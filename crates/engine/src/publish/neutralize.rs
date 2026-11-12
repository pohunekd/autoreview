//! Neutralization of model output before it is published to GitHub.
//!
//! Model Markdown keeps its code: fenced blocks that open at the start of a line, and code
//! spans closed on their own line. Everything else is text. In text, mentions, HTML, entities
//! and images are defused, and every backtick or tilde run that could open code is escaped, so
//! GitHub finds code exactly where this module keeps it. No comment delimiter survives
//! anywhere, so model output can neither hide content nor imitate the review marker.

// Rust guideline compliant 2026-11-12

/// Neutralizes model Markdown that may follow other text on its first line.
///
/// The first line never opens a code block, because it may continue a line of the review.
pub(crate) fn neutralize(value: &str) -> String {
    neutralize_text(value, false)
}

/// Neutralizes model Markdown that the caller places at the start of a Markdown block.
///
/// Its first line may open a code block, so the caller must put a blank line before it.
pub(crate) fn neutralize_block(value: &str) -> String {
    neutralize_text(value, true)
}

/// Neutralizes model Markdown line by line; `block_start` lets the first line open a fence.
///
/// Line endings become `\n` first, because GitHub also ends a line at a lone `\r`.
fn neutralize_text(value: &str, block_start: bool) -> String {
    let normalized = value.replace("\r\n", "\n").replace('\r', "\n");
    let mut fence: Option<&str> = None;
    let mut lines: Vec<String> = Vec::new();
    for (index, line) in normalized.split('\n').enumerate() {
        if let Some(current) = fence {
            if closes_fence(line, current) {
                fence = None;
            }
            lines.push(line.to_owned());
            continue;
        }
        if index > 0 || block_start {
            fence = opening_fence(line);
            if fence.is_some() {
                lines.push(line.to_owned());
                continue;
            }
        }
        lines.push(neutralize_line(line));
    }
    defuse_comment_delimiters(&lines.join("\n"))
}

/// Returns the fence that opens a code block on `line`: three or more backticks or tildes.
///
/// The fence must start the line. An indented fence may belong to a list item, whose end
/// would close the block where this module cannot see it, so it is text.
pub(crate) fn opening_fence(line: &str) -> Option<&str> {
    for character in ['`', '~'] {
        let run = run_length(line, character);
        if run >= 3 {
            let marker = line.get(..run)?;
            let info = line.get(run..).unwrap_or_default();
            if character == '`' && info.contains('`') {
                return None;
            }
            return Some(marker);
        }
    }
    None
}

/// Reports whether `line` closes the block that `fence` opened, as CommonMark closes it.
///
/// Up to three spaces, at least as many fence characters, then only spaces and tabs.
pub(crate) fn closes_fence(line: &str, fence: &str) -> bool {
    let rest = line.trim_start_matches(' ');
    if line.len() - rest.len() > 3 {
        return false;
    }
    let character = fence.chars().next().unwrap_or('`');
    let run = run_length(rest, character);
    run >= fence.len()
        && rest
            .get(run..)
            .is_some_and(|tail| tail.chars().all(|c| c == ' ' || c == '\t'))
}

/// Defuses one line of text that is not inside a code block.
///
/// A code span is kept when a run of the same number of backticks closes it on the same line
/// and it holds no `|`, which would split it in a table row. Every other backtick, and the
/// first tilde of a run of three, is escaped. Outside the kept spans, `&` and `<` become
/// entities, `@` before a letter or digit gets a zero-width space and `![` loses its image.
fn neutralize_line(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut index = 0;
    let mut backslashes = 0;
    while let Some(&character) = chars.get(index) {
        let escaped = backslashes % 2 == 1;
        backslashes = 0;
        if character == '`' {
            let run = run_length_at(&chars, index, '`');
            if !escaped && let Some(end) = span_end(&chars, index, run) {
                out.extend(chars.get(index..end).unwrap_or_default());
                index = end;
                continue;
            }
            for position in 0..run {
                if position > 0 || !escaped {
                    out.push('\\');
                }
                out.push('`');
            }
            index += run;
            continue;
        }
        if character == '~' && run_length_at(&chars, index, '~') >= 3 {
            let run = run_length_at(&chars, index, '~');
            if !escaped {
                out.push('\\');
            }
            out.extend(std::iter::repeat_n('~', run));
            index += run;
            continue;
        }
        let next = chars.get(index + 1).copied();
        match character {
            '\\' => {
                out.push('\\');
                backslashes = if escaped { 0 } else { 1 };
            }
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '@' if next.is_some_and(|c| c.is_ascii_alphanumeric()) => out.push_str("@\u{200b}"),
            '!' if next == Some('[') => out.push_str("!\\"),
            other => out.push(other),
        }
        index += 1;
    }
    out
}

/// Returns the end of the code span that the run of `run` backticks at `start` opens.
///
/// The span ends after the next run of exactly `run` backticks on the line. A span without
/// one, or with a `|` inside, is not kept.
fn span_end(chars: &[char], start: usize, run: usize) -> Option<usize> {
    let mut index = start + run;
    while let Some(&character) = chars.get(index) {
        if character != '`' {
            index += 1;
            continue;
        }
        let closing = run_length_at(chars, index, '`');
        if closing == run {
            let content = chars.get(start + run..index)?;
            return (!content.contains(&'|')).then_some(index + closing);
        }
        index += closing;
    }
    None
}

/// Counts the run of `character` at the start of `text`.
fn run_length(text: &str, character: char) -> usize {
    text.chars().take_while(|c| *c == character).count()
}

/// Counts the run of `character` in `chars` that starts at `start`.
fn run_length_at(chars: &[char], start: usize, character: char) -> usize {
    chars
        .get(start..)
        .unwrap_or_default()
        .iter()
        .take_while(|c| **c == character)
        .count()
}

/// Replaces `<!--` and `-->` in one left-to-right pass.
///
/// Sequential replacement would change `<!-->` differently, because the two delimiters overlap.
pub(crate) fn defuse_comment_delimiters(text: &str) -> String {
    let mut out = String::new();
    let bytes = text.as_bytes();
    let mut index = 0;
    let mut copied = 0;
    while index < bytes.len() {
        if bytes[index..].starts_with(b"<!--") {
            out.push_str(&text[copied..index]);
            out.push_str("&lt;!--");
            index += 4;
            copied = index;
            continue;
        }
        if bytes[index..].starts_with(b"-->") {
            out.push_str(&text[copied..index]);
            out.push_str("--&gt;");
            index += 3;
            copied = index;
            continue;
        }
        let byte = bytes[index];
        let char_len = if byte & 0x80 == 0 {
            1
        } else if byte & 0xE0 == 0xC0 {
            2
        } else if byte & 0xF0 == 0xE0 {
            3
        } else if byte & 0xF8 == 0xF0 {
            4
        } else {
            1
        };
        index += char_len;
    }
    out.push_str(&text[copied..]);
    out
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::{neutralize, neutralize_block};

    /// Go-written neutralized text, shared with the Go reference tests.
    const GOLDEN: &str = include_str!("../../../../testdata/parity/neutralize.json");

    /// Golden cases whose Rust output differs from Go on purpose, with the Rust output.
    ///
    /// Rust escapes every backtick that GitHub could read as code where Rust reads text, and
    /// defuses a mention whatever comes before it.
    const CHANGED: [(&str, &str); 5] = [
        ("indented_fence_is_code", "    \\`\\`\\`\n@\u{200b}victim"),
        (
            "fence_info_with_backtick_is_text",
            "\\`\\`\\`a\\`b\n@\u{200b}victim",
        ),
        (
            "address_kept_mention_defused",
            "mail dev@\u{200b}example.com and @\u{200b}octocat",
        ),
        ("at_after_letter_kept", "a@\u{200b}b"),
        ("unclosed_backtick", "a \\`b @\u{200b}octocat"),
    ];

    /// One golden case: a named input and the Go output.
    #[derive(Debug, Deserialize)]
    struct Case {
        name: String,
        input: String,
        output: String,
    }

    /// Each row is named by the Markdown shape it models. A row that opens a fence starts
    /// with a line of text, as a fence never opens on the first line of a field.
    #[test]
    fn neutralize_follows_github_fences_and_blocks_images_and_html() {
        let cases = [
            (
                "mention_after_indented_fence",
                "Code:\n    ```\n@victim",
                "Code:\n    \\`\\`\\`\n@\u{200b}victim",
            ),
            (
                "mention_after_fence_with_backtick_info",
                "Code:\n```a`b\n@victim",
                "Code:\n\\`\\`\\`a\\`b\n@\u{200b}victim",
            ),
            (
                "mention_after_closed_fence",
                "Code:\n```go\nx := @y\n```\n@after",
                "Code:\n```go\nx := @y\n```\n@\u{200b}after",
            ),
            (
                "fence_not_closed_by_shorter_tildes",
                "Code:\n~~~~\n@kept\n~~~\n@kept too\n~~~~",
                "Code:\n~~~~\n@kept\n~~~\n@kept too\n~~~~",
            ),
            (
                "image_pixel",
                "see ![x](https://example.org/a.png)",
                "see !\\[x](https://example.org/a.png)",
            ),
            (
                "html_image_pixel",
                "<img src=x> and `<b>`",
                "&lt;img src=x> and `<b>`",
            ),
            (
                "escaped_backtick_opens_no_code_span",
                "Note \\`@victim <img src=x>` end",
                "Note \\`@\u{200b}victim &lt;img src=x>\\` end",
            ),
            (
                "closing_backtick_run_of_another_length",
                "Note `@victim <img src=x>`` end",
                "Note \\`@\u{200b}victim &lt;img src=x>\\`\\` end",
            ),
            (
                "code_span_across_a_line_break",
                "Note `a\nb` @victim <img src=x> `c`",
                "Note \\`a\nb` @victim <img src=x> `c\\`",
            ),
            (
                "fence_closed_by_the_end_of_a_list_item",
                "- item\n  ```\n@victim <img src=x>\n  ```",
                "- item\n  \\`\\`\\`\n@\u{200b}victim &lt;img src=x>\n  \\`\\`\\`",
            ),
            (
                "table_cell_split_inside_a_code_span",
                "| a | b |\n|---|---|\n| `x | @victim <img src=x>` |",
                "| a | b |\n|---|---|\n| \\`x | @\u{200b}victim &lt;img src=x>\\` |",
            ),
            (
                "fence_opener_in_a_one_line_field",
                "```<img src=x> @victim",
                "\\`\\`\\`&lt;img src=x> @\u{200b}victim",
            ),
            (
                "fence_closed_by_a_lone_carriage_return",
                "Code:\n```\nx\r```\r@victim <img src=x>",
                "Code:\n```\nx\n```\n@\u{200b}victim &lt;img src=x>",
            ),
            (
                "fence_closed_by_a_line_with_other_whitespace",
                "Code:\n```\n```\u{a0}\n@x\n```\n@victim",
                "Code:\n```\n```\u{a0}\n@x\n```\n@\u{200b}victim",
            ),
            (
                "mention_written_as_an_entity",
                "Hi &#64;victim and &commat;victim, &lt;img src=x>",
                "Hi &amp;#64;victim and &amp;commat;victim, &amp;lt;img src=x>",
            ),
            (
                "mention_after_punctuation_or_an_escape",
                "x.@victim -@victim _@victim \\@victim",
                "x.@\u{200b}victim -@\u{200b}victim _@\u{200b}victim \\@\u{200b}victim",
            ),
            (
                "mention_after_a_zero_width_space",
                "\u{200b}@victim",
                "\u{200b}@\u{200b}victim",
            ),
            (
                "tilde_fence_in_a_blockquote",
                "> ~~~\n> @victim",
                "> \\~~~\n> @\u{200b}victim",
            ),
            (
                "fence_in_a_nested_list",
                "1. a\n   - b\n     ```\n<img src=x>\n     ```",
                "1. a\n   - b\n     \\`\\`\\`\n&lt;img src=x>\n     \\`\\`\\`",
            ),
            (
                "image_by_a_link_reference_definition",
                "![p]\n\n[p]: https://example.org/p.png",
                "!\\[p]\n\n[p]: https://example.org/p.png",
            ),
            (
                "double_escaped_backslash_before_a_backtick",
                "a \\\\`<img src=x>",
                "a \\\\\\`&lt;img src=x>",
            ),
        ];
        let failed: Vec<&str> = cases
            .iter()
            .filter(|(_, input, want)| neutralize(input) != *want)
            .map(|(name, _, _)| *name)
            .collect();
        assert!(failed.is_empty(), "neutralized differently: {failed:?}");
    }

    #[test]
    fn neutralize_matches_go_golden() {
        let cases: Vec<Case> = serde_json::from_str(GOLDEN).expect("golden is JSON");
        assert_eq!(cases.len(), 13, "golden has the thirteen neutralize cases");
        for (name, _) in CHANGED {
            assert!(
                cases.iter().any(|case| case.name == name),
                "{name} is a golden case"
            );
        }
        for case in &cases {
            let want = CHANGED
                .iter()
                .find(|(name, _)| *name == case.name)
                .map_or(case.output.as_str(), |(_, output)| *output);
            assert_eq!(neutralize_block(&case.input), want, "case {}", case.name);
        }
    }
}
