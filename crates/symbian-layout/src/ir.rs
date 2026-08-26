//! The Page IR: a resolved display list, in one flat buffer.
//!
//! # Why flat, and why that was not a choice
//!
//! Five requirements land on the same shape, from four different phases of the plan:
//!
//! - The worker thread returns its result through a buffer the **caller** owns, and nothing it
//!   allocates may outlive it — a `Vec` built on the worker and dropped on the UI thread is a
//!   cross-heap free, which is silent corruption. So layout cannot hand back a tree of boxes.
//! - The desktop preview renders pages, so the IR has to survive leaving the process.
//! - Save-for-offline stores a page, so it has to survive leaving the *run*.
//! - A frozen tab keeps the IR and discards the DOM, so it has to be one thing to keep.
//! - In-page search scans the text, so the text has to be contiguous and readable.
//!
//! One `Vec<u8>` answers all five, and it answers a sixth for free: the plan's memory risk asked for
//! "IR in an arena with a hard byte cap", and a flat buffer *is* the arena — its length is the cap.
//!
//! # The size is in the header, because the worker cannot ask for more
//!
//! `rust_work` returns an `i32` and `symbian::work::Job::on_event` treats anything but `SHIM_OK` as
//! an error, so a job has no channel to say "I needed more room". The caller therefore sizes the
//! buffer generously and the IR states its own length; a page that does not fit is refused with an
//! honest error rather than truncated into something paintable and wrong.
//!
//! # The vocabulary is exactly what the canvas can paint
//!
//! Every node maps 1:1 onto a `symbian_gfx::Canvas` call. There is deliberately nothing here for a
//! rotation, a gradient, a scaled blit or an arbitrary path, because the canvas has none of those —
//! an IR able to express what the painter cannot is a bug waiting for a page that uses it.

use alloc::vec::Vec;

use symbian_gfx::{Color, Point, Rect};

use crate::style::{FontRole, Span};

/// `PI` and a format version. A buffer whose magic does not match is not read, which is what lets
/// the format change without a migration: an old saved page becomes a miss, not a wrong render.
pub const MAGIC: [u8; 4] = *b"PI\x02\x00";

/// Bytes of fixed header before the node array.
const HEADER: usize = 4 + 4 + 4 + 4 + 4 + 4;

/// One drawing instruction, or one non-drawing fact about a rectangle.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Node {
    /// A filled rectangle: backgrounds, and the placeholder for an image that has no bytes yet.
    Fill { rect: Rect, color: Color },
    /// Text, positioned by **baseline** because that is what `Canvas::draw_text` takes and what
    /// makes two runs of different sizes sit on the same line.
    ///
    /// One font, one colour, no line breaks. A paragraph is many of these; a bold word inside a
    /// sentence is its own. That is what "resolved" means, and it is why painting needs no engine.
    Text { baseline: Point, text: Span, font: FontRole, color: Color },
    /// A horizontal or vertical rule, one pixel thick. `<hr>` and borders.
    Rule { rect: Rect, color: Color },
    /// A replaced element, at the size layout decided it should be.
    ///
    /// `handle` is opaque here: layout does not decode anything. It is an index into whatever the
    /// application's image store is, and the destination rectangle is the contract — the decoder is
    /// told what size to produce, because the canvas has no scaling blit.
    Image { rect: Rect, handle: u32, src: Span },
    /// Where a click goes. Paints nothing.
    ///
    /// One per line, not one per link: a link that wraps produces two of these, so that the gap at
    /// the end of the first line is not clickable. A single covering rectangle would make the empty
    /// right margin of a paragraph a hit target for whatever link happened to end there.
    Link { rect: Rect, href: Span },
    /// A named position, for `#fragment`. Paints nothing.
    Anchor { name: Span, y: i32 },
    /// A form control's box. Paints nothing here — the application draws it, as with `Link`.
    ///
    /// `form` groups controls for submission; `kind` is a [`crate::style::FieldKind`] tag. `value`
    /// is what the document said it holds, which is the *initial* value: what the reader has typed
    /// since lives in the application, because the IR is a snapshot of the page and not of the
    /// session.
    Field { rect: Rect, form: u16, kind: u8, name: Span, value: Span },
    /// Where a form submits. Paints nothing; it is a fact the application looks up.
    ///
    /// Its own node rather than a copy on every control, because an action is one string and a form
    /// can have a dozen fields — and because a form with no controls at all still has an action,
    /// which a per-control copy could not express.
    Form { id: u16, action: Span, method: u8 },
}

