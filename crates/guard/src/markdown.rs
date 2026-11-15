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

impl<F: FnMut(Signal, u32)> Walk<'_, F> {
    /// Takes shown text, or code when `code` is set, in document order.
    fn text(&mut self, text: &str, code: bool) {
        if let Some(alt) = self.alt.as_mut() {
            alt.push_str(text);
        }
        if let Some(frame) = self.frames.first_mut() {
            frame.all.push_str(text);
            if !code {
                frame.plain.push_str(text);
            }
        }
    }

    /// Adds one `signal` when the hidden `text` instructs a reviewer.
    fn report(&mut self, signal: Signal, text: &str) {
        if words::instructs(text) {
            (self.add)(signal, 1);
        }
    }

    /// Closes every open frame and reports the outermost one.
    ///
    /// A collapsed block shows its code as code once opened, and logs are its usual content,
    /// so the model-and-directive part of the cue rule leaves code out; override phrases and
    /// chat-template tokens count in code too.
    fn close_all(&mut self) {
        let frames = std::mem::take(&mut self.frames);
        self.open.clear();
        if let Some(frame) = frames.into_iter().next() {
            let hit = if frame.signal == Signal::CollapsedBlockInstruction {
                words::instructs(&frame.plain) || words::overrides_in(&frame.all)
            } else {
                words::instructs(&frame.all)
            };
            if hit {
                (self.add)(frame.signal, 1);
            }
        }
    }

    /// Opens a frame for the element `name`.
    fn push(&mut self, name: String, signal: Signal, paragraph_only: bool) {
        let count = self.open.entry(name.clone()).or_insert(0);
        *count = count.saturating_add(1);
        self.frames.push(Frame {
            name,
            signal,
            paragraph_only,
            all: String::new(),
            plain: String::new(),
        });
    }

    /// Closes the innermost open frame of the element `name`, and those opened inside it.
    fn pop(&mut self, name: &str) {
        if self.open.get(name).copied().unwrap_or_default() == 0 {
            return;
        }
        let Some(index) = self.frames.iter().rposition(|frame| frame.name == name) else {
            return;
        };
        if index == 0 {
            self.close_all();
            return;
        }
        for frame in self.frames.drain(index..) {
            if let Some(count) = self.open.get_mut(&frame.name) {
                *count = count.saturating_sub(1);
            }
        }
    }

    /// Ends a paragraph: an inline `HIDDEN_ELEMENTS` element still open there was prose.
    fn end_paragraph(&mut self) {
        if self
            .frames
            .first()
            .is_some_and(|frame| frame.paragraph_only)
        {
            self.frames.clear();
            self.open.clear();
        }
    }

    /// Reads one raw HTML piece; `inline` is set for inline HTML inside a paragraph.
    fn raw_html(&mut self, html: &str, inline: bool) {
        let mut rest = html;
        while let Some(start) = rest.find('<') {
            self.text(rest.get(..start).unwrap_or_default(), false);
            self.text(" ", false);
            let from = rest.get(start..).unwrap_or_default();
            let head = from
                .get(..from.len().min(OPENER_MAX))
                .unwrap_or(from)
                .to_ascii_lowercase();
            let markup = HIDDEN_MARKUP
                .iter()
                .find(|(open, _)| head.starts_with(open) && (*open != "<!" || declaration(from)));
            if let Some((open, close)) = markup {
                let body = from.get(open.len()..).unwrap_or_default();
                let (content, after) = match body.find(close) {
                    Some(end) => (
                        body.get(..end).unwrap_or_default(),
                        body.get(end.saturating_add(close.len())..)
                            .unwrap_or_default(),
                    ),
                    None => (body, ""),
                };
                self.report(Signal::HiddenCommentInstruction, content);
                rest = after;
                continue;
            }
            rest = self.tag(from, inline);
        }
        self.text(rest, false);
    }

    /// Reads the tag that starts `from` and returns the text after it.
    fn tag<'h>(&mut self, from: &'h str, inline: bool) -> &'h str {
        let closing = from.starts_with("</");
        let name_from = from.get(if closing { 2 } else { 1 }..).unwrap_or_default();
        let name_len = name_from
            .bytes()
            .take_while(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
            .count();
        if name_len == 0 {
            self.text("<", false);
            return from.get(1..).unwrap_or_default();
        }
        let name = name_from
            .get(..name_len)
            .unwrap_or_default()
            .to_ascii_lowercase();
        let attributes_from = name_from.get(name_len..).unwrap_or_default();
        let end = tag_end(attributes_from);
        let attributes = attributes_from.get(..end).unwrap_or_default();
        let after = attributes_from
            .get(end.saturating_add(1)..)
            .unwrap_or_default();
        if closing {
            self.pop(&name);
            return after;
        }
        self.report(Signal::LinkMetadataInstruction, attributes);
        let hidden_element = HIDDEN_ELEMENTS.contains(&name.as_str());
        let signal = if name == "details" {
            Some(Signal::CollapsedBlockInstruction)
        } else if hides(attributes)
            || hidden_element
            || self.open.get(&name).copied().unwrap_or_default() > 0
        {
            Some(Signal::HiddenElementInstruction)
        } else {
            None
        };
        if let Some(signal) = signal
            && !attributes.trim_end().ends_with('/')
        {
            let paragraph_only = inline && hidden_element && self.frames.is_empty();
            self.push(name, signal, paragraph_only);
        }
        after
    }
}

