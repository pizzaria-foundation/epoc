//! A labelled on/off switch — the affordance a settings row needs for a boolean. Select flips it;
//! it draws a caption on the left and a pill-and-knob switch on the right, filled in the accent
//! colour when on. The caller supplies the label (the crate ships no text) and whether the row is
//! focused, so the row can carry the selection highlight like any other list row.

use symbian_gfx::{Align, Canvas, Rect};

use crate::input::{Handled, Key, KeyEvent};
use crate::theme::Theme;

/// How wide a switch is, in pixels.
///
/// A constant rather than a metric because it is a *shape*, not a spacing: the track has to be about
/// twice its own height for the knob's travel to read as travel, and a theme that made it 20 would
/// have made it a circle with a bulge. If a theme ever needs to scale it, it scales with the row
/// height through [`switch_height`] rather than by picking a new number here.
pub const SWITCH_W: i32 = 30;

/// How tall a switch is inside a band `band_h` pixels high.
///
/// Derived from the band rather than fixed, so a switch in a 38-pixel list row and one in a short
/// dialog line are both proportionate — and clamped at both ends, because below ten pixels the knob
/// and the track are the same object and above eighteen it stops reading as a switch and starts
/// reading as a bar.
pub fn switch_height(band_h: i32, theme: &Theme<'_>) -> i32 {
    (band_h - theme.metrics.pad * 2).clamp(10, 18)
}

/// Where the switch sits inside `band`: against its right edge, centred across it.
///
/// Extracted so the two callers cannot disagree. [`Toggle::draw`] paints a whole settings row and
/// `symbian_decl_ui`'s `Switch` paints only the switch, and before this the geometry lived inside the
/// first one — so the second would have been a second implementation of the same rounding, agreeing
/// on the day it was written.
pub fn switch_track(band: Rect, theme: &Theme<'_>) -> Rect {
    let h = switch_height(band.height(), theme);
    Rect::from_xywh(band.x1 - SWITCH_W, band.y0 + (band.height() - h) / 2, SWITCH_W, h)
}

/// Paint the track and the knob into exactly `track`.
///
/// Takes the track rather than the band, so a caller that has already reserved room — a row that put
/// a label to the left of it — passes what it reserved instead of trusting this to reach the same
/// answer twice.
pub fn draw_switch(c: &mut Canvas<'_>, track: Rect, theme: &Theme<'_>, on: bool, selected: bool) {
    // `selected` and not the palette directly: on the selection band the page's `accent`, `dim` and
    // `bg` are three colours chosen for a ground that is not there. See `chrome::control_colors`,
    // which is where that argument lives and which four controls share so they cannot disagree.
    let (ground, ink, quiet) = crate::chrome::control_colors(theme, selected);
    let h = track.height();
    c.fill_round_rect(track, h / 2, if on { ink } else { quiet });
    // The knob inset two pixels on every side, so the track shows as a ring around it. `max(2)`
    // because a track clamped to its own floor would otherwise give a knob of zero.
    let d = (h - 4).max(2);
    let x = if on { track.x1 - d - 2 } else { track.x0 + 2 };
    c.fill_round_rect(Rect::from_xywh(x, track.y0 + 2, d, d), d / 2, ground);
}

/// An on/off switch. Owns only the boolean.
#[derive(Copy, Clone, Debug, Default)]
pub struct Toggle {
    on: bool,
}

impl Toggle {
    pub const fn new(on: bool) -> Self {
        Self { on }
    }

    pub fn on(&self) -> bool {
        self.on
    }

    pub fn set(&mut self, on: bool) {
        self.on = on;
    }

    /// Select flips it. Everything else is `Ignored` so the screen keeps its navigation.
    pub fn handle_key(&mut self, ev: KeyEvent) -> Handled {
        match ev.key {
            Key::Select => {
                self.on = !self.on;
                Handled::Consumed
            }
            _ => Handled::Ignored,
        }
    }

    /// Draw the row: `label` on the left, the switch on the right. `focused` carries the full-bleed
    /// selection band so the row reads as the cursor's position.
    pub fn draw(&self, c: &mut Canvas<'_>, r: Rect, theme: &Theme<'_>, label: &str, focused: bool) {
        let p = &theme.palette;
        if focused {
            crate::chrome::selection(c, r, theme);
        }
        let inner = r.inset_xy(theme.metrics.pad, 0);
        // The geometry and the ink both come from the free functions above, so the declarative
        // `Switch` cannot draw a different switch from this one.
        let track = switch_track(inner, theme);
        let text_color = if focused { p.selection_text } else { p.text };
        // Label gets everything to the left of the switch, with a gap.
        let label_area = Rect { x1: track.x0 - theme.metrics.pad, ..inner };
        c.draw_text_in(label_area, label, theme.fonts.body, text_color, Align::Start);
        draw_switch(c, track, theme, self.on, focused);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;
    use crate::theme::Palette;

    fn ev(key: Key) -> KeyEvent {
        KeyEvent::new(key)
    }

    #[test]
    fn select_flips_and_reports_consumed() {
        let mut t = Toggle::new(false);
        assert_eq!(t.handle_key(ev(Key::Select)), Handled::Consumed);
        assert!(t.on());
        t.handle_key(ev(Key::Select));
        assert!(!t.on());
    }

    #[test]
    fn other_keys_pass_through() {
        let mut t = Toggle::new(false);
        assert_eq!(t.handle_key(ev(Key::Up)), Handled::Ignored);
        assert!(!t.on());
    }

    #[test]
    fn the_extracted_geometry_is_the_geometry_this_row_always_had() {
        // Pinned to the numbers rather than to the expression, so a refactor of `switch_track` that
        // shifted the switch by a pixel fails here instead of in a screenshot. 38-pixel row, 5-pixel
        // pad: a 30x18 track against the right edge, centred.
        testing::with_theme(Palette::DARK, |t| {
            let row = symbian_gfx::Rect::from_xywh(0, 0, 320, 38);
            let inner = row.inset_xy(t.metrics.pad, 0);
            let track = switch_track(inner, t);
            assert_eq!(track.width(), SWITCH_W);
            assert_eq!(track.height(), 18);
            assert_eq!(track.x1, 315, "against the right edge, inside the row's pad");
            assert_eq!(track.y0, 10, "centred in the row");
        });
    }

    #[test]
    fn a_short_band_gets_a_shorter_switch_and_never_a_knobless_one() {
        testing::with_theme(Palette::DARK, |t| {
            assert_eq!(switch_height(38, t), 18, "clamped at the top");
            assert_eq!(switch_height(12, t), 10, "and at the bottom");
            // A track at its floor still has a knob: `(10 - 4).max(2)` is 6, not 0.
            let track = switch_track(symbian_gfx::Rect::from_xywh(0, 0, 60, 12), t);
            assert_eq!(track.height(), 10);
        });
    }

    #[test]
    fn draws_on_and_off_in_every_palette() {
        for (_, palette) in Palette::ALL {
            for on in [false, true] {
                let t = Toggle::new(on);
                let (_, px) = testing::with_canvas(symbian_gfx::Size::new(320, 40), |c| {
                    testing::with_theme(palette, |th| {
                        t.draw(c, symbian_gfx::Rect::from_xywh(0, 0, 320, 38), th, "Replace Main", on);
                    });
                });
                assert!(px.iter().any(|&p| p != 0));
            }
        }
    }
}
