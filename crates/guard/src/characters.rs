//! Character classes that hide or reorder text, and the scanner of their attack shapes.

// Rust guideline compliant 2026-06-26

use crate::Signal;

/// Base of an emoji subdivision flag, WAVING BLACK FLAG; tag characters may follow it.
const FLAG_BASE: char = '\u{1F3F4}';

/// CANCEL TAG, the end of an emoji subdivision flag.
const CANCEL_TAG: char = '\u{E007F}';

/// The tag letters of the subdivision flags in the recommended emoji set (UTS #51).
///
/// Any other tag sequence renders as a plain black flag on common platforms, so it could carry
/// a payload of up to seven letters per flag.
const RGI_FLAGS: [&[u8]; 3] = [b"gbeng", b"gbsct", b"gbwls"];

/// Most tag letters kept for the comparison with `RGI_FLAGS`.
const FLAG_LETTERS_MAX: usize = 5;

/// Shortest run of zero-width characters or variation selectors that the scanner reports.
///
/// One such character is common in emoji, scripts that join letters and pasted text; two in
/// a row have no legitimate use and are how payloads are encoded in invisible characters.
const RUN_MIN: usize = 2;

/// Shortest mixed run of invisible characters and bidi marks or controls that is reported.
///
/// The longest legitimate mixes are two long: VS16 then ZWJ inside an emoji sequence, or an
/// isolate next to a mark in right-to-left prose.
const MIXED_RUN_MIN: usize = 3;

/// BRAILLE PATTERN BLANK: not whitespace, but shown as an empty cell.
const BRAILLE_BLANK: char = '\u{2800}';

/// Reports whether a character keeps a run of invisible characters open.
///
/// Whitespace and blank cells show nothing either, so characters spread over them still
/// form one run.
fn transparent(character: char) -> bool {
    character.is_whitespace() || character == BRAILLE_BLANK
}

/// Reports whether a character is a Unicode tag character, U+E0000-E007F.
pub(crate) const fn tag(character: char) -> bool {
    matches!(character, '\u{E0000}'..='\u{E007F}')
}

/// Reports whether a character shows no width: zero-width, soft hyphen or Hangul filler.
///
/// The class holds every Default_Ignorable_Code_Point that renders as nothing, except tags,
/// variation selectors and bidi controls and marks, which have classes of their own.
pub(crate) const fn zero_width(character: char) -> bool {
    matches!(
        character,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{115F}'
            | '\u{1160}'
            | '\u{17B4}'..='\u{17B5}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200D}'
            | '\u{2060}'..='\u{2064}'
            | '\u{206A}'..='\u{206F}'
            | '\u{3164}'
            | '\u{FEFF}'
            | '\u{FFA0}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0080}'..='\u{E00FF}'
            | '\u{E01F0}'..='\u{E0FFF}'
    )
}

/// Reports whether a character is a variation selector, Mongolian ones included.
pub(crate) const fn variation_selector(character: char) -> bool {
    matches!(
        character,
        '\u{180B}'..='\u{180D}'
            | '\u{180F}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{E0100}'..='\u{E01EF}'
    )
}

/// Reports whether a character is a bidi mark: ALM, LRM or RLM.
const fn bidi_mark(character: char) -> bool {
    matches!(character, '\u{061C}' | '\u{200E}' | '\u{200F}')
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
/// It covers every class above, so a phrase split by any invisible character still matches.
pub(crate) const fn invisible(character: char) -> bool {
    tag(character)
        || zero_width(character)
        || variation_selector(character)
        || bidi_override(character)
        || bidi_embedding_or_isolate(character)
        || bidi_mark(character)
}

/// The state of one run of tag characters.
#[derive(Debug, Default)]
struct TagRun {
    /// The run directly follows `FLAG_BASE`.
    after_flag_base: bool,
    /// Tag letters and digits before the cancel tag, as ASCII, up to `FLAG_LETTERS_MAX`.
    letters: [u8; FLAG_LETTERS_MAX],
    /// Number of tag letters and digits before the cancel tag.
    len: usize,
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
            let ascii = u32::from(character)
                .checked_sub(0xE0000)
                .and_then(|code| u8::try_from(code).ok())
                .unwrap_or_default();
            match self.letters.get_mut(self.len) {
                Some(slot) => *slot = ascii,
                None => self.foreign = true,
            }
            self.len = self.len.saturating_add(1);
        } else {
            self.foreign = true;
        }
    }

    /// Reports whether the finished run is one of the `RGI_FLAGS`.
    fn is_flag(&self) -> bool {
        let letters = self.letters.get(..self.len).unwrap_or_default();
        self.after_flag_base && !self.foreign && self.cancelled && RGI_FLAGS.contains(&letters)
    }
}

