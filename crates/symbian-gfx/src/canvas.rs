//! The drawing surface.
//!
//! A `Canvas` borrows a 16bpp pixel buffer plus a clip rectangle and a local
//! origin. Widgets always draw in their own coordinate space starting at (0,0);
//! the canvas translates and clips on their behalf, so a widget cannot scribble
//! outside the box it was given even if it miscalculates.
//!
//! The buffer can be a plain `Vec<u16>` on the host or, on device, the scanline
//! data of a `CFbsBitmap` obtained through the shim — hence the explicit
//! `stride`, since Symbian aligns bitmap scanlines to 4 bytes and a 320-pixel
//! wide bitmap is not guaranteed to have `stride == width`.

use crate::color::{blend565, Color};
use crate::font::Font;
use crate::geom::{Point, Rect, Size};

/// Saved clip/origin state, returned by [`Canvas::save`].
#[derive(Copy, Clone, Debug)]
pub struct CanvasState {
    clip: Rect,
    origin: Point,
}

pub struct Canvas<'a> {
    buf: &'a mut [u16],
    stride: usize,
    size: Size,
    /// Always in surface coordinates and always inside the surface bounds.
    clip: Rect,
    /// Added to incoming local coordinates to reach surface coordinates.
    origin: Point,
    /// Bounding box, in surface coordinates, of every pixel this canvas has
    /// actually *changed the value of* — the region worth presenting. `None`
    /// while nothing has changed. A write of the same value a pixel already held
    /// (a `clear` to the same background as last frame) does not grow it, which
    /// is what makes a dirty-rect present pay on a UI that redraws in full.
    damage: Option<Rect>,
}

impl<'a> Canvas<'a> {
    /// Wrap a pixel buffer. `stride` is in pixels, not bytes.
    ///
    /// # Panics
    /// If the buffer is too small for `stride * height`, or the geometry is
    /// negative. This is a programming error at setup time, not a runtime
    /// condition worth propagating.
    pub fn new(buf: &'a mut [u16], size: Size, stride: usize) -> Self {
        assert!(size.w >= 0 && size.h >= 0, "negative canvas size {size:?}");
        assert!(stride >= size.w as usize, "stride {stride} narrower than width {}", size.w);
        assert!(
            buf.len() >= stride * size.h as usize,
            "buffer holds {} px, need {}",
            buf.len(),
            stride * size.h as usize
        );
        Self { buf, stride, size, clip: Rect::from_size(size), origin: Point::ZERO, damage: None }
    }

    /// Wrap a tightly packed buffer.
    pub fn from_slice(buf: &'a mut [u16], size: Size) -> Self {
        let stride = size.w.max(0) as usize;
        Self::new(buf, size, stride)
    }

    #[inline]
    pub fn size(&self) -> Size {
        self.size
    }

    #[inline]
    pub fn stride(&self) -> usize {
        self.stride
    }

    /// The visible region, in local coordinates. Widgets can use this to skip
    /// work for content that is scrolled out of view.
    #[inline]
    pub fn clip_local(&self) -> Rect {
        self.clip.translate(Point::new(-self.origin.x, -self.origin.y))
    }

    #[inline]
    pub fn is_fully_clipped(&self) -> bool {
        self.clip.is_empty()
    }

    /// The bounding box, in surface coordinates, of every pixel whose value this
    /// canvas actually changed. `None` means the frame drew nothing new — the
    /// caller can skip the present entirely. Otherwise it is the smallest rect
    /// that needs to reach the screen, which the shim's `present(x, y, w, h)`
    /// expands and blits directly.
    #[inline]
    pub fn damage(&self) -> Option<Rect> {
        self.damage
    }

    /// Forget accumulated damage, e.g. when reusing a canvas after a full present.
    #[inline]
    pub fn clear_damage(&mut self) {
        self.damage = None;
    }

    /// Grow the damage box to include `r` (surface coordinates, already clipped).
    #[inline]
    fn mark_damage(&mut self, r: Rect) {
        if r.is_empty() {
            return;
        }
        self.damage = Some(match self.damage {
            Some(d) => d.union(r),
            None => r,
        });
    }

    #[inline]
    pub fn save(&self) -> CanvasState {
        CanvasState { clip: self.clip, origin: self.origin }
    }

