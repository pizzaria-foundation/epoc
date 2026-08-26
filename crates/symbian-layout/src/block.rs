//! Block flow, and the fit-to-width policy.
//!
//! # This file is the product
//!
//! Everything else in the crate is machinery. What makes the browser worth having on this device is
//! the decision made here: **the declared width is ignored.** A page that says `width: 980px` gets
//! the screen's width, its columns become one column, and its layout becomes a single vertical
//! flow. A faithful CSS 2.1 layout would put a 980-pixel canvas behind a 320-pixel window and ask
//! the user to pan around it, which is what the platform's own browser does and is the reason nobody
//! uses it.
//!
//! So there is no float here, no positioning, no multi-column, and no table grid — not as
//! unfinished work, but because each of them is a way of placing content *across* a width this
//! screen does not have.
//!
//! # What "one column" costs, honestly
//!
//! A data table whose meaning is in its columns comes out as a list of cells. That is a real loss
//! and it is why the plan has a reading mode. The alternative — a table that keeps its columns —
//! renders at 40 pixels a column and is unreadable in a different way.
//!
//! # Where the font-size floor went
//!
//! The plan asked for one, so that a desktop site's `font-size: 9px` stays legible. It turns out
//! there is nothing to implement: styles carry a [`FontRole`], not a pixel size, and the theme
//! chooses the atlas per role. The floor is a property of role-based fonts, and the producer that
//! maps computed sizes onto roles is where the mapping lives.

use alloc::vec::Vec;

use symbian_gfx::{Color, Point, Rect};

use crate::inline::{break_lines, FontSet, Item};
use crate::ir::{Node, PageIr};
use crate::style::{Display, FieldKind, FontRole, Marker, NodeKind, Span, StyledTree, NONE};

/// Space between a list marker and the text it belongs to.
const MARKER_GAP: i32 = 4;

/// A placeholder box for an image with no intrinsic size, so the page does not silently lose it.
const PLACEHOLDER: i32 = 32;

/// Air inside a control's frame, on every side.
const FIELD_PAD: i32 = 3;

/// The narrowest a button gets, so a one-character label is still something you can aim at.
const FIELD_MIN_LABEL: i32 = 24;

/// The narrowest a text field gets, so a field on a cramped column is still visibly a field.
const FIELD_MIN_TEXT: i32 = 60;

/// Lay a document out for `width` pixels.
///
/// The result covers the whole document, not the viewport: its height is the document's height, and
/// scrolling reads it without reflowing. Reflow is needed only when `width` changes.
pub fn layout<F: FontSet>(tree: &StyledTree, width: i32, fonts: &F) -> PageIr {
    let mut ir = PageIr::new(width);
    ir.set_text(tree.text());

    let Some(root) = tree.root() else { return ir };
    let mut cx = Ctx { tree, fonts, ir, y: 0, image_handle: 0, forms: Vec::new() };
    cx.block(root, 0, width);
    let h = cx.y;
    let mut ir = cx.ir;
    ir.set_height(h);
    ir
}

struct Ctx<'a, F: FontSet> {
    tree: &'a StyledTree,
    fonts: &'a F,
    ir: PageIr,
    /// The pen, in document space. Only ever moves down.
    y: i32,
    /// Handed out in document order, so the application can decode in the order the page reads.
    image_handle: u32,
    /// Which forms have had their action written out already. A page has a handful, so a `Vec` is
    /// the right shape and a set would be more machinery than the problem.
    forms: Vec<u16>,
}

