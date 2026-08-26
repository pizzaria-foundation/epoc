//! Something the screen wants to say, at the top of it.
//!
//! Two shapes, one file, because they differ in exactly one thing: how they go away.
//!
//! * A **toast** goes away on its own. `update` arms a timer and clears it.
//! * A **notice** stays until the user does something about it.
//!
//! Everything else — where it sits, how it is measured, what it looks like, that it never takes a
//! key — is the same, so it is one widget with a constructor each rather than two files that agree by
//! inspection. That is the argument [`tick`](symbian_ui::tick) makes for keeping a checkbox and a
//! radio button together, applied again.
//!
//! # The timer is `update`'s, not the widget's
//!
//! A toast holds no clock. The model holds the message and `update` returns
//! [`Cmd::SetTimer`](crate::Cmd), the platform wakes the app, and `update` clears it:
//!
//! ```ignore
//! Msg::Show(text) => { m.toast = Some(text); Cmd::SetTimer { handle: TOAST, ms: 2500 } }
//! Msg::ToastDone  => { m.toast = None;       Cmd::None }
//! ```
//!
//! The alternative was a `phase` the widget counts down, like [`Marquee`](super::Marquee)'s. It was
//! rejected for a specific failure: a phase only advances while *something else* is advancing it, so
//! on a still screen with no timer running the toast would never leave. A message that stays for ever
//! is the worst way for this to break, because it looks like a working feature.
//!
//! It also keeps the rule the whole crate runs on — a widget cannot reach the platform, and `Cmd` is
//! the one channel for asking it to do something. `update` stays testable because it returns a value
//! rather than calling a clock.
//!
//! The cost is honest: every app that wants a toast writes those two lines. The gallery already writes
//! the same shape for the marquee's tick, so it is glue this SDK has decided to have.
//!
//! # It never takes a key
//!
//! Not even to dismiss itself. On this handset the softkey bar owns `Select` unconditionally — see
//! [`Screen`](super::Screen) — so a notice that tried to claim it would be claiming a key it can never
//! receive, which is [`crate::keys`]'s whole subject. A screen that wants "press Back to dismiss"
//! binds Back in its own softkeys and clears the model. The notice is a thing that is *shown*.

use alloc::string::String;

use symbian_gfx::{Align, Canvas, Rect, Size};
use symbian_ui::{icon, paint, Theme};

use crate::constraints::Constraints;
use crate::widget::{hash_i32, hash_str, Widget, WidgetHash};

/// How loud a notice is, which decides its colours.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Tone {
    /// Something happened and it went fine. The accent.
    #[default]
    Plain,
    /// Something wants a look before it is acted on. The palette's `warn`, which exists precisely
    /// because it is not derivable from the accent — see its doc comment on `Palette`.
    Warn,
}

/// A line or two at the top of the screen.
pub struct Notice {
    text: String,
    /// A second line, for the shape KaiOS calls an in-app notice: a headline and a detail.
    detail: Option<String>,
    tone: Tone,
    /// Whether to draw the tone's glyph to the left of the text.
    glyph: bool,
}

impl Notice {
    /// A notice that stays until the screen takes it away.
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into(), detail: None, tone: Tone::Plain, glyph: true }
    }

    /// A toast: identical, and named differently so the call site says which one it is.
    ///
    /// There is no behavioural difference here — the difference is that `update` armed a timer. Naming
    /// it is the only way a reader of `view` can tell, and a reader of `view` is who has to know.
    pub fn toast(text: impl Into<String>) -> Self {
        Self::new(text)
    }

    /// A second, quieter line under the first.
    pub fn detail(mut self, text: impl Into<String>) -> Self {
        self.detail = Some(text.into());
        self
    }

    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }

    /// Draw it without the leading glyph.
    pub fn no_glyph(mut self) -> Self {
        self.glyph = false;
        self
    }

    /// How tall this notice is, so a screen can reserve the band before building it.
    ///
    /// Public because a notice is placed by whatever is stacking it, and a caller guessing this number
    /// is a caller whose band and whose notice disagree by a pixel or two — which reads as the notice
    /// being slightly cut off rather than as an arithmetic error.
    pub fn height(&self, theme: &Theme<'_>) -> i32 {
        let lines = if self.detail.is_some() { 2 } else { 1 };
        theme.fonts.body.line_height() * lines + theme.metrics.space.snug * 2
    }

    /// The band's fill and its text, for this tone.
    fn colors(&self, theme: &Theme<'_>) -> (symbian_gfx::Color, symbian_gfx::Color) {
        let p = &theme.palette;
        match self.tone {
            Tone::Plain => (p.accent, p.accent_text),
            Tone::Warn => (p.warn, p.warn_text),
        }
    }
}

