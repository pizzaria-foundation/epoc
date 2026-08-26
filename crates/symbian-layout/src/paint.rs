//! Painting a Page IR onto a canvas.
//!
//! # Why this is the short file
//!
//! Because the IR is *resolved*. Every text run already knows its font, its colour and its baseline;
//! every rectangle is already in document coordinates. So painting is a walk with a translation and
//! a clip, and there is no engine behind it — which is precisely what lets a frozen tab scroll after
//! its DOM has been thrown away, and what lets the desktop preview render a page it only has bytes
//! for.
//!
//! # Scrolling reads, it does not reflow
//!
//! The caller passes the scroll offset and the viewport. Nothing here recomputes a position, so
//! scrolling costs one pass over the node list and the culling below skips most of it.
//!
//! # The rule the image viewer paid for
//!
//! Scrolling and drawing must be clamped against the **same** rectangle. The toolkit's image viewer
//! has a `content()` function for exactly this, and the bug that put it there was a photo whose
//! bottom rows were reachable by the D-pad and never visible, because panning was clamped to the
//! screen while drawing clipped to the content band. [`max_scroll`] is this file's version of that
//! function, and it exists so a caller cannot use two different numbers.

use symbian_gfx::{Canvas, Point, Rect};

use crate::inline::FontSet;
use crate::ir::{Node, PageIr};

/// The furthest the page can scroll, given the height on screen.
///
/// The one place this arithmetic lives. A caller that clamps scrolling with its own subtraction and
/// draws through [`paint`] will eventually disagree with it by a few pixels, and the symptom is a
/// last line that can be scrolled to and never seen.
pub fn max_scroll(ir: &PageIr, viewport_h: i32) -> i32 {
    (ir.height() - viewport_h).max(0)
}

/// Paint the part of `ir` visible at `scroll` into `area`.
///
/// `area` is in the canvas's local coordinates. `scroll` is how far down the document the top of
/// `area` sits; it is clamped, so a caller cannot scroll past the end.
pub fn paint<F: FontSet>(
    c: &mut Canvas<'_>,
    area: Rect,
    ir: &PageIr,
    scroll: i32,
    fonts: &F,
) {
    let scroll = scroll.clamp(0, max_scroll(ir, area.height()));

    c.with(area, |c| {
        // Everything below is in document space, offset by the scroll. `clip_local` is the visible
        // band, and it is what makes a 4000-pixel document cost one screen of drawing.
        let visible = c.clip_local();
        let top = scroll + visible.y0;
        let bottom = scroll + visible.y1;

        for n in ir.nodes() {
            match *n {
                Node::Fill { rect, color } => {
                    if rect.y1 <= top || rect.y0 >= bottom {
                        continue;
                    }
                    c.fill_rect(shift(rect, scroll), color);
                }
                Node::Rule { rect, color } => {
                    if rect.y1 <= top || rect.y0 >= bottom {
                        continue;
                    }
                    let r = shift(rect, scroll);
                    // One pixel, and which way it runs depends on which way it is long. A border is
                    // a rule; so is an `<hr>`.
                    if r.height() <= 1 {
                        c.hline(r.y0, r.x0, r.x1, color);
                    } else {
                        c.vline(r.x0, r.y0, r.y1, color);
                    }
                }
                Node::Text { baseline, text, font, color } => {
                    // A run's box is not its baseline: it rises above by the ascent and falls below
                    // by the descent. Culling on the baseline alone would clip the top line of the
                    // viewport in half.
                    let f = fonts.font(font);
                    if baseline.y + f.descent() <= top || baseline.y - f.ascent() >= bottom {
                        continue;
                    }
                    let s = ir.str(text);
                    if s.is_empty() {
                        continue;
                    }
                    c.draw_text(Point { x: baseline.x, y: baseline.y - scroll }, s, f, color);
                }
                // Images and the non-painting nodes. An image is drawn by the application, which
                // owns the decoded pixels; layout only said where and how big. Painting a
                // placeholder here would draw over a picture the caller is about to blit.
                // Drawn by the application, not here: a link's focus ring, an image's placeholder or
            // decoded bitmap, and a control's frame all belong to whoever owns the interaction.
            Node::Image { .. }
            | Node::Link { .. }
            | Node::Anchor { .. }
            | Node::Field { .. }
            | Node::Form { .. } => {}
            }
        }
    });
}

