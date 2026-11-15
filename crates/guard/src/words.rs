//! Word normalization and the cue rule that decides whether hidden text instructs a reviewer.

// Rust guideline compliant 2026-06-26

use crate::{characters, template};

/// Words that address a model; each phrase is normalized and padded with one space.
const ADDRESSEES: [&str; 19] = [
    " ai ",
    " llm ",
    " llms ",
    " gpt ",
    " chatgpt ",
    " claude ",
    " copilot ",
    " codex ",
    " gemini ",
    " assistant ",
    " language model ",
    " coding agent ",
    " review agent ",
    " review bot ",
    " automated reviewer ",
    " system prompt ",
    " a i ",
    " l l m ",
    " co pilot ",
];

/// Verbs that tell a model what to do or leave out; normalized and padded.
///
/// Template verbs such as "describe", "link" or "check" and the modals "must" and "never" are
/// left out on purpose: they fill the HTML comments of pull request templates.
const DIRECTIVES: [&str; 16] = [
    " ignore ",
    " disregard ",
    " forget ",
    " approve ",
    " override ",
    " pretend ",
    " obey ",
    " follow ",
    " respond ",
    " reply ",
    " output ",
    " say ",
    " report ",
    " flag ",
    " mention ",
    " skip ",
];

/// Phrases that try to replace a reviewer's instructions or verdict; normalized and padded.
const OVERRIDES: [&str; 17] = [
    " ignore previous instructions ",
    " ignore all previous ",
    " ignore prior instructions ",
    " ignore the above instructions ",
    " ignore your instructions ",
    " disregard previous instructions ",
    " disregard all previous ",
    " disregard the above instructions ",
    " disregard your instructions ",
    " forget previous instructions ",
    " forget your instructions ",
    " approve this pull request ",
    " approve this pr ",
    " approve this change ",
    " approve this merge request ",
    " report no issues ",
    " report no findings ",
];

/// Returns `text` as lowercase words, each preceded and followed by one space.
///
/// Letters and digits form words; every other character separates them, except invisible
/// characters, which are dropped so that a word split by them still matches. An empty text
/// gives `" "`. There is no compatibility folding: homoglyphs and decomposed accents differ.
pub(crate) fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len().saturating_add(2));
    out.push(' ');
    for character in text.chars() {
        if characters::invisible(character) {
            continue;
        }
        if character.is_alphanumeric() {
            out.extend(character.to_lowercase());
        } else if !out.ends_with(' ') {
            out.push(' ');
        }
    }
    if !out.ends_with(' ') {
        out.push(' ');
    }
    out
}

/// Verbs that start an override: a qualifier and an object follow within `OVERRIDE_WINDOW`.
const OVERRIDE_VERBS: [&str; 8] = [
    "ignore",
    "ignoring",
    "disregard",
    "disregarding",
    "forget",
    "forgetting",
    "override",
    "overriding",
];

/// Words that point an override at instructions that came before.
const OVERRIDE_QUALIFIERS: [&str; 8] = [
    "previous",
    "prior",
    "above",
    "earlier",
    "preceding",
    "original",
    "system",
    "your",
];

/// Words that name what an override replaces.
const OVERRIDE_OBJECTS: [&str; 10] = [
    "instruction",
    "instructions",
    "rule",
    "rules",
    "guideline",
    "guidelines",
    "prompt",
    "prompts",
    "directions",
    "directives",
];

/// Most words after an override verb that may hold its qualifier and object.
const OVERRIDE_WINDOW: usize = 5;

/// Named character references that are decoded, with their characters.
///
/// The usual escapes, and the invisible characters HTML has names for.
const NAMED_REFERENCES: [(&str, char); 12] = [
    ("amp", '&'),
    ("lt", '<'),
    ("gt", '>'),
    ("quot", '"'),
    ("apos", '\''),
    ("nbsp", '\u{00A0}'),
    ("shy", '\u{00AD}'),
    ("zwj", '\u{200D}'),
    ("zwnj", '\u{200C}'),
    ("lrm", '\u{200E}'),
    ("rlm", '\u{200F}'),
    ("ZeroWidthSpace", '\u{200B}'),
];

/// Named references that HTML also decodes without their `;` (legacy names).
const LEGACY_NAMES: [&str; 6] = ["amp", "lt", "gt", "quot", "nbsp", "shy"];

/// Most characters of a reference name; longer runs of letters are no reference.
const REFERENCE_MAX: usize = 16;

/// Most significant digits of a numeric reference that can still name a character.
///
/// U+10FFFF is 1114111 in decimal; more digits always give the replacement character.
const NUMBER_DIGITS_MAX: usize = 7;