impl<'a, F: FontSet> Ctx<'a, F> {
    /// Lay out one block and everything inside it.
    ///
    /// `x` is its left edge and `avail` the width it may use — already reduced by every ancestor's
    /// padding, which is the only way the declared width of anything enters the arithmetic.
    fn block(&mut self, node: u32, x: i32, avail: i32) {
        let n = self.tree.node(node);
        let style = n.style;
        if style.display == Display::None {
            return;
        }

        // The first node carrying a form id is the `<form>` element itself, because the id is
        // inherited from it down. So this is where its action reaches the IR — once per form, not
        // once per control.
        if style.form != crate::style::NO_FORM && !self.forms.contains(&style.form) {
            self.forms.push(style.form);
            self.ir.push(Node::Form {
                id: style.form,
                action: style.href,
                method: style.method,
            });
        }

        let m = style.margin;
        let p = style.padding;
        self.y += m.top;
        let top = self.y;

        // Reserved before the children so the background paints behind them; sized after, because
        // its height is theirs.
        let bg_slot = style.background.map(|_| self.ir.reserve());

        let content_x = x + m.left + p.left;
        // Never negative: deeply nested padding on a narrow screen would otherwise ask for a
        // negative width and every measurement after it would be nonsense.
        let content_w = (avail - m.left - m.right - p.left - p.right).max(1);
        self.y += p.top;

        let marker_indent = self.marker(&style, content_x, content_w);

        self.children(node, content_x + marker_indent, content_w - marker_indent);

        self.y += p.bottom;

        if let (Some(slot), Some(c)) = (bg_slot, style.background) {
            self.ir.patch(
                slot,
                Node::Fill { rect: Rect::new(x + m.left, top, x + avail - m.right, self.y), color: c },
            );
        }

        if style.rule_below {
            self.ir.push(Node::Rule {
                rect: Rect::new(content_x, self.y, content_x + content_w, self.y + 1),
                color: style.color,
            });
            self.y += 1;
        }

        self.y += m.bottom;
    }

    /// Draw the list marker, and report how far the content has to move right for it.
    ///
    /// The indent is capped at half the column: a deeply nested list on a 320-pixel screen would
    /// otherwise indent until there is no room left for the text it is indenting.
    fn marker(&mut self, style: &crate::style::Style, x: i32, avail: i32) -> i32 {
        let font = self.fonts.font(style.font);
        match style.marker {
            Marker::None => 0,
            Marker::Bullet => {
                let d = (font.ascent() / 3).max(2);
                let cy = self.y + font.ascent() - d;
                self.ir.push(Node::Fill { rect: Rect::from_xywh(x, cy, d, d), color: style.color });
                (d + MARKER_GAP).min(avail / 2)
            }
            Marker::Text(span) => {
                let s = self.tree.str(span);
                if s.is_empty() {
                    return 0;
                }
                let w: i32 = s.chars().map(|c| font.advance(c)).sum();
                self.ir.push(Node::Text {
                    baseline: Point { x, y: self.y + font.ascent() },
                    text: span,
                    font: style.font,
                    color: style.color,
                });
                (w + MARKER_GAP).min(avail / 2)
            }
        }
    }

    /// Lay out a node's children, gathering inline runs into lines and recursing into blocks.
    fn children(&mut self, node: u32, x: i32, avail: i32) {
        let mut pending: Vec<Item> = Vec::new();
        let mut child = self.tree.node(node).first_child;

        while child != NONE {
            let c = self.tree.node(child);
            match c.kind {
                NodeKind::Image { src, w, h } => {
                    // An image ends the line it interrupts. Inline images exist, and treating them
                    // as blocks is the fit-to-width answer: on a 320-pixel column an image beside
                    // text leaves neither enough room.
                    self.flush(&mut pending, x, avail);
                    self.image(src, w, h, x, avail);
                }
                NodeKind::Text(span) => {
                    if c.style.display != Display::None {
                        pending.push(Item {
                            text: span,
                            font: c.style.font,
                            color: c.style.color,
                            href: c.style.href,
                        control: None,
                        });
                    }
                }
                NodeKind::Control { kind, name, value } => {
                    // A control the reader cannot reach is still a control the form submits.
                    //
                    // This used to be the same `if` as the box below, so anything not focusable —
                    // every `<input type=hidden>`, and any control a stylesheet had hidden — was
                    // dropped from the IR entirely. The app never saw it and therefore could never
                    // send it: `<input type=hidden name=_csrf value=tok>` submitted as `_csrf=`
                    // with nothing in it, which a server answers with the login page again. HTML is
                    // explicit that display has nothing to do with submission, and nothing on the
                    // phone would have said which field went missing.
                    //
                    // A zero-width rect at the current line, because there is nothing to draw and
                    // the app draws controls from the *focusable* ones only.
                    let boxed = c.style.display != Display::None && kind.focusable();
                    if !boxed {
                        self.ir.push(crate::ir::Node::Field {
                            rect: Rect::from_xywh(x, self.y, 0, 0),
                            form: c.style.form,
                            kind: kind.tag(),
                            name,
                            value,
                        });
                    }
                    // Queued as an inline box rather than ending the line.
                    //
                    // The first version flushed here, on the argument that a box beside text leaves
                    // neither enough room on a 320-pixel column. That is right for an image and
                    // wrong for a control: a search field and its button are one thing, and putting
                    // them on separate lines is not a search box. Reported as "eles não estão
                    // ficando próximos", and it is exactly what the code said to do.
                    if boxed {
                        let (w, h) = self.control_size(kind, value, &c.style, avail);
                        pending.push(Item {
                            text: value,
                            font: c.style.font,
                            color: c.style.color,
                            href: Span::EMPTY,
                            control: Some(crate::inline::Control {
                                kind: kind.tag(),
                                name,
                                form: c.style.form,
                                w,
                                h,
                            }),
                        });
                    }
                }
                NodeKind::Element => match c.style.display {
                    Display::None => {}
                    Display::Inline => self.collect(child, &mut pending),
                    Display::Block => {
                        self.flush(&mut pending, x, avail);
                        self.block(child, x, avail);
                    }
                },
            }
            child = c.next_sibling;
        }

        self.flush(&mut pending, x, avail);
    }