/// Every image visible at `scroll`, with its destination rectangle already in local coordinates.
///
/// Separate from [`paint`] because the pixels are not this crate's: the application decodes them,
/// and it needs to know which handles are worth decoding *now* — a page with forty images on a
/// device with 45 MB free cannot decode all of them to show the first screenful.
pub fn visible_images(
    ir: &PageIr,
    area: Rect,
    scroll: i32,
) -> impl Iterator<Item = (u32, Rect)> + '_ {
    let scroll = scroll.clamp(0, max_scroll(ir, area.height()));
    let top = scroll;
    let bottom = scroll + area.height();
    ir.nodes().iter().filter_map(move |n| match n {
        Node::Image { rect, handle, .. } if rect.y1 > top && rect.y0 < bottom => {
            Some((*handle, shift(*rect, scroll)))
        }
        _ => None,
    })
}

/// The link at a point on screen, translating the point back into the document.
///
/// Takes the same `scroll` and `area` as [`paint`] and clamps them the same way, so a hit cannot
/// disagree with what was drawn.
pub fn link_at_screen(
    ir: &PageIr,
    area: Rect,
    scroll: i32,
    screen: Point,
) -> Option<crate::style::Span> {
    let scroll = scroll.clamp(0, max_scroll(ir, area.height()));
    if !area.contains(screen) {
        return None;
    }
    ir.link_at(Point { x: screen.x - area.x0, y: screen.y - area.y0 + scroll })
}

