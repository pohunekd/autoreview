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

/// Indentation of an indented code block line.
const CODE_INDENT: &str = "    ";

/// Most digits of an ordered list item marker, as in CommonMark.
const LIST_DIGITS_MAX: usize = 9;

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
    /// An indented code block from byte `start`; its last indented line ends at `end`.
    Indented { start: usize, end: usize },
}

/// Returns the length of the list item marker that starts `rest`, space included.
///
/// A marker is `-`, `*` or `+`, or 1 to `LIST_DIGITS_MAX` digits and `.` or `)`, followed by
/// a space, a tab or the end of the line.
fn list_marker(rest: &[u8]) -> Option<usize> {
    let digits = rest
        .iter()
        .take(LIST_DIGITS_MAX.saturating_add(1))
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    let marker = match rest.first() {
        Some(b'-' | b'*' | b'+') => 1,
        Some(_)
            if (1..=LIST_DIGITS_MAX).contains(&digits)
                && matches!(rest.get(digits), Some(b'.' | b')')) =>
        {
            digits.saturating_add(1)
        }
        _ => return None,
    };
    match rest.get(marker) {
        Some(b' ' | b'\t') => Some(marker.saturating_add(1)),
        None | Some(b'\n' | b'\r') => Some(marker),
        Some(_) => None,
    }
}

/// Returns the byte ranges of the code blocks of `text` that are certainly code.
///
/// A fenced block counts when it opens in column 0 (a fence inside a list item ends with the
/// item), at the start of the text or after a blank line, never inside an HTML block, and a
/// closing fence follows; the range includes the fence lines. An indented block counts when
/// its lines start with four spaces, it follows a blank line or the start of the text, and no
/// list item or footnote came before it in the text (their continuation lines are indented
/// too). Each rule is stricter than CommonMark, so a range is always code that GitHub shows;
/// text it misses is scanned.
pub(crate) fn code_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut block = Block::Text;
    let mut after_blank = true;
    let mut container_seen = false;
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
            Block::Indented { start: open, end } => {
                if blank || line.starts_with(CODE_INDENT) {
                    if !blank {
                        block = Block::Indented {
                            start: open,
                            end: offset,
                        };
                    }
                    after_blank = blank;
                    continue;
                }
                ranges.push((open, end));
                block = Block::Text;
            }
            Block::Text => {}
        }
        if blank {
            after_blank = true;
            continue;
        }
        if after_blank && !container_seen && line.starts_with(CODE_INDENT) {
            block = Block::Indented { start, end: offset };
            continue;
        }
        if let Some(rest) = indented(line) {
            container_seen |= list_marker(rest.as_bytes()).is_some() || rest.starts_with("[^");
            let column_zero = rest.len() == line.len();
            if let Some((marker, len)) = opens_fence(rest).filter(|_| after_blank && column_zero) {
                block = Block::Fence { marker, len, start };
            } else if let Some(end) = html_start(rest)
                && !ends_html(line, end, false)
            {
                block = Block::Html(end);
            }
        }
        after_blank = false;
    }
    if let Block::Indented { start, end } = block {
        ranges.push((start, end));
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

/// What a cached search looks for.
#[derive(Debug, Clone, Copy)]
enum Class {
    /// One byte.
    Byte(u8),
    /// One byte that no odd run of backslashes escapes.
    Unescaped(u8),
    /// The end of a `<...>` link destination: an unescaped `>`, `<` or a line break.
    AngleEnd,
    /// An unescaped `]` followed by `(` or `[`.
    LinkClose,
    /// ASCII whitespace.
    Space,
    /// Anything but ASCII whitespace.
    NonSpace,
}

/// Cache slot of the first `>` after a tag name.
const TAG_END: usize = 0;
/// Cache slot of the `"` that closes a double-quoted attribute value.
const ATTRIBUTE_QUOTE: usize = 1;
/// Cache slot of the `'` that closes a single-quoted attribute value.
const ATTRIBUTE_APOSTROPHE: usize = 2;
/// Cache slot of the first `>` after a quoted attribute value.
const VALUE_END: usize = 3;
/// Cache slot of the start of a link destination.
const DESTINATION: usize = 4;
/// Cache slot of the whitespace after a link destination.
const DESTINATION_END: usize = 5;
/// Cache slot of the end of a `<...>` link destination.
const ANGLE_END: usize = 6;
/// Cache slot of the `]` that closes a reference definition label.
const LABEL_END: usize = 7;
/// First cache slot of a link title after a plain destination.
const TITLE_AFTER_PLAIN: usize = 8;
/// First cache slot of a link title after a `<...>` destination.
const TITLE_AFTER_ANGLE: usize = 13;
/// Offset of the title opener slot within a title slot group.
const TITLE_OPEN: usize = 0;
/// Offset of the `)` after the title within a title slot group.
const TITLE_AFTER: usize = 4;
/// Cache slot of the fallback `]` of an alt text.
const LINK_CLOSE: usize = 18;
/// Number of cache slots.
const SLOTS: usize = 19;

/// The last search of one slot: where it started and what it found.
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
    /// The last search of each cache slot.
    cache: [Option<Cached>; SLOTS],
    /// The last paragraph end search.
    paragraph: Option<Cached>,
    /// Matching unescaped `[` and `]` offsets, sorted by the `[`.
    brackets: Vec<(usize, usize)>,
}

