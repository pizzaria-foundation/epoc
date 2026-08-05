//! The era's drawing primitives, in terms of [`Surface`].
//!
//! Everything here is a handful of `fill_rect`/`hline` calls. They live in one
//! place so that "how a raised band looks" is decided once and every component
//! inherits it — which is what makes a theme swap actually change the look rather
//! than just the hues.
//!
//! Nothing allocates and nothing leaves; these are safe to call from the render
//! path on the device.

use symbian_gfx::{Canvas, Color, Rect};

use crate::tokens::{darken, lighten, luma, Surface};

/// Fill `r` with a surface: gradient body, then the two edge lines.
///
/// The gradient is computed per row with `Color::lerp`, one lerp and one `hline`
/// per row. On a 240px-tall screen the tallest band anyone fills is the full
/// content area, so the worst case is 240 lerps — under a microsecond of the
/// E72's 600 MHz budget, against the ~75,000 pixel writes the fill itself costs.
pub fn band(c: &mut Canvas, r: Rect, s: &Surface) {
    if r.is_empty() {
        return;
    }
    if s.is_flat() {
        c.fill_rect(r, s.top);
    } else {
        let h = r.height();
        for i in 0..h {
            // (i * 255) / (h - 1) so the last row is exactly `bottom`. A plain
            // i*255/h never reaches the end colour, which shows as a seam where two
            // bands meet.
            let t = if h > 1 { (i * 255 / (h - 1)) as u8 } else { 0 };
            let y = r.y0 + i;
            c.hline(y, r.x0, r.x1, s.top.lerp(s.bottom, t));
        }
    }
    if s.edge_light != s.top {
        c.hline(r.y0, r.x0, r.x1, s.edge_light);
    }
    if s.edge_dark != s.bottom {
        c.hline(r.y1 - 1, r.x0, r.x1, s.edge_dark);
    }
}

/// A band with rounded top corners only — the shape S60 used for the active tab
/// and for a popup's title.
///
/// Implemented as a band plus background-coloured corner notches, because the
/// canvas has no partial-radius fill and adding one for two call sites is not
/// worth the arithmetic. `bg` must be what is actually behind the rect.
pub fn band_top_rounded(c: &mut Canvas, r: Rect, s: &Surface, radius: i32, bg: Color) {
    band(c, r, s);
    let rad = radius.min(r.width() / 2).min(r.height());
    for i in 0..rad {
        // A quarter circle by the same integer test fill_round_rect uses: a row is
        // notched by however much the circle has not yet come in.
        let dy = rad - 1 - i;
        let dx = rad - isqrt(rad * rad - dy * dy);
        if dx > 0 {
            let y = r.y0 + i;
            c.hline(y, r.x0, r.x0 + dx, bg);
            c.hline(y, r.x1 - dx, r.x1, bg);
        }
    }
}

/// Integer square root. `f32::sqrt` is not in `core`, and pulling in a soft-float
/// sqrt to round four corners would be absurd.
fn isqrt(v: i32) -> i32 {
    if v <= 0 {
        return 0;
    }
    let mut x = v;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + v / x) / 2;
    }
    x
}

/// A one-pixel horizontal rule.
///
/// Two colours, not one: a single dark line on a dark background is invisible, and
/// a single light line looks like a scratch. The era drew separators as a dark
/// pixel with a light one under it — an engraved groove, which reads as a division
/// at any background lightness. Pass `None` for the second to get a plain rule.
pub fn separator(c: &mut Canvas, y: i32, x0: i32, x1: i32, dark: Color, light: Option<Color>) {
    c.hline(y, x0, x1, dark);
    if let Some(l) = light {
        c.hline(y + 1, x0, x1, l);
    }
}

/// Derive an engraved separator pair from the surface it divides, so a theme gets
/// one for free instead of naming two more colours.
pub fn separator_for(c: &mut Canvas, y: i32, x0: i32, x1: i32, on: Color) {
    // On a dark surface the groove has to be lighter than the surface to be seen at
    // all; on a light one, darker. Same shape, inverted, which is why this is
    // derived rather than fixed.
    let (a, b) = if luma(on) < 128 {
        (lighten(on, 28), darken(on, 20))
    } else {
        (darken(on, 26), lighten(on, 40))
    };
    separator(c, y, x0, x1, a, Some(b));
}

/// A rectangular outline one pixel wide, lighter at the top-left and darker at the
/// bottom-right — the classic raised frame.
pub fn frame_raised(c: &mut Canvas, r: Rect, light: Color, dark: Color) {
    if r.width() < 2 || r.height() < 2 {
        return;
    }
    c.hline(r.y0, r.x0, r.x1, light);
    c.vline(r.x0, r.y0, r.y1, light);
    c.hline(r.y1 - 1, r.x0, r.x1, dark);
    c.vline(r.x1 - 1, r.y0, r.y1, dark);
}

