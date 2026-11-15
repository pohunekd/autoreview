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

/// Shortest run of bidi marks and controls alone that is reported.
///
/// Arabic, Hebrew and Persian text puts marks and isolates around numbers and Latin words, at
/// most two or three in a row (`PDI PDI RLM` in nested isolates); a payload needs many more.
/// Whitespace keeps such a run open, so marks spread over spaces count together.
const MARKS_ONLY_MIN: usize = 6;

/// BRAILLE PATTERN BLANK: not whitespace, but shown as an empty cell.
const BRAILLE_BLANK: char = '\u{2800}';

/// Reports whether a character can keep a run of invisible characters open.
///
/// Whitespace and blank cells show nothing either, so characters spread over them still
/// form one run, once the run holds a character that natural text does not use
/// (`MixedRun::unnatural`).
fn transparent(character: char) -> bool {
    character.is_whitespace() || character == BRAILLE_BLANK
}

/// COMBINING ENCLOSING KEYCAP: a digit, `#` or `*`, a selector and it form an emoji.
const KEYCAP: char = '\u{20E3}';

/// Reports whether a selector after `base`, followed by `next`, has a text base.
///
/// No standardized or emoji sequence starts with ASCII, whitespace, or a Latin, Greek or
/// Cyrillic letter; the one exception is a keycap (`1`, VS16, U+20E3). Emoji, CJK, math and
/// Mongolian bases stay legitimate.
fn text_base(base: char, next: Option<char>) -> bool {
    let keycap = matches!(base, '0'..='9' | '#' | '*') && next == Some(KEYCAP);
    !keycap
        && (base.is_ascii()
            || base.is_whitespace()
            || matches!(base, '\u{00C0}'..='\u{024F}' | '\u{0370}'..='\u{052F}'))
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
