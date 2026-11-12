//! Neutralization of model output before it is published to GitHub.
//!
//! Model text left raw must stay harmless wherever GitHub reads it as text. One-line fields
//! keep no code: every backtick is escaped. Block text keeps its fenced blocks, written again
//! with this module's own fence, and the code spans whose content is harmless as text. All
//! other text has its mentions, HTML, entities and images defused. `<!--` and `-->` are defused
//! everywhere, code included, so model output cannot hide content or imitate the review marker.

// Rust guideline compliant 2026-11-12

/// Neutralizes model text that keeps no code: a one-line field, or an MCP string.
pub(crate) fn neutralize(value: &str) -> String {
    let lines: Vec<String> = normalized_lines(value)
        .iter()
        .map(|line| neutralize_line(line, false))
        .collect();
    defuse_comment_delimiters(&lines.join("\n"))
}

/// Languages whose fenced blocks GitHub renders in a viewer instead of as code.
const RICH_LANGUAGES: [&str; 5] = ["mermaid", "math", "geojson", "topojson", "stl"];

/// Neutralizes model Markdown that the caller puts where a paragraph may start.
///
/// A fenced block is written again on a line of its own at column 0, after a blank line unless
/// it starts the text, with its closing fence and a backtick fence longer than any backtick run
/// inside, so GitHub reads its content as code. A rich language is dropped from it.
pub(crate) fn neutralize_block(value: &str) -> String {
    let lines = normalized_lines(value);
    let mut out: Vec<String> = Vec::new();
    let mut index = 0;
    while let Some(line) = lines.get(index) {
        index += 1;
        let Some((marker, info)) = opening_fence(line) else {
            out.push(neutralize_line(line, true));
            continue;
        };
        let mut content: Vec<&str> = Vec::new();
        while let Some(next) = lines.get(index) {
            index += 1;
            if closes_fence(next, marker) {
                break;
            }
            content.push(next);
        }
        let runs = content.iter().flat_map(|text| text.split(|c| c != '`'));
        let fence = "`".repeat(runs.map(str::len).max().unwrap_or(0).max(2) + 1);
        if out
            .last()
            .is_none_or(|last| !last.trim_matches([' ', '\t']).is_empty())
        {
            out.push(String::new());
        }
        let language = info.trim();
        let keep = language
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_+.#-".contains(c))
            && !RICH_LANGUAGES.contains(&language.to_ascii_lowercase().as_str());
        out.push(format!("{fence}{}", if keep { language } else { "" }));
        out.extend(content.iter().map(|text| (*text).to_owned()));
        out.push(fence);
        if index < lines.len() {
            out.push(String::new());
        }
    }
    defuse_comment_delimiters(&out.join("\n"))
}

/// Splits model text into lines; GitHub also ends a line at a lone `\r`.
fn normalized_lines(value: &str) -> Vec<String> {
    let normalized = value.replace("\r\n", "\n").replace('\r', "\n");
    normalized.split('\n').map(str::to_owned).collect()
}

/// Returns the fence and the info string of a line that opens a fenced block in model text.
///
/// Up to three spaces, then three or more backticks or tildes; a backtick fence's info string
/// holds no backtick, as CommonMark requires.
pub(crate) fn opening_fence(line: &str) -> Option<(&str, &str)> {
    let rest = line.trim_start_matches(' ');
    if line.len() - rest.len() > 3 {
        return None;
    }
    let character = rest.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let run = rest.chars().take_while(|c| *c == character).count();
    let (marker, info) = (rest.get(..run)?, rest.get(run..)?);
    (run >= 3 && !(character == '`' && info.contains('`'))).then_some((marker, info))
}

/// Reports whether `line` closes the block that `fence` opened: up to three spaces, at least
/// as many fence characters, then only spaces and tabs.
pub(crate) fn closes_fence(line: &str, fence: &str) -> bool {
    let rest = line.trim_start_matches(' ');
    let character = fence.chars().next().unwrap_or('`');
    let run = rest.chars().take_while(|c| *c == character).count();
    line.len() - rest.len() <= 3
        && run >= fence.len()
        && rest.chars().skip(run).all(|c| c == ' ' || c == '\t')
}