impl Widget for Notice {
    fn content_hash(&self) -> WidgetHash {
        // The text and whether there is a second line, because both change the height. Not the tone:
        // a warning and a plain notice are the same box in different colours, and folding colour in
        // would re-measure a band to recolour it.
        let h = hash_str(0, &self.text);
        let h = hash_i32(h, self.detail.is_some() as i32);
        hash_i32(h, self.glyph as i32)
    }

    fn measure(&self, constraints: Constraints, theme: &Theme<'_>) -> Size {
        // As wide as offered: a band that shrank to its text would be a stripe ending mid-screen, the
        // same reason `SectionHeader` fills its width.
        constraints.constrain(Size::new(constraints.max_w, self.height(theme)))
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        let (fill, ink) = self.colors(theme);
        // Its own height, centred in whatever it was given — the `CrossAlign::Stretch` trap every
        // control in this catalogue shares. A notice drawn into a stretched band would be a slab.
        let h = self.height(theme).min(rect.height());
        let band = Rect::from_xywh(rect.x0, rect.y0 + (rect.height() - h) / 2, rect.width(), h);
        paint::band(c, band, &symbian_ui::Surface::flat(fill));

        let pad = theme.metrics.space.base;
        let mut text = Rect { x0: band.x0 + pad, x1: band.x1 - pad, ..band };

        if self.glyph {
            let size = theme.metrics.icon_md;
            let g = match self.tone {
                Tone::Plain => icon::Icon::Check,
                Tone::Warn => icon::Icon::Warning,
            };
            let w = icon::width_for(g, size);
            let at = Rect::from_xywh(text.x0, band.y0 + (band.height() - size) / 2, w, size);
            icon::draw(c, at, g, ink);
            text.x0 = at.x1 + theme.metrics.space.snug;
        }

        let lh = theme.fonts.body.line_height();
        match &self.detail {
            None => {
                c.draw_text_in(text, &self.text, theme.fonts.body, ink, Align::Start);
            }
            Some(detail) => {
                let head = Rect { y0: band.y0 + theme.metrics.space.snug, y1: band.y0 + theme.metrics.space.snug + lh, ..text };
                let rest = Rect { y0: head.y1, y1: head.y1 + lh, ..text };
                c.draw_text_in(head, &self.text, theme.fonts.strong, ink, Align::Start);
                c.draw_text_in(rest, detail, theme.fonts.small, ink, Align::Start);
            }
        }
    }

    // No `handle_key`. The default is `Ignored`, and that is the whole behaviour — see the module
    // docs on why a notice cannot dismiss itself on this platform.
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian_ui::{testing, Handled, Key, KeyEvent, Palette};

    const BAND: Rect = Rect { x0: 0, y0: 0, x1: 320, y1: 60 };

    fn paint_it(n: &Notice) -> Vec<u16> {
        let (_, buf) = testing::with_canvas(Size::new(320, 60), |c| {
            testing::with_theme(Palette::DARK, |t| {
                c.clear(Palette::DARK.bg.mid());
                n.draw(c, BAND, t);
            });
        });
        buf
    }

