//! Chat-template and role tokens that mark turns of a model conversation.

// Rust guideline compliant 2026-06-26

use std::borrow::Cow;

use crate::characters;

/// Fixed tokens of common chat templates, lowercase; matched ignoring ASCII case.
const TOKENS: [&str; 6] = [
    "[inst]",
    "[/inst]",
    "<<sys>>",
    "<</sys>>",
    "<start_of_turn>",
    "<end_of_turn>",
];

/// Role prefixes that start a line of a transcript-style prompt, matched with their case.
const ROLE_LINES: [&str; 2] = ["Human:", "Assistant:"];

/// Most name characters of a special token `<|name|>`.
const SPECIAL_NAME_MAX: usize = 32;

/// Opening of a fullwidth special token (DeepSeek style): `<` and U+FF5C.
const FULLWIDTH_OPEN: &str = "<\u{FF5C}";

/// Closing of a fullwidth special token: U+FF5C and `>`.
const FULLWIDTH_CLOSE: &str = "\u{FF5C}>";

/// Word separator allowed in a fullwidth special token name, LOWER ONE EIGHTH BLOCK.
const NAME_SPACE: char = '\u{2581}';

/// Most spaces before a role prefix, as Markdown allows before a paragraph.
const ROLE_INDENT_MAX: usize = 3;

/// Counts the chat-template and role tokens of `text`.
///
/// Invisible characters are dropped first, so a token split by them still counts.
///
/// A special token is `<|`, 1 to `SPECIAL_NAME_MAX` ASCII letters, digits or `_`, and `|>`,
/// or the same between `FULLWIDTH_OPEN` and `FULLWIDTH_CLOSE` with `NAME_SPACE` allowed.
/// A role token is a line that starts with a `ROLE_LINES` prefix after at most three spaces.
pub(crate) fn count(text: &str) -> u32 {
    let visible: Cow<'_, str> = if text.chars().any(characters::invisible) {
        Cow::Owned(
            text.chars()
                .filter(|character| !characters::invisible(*character))
                .collect(),
        )
    } else {
        Cow::Borrowed(text)
    };
    let text = visible.as_ref();
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
    for (start, _) in text.match_indices(FULLWIDTH_OPEN) {
        let rest = text
            .get(start.saturating_add(FULLWIDTH_OPEN.len())..)
            .unwrap_or_default();
        let name: usize = rest
            .chars()
            .take(SPECIAL_NAME_MAX.saturating_add(1))
            .take_while(|character| {
                character.is_ascii_alphanumeric() || matches!(*character, '_' | NAME_SPACE)
            })
            .map(char::len_utf8)
            .sum();
        let chars = rest.get(..name).unwrap_or_default().chars().count();
        if (1..=SPECIAL_NAME_MAX).contains(&chars)
            && rest
                .get(name..)
                .is_some_and(|tail| tail.starts_with(FULLWIDTH_CLOSE))
        {
            found = found.saturating_add(1);
        }
    }
    let lower = text.to_ascii_lowercase();
    for token in TOKENS {
        let matches = u32::try_from(lower.matches(token).count()).unwrap_or(u32::MAX);
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
