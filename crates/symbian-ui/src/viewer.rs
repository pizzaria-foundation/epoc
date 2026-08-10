//! Full-screen image viewer with panning.
//!
//! For a screen that shows one decoded image larger than the display: it pans with the D-pad,
//! clamps at the edges, centres whichever axis has room to spare, and blits once per frame.
//!
//! The image arrives as pixels and a size rather than as a decoded-image type, so this
//! toolkit keeps depending only on `symbian-gfx` and `symbian-sys` — `symbian::Image` is
//! exactly `{ pixels, width, height }`, so a caller holding one passes its three fields.
//!
//! Titles are the caller's: text on screen is the application's language, not the SDK's.
//!
//! Lifted out of the Telegram client's photo screen, and the two bugs it was shaped by are
//! worth keeping in mind because both are invisible until someone pans to an edge:
//! panning and drawing must clamp against *the same* rectangle (see [`Viewer::content`]),
//! and an image that decoded to zero pixels must draw nothing rather than panic — a panic on
//! this device is a silent vanish.

use alloc::vec::Vec;

use crate::{chrome, Canvas, Frame, Handled, Key, KeyEvent, Point, Rect, Size, Theme};

/// How far one D-pad press pans. A fifth of the screen: small enough to aim with, large
/// enough to cross a 320-pixel-wide photo in a few presses rather than sixteen.
const PAN: i32 = 48;

pub struct Viewer {
    pixels: Vec<u16>,
    width: i32,
    height: i32,
    scroll_x: i32,
    scroll_y: i32,
}

pub enum ViewerAction {
    /// The user asked to leave. What that means is the caller's business — this screen does
    /// not know what it was opened from.
    Back,
    None,
}

impl Viewer {
    /// `pixels` is RGB565, `size.width` wide, row-major and at least `width * height` long.
    /// A shorter buffer draws nothing rather than panicking.
    pub fn new(pixels: Vec<u16>, size: Size) -> Self {
        Self { pixels, width: size.w, height: size.h, scroll_x: 0, scroll_y: 0 }
    }

    /// The image's size, as given.
    pub fn size(&self) -> Size {
        Size::new(self.width, self.height)
    }
    /// The area the image is drawn into. Both panning and drawing derive their limits
    /// from this, which is the fix for the bug where they disagreed: the scroll was
    /// clamped against the whole screen while the drawing clipped to the content box, so
    /// the bottom of a tall photo could be scrolled to but never shown.
    pub fn content(screen: Rect, theme: &Theme<'_>) -> Rect {
        Frame::split(screen, theme, true, true).content.inset_xy(2, 2)
    }

    fn clamp(&mut self, area: Rect) {
        let max_x = (self.width - area.width()).max(0);
        let max_y = (self.height - area.height()).max(0);
        self.scroll_x = self.scroll_x.clamp(0, max_x);
        self.scroll_y = self.scroll_y.clamp(0, max_y);
    }

    /// Takes the content area rather than the theme and the screen, because the area is
    /// all it ever wanted from them — and a caller that hands over the wrong rectangle is
    /// then the caller with the bug, rather than this screen silently panning against
    /// dimensions the drawing does not share.
    pub fn handle_key(&mut self, ev: KeyEvent, area: Rect) -> (Handled, ViewerAction) {
        match ev.key {
            Key::Softkey(crate::Softkey::Right) | Key::Softkey(crate::Softkey::Middle) => {
                (Handled::Consumed, ViewerAction::Back)
            }
            Key::Down => {
                self.scroll_y += PAN;
                self.clamp(area);
                (Handled::Consumed, ViewerAction::None)
            }
            Key::Up => {
                self.scroll_y -= PAN;
                self.clamp(area);
                (Handled::Consumed, ViewerAction::None)
            }
            Key::Right => {
                self.scroll_x += PAN;
                self.clamp(area);
                (Handled::Consumed, ViewerAction::None)
            }
            Key::Left => {
                self.scroll_x -= PAN;
                self.clamp(area);
                (Handled::Consumed, ViewerAction::None)
            }
            _ => (Handled::Ignored, ViewerAction::None),
        }
    }