/// A page, laid out.
#[derive(Debug, Default, Clone)]
pub struct PageIr {
    nodes: Vec<Node>,
    text: Vec<u8>,
    width: i32,
    height: i32,
}

impl PageIr {
    pub fn new(width: i32) -> Self {
        PageIr { nodes: Vec::new(), text: Vec::new(), width, height: 0 }
    }

    /// Copy a text arena in wholesale. Called once, with the styled tree's arena, so that every
    /// [`Span`] produced during layout stays valid without being re-interned one string at a time.
    pub fn set_text(&mut self, text: &str) {
        self.text.clear();
        self.text.extend_from_slice(text.as_bytes());
    }

    pub fn push(&mut self, n: Node) {
        self.nodes.push(n);
    }

    /// Reserve a slot now and fill it in later, returning its index.
    ///
    /// A block's background has to be painted *before* its children and sized *after* them — its
    /// height is not known until they are laid out. Emitting it late and inserting it early would be
    /// an O(n) move per block; emitting a placeholder and patching it is the same thing every real
    /// engine does.
    pub fn reserve(&mut self) -> usize {
        self.nodes.push(Node::Fill { rect: Rect::EMPTY, color: Color::TRANSPARENT });
        self.nodes.len() - 1
    }

    /// Fill in a slot from [`reserve`]. Out-of-range is ignored rather than a panic: the index comes
    /// from this crate alone, and a vanishing browser is worse than a missing background.
    pub fn patch(&mut self, at: usize, n: Node) {
        if let Some(slot) = self.nodes.get_mut(at) {
            *slot = n;
        }
    }

    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// The width the page was laid out for. Reflow is needed only if this changes.
    pub fn width(&self) -> i32 {
        self.width
    }

    /// The whole document's height, not the viewport's. Scrolling reads this; it does not reflow.
    pub fn height(&self) -> i32 {
        self.height
    }

    pub fn set_height(&mut self, h: i32) {
        self.height = h;
    }

    /// The characters behind a span, or `""` for one this page does not contain.
    pub fn str(&self, s: Span) -> &str {
        let start = s.off as usize;
        let end = start.saturating_add(s.len as usize);
        if end > self.text.len() {
            return "";
        }
        core::str::from_utf8(&self.text[start..end]).unwrap_or("")
    }

    /// Every text run in document order, for in-page search.
    ///
    /// Document order, not paint order, and they are the same here because layout emits in order —
    /// a search that reported hits in paint order would jump around the page.
    pub fn text_runs(&self) -> impl Iterator<Item = (Span, i32)> + '_ {
        self.nodes.iter().filter_map(|n| match n {
            Node::Text { text, baseline, .. } => Some((*text, baseline.y)),
            _ => None,
        })
    }

    /// The link whose rectangle contains a document-space point, if any.
    ///
    /// Last match wins: nodes are emitted in document order, so a later one is painted over an
    /// earlier one and is what the user sees.
    pub fn link_at(&self, p: Point) -> Option<Span> {
        let mut found = None;
        for n in &self.nodes {
            if let Node::Link { rect, href } = n {
                if rect.contains(p) {
                    found = Some(*href);
                }
            }
        }
        found
    }
}

