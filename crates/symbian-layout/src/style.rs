//! The layout engine's input: a tree whose styles are already resolved.
//!
//! # Why this is our type and not libcss's
//!
//! Layout could have consumed `css_computed_style *` directly, and that would have tied this crate
//! to five C libraries, a 36-callback select handler, and a cross toolchain — none of which can run
//! on a desktop today, because `tools/build-netsurf` only knows how to target armv5. Layout is the
//! part of the browser that is *ours* and the part most worth testing, so it takes a type it can be
//! handed in ten lines by a test.
//!
//! What fills it on the device is a separate producer walking libdom and asking libcss for a
//! computed style per element. That producer is the next phase; this type is the contract between
//! the two, and it is deliberately small — every field here is one a `css_computed_*` accessor can
//! answer, and those are real exported symbols, unlike libdom's vtable-dispatched macros.
//!
//! # Flat, not a tree of boxes
//!
//! Nodes live in one `Vec` and refer to each other by index; text lives in one `String` and is
//! referred to by byte range. Two reasons, and neither is tidiness: the producer builds this from a
//! C traversal where a `Box` per node would be a per-node allocation on a handset with ~45 MB free,
//! and the whole document has to be discardable in one drop when a tab is frozen.

use alloc::string::String;
use alloc::vec::Vec;

use symbian_gfx::{Color, Edges};

/// A byte range into [`StyledTree`]'s text arena.
///
/// `u32` rather than `usize`: a document larger than 4 GB is not a case this device has, and half
/// the width matters when there is one of these per node.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Span {
    pub off: u32,
    pub len: u32,
}

impl Span {
    pub const EMPTY: Span = Span { off: 0, len: 0 };

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// Which font a run is drawn in, by **role** rather than by size or pointer.
///
/// The toolkit selects fonts by role (`symbian_ui::Fonts`) and the desktop preview deliberately
/// loads larger atlases than the handset, so a role is the only thing that means the same on both.
/// A style that carried a font pointer could not be serialised either, which the Page IR requires.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum FontRole {
    #[default]
    Body,
    /// Bold body text: `<strong>`, `<b>`, and table headers.
    Strong,
    /// Smaller than body: `<small>`, captions, footers.
    Small,
    /// Headings. There is one heading font, not six — `<h1>` through `<h6>` differ by spacing and
    /// weight here, because six atlases would cost more image than the distinction is worth.
    Title,
}

/// How a node participates in layout.
///
/// Deliberately three values where CSS has dozens. The fit-to-width policy collapses `float`,
/// `table`, `table-row`, `inline-block` and the rest into one of these, and doing that in the
/// *producer* rather than here would spread the policy across two crates.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Display {
    /// Starts on a new line and fills the available width.
    #[default]
    Block,
    /// Flows with its siblings inside a line box.
    Inline,
    /// Contributes nothing: `display: none`, `<script>`, `<style>`, `<head>`.
    None,
}

/// A list marker, when a block is an item in one.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Marker {
    #[default]
    None,
    /// `<ul>` — a square, drawn rather than typed.
    ///
    /// Drawn because the atlas is not guaranteed to have U+2022: a missing glyph becomes the
    /// fallback advance, which is an invisible marker that still indents.
    Bullet,
    /// `<ol>` — the marker text, already interned by the producer.
    ///
    /// A span rather than a number, and that is not a detail. Every [`crate::ir::Node::Text`] is a
    /// range into this tree's arena, and "3." is not in the document — so either the IR grows a
    /// second string table for generated content, or the producer interns the marker. The producer
    /// already knows the index, the `start=` attribute and the numbering style, so it is the right
    /// place: layout draws what it is given.
    Text(Span),
}