/// The state of one run of zero-width characters, selectors, bidi marks and bidi controls.
///
/// The bidi controls are the embeddings and isolates; overrides have their own signal.
#[derive(Debug, Default)]
struct MixedRun {
    /// Characters in the run.
    len: usize,
    /// The run holds a zero-width character.
    zero_width: bool,
    /// The run holds a variation selector.
    selector: bool,
    /// The run holds a bidi mark.
    mark: bool,
    /// The run holds a bidi embedding or isolate.
    control: bool,
}

impl MixedRun {
    /// Reports whether the finished run is an `invisible_run`.
    ///
    /// It is when it has `MIXED_RUN_MIN` characters and either holds a zero-width character or
    /// a selector together with another class, or consists of bidi marks only. A run of one
    /// class of zero-width characters or selectors has its own signal; marks and controls
    /// without them are right-to-left formatting.
    fn is_hit(&self) -> bool {
        let classes = [self.zero_width, self.selector, self.mark, self.control];
        let mixed = classes.iter().filter(|present| **present).count() > 1;
        let marks_only = self.mark && !self.zero_width && !self.selector && !self.control;
        self.len >= MIXED_RUN_MIN && (((self.zero_width || self.selector) && mixed) || marks_only)
    }
}

/// Reports the character signals of `text` to `add`: tags, runs and bidi controls.
///
/// A variation selector directly after an ASCII letter also counts as a
/// `variation_selector_run`: no standardized sequence uses a Latin letter as its base.
///
/// `in_patch` is set for added patch lines, where embeddings and isolates are reported too.
pub(crate) fn scan(text: &str, in_patch: bool, add: &mut impl FnMut(Signal, u32)) {
    let mut tag_run: Option<TagRun> = None;
    let mut previous = '\n';
    let mut zero_width_len = 0_usize;
    let mut selector_len = 0_usize;
    let mut selector_after_letter = false;
    let mut mixed = MixedRun::default();
    for character in text.chars() {
        if transparent(character) {
            if let Some(run) = tag_run.take() {
                add(Signal::UnicodeTags, u32::from(!run.is_flag()));
            }
            previous = character;
            continue;
        }
        let (zero, selector, mark) = (
            zero_width(character),
            variation_selector(character),
            bidi_mark(character),
        );
        let control = bidi_embedding_or_isolate(character);
        if zero || selector || mark || control {
            mixed.len = mixed.len.saturating_add(1);
            mixed.zero_width |= zero;
            mixed.selector |= selector;
            mixed.mark |= mark;
            mixed.control |= control;
        } else {
            add(Signal::InvisibleRun, u32::from(mixed.is_hit()));
            mixed = MixedRun::default();
        }
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
        zero_width_len = if zero {
            zero_width_len.saturating_add(1)
        } else {
            0
        };
        if zero_width_len == RUN_MIN {
            add(Signal::ZeroWidthRun, 1);
        }
        selector_len = if selector {
            selector_len.saturating_add(1)
        } else {
            0
        };
        if selector_len == 1 {
            selector_after_letter = previous.is_ascii_alphabetic();
            add(
                Signal::VariationSelectorRun,
                u32::from(selector_after_letter),
            );
        }
        if selector_len == RUN_MIN && !selector_after_letter {
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
    add(Signal::InvisibleRun, u32::from(mixed.is_hit()));
}