/// Defuses one line of text outside fenced blocks.
///
/// With `keep_spans`, a code span closed on the line by a run of one or two backticks stays
/// raw when its content is harmless as text; otherwise both its runs are escaped. Every other
/// backtick, and the first tilde of a run of three, is escaped. Outside kept spans, `&` and `<`
/// become entities, `@` before a letter or digit gets a zero-width space, `![` loses its image
/// and `[^` its footnote.
fn neutralize_line(line: &str, keep_spans: bool) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut index = 0;
    let mut odd_backslashes = false;
    let mut open_bracket = false;
    let mut closer = None;
    while let Some(&character) = chars.get(index) {
        let escaped = odd_backslashes;
        odd_backslashes = character == '\\' && !escaped;
        let after_bracket = open_bracket;
        open_bracket = character == '[' && !escaped;
        let run = match character {
            '`' | '~' => run_length(&chars, index, character),
            _ => 0,
        };
        if character == '`' || (character == '~' && run >= 3) {
            if closer == Some(index) {
                closer = None;
            } else if character == '`'
                && keep_spans
                && !escaped
                && run < 3
                && let Some(close) = span_close(&chars, index, run)
            {
                let content: String = chars
                    .get(index + run..close)
                    .unwrap_or_default()
                    .iter()
                    .collect();
                if harmless_as_text(&content) {
                    out.extend(chars.get(index..close + run).unwrap_or_default());
                    index = close + run;
                    continue;
                }
                closer = Some(close);
            }
            for position in 0..run {
                if (position == 0 && !escaped) || (position > 0 && character == '`') {
                    out.push('\\');
                }
                out.push(character);
            }
            index += run;
            continue;
        }
        let next = chars.get(index + 1).copied();
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '@' if next.is_some_and(|c| c.is_ascii_alphanumeric()) => out.push_str("@\u{200b}"),
            '!' if next == Some('[') => out.push_str("!\\"),
            '^' if after_bracket => out.push_str("\\^"),
            other => out.push(other),
        }
        index += 1;
    }
    out
}

/// Returns where the span that `run` backticks at `start` open closes: at the next run of
/// exactly `run` backticks on the line.
fn span_close(chars: &[char], start: usize, run: usize) -> Option<usize> {
    let mut index = start + run;
    while chars.get(index).is_some() {
        let closing = run_length(chars, index, '`');
        if closing == run {
            return Some(index);
        }
        index += closing.max(1);
    }
    None
}

/// Reports whether `text` publishes nothing when GitHub reads it as text: no `@`, no `<`, no
/// image opener `![`, and no `&` that starts an entity (a name or number, then `;`).
fn harmless_as_text(text: &str) -> bool {
    let entity = |rest: &str| {
        let name = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '#')
            .count();
        name > 0 && rest.chars().nth(name) == Some(';')
    };
    !text.contains(['@', '<']) && !text.contains("![") && !text.split('&').skip(1).any(entity)
}

/// Counts the run of `character` in `chars` that starts at `start`.
fn run_length(chars: &[char], start: usize, character: char) -> usize {
    chars
        .iter()
        .skip(start)
        .take_while(|c| **c == character)
        .count()
}