    /// How big a control's box is.
    ///
    /// Height is a line of text plus the frame, so a field sits on the page like a line of prose
    /// rather than like a picture.
    ///
    /// Width is where the judgement is. A text field used to take the whole column, which left no
    /// room for the button that submits it — the two ended up on separate lines and the form stopped
    /// reading as a form. So a field asks for about twenty characters and no more than most of the
    /// column, which leaves a button beside it and still fits a long value by scrolling inside the
    /// box rather than by growing.
    fn control_size(
        &self,
        kind: FieldKind,
        value: Span,
        style: &crate::style::Style,
        avail: i32,
    ) -> (i32, i32) {
        let font = self.fonts.font(style.font);
        let h = font.line_height() + FIELD_PAD * 2;
        let w = match kind {
            FieldKind::Button | FieldKind::Submit => {
                let label = font.measure(self.tree.str(value)).max(FIELD_MIN_LABEL);
                (label + FIELD_PAD * 4).min(avail)
            }
            FieldKind::Checkbox | FieldKind::Radio => h,
            _ => {
                // Twenty characters of the digit zero: a stand-in for average width that does not
                // depend on what happens to be in the field right now.
                let twenty = font.measure("00000000000000000000");
                twenty.clamp(FIELD_MIN_TEXT, avail * 3 / 4).min(avail)
            }
        };
        (w, h)
    }

    /// Gather an inline subtree's text into `out`, in document order.
    fn collect(&self, node: u32, out: &mut Vec<Item>) {
        let n = self.tree.node(node);
        if n.style.display == Display::None {
            return;
        }
        if let NodeKind::Text(span) = n.kind {
            out.push(Item {
                text: span,
                font: n.style.font,
                color: n.style.color,
                href: n.style.href,
                        control: None,
            });
        }
        let mut child = n.first_child;
        while child != NONE {
            self.collect(child, out);
            child = self.tree.node(child).next_sibling;
        }
    }

    /// Break the gathered inline content into lines and emit them.
    fn flush(&mut self, pending: &mut Vec<Item>, x: i32, avail: i32) {
        if pending.is_empty() {
            return;
        }
        let lines = break_lines(pending, self.tree.text(), avail, self.fonts);
        pending.clear();

        for line in lines {
            let baseline_y = self.y + line.baseline;
            for run in &line.runs {
                // A control is a box on the line, not characters: it gets a `Field` and no text
                // node, because the application draws its frame and whatever is inside it.
                if let Some(ctl) = run.control {
                    self.ir.push(Node::Field {
                        rect: Rect::new(
                            x + run.x,
                            self.y,
                            x + run.x + run.width,
                            self.y + ctl.h.min(line.height),
                        ),
                        form: ctl.form,
                        kind: ctl.kind,
                        name: ctl.name,
                        value: run.text,
                    });
                    continue;
                }
                self.ir.push(Node::Text {
                    baseline: Point { x: x + run.x, y: baseline_y },
                    text: run.text,
                    font: run.font,
                    color: run.color,
                });
                // One hit rectangle per run, so a link that wraps is clickable on both lines and the
                // empty margin at the end of the first is not.
                if !run.href.is_empty() {
                    self.ir.push(Node::Link {
                        rect: Rect::new(x + run.x, self.y, x + run.x + run.width, self.y + line.height),
                        href: run.href,
                    });
                }
            }
            self.y += line.height;
        }
    }

