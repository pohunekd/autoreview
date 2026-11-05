//! Neutralization of model output before it is published to GitHub.
//!
//! Model Markdown keeps its formatting, code included. Outside code it cannot mention users,
//! embed HTML or load images, and no comment delimiter survives anywhere, so model output
//! can neither hide content nor imitate the review marker.

// Rust guideline compliant 2026-06-26

/// Neutralizes model Markdown so that GitHub cannot mention, embed or fetch from it.
#[cfg_attr(not(test), expect(dead_code, reason = "called by S12"))]
pub(crate) fn neutralize(value: &str) -> String {
    let mut lines: Vec<String> = value
        .split('\n')
        .map(std::string::ToString::to_string)
        .collect();
    let mut fence: Option<String> = None;
    for line in &mut lines {
        let line_copy = line.clone();
        let indent = line_copy.chars().take_while(|c| *c == ' ').count();
        let trimmed = if indent <= line_copy.len() {
            &line_copy[indent..]
        } else {
            ""
        };
        if let Some(current) = fence.clone() {
            if indent <= 3 && closes_fence(trimmed, &current) {
                fence = None;
            }
            continue;
        }
        if indent <= 3
            && let Some(marker) = opening_fence(trimmed)
        {
            fence = Some(marker.to_string());
            continue;
        }
        *line = neutralize_inline(&line_copy);
    }
    let joined = lines.join("\n");
    defuse_comment_delimiters(&joined)
}

/// Returns the opening fence: three or more backticks or tildes at the line start.
fn opening_fence(line: &str) -> Option<&str> {
    for &character in b"`~" {
        let mut run = 0;
        let bytes = line.as_bytes();
        while run < bytes.len() && bytes[run] == character {
            run += 1;
        }
        if run >= 3 {
            if character == b'`' && line[run..].contains('`') {
                return None;
            }
            return Some(&line[..run]);
        }
    }
    None
}

/// Reports a closing fence line of at least the opener's length, then whitespace only.
fn closes_fence(line: &str, fence: &str) -> bool {
    let first = fence.as_bytes().first().copied().unwrap_or(b'`');
    let mut run = 0;
    let bytes = line.as_bytes();
    while run < bytes.len() && bytes[run] == first {
        run += 1;
    }
    if run < fence.len() {
        return false;
    }
    line[run..].trim().is_empty()
}

/// Defuses mentions, HTML and images outside the code spans of one line.
fn neutralize_inline(line: &str) -> String {
    let mut out = String::new();
    let bytes = line.as_bytes();
    let mut index = 0;
    let mut copied = 0;
    while index < bytes.len() {
        if bytes[index] == b'`' {
            let mut run = 1;
            while index + run < bytes.len() && bytes[index + run] == b'`' {
                run += 1;
            }
            let delimiter = &line[index..index + run];
            let after = &line[index + run..];
            if let Some(closing) = after.find(delimiter) {
                index = index + run + closing + run;
                continue;
            }
            index += run;
            continue;
        }
        if bytes[index] == b'@'
            && bytes.get(index + 1).is_some_and(|b| is_mention_start(*b))
            && (index == 0
                || index
                    .checked_sub(1)
                    .and_then(|previous| bytes.get(previous))
                    .is_none_or(|b| !is_word_byte(*b)))
        {
            out.push_str(&line[copied..index]);
            out.push_str("@\u{200b}");
            copied = index + 1;
            index += 1;
            continue;
        }
        if bytes[index] == b'<' {
            out.push_str(&line[copied..index]);
            out.push_str("&lt;");
            copied = index + 1;
            index += 1;
            continue;
        }
        if bytes[index] == b'!' && bytes.get(index + 1).is_some_and(|b| *b == b'[') {
            out.push_str(&line[copied..index]);
            out.push_str("!\\");
            copied = index + 1;
            index += 1;
            continue;
        }
        index += 1;
    }
    out.push_str(&line[copied..]);
    out
}

/// Replaces `<!--` and `-->` in one left-to-right pass.
///
/// Sequential replacement would change `<!-->` differently, because the two delimiters overlap.
fn defuse_comment_delimiters(text: &str) -> String {
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

/// Reports a byte that may start a mention: an ASCII letter or digit.
fn is_mention_start(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
}

/// Reports a byte that makes a preceding `@` part of a word.
fn is_word_byte(byte: u8) -> bool {
    is_mention_start(byte) || byte == b'_' || byte == b'.' || byte == b'-'
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::neutralize;

    /// Go-written neutralized text, shared with the Go reference tests.
    const GOLDEN: &str = include_str!("../../../../testdata/parity/neutralize.json");

    /// One golden case: a named input and the Go output.
    #[derive(Debug, Deserialize)]
    struct Case {
        name: String,
        input: String,
        output: String,
    }

    #[test]
    fn neutralize_follows_github_fences_and_blocks_images_and_html() {
        let cases = [
            ("    ```\n@victim", "    ```\n@\u{200b}victim"),
            ("```a`b\n@victim", "```a`b\n@\u{200b}victim"),
            (
                "```go\nx := @y\n```\n@after",
                "```go\nx := @y\n```\n@\u{200b}after",
            ),
            (
                "~~~~\n@kept\n~~~\n@kept too\n~~~~",
                "~~~~\n@kept\n~~~\n@kept too\n~~~~",
            ),
            (
                "see ![x](https://example.org/a.png)",
                "see !\\[x](https://example.org/a.png)",
            ),
            ("<img src=x> and `<b>`", "&lt;img src=x> and `<b>`"),
        ];
        for (input, want) in cases {
            assert_eq!(neutralize(input), want, "neutralize({input:?})");
        }
    }

    #[test]
    fn neutralize_matches_go_golden() {
        let cases: Vec<Case> = serde_json::from_str(GOLDEN).expect("golden is JSON");
        assert_eq!(cases.len(), 13, "golden has the thirteen neutralize cases");
        for case in &cases {
            assert_eq!(neutralize(&case.input), case.output, "case {}", case.name);
        }
    }
}
