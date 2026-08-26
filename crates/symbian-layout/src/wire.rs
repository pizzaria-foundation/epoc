//! The [`StyledTree`] on the wire: what a producer written in C hands to Rust.
//!
//! # Why a buffer and not an API
//!
//! The producer that matters is libhubbub + libdom + libcss, and none of it is callable from Rust:
//! every libdom accessor is a `static inline` dispatching through a per-node vtable, wrapped in a
//! macro of the same name, so `nm libdom.a` has no `dom_node_get_first_child` to link against. A
//! binding that asked C for one node at a time would mean a hand-written out-of-line wrapper per
//! accessor, plus a Rust model of libdom's handles and their reference counts.
//!
//! So the C side does the whole walk — parse, cascade, select a computed style per element — and
//! emits the finished tree. One crossing, no handles, and the DOM never leaves C.
//!
//! It is the third time this shape has been the answer here: the worker returns a page this way, the
//! response cache stores one this way. The reasons keep being the same ones — a buffer is what
//! crosses a thread, a process and a disk unchanged.
//!
//! # The format is checked, not trusted
//!
//! The writer is C, which means a mistake there arrives as bytes rather than as a type error.
//! [`decode`] validates every index and every span before building anything, and answers `None` for
//! the whole buffer rather than a partial tree: half a document is not a smaller document, it is a
//! wrong one.

use alloc::vec::Vec;

use symbian_gfx::{Color, Edges};

use crate::style::{Display, FontRole, Marker, NodeKind, Span, Style, StyledTree, NONE};

/// `ST` and a format version. A buffer whose magic does not match is refused, so the format can
/// change without a migration: an old producer becomes an error, not a wrong page.
pub const MAGIC: [u8; 4] = *b"ST\x01\x00";

/// Bytes before the node array: magic, node count, text length, total length.
const HEADER: usize = 4 + 4 + 4 + 4;

/// Bytes per node. Fixed width, so the array can be indexed and a truncated buffer is caught by
/// arithmetic rather than by walking off the end.
///
/// The C producer has the same sum, in the same order, with a `#error` if the two disagree. That
/// guard exists because getting it wrong does not fail loudly: every node after the first is read
/// from the wrong offset, so the tree arrives as garbage with the right shape.
const NODE: usize = 1 // kind
    + 4 + 4 // span off/len   (text, or image src)
    + 4 + 4 // image w/h
    + 1 // display
    + 1 // font
    + 4 // color
    + 1 + 4 // has background, background
    + 8 // margin: four i16
    + 8 // padding: four i16
    + 1 + 4 + 4 // marker kind, marker span
    + 4 + 4 // href span
    + 1 // rule below
    + 1 // field kind (0 = not a control)
    + 4 + 4 // field name span
    + 2 // form id
    + 1 // form method
    + 4 + 4; // first child, next sibling — MUST stay last, see below

// ------------------------------------------------------------------------------ writing --

fn put_u32(o: &mut Vec<u8>, v: u32) {
    o.extend_from_slice(&v.to_le_bytes());
}

fn put_i32(o: &mut Vec<u8>, v: i32) {
    o.extend_from_slice(&v.to_le_bytes());
}

fn put_i16(o: &mut Vec<u8>, v: i32) {
    // Clamped rather than truncated: a margin wider than a screen is a producer bug, and wrapping it
    // into a negative would move the box left instead of merely being too wide.
    o.extend_from_slice(&(v.clamp(i16::MIN as i32, i16::MAX as i32) as i16).to_le_bytes());
}

fn put_span(o: &mut Vec<u8>, s: Span) {
    put_u32(o, s.off);
    put_u32(o, s.len);
}

fn put_edges(o: &mut Vec<u8>, e: Edges) {
    put_i16(o, e.left);
    put_i16(o, e.top);
    put_i16(o, e.right);
    put_i16(o, e.bottom);
}

fn display_tag(d: Display) -> u8 {
    match d {
        Display::Block => 0,
        Display::Inline => 1,
        Display::None => 2,
    }
}

fn font_tag(f: FontRole) -> u8 {
    match f {
        FontRole::Body => 0,
        FontRole::Strong => 1,
        FontRole::Small => 2,
        FontRole::Title => 3,
    }
}

