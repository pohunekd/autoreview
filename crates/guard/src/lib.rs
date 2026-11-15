//! Deterministic scan of pull request text for instructions hidden from a human reader.
//!
//! The guard reads the title, body, commit messages, comments, review threads and added patch
//! lines of a pull request and reports fixed signal codes and a score. It never returns or logs
//! the text it matched. Every scanner is one forward pass over its input and never panics, so a
//! hostile text costs time linear in its length (times the number of lexicon entries).

// Rust guideline compliant 2026-06-26

mod characters;
mod lexicon;
mod markdown;
mod template;
mod words;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};

pub use lexicon::{LEXICON_MAX_WEIGHT, Lexicon};

/// Weight of one hidden-text signal, equal to the largest weight of a lexicon entry.
const HIDDEN_TEXT_WEIGHT: u32 = LEXICON_MAX_WEIGHT;

/// Weight of a chat-template token: visible text, common in code of model tooling.
const CHAT_TEMPLATE_WEIGHT: u32 = 30;

/// Weight of an embedding or isolate control in added code (Trojan Source shape).
const BIDI_IN_PATCH_WEIGHT: u32 = 40;

/// Where a scanned text comes from in a pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FieldKind {
    /// The pull request title.
    Title,
    /// The pull request description.
    Body,
    /// One commit message.
    CommitMessage,
    /// One issue comment or review body.
    Comment,
    /// One comment of a review thread.
    ReviewThread,
    /// The unified diff of one changed file; only its added lines are scanned.
    Patch,
}

impl FieldKind {
    /// Returns the stable identifier of the kind; stored and published, never renamed.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Body => "body",
            Self::CommitMessage => "commit_message",
            Self::Comment => "comment",
            Self::ReviewThread => "review_thread",
            Self::Patch => "patch",
        }
    }

    /// Reports whether GitHub renders the field as Markdown, which can hide text.
    const fn renders_markdown(self) -> bool {
        matches!(self, Self::Body | Self::Comment | Self::ReviewThread)
    }
}

/// One text of a pull request, as the engine read it.
///
/// It has no name or path: a file path is pull request text and must not reach a log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Field<'a> {
    /// Where the text comes from.
    pub kind: FieldKind,
    /// The text; for `FieldKind::Patch` the unified diff of one file.
    pub text: &'a str,
}

/// A fixed code for one kind of suspicious text; it never carries the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Signal {
    /// A run of tag characters U+E0000-E007F that is not an emoji subdivision flag.
    UnicodeTags,
    /// A run of two or more zero-width characters.
    ZeroWidthRun,
    /// A run of two or more variation selectors.
    VariationSelectorRun,
    /// A run of 3+ invisible characters that mixes classes or holds a bidi mark.
    InvisibleRun,
    /// A left-to-right or right-to-left override, U+202D or U+202E.
    BidiOverride,
    /// An embedding U+202A-202C or isolate U+2066-2069 on an added patch line.
    BidiControlInPatch,
    /// An HTML comment whose text instructs a reviewer.
    HiddenCommentInstruction,
    /// A `<details>` element whose text instructs a reviewer.
    CollapsedBlockInstruction,
    /// An element marked `hidden` or hidden by its style whose text instructs a reviewer.
    HiddenElementInstruction,
    /// Image alt text, a link title or tag attributes that instruct a reviewer.
    LinkMetadataInstruction,
    /// A reference definition line that instructs a reviewer.
    ReferenceDefinitionInstruction,
    /// A chat-template or role token.
    ChatTemplateToken,
    /// A lexicon entry; the count is the number of distinct entries found.
    LexiconPhrase,
}

impl Signal {
    /// Returns the stable code of the signal; stored and published, never renamed.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnicodeTags => "unicode_tags",
            Self::ZeroWidthRun => "zero_width_run",
            Self::VariationSelectorRun => "variation_selector_run",
            Self::InvisibleRun => "invisible_run",
            Self::BidiOverride => "bidi_override",
            Self::BidiControlInPatch => "bidi_control_in_patch",
            Self::HiddenCommentInstruction => "hidden_comment_instruction",
            Self::CollapsedBlockInstruction => "collapsed_block_instruction",
            Self::HiddenElementInstruction => "hidden_element_instruction",
            Self::LinkMetadataInstruction => "link_metadata_instruction",
            Self::ReferenceDefinitionInstruction => "reference_definition_instruction",
            Self::ChatTemplateToken => "chat_template_token",
            Self::LexiconPhrase => "lexicon_phrase",
        }
    }

    /// Returns what one signal adds to the score; a lexicon entry carries its own weight.
    const fn weight(self) -> u32 {
        match self {
            Self::BidiControlInPatch => BIDI_IN_PATCH_WEIGHT,
            Self::ChatTemplateToken => CHAT_TEMPLATE_WEIGHT,
            Self::LexiconPhrase => 0,
            Self::UnicodeTags
            | Self::ZeroWidthRun
            | Self::VariationSelectorRun
            | Self::InvisibleRun
            | Self::BidiOverride
            | Self::HiddenCommentInstruction
            | Self::CollapsedBlockInstruction
            | Self::HiddenElementInstruction
            | Self::LinkMetadataInstruction
            | Self::ReferenceDefinitionInstruction => HIDDEN_TEXT_WEIGHT,
        }
    }
}

