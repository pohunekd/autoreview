//! Table tests over the synthetic fixture corpus in `testdata/`.
//!
//! A corpus file is ASCII; a non-ASCII character is written `<U+XXXX>`. Lines before the
//! first row and lines starting with `#` before a row's first field are comments.
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
const SIGNALS: [Signal; 12] = [
    Signal::UnicodeTags,
    Signal::ZeroWidthRun,
    Signal::VariationSelectorRun,
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

/// Parses a corpus file into rows.
fn rows(corpus: &str) -> Vec<Row> {
    assert!(
        corpus
            .bytes()
            .all(|byte| byte == b'\n' || (b' '..=b'~').contains(&byte)),
        "corpus files are printable ASCII; write other characters as <U+XXXX>"
    );
    let mut rows: Vec<Row> = Vec::new();
    let mut text: Option<Vec<&str>> = None;
    let mut repeat = 1;
    let close = |rows: &mut Vec<Row>, text: &mut Option<Vec<&str>>, repeat: usize| {
        if let (Some(lines), Some(row)) = (text.take(), rows.last_mut()) {
            let field = row.fields.last_mut().expect("open field");
            field.1 = decode(&lines.join("\n")).repeat(repeat);
        }
    };
    for line in corpus.lines() {
        if let Some(name) = line.strip_prefix("== ") {
            close(&mut rows, &mut text, repeat);
            let expected = Report {
                hits: Vec::new(),
                score: 0,
            };
            rows.push(Row {
                name: name.to_owned(),
                expected,
                fields: Vec::new(),
            });
        } else if let Some(header) = line.strip_prefix("--- ") {
            close(&mut rows, &mut text, repeat);
            let (name, times) = header.split_once(" x").unwrap_or((header, "1"));
            repeat = times.parse().expect("repeat count");
            let row = rows.last_mut().expect("field inside a row");
            row.fields.push((kind(name), String::new()));
            text = Some(Vec::new());
        } else if let Some(lines) = text.as_mut() {
            lines.push(line);
        } else if let Some(row) = rows.last_mut() {
            if let Some(hits) = line.strip_prefix("expect: ") {
                row.expected.hits = expected_hits(hits);
            } else if let Some(score) = line.strip_prefix("score: ") {
                row.expected.score = score.parse().expect("score");
            } else {
                assert!(
                    line.starts_with('#'),
                    "row {}: unknown line {line:?}",
                    row.name
                );
            }
        }
    }
    close(&mut rows, &mut text, repeat);
    rows
}