    #[inline]
    pub fn restore(&mut self, s: CanvasState) {
        self.clip = s.clip;
        self.origin = s.origin;
    }

    /// Shift the local coordinate space.
    #[inline]
    pub fn translate(&mut self, d: Point) {
        self.origin += d;
    }

    /// Narrow the clip to `r` (local coordinates). Clipping only ever shrinks,
    /// so a child can never widen its way out of its parent's bounds.
    #[inline]
    pub fn clip_to(&mut self, r: Rect) {
        self.clip = self.clip.intersect(r.translate(self.origin));
    }

    /// Enter `r`: clip to it and move the origin to its top-left corner, so the
    /// child draws from (0,0). Returns the state to hand back to [`restore`].
    ///
    /// [`restore`]: Canvas::restore
    #[inline]
    pub fn enter(&mut self, r: Rect) -> CanvasState {
        let saved = self.save();
        self.clip_to(r);
        self.translate(r.origin());
        saved
    }

    /// Run `f` with the canvas clipped and translated into `r`, then restore.
    /// Preferred over manual save/restore because it cannot leak state on an
    /// early return.
    pub fn with(&mut self, r: Rect, f: impl FnOnce(&mut Canvas<'_>)) {
        let saved = self.enter(r);
        if !self.clip.is_empty() {
            f(self);
        }
        self.restore(saved);
    }

    /// Local rect to the clipped surface-space rect that is safe to touch.
    #[inline]
    fn resolve(&self, r: Rect) -> Rect {
        self.clip.intersect(r.translate(self.origin))
    }

    /// One row of the surface, `x0..x1`, already bounds-checked.
    #[inline]
    fn row(&mut self, y: i32, x0: i32, x1: i32) -> &mut [u16] {
        let base = y as usize * self.stride;
        &mut self.buf[base + x0 as usize..base + x1 as usize]
    }

    // ---------------------------------------------------------------- fills

    /// Fill the entire clip with an opaque colour.
    pub fn clear(&mut self, color: Color) {
        let all = self.clip_local();
        self.fill_rect(all, color);
    }

    pub fn fill_rect(&mut self, r: Rect, color: Color) {
        let a = color.a();
        if a == 0 {
            return;
        }
        let dst = self.resolve(r);
        if dst.is_empty() {
            return;
        }
        let src = color.to_rgb565().0;

        // Write and detect change in one pass: a pixel already holding the target
        // value contributes no damage, so a background repainted identically each
        // frame costs a read but presents nothing. The old fast `slice::fill` gave
        // up that information; on this target the read is cheap next to the present.
        let mut changed = false;
        if a == 0xFF {
            for y in dst.y0..dst.y1 {
                for px in self.row(y, dst.x0, dst.x1) {
                    if *px != src {
                        *px = src;
                        changed = true;
                    }
                }
            }
        } else {
            for y in dst.y0..dst.y1 {
                for px in self.row(y, dst.x0, dst.x1) {
                    let nv = blend565(*px, src, a);
                    if nv != *px {
                        *px = nv;
                        changed = true;
                    }
                }
            }
        }
        if changed {
            self.mark_damage(dst);
        }
    }

    /// A 1px-per-side outline drawn just inside `r`.
    pub fn stroke_rect(&mut self, r: Rect, color: Color) {
        self.stroke_rect_width(r, color, 1);
    }

    pub fn stroke_rect_width(&mut self, r: Rect, color: Color, w: i32) {
        if w <= 0 || r.is_empty() {
            return;
        }
        // Clamp so a thick border on a small rect degrades to a solid fill
        // instead of drawing the sides twice.
        let w = w.min(r.width().min(r.height()));
        self.fill_rect(Rect::new(r.x0, r.y0, r.x1, r.y0 + w), color);
        self.fill_rect(Rect::new(r.x0, r.y1 - w, r.x1, r.y1), color);
        self.fill_rect(Rect::new(r.x0, r.y0 + w, r.x0 + w, r.y1 - w), color);
        self.fill_rect(Rect::new(r.x1 - w, r.y0 + w, r.x1, r.y1 - w), color);
    }

    pub fn hline(&mut self, y: i32, x0: i32, x1: i32, color: Color) {
        self.fill_rect(Rect::new(x0.min(x1), y, x0.max(x1), y + 1), color);
    }

    pub fn vline(&mut self, x: i32, y0: i32, y1: i32, color: Color) {
        self.fill_rect(Rect::new(x, y0.min(y1), x + 1, y0.max(y1)), color);
    }

    /// Rounded rectangle with square-cut corners approximated per scanline.
    /// Corner coverage is computed analytically rather than by sampling a
    /// circle, which keeps it allocation-free and looks clean at the 2-6px radii
    /// a 320x240 UI actually uses.
    pub fn fill_round_rect(&mut self, r: Rect, radius: i32, color: Color) {
        self.fill_round_rect_shaded(r, radius, color, color);
    }

    /// A rounded rectangle filled with a vertical gradient from `top` to `bottom`.
    ///
    /// The gradient has to happen *inside* the corner arithmetic rather than as a
    /// pass over the result: the only other way to shade a rounded shape is to fill
    /// it flat and then draw gradient rows over the interior, which leaves the corner
    /// pixels at the wrong colour. Equal `top` and `bottom` take the flat path, so
    /// [`Self::fill_round_rect`] costs nothing extra.
    pub fn fill_round_rect_shaded(&mut self, r: Rect, radius: i32, top: Color, bottom: Color) {
        if r.is_empty() {
            return;
        }
        let flat = top == bottom;
        let h = r.height();
        // Colour of row `y`, absolute. Hoisted so both the middle band and the caps
        // agree — computing it twice from different origins is how a seam appears
        // between the straight part and the corners.
        let shade = |y: i32| -> Color {
            if flat || h <= 1 {
                top
            } else {
                top.lerp(bottom, ((y - r.y0) * 255 / (h - 1)) as u8)
            }
        };

        let rad = radius.min(r.width() / 2).min(r.height() / 2);
        if rad <= 0 {
            if flat {
                self.fill_rect(r, top);
            } else {
                for y in r.y0..r.y1 {
                    self.hline(y, r.x0, r.x1, shade(y));
                }
            }
            return;
        }

        // Middle band: full width.
        if flat {
            self.fill_rect(Rect::new(r.x0, r.y0 + rad, r.x1, r.y1 - rad), top);
        } else {
            for y in (r.y0 + rad)..(r.y1 - rad) {
                self.hline(y, r.x0, r.x1, shade(y));
            }
        }

        // Caps: inset each row by the horizontal chord of the corner circle.
        //
        // Integer-only, for two reasons: `core` has no `f32::sqrt` (it lives in
        // std, via libm), and float code on this target is something to avoid on
        // principle — see targets/README.md on the stack cost of soft-float.
        //
        // We want dx = sqrt(rad² - dy²) with dy = rad - 0.5 - i. Working in
        // doubled coordinates clears the half-pixel: (2dx)² = (2rad)² - (2dy)².
        let two_rad_sq = (2 * rad) * (2 * rad);
        for i in 0..rad {
            let two_dy = 2 * rad - 1 - 2 * i;
            let two_dx = (two_rad_sq - two_dy * two_dy).max(0).isqrt();
            // Halve with rounding to get back to whole pixels.
            let inset = rad - (two_dx + 1) / 2;
            let x0 = r.x0 + inset;
            let x1 = r.x1 - inset;
            let (ty, by) = (r.y0 + i, r.y1 - i - 1);
            self.hline(ty, x0, x1, shade(ty));
            self.hline(by, x0, x1, shade(by));
        }
    }

    // ----------------------------------------------------------- blitting

    /// Copy an RGB565 image. `src_stride` is in pixels.
    pub fn blit(&mut self, at: Point, src: &[u16], src_size: Size, src_stride: usize) {
        if src_size.is_empty() {
            return;
        }
        let target = Rect::from_origin_size(at, src_size);
        let dst = self.resolve(target);
        if dst.is_empty() {
            return;
        }
        // Where inside the source the clipped region begins.
        let skip = dst.origin() - target.translate(self.origin).origin();
        let mut changed = false;
        for y in 0..dst.height() {
            let s0 = (skip.y + y) as usize * src_stride + skip.x as usize;
            let width = dst.width() as usize;
            let sy = &src[s0..s0 + width];
            for (px, &s) in self.row(dst.y0 + y, dst.x0, dst.x1).iter_mut().zip(sy) {
                if *px != s {
                    *px = s;
                    changed = true;
                }
            }
        }
        if changed {
            self.mark_damage(dst);
        }
    }

    /// Composite an 8-bit coverage mask in a single colour. This is the glyph
    /// path, and also how icons drawn as alpha masks get tinted by the theme.
    pub fn blit_mask(&mut self, at: Point, mask: &[u8], mask_size: Size, mask_stride: usize, color: Color) {
        if mask_size.is_empty() || color.a() == 0 {
            return;
        }
        let target = Rect::from_origin_size(at, mask_size);
        let dst = self.resolve(target);
        if dst.is_empty() {
            return;
        }
        let skip = dst.origin() - target.translate(self.origin).origin();
        let src = color.to_rgb565().0;
        let alpha = color.a() as u32;

        let mut changed = false;
        for y in 0..dst.height() {
            let m0 = (skip.y + y) as usize * mask_stride + skip.x as usize;
            let width = dst.width() as usize;
            let mrow = &mask[m0..m0 + width];
            let prow = self.row(dst.y0 + y, dst.x0, dst.x1);
            for (px, &cov) in prow.iter_mut().zip(mrow) {
                if cov == 0 {
                    continue;
                }
                // Fold the colour's own alpha into the mask coverage.
                let c = if alpha == 0xFF { cov } else { ((cov as u32 * alpha) / 255) as u8 };
                let nv = blend565(*px, src, c);
                if nv != *px {
                    *px = nv;
                    changed = true;
                }
            }
        }
        if changed {
            self.mark_damage(dst);
        }
    }

    /// Draw a full-colour RGB565 image with a companion 8-bit coverage mask,
    /// nearest-neighbour scaled to fill `dst`. This is the app-icon path: unlike
    /// [`blit`] it keeps the source's own colours (not one tint), and unlike
    /// [`blit_mask`] the coverage is per-pixel transparency carried alongside the
    /// pixels — exactly the shape of a Symbian `CApaMaskedBitmap` (colour plane +
    /// mask plane). `src` and `mask` are row-major with the same `src_stride`
    /// (in pixels) and the same dimensions `src_size`; a mask byte of 0 is fully
    /// transparent, 255 fully opaque, and anything between is blended.
    ///
    /// Scaling is nearest-neighbour and integer-only, on purpose: the target is a
    /// soft-float handset, an icon is drawn once per frame at small sizes, and the
    /// alternative (sampling/filtering) buys nothing a 320x240 screen would show.
    pub fn blit_icon(&mut self, dst: Rect, src: &[u16], mask: &[u8], src_size: Size, src_stride: usize) {
        if src_size.is_empty() || dst.is_empty() {
            return;
        }
        // The target in surface space *before* clipping — the scale is defined
        // against the full requested rect, so a partly-clipped icon keeps its
        // proportions instead of stretching the visible sliver.
        let target = dst.translate(self.origin);
        let clipped = self.clip.intersect(target);
        if clipped.is_empty() {
            return;
        }
        let (dw, dh) = (target.width(), target.height());
        let (sw, sh) = (src_size.w, src_size.h);

        let mut changed = false;
        for y in clipped.y0..clipped.y1 {
            // Map this surface row back through the unclipped target to a source row.
            let sy = (((y - target.y0) * sh / dh).clamp(0, sh - 1)) as usize;
            let row_base = sy * src_stride;
            let prow = self.row(y, clipped.x0, clipped.x1);
            for (i, px) in prow.iter_mut().enumerate() {
                let dx = clipped.x0 + i as i32;
                let sx = (((dx - target.x0) * sw / dw).clamp(0, sw - 1)) as usize;
                let idx = row_base + sx;
                let cov = mask[idx];
                if cov == 0 {
                    continue;
                }
                let nv = blend565(*px, src[idx], cov);
                if nv != *px {
                    *px = nv;
                    changed = true;
                }
            }
        }
        if changed {
            self.mark_damage(clipped);
        }
    }

    // --------------------------------------------------------------- text

    /// Draw `text` with its baseline at `baseline`. Returns the advance width,
    /// so callers can chain runs without re-measuring.
    pub fn draw_text(&mut self, baseline: Point, text: &str, font: &dyn Font, color: Color) -> i32 {
        let mut pen = baseline.x;
        for ch in text.chars() {
            if let Some(g) = font.glyph(ch) {
                if !g.coverage.is_empty() {
                    self.blit_mask(
                        Point::new(pen + g.bearing_x, baseline.y - g.bearing_y),
                        g.coverage,
                        Size::new(g.width, g.height),
                        g.width.max(0) as usize,
                        color,
                    );
                }
                pen += g.advance;
            } else {
                pen += font.fallback_advance();
            }
        }
        pen - baseline.x
    }

    /// Draw `text` inside `r`, positioned by `align`, vertically centred on the
    /// font's own metrics. Truncates with an ellipsis when it does not fit.
    pub fn draw_text_in(
        &mut self,
        r: Rect,
        text: &str,
        font: &dyn Font,
        color: Color,
        align: Align,
    ) -> i32 {
        let avail = r.width();
        let fitted = font.fit(text, avail);
        let ellipsis = if fitted.ellipsized { font.ellipsis() } else { "" };
        let total = fitted.width + font.measure(ellipsis);
        let x = match align {
            Align::Start => r.x0,
            Align::Center => r.x0 + (avail - total) / 2,
            Align::End => r.x1 - total,
        };
        // Centre the em box rather than the glyph ink, so labels on adjacent rows
        // line up even when one of them happens to have no descenders.
        let y = r.y0 + (r.height() - font.line_height()) / 2 + font.ascent();
        let mut drawn = self.draw_text(Point::new(x, y), fitted.text, font, color);
        if fitted.ellipsized {
            drawn += self.draw_text(Point::new(x + drawn, y), ellipsis, font, color);
        }
        drawn
    }
}

/// Horizontal placement within a box.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Rgb565;

