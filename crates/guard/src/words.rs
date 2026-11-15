//! Word normalization and the cue rule that decides whether hidden text instructs a reviewer.

// Rust guideline compliant 2026-06-26

use crate::{characters, template};

/// Words that address a model; each phrase is normalized and padded with one space.
const ADDRESSEES: [&str; 27] = [
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
    " llama ",
    " deepseek ",
    " qwen ",
    " grok ",
    " mistral ",
    " openai ",
    " anthropic ",
    " review tool ",
];

/// Verbs that tell a model what to do or leave out; normalized and padded.
///
/// Template verbs such as "describe", "link", "check" or "say" and the modals "must" and
/// "never" are left out on purpose: they fill the HTML comments of pull request templates
/// ("If you used an AI assistant, say which parts it wrote").
const DIRECTIVES: [&str; 19] = [
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
    " report ",
    " flag ",
    " mention ",
    " skip ",
    " ignoring ",
    " disregarding ",
    " approving ",
    " skipping ",
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

/// Shortest last override word that more letters may follow in a glued word.
///
/// "IgnorePreviousInstructionsNow" holds an override; "ApproveThisProposal" does not, so a
/// phrase that ends in a short word such as "pr" only matches a glued word exactly.
const GLUED_TAIL_MIN: usize = 5;

/// The `OVERRIDES` without their spaces, for words written together.
const GLUED_OVERRIDES: [&str; 17] = [
    "ignorepreviousinstructions",
    "ignoreallprevious",
    "ignorepriorinstructions",
    "ignoretheaboveinstructions",
    "ignoreyourinstructions",
    "disregardpreviousinstructions",
    "disregardallprevious",
    "disregardtheaboveinstructions",
    "disregardyourinstructions",
    "forgetpreviousinstructions",
    "forgetyourinstructions",
    "approvethispullrequest",
    "approvethispr",
    "approvethischange",
    "approvethismergerequest",
    "reportnoissues",
    "reportnofindings",
];

/// Lowercase letters of other scripts that look like a Latin letter, with that letter.
const CONFUSABLES: [(char, char); 25] = [
    ('\u{0430}', 'a'),
    ('\u{0435}', 'e'),
    ('\u{043E}', 'o'),
    ('\u{0440}', 'p'),
    ('\u{0441}', 'c'),
    ('\u{0443}', 'y'),
    ('\u{0445}', 'x'),
    ('\u{0456}', 'i'),
    ('\u{0458}', 'j'),
    ('\u{0455}', 's'),
    ('\u{0501}', 'd'),
    ('\u{04BB}', 'h'),
    ('\u{04CF}', 'l'),
    ('\u{051B}', 'q'),
    ('\u{051D}', 'w'),
    ('\u{043A}', 'k'),
    ('\u{03B1}', 'a'),
    ('\u{03BF}', 'o'),
    ('\u{03BD}', 'v'),
    ('\u{03B9}', 'i'),
    ('\u{03BA}', 'k'),
    ('\u{03C1}', 'p'),
    ('\u{03C5}', 'u'),
    ('\u{03C7}', 'x'),
    ('\u{0131}', 'i'),
];

/// Offset from a fullwidth form U+FF01-FF5E to its ASCII character.
const FULLWIDTH_OFFSET: u32 = 0xFEE0;

/// Returns `text` as lowercase words, each preceded and followed by one space.
///
/// Letters and digits form words; every other character separates them, and so does a
/// percent escape such as `%20`. Invisible characters are dropped, so that a word split by
/// them still matches. Fullwidth forms count as their ASCII characters, and a word whose
/// other letters all look like Latin ones (`CONFUSABLES`) is spelled in Latin. An empty text
/// gives `" "`. There is no other folding: decomposed accents still differ.
pub(crate) fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len().saturating_add(2));
    out.push(' ');
    let mut word = String::new();
    let mut at = 0_usize;
    while let Some(raw) = text.get(at..).and_then(|rest| rest.chars().next()) {
        let next = at.saturating_add(raw.len_utf8());
        at = next;
        if characters::invisible(raw) {
            continue;
        }
        let escape = text.get(next..next.saturating_add(2));
        if raw == '%' && escape.is_some_and(|hex| hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            push_word(&mut out, &mut word);
            at = next.saturating_add(2);
            continue;
        }
        let character = match u32::from(raw) {
            0xFF01..=0xFF5E => char::from_u32(u32::from(raw) - FULLWIDTH_OFFSET).unwrap_or(raw),
            _ => raw,
        };
        if character.is_alphanumeric() {
            word.extend(character.to_lowercase());
        } else {
            push_word(&mut out, &mut word);
        }
    }
    push_word(&mut out, &mut word);
    out
}

