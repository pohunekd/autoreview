//! Bounded, neutralized reasons that the engine logs from untrusted reviewer output.
//!
//! A reason quotes what a reviewer program wrote, so it is cut to `REASON_MAX_BYTES` and every
//! character that could make a log line read differently than it is shows as `?`. It only goes
//! into the `error` field of a log line: a recorded failure keeps only the code.

// Rust guideline compliant 2026-11-11

use crate::protocol::go_utf8_lossy;

/// Most UTF-8 bytes of a logged reason, as the Codex adapter's `REASON_MAX_BYTES`.
const REASON_MAX_BYTES: usize = 512;

/// Every character of general category Cf (format) in Unicode 16.0, as inclusive ranges.
///
/// A logged reason shows them as `?`: bidi controls, soft hyphens, tags and invisible
/// characters could make a log line read differently than it is. The adapter neutralizes by
/// Python's Unicode data, which may be another version; the engine applies this table anyway.
const FORMAT_CHARACTERS: [(char, char); 21] = [
    ('\u{00AD}', '\u{00AD}'),
    ('\u{0600}', '\u{0605}'),
    ('\u{061C}', '\u{061C}'),
    ('\u{06DD}', '\u{06DD}'),
    ('\u{070F}', '\u{070F}'),
    ('\u{0890}', '\u{0891}'),
    ('\u{08E2}', '\u{08E2}'),
    ('\u{180E}', '\u{180E}'),
    ('\u{200B}', '\u{200F}'),
    ('\u{202A}', '\u{202E}'),
    ('\u{2060}', '\u{2064}'),
    ('\u{2066}', '\u{206F}'),
    ('\u{FEFF}', '\u{FEFF}'),
    ('\u{FFF9}', '\u{FFFB}'),
    ('\u{110BD}', '\u{110BD}'),
    ('\u{110CD}', '\u{110CD}'),
    ('\u{13430}', '\u{1343F}'),
    ('\u{1BCA0}', '\u{1BCA3}'),
    ('\u{1D173}', '\u{1D17A}'),
    ('\u{E0001}', '\u{E0001}'),
    ('\u{E0020}', '\u{E007F}'),
];

/// The line separator (Zl) and the paragraph separator (Zp), the only characters of either.
const LINE_SEPARATORS: [char; 2] = ['\u{2028}', '\u{2029}'];

/// Returns the bounded reason at the start of `bytes`.
///
/// The first `REASON_MAX_BYTES` bytes are decoded lossily and neutralized; the text is then
/// cut to `REASON_MAX_BYTES` UTF-8 bytes at a character boundary, as the adapter cuts it.
pub(crate) fn leading_reason(bytes: &[u8]) -> String {
    let cut = bytes.get(..REASON_MAX_BYTES).unwrap_or(bytes);
    let mut reason = neutralized(&go_utf8_lossy(cut));
    reason.truncate(reason.floor_char_boundary(REASON_MAX_BYTES));
    reason
}

/// Returns the bounded reason at the end of `bytes`, such as a stderr.
///
/// The bytes are decoded lossily, stripped of ASCII whitespace at both ends and neutralized,
/// so a line break inside shows as `?`. The last `REASON_MAX_BYTES` UTF-8 bytes are kept,
/// starting at a character boundary.
pub(crate) fn trailing_reason(bytes: &[u8]) -> String {
    let text = go_utf8_lossy(bytes);
    let reason = neutralized(text.trim_matches(|character: char| character.is_ascii_whitespace()));
    let start = reason.ceil_char_boundary(reason.len().saturating_sub(REASON_MAX_BYTES));
    reason.get(start..).unwrap_or_default().to_owned()
}

/// Returns `text` with each character that `is_neutralized` names replaced by `?`.
fn neutralized(text: &str) -> String {
    text.chars()
        .map(|character| {
            if is_neutralized(character) {
                '?'
            } else {
                character
            }
        })
        .collect()
}

/// Reports whether a logged reason shows `character` as `?`.
///
/// Controls (Cc), format characters (Cf) and the line and paragraph separators (Zl, Zp).
fn is_neutralized(character: char) -> bool {
    character.is_control()
        || LINE_SEPARATORS.contains(&character)
        || FORMAT_CHARACTERS
            .iter()
            .any(|(low, high)| (*low..=*high).contains(&character))
}