    #[test]
    fn a_second_line_makes_it_taller_and_nothing_else_does() {
        testing::with_theme(Palette::DARK, |t| {
            let one = Notice::new("a").height(t);
            let two = Notice::new("a").detail("a").height(t);
            assert!(two > one, "a detail line has to be somewhere");
            assert_eq!(Notice::new("a").tone(Tone::Warn).height(t), one, "a tone is not a size");
            assert_eq!(Notice::new("a").no_glyph().height(t), one, "nor is a glyph");
        });
    }

    #[test]
    fn the_published_height_is_the_height_it_measures() {
        // A screen reserves the band from `height` and the widget fills it. A divergence reads as the
        // notice being slightly cut off rather than as arithmetic.
        testing::with_theme(Palette::DARK, |t| {
            for n in [Notice::new("a"), Notice::new("a").detail("a"), Notice::new("a").no_glyph()] {
                assert_eq!(n.measure(Constraints::loose(320, 240), t).h, n.height(t));
            }
        });
    }

    #[test]
    fn a_stretched_band_does_not_make_a_slab() {
        // The trap every control here shares: handed 60 pixels, a notice must draw its own height and
        // centre it, not fill what it was given.
        let buf = paint_it(&Notice::new("a"));
        let bg = Palette::DARK.bg.mid().to_rgb565().0;
        let rows: Vec<i32> =
            (0..60).filter(|&y| (0..320).any(|x| buf[(y * 320 + x) as usize] != bg)).collect();
        let h = testing::with_theme(Palette::DARK, |t| Notice::new("a").height(t));
        assert_eq!(rows.len() as i32, h, "the band is its own height, not the offer's");
        assert_eq!(rows[0], (60 - h) / 2, "and centred in it");
    }

    #[test]
    fn the_two_tones_are_different_bands_in_every_palette() {
        for (name, palette) in Palette::ALL {
            let paint = |tone| {
                let (_, buf) = testing::with_canvas(Size::new(320, 60), |c| {
                    testing::with_theme(palette, |t| {
                        c.clear(palette.bg.mid());
                        Notice::new("a").tone(tone).draw(c, BAND, t);
                    });
                });
                buf
            };
            assert_ne!(paint(Tone::Plain), paint(Tone::Warn), "{name}");
        }
    }

    #[test]
    fn a_notice_never_takes_a_key_not_even_the_one_that_would_dismiss_it() {
        // The softkey bar owns `Select` unconditionally, so a notice that claimed it would be claiming
        // a key it can never receive. A screen dismisses this from its own softkeys.
        testing::with_theme(Palette::DARK, |_t| {
            crate::widget::with_key_ctx(|cx| {
                let n = Notice::new("a");
                for key in [Key::Select, Key::Up, Key::Down, Key::Backspace, Key::End] {
                    assert_eq!(n.handle_key(KeyEvent::new(key), BAND, cx), Handled::Ignored, "{key:?}");
                }
            });
        });
    }

    #[test]
    fn a_toast_and_a_notice_are_the_same_widget() {
        // The difference is that `update` armed a timer, which is not something this file can see.
        // Asserting it stops a future edit from quietly making them diverge and leaving the docs wrong.
        assert_eq!(Notice::toast("a").content_hash(), Notice::new("a").content_hash());
        assert_eq!(paint_it(&Notice::toast("a")), paint_it(&Notice::new("a")));
    }

    #[test]
    fn the_digest_moves_with_the_shape_and_not_with_the_colour() {
        let a = Notice::new("a");
        assert_ne!(a.content_hash(), Notice::new("aa").content_hash(), "text");
        assert_ne!(a.content_hash(), Notice::new("a").detail("a").content_hash(), "a second line");
        assert_ne!(a.content_hash(), Notice::new("a").no_glyph().content_hash(), "the glyph's room");
        assert_eq!(a.content_hash(), Notice::new("a").tone(Tone::Warn).content_hash(), "tone is colour");
        assert_ne!(a.content_hash(), 0);
    }

    #[test]
    fn it_is_as_wide_as_it_is_offered() {
        testing::with_theme(Palette::DARK, |t| {
            assert_eq!(Notice::new("a").measure(Constraints::loose(120, 240), t).w, 120);
        });
    }
}
