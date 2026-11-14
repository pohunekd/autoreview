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

/// Returns `line` without its indentation when that is at most `INDENT_MAX` spaces.
fn indented(line: &str) -> Option<&str> {
    let rest = line.trim_start_matches(' ');
    (line.len().saturating_sub(rest.len()) <= INDENT_MAX).then_some(rest)
}

/// Returns the fence character and length when `rest` opens a fenced code block.
fn opens_fence(rest: &str) -> Option<(u8, usize)> {
    let marker = *rest
        .as_bytes()
        .first()
        .filter(|byte| matches!(byte, b'`' | b'~'))?;
    let len = rest.bytes().take_while(|byte| *byte == marker).count();
    let info = rest.get(len..).unwrap_or_default();
    (len >= FENCE_MIN && !(marker == b'`' && info.contains('`'))).then_some((marker, len))
}

/// Reports whether `line` closes a fence of at least `len` times `marker`.
fn closes_fence(line: &str, marker: u8, len: usize) -> bool {
    indented(line).is_some_and(|rest| {
        let run = rest.bytes().take_while(|byte| *byte == marker).count();
        let after = rest.get(run..).unwrap_or_default();
        run >= len && after.trim_matches([' ', '\t']).is_empty()
    })
}

/// Returns how the HTML block starting at `rest` ends (CommonMark kinds 1-7).
///
/// Any `<` followed by a letter or `/` counts as kinds 6 and 7, more than CommonMark accepts,
/// so fences are recognized less often and more text is scanned.
fn html_start(rest: &str) -> Option<HtmlEnd> {
    let lower = rest
        .get(..rest.len().min(16))
        .unwrap_or(rest)
        .to_ascii_lowercase();
    let after_name = |name: &str| {
        lower
            .strip_prefix('<')
            .and_then(|tail| tail.strip_prefix(name))
            .is_some_and(|tail| tail.is_empty() || tail.starts_with([' ', '\t', '>']))
    };
    if lower.starts_with("<!--") {
        Some(HtmlEnd::Marker(&["-->"]))
    } else if lower.starts_with("<?") {
        Some(HtmlEnd::Marker(&["?>"]))
    } else if lower.starts_with("<![cdata[") {
        Some(HtmlEnd::Marker(&["]]>"]))
    } else if lower.starts_with("<!")
        && lower.as_bytes().get(2).is_some_and(u8::is_ascii_alphabetic)
    {
        Some(HtmlEnd::Marker(&[">"]))
    } else if RAW_TAGS.iter().any(|name| after_name(name)) {
        Some(HtmlEnd::Marker(&RAW_TAG_ENDS))
    } else if lower
        .as_bytes()
        .get(1)
        .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'/')
        && lower.starts_with('<')
    {
        Some(HtmlEnd::BlankLine)
    } else {
        None
    }
}

/// Reports whether `line` ends an HTML block that ends at `end`.
fn ends_html(line: &str, end: HtmlEnd, blank: bool) -> bool {
    match end {
        HtmlEnd::Marker(markers) => {
            let lower = line.to_ascii_lowercase();
            markers.iter().any(|marker| lower.contains(marker))
        }
        HtmlEnd::BlankLine => blank,
    }
}

/// A byte class that the forward pass looks for; one cached search per class.
#[derive(Debug, Clone, Copy)]
enum Find {
    /// `>`, the end of an HTML tag.
    TagEnd,
    /// An unescaped `]`.
    Bracket,
    /// An unescaped `"`.
    Quote,
    /// An unescaped `'`.
    Apostrophe,
    /// An unescaped `)`.
    Paren,
    /// ASCII whitespace.
    Space,
    /// Anything but ASCII whitespace.
    NonSpace,
}

/// Number of `Find` classes.
const FIND_CLASSES: usize = 7;

/// The last search of one class: where it started and what it found.
#[derive(Debug, Clone, Copy)]
struct Cached {
    /// The byte offset the search started at.
    from: usize,
    /// The first match at or after `from`.
    found: Option<usize>,
}

/// The forward pass over one rendered field.
struct Pass<'a> {
    /// The field text.
    text: &'a str,
    /// The field text as bytes; every offset the pass keeps is at an ASCII byte or the end.
    bytes: &'a [u8],
    /// The last search of each `Find` class.
    cache: [Option<Cached>; FIND_CLASSES],
}
