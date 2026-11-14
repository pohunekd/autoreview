//! Chat-template and role tokens that mark turns of a model conversation.

// Rust guideline compliant 2026-06-26

/// Fixed tokens of common chat templates, matched with their case.
const TOKENS: [&str; 6] = [
    "[INST]",
    "[/INST]",
    "<<SYS>>",
    "<</SYS>>",
    "<start_of_turn>",
    "<end_of_turn>",
];

/// Role prefixes that start a line of a transcript-style prompt, matched with their case.
const ROLE_LINES: [&str; 2] = ["Human:", "Assistant:"];

/// Most name characters of a special token `<|name|>`.
const SPECIAL_NAME_MAX: usize = 32;

/// Most spaces before a role prefix, as Markdown allows before a paragraph.
const ROLE_INDENT_MAX: usize = 3;

/// Counts the chat-template and role tokens of `text`.
///
/// A special token is `<|`, 1 to `SPECIAL_NAME_MAX` ASCII letters, digits or `_`, and `|>`.
/// A role token is a line that starts with a `ROLE_LINES` prefix after at most three spaces.
pub(crate) fn count(text: &str) -> u32 {
    let mut found = 0_u32;
    for (start, _) in text.match_indices("<|") {
        let name = text
            .get(start.saturating_add(2)..)
            .unwrap_or_default()
            .bytes()
            .take(SPECIAL_NAME_MAX.saturating_add(1))
            .take_while(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
            .count();
        let close = start.saturating_add(2).saturating_add(name);
        if (1..=SPECIAL_NAME_MAX).contains(&name)
            && text.get(close..).is_some_and(|rest| rest.starts_with("|>"))
        {
            found = found.saturating_add(1);
        }
    }
    for token in TOKENS {
        let matches = u32::try_from(text.matches(token).count()).unwrap_or(u32::MAX);
        found = found.saturating_add(matches);
    }
    for line in text.split('\n') {
        let content = line.trim_start_matches(' ');
        let indent = line.len().saturating_sub(content.len());
        if indent <= ROLE_INDENT_MAX && ROLE_LINES.iter().any(|role| content.starts_with(role)) {
            found = found.saturating_add(1);
        }
    }
    found
}