/// One signal found in the fields of one kind, with how often it occurred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    /// The kind of the fields the signal was found in.
    pub field: FieldKind,
    /// The signal.
    pub signal: Signal,
    /// Occurrences over all fields of that kind; distinct entries for `LexiconPhrase`.
    pub count: u32,
}

/// The hits of a scan, ordered by field kind then signal, and their score.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// One hit per field kind and signal found, never with a zero count.
    pub hits: Vec<Hit>,
    /// The weights of the distinct signals plus those of the distinct lexicon entries found.
    pub score: u32,
}

/// Errors of the guard crate: a lexicon that does not validate.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// A lexicon line does not start with a whole number from 1 to `LEXICON_MAX_WEIGHT`.
    #[error("lexicon line {line}: the weight is not a number from 1 to {LEXICON_MAX_WEIGHT}")]
    LexiconWeight {
        /// The 1-based line number.
        line: usize,
    },
    /// A lexicon line has no letter or digit after its weight.
    #[error("lexicon line {line}: the phrase has no letter or digit")]
    LexiconPhrase {
        /// The 1-based line number.
        line: usize,
    },
    /// A lexicon line holds the same phrase as an earlier line.
    #[error("lexicon line {line}: the phrase repeats line {first}")]
    LexiconDuplicate {
        /// The 1-based line number of the repeat.
        line: usize,
        /// The 1-based line number of the first occurrence.
        first: usize,
    },
    /// The lexicon has no entry.
    #[error("the lexicon has no phrase")]
    LexiconEmpty,
}

/// Scans the fields and scores what it found.
///
/// Signals found in several fields of one kind add up under that kind. The score counts each
/// signal and each lexicon entry once, whatever the number of occurrences.
#[must_use]
pub fn scan(fields: &[Field<'_>], lexicon: &Lexicon) -> Report {
    let mut counts: BTreeMap<(FieldKind, Signal), u32> = BTreeMap::new();
    let mut entries_by_kind: BTreeMap<FieldKind, BTreeSet<usize>> = BTreeMap::new();
    for field in fields {
        let added;
        let text = if field.kind == FieldKind::Patch {
            added = added_lines(field.text);
            added.as_str()
        } else {
            field.text
        };
        let mut add = |signal: Signal, count: u32| {
            if count > 0 {
                let total = counts.entry((field.kind, signal)).or_insert(0);
                *total = total.saturating_add(count);
            }
        };
        characters::scan(text, field.kind == FieldKind::Patch, &mut add);
        if field.kind.renders_markdown() {
            markdown::scan(text, &mut add);
        }
        add(Signal::ChatTemplateToken, template::count(text));
        let normalized = words::normalize(text);
        entries_by_kind
            .entry(field.kind)
            .or_default()
            .extend(lexicon.found(&normalized));
    }
    let mut all_entries = BTreeSet::new();
    for (kind, entries) in entries_by_kind {
        let count = u32::try_from(entries.len()).unwrap_or(u32::MAX);
        if count > 0 {
            counts.insert((kind, Signal::LexiconPhrase), count);
        }
        all_entries.extend(entries);
    }
    let signals: BTreeSet<Signal> = counts.keys().map(|&(_, signal)| signal).collect();
    let score = signals
        .iter()
        .map(|signal| signal.weight())
        .chain(lexicon.weights(&all_entries))
        .fold(0_u32, u32::saturating_add);
    let hits = counts
        .into_iter()
        .map(|((field, signal), count)| Hit {
            field,
            signal,
            count,
        })
        .collect();
    Report { hits, score }
}

/// Returns the added lines of a unified diff, without `+`, one per line.
///
/// Removed lines are not in the head and context lines are base content, so only added lines
/// can carry text the pull request brings in.
fn added_lines(patch: &str) -> String {
    let mut out = String::new();
    for line in patch.split('\n') {
        if let Some(added) = line.strip_prefix('+') {
            out.push_str(added);
            out.push('\n');
        }
    }
    out
}
