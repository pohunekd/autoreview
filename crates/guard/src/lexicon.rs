//! The weighted phrase lexicon: its text format, validation and matching.
//!
//! One entry per line: a weight from 1 to `LEXICON_MAX_WEIGHT`, whitespace, then the phrase.
//! Blank lines and lines whose first non-blank character is `#` are skipped. A phrase matches
//! whole words, ignoring case, punctuation, line breaks and invisible characters, so
//! `40 ignore previous instructions` also matches "Ignore, previous\ninstructions".
//!
//! ```text
//! # weight phrase
//! 40 ignore previous instructions
//! 25 approve this pull request
//! ```

// Rust guideline compliant 2026-06-26

use std::collections::{BTreeMap, BTreeSet};

use crate::{Error, words};

/// Largest weight of a lexicon entry; also the weight of one hidden-text signal.
pub const LEXICON_MAX_WEIGHT: u32 = 100;

/// One validated lexicon line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    /// What the entry adds to the score when it is found.
    weight: u32,
    /// The phrase as normalized words, padded with one space on each side.
    words: String,
}

/// Weighted phrases that add to the score; parsed and validated from text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lexicon {
    /// The entries in file order.
    entries: Vec<Entry>,
}

impl Lexicon {
    /// Parses and validates a lexicon from the text of a lexicon file.
    ///
    /// # Errors
    ///
    /// Returns the first invalid line: `Error::LexiconWeight`, `Error::LexiconPhrase` or
    /// `Error::LexiconDuplicate`; `Error::LexiconEmpty` when no line holds an entry.
    pub fn parse(text: &str) -> Result<Self, Error> {
        let mut entries: Vec<Entry> = Vec::new();
        let mut first_lines: BTreeMap<String, usize> = BTreeMap::new();
        for (index, raw) in text.lines().enumerate() {
            let line = index.saturating_add(1);
            let content = raw.trim();
            if content.is_empty() || content.starts_with('#') {
                continue;
            }
            let (weight_text, phrase) = content
                .split_once(char::is_whitespace)
                .unwrap_or((content, ""));
            let weight = weight_text
                .bytes()
                .all(|byte| byte.is_ascii_digit())
                .then(|| weight_text.parse::<u32>().ok())
                .flatten()
                .filter(|weight| (1..=LEXICON_MAX_WEIGHT).contains(weight))
                .ok_or(Error::LexiconWeight { line })?;
            let words = words::normalize(phrase);
            if words.trim().is_empty() {
                return Err(Error::LexiconPhrase { line });
            }
            if let Some(&first) = first_lines.get(&words) {
                return Err(Error::LexiconDuplicate { line, first });
            }
            first_lines.insert(words.clone(), line);
            entries.push(Entry { weight, words });
        }
        if entries.is_empty() {
            return Err(Error::LexiconEmpty);
        }
        Ok(Self { entries })
    }

    /// Returns the indexes of the entries found in `normalized`, the output of `normalize`.
    pub(crate) fn found<'a>(&'a self, normalized: &'a str) -> impl Iterator<Item = usize> + 'a {
        self.entries
            .iter()
            .enumerate()
            .filter(move |(_, entry)| normalized.contains(entry.words.as_str()))
            .map(|(index, _)| index)
    }

    /// Returns the weights of the entries at `indexes`.
    pub(crate) fn weights<'a>(
        &'a self,
        indexes: &'a BTreeSet<usize>,
    ) -> impl Iterator<Item = u32> + 'a {
        indexes
            .iter()
            .filter_map(|&index| self.entries.get(index).map(|entry| entry.weight))
    }
}