/// Serialise a tree. Exists so the format has a reference implementation and a round-trip test —
/// the writer that matters is in C, and a format with only one implementation has no way to be
/// wrong out loud.
pub fn encode(t: &StyledTree) -> Vec<u8> {
    let mut o = Vec::with_capacity(HEADER + t.len() * NODE + t.text().len());
    o.extend_from_slice(&MAGIC);
    put_u32(&mut o, t.len() as u32);
    put_u32(&mut o, t.text().len() as u32);
    let total_at = o.len();
    put_u32(&mut o, 0);

    for i in 0..t.len() as u32 {
        let n = t.node(i);
        // A control reuses the node's own span for its value — the field's text, the button's
        // label, the chosen option. It is the same slot `Image` uses for `src`: one span per node,
        // spent on whatever that node's kind means by it.
        let (kind, span, w, h) = match n.kind {
            NodeKind::Element => (0u8, Span::EMPTY, 0, 0),
            NodeKind::Text(s) => (1u8, s, 0, 0),
            NodeKind::Image { src, w, h } => (2u8, src, w, h),
            NodeKind::Control { value, .. } => (3u8, value, 0, 0),
        };
        o.push(kind);
        put_span(&mut o, span);
        put_i32(&mut o, w);
        put_i32(&mut o, h);

        let s = n.style;
        o.push(display_tag(s.display));
        o.push(font_tag(s.font));
        put_u32(&mut o, s.color.0);
        match s.background {
            Some(c) => {
                o.push(1);
                put_u32(&mut o, c.0);
            }
            None => {
                o.push(0);
                put_u32(&mut o, 0);
            }
        }
        put_edges(&mut o, s.margin);
        put_edges(&mut o, s.padding);
        match s.marker {
            Marker::None => {
                o.push(0);
                put_span(&mut o, Span::EMPTY);
            }
            Marker::Bullet => {
                o.push(1);
                put_span(&mut o, Span::EMPTY);
            }
            Marker::Text(m) => {
                o.push(2);
                put_span(&mut o, m);
            }
        }
        put_span(&mut o, s.href);
        o.push(u8::from(s.rule_below));
        match n.kind {
            NodeKind::Control { kind, name, .. } => {
                o.push(kind.tag());
                put_span(&mut o, name);
            }
            _ => {
                o.push(0);
                put_span(&mut o, Span::EMPTY);
            }
        }
        o.extend_from_slice(&s.form.to_le_bytes());
        o.push(s.method);
        put_u32(&mut o, n.first_child);
        put_u32(&mut o, n.next_sibling);
    }

    o.extend_from_slice(t.text().as_bytes());
    let total = o.len() as u32;
    o[total_at..total_at + 4].copy_from_slice(&total.to_le_bytes());
    o
}

// ------------------------------------------------------------------------------ reading --

struct R<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> R<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let e = self.p.checked_add(n)?;
        if e > self.b.len() {
            return None;
        }
        let s = &self.b[self.p..e];
        self.p = e;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u32(&mut self) -> Option<u32> {
        let s = self.take(4)?;
        Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn i32(&mut self) -> Option<i32> {
        Some(self.u32()? as i32)
    }
    fn i16(&mut self) -> Option<i32> {
        let s = self.take(2)?;
        Some(i16::from_le_bytes([s[0], s[1]]) as i32)
    }
    fn span(&mut self) -> Option<Span> {
        Some(Span { off: self.u32()?, len: self.u32()? })
    }
    fn edges(&mut self) -> Option<Edges> {
        Some(Edges { left: self.i16()?, top: self.i16()?, right: self.i16()?, bottom: self.i16()? })
    }
}