    fn surface(w: i32, h: i32) -> (alloc::vec::Vec<u16>, Size) {
        (alloc::vec![0u16; (w * h) as usize], Size::new(w, h))
    }

    #[test]
    fn fill_respects_clip() {
        let (mut buf, size) = surface(8, 8);
        let mut c = Canvas::from_slice(&mut buf, size);
        c.clip_to(Rect::from_xywh(2, 2, 4, 4));
        c.fill_rect(Rect::from_size(size), Color::WHITE);
        drop(c);

        for y in 0..8 {
            for x in 0..8 {
                let inside = (2..6).contains(&x) && (2..6).contains(&y);
                let px = buf[(y * 8 + x) as usize];
                assert_eq!(px != 0, inside, "at {x},{y}");
            }
        }
    }

    #[test]
    fn enter_translates_and_clips() {
        let (mut buf, size) = surface(8, 8);
        let mut c = Canvas::from_slice(&mut buf, size);
        c.with(Rect::from_xywh(4, 4, 2, 2), |c| {
            // Local (0,0) must land at surface (4,4), and the overspill clipped.
            c.fill_rect(Rect::from_xywh(0, 0, 100, 100), Color::WHITE);
        });
        drop(c);
        assert_ne!(buf[(4 * 8 + 4) as usize], 0);
        assert_ne!(buf[(5 * 8 + 5) as usize], 0);
        assert_eq!(buf[(6 * 8 + 6) as usize], 0);
        assert_eq!(buf[(3 * 8 + 3) as usize], 0);
    }

