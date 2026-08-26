//! A single line of text that slides when it is too long, instead of being cut.
//!
//! # Why not just widen [`Text`](super::Text)
//!
//! Because the two answer the same question differently and both answers are right somewhere.
//! `Text` truncates with an ellipsis, which is correct for every label the user is not looking at:
//! it is stable, it costs one measurement, and a screen full of sliding text is unreadable.
//!
//! A marquee is for the *one* line that has the cursor. A user who has selected a row and still
//! cannot read its name has no way to find out what it says — truncation has turned the label into a
//! puzzle. So this is a widget you reach for on the focused row and nowhere else, and
//! [`focused`](Marquee::focused) defaults to `false` so a row built once and used for every index
//! behaves like `Text` until it is the one being looked at.
//!
//! # The phase comes from the model
//!
//! No timer here, and no interior mutability holding a frame counter. The offset is a pure function
//! of the phase — [`symbian_ui::marquee::offset`] — and the phase is a number the app increments in
//! `update` when its tick arrives:
//!
//! ```ignore
//! // in update
//! Msg::Tick => { model.phase = model.phase.wrapping_add(1); Cmd::SetTimer { handle: TICK, ms: 250 } }
//! // in view
//! Marquee::new(&chat.name).focused(i == model.selected).phase(model.phase)
//! ```
//!
//! This is the same shape [`Meter::Busy`](symbian_ui::Meter) already uses, and it is not only
//! consistency: a phase in the model is a phase a test can set, so "the label at frame 37" is a
//! value rather than a stopwatch. It also keeps the animation honest about its cost — a screen that
//! wants sliding text has to ask for a timer, out loud, in `update`.
//!
//! # Why the offset is not in the slot table
//!
//! A scroll offset lives in a slot because it is derived from a viewport height the model cannot
//! know. This is the opposite case: the *travel* depends on the box, but the **clock** does not, and
//! a slot cannot advance itself. Something has to tick, that something is `update`, and once the
//! number is there the slot would only be a second copy of it.

use alloc::string::String;

use symbian_gfx::{Canvas, Point, Rect, Size};
use symbian_ui::marquee::{self, Pace};
use symbian_ui::Theme;

use crate::constraints::Constraints;
use crate::theme::FontRole;
use crate::widget::{hash_i32, hash_str, Widget, WidgetHash};
use crate::widgets::Ink;

/// One line of text that slides left and back when it does not fit.
pub struct Marquee {
    text: String,
    role: FontRole,
    ink: Ink,
    /// Whether this line is the one with the cursor. Off by default — see the module docs.
    focused: bool,
    /// The app's tick. Only read when focused.
    phase: u32,
    pace: Pace,
    flex: i32,
}