/// Parse a tree, validating every field.
///
/// `None` for a buffer that is not one, and for one whose contents do not hang together: an index
/// past the node array, a span past the text arena, an unknown tag. The producer is C, so those are
/// the shapes a bug there takes — and a tree with one bad index is a tree a walk can loop in.
pub fn decode(bytes: &[u8]) -> Option<StyledTree> {
    let mut r = R { b: bytes, p: 0 };
    if r.take(4)? != MAGIC {
        return None;
    }
    let count = r.u32()? as usize;
    let text_len = r.u32()? as usize;
    let total = r.u32()? as usize;
    if total != bytes.len() {
        return None;
    }
    // The arithmetic, before any allocation: a count from a corrupt header must not be a capacity.
    if HEADER.checked_add(count.checked_mul(NODE)?)?.checked_add(text_len)? != total {
        return None;
    }

    let text_at = HEADER + count * NODE;
    let text = core::str::from_utf8(bytes.get(text_at..text_at + text_len)?).ok()?;

    let mut t = StyledTree::new();
    let interned = t.intern(text);
    debug_assert_eq!(interned.off, 0);

    // Two passes: every node is created before any link is set, because a child index may point
    // forwards and `append_child` would otherwise be given an index that does not exist yet.
    let mut links: Vec<(u32, u32)> = Vec::with_capacity(count);
    for _ in 0..count {
        let kind_tag = r.u8()?;
        let span = r.span()?;
        let w = r.i32()?;
        let h = r.i32()?;
        // A control's kind byte lives further down the record, so the node kind is finished after
        // it is read. Held as an Option until then rather than defaulted, so a missing case cannot
        // silently become an Element.
        let kind_is_control = kind_tag == 3;
        let kind = match kind_tag {
            0 => Some(NodeKind::Element),
            1 => Some(NodeKind::Text(check_span(span, text_len)?)),
            2 => Some(NodeKind::Image { src: check_span(span, text_len)?, w, h }),
            3 => None,
            _ => return None,
        };

        let display = match r.u8()? {
            0 => Display::Block,
            1 => Display::Inline,
            2 => Display::None,
            _ => return None,
        };
        let font = match r.u8()? {
            0 => FontRole::Body,
            1 => FontRole::Strong,
            2 => FontRole::Small,
            3 => FontRole::Title,
            _ => return None,
        };
        let color = Color(r.u32()?);
        let has_bg = r.u8()?;
        let bg_raw = r.u32()?;
        let background = match has_bg {
            0 => None,
            1 => Some(Color(bg_raw)),
            _ => return None,
        };
        let margin = r.edges()?;
        let padding = r.edges()?;
        let marker_tag = r.u8()?;
        let marker_span = r.span()?;
        let marker = match marker_tag {
            0 => Marker::None,
            1 => Marker::Bullet,
            2 => Marker::Text(check_span(marker_span, text_len)?),
            _ => return None,
        };
        let href = check_span(r.span()?, text_len)?;
        let rule_below = match r.u8()? {
            0 => false,
            1 => true,
            _ => return None,
        };

        let field_tag = r.u8()?;
        let field_name = r.span()?;
        let form = u16::from_le_bytes([r.u8()?, r.u8()?]);
        let method = r.u8()?;
        // The two halves have to agree: a node whose kind says control must carry a control tag,
        // and a node that is not one must not. Disagreement is a build problem — the two sides of
        // this format drifted — and it is refused rather than guessed at.
        let kind = match (kind, kind_is_control, field_tag) {
            (Some(k), false, 0) => k,
            (None, true, t) => NodeKind::Control {
                kind: crate::style::FieldKind::from_tag(t)?,
                name: check_span(field_name, text_len)?,
                value: check_span(span, text_len)?,
            },
            _ => return None,
        };

        let first = r.u32()?;
        let next = r.u32()?;
        // An index into a node array that does not contain it is how a tree walk loops forever.
        if (first != NONE && first as usize >= count) || (next != NONE && next as usize >= count) {
            return None;
        }
        links.push((first, next));

        let style = Style {
            display,
            font,
            color,
            background,
            margin,
            padding,
            marker,
            href,
            rule_below,
            form,
            method,
        };
        t.push(kind, style);
    }

    t.set_links(&links);
    Some(t)
}