    #[test]
    fn negative_coordinates_clip_rather_than_wrap() {
        let (mut buf, size) = surface(8, 8);
        let mut c = Canvas::from_slice(&mut buf, size);
        c.fill_rect(Rect::new(-100, -100, 2, 2), Color::WHITE);
        drop(c);
        assert_ne!(buf[0], 0);
        assert_ne!(buf[(1 * 8 + 1) as usize], 0);
        assert_eq!(buf[(2 * 8 + 2) as usize], 0);
        // Nothing may have landed on the last row via a wrapped index.
        assert!(buf[(7 * 8) as usize..].iter().all(|&p| p == 0));
    }

    #[test]
    fn stride_larger_than_width_is_honoured() {
        // 4 visible px per row, 6 px of storage: the 2 padding px stay pristine.
        let mut buf = alloc::vec![0u16; 6 * 4];
        let mut c = Canvas::new(&mut buf, Size::new(4, 4), 6);
        c.fill_rect(Rect::from_xywh(0, 0, 4, 4), Color::WHITE);
        drop(c);
        for y in 0..4usize {
            for x in 0..6usize {
                let expect_set = x < 4;
                assert_eq!(buf[y * 6 + x] != 0, expect_set, "at {x},{y}");
            }
        }
    }

    #[test]
    fn blit_clips_at_all_four_edges() {
        let (mut buf, size) = surface(6, 6);
        let src = alloc::vec![Rgb565::WHITE.0; 4 * 4];
        let mut c = Canvas::from_slice(&mut buf, size);
        c.blit(Point::new(-2, -2), &src, Size::new(4, 4), 4);
        c.blit(Point::new(4, 4), &src, Size::new(4, 4), 4);
        drop(c);
        assert_ne!(buf[0], 0, "top-left corner should be covered");
        assert_ne!(buf[6 * 5 + 5], 0, "bottom-right corner should be covered");
        assert_eq!(buf[6 * 0 + 3], 0, "middle should be untouched");
    }