/// Everything layout needs to know about one node's appearance.
///
/// Everything it does **not** contain is as deliberate: no width, no height, no position, no float,
/// no z-index. Those are the properties the fit-to-width policy exists to ignore — a page that
/// declares `width: 980px` gets the screen's width, and carrying the 980 would only invite some
/// later code to honour it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Style {
    pub display: Display,
    pub font: FontRole,
    pub color: Color,
    /// Painted behind the block. `None` means "whatever is behind", which is not the same as white:
    /// filling every block with an opaque background would defeat the theme.
    pub background: Option<Color>,
    pub margin: Edges,
    pub padding: Edges,
    pub marker: Marker,
    /// Where a click goes. Set on the *inline* node so that a link spanning a line break produces
    /// two hit rectangles rather than one that covers the gap between them.
    pub href: Span,
    /// Draw a horizontal rule below this block. `<hr>`, and the bottom border of a table row.
    pub rule_below: bool,
    /// Which form this node belongs to, or [`NO_FORM`].
    ///
    /// Inherited, and that is the point: a control is usually several elements deep inside its
    /// `<form>`, and submitting means gathering every control that shares one. Carrying the id down
    /// costs two bytes a node and saves matching up ancestors later.
    pub form: u16,
    /// How that form submits: 0 GET, 1 POST. Inherited with the id it belongs to.
    pub method: u8,
}

/// A node that is not inside any form.
pub const NO_FORM: u16 = u16::MAX;

impl Default for Style {
    fn default() -> Self {
        Style {
            display: Display::Block,
            font: FontRole::Body,
            color: Color::BLACK,
            background: None,
            margin: Edges::ZERO,
            padding: Edges::ZERO,
            marker: Marker::None,
            href: Span::EMPTY,
            rule_below: false,
            form: NO_FORM,
            method: 0,
        }
    }
}

/// What a node is.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum NodeKind {
    /// A box. Children are laid out inside it.
    Element,
    /// A run of characters. Always a leaf.
    Text(Span),
    /// A replaced element. `w` and `h` are the *intrinsic* size in pixels, which layout scales down
    /// to fit and which may be zero when the document did not say — an `<img>` with no dimensions
    /// is the common case, and the honest answer then is a placeholder box.
    Image { src: Span, w: i32, h: i32 },
    /// A form control: a text box, a button, a checkbox, a dropdown.
    ///
    /// Always a leaf, deliberately. A `<button>Send</button>` has a text child and a `<textarea>`
    /// has its content, but a control is one box with one label — descending into it would lay that
    /// text out as prose *beside* the box as well as inside it, which is what a `<select>` does
    /// today and why its options appear twice. So the emitter reads the text it needs, puts it in
    /// `value`, and stops.
    Control {
        kind: FieldKind,
        /// The submitted name. Empty means the control is not submitted, which is legal and common
        /// for a decorative button.
        name: Span,
        /// What is in it: the text of a field, the label of a button, the chosen option.
        value: Span,
    },
}

/// The kinds of control, in the order their tags appear on the wire.
///
/// `Text` is the fallback for every `type` this does not know, because that is what HTML says an
/// unknown `type` means — and it is the behaviour that keeps a page usable when a new input type
/// appears that this browser has never heard of.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    Password,
    Button,
    Submit,
    Checkbox,
    Radio,
    Select,
    TextArea,
    /// Submitted but never shown, and never focusable.
    Hidden,
}

impl FieldKind {
    /// The wire tag. Zero is reserved for "not a control", so these start at one.
    pub fn tag(self) -> u8 {
        match self {
            FieldKind::Text => 1,
            FieldKind::Password => 2,
            FieldKind::Button => 3,
            FieldKind::Submit => 4,
            FieldKind::Checkbox => 5,
            FieldKind::Radio => 6,
            FieldKind::Select => 7,
            FieldKind::TextArea => 8,
            FieldKind::Hidden => 9,
        }
    }

    pub fn from_tag(t: u8) -> Option<Self> {
        Some(match t {
            1 => FieldKind::Text,
            2 => FieldKind::Password,
            3 => FieldKind::Button,
            4 => FieldKind::Submit,
            5 => FieldKind::Checkbox,
            6 => FieldKind::Radio,
            7 => FieldKind::Select,
            8 => FieldKind::TextArea,
            9 => FieldKind::Hidden,
            _ => return None,
        })
    }

    /// Whether the reader can put a cursor on it.
    ///
    /// Hidden cannot, and neither can anything the page means as decoration. Kept here rather than
    /// in the browser so that "can this be focused" has one answer.
    pub fn focusable(self) -> bool {
        !matches!(self, FieldKind::Hidden)
    }

    /// Whether activating it submits the form.
    pub fn submits(self) -> bool {
        matches!(self, FieldKind::Submit)
    }