/// A span must lie inside the arena. A producer that miscounted would otherwise render a gap in the
/// best case and read another node's text in the worst.
fn check_span(s: Span, text_len: usize) -> Option<Span> {
    let end = (s.off as usize).checked_add(s.len as usize)?;
    if end > text_len {
        return None;
    }
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Style;

    fn sample() -> StyledTree {
        let mut t = StyledTree::new();
        let href = t.intern("https://e.com/");
        let mark = t.intern("1.");
        let words = t.intern_collapsed("hello world");
        let src = t.intern("cat.png");

        let root = t.push(NodeKind::Element, Style::default());
        let p = t.push(
            NodeKind::Element,
            Style {
                background: Some(Color::rgb(1, 2, 3)),
                margin: Edges::new(1, 2, 3, 4),
                padding: Edges::all(5),
                marker: Marker::Text(mark),
                rule_below: true,
                ..Default::default()
            },
        );
        let txt = t.push(
            NodeKind::Text(words),
            Style {
                display: Display::Inline,
                font: FontRole::Title,
                color: Color::rgb(9, 8, 7),
                href,
                ..Default::default()
            },
        );
        let img = t.push(NodeKind::Image { src, w: 640, h: 480 }, Style::default());
        t.append_child(p, txt);
        t.append_child(p, img);
        t.append_child(root, p);
        t
    }

    /// Every field survives, because the writer that matters is in C and a dropped field there
    /// arrives as a page that is subtly wrong rather than as an error.
    /// The record size is the one the C producer hard-codes a `#error` against. Asserted here so a
    /// field added above changes a number a human has to look at, in both places.
    #[test]
    fn a_node_record_is_eighty_two_bytes() {
        // The number is load-bearing on both sides of an FFI boundary that has no other check.
        // When it last disagreed — 61 in C against 70 here — nothing errored: every record after
        // the first was read from the wrong offset and the tree arrived as garbage with the right
        // shape. If this assertion fires, `WIRE_NODE` in shim/csrc/dom_bridge.c and the literal in
        // its `#if WIRE_NODE != 82` guard change with it.
        assert_eq!(NODE, 82, "if this changes, change shim/csrc/dom_bridge.c's WIRE_NODE too");
        assert_eq!(HEADER, 16);
    }

    #[test]
    fn a_tree_round_trips_field_for_field() {
        let t = sample();
        let back = decode(&encode(&t)).expect("a tree we wrote must read back");

        assert_eq!(back.len(), t.len());
        assert_eq!(back.text(), t.text());
        for i in 0..t.len() as u32 {
            let a = t.node(i);
            let b = back.node(i);
            assert_eq!(b.kind, a.kind, "node {i} kind");
            assert_eq!(b.style, a.style, "node {i} style");
            assert_eq!(b.first_child, a.first_child, "node {i} first child");
            assert_eq!(b.next_sibling, a.next_sibling, "node {i} sibling");
        }
    }

    #[test]
    fn the_format_is_canonical() {
        let first = encode(&sample());
        let second = encode(&decode(&first).unwrap());
        assert_eq!(first, second);
    }

    #[test]
    fn every_prefix_is_refused() {
        let bytes = encode(&sample());
        for cut in 0..bytes.len() {
            assert!(decode(&bytes[..cut]).is_none(), "a {cut}-byte prefix decoded");
        }
        assert!(decode(&bytes).is_some());
    }

    #[test]
    fn a_foreign_magic_is_refused() {
        let mut b = encode(&sample());
        b[0] = b'X';
        assert!(decode(&b).is_none());
    }

    /// A child index past the array is how a tree walk loops forever, so it is caught before a tree
    /// exists rather than during a walk.
    #[test]
    fn an_out_of_range_child_index_is_refused() {
        let t = sample();
        let mut b = encode(&t);
        // The first node's `first_child` sits at the end of its record.
        let at = HEADER + NODE - 8;
        b[at..at + 4].copy_from_slice(&999u32.to_le_bytes());
        assert!(decode(&b).is_none(), "an index into nothing must not become a tree");
    }

    /// A span past the arena would render another node's text, or nothing, depending on luck.
    #[test]
    fn a_span_past_the_text_arena_is_refused() {
        let t = sample();
        let mut b = encode(&t);
        // Node 2 is the text node; its span is the first field after the kind byte.
        let at = HEADER + 2 * NODE + 1;
        b[at..at + 4].copy_from_slice(&9999u32.to_le_bytes());
        assert!(decode(&b).is_none());
    }

    #[test]
    fn an_unknown_tag_is_refused() {
        for offset in [0usize /* kind */, 17 /* display */, 18 /* font */] {
            let mut b = encode(&sample());
            b[HEADER + offset] = 99;
            assert!(decode(&b).is_none(), "tag at offset {offset} must be validated");
        }
    }

    /// A header that disagrees with the buffer is refused before anything is allocated: a node count
    /// from a corrupt header must not become a capacity.
    #[test]
    fn a_lying_header_allocates_nothing() {
        let mut b = encode(&sample());
        b[4..8].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        assert!(decode(&b).is_none());
    }

    #[test]
    fn an_empty_tree_round_trips() {
        let t = StyledTree::new();
        let back = decode(&encode(&t)).expect("an empty tree is legal");
        assert_eq!(back.len(), 0);
        assert_eq!(back.root(), None);
    }

    /// The decoded tree is walkable, which is the property a layout depends on.
    #[test]
    fn the_decoded_tree_walks_like_the_original() {
        let t = sample();
        let back = decode(&encode(&t)).unwrap();
        let mut order = alloc::vec::Vec::new();
        let mut cur = back.node(back.root().unwrap()).first_child;
        while cur != NONE {
            order.push(cur);
            let mut child = back.node(cur).first_child;
            while child != NONE {
                order.push(child);
                child = back.node(child).next_sibling;
            }
            cur = back.node(cur).next_sibling;
        }
        assert_eq!(order, alloc::vec![1, 2, 3], "root -> p -> (text, image)");
    }
}
