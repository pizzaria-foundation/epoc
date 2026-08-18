//! A run of text, measured and placed.

use alloc::string::String;

use symbian_gfx::{Align, Canvas, Color, Rect, Size};
use symbian_ui::Theme;

use crate::constraints::Constraints;
use crate::theme::FontRole;
use crate::widget::{hash_bytes, hash_i32, hash_str, Widget, WidgetHash};

/// Where a colour comes from: the palette, or the caller.
///
/// A widget cannot hold a resolved [`Color`] taken from the palette, because it is built before the
/// theme is chosen — see [`crate::theme`]. Naming the palette entry instead is what lets the same
/// tree be drawn light or dark; a widget built with `Text::new(..).dim()` is dim in both, and one
/// built with a literal is exactly that literal in both, which is what a caller who reached for a
/// literal meant.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum Ink {
    /// Ordinary reading text.
    #[default]
    Text,
    /// De-emphasised: timestamps, previews, hints.
    Dim,
    /// The accent, for the one thing on screen that is being offered.
    Accent,
    /// Text on chrome — the title and softkey bars.
    Chrome,
    /// The hairline between rows.
    ///
    /// Not text, but it belongs in the same vocabulary: a colour named by its *role* is resolved
    /// against the theme at draw time, which is what lets a `view` be built without a palette in
    /// hand. That matters more than it sounds — `DeclarativeApp::view` has no theme, deliberately,
    /// so a widget that wanted a literal `Color` could not be built there at all.
    Divider,
    /// Text inside a selected row.
    ///
    /// Its own role rather than `Chrome`, which is the nearest-looking one and is wrong: a
    /// selection band and a title bar are different colours in most palettes, and a row that
    /// borrowed the title's ink would be a shade out under the highlight and nowhere else. The
    /// mistake is easy to make and invisible without a pixel comparison — it was made, and that is
    /// why this variant exists.
    Selection,
    /// A colour the caller insists on. Rare, and usually a sign the palette is missing an entry.
    Fixed(Color),
}

impl Ink {
    pub fn resolve(self, theme: &Theme<'_>) -> Color {
        match self {
            Ink::Text => theme.palette.text,
            Ink::Dim => theme.palette.dim,
            Ink::Accent => theme.palette.accent,
            Ink::Chrome => theme.palette.chrome_text,
            Ink::Selection => theme.palette.selection_text,
            Ink::Divider => theme.palette.divider,
            Ink::Fixed(c) => c,
        }
    }
}

/// Text, in one of the theme's font roles.
///
/// One line by default, truncated with an ellipsis when it does not fit. `max_lines(n)` turns on
/// word wrapping — off unless asked for, because the common case on this screen is a label in a
/// fixed-height row, and a label that silently grew a second line would push everything below it
/// off a 240-pixel screen.
///
/// ```ignore
/// Text::new(&chat.name).font(FontRole::Strong)
/// Text::new(&chat.preview).dim().max_lines(2)
/// Text::new(&chat.time).dim().align(Align::End)
/// ```
#[derive(Clone, Debug)]
pub struct Text {
    text: String,
    role: FontRole,
    ink: Ink,
    align: Align,
    /// At least 1. Zero would be a widget that measures to nothing and draws nothing, which is a
    /// [`Spacer`](crate::widget) written by accident.
    max_lines: usize,
    flex: i32,
}

impl Text {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            role: FontRole::Body,
            ink: Ink::Text,
            align: Align::Start,
            max_lines: 1,
            flex: 0,
        }
    }

    pub fn font(mut self, role: FontRole) -> Self {
        self.role = role;
        self
    }

    pub fn color(mut self, c: Color) -> Self {
        self.ink = Ink::Fixed(c);
        self
    }

    /// The palette's de-emphasised colour. Shorthand for the commonest non-default choice.
    pub fn dim(mut self) -> Self {
        self.ink = Ink::Dim;
        self
    }

    pub fn accent(mut self) -> Self {
        self.ink = Ink::Accent;
        self
    }

    pub fn ink(mut self, ink: Ink) -> Self {
        self.ink = ink;
        self
    }

    pub fn align(mut self, align: Align) -> Self {
        self.align = align;
        self
    }

    /// Wrap to at most `n` lines. `0` is read as `1`.
    pub fn max_lines(mut self, n: usize) -> Self {
        self.max_lines = n.max(1);
        self
    }

    /// This widget's share of leftover space in its parent.
    pub fn flex(mut self, weight: i32) -> Self {
        self.flex = weight.max(0);
        self
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn role(&self) -> FontRole {
        self.role
    }

    /// How many whole lines of this role fit in `height`, capped at `max_lines`.
    ///
    /// Never zero: a band one pixel shorter than a line still shows most of a line, and drawing
    /// nothing there would read as missing content rather than as a cramped layout. The canvas
    /// clips what does not fit.
    fn line_capacity(&self, height: i32, line_h: i32) -> usize {
        if line_h <= 0 {
            return 1;
        }
        ((height / line_h).max(1) as usize).min(self.max_lines)
    }
}