/// Reports whether the byte at `at` is escaped: an odd run of backslashes precedes it.
fn escaped(bytes: &[u8], at: usize) -> bool {
    let before = bytes.get(..at).unwrap_or_default();
    before
        .iter()
        .rev()
        .take_while(|byte| **byte == b'\\')
        .count()
        % 2
        == 1
}

/// Returns the matching unescaped `[` and `]` pairs of `bytes`, sorted by `[`.
fn bracket_pairs(bytes: &[u8]) -> Vec<(usize, usize)> {
    let mut open = Vec::new();
    let mut pairs = Vec::new();
    for (at, byte) in bytes.iter().enumerate() {
        if matches!(byte, b'[' | b']') && escaped(bytes, at) {
            continue;
        }
        if *byte == b'[' {
            open.push(at);
        } else if *byte == b']'
            && let Some(start) = open.pop()
        {
            pairs.push((start, at));
        }
    }
    pairs.sort_unstable();
    pairs
}

/// Returns the first quote that opens an attribute value in `start..end`, and its offset.
///
/// A value is quoted when `=` and optional whitespace precede the quote.
fn value_quote(bytes: &[u8], start: usize, end: usize) -> Option<(u8, usize)> {
    let mut at = start;
    while at < end {
        if bytes.get(at) == Some(&b'=') {
            let mut quote = at.saturating_add(1);
            while quote < end && bytes.get(quote).is_some_and(u8::is_ascii_whitespace) {
                quote = quote.saturating_add(1);
            }
            match bytes.get(quote) {
                Some(&byte @ (b'"' | b'\'')) if quote < end => return Some((byte, quote)),
                _ => at = quote,
            }
        } else {
            at = at.saturating_add(1);
        }
    }
    None
}

impl<'a> Pass<'a> {
    /// Prepares the pass over `text`.
    fn new(text: &'a str) -> Self {
        Self {
            text,
            bytes: text.as_bytes(),
            cache: [None; SLOTS],
            paragraph: None,
            brackets: bracket_pairs(text.as_bytes()),
        }
    }

    /// Returns the first offset at or after `from` that matches `class`, cached in `slot`.
    ///
    /// Each slot serves one call site, whose start offsets only grow while the pass runs. A
    /// search that starts between an earlier start and its result has the same result, so
    /// each slot reads each byte a bounded number of times.
    fn find(&mut self, slot: usize, class: Class, from: usize) -> Option<usize> {
        let cached = self.cache.get_mut(slot)?;
        if let Some(last) = *cached
            && last.from <= from
            && last.found.is_none_or(|found| found >= from)
        {
            return last.found;
        }
        let bytes = self.bytes;
        let found = (from..bytes.len()).find(|&at| {
            let byte = bytes.get(at).copied().unwrap_or_default();
            match class {
                Class::Byte(wanted) => byte == wanted,
                Class::Unescaped(wanted) => byte == wanted && !escaped(bytes, at),
                Class::AngleEnd => matches!(byte, b'>' | b'<' | b'\n') && !escaped(bytes, at),
                Class::LinkClose => {
                    byte == b']'
                        && matches!(bytes.get(at.saturating_add(1)), Some(b'(' | b'['))
                        && !escaped(bytes, at)
                }
                Class::Space => byte.is_ascii_whitespace(),
                Class::NonSpace => !byte.is_ascii_whitespace(),
            }
        });
        *cached = Some(Cached { from, found });
        found
    }