    /// Place an image, scaled down to fit the column.
    fn image(&mut self, src: Span, w: i32, h: i32, x: i32, avail: i32) {
        let handle = self.image_handle;
        self.image_handle += 1;

        // No intrinsic size: the document did not say, which is the common case for `<img>` with no
        // attributes. A placeholder box keeps the page's shape honest — an image that occupies
        // nothing would make the text above and below it touch.
        if w <= 0 || h <= 0 {
            let rect = Rect::from_xywh(x, self.y, PLACEHOLDER.min(avail), PLACEHOLDER);
            self.ir.push(Node::Image { rect, handle, src });
            self.y += PLACEHOLDER;
            return;
        }

        // Scaled down only, never up: a 16-pixel icon blown up to the column width is worse than a
        // 16-pixel icon. The destination rectangle is the contract with the decoder, because the
        // rasterizer has no scaling blit — whoever decodes must produce exactly this size.
        let (dw, dh) = if w > avail {
            (avail, (h as i64 * avail as i64 / w as i64) as i32)
        } else {
            (w, h)
        };
        let rect = Rect::from_xywh(x, self.y, dw, dh.max(1));
        self.ir.push(Node::Image { rect, handle, src });
        self.y += dh.max(1);
    }
}

/// The default style for a paragraph of body text, as a starting point for a producer or a test.
pub fn body() -> crate::style::Style {
    crate::style::Style { font: FontRole::Body, color: Color::BLACK, ..Default::default() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::{Marker, Style};
    use symbian_gfx::{BitmapFont, Edges, Font};

    struct Fixed(BitmapFont<'static>);

    fn atlas() -> &'static [u8] {
        alloc::boxed::Box::leak(symbian_ui::testing::atlas().into_boxed_slice())
    }

    impl Fixed {
        fn new() -> Self {
            Fixed(BitmapFont::new(atlas()).unwrap())
        }
        fn adv(&self) -> i32 {
            self.0.advance('a')
        }
        fn lh(&self) -> i32 {
            self.0.line_height()
        }
    }

    impl FontSet for Fixed {
        fn font(&self, _r: FontRole) -> &dyn Font {
            &self.0
        }
    }

    /// A document of `n` paragraphs of the same text.
    fn paragraphs(texts: &[&str]) -> StyledTree {
        let mut t = StyledTree::new();
        let root = t.push(NodeKind::Element, Style::default());
        for s in texts {
            let span = t.intern_collapsed(s);
            let p = t.push(NodeKind::Element, body());
            let txt = t.push(NodeKind::Text(span), body());
            t.append_child(p, txt);
            t.append_child(root, p);
        }
        t
    }

    fn texts(ir: &PageIr) -> Vec<&str> {
        ir.text_runs().map(|(s, _)| ir.str(s)).collect()
    }

    #[test]
    fn an_empty_document_lays_out_to_nothing() {
        let f = Fixed::new();
        let ir = layout(&StyledTree::new(), 320, &f);
        assert!(ir.nodes().is_empty());
        assert_eq!(ir.height(), 0);
    }

    #[test]
    fn one_paragraph_becomes_one_text_run() {
        let f = Fixed::new();
        let t = paragraphs(&["hello world"]);
        let ir = layout(&t, 320, &f);
        assert_eq!(texts(&ir), vec!["hello world"]);
        assert_eq!(ir.height(), f.lh(), "one line of body text is one line tall");
    }

    /// Paragraphs stack downwards, in document order, and the height is their sum.
    #[test]
    fn paragraphs_stack_in_order() {
        let f = Fixed::new();
        let t = paragraphs(&["one", "two", "three"]);
        let ir = layout(&t, 320, &f);
        assert_eq!(texts(&ir), vec!["one", "two", "three"]);

        let ys: Vec<i32> = ir.text_runs().map(|(_, y)| y).collect();
        assert!(ys.windows(2).all(|w| w[0] < w[1]), "later paragraphs must be lower: {ys:?}");
        assert_eq!(ir.height(), f.lh() * 3);
    }

    /// **The policy.** A narrower column produces more lines and a taller document from the same
    /// text — the content reflows rather than being clipped or panned.
    #[test]
    fn a_narrower_column_reflows_instead_of_clipping() {
        let f = Fixed::new();
        let t = paragraphs(&["aaa bbb ccc ddd eee fff ggg hhh"]);

        let wide = layout(&t, f.adv() * 40, &f);
        let narrow = layout(&t, f.adv() * 10, &f);

        assert!(
            narrow.height() > wide.height(),
            "narrow {} should be taller than wide {}",
            narrow.height(),
            wide.height()
        );
        // And every run stays inside the column it was laid out for.
        for n in narrow.nodes() {
            if let Node::Text { baseline, text, .. } = n {
                let w: i32 = narrow.str(*text).chars().map(|c| f.0.advance(c)).sum();
                assert!(
                    baseline.x + w <= f.adv() * 10 + f.adv(),
                    "a run runs past the column: x={} w={w}",
                    baseline.x
                );
            }
        }
    }

    /// Padding moves content in and makes the block taller; it never produces a negative width.
    #[test]
    fn padding_insets_the_content() {
        let f = Fixed::new();
        let mut t = StyledTree::new();
        let root = t.push(NodeKind::Element, Style::default());
        let span = t.intern_collapsed("x");
        let p = t.push(
            NodeKind::Element,
            Style { padding: Edges::all(10), ..body() },
        );
        let txt = t.push(NodeKind::Text(span), body());
        t.append_child(p, txt);
        t.append_child(root, p);

        let ir = layout(&t, 320, &f);
        let (_, y) = ir.text_runs().next().expect("the text must be there");
        assert!(y >= 10, "top padding must push the baseline down, got {y}");
        assert_eq!(ir.height(), 10 + f.lh() + 10);
        match ir.nodes().iter().find(|n| matches!(n, Node::Text { .. })).unwrap() {
            Node::Text { baseline, .. } => assert_eq!(baseline.x, 10),
            _ => unreachable!(),
        }
    }

    /// Padding deeper than the screen is wide must not ask for a negative column.
    #[test]
    fn absurd_padding_still_leaves_a_column() {
        let f = Fixed::new();
        let mut t = StyledTree::new();
        let root = t.push(NodeKind::Element, Style { padding: Edges::all(500), ..body() });
        let span = t.intern_collapsed("x");
        let txt = t.push(NodeKind::Text(span), body());
        t.append_child(root, txt);
        // Must not panic and must still emit the text.
        let ir = layout(&t, 320, &f);
        assert_eq!(texts(&ir), vec!["x"]);
    }

    /// A background is painted behind its children, and sized to them.
    #[test]
    fn a_background_is_behind_its_content_and_as_tall_as_it() {
        let f = Fixed::new();
        let mut t = StyledTree::new();
        let root = t.push(NodeKind::Element, Style::default());
        let span = t.intern_collapsed("hello");
        let p = t.push(
            NodeKind::Element,
            Style { background: Some(Color::rgb(1, 2, 3)), ..body() },
        );
        let txt = t.push(NodeKind::Text(span), body());
        t.append_child(p, txt);
        t.append_child(root, p);

        let ir = layout(&t, 320, &f);
        // Painted first, so it is behind.
        match ir.nodes()[0] {
            Node::Fill { rect, color } => {
                assert_eq!(color, Color::rgb(1, 2, 3));
                assert_eq!(rect.height(), f.lh(), "the background must be as tall as its content");
                assert_eq!(rect.width(), 320);
            }
            ref other => panic!("expected the background first, got {other:?}"),
        }
        assert!(matches!(ir.nodes()[1], Node::Text { .. }));
    }

    /// `display: none` contributes nothing — not a gap, not a background, not a height.
    #[test]
    fn display_none_contributes_nothing() {
        let f = Fixed::new();
        let mut t = StyledTree::new();
        let root = t.push(NodeKind::Element, Style::default());
        let span = t.intern_collapsed("invisible");
        let p = t.push(
            NodeKind::Element,
            Style {
                display: Display::None,
                background: Some(Color::WHITE),
                padding: Edges::all(20),
                ..body()
            },
        );
        let txt = t.push(NodeKind::Text(span), body());
        t.append_child(p, txt);
        t.append_child(root, p);

        let ir = layout(&t, 320, &f);
        assert!(ir.nodes().is_empty(), "got {:?}", ir.nodes());
        assert_eq!(ir.height(), 0);
    }

    /// An inline element's text joins the line around it rather than starting a block.
    #[test]
    fn inline_children_share_a_line() {
        let f = Fixed::new();
        let mut t = StyledTree::new();
        let root = t.push(NodeKind::Element, Style::default());
        let p = t.push(NodeKind::Element, body());

        let a = t.intern_collapsed("plain ");
        let b = t.intern_collapsed("bold ");
        let c = t.intern_collapsed("plain");
        let t1 = t.push(NodeKind::Text(a), body());
        let strong = t.push(
            NodeKind::Element,
            Style { display: Display::Inline, font: FontRole::Strong, ..body() },
        );
        let t2 = t.push(NodeKind::Text(b), Style { font: FontRole::Strong, ..body() });
        let t3 = t.push(NodeKind::Text(c), body());
        t.append_child(strong, t2);
        t.append_child(p, t1);
        t.append_child(p, strong);
        t.append_child(p, t3);
        t.append_child(root, p);

        let ir = layout(&t, 1000, &f);
        let ys: Vec<i32> = ir.text_runs().map(|(_, y)| y).collect();
        assert!(ys.windows(2).all(|w| w[0] == w[1]), "all on one baseline: {ys:?}");
        assert_eq!(ir.height(), f.lh(), "one line, not three");
    }

    /// A link produces a hit rectangle over its own run, and finding it back is what the D-pad
    /// needs.
    #[test]
    fn a_link_gets_a_hit_rectangle_you_can_find() {
        let f = Fixed::new();
        let mut t = StyledTree::new();
        let root = t.push(NodeKind::Element, Style::default());
        let href = t.intern("https://e.com/");
        let span = t.intern_collapsed("click");
        let p = t.push(NodeKind::Element, body());
        let txt = t.push(NodeKind::Text(span), Style { href, ..body() });
        t.append_child(p, txt);
        t.append_child(root, p);

        let ir = layout(&t, 320, &f);
        let link = ir
            .nodes()
            .iter()
            .find_map(|n| match n {
                Node::Link { rect, href } => Some((*rect, *href)),
                _ => None,
            })
            .expect("a link must produce a hit rectangle");
        assert_eq!(ir.str(link.1), "https://e.com/");
        // A point inside it finds it; a point past the text does not.
        assert_eq!(ir.link_at(Point { x: 1, y: 1 }).map(|s| ir.str(s)), Some("https://e.com/"));
        assert_eq!(ir.link_at(Point { x: 300, y: 1 }), None, "the empty margin is not a link");
    }

    /// An image wider than the column is scaled down, keeping its aspect ratio.
    #[test]
    fn a_wide_image_is_scaled_to_the_column() {
        let f = Fixed::new();
        let mut t = StyledTree::new();
        let root = t.push(NodeKind::Element, Style::default());
        let src = t.intern("cat.png");
        let img = t.push(NodeKind::Image { src, w: 640, h: 480 }, body());
        t.append_child(root, img);

        let ir = layout(&t, 320, &f);
        match ir.nodes()[0] {
            Node::Image { rect, .. } => {
                assert_eq!(rect.width(), 320);
                assert_eq!(rect.height(), 240, "480 * 320 / 640");
            }
            ref other => panic!("expected an image, got {other:?}"),
        }
        assert_eq!(ir.height(), 240);
    }

    /// A small image is left alone. Blowing a 16-pixel icon up to the column width is worse than a
    /// 16-pixel icon.
    #[test]
    fn a_small_image_is_not_enlarged() {
        let f = Fixed::new();
        let mut t = StyledTree::new();
        let root = t.push(NodeKind::Element, Style::default());
        let src = t.intern("icon.png");
        let img = t.push(NodeKind::Image { src, w: 16, h: 16 }, body());
        t.append_child(root, img);

        let ir = layout(&t, 320, &f);
        match ir.nodes()[0] {
            Node::Image { rect, .. } => assert_eq!((rect.width(), rect.height()), (16, 16)),
            ref other => panic!("expected an image, got {other:?}"),
        }
    }

    /// An image with no declared size still occupies space, so the text above and below it does not
    /// touch.
    #[test]
    fn an_image_of_unknown_size_gets_a_placeholder() {
        let f = Fixed::new();
        let mut t = StyledTree::new();
        let root = t.push(NodeKind::Element, Style::default());
        let src = t.intern("mystery.png");
        let img = t.push(NodeKind::Image { src, w: 0, h: 0 }, body());
        t.append_child(root, img);

        let ir = layout(&t, 320, &f);
        assert_eq!(ir.height(), PLACEHOLDER);
    }

    /// A control nobody can reach is still a control the form submits.
    #[test]
    fn a_hidden_control_reaches_the_ir_with_no_box() {
        let f = Fixed::new();
        let mut t = StyledTree::new();
        let root = t.push(NodeKind::Element, body());
        let name = t.intern("_csrf");
        let value = t.intern("tok");
        let hidden = t.push(
            NodeKind::Control { kind: FieldKind::Hidden, name, value },
            Style { display: Display::None, form: 0, ..body() },
        );
        t.append_child(root, hidden);

        let ir = layout(&t, 200, &f);
        let found = ir.nodes().iter().find_map(|n| match n {
            Node::Field { rect, name, value, kind, .. } => Some((*rect, *kind, *name, *value)),
            _ => None,
        });
        let (rect, kind, name, value) = found.expect("a hidden control must still be in the IR");
        assert_eq!(kind, FieldKind::Hidden.tag());
        assert_eq!(ir.str(name), "_csrf");
        assert_eq!(ir.str(value), "tok");
        assert_eq!((rect.width(), rect.height()), (0, 0), "nothing to draw, so no box");
    }

    /// Image handles are handed out in document order, so the application can decode top-down.
    #[test]
    fn image_handles_are_in_document_order() {
        let f = Fixed::new();
        let mut t = StyledTree::new();
        let root = t.push(NodeKind::Element, Style::default());
        for name in ["a", "b", "c"] {
            let src = t.intern(name);
            let img = t.push(NodeKind::Image { src, w: 10, h: 10 }, body());
            t.append_child(root, img);
        }
        let ir = layout(&t, 320, &f);
        let handles: Vec<u32> = ir
            .nodes()
            .iter()
            .filter_map(|n| match n {
                Node::Image { handle, .. } => Some(*handle),
                _ => None,
            })
            .collect();
        assert_eq!(handles, vec![0, 1, 2]);
    }

    /// A bullet indents its item and paints a mark.
    #[test]
    fn a_bullet_indents_and_paints() {
        let f = Fixed::new();
        let mut t = StyledTree::new();
        let root = t.push(NodeKind::Element, Style::default());
        let span = t.intern_collapsed("item");
        let li = t.push(NodeKind::Element, Style { marker: Marker::Bullet, ..body() });
        let txt = t.push(NodeKind::Text(span), body());
        t.append_child(li, txt);
        t.append_child(root, li);

        let ir = layout(&t, 320, &f);
        assert!(
            ir.nodes().iter().any(|n| matches!(n, Node::Fill { .. })),
            "the bullet must be painted"
        );
        match ir.nodes().iter().find(|n| matches!(n, Node::Text { .. })).unwrap() {
            Node::Text { baseline, .. } => assert!(baseline.x > 0, "the item must be indented"),
            _ => unreachable!(),
        }
    }

    /// A numbered marker is text the producer interned, and it is drawn.
    #[test]
    fn a_text_marker_is_drawn_and_indents() {
        let f = Fixed::new();
        let mut t = StyledTree::new();
        let root = t.push(NodeKind::Element, Style::default());
        let mark = t.intern("3.");
        let span = t.intern_collapsed("third");
        let li = t.push(NodeKind::Element, Style { marker: Marker::Text(mark), ..body() });
        let txt = t.push(NodeKind::Text(span), body());
        t.append_child(li, txt);
        t.append_child(root, li);

        let ir = layout(&t, 320, &f);
        let drawn = texts(&ir);
        assert_eq!(drawn, vec!["3.", "third"], "the marker is a text run like any other");
    }

    /// `<hr>` and row borders come out as a rule that occupies a pixel.
    #[test]
    fn a_rule_below_takes_one_pixel() {
        let f = Fixed::new();
        let mut t = StyledTree::new();
        let _root = t.push(NodeKind::Element, Style { rule_below: true, ..body() });
        let ir = layout(&t, 320, &f);
        assert!(matches!(ir.nodes()[0], Node::Rule { .. }));
        assert_eq!(ir.height(), 1);
    }

    /// Whatever layout produces must survive the round trip, because that is what a frozen tab and
    /// a saved page depend on. Asserted here, on a real laid-out page, and not only on a fixture.
    #[test]
    fn a_laid_out_page_round_trips_through_the_ir() {
        let f = Fixed::new();
        let t = paragraphs(&["the quick brown fox", "jumps over the lazy dog"]);
        let ir = layout(&t, 120, &f);
        let bytes = crate::ir::encode(&ir);
        let back = crate::ir::decode(&bytes).expect("a page we produced must decode");
        assert_eq!(back.nodes(), ir.nodes());
        assert_eq!(back.height(), ir.height());
        assert_eq!(back.width(), ir.width());
        assert_eq!(crate::ir::encode(&back), bytes, "and re-encode identically");
    }
}

#[cfg(test)]
mod control_layout_tests {
    use super::*;
    use crate::style::FieldKind;
    use symbian_gfx::BitmapFont;

    struct Fonts(BitmapFont<'static>);
    impl FontSet for Fonts {
        fn font(&self, _role: FontRole) -> &dyn symbian_gfx::Font {
            &self.0
        }
    }
    fn fonts() -> Fonts {
        let atlas = alloc::boxed::Box::leak(symbian_ui::testing::atlas().into_boxed_slice());
        Fonts(BitmapFont::new(atlas).unwrap())
    }

    /// A search form: one text field, one submit button, nothing else.
    fn search_form() -> PageIr {
        let mut t = StyledTree::new();
        let q = t.intern("q");
        let go = t.intern("Go");
        let root = t.push(NodeKind::Element, crate::style::Style::default());
        let field = t.push(
            NodeKind::Control { kind: FieldKind::Text, name: q, value: Span::EMPTY },
            crate::style::Style { form: 0, ..Default::default() },
        );
        let button = t.push(
            NodeKind::Control { kind: FieldKind::Submit, name: Span::EMPTY, value: go },
            crate::style::Style { form: 0, ..Default::default() },
        );
        t.append_child(root, field);
        t.append_child(root, button);
        layout(&t, 320, &fonts())
    }

    fn fields(ir: &PageIr) -> Vec<Rect> {
        ir.nodes()
            .iter()
            .filter_map(|n| match n {
                Node::Field { rect, .. } => Some(*rect),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_search_form_puts_the_button_beside_the_field_not_under_it() {
        // The reported problem, as a measurement: "eles não estão ficando próximos". A field on one
        // line and its button on the next is not a search box, it is two controls that happen to be
        // near each other — and on a 320px screen the button can end up off the bottom entirely.
        let ir = search_form();
        let f = fields(&ir);
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].y0, f[1].y0, "same line: {f:?}");
        assert!(f[1].x0 >= f[0].x1, "button starts after the field ends: {f:?}");
    }

    #[test]
    fn the_pair_still_fits_the_column() {
        let ir = search_form();
        let f = fields(&ir);
        assert!(f.iter().all(|r| r.x1 <= 320), "nothing hangs off the edge: {f:?}");
    }
}