    #[test]
    fn mask_blit_skips_zero_coverage() {
        let (mut buf, size) = surface(4, 1);
        let mask = [0u8, 255, 0, 255];
        let mut c = Canvas::from_slice(&mut buf, size);
        c.blit_mask(Point::ZERO, &mask, Size::new(4, 1), 4, Color::WHITE);
        drop(c);
        assert_eq!(buf[0], 0);
        assert_ne!(buf[1], 0);
        assert_eq!(buf[2], 0);
        assert_ne!(buf[3], 0);
    }

    #[test]
    fn icon_blit_scales_keeps_colour_and_honours_mask() {
        // A 2x2 source: red, green / blue, transparent. Scaled 2x into a 4x4 dst.
        let r = Color::rgb(0xFF, 0, 0).to_rgb565().0;
        let g = Color::rgb(0, 0xFF, 0).to_rgb565().0;
        let b = Color::rgb(0, 0, 0xFF).to_rgb565().0;
        let src = [r, g, b, 0u16];
        let mask = [255u8, 255, 255, 0]; // bottom-right transparent
        let (mut buf, size) = surface(4, 4);
        let mut c = Canvas::from_slice(&mut buf, size);
        c.blit_icon(Rect::from_xywh(0, 0, 4, 4), &src, &mask, Size::new(2, 2), 2);
        drop(c);
        // Each source pixel expands to a 2x2 block; colours are preserved (not tinted).
        assert_eq!(buf[0 * 4 + 0], r, "top-left quadrant is the red source pixel");
        assert_eq!(buf[0 * 4 + 3], g, "top-right quadrant is the green source pixel");
        assert_eq!(buf[3 * 4 + 0], b, "bottom-left quadrant is the blue source pixel");
        // Bottom-right quadrant had zero coverage, so the surface stays untouched.
        assert_eq!(buf[3 * 4 + 3], 0, "masked-out quadrant is left transparent");
    }

