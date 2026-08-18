//! The unread count that rides at the end of a chat row.

use alloc::string::String;

use symbian_gfx::{Canvas, Point, Rect, Size};
use symbian_ui::{chrome, Theme};

use crate::constraints::Constraints;
use crate::widget::{hash_i32, hash_str, Widget, WidgetHash};

/// A small filled pill with a number in it.
///
/// A leaf over [`symbian_ui::chrome::badge`], with one wrinkle worth knowing about: that function
/// draws from a **right edge** and returns the width it used, because in the imperative row the
/// badge is placed by walking leftwards from the margin. Here the layout has already decided the
/// rect, so this asks `chrome` for the same width up front, reports it as its measured size, and
/// then draws against the right edge of the rect it was given. The number is derived the same way
/// in both calls, so the box and the pill agree by construction rather than by coincidence.
pub struct Badge {
    label: String,
    /// Whether this sits inside a selected row — the fill and text colours differ, and
    /// `chrome::unread_colors` owns that choice for both this and the imperative row.
    selected: bool,
}

impl Badge {
    /// A badge showing `count`, or `None` when there is nothing to show.
    ///
    /// Returning `None` for zero rather than an empty badge is deliberate: a row with no unread
    /// messages has no badge at all, and a zero-width widget in the line would still take its gap.
    pub fn count(count: u32, selected: bool) -> Option<Self> {
        if count == 0 {
            return None;
        }
        // Four characters is the ceiling the imperative row uses; beyond it the pill stops being a
        // glanceable shape and starts being a number nobody reads.
        let label = if count > 999 { String::from("999+") } else { itoa(count) };
        Some(Self { label, selected })
    }

    /// A badge with arbitrary text, for a caller that is not counting messages.
    pub fn new(label: impl Into<String>, selected: bool) -> Self {
        Self { label: label.into(), selected }
    }
}

impl Widget for Badge {
    fn content_hash(&self) -> WidgetHash {
        let h = hash_str(0, &self.label);
        hash_i32(h, self.selected as i32)
    }

    fn measure(&self, constraints: Constraints, theme: &Theme<'_>) -> Size {
        let f = theme.fonts.small;
        let h = f.line_height() + 2;
        // `chrome::badge`'s own sizing, copied exactly: `(text + 8).max(h)` — eight pixels of
        // padding, never narrower than tall so a single digit stays a circle rather than a slot.
        // Written as `text + h` at first, which is wider by half the pill on every badge and shows
        // up not as a fat badge — the draw uses chrome's number — but as a preview truncated early,
        // because the *measured* width is what the row divides by.
        let w = (f.measure(&self.label) + 8).max(h);
        constraints.constrain(Size::new(w, h))
    }

    /// The pill is taller than the line it sits on, and the overlap is the design.
    ///
    /// `measure` asks for `line_height + 2`; the line it lands in is sized by the small text around
    /// it, and the column above it has already spent the slack — so the rect that comes back is two
    /// pixels short and `chrome::badge` draws its own intrinsic height regardless. Clipped to that
    /// rect the pill loses its top two rows, which is a flat lid on a circle: invisible in a glance,
    /// forty differing pixels in a comparison. The hand-written row has no clip and reaches into the
    /// name's line box by exactly that much.
    fn overflow_visible(&self) -> bool {
        true
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        let (fill, fg) = chrome::unread_colors(theme, self.selected);
        // Anchored to the BOTTOM of the rect, not the top. `chrome::badge` always draws its own
        // intrinsic height — a line of small text plus two — and a pill is two pixels taller than
        // the text beside it, so in a line sized to that text there is not room for it. Drawn from
        // the top it hangs two pixels low; drawn from the bottom it sits on the baseline its
        // neighbours share, which is what the hand-written row gets by anchoring to `r.y1 - 4`.
        let h = theme.fonts.small.line_height() + 2;

        chrome::badge(c, Point::new(rect.x1, rect.y1 - h), theme, &self.label, fill, fg);
    }
}

/// A `u32` as decimal, without `alloc::format!`.
///
/// `format!` here would allocate on every frame for every unread row, which is the cost this crate
/// spends its cache budget avoiding elsewhere. A badge is at most four characters.
fn itoa(mut n: u32) -> String {
    if n == 0 {
        return String::from("0");
    }
    let mut buf = [0u8; 10];
    let mut i = buf.len();
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    String::from_utf8_lossy(&buf[i..]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian_ui::{testing, Palette};

    #[test]
    fn no_unread_is_no_badge_at_all() {
        // Not an empty badge: a zero-width widget in a row still takes the gap beside it, so the
        // row would be spaced as though something were there.
        assert!(Badge::count(0, false).is_none());
        assert!(Badge::count(1, false).is_some());
    }

    #[test]
    fn a_big_count_becomes_a_shape_rather_than_a_number() {
        assert_eq!(Badge::count(999, false).unwrap().label, "999");
        assert_eq!(Badge::count(1000, false).unwrap().label, "999+");
        assert_eq!(Badge::count(u32::MAX, false).unwrap().label, "999+");
    }

    #[test]
    fn a_single_digit_stays_round() {
        testing::with_theme(Palette::DARK, |t| {
            let s = Badge::count(7, false).unwrap().measure(Constraints::loose(200, 40), t);
            assert!(s.w >= s.h, "a 1 must not make a narrow slot: {s:?}");
        });
    }

    #[test]
    fn a_wider_label_makes_a_wider_pill() {
        testing::with_theme(Palette::DARK, |t| {
            let one = Badge::count(7, false).unwrap().measure(Constraints::loose(200, 40), t);
            let many = Badge::count(999, false).unwrap().measure(Constraints::loose(200, 40), t);
            assert!(many.w > one.w);
            assert_eq!(many.h, one.h, "the height is the font's, not the text's");
        });
    }

    #[test]
    fn selection_changes_the_colours_not_the_size() {
        testing::with_theme(Palette::DARK, |t| {
            let off = Badge::count(7, false).unwrap().measure(Constraints::loose(200, 40), t);
            let on = Badge::count(7, true).unwrap().measure(Constraints::loose(200, 40), t);
            assert_eq!(off, on);
            // But the digest must move, or a row that becomes selected keeps the old pixels.
            assert_ne!(
                Badge::count(7, false).unwrap().content_hash(),
                Badge::count(7, true).unwrap().content_hash()
            );
        });
    }

    #[test]
    fn itoa_agrees_with_the_formatter_it_replaces() {
        for n in [0u32, 1, 9, 10, 99, 100, 999, 1000, 4294967295] {
            assert_eq!(itoa(n), alloc::format!("{n}"));
        }
    }
}