/// Reports whether `from` starts a declaration: `<!` and an ASCII letter.
fn declaration(from: &str) -> bool {
    from.as_bytes().get(2).is_some_and(u8::is_ascii_alphabetic)
}

/// Returns where the attributes of a tag end: the first `>` outside a quoted value.
fn tag_end(attributes: &str) -> usize {
    let mut quote: Option<u8> = None;
    for (at, byte) in attributes.bytes().enumerate() {
        match quote {
            Some(open) if byte == open => quote = None,
            Some(_) => {}
            None if matches!(byte, b'"' | b'\'') => quote = Some(byte),
            None if byte == b'>' => return at,
            None => {}
        }
    }
    attributes.len()
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
            let style: String = words::decode_references(value)
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

/// Reports whether a node is a block that GitHub renders from its source lines.
fn renders_lines(value: &NodeValue) -> bool {
    matches!(
        value,
        NodeValue::Paragraph
            | NodeValue::Heading(_)
            | NodeValue::CodeBlock(_)
            | NodeValue::HtmlBlock(_)
            | NodeValue::Table(_)
            | NodeValue::ThematicBreak
    )
}

/// Reads one node on entry; marks the lines it renders in `covered`.
fn enter<F: FnMut(Signal, u32)>(walk: &mut Walk<'_, F>, node: &AstNode<'_>, covered: &mut [bool]) {
    let data = node.data();
    if renders_lines(&data.value) {
        let (first, last) = (data.sourcepos.start.line, data.sourcepos.end.line);
        for slot in covered.iter_mut().take(last.saturating_add(1)).skip(first) {
            *slot = true;
        }
    }
    match &data.value {
        NodeValue::Text(text) => walk.text(text, false),
        NodeValue::Code(code) => walk.text(&code.literal, true),
        NodeValue::CodeBlock(block) => walk.text(&block.literal, true),
        NodeValue::SoftBreak | NodeValue::LineBreak => walk.text("\n", false),
        NodeValue::HtmlBlock(block) => {
            let literal = block.literal.clone();
            drop(data);
            walk.raw_html(&literal, false);
        }
        NodeValue::HtmlInline(html) => {
            let html = html.clone();
            drop(data);
            walk.raw_html(&html, true);
        }
        NodeValue::Link(link) | NodeValue::Image(link) => {
            let image = matches!(data.value, NodeValue::Image(_));
            let title = (!link.title.is_empty()).then(|| format!("{} {}", link.url, link.title));
            drop(data);
            if let Some(title) = title {
                walk.report(Signal::LinkMetadataInstruction, &title);
            }
            if image {
                walk.alt = Some(String::new());
            }
        }
        _ => {}
    }
}

/// Reads one node on exit.
fn leave<F: FnMut(Signal, u32)>(walk: &mut Walk<'_, F>, node: &AstNode<'_>) {
    match node.data().value {
        NodeValue::Image(_) => {
            if let Some(alt) = walk.alt.take() {
                walk.report(Signal::LinkMetadataInstruction, &alt);
            }
        }
        NodeValue::Paragraph => walk.end_paragraph(),
        _ => {}
    }
}