    /// Returns the offset of the line break that ends the paragraph holding `from`.
    ///
    /// A paragraph ends before a blank line; without one it ends at the end of the text.
    fn paragraph_end(&mut self, from: usize) -> usize {
        let len = self.bytes.len();
        if let Some(last) = self.paragraph
            && last.from <= from
            && last.found.is_some_and(|found| found >= from)
        {
            return last.found.unwrap_or(len);
        }
        let mut at = from;
        let end = loop {
            let Some(found) = self.slice(at, len).find('\n') else {
                break len;
            };
            let line_break = at.saturating_add(found);
            let next = line_break.saturating_add(1);
            let next_end = self
                .slice(next, len)
                .find('\n')
                .map_or(len, |end| next + end);
            if self
                .slice(next, next_end)
                .trim_matches([' ', '\t', '\r'])
                .is_empty()
            {
                break line_break;
            }
            at = next;
        };
        self.paragraph = Some(Cached {
            from,
            found: Some(end),
        });
        end
    }

    /// Returns the text between two offsets of the pass.
    fn slice(&self, start: usize, end: usize) -> &str {
        self.text.get(start..end).unwrap_or_default()
    }

    /// Reports whether the text at `at` starts with `prefix`, ignoring ASCII case.
    fn starts_with(&self, at: usize, prefix: &str) -> bool {
        self.bytes
            .get(at..at.saturating_add(prefix.len()))
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix.as_bytes()))
    }

    /// Reports whether the tag name `name` starts at `at`.
    fn tag_named(&self, at: usize, name: &str) -> bool {
        self.starts_with(at, name)
            && self
                .bytes
                .get(at.saturating_add(name.len()))
                .is_none_or(|byte| byte.is_ascii_whitespace() || matches!(byte, b'>' | b'/'))
    }

    /// Returns the content range and the end of the comment at `at`.
    fn comment(&self, at: usize) -> (usize, usize, usize) {
        let open = at.saturating_add(4);
        if self.starts_with(open, ">") {
            return (open, open, open.saturating_add(1));
        }
        if self.starts_with(open, "->") {
            return (open, open, open.saturating_add(2));
        }
        match self.slice(open, self.bytes.len()).find("-->") {
            Some(close) => {
                let close = open.saturating_add(close);
                (open, close, close.saturating_add(3))
            }
            None => (open, self.bytes.len(), self.bytes.len()),
        }
    }

    /// Returns the offset after the closing tag of the element `name`.
    ///
    /// Its content starts at `from`. Nested elements of the same name count, comments are
    /// skipped, and an element that is never closed runs to the end of the text.
    fn element_end(&self, from: usize, name: &str) -> usize {
        let len = self.bytes.len();
        let mut depth = 1_usize;
        let mut at = from;
        while let Some(found) = self.slice(at, len).find('<') {
            at = at.saturating_add(found);
            if self.starts_with(at, "<!--") {
                at = self.comment(at).2;
            } else if self.starts_with(at, "</") && self.tag_named(at.saturating_add(2), name) {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let gt = self.slice(at, len).find('>');
                    return gt.map_or(len, |gt| at.saturating_add(gt).saturating_add(1));
                }
                at = at.saturating_add(2);
            } else {
                if self.tag_named(at.saturating_add(1), name) {
                    depth = depth.saturating_add(1);
                }
                at = at.saturating_add(1);
            }
        }
        len
    }

    /// Returns where the tag with attributes at `from` ends, and whether it is closed.
    ///
    /// A `>` inside a quoted attribute value does not end the tag. A closed tag ends at its
    /// `>`; a value that is never closed, or no `>` after it, ends the tag-like text there
    /// (`false`). Without any `>` after `from` there is no tag.
    fn tag_end(&mut self, from: usize) -> Option<(usize, bool)> {
        let mut gt = self.find(TAG_END, Class::Byte(b'>'), from)?;
        let mut at = from;
        while let Some((quote, open)) = value_quote(self.bytes, at, gt) {
            let slot = if quote == b'"' {
                ATTRIBUTE_QUOTE
            } else {
                ATTRIBUTE_APOSTROPHE
            };
            let after_open = open.saturating_add(1);
            let Some(close) = self.find(slot, Class::Byte(quote), after_open) else {
                return Some((after_open, false));
            };
            at = close.saturating_add(1);
            let Some(next) = self.find(VALUE_END, Class::Byte(b'>'), at) else {
                return Some((at, false));
            };
            gt = next;
        }
        Some((gt, true))
    }
}

