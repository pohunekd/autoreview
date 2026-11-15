//! Table tests over the synthetic fixture corpus in `testdata/`.
//!
//! A corpus file is ASCII; a non-ASCII character is written `<U+XXXX>`, and
//! `<REP n>text</REP>` stands for `text` repeated `n` times. Lines before the first row and
//! lines starting with `#` before a row's first field are comments.
//!
//! ```text
//! == row_name
//! expect: body:hidden_comment_instruction=1 body:lexicon_phrase=2
//! score: 165
//! --- body
//! the text, up to the next `---` or `==` line, lines joined by a line break
//! --- comment x3
//! a text repeated three times
//! ```
//!
//! `expect: none` means no hit. Every row is scanned with the lexicon `testdata/phrases.txt`
//! and must give exactly the expected hits and score.

use crate::{Field, FieldKind, Hit, Lexicon, Report, Signal, scan};

/// Every field kind, to read the kinds of the corpus by their stable identifier.
const KINDS: [FieldKind; 6] = [
    FieldKind::Title,
    FieldKind::Body,
    FieldKind::CommitMessage,
    FieldKind::Comment,
    FieldKind::ReviewThread,
    FieldKind::Patch,
];

/// Every signal, to read the codes of the corpus by their stable code.
const SIGNALS: [Signal; 13] = [
    Signal::UnicodeTags,
    Signal::ZeroWidthRun,
    Signal::VariationSelectorRun,
    Signal::InvisibleRun,
    Signal::BidiOverride,
    Signal::BidiControlInPatch,
    Signal::HiddenCommentInstruction,
    Signal::CollapsedBlockInstruction,
    Signal::HiddenElementInstruction,
    Signal::LinkMetadataInstruction,
    Signal::ReferenceDefinitionInstruction,
    Signal::ChatTemplateToken,
    Signal::LexiconPhrase,
];

/// One row of a corpus file.
struct Row {
    /// The row name, shown when the row fails.
    name: String,
    /// The expected report.
    expected: Report,
    /// The fields to scan, in corpus order.
    fields: Vec<(FieldKind, String)>,
}

/// Returns the field kind with the identifier `name`.
fn kind(name: &str) -> FieldKind {
    *KINDS
        .iter()
        .find(|kind| kind.as_str() == name)
        .expect("known field kind")
}

/// Replaces every `<U+XXXX>` of a corpus text with its character.
fn decode(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("<U+") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 3..];
        let end = after.find('>').expect("closed <U+ escape");
        let code = u32::from_str_radix(&after[..end], 16).expect("hex escape");
        out.push(char::from_u32(code).expect("escape is a character"));
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Replaces every `<REP n>text</REP>` of a corpus text with `text` repeated `n` times.
fn expand(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("<REP ") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 5..];
        let close = after.find('>').expect("closed <REP");
        let times: usize = after[..close].parse().expect("repeat count");
        let body = &after[close + 1..];
        let end = body.find("</REP>").expect("closed </REP>");
        out.push_str(&body[..end].repeat(times));
        rest = &body[end + 6..];
    }
    out.push_str(rest);
    out
}