impl Widget for Text {
    /// # Why the whole string is hashed, and the colour is not
    ///
    /// The plan's sketch hashes the length and the first eight bytes. Two chat messages of the same
    /// length that differ after the eighth character then collide, and the second one keeps the
    /// first's measured height — a three-line message drawn in a two-line box, which is exactly the
    /// content this screen is full of. FNV over the whole string is a few hundred cycles for a
    /// screenful of text and cannot do that.
    ///
    /// The colour is deliberately absent: this digest answers "could the *size* have changed", and
    /// including a property that cannot move a pixel of layout would throw away a valid measurement
    /// every time a row went from selected to not.
    ///
    /// What it cannot see is the [`Theme`]. Two different themes with different atlases produce
    /// different widths for the same string and the same hash, so whatever holds these measurements
    /// must be dropped when the theme changes. That is the cache's business, not the widget's, but
    /// nothing in the type says so — see the note in the crate's Phase 2 report.
    fn content_hash(&self) -> WidgetHash {
        let h = hash_str(0, &self.text);
        let h = hash_bytes(h, &[self.role.tag(), self.align as u8]);
        hash_i32(h, self.max_lines as i32)
    }

    fn measure(&self, constraints: Constraints, theme: &Theme<'_>) -> Size {
        let font = self.role.font(theme);
        let line_h = font.line_height();

        if self.max_lines == 1 {
            // A single line is as wide as it wants and one line tall; `constrain` is what stops a
            // long name from returning a width its parent never offered.
            return constraints.constrain(Size::new(font.measure(&self.text), line_h));
        }

        // Wrapping needs a width to wrap to. A zero or negative maximum is not an error here — it
        // is what a parent hands out when padding has eaten everything — so it answers "one line
        // tall, no width" rather than dividing by it.
        let width = constraints.max_w;
        if width <= 0 {
            return constraints.constrain(Size::new(0, line_h));
        }

        let cap = self.max_lines;
        let mut lines = 0usize;
        let mut widest = 0i32;
        font.wrap(&self.text, width, &mut |line| {
            if lines < cap {
                widest = widest.max(font.measure(line));
            }
            lines += 1;
        });
        let shown = lines.clamp(1, cap) as i32;
        constraints.constrain(Size::new(widest, shown * line_h))
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        if rect.is_empty() {
            return;
        }
        let font = self.role.font(theme);
        let ink = self.ink.resolve(theme);

        if self.max_lines == 1 {
            // `draw_text_in` already truncates with the font's ellipsis and centres on the em box.
            c.draw_text_in(rect, &self.text, font, ink, self.align);
            return;
        }

        let line_h = font.line_height();
        let cap = self.line_capacity(rect.height(), line_h);

        // Two passes over the string. `Font::wrap` streams its lines and cannot look ahead, so
        // there is no way to know while drawing line `cap` whether it is the last one — and that is
        // precisely when the ellipsis has to be reserved for. Counting first costs one more walk of
        // a string that is already in cache; guessing costs a paragraph that ends mid-sentence with
        // nothing to say it was cut.
        let mut total = 0usize;
        font.wrap(&self.text, rect.width(), &mut |_| total += 1);

        let mut y = rect.y0;
        let mut n = 0usize;
        font.wrap(&self.text, rect.width(), &mut |line| {
            if n >= cap {
                return;
            }
            let band = Rect { y0: y, y1: (y + line_h).min(rect.y1), ..rect };
            let last_shown = n + 1 == cap && total > cap;
            if !band.is_empty() {
                if last_shown {
                    // Reserve a strip on the right for the ellipsis and draw the line into what is
                    // left. Two calls rather than one string concatenation: this runs every frame a
                    // long message is on screen, and a `String` per frame is the allocation churn
                    // this crate exists to keep off a non-compacting allocator.
                    let ell = font.ellipsis();
                    let ew = font.measure(ell);
                    let (right, left) = band.split_right(ew);
                    if !left.is_empty() {
                        c.draw_text_in(left, line, font, ink, self.align);
                    }
                    c.draw_text_in(right, ell, font, ink, Align::Start);
                } else {
                    c.draw_text_in(band, line, font, ink, self.align);
                }
            }
            y += line_h;
            n += 1;
        });
    }