/// Reports whether hidden text instructs a reviewer (the cue rule).
///
/// It does when it holds an override phrase or a chat-template token, or when, with its web
/// addresses left out (their paths hold arbitrary words), it names a model and holds a
/// directive verb. Character references are decoded first, as a reader of the text would.
pub(crate) fn instructs(text: &str) -> bool {
    let text = decode_references(text);
    let words = normalize(&text);
    let without_addresses = normalize(&without_web_addresses(&text));
    let has = |words: &str, phrases: &[&str]| phrases.iter().any(|phrase| words.contains(phrase));
    has(&words, &OVERRIDES)
        || overrides(&words)
        || template::count(&text) > 0
        || (has(&without_addresses, &ADDRESSEES) && has(&without_addresses, &DIRECTIVES))
}

/// Reports whether normalized words hold an override verb, then a qualifier and an object.
///
/// "Ignore all the previous instructions" and "disregard prior rules" are overrides; "ignore
/// this rule if it does not apply" is not, as no qualifier points back.
fn overrides(words: &str) -> bool {
    let tokens: Vec<&str> = words.split(' ').filter(|token| !token.is_empty()).collect();
    tokens.iter().enumerate().any(|(index, token)| {
        if !OVERRIDE_VERBS.contains(token) {
            return false;
        }
        let window = tokens
            .get(index.saturating_add(1)..)
            .unwrap_or_default()
            .iter()
            .take(OVERRIDE_WINDOW);
        let mut qualified = false;
        for word in window {
            qualified |= OVERRIDE_QUALIFIERS.contains(word);
            if qualified && OVERRIDE_OBJECTS.contains(word) {
                return true;
            }
        }
        false
    })
}

/// Returns `text` with its character references decoded, as an HTML parser decodes them.
///
/// A numeric reference takes its whole digit run, `;` optional, leading zeros skipped; one
/// that names no character (0, a surrogate, above U+10FFFF) becomes U+FFFD. A name of
/// `NAMED_REFERENCES` needs its `;`, except the `LEGACY_NAMES`. Anything else stays as
/// written. The output is decoded once: `&amp;#105;` gives `&#105;`.
pub(crate) fn decode_references(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(rest.get(..start).unwrap_or_default());
        let from = rest.get(start.saturating_add(1)..).unwrap_or_default();
        match reference(from) {
            Some((character, used)) => {
                out.push(character);
                rest = from.get(used..).unwrap_or_default();
            }
            None => {
                out.push('&');
                rest = from;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Decodes the reference that starts `from` (the text after `&`) with its length.
fn reference(from: &str) -> Option<(char, usize)> {
    if let Some(number) = from.strip_prefix('#') {
        let (body, radix, prefix) = match number.strip_prefix(['x', 'X']) {
            Some(hex) => (hex, 16, 2_usize),
            None => (number, 10, 1_usize),
        };
        let digits = body
            .chars()
            .take_while(|digit| digit.is_digit(radix))
            .count();
        let all = body.get(..digits).filter(|all| !all.is_empty())?;
        let significant = all.trim_start_matches('0');
        let character = (significant.len() <= NUMBER_DIGITS_MAX)
            .then(|| u32::from_str_radix(significant, radix).ok())
            .flatten()
            .filter(|code| *code != 0)
            .and_then(char::from_u32)
            .unwrap_or(char::REPLACEMENT_CHARACTER);
        let semicolon = usize::from(body.get(digits..).is_some_and(|tail| tail.starts_with(';')));
        return Some((
            character,
            prefix.saturating_add(digits).saturating_add(semicolon),
        ));
    }
    let name_len = from
        .bytes()
        .take(REFERENCE_MAX.saturating_add(1))
        .take_while(u8::is_ascii_alphanumeric)
        .count();
    let name = from.get(..name_len).unwrap_or_default();
    let closed = from
        .get(name_len..)
        .is_some_and(|tail| tail.starts_with(';'));
    let named = |name: &str| {
        NAMED_REFERENCES
            .iter()
            .find(|(known, _)| *known == name)
            .map(|(_, character)| *character)
    };
    if closed && let Some(character) = named(name) {
        return Some((character, name_len.saturating_add(1)));
    }
    let legacy = LEGACY_NAMES
        .iter()
        .find(|legacy| from.starts_with(**legacy))?;
    named(legacy).map(|character| (character, legacy.len()))
}

/// Returns `text` without its `http://` and `https://` addresses.
///
/// An address runs to the next whitespace, quote, angle bracket, `)` or `]`.
fn without_web_addresses(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("http") {
        out.push_str(rest.get(..start).unwrap_or_default());
        let from = rest.get(start..).unwrap_or_default();
        let scheme = ["http://", "https://"]
            .iter()
            .find(|scheme| from.starts_with(**scheme));
        let Some(scheme) = scheme else {
            out.push_str("http");
            rest = from.get(4..).unwrap_or_default();
            continue;
        };
        let address = from.get(scheme.len()..).unwrap_or_default();
        let end = address.find(ends_address).unwrap_or(address.len());
        out.push(' ');
        rest = address.get(end..).unwrap_or_default();
    }
    out.push_str(rest);
    out
}

/// Reports whether a character ends a web address: whitespace, a quote or a closing bracket.
fn ends_address(character: char) -> bool {
    character.is_whitespace() || matches!(character, '"' | '\'' | '<' | '>' | ')' | ']')
}