/// Replaces `<!--` and `-->` in one left-to-right pass.
///
/// Sequential replacement would change `<!-->` differently, because the two delimiters overlap.
pub(crate) fn defuse_comment_delimiters(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(position) = rest.find(['<', '-']) {
        let (before, from) = rest.split_at(position);
        out.push_str(before);
        let (replacement, used) = if from.starts_with("<!--") {
            ("&lt;!--", 4)
        } else if from.starts_with("-->") {
            ("--&gt;", 3)
        } else {
            (from.get(..1).unwrap_or_default(), 1)
        };
        out.push_str(replacement);
        rest = from.get(used..).unwrap_or_default();
    }
    out.push_str(rest);
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
    /// Rust writes a fenced block again with its own fence between blank lines, keeps a code
    /// span only when its content is harmless as text, escapes every other backtick, and
    /// defuses a mention whatever comes before it.
    const CHANGED: [(&str, &str); 10] = [
        ("indented_fence_is_code", "    \\`\\`\\`\n@\u{200b}victim"),
        (
            "fence_info_with_backtick_is_text",
            "\\`\\`\\`a\\`b\n@\u{200b}victim",
        ),
        (
            "mention_after_closed_fence",
            "\n```go\nx := @y\n```\n\n@\u{200b}after",
        ),
        (
            "tilde_fence_closed_by_tildes_only",
            "\n```\n@kept\n~~~\n@kept too\n```",
        ),
        (
            "html_outside_code_defused",
            "&lt;img src=x> and \\`&lt;b>\\`",
        ),
        (
            "comment_delimiters_inside_fence",
            "\n```\n&lt;!-- hidden --&gt;\n```\n\n&lt;!-- out --&gt;",
        ),
        (
            "address_kept_mention_defused",
            "mail dev@\u{200b}example.com and @\u{200b}octocat",
        ),
        ("at_after_letter_kept", "a@\u{200b}b"),
        ("unclosed_backtick", "a \\`b @\u{200b}octocat"),
        (
            "mention_inside_code_span_kept",
            "\\`@\u{200b}octocat\\` and @\u{200b}octocat",
        ),
    ];

    /// One golden case: a named input and the Go output.
    #[derive(Debug, Deserialize)]
    struct Case {
        name: String,
        input: String,
        output: String,
    }

    /// Each row is named by the Markdown shape it models and holds the input, the output of
    /// `neutralize_block` (Markdown that starts a paragraph) and of `neutralize` (a field that
    /// keeps no code).
    #[test]
    fn neutralize_follows_github_fences_and_blocks_images_and_html() {
        let cases = [
            (
                "mention_after_indented_fence",
                "Code:\n    ```\n@victim",
                "Code:\n    \\`\\`\\`\n@\u{200b}victim",
                "Code:\n    \\`\\`\\`\n@\u{200b}victim",
            ),
            (
                "mention_after_fence_with_backtick_info",
                "Code:\n```a`b\n@victim",
                "Code:\n\\`\\`\\`a\\`b\n@\u{200b}victim",
                "Code:\n\\`\\`\\`a\\`b\n@\u{200b}victim",
            ),
            (
                "mention_after_closed_fence",
                "Code:\n```go\nx := @y\n```\n@after",
                "Code:\n\n```go\nx := @y\n```\n\n@\u{200b}after",
                "Code:\n\\`\\`\\`go\nx := @\u{200b}y\n\\`\\`\\`\n@\u{200b}after",
            ),
            (
                "fence_not_closed_by_shorter_tildes",
                "Code:\n~~~~\n@kept\n~~~\n@kept too\n~~~~",
                "Code:\n\n```\n@kept\n~~~\n@kept too\n```",
                "Code:\n\\~~~~\n@\u{200b}kept\n\\~~~\n@\u{200b}kept too\n\\~~~~",
            ),
            (
                "image_pixel",
                "see ![x](https://example.org/a.png)",
                "see !\\[x](https://example.org/a.png)",
                "see !\\[x](https://example.org/a.png)",
            ),
            (
                "html_image_pixel",
                "<img src=x> and `<b>`",
                "&lt;img src=x> and \\`&lt;b>\\`",
                "&lt;img src=x> and \\`&lt;b>\\`",
            ),
            (
                "escaped_backtick_opens_no_code_span",
                "Note \\`@victim <img src=x>` end",
                "Note \\`@\u{200b}victim &lt;img src=x>\\` end",
                "Note \\`@\u{200b}victim &lt;img src=x>\\` end",
            ),
            (
                "closing_backtick_run_of_another_length",
                "Note `@victim <img src=x>`` end",
                "Note \\`@\u{200b}victim &lt;img src=x>\\`\\` end",
                "Note \\`@\u{200b}victim &lt;img src=x>\\`\\` end",
            ),
            (
                "code_span_across_a_line_break",
                "Note `a\nb` @victim <img src=x> `c`",
                "Note \\`a\nb\\` @\u{200b}victim &lt;img src=x> \\`c\\`",
                "Note \\`a\nb\\` @\u{200b}victim &lt;img src=x> \\`c\\`",
            ),
            (
                "fence_closed_by_the_end_of_a_list_item",
                "- item\n  ```\n@victim <img src=x>\n  ```",
                "- item\n\n```\n@victim <img src=x>\n```",
                "- item\n  \\`\\`\\`\n@\u{200b}victim &lt;img src=x>\n  \\`\\`\\`",
            ),
            (
                "table_cell_split_inside_a_code_span",
                "| a | b |\n|---|---|\n| `x | @victim <img src=x>` |",
                "| a | b |\n|---|---|\n| \\`x | @\u{200b}victim &lt;img src=x>\\` |",
                "| a | b |\n|---|---|\n| \\`x | @\u{200b}victim &lt;img src=x>\\` |",
            ),
            (
                "fence_opener_in_a_one_line_field",
                "```<img src=x> @victim",
                "\n```\n```",
                "\\`\\`\\`&lt;img src=x> @\u{200b}victim",
            ),
            (
                "fence_closed_by_a_lone_carriage_return",
                "Code:\n```\nx\r```\r@victim <img src=x>",
                "Code:\n\n```\nx\n```\n\n@\u{200b}victim &lt;img src=x>",
                "Code:\n\\`\\`\\`\nx\n\\`\\`\\`\n@\u{200b}victim &lt;img src=x>",
            ),
            (
                "fence_closed_by_a_line_with_other_whitespace",
                "Code:\n```\n```\u{a0}\n@x\n```\n@victim",
                "Code:\n\n````\n```\u{a0}\n@x\n````\n\n@\u{200b}victim",
                "Code:\n\\`\\`\\`\n\\`\\`\\`\u{a0}\n@\u{200b}x\n\\`\\`\\`\n@\u{200b}victim",
            ),
            (
                "mention_written_as_an_entity",
                "Hi &#64;victim and &commat;victim, &lt;img src=x>",
                "Hi &amp;#64;victim and &amp;commat;victim, &amp;lt;img src=x>",
                "Hi &amp;#64;victim and &amp;commat;victim, &amp;lt;img src=x>",
            ),
            (
                "mention_after_punctuation_or_an_escape",
                "x.@victim -@victim _@victim \\@victim",
                "x.@\u{200b}victim -@\u{200b}victim _@\u{200b}victim \\@\u{200b}victim",
                "x.@\u{200b}victim -@\u{200b}victim _@\u{200b}victim \\@\u{200b}victim",
            ),
            (
                "mention_after_a_zero_width_space",
                "\u{200b}@victim",
                "\u{200b}@\u{200b}victim",
                "\u{200b}@\u{200b}victim",
            ),
            (
                "tilde_fence_in_a_blockquote",
                "> ~~~\n> @victim",
                "> \\~~~\n> @\u{200b}victim",
                "> \\~~~\n> @\u{200b}victim",
            ),
            (
                "fence_in_a_nested_list",
                "1. a\n   - b\n     ```\n<img src=x>\n     ```",
                "1. a\n   - b\n     \\`\\`\\`\n&lt;img src=x>\n     \\`\\`\\`",
                "1. a\n   - b\n     \\`\\`\\`\n&lt;img src=x>\n     \\`\\`\\`",
            ),
            (
                "image_by_a_link_reference_definition",
                "![p]\n\n[p]: https://example.org/p.png",
                "!\\[p]\n\n[p]: https://example.org/p.png",
                "!\\[p]\n\n[p]: https://example.org/p.png",
            ),
            (
                "double_escaped_backslash_before_a_backtick",
                "a \\\\`<img src=x>",
                "a \\\\\\`&lt;img src=x>",
                "a \\\\\\`&lt;img src=x>",
            ),
            (
                "span_opened_in_a_link_destination",
                "[a](`) @octocat <sub>raw</sub> ![i](https://example.org/p.png) `",
                concat!(
                    "[a](\\`) @\u{200b}octocat &lt;sub>raw&lt;/sub> ",
                    "!\\[i](https://example.org/p.png) \\`"
                ),
                concat!(
                    "[a](\\`) @\u{200b}octocat &lt;sub>raw&lt;/sub> ",
                    "!\\[i](https://example.org/p.png) \\`"
                ),
            ),
            (
                "span_opened_in_a_link_title",
                "[a](/u \"`\") @octocat <sub>raw</sub> `",
                "[a](/u \"\\`\") @\u{200b}octocat &lt;sub>raw&lt;/sub> \\`",
                "[a](/u \"\\`\") @\u{200b}octocat &lt;sub>raw&lt;/sub> \\`",
            ),
            (
                "span_opened_in_a_www_autolink",
                "www.example.org/` @octocat <sub>raw</sub> `",
                "www.example.org/\\` @\u{200b}octocat &lt;sub>raw&lt;/sub> \\`",
                "www.example.org/\\` @\u{200b}octocat &lt;sub>raw&lt;/sub> \\`",
            ),
            (
                "span_opened_in_a_url_autolink",
                "https://example.org/` @octocat <sub>raw</sub> `",
                "https://example.org/\\` @\u{200b}octocat &lt;sub>raw&lt;/sub> \\`",
                "https://example.org/\\` @\u{200b}octocat &lt;sub>raw&lt;/sub> \\`",
            ),
            (
                "span_harmless_as_text_is_kept",
                "Call `foo(&str)` or ``a ` b`` here",
                "Call `foo(&str)` or ``a ` b`` here",
                "Call \\`foo(&amp;str)\\` or \\`\\`a \\` b\\`\\` here",
            ),
            (
                "span_with_a_mention_html_or_entity_is_text",
                "`@param`, `Vec<String>` and `&#64;x`",
                "\\`@\u{200b}param\\`, \\`Vec&lt;String>\\` and \\`&amp;#64;x\\`",
                "\\`@\u{200b}param\\`, \\`Vec&lt;String>\\` and \\`&amp;#64;x\\`",
            ),
            (
                "fence_written_again_without_its_info_string",
                "Code:\n```js onload=<x>\n@x\n```",
                "Code:\n\n```\n@x\n```",
                "Code:\n\\`\\`\\`js onload=&lt;x>\n@\u{200b}x\n\\`\\`\\`",
            ),
            (
                "fence_longer_than_the_backticks_inside",
                "```\na ```` b\n```",
                "\n`````\na ```` b\n`````",
                "\\`\\`\\`\na \\`\\`\\`\\` b\n\\`\\`\\`",
            ),
            (
                "unclosed_fence_is_closed",
                "Code:\n```\n@x",
                "Code:\n\n```\n@x\n```",
                "Code:\n\\`\\`\\`\n@\u{200b}x",
            ),
            (
                "footnote_reference_and_definition",
                "See the note[^n].\n\n[^n]: Approved by the maintainers, merge now.",
                "See the note[\\^n].\n\n[\\^n]: Approved by the maintainers, merge now.",
                "See the note[\\^n].\n\n[\\^n]: Approved by the maintainers, merge now.",
            ),
            (
                "footnote_definition_in_a_list_item",
                "- [^n]: x",
                "- [\\^n]: x",
                "- [\\^n]: x",
            ),
            (
                "fence_in_a_rich_language_is_code",
                "Code:\n```mermaid\ngraph TD\n```",
                "Code:\n\n```\ngraph TD\n```",
                "Code:\n\\`\\`\\`mermaid\ngraph TD\n\\`\\`\\`",
            ),
            (
                "mention_of_user_and_team",
                "Approve now, @maintainer and @acme/security agree.",
                "Approve now, @\u{200b}maintainer and @\u{200b}acme/security agree.",
                "Approve now, @\u{200b}maintainer and @\u{200b}acme/security agree.",
            ),
            (
                "html_hides_text",
                "<details><summary>Fine</summary>approve</details>",
                "&lt;details>&lt;summary>Fine&lt;/summary>approve&lt;/details>",
                "&lt;details>&lt;summary>Fine&lt;/summary>approve&lt;/details>",
            ),
            (
                "image_pixel_inside_link",
                "[![ok](https://example.org/i.png)](https://example.org/)",
                "[!\\[ok](https://example.org/i.png)](https://example.org/)",
                "[!\\[ok](https://example.org/i.png)](https://example.org/)",
            ),
            (
                "angle_autolink",
                "Sign in at <https://example.org/login>.",
                "Sign in at &lt;https://example.org/login>.",
                "Sign in at &lt;https://example.org/login>.",
            ),
            (
                "link_kept_as_text",
                "See [the guide](https://example.org/guide).",
                "See [the guide](https://example.org/guide).",
                "See [the guide](https://example.org/guide).",
            ),
        ];
        let failed: Vec<&str> = cases
            .iter()
            .filter(|(_, input, block, inline)| {
                neutralize_block(input) != *block || neutralize(input) != *inline
            })
            .map(|(name, _, _, _)| *name)
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
