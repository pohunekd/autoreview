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