/// Moves `word` and a space to `out`, spelled in Latin when it can be.
fn push_word(out: &mut String, word: &mut String) {
    if word.is_empty() {
        return;
    }
    let confusable = |character: char| {
        CONFUSABLES
            .iter()
            .find(|(other, _)| *other == character)
            .map(|(_, latin)| *latin)
    };
    if word
        .chars()
        .all(|character| character.is_ascii() || confusable(character).is_some())
    {
        out.extend(
            word.chars()
                .map(|character| confusable(character).unwrap_or(character)),
        );
    } else {
        out.push_str(word);
    }
    out.push(' ');
    word.clear();
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

/// Named character references that are decoded, with their text.
///
/// The usual escapes, and every HTML5 name whose text holds a character of a guard class
/// (zero-width characters, variation selectors, bidi marks): the full list as of HTML5.
const NAMED_REFERENCES: [(&str, &str); 43] = [
    ("amp", "&"),
    ("lt", "<"),
    ("gt", ">"),
    ("quot", "\""),
    ("apos", "'"),
    ("nbsp", "\u{00A0}"),
    ("ApplyFunction", "\u{2061}"),
    ("InvisibleComma", "\u{2063}"),
    ("InvisibleTimes", "\u{2062}"),
    ("NegativeMediumSpace", "\u{200B}"),
    ("NegativeThickSpace", "\u{200B}"),
    ("NegativeThinSpace", "\u{200B}"),
    ("NegativeVeryThinSpace", "\u{200B}"),
    ("NoBreak", "\u{2060}"),
    ("ZeroWidthSpace", "\u{200B}"),
    ("af", "\u{2061}"),
    ("caps", "\u{2229}\u{FE00}"),
    ("cups", "\u{222A}\u{FE00}"),
    ("gesl", "\u{22DB}\u{FE00}"),
    ("gvertneqq", "\u{2269}\u{FE00}"),
    ("gvnE", "\u{2269}\u{FE00}"),
    ("ic", "\u{2063}"),
    ("it", "\u{2062}"),
    ("lates", "\u{2AAD}\u{FE00}"),
    ("lesg", "\u{22DA}\u{FE00}"),
    ("lrm", "\u{200E}"),
    ("lvertneqq", "\u{2268}\u{FE00}"),
    ("lvnE", "\u{2268}\u{FE00}"),
    ("rlm", "\u{200F}"),
    ("shy", "\u{00AD}"),
    ("smtes", "\u{2AAC}\u{FE00}"),
    ("sqcaps", "\u{2293}\u{FE00}"),
    ("sqcups", "\u{2294}\u{FE00}"),
    ("varsubsetneq", "\u{228A}\u{FE00}"),
    ("varsubsetneqq", "\u{2ACB}\u{FE00}"),
    ("varsupsetneq", "\u{228B}\u{FE00}"),
    ("varsupsetneqq", "\u{2ACC}\u{FE00}"),
    ("vsubnE", "\u{2ACB}\u{FE00}"),
    ("vsubne", "\u{228A}\u{FE00}"),
    ("vsupnE", "\u{2ACC}\u{FE00}"),
    ("vsupne", "\u{228B}\u{FE00}"),
    ("zwj", "\u{200D}"),
    ("zwnj", "\u{200C}"),
];

/// Named references that HTML also decodes without their `;` (legacy names).
const LEGACY_NAMES: [&str; 6] = ["amp", "lt", "gt", "quot", "nbsp", "shy"];

/// Most characters of a reference name ("CounterClockwiseContourIntegral" has 31).
const REFERENCE_MAX: usize = 32;

/// Most significant digits of a numeric reference that can still name a character.
///
/// U+10FFFF is 1114111 in decimal; more digits always give the replacement character.
const NUMBER_DIGITS_MAX: usize = 7;

/// Reports whether hidden text instructs a reviewer (the cue rule).
///
/// It does when it holds an override phrase or a chat-template token (`overrides_in`), or
/// when, with its web addresses left out (their paths hold arbitrary words), it names a model
/// and holds a directive verb. Character references are decoded first, as a reader would.
pub(crate) fn instructs(text: &str) -> bool {
    let text = decode_references(text);
    if overrides_decoded(&text) {
        return true;
    }
    let without_addresses = normalize(&without_web_addresses(&text));
    let has = |phrases: &[&str]| {
        phrases
            .iter()
            .any(|phrase| without_addresses.contains(phrase))
    };
    has(&ADDRESSEES) && has(&DIRECTIVES)
}

/// Reports whether text holds an override phrase or a chat-template token.
///
/// The override phrases include the window rule (`overrides`) and words written together
/// (`glued`); character references are decoded first.
pub(crate) fn overrides_in(text: &str) -> bool {
    overrides_decoded(&decode_references(text))
}

/// Reports `overrides_in` for text whose references are already decoded.
fn overrides_decoded(text: &str) -> bool {
    let words = normalize(text);
    OVERRIDES.iter().any(|phrase| words.contains(phrase))
        || overrides(&words)
        || glued(&words)
        || template::count(text) > 0
}

/// Reports whether one normalized word holds an override written without spaces.
///
/// The match never crosses a word boundary: "approve this proposal" holds no override.
fn glued(words: &str) -> bool {
    let shortest = GLUED_OVERRIDES
        .iter()
        .map(|glued| glued.len())
        .min()
        .unwrap_or_default();
    words
        .split(' ')
        .filter(|word| word.len() >= shortest)
        .any(|word| {
            OVERRIDES
                .iter()
                .zip(GLUED_OVERRIDES)
                .any(|(phrase, glued)| {
                    let tail = phrase.trim().rsplit(' ').next().unwrap_or_default();
                    word == glued || (tail.len() >= GLUED_TAIL_MIN && word.contains(glued))
                })
        })
}

/// Reports whether normalized words hold an override verb, then a qualifier and an object.
///
/// "Ignore all the previous instructions" and "disregard prior rules" are overrides; "ignore
/// this rule if it does not apply" is not, as no qualifier points back.
fn overrides(words: &str) -> bool {
    if !OVERRIDE_VERBS.iter().any(|verb| words.contains(verb)) {
        return false;
    }
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
