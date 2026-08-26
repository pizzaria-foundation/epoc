//! A labelled numeric stepper for a settings row — the integer counterpart of [`crate::Toggle`].
//! It owns a value bounded to `[min, max]`. Select cycles it up by one and wraps past the top back
//! to the bottom (so it works where Left/Right are unavailable — e.g. a tabbed screen whose tab
//! strip already consumes Left/Right); Left/Right also step it down/up when they do reach the
//! widget. Draws the caption on the left and `‹ N ›` on the right.
//!
//! The right-hand block's geometry and ink are free functions — [`stepper_box`] and
//! [`draw_stepper`] — for the reason [`crate::switch_track`] is one: `symbian_decl_ui`'s `Stepper`
//! draws only that block, and a second implementation of the same rounding would agree with this
//! one for exactly as long as nobody touched either.

use symbian_gfx::{Align, Canvas, Rect};

use crate::input::{Handled, Key, KeyEvent};
use crate::theme::Theme;

/// How wide the `‹ N ›` block is, in pixels.
///
/// Fixed rather than measured, and that is the whole reason the label area is stable: a block sized
/// to its own digits moves the caption sideways when the count crosses ten, and a settings screen
/// where every row's label sits at a different x is the defect this number prevents. Wide enough for
/// three digits and the two chevrons in the 12px body face.
pub const STEPPER_W: i32 = 46;

/// How tall the `‹ N ›` block is inside a band `band_h` pixels high.
///
/// One line of body text, never the band — the counterpart of [`crate::switch_height`], and needed
/// for the same reason: `symbian_decl_ui`'s `Stepper` is placed by a list row with
/// `CrossAlign::Stretch`, which hands it the whole 38-pixel band. A block that took the band would
/// still *look* right here, because [`Canvas::draw_text_in`](symbian_gfx::Canvas::draw_text_in)
/// centres within whatever it is given — and would be a lie to every caller that asked this widget
/// how big it is.
///
/// Clamped down to the band so a stepper in a band shorter than a line still draws where it drew
/// before this function existed.
pub fn stepper_height(band_h: i32, theme: &Theme<'_>) -> i32 {
    theme.fonts.body.line_height().min(band_h.max(0))
}

/// Where the `‹ N ›` block sits inside `band`: against its right edge, centred across it.
///
/// Extracted so the two callers cannot disagree, exactly as [`crate::switch_track`] was.
/// [`Stepper::draw`] paints a whole settings row and `symbian_decl_ui`'s `Stepper` paints only the
/// block; before this the arithmetic lived inside the first one, so the second would have been a
/// second copy of it that agreed on the day it was written.
pub fn stepper_box(band: Rect, theme: &Theme<'_>) -> Rect {
    let h = stepper_height(band.height(), theme);
    Rect::from_xywh(band.x1 - STEPPER_W, band.y0 + (band.height() - h) / 2, STEPPER_W, h)
}

/// Paint `‹ N ›` into exactly `slot`, in the accent colour — or the selection colour when the row
/// under it is carrying the selection band.
///
/// Takes the slot rather than the band, so a caller that has already reserved room for a label to
/// the left of it passes what it reserved instead of trusting this to reach the same answer twice.
pub fn draw_stepper(c: &mut Canvas<'_>, slot: Rect, theme: &Theme<'_>, value: i32, focused: bool) {
    let mut buf = [0u8; 16];
    let s = fmt_stepper(value, &mut buf);
    draw_chevrons(c, slot, theme, s, focused);
}