    fn flex_weight(&self) -> i32 {
        self.flex
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian_ui::{testing, Palette};

    /// Anything drawn on the scratch buffer, since the test atlas inks every glyph solid.
    fn painted(px: &[u16]) -> usize {
        px.iter().filter(|&&p| p != 0).count()
    }

    #[test]
    fn a_line_measures_to_its_font_metrics() {
        testing::with_theme(Palette::DARK, |t| {
            let font = FontRole::Body.font(t);
            let s = Text::new("hello").measure(Constraints::unbounded(), t);
            assert_eq!(s.w, font.measure("hello"));
            assert_eq!(s.h, font.line_height());
            // The test atlas charges every character the same, so width is strictly proportional.
            let long = Text::new("hellohello").measure(Constraints::unbounded(), t);
            assert_eq!(long.w, s.w * 2);
        });
    }

    #[test]
    fn an_empty_string_still_occupies_a_line() {
        // A label that empties must not collapse the row it sits in — the row below jumping up by
        // a line the moment a preview arrives empty is a worse bug than a blank line.
        testing::with_theme(Palette::DARK, |t| {
            let s = Text::new("").measure(Constraints::unbounded(), t);
            assert_eq!(s.w, 0);
            assert_eq!(s.h, FontRole::Body.line_height(t));
        });
    }

    #[test]
    fn a_line_never_measures_wider_than_it_was_offered() {
        testing::with_theme(Palette::DARK, |t| {
            let s = Text::new("a very long name indeed").measure(Constraints::loose(40, 20), t);
            assert!(s.w <= 40, "measured {} against an offer of 40", s.w);
            assert!(s.h <= 20);
        });
    }

    #[test]
    fn long_text_in_a_narrow_band_never_produces_a_negative_size() {
        // The failure this guards is the one `Constraints` documents: a negative dimension becomes
        // an inverted rect, an inverted rect draws nothing, and nothing reports anything.
        testing::with_theme(Palette::DARK, |t| {
            let long = "a".repeat(500);
            for (w, h) in [(0, 0), (1, 1), (3, 2), (-5, -5), (2, 100)] {
                for lines in [1usize, 3] {
                    let s = Text::new(&long)
                        .max_lines(lines)
                        .measure(Constraints::loose(w, h), t);
                    assert!(s.w >= 0 && s.h >= 0, "{w}x{h}, {lines} lines gave {s:?}");
                    assert!(s.w <= w.max(0) && s.h <= h.max(0), "{w}x{h} gave {s:?}");
                }
            }
        });
    }

    #[test]
    fn drawing_into_a_hairline_rect_paints_nothing_and_does_not_panic() {
        testing::with_theme(Palette::DARK, |t| {
            let ((), px) = testing::with_canvas(Size::new(40, 20), |c| {
                Text::new("overflowing").max_lines(3).draw(c, Rect::EMPTY, t);
                // An inverted rect: what an over-subtracted constraint turns into downstream.
                Text::new("overflowing").draw(c, Rect::new(30, 10, 5, 2), t);
            });
            assert_eq!(painted(&px), 0);
        });
    }

    #[test]
    fn wrapping_grows_the_height_by_whole_lines_and_stops_at_max_lines() {
        testing::with_theme(Palette::DARK, |t| {
            let lh = FontRole::Body.line_height(t);
            let advance = FontRole::Body.measure(t, "a");
            // Room for four characters a line, given six words of one character.
            let width = advance * 4;
            let words = "a a a a a a";

            let two = Text::new(words).max_lines(2).measure(Constraints::loose(width, 200), t);
            assert_eq!(two.h, 2 * lh);

            let one = Text::new(words).max_lines(1).measure(Constraints::loose(width, 200), t);
            assert_eq!(one.h, lh, "one line stays one line however much text there is");

            let ten = Text::new(words).max_lines(10).measure(Constraints::loose(width, 200), t);
            assert!(ten.h > two.h, "unclamped it should need more than two lines");
            assert_eq!(ten.h % lh, 0, "height is whole lines or the box has a sliver in it");
        });
    }

    #[test]
    fn a_short_string_does_not_pay_for_the_lines_it_was_allowed() {
        // `max_lines` is a ceiling, not a reservation. A two-line preview slot holding a one-line
        // preview must measure one line, or every short row on the screen grows by a line.
        testing::with_theme(Palette::DARK, |t| {
            let lh = FontRole::Body.line_height(t);
            let s = Text::new("a").max_lines(3).measure(Constraints::loose(200, 200), t);
            assert_eq!(s.h, lh);
        });
    }

    // ---- the measure cache's side of the contract ------------------------------------------------

    #[test]
    fn the_hash_moves_when_the_measurement_would() {
        let base = Text::new("Hello");
        assert_ne!(base.content_hash(), Text::new("Hellp").content_hash(), "text");
        assert_ne!(
            base.content_hash(),
            Text::new("Hello").font(FontRole::Title).content_hash(),
            "role"
        );
        assert_ne!(base.content_hash(), Text::new("Hello").max_lines(2).content_hash(), "max_lines");
        assert_ne!(base.content_hash(), Text::new("Hello").align(Align::End).content_hash(), "align");
        // Stable for the same description, or nothing would ever be cached.
        assert_eq!(base.content_hash(), Text::new("Hello").content_hash());
    }

    #[test]
    fn the_hash_ignores_what_cannot_move_a_pixel() {
        // Selection changes a row's colour on every D-pad press. If that invalidated the measure,
        // the cache would miss on every row of the list every time the highlight moved — which is
        // the exact moment the frame budget is tightest.
        let base = Text::new("Hello");
        assert_eq!(base.content_hash(), Text::new("Hello").dim().content_hash());
        assert_eq!(base.content_hash(), Text::new("Hello").accent().content_hash());
        assert_eq!(
            base.content_hash(),
            Text::new("Hello").color(Color::hex(0xFF00FF)).content_hash()
        );
        assert_eq!(base.content_hash(), Text::new("Hello").flex(3).content_hash());
    }

    #[test]
    fn a_late_difference_in_a_long_string_still_changes_the_hash() {
        // The plan's sketch hashed the first eight bytes. Two chat messages differing at character
        // fifty would have collided, and the second would have been laid out at the first's height.
        let a = "the quick brown fox jumps over the lazy dog";
        let b = "the quick brown fox jumps over the lazy cat";
        assert_ne!(Text::new(a).content_hash(), Text::new(b).content_hash());
        assert_ne!(Text::new(a).content_hash(), Text::new(&a[..a.len() - 1]).content_hash());
    }

    // ---- drawing ---------------------------------------------------------------------------------

    #[test]
    fn drawing_puts_ink_where_the_rect_is_and_nowhere_else() {
        testing::with_theme(Palette::DARK, |t| {
            let ((), px) = testing::with_canvas(Size::new(64, 32), |c| {
                Text::new("aaa").draw(c, Rect::new(0, 0, 64, 16), t);
            });
            assert!(painted(&px) > 0, "nothing was drawn at all");
            let bottom_half = &px[(64 * 16) as usize..];
            assert_eq!(painted(bottom_half), 0, "text escaped the rect it was given");
        });
    }

    #[test]
    fn wrapped_text_uses_the_rows_it_was_given() {
        testing::with_theme(Palette::DARK, |t| {
            let lh = FontRole::Body.line_height(t);
            let advance = FontRole::Body.measure(t, "a");
            let box_ = Rect::from_xywh(0, 0, advance * 4, lh * 4);
            let ink_for = |lines: usize| {
                let ((), px) = testing::with_canvas(Size::new(advance * 4, lh * 4), |c| {
                    Text::new("a a a a a a").max_lines(lines).draw(c, box_, t);
                });
                painted(&px)
            };
            assert!(
                ink_for(3) > ink_for(1),
                "three wrapped lines should ink more than one truncated one"
            );
        });
    }

    #[test]
    fn a_role_change_changes_what_is_drawn_with() {
        // The point of a role rather than a captured font: the same widget resolves differently
        // against a different theme, and nothing in the widget had to be rebuilt.
        testing::with_theme(Palette::DARK, |t| {
            assert_eq!(FontRole::Small.font(t).line_height(), t.fonts.small.line_height());
            assert_eq!(FontRole::Title.font(t).line_height(), t.fonts.title.line_height());
        });
    }

    #[test]
    fn ink_resolves_against_the_palette_not_against_construction() {
        testing::with_theme(Palette::DARK, |t| {
            assert_eq!(Ink::Text.resolve(t), t.palette.text);
            assert_eq!(Ink::Dim.resolve(t), t.palette.dim);
            assert_eq!(Ink::Accent.resolve(t), t.palette.accent);
            assert_eq!(Ink::Chrome.resolve(t), t.palette.chrome_text);
            let literal = Color::hex(0x123456);
            assert_eq!(Ink::Fixed(literal).resolve(t), literal);
        });
    }

    #[test]
    fn flex_is_reported_to_the_parent_and_never_negative() {
        assert_eq!(Text::new("x").flex_weight(), 0);
        assert_eq!(Text::new("x").flex(2).flex_weight(), 2);
        // A negative weight would make a parent's `remaining * weight / total` go backwards and
        // hand a sibling a negative width — the inverted rect again, one layer up.
        assert_eq!(Text::new("x").flex(-4).flex_weight(), 0);
    }

    #[test]
    fn max_lines_zero_is_read_as_one() {
        assert_eq!(Text::new("x").max_lines(0).max_lines, 1);
    }
}