// ------------------------------------------------------------------------------ encoding --

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_i32(out: &mut Vec<u8>, v: i32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_rect(out: &mut Vec<u8>, r: Rect) {
    put_i32(out, r.x0);
    put_i32(out, r.y0);
    put_i32(out, r.x1);
    put_i32(out, r.y1);
}

fn put_span(out: &mut Vec<u8>, s: Span) {
    put_u32(out, s.off);
    put_u32(out, s.len);
}

fn font_tag(f: FontRole) -> u8 {
    match f {
        FontRole::Body => 0,
        FontRole::Strong => 1,
        FontRole::Small => 2,
        FontRole::Title => 3,
    }
}

fn font_of(tag: u8) -> Option<FontRole> {
    match tag {
        0 => Some(FontRole::Body),
        1 => Some(FontRole::Strong),
        2 => Some(FontRole::Small),
        3 => Some(FontRole::Title),
        _ => None,
    }
}

/// Serialise. The result is self-describing: [`decode`] needs nothing but these bytes.
pub fn encode(ir: &PageIr) -> Vec<u8> {
    // Sized once. A doubling `Vec` holds two buffers at its peak, and this one is the size of a
    // page — the same lesson the response cache learned the hard way.
    let mut out = Vec::with_capacity(HEADER + ir.nodes.len() * 24 + ir.text.len());
    out.extend_from_slice(&MAGIC);
    put_i32(&mut out, ir.width);
    put_i32(&mut out, ir.height);
    put_u32(&mut out, ir.nodes.len() as u32);
    put_u32(&mut out, ir.text.len() as u32);
    // Total length, so a reader knows the buffer is complete before trusting any of it. Patched
    // below, once it is known.
    let total_at = out.len();
    put_u32(&mut out, 0);

    for n in &ir.nodes {
        match *n {
            Node::Fill { rect, color } => {
                out.push(0);
                put_rect(&mut out, rect);
                put_u32(&mut out, color.0);
            }
            Node::Text { baseline, text, font, color } => {
                out.push(1);
                put_i32(&mut out, baseline.x);
                put_i32(&mut out, baseline.y);
                put_span(&mut out, text);
                out.push(font_tag(font));
                put_u32(&mut out, color.0);
            }
            Node::Rule { rect, color } => {
                out.push(2);
                put_rect(&mut out, rect);
                put_u32(&mut out, color.0);
            }
            Node::Image { rect, handle, src } => {
                out.push(3);
                put_rect(&mut out, rect);
                put_u32(&mut out, handle);
                put_span(&mut out, src);
            }
            Node::Link { rect, href } => {
                out.push(4);
                put_rect(&mut out, rect);
                put_span(&mut out, href);
            }
            Node::Anchor { name, y } => {
                out.push(5);
                put_span(&mut out, name);
                put_i32(&mut out, y);
            }
            Node::Field { rect, form, kind, name, value } => {
                out.push(6);
                put_rect(&mut out, rect);
                out.extend_from_slice(&form.to_le_bytes());
                out.push(kind);
                put_span(&mut out, name);
                put_span(&mut out, value);
            }
            Node::Form { id, action, method } => {
                out.push(7);
                out.extend_from_slice(&id.to_le_bytes());
                put_span(&mut out, action);
                out.push(method);
            }
        }
    }

    out.extend_from_slice(&ir.text);
    let total = out.len() as u32;
    out[total_at..total_at + 4].copy_from_slice(&total.to_le_bytes());
    out
}

struct Reader<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.p.checked_add(n)?;
        if end > self.b.len() {
            return None;
        }
        let s = &self.b[self.p..end];
        self.p = end;
        Some(s)
    }
    fn u32(&mut self) -> Option<u32> {
        let s = self.take(4)?;
        Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn i32(&mut self) -> Option<i32> {
        Some(self.u32()? as i32)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn rect(&mut self) -> Option<Rect> {
        Some(Rect { x0: self.i32()?, y0: self.i32()?, x1: self.i32()?, y1: self.i32()? })
    }
    fn span(&mut self) -> Option<Span> {
        Some(Span { off: self.u32()?, len: self.u32()? })
    }
}

/// The length of the page at the start of `bytes`, if one is there.
///
/// Exists because the worker writes a page into a buffer sized for the largest one, so the caller
/// has a buffer with a page at the front and slack behind it — and [`decode`] deliberately refuses a
/// buffer whose length disagrees with the header, which is what makes a truncated page unpaintable.
/// Reading the header field by hand at the call site is a magic offset duplicated per caller, and
/// getting it wrong reads the *text length* as the total: measured, in the browser, where it made
/// every page unreadable at once.
pub fn encoded_len(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < HEADER || bytes[..4] != MAGIC {
        return None;
    }
    Some(u32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]) as usize)
}

/// Parse a page from the front of a buffer that may be longer than it.
///
/// The pairing for [`encoded_len`], and what a caller holding a worker's output buffer wants.
pub fn decode_prefix(bytes: &[u8]) -> Option<PageIr> {
    let n = encoded_len(bytes)?;
    decode(bytes.get(..n)?)
}