    /// Whether the reader types into it.
    pub fn typed_into(self) -> bool {
        matches!(self, FieldKind::Text | FieldKind::Password | FieldKind::TextArea)
    }
}

/// One node.
#[derive(Copy, Clone, Debug)]
pub struct Node {
    pub kind: NodeKind,
    pub style: Style,
    /// First child, or `NONE`.
    pub first_child: u32,
    /// Next sibling, or `NONE`.
    pub next_sibling: u32,
}

/// The absent index. `u32::MAX` rather than `Option<u32>` so a `Node` stays `Copy` and small; the
/// tree is walked by this crate alone and the sentinel never escapes it.
pub const NONE: u32 = u32::MAX;

/// A document with its styles resolved, ready to lay out.
#[derive(Debug, Default)]
pub struct StyledTree {
    nodes: Vec<Node>,
    text: String,
}

impl StyledTree {
    pub fn new() -> Self {
        StyledTree { nodes: Vec::new(), text: String::new() }
    }

    /// Intern a string into the text arena.
    ///
    /// Not deduplicated. Deduplication would pay off for `href`s repeated down a navigation bar,
    /// and it costs a hash map per document to find out — worth measuring on a real page before
    /// paying for, and pointless to guess at now.
    pub fn intern(&mut self, s: &str) -> Span {
        let off = self.text.len() as u32;
        self.text.push_str(s);
        Span { off, len: s.len() as u32 }
    }

    /// Intern text with HTML's whitespace collapsing applied.
    ///
    /// This is where collapsing belongs, and it belongs here rather than in the line breaker for a
    /// concrete reason: the breaker emits spans that are byte ranges into this arena, and a run of
    /// three spaces that has to render as one cannot be expressed as a range over the original. So
    /// the arena holds text that is already normalised, and every span the breaker cuts is
    /// contiguous.
    ///
    /// Runs of ASCII whitespace — including the newlines and indentation that make a document
    /// readable in a text editor — become one space. Without this, source formatting becomes gaps on
    /// screen, which on a 320-pixel column is most of the column.
    ///
    /// `white-space: pre` is the exception, and the producer handles it by calling [`intern`]
    /// instead. That is the whole reason both exist.
    pub fn intern_collapsed(&mut self, s: &str) -> Span {
        let off = self.text.len() as u32;
        let mut last_was_space = false;
        for ch in s.chars() {
            if ch.is_ascii_whitespace() {
                if !last_was_space {
                    self.text.push(' ');
                    last_was_space = true;
                }
            } else {
                self.text.push(ch);
                last_was_space = false;
            }
        }
        Span { off, len: self.text.len() as u32 - off }
    }

    /// Add a node with no children and no sibling, returning its index.
    pub fn push(&mut self, kind: NodeKind, style: Style) -> u32 {
        let i = self.nodes.len() as u32;
        self.nodes.push(Node { kind, style, first_child: NONE, next_sibling: NONE });
        i
    }

    /// Make `child` the last child of `parent`.
    ///
    /// Walks the sibling chain rather than keeping a `last_child`, because building is one pass over
    /// a document and the chains are short; a second index per node would cost more across a whole
    /// page than the walk does.
    pub fn append_child(&mut self, parent: u32, child: u32) {
        let head = self.nodes[parent as usize].first_child;
        if head == NONE {
            self.nodes[parent as usize].first_child = child;
            return;
        }
        let mut cur = head;
        loop {
            let next = self.nodes[cur as usize].next_sibling;
            if next == NONE {
                break;
            }
            cur = next;
        }
        self.nodes[cur as usize].next_sibling = child;
    }

    /// Set every node's `first_child` and `next_sibling` at once.
    ///
    /// For a decoder, which has the links before it can validate that the nodes they point at exist:
    /// `append_child` walks a sibling chain and would be handed indices for nodes not yet created.
    /// Extra entries are ignored and missing ones leave a node childless — a decoder has already
    /// checked the count, and a panic here would be a vanished process rather than a rejected tree.
    pub fn set_links(&mut self, links: &[(u32, u32)]) {
        for (i, (first, next)) in links.iter().enumerate() {
            if let Some(n) = self.nodes.get_mut(i) {
                n.first_child = *first;
                n.next_sibling = *next;
            }
        }
    }

