//! Hidden Markdown: text that GitHub does not show, reported when it instructs a reviewer.
//!
//! A GitHub Flavored Markdown parser (comrak) decides what is code, what is raw HTML, what is
//! a link title and what is alt text, as GitHub's own parser does. The raw HTML pieces are then
//! read in document order: comments and other markup, tag attributes, and the elements that
//! hide their content (`<details>`, `hidden`, a hiding style, `script`/`style`/`template`).
//! Reference definitions and unreferenced footnotes render nothing, so they are found as the
//! source lines that no rendered block covers. Each hidden text goes through the cue rule.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeMap;

use comrak::arena_tree::NodeEdge;
use comrak::nodes::{AstNode, NodeValue, Sourcepos};
use comrak::{Arena, Options, parse_document};

use crate::{Signal, words};

/// Style declarations that hide an element, compared lowercased and without whitespace.
const HIDING_STYLES: [&str; 2] = ["display:none", "visibility:hidden"];

/// Elements whose content a page never shows.
///
/// Opened inline, they hide only up to the end of their paragraph: an unclosed one there is
/// usually prose that names the tag.
const HIDDEN_ELEMENTS: [&str; 3] = ["script", "style", "template"];

/// Raw HTML that is never shown besides tags: opener, closer and how the opener is matched.
const HIDDEN_MARKUP: [(&str, &str); 4] = [
    ("<!--", "-->"),
    ("<![cdata[", "]]>"),
    ("<?", "?>"),
    ("<!", ">"),
];

/// Most bytes of a raw HTML piece compared, lowercased, with the `HIDDEN_MARKUP` openers.
const OPENER_MAX: usize = 9;

/// Returns the parser options of GitHub comments and descriptions (GFM extensions).
fn options() -> Options<'static> {
    let mut options = Options::default();
    options.extension.strikethrough = true;
    options.extension.table = true;
    options.extension.autolink = true;
    options.extension.tasklist = true;
    options.extension.footnotes = true;
    options
}

/// Byte offsets of the line starts of a text, for source positions.
struct Lines {
    /// The offset of each line start, the first line at index 0.
    starts: Vec<usize>,
    /// The length of the text.
    len: usize,
}

impl Lines {
    /// Indexes the line starts of `text`.
    fn new(text: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(at, _)| at.saturating_add(1)));
        Self {
            starts,
            len: text.len(),
        }
    }

    /// Returns the byte offset of a 1-based line and byte column.
    fn offset(&self, line: usize, column: usize) -> usize {
        let start = self
            .starts
            .get(line.saturating_sub(1))
            .copied()
            .unwrap_or(self.len);
        start.saturating_add(column.saturating_sub(1)).min(self.len)
    }

    /// Returns the byte range of a source position, whose end column is inclusive.
    fn range(&self, position: Sourcepos) -> (usize, usize) {
        let start = self.offset(position.start.line, position.start.column);
        let end = self
            .offset(position.end.line, position.end.column)
            .saturating_add(1)
            .min(self.len);
        (start, end.max(start))
    }
}

/// An open element that hides its content.
#[derive(Debug)]
struct Frame {
    /// The lowercase element name.
    name: String,
    /// The signal its content gives when it instructs.
    signal: Signal,
    /// Opened inline, the element hides only up to the end of its paragraph.
    paragraph_only: bool,
    /// Its text, code included.
    all: String,
    /// Its text without code.
    plain: String,
}

/// The walk over one parsed field.
struct Walk<'a, F: FnMut(Signal, u32)> {
    /// Where signals go.
    add: &'a mut F,
    /// The open hiding elements, outermost first; only the outermost collects text.
    frames: Vec<Frame>,
    /// How many open frames have each element name.
    open: BTreeMap<String, usize>,
    /// The alt text of the image being read.
    alt: Option<String>,
}