/// Paint `‹ word ›` into exactly `slot`, for a spinner whose value is not a number.
///
/// A month spinner is the case: `‹ 2 ›` is a month only to whoever wrote the code, and this crate
/// ships no text of its own, so the *caller* supplies the names and this draws whichever one the
/// value selects. Split out rather than reimplemented in the widget that needed it, for the reason
/// [`stepper_box`] was: a second copy of the chevrons would agree with this one on the day it was
/// written and drift on the day either changed.
///
/// The ink and the alignment are [`draw_stepper`]'s, unchanged and shared — a labelled field beside
/// a numeric one has to sit on the same baseline in the same colour, and two functions that each
/// chose would eventually choose differently.
pub fn draw_chevrons(c: &mut Canvas<'_>, slot: Rect, theme: &Theme<'_>, text: &str, focused: bool) {
    let mut buf = [0u8; CHEVRON_BUF];
    // Through the shared helper rather than the two-branch conditional this used to hold. It already
    // picked the right two colours — it was the only one of the four controls that did — and routing
    // it here means there is one definition of "what goes on the band" rather than one correct
    // instance and three that were not.
    let (_, ink, _) = crate::chrome::control_colors(theme, focused);
    c.draw_text_in(slot, wrap_chevrons(text, &mut buf), theme.fonts.body, ink, Align::Center);
}

/// How wide a `‹ word ›` block has to be to hold the widest of `words` without moving.
///
/// The **widest**, not the current one, and that is the whole reason this is a function rather than
/// a measurement at draw time: a block sized to the word it is showing moves its neighbours every
/// time the value steps, so a date picker's year field would shuffle sideways between May and
/// September. It is [`STEPPER_W`]'s argument applied to text, and the answer is only stable if it
/// does not depend on the value.
pub fn chevron_width<'a>(theme: &Theme<'_>, words: impl IntoIterator<Item = &'a str>) -> i32 {
    let f = theme.fonts.body;
    // `"<  >"` is the chrome exactly: the two chevrons and the two spaces `wrap_chevrons` inserts.
    f.measure("<  >") + words.into_iter().map(|w| f.measure(w)).max().unwrap_or(0)
}

/// How long a word `draw_chevrons` will render before it starts cutting.
///
/// Bounded because this is a draw path on a phone with a shim allocator we measure, and a heap
/// allocation per field per frame to print a month name is not worth it. Forty bytes is several
/// times the longest month name in any language this device has a font for, chevrons included.
const CHEVRON_BUF: usize = 40;

/// `"< "` + as much of `text` as fits + `" >"`, never splitting a character.
///
/// Cut rather than ellipsised, and cut on a char boundary rather than a byte: a `&str` sliced
/// mid-character panics, and a panic in a draw pass is a dead screen on a device whose whole failure
/// report is a dialog with a number in it. In practice nothing is cut — [`chevron_width`] sizes the
/// block from the longest word — so this is the guard for the caller that passed a sentence.
fn wrap_chevrons<'a>(text: &str, buf: &'a mut [u8; CHEVRON_BUF]) -> &'a str {
    let mut w = 0;
    for &b in b"< " {
        buf[w] = b;
        w += 1;
    }
    // By characters, so a multi-byte name is either fully copied or stops before it starts.
    for ch in text.chars() {
        let n = ch.len_utf8();
        if w + n + 2 > CHEVRON_BUF {
            break;
        }
        w += ch.encode_utf8(&mut buf[w..]).len();
    }
    for &b in b" >" {
        buf[w] = b;
        w += 1;
    }
    core::str::from_utf8(&buf[..w]).unwrap_or("?")
}

/// A bounded integer picker. Owns the value and its inclusive bounds.
#[derive(Copy, Clone, Debug)]
pub struct Stepper {
    value: i32,
    min: i32,
    max: i32,
}

impl Stepper {
    pub fn new(value: i32, min: i32, max: i32) -> Self {
        let (min, max) = if min <= max { (min, max) } else { (max, min) };
        Self { value: value.clamp(min, max), min, max }
    }

    pub fn value(&self) -> i32 {
        self.value
    }

    pub fn set(&mut self, value: i32) {
        self.value = value.clamp(self.min, self.max);
    }

    /// Select cycles up with wrap; Left/Right step down/up with clamping. Anything else is
    /// `Ignored` so the surrounding screen keeps its navigation.
    pub fn handle_key(&mut self, ev: KeyEvent) -> Handled {
        match ev.key {
            Key::Select => {
                self.value = if self.value >= self.max { self.min } else { self.value + 1 };
                Handled::Consumed
            }
            Key::Right => {
                if self.value < self.max {
                    self.value += 1;
                }
                Handled::Consumed
            }
            Key::Left => {
                if self.value > self.min {
                    self.value -= 1;
                }
                Handled::Consumed
            }
            _ => Handled::Ignored,
        }
    }