    #[test]
    fn icon_blit_clips_without_stretching() {
        // A 2x2 opaque source drawn as a 4x4 icon anchored at (-2,-2). Only the
        // bottom-right quarter (surface 0..2, 0..2) is visible, and because the
        // scale is anchored to the *full* 4x4 target, that whole quarter samples
        // the source's own bottom-right pixel — a stretched-to-the-sliver bug
        // would instead show all four source colours.
        let w = Color::WHITE.to_rgb565().0;
        let src = [
            Color::rgb(0xFF, 0, 0).to_rgb565().0,
            Color::rgb(0, 0xFF, 0).to_rgb565().0,
            Color::rgb(0, 0, 0xFF).to_rgb565().0,
            w,
        ];
        let mask = [255u8; 4];
        let (mut buf, size) = surface(4, 4);
        let mut c = Canvas::from_slice(&mut buf, size);
        c.blit_icon(Rect::from_xywh(-2, -2, 4, 4), &src, &mask, Size::new(2, 2), 2);
        drop(c);
        assert_eq!(buf[0], w, "visible (0,0) samples the source bottom-right pixel");
        assert_eq!(buf[1], w, "visible (1,0) too — the whole quarter is that one pixel");
        assert_eq!(buf[4], w, "visible (0,1) too");
        assert_eq!(buf[2], 0, "surface x=2 is outside the icon");
        assert_eq!(buf[8], 0, "surface row 2 is outside the icon");
    }