    pub fn node(&self, i: u32) -> &Node {
        &self.nodes[i as usize]
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The characters behind a span. Empty for a span this tree did not produce, rather than a
    /// panic: a malformed span is a bug in the producer, and a browser that vanishes on one is
    /// worse than a browser that renders a gap.
    pub fn str(&self, s: Span) -> &str {
        let start = s.off as usize;
        let end = start.saturating_add(s.len as usize);
        if end > self.text.len() || !self.text.is_char_boundary(start) || !self.text.is_char_boundary(end)
        {
            return "";
        }
        &self.text[start..end]
    }

    /// The whole text arena, for the Page IR to copy in one go.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The root, or `None` for an empty document.
    pub fn root(&self) -> Option<u32> {
        if self.nodes.is_empty() {
            None
        } else {
            Some(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn children_come_back_in_the_order_they_were_added() {
        let mut t = StyledTree::new();
        let root = t.push(NodeKind::Element, Style::default());
        let a = t.push(NodeKind::Element, Style::default());
        let b = t.push(NodeKind::Element, Style::default());
        let c = t.push(NodeKind::Element, Style::default());
        t.append_child(root, a);
        t.append_child(root, b);
        t.append_child(root, c);

        let mut seen = Vec::new();
        let mut cur = t.node(root).first_child;
        while cur != NONE {
            seen.push(cur);
            cur = t.node(cur).next_sibling;
        }
        assert_eq!(seen, [a, b, c], "document order is the one thing a DOM must not scramble");
    }

    #[test]
    fn interned_text_comes_back() {
        let mut t = StyledTree::new();
        let a = t.intern("hello");
        let b = t.intern("world");
        assert_eq!(t.str(a), "hello");
        assert_eq!(t.str(b), "world");
        assert_eq!(t.text(), "helloworld");
    }

    /// A span the tree did not produce renders as nothing rather than panicking.
    ///
    /// The producer is a C traversal, and a wrong offset from it is a rendering gap here. A panic on
    /// this platform is a process that vanishes with no message.
    #[test]
    fn a_bogus_span_is_empty_and_not_a_panic() {
        let mut t = StyledTree::new();
        t.intern("abc");
        assert_eq!(t.str(Span { off: 99, len: 3 }), "");
        assert_eq!(t.str(Span { off: 1, len: 99 }), "");
        assert_eq!(t.str(Span { off: 0, len: 0 }), "");
    }

    /// A span that lands mid-character is refused, not sliced.
    #[test]
    fn a_span_inside_a_multibyte_character_is_empty() {
        let mut t = StyledTree::new();
        t.intern("aé");
        // 'é' is two bytes at offset 1, so offset 2 is inside it.
        assert_eq!(t.str(Span { off: 2, len: 1 }), "");
    }

    /// Source formatting must not become screen gaps. On a 320-pixel column it would be most of
    /// the column.
    #[test]
    fn whitespace_collapses_when_interned_that_way() {
        let mut t = StyledTree::new();
        let a = t.intern_collapsed("  hello\n\n   world  ");
        assert_eq!(t.str(a), " hello world ");

        let b = t.intern_collapsed("a\tb\r\nc");
        assert_eq!(t.str(b), "a b c");
    }

    /// `white-space: pre` needs the raw form, which is why both entry points exist.
    #[test]
    fn plain_intern_keeps_whitespace_exactly() {
        let mut t = StyledTree::new();
        let a = t.intern("  a\n  b");
        assert_eq!(t.str(a), "  a\n  b");
    }

    #[test]
    fn an_empty_tree_has_no_root() {
        assert_eq!(StyledTree::new().root(), None);
        let mut t = StyledTree::new();
        t.push(NodeKind::Element, Style::default());
        assert_eq!(t.root(), Some(0));
    }

    /// The default style is the one a producer gets when it says nothing, so it has to be the
    /// harmless one: a block that adds no space and paints no background.
    #[test]
    fn the_default_style_adds_nothing() {
        let s = Style::default();
        assert_eq!(s.display, Display::Block);
        assert_eq!(s.margin, Edges::ZERO);
        assert_eq!(s.padding, Edges::ZERO);
        assert!(s.background.is_none(), "an opaque default would defeat the theme");
        assert!(s.href.is_empty());
        assert!(!s.rule_below);
    }
}