/// Parse. `None` for anything that is not a complete, current-version buffer.
///
/// Every failure is the same failure to a caller — a page it cannot show — so there is no error type
/// to distinguish them. A short read, a foreign magic and an unknown node tag are all "not a page".
pub fn decode(bytes: &[u8]) -> Option<PageIr> {
    let mut r = Reader { b: bytes, p: 0 };
    if r.take(4)? != MAGIC {
        return None;
    }
    let width = r.i32()?;
    let height = r.i32()?;
    let count = r.u32()? as usize;
    let text_len = r.u32()? as usize;
    let total = r.u32()? as usize;
    // Refused before a single node is read: a truncated page must not paint its first half.
    if total != bytes.len() {
        return None;
    }

    let mut nodes = Vec::with_capacity(count);
    for _ in 0..count {
        let node = match r.u8()? {
            0 => Node::Fill { rect: r.rect()?, color: Color(r.u32()?) },
            1 => {
                let baseline = Point { x: r.i32()?, y: r.i32()? };
                let text = r.span()?;
                let font = font_of(r.u8()?)?;
                Node::Text { baseline, text, font, color: Color(r.u32()?) }
            }
            2 => Node::Rule { rect: r.rect()?, color: Color(r.u32()?) },
            3 => Node::Image { rect: r.rect()?, handle: r.u32()?, src: r.span()? },
            4 => Node::Link { rect: r.rect()?, href: r.span()? },
            5 => Node::Anchor { name: r.span()?, y: r.i32()? },
            6 => Node::Field {
                rect: r.rect()?,
                form: u16::from_le_bytes([r.u8()?, r.u8()?]),
                kind: r.u8()?,
                name: r.span()?,
                value: r.span()?,
            },
            7 => Node::Form {
                id: u16::from_le_bytes([r.u8()?, r.u8()?]),
                action: r.span()?,
                method: r.u8()?,
            },
            _ => return None,
        };
        nodes.push(node);
    }

    let text = r.take(text_len)?.to_vec();
    // Nothing may follow the text arena. Trailing bytes mean the writer and the reader disagree
    // about the format, and guessing which is right is how a wrong page gets painted.
    if r.p != bytes.len() {
        return None;
    }

    Some(PageIr { nodes, text, width, height })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn sample() -> PageIr {
        let mut ir = PageIr::new(320);
        ir.set_text("hello worldhttps://example.com/top");
        ir.push(Node::Fill {
            rect: Rect::from_xywh(0, 0, 320, 40),
            color: Color::rgb(0x11, 0x22, 0x33),
        });
        ir.push(Node::Text {
            baseline: Point { x: 4, y: 20 },
            text: Span { off: 0, len: 5 },
            font: FontRole::Title,
            color: Color::WHITE,
        });
        ir.push(Node::Text {
            baseline: Point { x: 4, y: 40 },
            text: Span { off: 6, len: 5 },
            font: FontRole::Body,
            color: Color::BLACK,
        });
        ir.push(Node::Rule { rect: Rect::from_xywh(0, 44, 320, 1), color: Color::BLACK });
        ir.push(Node::Image {
            rect: Rect::from_xywh(8, 48, 100, 60),
            handle: 7,
            src: Span { off: 11, len: 20 },
        });
        ir.push(Node::Link {
            rect: Rect::from_xywh(4, 30, 60, 12),
            href: Span { off: 11, len: 20 },
        });
        ir.push(Node::Anchor { name: Span { off: 31, len: 3 }, y: 48 });
        ir.set_height(120);
        ir
    }

    #[test]
    fn the_encoded_length_is_readable_from_the_front() {
        let bytes = encode(&sample());
        assert_eq!(encoded_len(&bytes), Some(bytes.len()));

        // With slack behind it, which is how it always arrives from the worker.
        let mut padded = bytes.clone();
        padded.extend_from_slice(&[0xAA; 500]);
        assert_eq!(encoded_len(&padded), Some(bytes.len()));
        assert_eq!(decode_prefix(&padded).map(|p| p.height()), Some(120));

        assert_eq!(encoded_len(&[]), None);
        assert_eq!(encoded_len(&[0; 8]), None, "no magic, no length");
    }

    #[test]
    fn a_page_round_trips_exactly() {
        let ir = sample();
        let back = decode(&encode(&ir)).expect("a page this crate wrote must read back");
        assert_eq!(back.nodes(), ir.nodes());
        assert_eq!(back.width(), 320);
        assert_eq!(back.height(), 120);
        assert_eq!(back.str(Span { off: 0, len: 5 }), "hello");
    }

    /// This is the assertion the frozen tab and the offline save both rest on: what comes back is
    /// not merely similar, it is the same list of nodes.
    #[test]
    fn re_encoding_what_was_decoded_gives_identical_bytes() {
        let first = encode(&sample());
        let second = encode(&decode(&first).unwrap());
        assert_eq!(first, second, "the format must be canonical, or a saved page drifts on reload");
    }

    /// A truncated buffer is refused before anything is read, not painted halfway.
    #[test]
    fn every_prefix_is_refused() {
        let bytes = encode(&sample());
        for cut in 0..bytes.len() {
            assert!(decode(&bytes[..cut]).is_none(), "a {cut}-byte prefix decoded as a page");
        }
        assert!(decode(&bytes).is_some());
    }

    #[test]
    fn a_foreign_magic_is_refused() {
        let mut bytes = encode(&sample());
        bytes[0] = b'X';
        assert!(decode(&bytes).is_none());
    }

    /// Trailing bytes mean writer and reader disagree about the format. Guessing is how a wrong page
    /// gets painted, so the answer is no page.
    #[test]
    fn trailing_bytes_are_refused() {
        let mut bytes = encode(&sample());
        bytes.push(0);
        assert!(decode(&bytes).is_none(), "the length in the header must be believed");
    }

    /// An unknown node tag is a newer writer. Refused whole rather than skipped, because a page
    /// missing the nodes a reader did not understand is a page that lies about what it shows.
    #[test]
    fn an_unknown_node_tag_is_refused() {
        let mut ir = PageIr::new(320);
        ir.push(Node::Fill { rect: Rect::from_xywh(0, 0, 1, 1), color: Color::BLACK });
        let mut bytes = encode(&ir);
        // The first node's tag sits immediately after the header.
        bytes[HEADER] = 99;
        assert!(decode(&bytes).is_none());
    }

    #[test]
    fn an_empty_page_is_still_a_page() {
        let ir = PageIr::new(320);
        let back = decode(&encode(&ir)).expect("an empty page is legal");
        assert!(back.nodes().is_empty());
        assert_eq!(back.height(), 0);
    }

    /// In-page search reads the runs, so they must come out in document order.
    #[test]
    fn text_runs_come_out_in_document_order() {
        let ir = sample();
        let ys: Vec<i32> = ir.text_runs().map(|(_, y)| y).collect();
        assert_eq!(ys, vec![20, 40]);
        let texts: Vec<&str> = ir.text_runs().map(|(s, _)| ir.str(s)).collect();
        assert_eq!(texts, vec!["hello", "world"]);
    }

    #[test]
    fn a_point_inside_a_link_finds_it() {
        let ir = sample();
        assert_eq!(
            ir.link_at(Point { x: 10, y: 35 }).map(|s| ir.str(s)),
            Some("https://example.com/")
        );
        assert_eq!(ir.link_at(Point { x: 200, y: 35 }), None, "outside the rect is not a link");
        assert_eq!(ir.link_at(Point { x: 10, y: 200 }), None);
    }

    /// A span the page does not contain reads as nothing. The producer is a C traversal one layer
    /// down; a wrong offset must be a gap, not a crash.
    #[test]
    fn a_bogus_span_reads_as_empty() {
        let ir = sample();
        assert_eq!(ir.str(Span { off: 9999, len: 4 }), "");
        assert_eq!(ir.str(Span { off: 0, len: 9999 }), "");
    }

    /// The font role survives the round trip. It is the one field that cannot be a pointer, because
    /// the device and the desktop preview load different atlases for the same role.
    #[test]
    fn every_font_role_survives() {
        for role in [FontRole::Body, FontRole::Strong, FontRole::Small, FontRole::Title] {
            let mut ir = PageIr::new(320);
            ir.push(Node::Text {
                baseline: Point { x: 0, y: 0 },
                text: Span::EMPTY,
                font: role,
                color: Color::BLACK,
            });
            let back = decode(&encode(&ir)).unwrap();
            match back.nodes()[0] {
                Node::Text { font, .. } => assert_eq!(font, role),
                ref other => panic!("expected Text, got {other:?}"),
            }
        }
    }
}
