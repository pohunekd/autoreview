//! Word normalization and the cue rule that decides whether hidden text instructs a reviewer.

// Rust guideline compliant 2026-06-26

use crate::{characters, template};

/// Words that address a model; each phrase is normalized and padded with one space.
const ADDRESSEES: [&str; 16] = [
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

/// Reports whether hidden text instructs a reviewer (the cue rule).
///
/// It does when it names a model and holds a directive verb, when it holds an override
/// phrase, or when it holds a chat-template token. Web addresses are left out first: their
/// paths hold arbitrary words.
pub(crate) fn instructs(text: &str) -> bool {
    let text = without_web_addresses(text);
    let text = text.as_str();
    let words = normalize(text);
    let has = |phrases: &[&str]| phrases.iter().any(|phrase| words.contains(phrase));
    (has(&ADDRESSEES) && has(&DIRECTIVES)) || has(&OVERRIDES) || template::count(text) > 0
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
