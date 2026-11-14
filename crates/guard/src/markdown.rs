//! Hidden Markdown: text that GitHub does not show, reported when it instructs a reviewer.
//!
//! One forward pass finds HTML comments, `<details>` elements, elements marked `hidden` or
//! hidden by their style, HTML tag attributes, image alt texts, link titles and reference
//! definitions, and applies the cue rule (`words::instructs`) to the hidden text. Closed fenced
//! code blocks are skipped, because GitHub shows their content as code. Every other doubt is
//! resolved towards scanning: inline code spans, indented code and lists are scanned too.

// Rust guideline compliant 2026-06-26

use crate::{Signal, words};

/// Most indentation spaces of a fence, HTML block or reference definition (CommonMark).
const INDENT_MAX: usize = 3;

/// Shortest fence of backticks or tildes.
const FENCE_MIN: usize = 3;

/// Tags that start an HTML block only their closing tags end (kind 1).
const RAW_TAGS: [&str; 4] = ["pre", "script", "style", "textarea"];

/// The closing tags that end a kind 1 HTML block.
const RAW_TAG_ENDS: [&str; 4] = ["</pre>", "</script>", "</style>", "</textarea>"];

/// Style declarations that hide an element, compared lowercased and without whitespace.
const HIDING_STYLES: [&str; 2] = ["display:none", "visibility:hidden"];

/// How an HTML block ends; no fence opens inside it, so its comments are not code.
#[derive(Debug, Clone, Copy)]
enum HtmlEnd {
    /// At the first line that contains one of the markers (ASCII case ignored).
    Marker(&'static [&'static str]),
    /// At the first blank line.
    BlankLine,
}

/// The block that the line scan of `code_ranges` is in.
#[derive(Debug, Clone, Copy)]
enum Block {
    /// Text outside fences and HTML blocks.
    Text,
    /// An HTML block.
    Html(HtmlEnd),
    /// A fenced code block opened by `len` times `marker` at byte `start`.
    Fence {
        marker: u8,
        len: usize,
        start: usize,
    },
}

/// Returns the byte ranges of closed fenced code blocks, fence lines included.
///
/// A fence opens only at the start of the text or after a blank line, never inside an HTML
/// block, and counts only when a closing fence follows. Each rule is stricter than CommonMark,
/// so a range is always code that GitHub shows; text it misses is scanned.
fn code_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut block = Block::Text;
    let mut after_blank = true;
    let mut offset = 0_usize;
    for raw in text.split_inclusive('\n') {
        let start = offset;
        offset = offset.saturating_add(raw.len());
        let line = raw.trim_end_matches(['\n', '\r']);
        let blank = line.trim_matches([' ', '\t']).is_empty();
        match block {
            Block::Fence {
                marker,
                len,
                start: open,
            } => {
                if closes_fence(line, marker, len) {
                    ranges.push((open, offset));
                    block = Block::Text;
                    after_blank = false;
                }
                continue;
            }
            Block::Html(end) => {
                if ends_html(line, end, blank) {
                    block = Block::Text;
                    after_blank = blank;
                }
                continue;
            }
            Block::Text => {}
        }
        if blank {
            after_blank = true;
            continue;
        }
        if let Some(rest) = indented(line) {
            if let Some((marker, len)) = opens_fence(rest).filter(|_| after_blank) {
                block = Block::Fence { marker, len, start };
            } else if let Some(end) = html_start(rest)
                && !ends_html(line, end, false)
            {
                block = Block::Html(end);
            }
        }
        after_blank = false;
    }
    ranges
}