impl Marquee {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            role: FontRole::Body,
            ink: Ink::Text,
            focused: false,
            phase: 0,
            pace: Pace::DEFAULT,
            flex: 0,
        }
    }

    pub fn font(mut self, role: FontRole) -> Self {
        self.role = role;
        self
    }

    pub fn ink(mut self, ink: Ink) -> Self {
        self.ink = ink;
        self
    }

    /// Whether this is the line being looked at. A marquee that is not focused draws exactly what
    /// [`Text`](super::Text) would.
    pub fn focused(mut self, on: bool) -> Self {
        self.focused = on;
        self
    }

    /// The app's tick counter. Wrapping is fine and expected — the cycle is a modulus.
    pub fn phase(mut self, phase: u32) -> Self {
        self.phase = phase;
        self
    }

    /// How fast it slides and how long it holds at each end.
    pub fn pace(mut self, pace: Pace) -> Self {
        self.pace = pace;
        self
    }

    /// This widget's share of leftover space in its parent. Almost always `1` — a marquee that
    /// wrapped to its own text would never be narrower than it, so nothing would ever slide.
    pub fn flex(mut self, weight: i32) -> Self {
        self.flex = weight.max(0);
        self
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

impl Widget for Marquee {
    /// # Why the phase is not in the digest
    ///
    /// It moves every tick and it never changes the size: the box is the box, and what slides is the
    /// ink inside it. A digest that folded the phase in would re-measure the row on every frame of
    /// the animation — turning a widget that exists to be readable into the one that makes a still
    /// screen expensive.
    ///
    /// Focus *is* in it, because a focused marquee and an unfocused one are the same size today and
    /// need not stay that way: a theme that gave the focused line a bolder font would change the
    /// measurement, and a digest that ignored focus would keep the lighter one.
    fn content_hash(&self) -> WidgetHash {
        let h = hash_str(0, &self.text);
        let h = hash_i32(h, self.role as u8 as i32);
        hash_i32(h, self.focused as i32)
    }

    fn measure(&self, constraints: Constraints, theme: &Theme<'_>) -> Size {
        let font = self.role.font(theme);
        // The *intrinsic* width — what the text wants — clamped by the offer. A marquee that
        // reported its own full width would make the row it sits in as wide as its longest label,
        // and the row would be the thing that overflowed instead.
        let w = font.measure(&self.text);
        constraints.constrain(Size::new(w, font.line_height()))
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        let font = self.role.font(theme);
        let colour = self.ink.resolve(theme);
        let text_w = font.measure(&self.text);

        if !self.focused || !marquee::scrolls(text_w, rect.width()) {
            // Nothing to slide, or nobody looking: this is `Text`'s job and `draw_text_in` already
            // ellipsises. Going through the same call is what keeps an unfocused marquee
            // pixel-identical to the label it replaced.
            c.draw_text_in(rect, &self.text, font, colour, symbian_gfx::Align::Start);
            return;
        }

        let dx = marquee::offset(text_w, rect.width(), self.phase, self.pace);
        // Clipped to its own rect, and this is the load-bearing line: the text is drawn from a
        // negative x, so without the clip the overhang paints across whatever is to the left of it —
        // an avatar, a checkbox, the screen edge. `draw_text` does not ellipsise and does not stop.
        c.with(rect, |c| {
            c.draw_text(Point::new(-dx, font.ascent()), &self.text, font, colour);
        });
    }

    fn flex_weight(&self) -> i32 {
        self.flex
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian_gfx::Align;
    use symbian_ui::{testing, Palette};

    /// Draw `m` into a box `w` wide and report the columns that got ink.
    fn inked(m: &Marquee, w: i32) -> Vec<i32> {
        let h = 20;
        let (_, buf) = testing::with_canvas(Size::new(w + 40, h), |c| {
            testing::with_theme(Palette::DARK, |t| {
                c.clear(Palette::DARK.bg.mid());
                // Offset by 20 so ink escaping to the left of the box lands somewhere visible rather
                // than off the canvas — the whole point of the clip test below.
                m.draw(c, Rect::from_xywh(20, 0, w, h), t);
            });
        });
        let bg = Palette::DARK.bg.mid().to_rgb565().0;
        (0..w + 40)
            .filter(|x| (0..h).any(|y| buf[(y * (w + 40) + x) as usize] != bg))
            .collect()
    }

    #[test]
    fn an_unfocused_marquee_draws_what_a_label_would() {
        // The property that lets a row builder use one for every index: until it has the cursor it is
        // a `Text`, ellipsis and all.
        testing::with_theme(Palette::DARK, |t| {
            let long = "a name far too long for any of this";
            let (_, mine) = testing::with_canvas(Size::new(80, 20), |c| {
                c.clear(Palette::DARK.bg.mid());
                Marquee::new(long).phase(7).draw(c, Rect::from_xywh(0, 0, 80, 20), t);
            });
            let (_, theirs) = testing::with_canvas(Size::new(80, 20), |c| {
                c.clear(Palette::DARK.bg.mid());
                c.draw_text_in(
                    Rect::from_xywh(0, 0, 80, 20),
                    long,
                    t.fonts.body,
                    Palette::DARK.text,
                    Align::Start,
                );
            });
            assert_eq!(mine, theirs, "an unfocused marquee is a label, pixel for pixel");
        });
    }

    #[test]
    fn text_that_fits_does_not_move_even_when_focused() {
        // Otherwise every short label on the focused row would jitter, which is worse than the
        // problem this widget exists to solve.
        let a = inked(&Marquee::new("hi").focused(true).phase(0), 200);
        let b = inked(&Marquee::new("hi").focused(true).phase(9), 200);
        assert_eq!(a, b);
    }

    #[test]
    fn a_focused_marquee_moves_between_phases() {
        let long = "a name far too long for any of this";
        let pace = Pace { pause: 0, step: 4 };
        let a = inked(&Marquee::new(long).focused(true).pace(pace).phase(0), 60);
        let b = inked(&Marquee::new(long).focused(true).pace(pace).phase(3), 60);
        assert_ne!(a, b, "three ticks at four pixels should have shifted the ink");
    }

    #[test]
    fn the_overhang_never_paints_outside_the_box() {
        // The defect this crate has already met once, arriving from the other side: the text is drawn
        // from a negative x, and `draw_text` neither ellipsises nor stops. Without the clip the
        // overhang lands on whatever is to the left — in a list row, the avatar.
        let long = "a name far too long for any of this to fit at all";
        let pace = Pace { pause: 0, step: 4 };
        for phase in 0..12 {
            let cols = inked(&Marquee::new(long).focused(true).pace(pace).phase(phase), 60);
            assert!(
                cols.iter().all(|x| (20..80).contains(x)),
                "phase {phase} painted outside 20..80: {cols:?}"
            );
        }
    }

    #[test]
    fn the_phase_is_not_in_the_digest() {
        // If it were, the row would re-measure on every frame of the animation — which is the one
        // thing a widget that exists to be read must not cost.
        let a = Marquee::new("something long").focused(true).phase(0);
        let b = Marquee::new("something long").focused(true).phase(99);
        assert_eq!(a.content_hash(), b.content_hash());
    }

    #[test]
    fn focus_and_the_text_are_in_the_digest() {
        assert_ne!(
            Marquee::new("x").focused(true).content_hash(),
            Marquee::new("x").focused(false).content_hash()
        );
        assert_ne!(Marquee::new("x").content_hash(), Marquee::new("y").content_hash());
        assert_ne!(Marquee::new("x").content_hash(), 0);
    }

    #[test]
    fn it_measures_what_the_text_wants_within_the_offer() {
        testing::with_theme(Palette::DARK, |t| {
            let m = Marquee::new("short");
            let want = t.fonts.body.measure("short");
            assert_eq!(m.measure(Constraints::loose(320, 240), t), Size::new(want, t.fonts.body.line_height()));
            // And it never asks for more than it was offered, or the row it is in overflows instead.
            let long = Marquee::new("a name far too long for any of this");
            assert_eq!(long.measure(Constraints::loose(40, 240), t).w, 40);
        });
    }
}