    #[test]
    fn thick_border_on_tiny_rect_does_not_double_draw() {
        let (mut buf, size) = surface(4, 4);
        let mut c = Canvas::from_slice(&mut buf, size);
        // Radius/width larger than the rect must degrade gracefully.
        c.stroke_rect_width(Rect::from_xywh(0, 0, 3, 3), Color::WHITE, 10);
        drop(c);
        for y in 0..3 {
            for x in 0..3 {
                assert_ne!(buf[(y * 4 + x) as usize], 0, "at {x},{y}");
            }
        }
    }

    #[test]
    fn round_rect_clears_its_corners_but_keeps_its_middle() {
        let (mut buf, size) = surface(16, 16);
        let mut c = Canvas::from_slice(&mut buf, size);
        c.fill_round_rect(Rect::from_size(size), 5, Color::WHITE);
        drop(c);
        assert_eq!(buf[0], 0, "corner pixel should be outside the shape");
        assert_ne!(buf[16 * 8 + 8], 0, "centre should be filled");
        assert_ne!(buf[16 * 8], 0, "middle of the left edge should be filled");
    }

    #[test]
    fn zero_alpha_draws_nothing() {
        let (mut buf, size) = surface(4, 4);
        let mut c = Canvas::from_slice(&mut buf, size);
        c.fill_rect(Rect::from_size(size), Color::WHITE.with_alpha(0));
        drop(c);
        assert!(buf.iter().all(|&p| p == 0));
    }

    // ---- damage tracking (the dirty-rect present) --------------------------

    #[test]
    fn nothing_drawn_leaves_no_damage() {
        let (mut buf, size) = surface(16, 16);
        let c = Canvas::from_slice(&mut buf, size);
        assert_eq!(c.damage(), None);
    }

    #[test]
    fn a_fill_reports_exactly_its_rect_as_damage() {
        let (mut buf, size) = surface(16, 16);
        let mut c = Canvas::from_slice(&mut buf, size);
        c.fill_rect(Rect::from_xywh(2, 3, 4, 5), Color::WHITE);
        assert_eq!(c.damage(), Some(Rect::from_xywh(2, 3, 4, 5)));
    }

    #[test]
    fn repainting_the_same_value_adds_no_damage() {
        // This is the whole point: an app that clears to the same background each
        // frame must not report the whole screen as dirty.
        let (mut buf, size) = surface(16, 16);
        let mut c = Canvas::from_slice(&mut buf, size);
        c.fill_rect(Rect::from_size(size), Color::WHITE);
        assert!(c.damage().is_some());
        c.clear_damage();
        c.fill_rect(Rect::from_size(size), Color::WHITE); // identical repaint
        assert_eq!(c.damage(), None, "a no-op repaint should not be presented");
    }

    #[test]
    fn only_the_pixels_that_change_bound_the_damage() {
        // Fill the whole surface, forget that, then change one small rect. The
        // damage is that rect, not the whole surface — even though the app "drew"
        // over everything in between.
        let (mut buf, size) = surface(16, 16);
        let mut c = Canvas::from_slice(&mut buf, size);
        c.fill_rect(Rect::from_size(size), Color::WHITE);
        c.clear_damage();
        c.fill_rect(Rect::from_size(size), Color::WHITE); // no-op
        c.fill_rect(Rect::from_xywh(4, 4, 3, 3), Color::BLACK); // the real change
        assert_eq!(c.damage(), Some(Rect::from_xywh(4, 4, 3, 3)));
    }

    #[test]
    fn damage_is_the_union_of_every_change() {
        let (mut buf, size) = surface(16, 16);
        let mut c = Canvas::from_slice(&mut buf, size);
        c.fill_rect(Rect::from_xywh(0, 0, 2, 2), Color::WHITE);
        c.fill_rect(Rect::from_xywh(10, 10, 2, 2), Color::WHITE);
        assert_eq!(c.damage(), Some(Rect::from_xywh(0, 0, 12, 12)));
    }

    #[test]
    fn damage_is_clipped_to_what_was_actually_touched() {
        // A fill spilling past the surface only damages the on-surface part.
        let (mut buf, size) = surface(8, 8);
        let mut c = Canvas::from_slice(&mut buf, size);
        c.fill_rect(Rect::from_xywh(6, 6, 10, 10), Color::WHITE);
        assert_eq!(c.damage(), Some(Rect::from_xywh(6, 6, 2, 2)));
    }
}