impl Pass<'_> {
    /// Handles the `<` at `at` and returns the offset after what it consumed.
    ///
    /// A name followed by `:` is an autolink such as `<https://...>`, shown as a link.
    fn tag(&mut self, at: usize, add: &mut impl FnMut(Signal, u32)) -> Option<usize> {
        if self.starts_with(at, "<!--") {
            let (open, close, end) = self.comment(at);
            report(
                add,
                Signal::HiddenCommentInstruction,
                self.slice(open, close),
            );
            return Some(end);
        }
        let name_start = at.saturating_add(1);
        let name_len = self
            .bytes
            .get(name_start..)
            .unwrap_or_default()
            .iter()
            .take_while(|byte| byte.is_ascii_alphanumeric() || **byte == b'-')
            .count();
        if !self
            .bytes
            .get(name_start)
            .is_some_and(u8::is_ascii_alphabetic)
        {
            return None;
        }
        let name_end = name_start.saturating_add(name_len);
        if self.bytes.get(name_end) == Some(&b':') {
            return None;
        }
        let (gt, closed) = self.tag_end(name_end)?;
        let name = self.slice(name_start, name_end);
        let attributes = self.slice(name_end, gt);
        let signal = if !closed {
            report(add, Signal::LinkMetadataInstruction, attributes);
            return Some(gt);
        } else if name.eq_ignore_ascii_case("details") {
            Signal::CollapsedBlockInstruction
        } else if hides(attributes) {
            Signal::HiddenElementInstruction
        } else {
            report(add, Signal::LinkMetadataInstruction, attributes);
            return Some(gt.saturating_add(1));
        };
        let end = self.element_end(gt.saturating_add(1), name);
        report(add, signal, self.slice(at, end));
        Some(end)
    }

    /// Handles `![` at `at`: an image alt text, reported when it instructs.
    ///
    /// The alt text runs to the matching `]`, so nested brackets stay inside it. When that `]`
    /// starts no destination (a code span can hold a `]`), it runs to the next `](` or `][` of
    /// the paragraph instead.
    fn image_alt(&mut self, at: usize, add: &mut impl FnMut(Signal, u32)) -> Option<usize> {
        if !self.starts_with(at, "![") {
            return None;
        }
        let open = at.saturating_add(1);
        let paired = self
            .brackets
            .binary_search_by_key(&open, |&(start, _)| start)
            .ok()
            .and_then(|index| self.brackets.get(index))
            .map(|&(_, close)| close)
            .filter(|close| matches!(self.bytes.get(close.saturating_add(1)), Some(b'(' | b'[')));
        let close = match paired {
            Some(close) => close,
            None => {
                let close = self.find(LINK_CLOSE, Class::LinkClose, at.saturating_add(2))?;
                if close >= self.paragraph_end(at) {
                    return None;
                }
                close
            }
        };
        report(
            add,
            Signal::LinkMetadataInstruction,
            self.slice(at.saturating_add(2), close),
        );
        Some(close)
    }

    /// Handles `](` at `at`: the title of an inline link or image, reported when it instructs.
    ///
    /// The destination is a `<...>` on one line or the run up to the next whitespace; it is
    /// not shown either, so it is checked with the title. A title counts only when `)`
    /// follows it; a failed parse consumes nothing.
    fn link_title(&mut self, at: usize, add: &mut impl FnMut(Signal, u32)) -> Option<usize> {
        if !self.starts_with(at, "](") {
            return None;
        }
        let destination = self.find(DESTINATION, Class::NonSpace, at.saturating_add(2))?;
        let (slots, after_destination) = if self.bytes.get(destination) == Some(&b'<') {
            let end = self.find(ANGLE_END, Class::AngleEnd, destination.saturating_add(1))?;
            if self.bytes.get(end) != Some(&b'>') {
                return None;
            }
            (TITLE_AFTER_ANGLE, end.saturating_add(1))
        } else {
            let end = self.find(DESTINATION_END, Class::Space, destination)?;
            (TITLE_AFTER_PLAIN, end)
        };
        let open_slot = slots.saturating_add(TITLE_OPEN);
        let open = self.find(open_slot, Class::NonSpace, after_destination)?;
        let (offset, closer) = match self.bytes.get(open) {
            Some(b'"') => (1, b'"'),
            Some(b'\'') => (2, b'\''),
            Some(b'(') => (3, b')'),
            _ => return None,
        };
        let close_slot = slots.saturating_add(offset);
        let close = self.find(close_slot, Class::Unescaped(closer), open.saturating_add(1))?;
        let after_slot = slots.saturating_add(TITLE_AFTER);
        let end = self.find(after_slot, Class::NonSpace, close.saturating_add(1))?;
        if self.bytes.get(end) != Some(&b')') {
            return None;
        }
        report(
            add,
            Signal::LinkMetadataInstruction,
            self.slice(destination, close),
        );
        Some(end.saturating_add(1))
    }

    /// Handles a line that starts at `at` with a reference definition `[label]: ...`.
    ///
    /// GitHub shows nothing of it, also inside a block quote and when its label or title
    /// continues on the next lines, so the definition runs to the end of its paragraph. A
    /// footnote `[^label]:` is shown and does not count.
    fn reference_definition(
        &mut self,
        at: usize,
        add: &mut impl FnMut(Signal, u32),
    ) -> Option<usize> {
        let mut start = at;
        loop {
            let spaces = self
                .bytes
                .get(start..)
                .unwrap_or_default()
                .iter()
                .take_while(|byte| **byte == b' ')
                .count();
            if spaces > INDENT_MAX {
                return None;
            }
            start = start.saturating_add(spaces);
            if self.bytes.get(start) != Some(&b'>') {
                break;
            }
            start = start.saturating_add(1);
        }
        if self.bytes.get(start) != Some(&b'[') || self.starts_with(start, "[^") {
            return None;
        }
        let label = start.saturating_add(1);
        let close = self.find(LABEL_END, Class::Unescaped(b']'), label)?;
        let end = self.paragraph_end(start);
        if close >= end
            || self.bytes.get(close.saturating_add(1)) != Some(&b':')
            || self.slice(label, close).trim().is_empty()
        {
            return None;
        }
        report(
            add,
            Signal::ReferenceDefinitionInstruction,
            self.slice(start, end),
        );
        Some(end)
    }
}