/// The same frame inverted, so the rect reads as pressed in. Text fields, the
/// message composer, and any "you can put something here" affordance.
pub fn frame_sunken(c: &mut Canvas, r: Rect, light: Color, dark: Color) {
    frame_raised(c, r, dark, light);
}

/// The selection highlight: a full-bleed band across the row.
///
/// Full-bleed on purpose. The era's list highlight ran edge to edge with no inset
/// and no rounding, and that is genuinely better on a keypad device than a rounded
/// inset pill: with no pointer, the highlight is the *only* thing telling you where
/// you are, so it wants to be the loudest object on screen. A rounded inset reads
/// as a button — something you press — rather than as a cursor.
pub fn highlight(c: &mut Canvas, r: Rect, s: &Surface) {
    band(c, r, s);
}

/// A proportional scrollbar in a right-hand gutter.
///
/// Always drawn, never faded out, which is the other thing the era got right: on a
/// screen that shows five rows of a fifty-row list, "where am I and how much is
/// left" is not incidental information, and a scrollbar that appears only while
/// moving answers the question exactly when you no longer need it.
///
/// `total` and `visible` are in the same unit (rows or pixels, either works);
/// `offset` is how far down the viewport starts.
pub fn scrollbar(
    c: &mut Canvas,
    gutter: Rect,
    total: i32,
    visible: i32,
    offset: i32,
    track: Color,
    thumb: Color,
) {
    if gutter.is_empty() {
        return;
    }
    c.fill_rect(gutter, track);
    if total <= visible || total <= 0 {
        // Everything fits: fill the gutter so the bar still reads as "all of it",
        // rather than leaving an empty channel that looks like a rendering bug.
        c.fill_rect(gutter, thumb);
        return;
    }
    let h = gutter.height();
    // A minimum of 4px: a proportional thumb on a 500-message transcript would
    // otherwise be sub-pixel and disappear at exactly the moment it matters most.
    let th = ((h * visible) / total).max(4).min(h);
    let span = h - th;
    let max_off = total - visible;
    let ty = gutter.y0 + if max_off > 0 { (span * offset) / max_off } else { 0 };
    c.fill_rect(Rect::from_xywh(gutter.x0, ty, gutter.width(), th), thumb);
}

/// A rounded band: the surface's gradient inside a rounded rect, with the edge
/// lines drawn across the straight part of the top and bottom.
///
/// The edges stop short of the corners by `radius` rather than running the full
/// width — a highlight line that carries on past where the shape has curved away
/// hangs in the air beside it, which is more visible at a 6px radius than it sounds.
pub fn band_round(c: &mut Canvas, r: Rect, s: &Surface, radius: i32) {
    if r.is_empty() {
        return;
    }
    c.fill_round_rect_shaded(r, radius, s.top, s.bottom);
    let rad = radius.min(r.width() / 2).min(r.height() / 2).max(0);
    if s.edge_light != s.top {
        c.hline(r.y0, r.x0 + rad, r.x1 - rad, s.edge_light);
    }
    if s.edge_dark != s.bottom {
        c.hline(r.y1 - 1, r.x0 + rad, r.x1 - rad, s.edge_dark);
    }
}