    /// Draw the row: `label` left, `‹ N ›` right; the value takes the accent colour when focused.
    pub fn draw(&self, c: &mut Canvas<'_>, r: Rect, theme: &Theme<'_>, label: &str, focused: bool) {
        let p = &theme.palette;
        if focused {
            crate::chrome::selection(c, r, theme);
        }
        let inner = r.inset_xy(theme.metrics.pad, 0);
        let text_color = if focused { p.selection_text } else { p.text };

        // The geometry and the ink both come from the free functions above, so the declarative
        // `Stepper` cannot draw a different block from this one.
        let val_area = stepper_box(inner, theme);
        draw_stepper(c, val_area, theme, self.value, focused);

        let label_area = Rect { x1: val_area.x0 - theme.metrics.pad, ..inner };
        c.draw_text_in(label_area, label, theme.fonts.body, text_color, Align::Start);
    }
}

/// Format the digits of `n` into a small stack buffer (no alloc). N is a small non-negative count
/// here, and the chevrons around it are [`draw_chevrons`]' — so a numeric field and a labelled one
/// cannot end up with two different pairs of brackets.
fn fmt_stepper(n: i32, buf: &mut [u8; 16]) -> &str {
    let mut tmp = [0u8; 8];
    let mut i = tmp.len();
    let mut v = n.max(0) as u32;
    loop {
        i -= 1;
        tmp[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    let digits = &tmp[i..];
    buf[..digits.len()].copy_from_slice(digits);
    core::str::from_utf8(&buf[..digits.len()]).unwrap_or("?")
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
    fn select_cycles_with_wrap() {
        let mut s = Stepper::new(2, 2, 4);
        s.handle_key(ev(Key::Select)); // 3
        assert_eq!(s.value(), 3);
        s.handle_key(ev(Key::Select)); // 4
        s.handle_key(ev(Key::Select)); // wrap to 2
        assert_eq!(s.value(), 2);
    }

    #[test]
    fn left_right_clamp() {
        let mut s = Stepper::new(2, 2, 4);
        assert_eq!(s.handle_key(ev(Key::Left)), Handled::Consumed);
        assert_eq!(s.value(), 2, "clamped at min");
        s.handle_key(ev(Key::Right));
        s.handle_key(ev(Key::Right));
        s.handle_key(ev(Key::Right));
        assert_eq!(s.value(), 4, "clamped at max");
    }

    #[test]
    fn new_clamps_and_orders_bounds() {
        assert_eq!(Stepper::new(9, 2, 4).value(), 4);
        assert_eq!(Stepper::new(0, 2, 4).value(), 2);
        // reversed bounds are tolerated
        assert_eq!(Stepper::new(3, 4, 2).value(), 3);
    }

    #[test]
    fn boxing_the_value_block_did_not_move_the_text_it_used_to_draw() {
        // `draw_text_in` centres the em box: `y0 + (height - line_height) / 2 + ascent`. This row
        // used to hand it the full band and now hands it a one-line slot centred in that band, and
        // the two must resolve to the same baseline or the extraction shifted every stepper on
        // every settings screen by a pixel — a change no palette test would have caught.
        testing::with_theme(Palette::DARK, |t| {

            let lh = t.fonts.body.line_height();
            for band_h in 0..48 {
                let band = symbian_gfx::Rect::from_xywh(7, 11, 200, band_h);
                let slot = stepper_box(band, t);
                assert_eq!(
                    slot.y0 + (slot.height() - lh) / 2,
                    band.y0 + (band.height() - lh) / 2,
                    "band {band_h}"
                );
                // And the horizontal half of it, which the label area is derived from.
                assert_eq!((slot.x0, slot.x1), (band.x1 - STEPPER_W, band.x1), "band {band_h}");
            }
        });
    }

    #[test]
    fn the_value_block_is_one_line_tall_and_centred_in_its_band() {
        // What a declarative caller measures. A block that reported the band's height would make a
        // list row hand it 38 pixels and believe it wanted them.
        testing::with_theme(Palette::DARK, |t| {

            let lh = t.fonts.body.line_height();
            let band = symbian_gfx::Rect::from_xywh(0, 0, 200, 38);
            assert!(lh < 38, "the interesting case is a line shorter than the band");
            assert_eq!(stepper_height(38, t), lh);
            assert_eq!(stepper_box(band, t).height(), lh);
            assert_eq!(stepper_box(band, t).y0, (38 - lh) / 2);
            // A band shorter than a line keeps the band, so nothing draws outside what it was given
            // more than it already did.
            assert_eq!(stepper_height(6, t), 6);
            assert_eq!(stepper_height(-4, t), 0);
        });
    }

    #[test]
    fn draws_in_every_palette() {
        for (_, palette) in Palette::ALL {
            let s = Stepper::new(3, 1, 5);
            let (_, px) = testing::with_canvas(symbian_gfx::Size::new(320, 40), |c| {
                testing::with_theme(palette, |th| {
                    s.draw(c, symbian_gfx::Rect::from_xywh(0, 0, 320, 38), th, "Columns", true);
                });
            });
            assert!(px.iter().any(|&p| p != 0));
        }
    }
    #[test]
    fn the_chevrons_are_the_same_pair_for_a_number_and_for_a_word() {
        // The extraction's whole claim. `fmt_stepper` prints digits and nothing else now, so a
        // numeric field and a month name go through one function and cannot end up bracketed
        // differently — which is what a second copy of `"< "` would have produced the first time
        // either was touched.
        let mut buf = [0u8; 16];
        assert_eq!(fmt_stepper(7, &mut buf), "7");
        assert_eq!(fmt_stepper(2026, &mut buf), "2026");
        assert_eq!(fmt_stepper(-4, &mut buf), "0", "a negative count is clamped, not printed");

        let mut buf = [0u8; CHEVRON_BUF];
        assert_eq!(wrap_chevrons("7", &mut buf), "< 7 >");
        assert_eq!(wrap_chevrons("Fev", &mut buf), "< Fev >");
        assert_eq!(wrap_chevrons("", &mut buf), "<  >");
    }

    #[test]
    fn a_word_too_long_for_the_buffer_is_cut_on_a_character_and_not_in_one() {
        // A `&str` sliced mid-character panics, and this runs in a draw pass. The input that gets
        // here is a label out of a caller's locale table, so it can be anything at all.
        let mut buf = [0u8; CHEVRON_BUF];
        let long = "\u{e9}".repeat(CHEVRON_BUF);
        let out = wrap_chevrons(&long, &mut buf);
        assert!(out.starts_with("< ") && out.ends_with(" >"));
        assert!(out.len() <= CHEVRON_BUF);
        // The cut landed between characters: every one that survived is whole.
        assert!(out.chars().all(|c| c == '<' || c == '>' || c == ' ' || c == '\u{e9}'));
    }

    #[test]
    fn a_labelled_block_is_sized_by_the_longest_word_and_not_by_the_current_one() {
        // What stops a picker's year field shuffling sideways between May and September. Sized to
        // the widest name, so the answer does not depend on the value.
        testing::with_theme(Palette::DARK, |t| {
            let months = ["Jan", "September"];
            let w = chevron_width(t, months);
            assert_eq!(w, chevron_width(t, ["September"]), "the widest is what decides");
            assert!(w > chevron_width(t, ["Jan"]));
            assert_eq!(chevron_width(t, core::iter::empty()), t.fonts.body.measure("<  >"));
            // And it really is the width of what gets drawn.
            let mut buf = [0u8; CHEVRON_BUF];
            assert_eq!(w, t.fonts.body.measure(wrap_chevrons("September", &mut buf)));
        });
    }
}