/// Reports whether the attributes of a tag hide its element: `hidden` or a hiding style.
fn hides(attributes: &str) -> bool {
    let mut rest = attributes;
    loop {
        rest = rest.trim_start_matches(|character: char| {
            character.is_ascii_whitespace() || character == '/'
        });
        if rest.is_empty() {
            return false;
        }
        let name_len = rest
            .find(|character: char| {
                character.is_ascii_whitespace() || matches!(character, '=' | '/')
            })
            .unwrap_or(rest.len());
        let name = rest.get(..name_len).unwrap_or_default();
        let after = rest.get(name_len..).unwrap_or_default().trim_start();
        let (value, next) = match after.strip_prefix('=') {
            Some(value) => attribute_value(value.trim_start()),
            None => ("", after),
        };
        if name.eq_ignore_ascii_case("hidden") {
            return true;
        }
        if name.eq_ignore_ascii_case("style") {
            let style: String = value
                .chars()
                .filter(|character| !character.is_whitespace())
                .collect::<String>()
                .to_ascii_lowercase();
            if HIDING_STYLES.iter().any(|hiding| style.contains(hiding)) {
                return true;
            }
        }
        rest = next;
    }
}

/// Splits an attribute value, quoted or not, from the attributes that follow it.
fn attribute_value(text: &str) -> (&str, &str) {
    match text.chars().next() {
        Some(quote @ ('"' | '\'')) => {
            let inner = text.get(1..).unwrap_or_default();
            match inner.split_once(quote) {
                Some((value, next)) => (value, next),
                None => (inner, ""),
            }
        }
        _ => {
            let end = text
                .find(|character: char| character.is_ascii_whitespace())
                .unwrap_or(text.len());
            (
                text.get(..end).unwrap_or_default(),
                text.get(end..).unwrap_or_default(),
            )
        }
    }
}

/// Adds one `signal` when the hidden `text` instructs a reviewer.
fn report(add: &mut impl FnMut(Signal, u32), signal: Signal, text: &str) {
    if words::instructs(text) {
        add(signal, 1);
    }
}

/// Reports the hidden Markdown signals of a rendered field to `add`.
///
/// `code` holds the field's `code_ranges`, which the pass skips.
pub(crate) fn scan(text: &str, code: &[(usize, usize)], add: &mut impl FnMut(Signal, u32)) {
    let mut code = code.iter().copied().peekable();
    let mut pass = Pass::new(text);
    let mut at = 0_usize;
    while let Some(&byte) = pass.bytes.get(at) {
        if let Some(&(start, end)) = code.peek()
            && at >= start
        {
            code.next();
            at = at.max(end);
            continue;
        }
        let line_start = at == 0 || pass.bytes.get(at.saturating_sub(1)) == Some(&b'\n');
        let consumed = if line_start {
            pass.reference_definition(at, add)
        } else {
            None
        };
        let next = consumed.or_else(|| match byte {
            b'<' => pass.tag(at, add),
            b'!' => pass.image_alt(at, add),
            b']' => pass.link_title(at, add),
            _ => None,
        });
        at = next.unwrap_or(at).max(at.saturating_add(1));
    }
}