fn shift(r: Rect, scroll: i32) -> Rect {
    Rect { x0: r.x0, y0: r.y0 - scroll, x1: r.x1, y1: r.y1 - scroll }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{body, layout};
    use crate::style::{NodeKind, Style, StyledTree};
    use alloc::vec::Vec;
    use symbian_gfx::{BitmapFont, Color, Font, Size};

    struct Fixed(BitmapFont<'static>);

    fn atlas() -> &'static [u8] {
        alloc::boxed::Box::leak(symbian_ui::testing::atlas().into_boxed_slice())
    }

    impl Fixed {
        fn new() -> Self {
            Fixed(BitmapFont::new(atlas()).unwrap())
        }
        fn lh(&self) -> i32 {
            self.0.line_height()
        }
    }

    impl FontSet for Fixed {
        fn font(&self, _r: crate::style::FontRole) -> &dyn Font {
            &self.0
        }
    }

    /// Ink, on a page whose text is **white**.
    ///
    /// Not a style choice: the buffer starts as zeros, and zero in RGB565 is black. Every "did
    /// anything draw" assertion below would pass vacuously with the default black text, and a
    /// painter that drew nothing at all would look correct.
    fn ink() -> Style {
        Style { color: Color::WHITE, ..body() }
    }

    /// A page of `lines` paragraphs.
    ///
    /// Every character is `'a'`, because the test atlas has exactly one glyph — any other letter
    /// measures the fallback advance and leaves no ink, which would make an assertion about pixels
    /// depend on which letters the fixture happened to use.
    ///
    /// The lines differ in **length**, which is the only way to tell them apart with one glyph. With
    /// identical lines, scrolling by a whole number of line heights produces a pixel-identical
    /// picture — correctly — and a test asserting that scrolling changes something would fail while
    /// the painter was right.
    fn page(lines: usize) -> StyledTree {
        let mut t = StyledTree::new();
        let root = t.push(NodeKind::Element, Style::default());
        for i in 0..lines {
            let mut word = alloc::string::String::new();
            // Strictly increasing, not a short cycle: a period of five made scrolling by five lines
            // produce a pixel-identical picture, and the test failed while the painter was right.
            for _ in 0..(1 + i).min(20) {
                word.push('a');
            }
            let s = t.intern_collapsed(&word);
            let p = t.push(NodeKind::Element, ink());
            let txt = t.push(NodeKind::Text(s), ink());
            t.append_child(p, txt);
            t.append_child(root, p);
        }
        t
    }

    fn render(ir: &PageIr, fonts: &Fixed, scroll: i32, h: i32) -> Vec<u16> {
        let size = Size::new(120, h);
        let mut buf = alloc::vec![0u16; (size.w * size.h) as usize];
        {
            let mut c = Canvas::from_slice(&mut buf, size);
            paint(&mut c, Rect::from_size(size), ir, scroll, fonts);
        }
        buf
    }

    #[test]
    fn something_is_drawn() {
        let f = Fixed::new();
        let ir = layout(&page(3), 120, &f);
        let px = render(&ir, &f, 0, 100);
        assert!(px.iter().any(|&p| p != 0), "a page with text must draw something");
    }

    #[test]
    fn an_empty_page_draws_nothing_rather_than_panicking() {
        let f = Fixed::new();
        let ir = layout(&StyledTree::new(), 120, &f);
        let px = render(&ir, &f, 0, 100);
        assert!(px.iter().all(|&p| p == 0));
    }

    /// Scrolling changes what is on screen. If it did not, the culling would be wrong in the way
    /// that is hardest to see: a page that looks right and never moves.
    #[test]
    fn scrolling_changes_the_picture() {
        let f = Fixed::new();
        let ir = layout(&page(20), 120, &f);
        let a = render(&ir, &f, 0, 40);
        let b = render(&ir, &f, f.lh() * 5, 40);
        assert_ne!(a, b, "scrolling must change what is painted");
    }

    /// The last line must be reachable *and* visible. This is the image viewer's bug, in a browser.
    #[test]
    fn the_bottom_of_the_document_can_be_seen() {
        let f = Fixed::new();
        let ir = layout(&page(20), 120, &f);
        let h = 40;
        let end = max_scroll(&ir, h);
        let px = render(&ir, &f, end, h);
        // The bottom row band must contain ink: at maximum scroll the document's last line is on
        // screen, not one line below it.
        let last_band = ((h - f.lh()) * 120) as usize;
        assert!(
            px[last_band..].iter().any(|&p| p != 0),
            "at max scroll the final line must be visible, not just reachable"
        );
    }

    /// Scrolling past the end is clamped, not permitted. Otherwise the page slides off the top.
    #[test]
    fn scrolling_past_the_end_is_the_same_as_the_end() {
        let f = Fixed::new();
        let ir = layout(&page(20), 120, &f);
        let h = 40;
        let at_end = render(&ir, &f, max_scroll(&ir, h), h);
        let past = render(&ir, &f, 99_999, h);
        assert_eq!(at_end, past);
    }

    /// A document shorter than the viewport cannot scroll at all.
    #[test]
    fn a_short_page_does_not_scroll() {
        let f = Fixed::new();
        let ir = layout(&page(1), 120, &f);
        assert_eq!(max_scroll(&ir, 200), 0);
        assert_eq!(render(&ir, &f, 0, 200), render(&ir, &f, 50, 200));
    }

    /// Painting the same IR twice gives the same pixels. The IR is immutable and painting must be
    /// too, or a frozen tab would drift every time it was redrawn.
    #[test]
    fn painting_is_deterministic() {
        let f = Fixed::new();
        let ir = layout(&page(10), 120, &f);
        assert_eq!(render(&ir, &f, 7, 60), render(&ir, &f, 7, 60));
    }

    /// **The claim the whole IR exists for.** A page painted from bytes that went to disk and came
    /// back is pixel-identical to the page painted from the live object.
    #[test]
    fn a_serialised_page_paints_identically() {
        let f = Fixed::new();
        let ir = layout(&page(12), 120, &f);
        let back = crate::ir::decode(&crate::ir::encode(&ir)).expect("must decode");
        for scroll in [0, 5, f.lh() * 3, max_scroll(&ir, 60)] {
            assert_eq!(
                render(&ir, &f, scroll, 60),
                render(&back, &f, scroll, 60),
                "a reloaded page must paint identically at scroll {scroll}"
            );
        }
    }

    /// A hit on screen maps back to the document, including after scrolling.
    #[test]
    fn a_link_is_found_at_the_place_it_was_drawn() {
        let f = Fixed::new();
        let mut t = StyledTree::new();
        let root = t.push(NodeKind::Element, Style::default());
        // A tall spacer, then a link, so the link is only reachable after scrolling.
        let filler = t.intern_collapsed("filler");
        for _ in 0..10 {
            let p = t.push(NodeKind::Element, body());
            let txt = t.push(NodeKind::Text(filler), body());
            t.append_child(p, txt);
            t.append_child(root, p);
        }
        let href = t.intern("https://e.com/");
        let label = t.intern_collapsed("go");
        let p = t.push(NodeKind::Element, body());
        let txt = t.push(NodeKind::Text(label), Style { href, ..body() });
        t.append_child(p, txt);
        t.append_child(root, p);

        let ir = layout(&t, 120, &f);
        let area = Rect::from_xywh(0, 0, 120, 40);

        // Not visible at the top of the document.
        assert_eq!(link_at_screen(&ir, area, 0, Point { x: 2, y: 2 }), None);

        let link_y = ir
            .nodes()
            .iter()
            .find_map(|n| match n {
                Node::Link { rect, .. } => Some(rect.y0),
                _ => None,
            })
            .expect("there is a link");

        // Scrolled to the end, which is as far as the link can be brought up: the document ends
        // shortly after it, so it lands part-way down the viewport rather than at the top. Asking
        // for `link_y` directly would be clamped — and computing the screen position from the
        // clamped scroll is exactly what a caller has to do, which is why the test does it too.
        let scroll = max_scroll(&ir, area.height());
        let on_screen_y = link_y - scroll + 2;
        let found = link_at_screen(&ir, area, scroll, Point { x: 2, y: on_screen_y });
        assert_eq!(found.map(|s| ir.str(s)), Some("https://e.com/"));
    }

    /// Only the images on screen are reported, so a page of forty does not decode forty to show one
    /// screenful.
    #[test]
    fn only_visible_images_are_reported() {
        let f = Fixed::new();
        let mut t = StyledTree::new();
        let root = t.push(NodeKind::Element, Style::default());
        for name in ["a", "b", "c", "d"] {
            let src = t.intern(name);
            let img = t.push(NodeKind::Image { src, w: 100, h: 100 }, body());
            t.append_child(root, img);
        }
        let ir = layout(&t, 120, &f);
        let area = Rect::from_xywh(0, 0, 120, 100);

        let first: Vec<u32> = visible_images(&ir, area, 0).map(|(h, _)| h).collect();
        assert_eq!(first, alloc::vec![0], "only the first image is on screen");

        let later: Vec<u32> = visible_images(&ir, area, 250).map(|(h, _)| h).collect();
        assert_eq!(later, alloc::vec![2, 3]);
    }

    /// A colour actually reaches the pixels. A painter that ignored `color` would pass every test
    /// about what it drew and none about how it looked.
    #[test]
    fn the_fill_colour_reaches_the_buffer() {
        let f = Fixed::new();
        let mut ir = PageIr::new(120);
        ir.push(Node::Fill { rect: Rect::from_xywh(0, 0, 120, 10), color: Color::WHITE });
        ir.set_height(10);
        let px = render(&ir, &f, 0, 20);
        assert_eq!(px[0], Color::WHITE.to_rgb565().0);
        assert_eq!(px[(15 * 120) as usize], 0, "and nothing below the rectangle");
    }
}