/// A filled pill with centred text, for unread counts and status chips.
///
/// The radius is half the height, so it is a stadium at any size and never looks
/// like a rounded rectangle that got it slightly wrong.
pub fn pill(c: &mut Canvas, r: Rect, fill: Color) {
    c.fill_round_rect(r, r.height() / 2, fill);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::Surface;
    use symbian_gfx::Size;

    fn canvas(w: i32, h: i32) -> (alloc::vec::Vec<u16>, Size) {
        (alloc::vec![0u16; (w * h) as usize], Size::new(w, h))
    }

    fn px(buf: &[u16], size: Size, x: i32, y: i32) -> u16 {
        buf[(y * size.w + x) as usize]
    }

    #[test]
    fn band_reaches_both_gradient_stops_exactly() {
        let (mut buf, size) = canvas(4, 8);
        let black = Color::hex(0x000000);
        let white = Color::hex(0xFFFFFF);
        {
            let mut c = Canvas::from_slice(&mut buf, size);
            band(&mut c, Rect::from_xywh(0, 0, 4, 8), &Surface::gradient(black, white));
        }
        // A seam appears where a band's last row is not its end colour, so both
        // ends are asserted rather than just the direction.
        assert_eq!(px(&buf, size, 0, 0), black.to_rgb565().0, "first row must be `top`");
        assert_eq!(px(&buf, size, 0, 7), white.to_rgb565().0, "last row must be `bottom`");
    }

    #[test]
    fn band_gradient_is_monotonic() {
        let (mut buf, size) = canvas(2, 16);
        {
            let mut c = Canvas::from_slice(&mut buf, size);
            band(
                &mut c,
                Rect::from_xywh(0, 0, 2, 16),
                &Surface::gradient(Color::hex(0x000000), Color::hex(0xFFFFFF)),
            );
        }
        let mut last = 0u16;
        for y in 0..16 {
            let v = px(&buf, size, 0, y);
            assert!(v >= last, "row {y} went backwards: {v} < {last}");
            last = v;
        }
    }

    #[test]
    fn band_draws_its_edges_over_the_gradient() {
        let (mut buf, size) = canvas(3, 6);
        let s = Surface {
            top: Color::hex(0x404040),
            bottom: Color::hex(0x202020),
            edge_light: Color::hex(0xFF0000),
            edge_dark: Color::hex(0x00FF00),
        };
        {
            let mut c = Canvas::from_slice(&mut buf, size);
            band(&mut c, Rect::from_xywh(0, 0, 3, 6), &s);
        }
        assert_eq!(px(&buf, size, 1, 0), s.edge_light.to_rgb565().0);
        assert_eq!(px(&buf, size, 1, 5), s.edge_dark.to_rgb565().0);
    }

    #[test]
    fn flat_band_writes_one_colour_everywhere() {
        let (mut buf, size) = canvas(3, 4);
        let c0 = Color::hex(0x123456);
        {
            let mut c = Canvas::from_slice(&mut buf, size);
            band(&mut c, Rect::from_xywh(0, 0, 3, 4), &Surface::flat(c0));
        }
        assert!(buf.iter().all(|&v| v == c0.to_rgb565().0));
    }

    #[test]
    fn band_of_one_row_does_not_divide_by_zero() {
        let (mut buf, size) = canvas(2, 1);
        let mut c = Canvas::from_slice(&mut buf, size);
        band(
            &mut c,
            Rect::from_xywh(0, 0, 2, 1),
            &Surface::gradient(Color::hex(0x000000), Color::hex(0xFFFFFF)),
        );
    }

    #[test]
    fn empty_band_is_a_no_op() {
        let (mut buf, size) = canvas(4, 4);
        {
            let mut c = Canvas::from_slice(&mut buf, size);
            band(&mut c, Rect::from_xywh(2, 2, 0, 0), &Surface::flat(Color::hex(0xFFFFFF)));
        }
        assert!(buf.iter().all(|&v| v == 0));
    }

    #[test]
    fn isqrt_is_the_floor_of_the_root() {
        for v in 0..200i32 {
            let r = isqrt(v);
            assert!(r * r <= v, "isqrt({v}) = {r} is too big");
            assert!((r + 1) * (r + 1) > v, "isqrt({v}) = {r} is too small");
        }
    }

    #[test]
    fn scrollbar_thumb_spans_everything_when_all_fits() {
        let (mut buf, size) = canvas(3, 20);
        let thumb = Color::hex(0xFFFFFF);
        {
            let mut c = Canvas::from_slice(&mut buf, size);
            scrollbar(
                &mut c,
                Rect::from_xywh(0, 0, 3, 20),
                5,
                10,
                0,
                Color::hex(0x000000),
                thumb,
            );
        }
        assert!(
            buf.iter().all(|&v| v == thumb.to_rgb565().0),
            "a list that fits should show a full bar, not an empty channel"
        );
    }

    #[test]
    fn scrollbar_thumb_reaches_the_bottom_at_the_end() {
        let (mut buf, size) = canvas(3, 40);
        let thumb = Color::hex(0xFFFFFF).to_rgb565().0;
        {
            let mut c = Canvas::from_slice(&mut buf, size);
            scrollbar(
                &mut c,
                Rect::from_xywh(0, 0, 3, 40),
                100,
                10,
                90, // scrolled all the way
                Color::hex(0x000000),
                Color::hex(0xFFFFFF),
            );
        }
        assert_eq!(px(&buf, size, 1, 39), thumb, "thumb must touch the bottom at max offset");
        assert_ne!(px(&buf, size, 1, 0), thumb, "and must not still be at the top");
    }

    #[test]
    fn scrollbar_thumb_never_vanishes() {
        let (mut buf, size) = canvas(3, 30);
        let thumb = Color::hex(0xFFFFFF).to_rgb565().0;
        {
            let mut c = Canvas::from_slice(&mut buf, size);
            // 1 visible row out of 5000: proportionally 0.006px.
            scrollbar(
                &mut c,
                Rect::from_xywh(0, 0, 3, 30),
                5000,
                1,
                0,
                Color::hex(0x000000),
                Color::hex(0xFFFFFF),
            );
        }
        let n = (0..30).filter(|&y| px(&buf, size, 1, y) == thumb).count();
        assert!(n >= 4, "thumb collapsed to {n} rows");
    }

    #[test]
    fn scrollbar_thumb_moves_monotonically() {
        let mut last_top = -1i32;
        for offset in [0, 10, 25, 40, 60, 90] {
            let (mut buf, size) = canvas(3, 40);
            let thumb = Color::hex(0xFFFFFF).to_rgb565().0;
            {
                let mut c = Canvas::from_slice(&mut buf, size);
                scrollbar(
                    &mut c,
                    Rect::from_xywh(0, 0, 3, 40),
                    100,
                    10,
                    offset,
                    Color::hex(0x000000),
                    Color::hex(0xFFFFFF),
                );
            }
            let top = (0..40).find(|&y| px(&buf, size, 1, y) == thumb).unwrap() as i32;
            assert!(top >= last_top, "offset {offset} moved the thumb up");
            last_top = top;
        }
    }

    #[test]
    fn separator_draws_two_rows_when_given_a_second_colour() {
        let (mut buf, size) = canvas(4, 4);
        let a = Color::hex(0xFF0000);
        let b = Color::hex(0x00FF00);
        {
            let mut c = Canvas::from_slice(&mut buf, size);
            separator(&mut c, 1, 0, 4, a, Some(b));
        }
        assert_eq!(px(&buf, size, 0, 1), a.to_rgb565().0);
        assert_eq!(px(&buf, size, 0, 2), b.to_rgb565().0);
        assert_eq!(px(&buf, size, 0, 0), 0, "must not bleed upwards");
    }

    #[test]
    fn separator_for_inverts_with_background_lightness() {
        // The groove must be visible either way, which means its first row is
        // lighter than a dark surface and darker than a light one.
        let (mut dark_buf, size) = canvas(4, 4);
        {
            let mut c = Canvas::from_slice(&mut dark_buf, size);
            separator_for(&mut c, 1, 0, 4, Color::hex(0x101010));
        }
        let (mut light_buf, _) = canvas(4, 4);
        {
            let mut c = Canvas::from_slice(&mut light_buf, size);
            separator_for(&mut c, 1, 0, 4, Color::hex(0xF0F0F0));
        }
        assert!(px(&dark_buf, size, 0, 1) > Color::hex(0x101010).to_rgb565().0);
        assert!(px(&light_buf, size, 0, 1) < Color::hex(0xF0F0F0).to_rgb565().0);
    }

    #[test]
    fn frame_sunken_is_frame_raised_with_the_colours_swapped() {
        let (mut a, size) = canvas(6, 6);
        let (mut b, _) = canvas(6, 6);
        let l = Color::hex(0xFFFFFF);
        let d = Color::hex(0x000000);
        {
            let mut c = Canvas::from_slice(&mut a, size);
            frame_raised(&mut c, Rect::from_xywh(0, 0, 6, 6), l, d);
        }
        {
            let mut c = Canvas::from_slice(&mut b, size);
            frame_sunken(&mut c, Rect::from_xywh(0, 0, 6, 6), d, l);
        }
        assert_eq!(a, b);
    }

    #[test]
    fn frame_on_a_degenerate_rect_is_a_no_op() {
        let (mut buf, size) = canvas(4, 4);
        {
            let mut c = Canvas::from_slice(&mut buf, size);
            frame_raised(
                &mut c,
                Rect::from_xywh(1, 1, 1, 1),
                Color::hex(0xFFFFFF),
                Color::hex(0xFFFFFF),
            );
        }
        assert!(buf.iter().all(|&v| v == 0));
    }

    #[test]
    fn band_top_rounded_clears_the_corners_to_the_background() {
        let (mut buf, size) = canvas(20, 12);
        let bg = Color::hex(0xFF0000);
        {
            let mut c = Canvas::from_slice(&mut buf, size);
            band(&mut c, Rect::from_xywh(0, 0, 20, 12), &Surface::flat(bg));
            band_top_rounded(
                &mut c,
                Rect::from_xywh(0, 0, 20, 12),
                &Surface::flat(Color::hex(0x00FF00)),
                4,
                bg,
            );
        }
        assert_eq!(px(&buf, size, 0, 0), bg.to_rgb565().0, "top-left corner not notched");
        assert_eq!(px(&buf, size, 19, 0), bg.to_rgb565().0, "top-right corner not notched");
        assert_ne!(px(&buf, size, 10, 0), bg.to_rgb565().0, "middle of the top edge was eaten");
        assert_ne!(px(&buf, size, 0, 11), bg.to_rgb565().0, "bottom corners must stay square");
    }
}
