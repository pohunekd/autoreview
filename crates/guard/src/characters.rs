//! Character classes that hide or reorder text, and the scanner of their attack shapes.

// Rust guideline compliant 2026-06-26

use crate::Signal;

/// Base of an emoji subdivision flag, WAVING BLACK FLAG; tag characters may follow it.
const FLAG_BASE: char = '\u{1F3F4}';

/// CANCEL TAG, the end of an emoji subdivision flag.
const CANCEL_TAG: char = '\u{E007F}';

/// Most tag letters and digits of a subdivision flag: a region of 2-3 and a suffix of 1-4.
const FLAG_TAG_MAX: usize = 7;

/// Shortest run of zero-width characters or variation selectors that the scanner reports.
///
/// One such character is common in emoji, scripts that join letters and pasted text; two in
/// a row have no legitimate use and are how payloads are encoded in invisible characters.
const RUN_MIN: usize = 2;

/// Reports whether a character is a Unicode tag character, U+E0000-E007F.
pub(crate) const fn tag(character: char) -> bool {
    matches!(character, '\u{E0000}'..='\u{E007F}')
}

/// Reports whether a character shows no width: zero-width, soft hyphen or Hangul filler.
pub(crate) const fn zero_width(character: char) -> bool {
    matches!(
        character,
        '\u{00AD}'
            | '\u{115F}'
            | '\u{1160}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200D}'
            | '\u{2060}'..='\u{2064}'
            | '\u{206A}'..='\u{206F}'
            | '\u{3164}'
            | '\u{FEFF}'
            | '\u{FFA0}'
    )
}

/// Reports whether a character is a variation selector, VS1-VS16 or VS17-VS256.
pub(crate) const fn variation_selector(character: char) -> bool {
    matches!(character, '\u{FE00}'..='\u{FE0F}' | '\u{E0100}'..='\u{E01EF}')
}

/// Reports whether a character is a bidi override, U+202D or U+202E.
const fn bidi_override(character: char) -> bool {
    matches!(character, '\u{202D}' | '\u{202E}')
}

/// Reports whether a character is a bidi embedding or isolate, legitimate in RTL prose.
const fn bidi_embedding_or_isolate(character: char) -> bool {
    matches!(character, '\u{202A}'..='\u{202C}' | '\u{2066}'..='\u{2069}')
}

/// Reports whether a character is dropped before words are compared.
///
/// Besides the classes above it covers the bidi marks U+200E, U+200F and U+061C, so a
/// phrase split by any invisible character still matches.
pub(crate) const fn invisible(character: char) -> bool {
    tag(character)
        || zero_width(character)
        || variation_selector(character)
        || bidi_override(character)
        || bidi_embedding_or_isolate(character)
        || matches!(character, '\u{061C}' | '\u{200E}' | '\u{200F}')
}

/// The state of one run of tag characters.
#[derive(Debug, Default)]
struct TagRun {
    /// The run directly follows `FLAG_BASE`.
    after_flag_base: bool,
    /// Tag letters and digits before the cancel tag.
    letters: usize,
    /// The cancel tag was seen.
    cancelled: bool,
    /// The run holds a tag character that no subdivision flag allows at that place.
    foreign: bool,
}

impl TagRun {
    /// Adds one tag character to the run.
    fn push(&mut self, character: char) {
        if character == CANCEL_TAG {
            self.foreign |= self.cancelled;
            self.cancelled = true;
        } else if matches!(character, '\u{E0030}'..='\u{E0039}' | '\u{E0061}'..='\u{E007A}') {
            self.foreign |= self.cancelled;
            self.letters = self.letters.saturating_add(1);
        } else {
            self.foreign = true;
        }
    }

    /// Reports whether the finished run is one emoji subdivision flag.
    fn is_flag(&self) -> bool {
        self.after_flag_base
            && !self.foreign
            && self.cancelled
            && (1..=FLAG_TAG_MAX).contains(&self.letters)
    }
}

/// Reports the character signals of `text` to `add`: tags, runs and bidi controls.
///
/// `in_patch` is set for added patch lines, where embeddings and isolates are reported too.
pub(crate) fn scan(text: &str, in_patch: bool, add: &mut impl FnMut(Signal, u32)) {
    let mut tag_run: Option<TagRun> = None;
    let mut previous = '\n';
    let mut zero_width_len = 0_usize;
    let mut selector_len = 0_usize;
    for character in text.chars() {
        if tag(character) {
            tag_run
                .get_or_insert_with(|| TagRun {
                    after_flag_base: previous == FLAG_BASE,
                    ..TagRun::default()
                })
                .push(character);
        } else if let Some(run) = tag_run.take() {
            add(Signal::UnicodeTags, u32::from(!run.is_flag()));
        }
        zero_width_len = if zero_width(character) {
            zero_width_len.saturating_add(1)
        } else {
            0
        };
        if zero_width_len == RUN_MIN {
            add(Signal::ZeroWidthRun, 1);
        }
        selector_len = if variation_selector(character) {
            selector_len.saturating_add(1)
        } else {
            0
        };
        if selector_len == RUN_MIN {
            add(Signal::VariationSelectorRun, 1);
        }
        if bidi_override(character) {
            add(Signal::BidiOverride, 1);
        } else if in_patch && bidi_embedding_or_isolate(character) {
            add(Signal::BidiControlInPatch, 1);
        }
        previous = character;
    }
    if let Some(run) = tag_run {
        add(Signal::UnicodeTags, u32::from(!run.is_flag()));
    }
}