    /// `title` and `back` are the caller's words: this crate ships no text.
    pub fn draw(&self, c: &mut Canvas<'_>, theme: &Theme<'_>, title: &str, back: &str) {
        let screen = Rect::from_size(c.size());
        let frame = Frame::split(screen, theme, true, true);
        chrome::clear(c, theme);
        chrome::title_bar(c, frame.title, theme, title, None);
        chrome::softkey_bar(c, frame.softkeys, theme, [None, None, Some(back)]);

        let area = frame.content.inset_xy(2, 2);
        if area.is_empty() || self.width <= 0 || self.height <= 0 {
            return;
        }

        let draw_w = self.width.min(area.width());
        let draw_h = self.height.min(area.height());
        // Centre whichever axis has room to spare.
        let ox = area.x0 + ((area.width() - draw_w) / 2).max(0);
        let oy = area.y0 + ((area.height() - draw_h) / 2).max(0);

        let src_x = self.scroll_x.clamp(0, (self.width - draw_w).max(0));
        let src_y = self.scroll_y.clamp(0, (self.height - draw_h).max(0));

        let saved = c.save();
        c.clip_to(area);
        // One blit for the whole window. Canvas::blit takes a stride and clips on its
        // own, so the per-scanline loop this replaces was doing the same arithmetic
        // draw_h times — and carried a length it computed but never passed, which looked
        // like a bounds check and was not one.
        let first = (src_y as usize) * (self.width as usize) + src_x as usize;
        if let Some(src) = self.pixels.get(first..) {
            c.blit(
                Point::new(ox, oy),
                src,
                Size::new(draw_w, draw_h),
                self.width as usize,
            );
        }
        c.restore(saved);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Softkey;

    fn viewer(w: i32, h: i32) -> Viewer {
        Viewer::new(alloc::vec![0x1234; (w * h) as usize], Size::new(w, h))
    }

    /// Stands in for the content box a 320x240 screen leaves once the title and softkey
    /// bars have taken their share. An approximation on purpose: what these tests are
    /// about is that panning and drawing clamp against *the same* rectangle, whatever
    /// its exact height turns out to be.
    fn area() -> Rect {
        Rect::from_origin_size(Point::new(2, 20), Size::new(316, 200))
    }

    fn press(v: &mut Viewer, key: Key) -> ViewerAction {
        v.handle_key(KeyEvent::new(key), area()).1
    }

    #[test]
    fn panning_stops_at_the_edges_instead_of_running_off() {
        let mut v = viewer(1000, 1000);
        for _ in 0..100 {
            press(&mut v, Key::Down);
            press(&mut v, Key::Right);
        }
        assert_eq!(v.scroll_y, 1000 - area().height());
        assert_eq!(v.scroll_x, 1000 - area().width());

        for _ in 0..100 {
            press(&mut v, Key::Up);
            press(&mut v, Key::Left);
        }
        assert_eq!((v.scroll_x, v.scroll_y), (0, 0));
    }

    #[test]
    fn the_far_edge_is_reachable() {
        // The bug this pins: the limit used to come from the whole screen while the
        // drawing clipped to the content box, so the last twenty rows of a tall photo
        // could be scrolled to and never appeared. Panning to the end must leave exactly
        // the content box showing the bottom of the image.
        let mut v = viewer(100, 1000);
        for _ in 0..100 {
            press(&mut v, Key::Down);
        }
        assert_eq!(v.scroll_y + area().height(), 1000);
    }

    #[test]
    fn an_image_smaller_than_the_screen_does_not_pan_at_all() {
        let mut v = viewer(64, 64);
        press(&mut v, Key::Down);
        press(&mut v, Key::Right);
        assert_eq!((v.scroll_x, v.scroll_y), (0, 0));
    }

    #[test]
    fn the_right_softkey_asks_to_leave() {
        let mut v = viewer(64, 64);
        assert!(matches!(
            press(&mut v, Key::Softkey(Softkey::Right)),
            ViewerAction::Back
        ));
    }

    #[test]
    fn an_empty_image_draws_nothing_rather_than_panicking() {
        // What a decode that reported success with no pixels would leave. A panic on the
        // device is a silent vanish, so the draw path has to tolerate it.
        let v = Viewer::new(alloc::vec::Vec::new(), Size::new(0, 0));
        assert_eq!(v.size(), Size::new(0, 0));
        // The draw path must survive it too, which is the half a field assertion misses.
        crate::testing::with_canvas(Size::new(320, 240), |c| {
            crate::testing::with_theme(crate::Palette::DARK, |theme| v.draw(c, theme, "t", "b"));
        });
    }
}
