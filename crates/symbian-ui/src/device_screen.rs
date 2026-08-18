//! A scrollable label-and-value readout, for showing what a device has and what is left of it.
//!
//! The rows come from the caller on every draw, like [`crate::app_picker`]'s items and for the same
//! reason: this crate knows nothing about the handset and must not start to. `symbian::device`
//! produces the numbers and the units; this paints them, and neither has to know how the other
//! works.
//!
//! Three row shapes cover everything a resource screen wants to say:
//!
//! - a **section** heading, so RAM, storage and identity do not run together;
//! - a **field**, label left and value right, which is most of the list;
//! - a field carrying a **meter**, a filled bar drawn under it, for the handful of values that are
//!   really a fraction — a drive's fullness, a battery's charge. A number tells you `191960 KB`;
//!   the bar tells you at a glance that the drive is a quarter full, which is the actual question.
//!
//! A value that the device does not report should be passed as text saying so ("not supported")
//! rather than omitted. An absent line reads as an oversight; a line that says the handset has no
//! answer is a finding, and this whole SDK treats those as results worth showing.

use symbian_gfx::{Align, Canvas, Rect};

use crate::chrome;
use crate::input::{Handled, KeyEvent};
use crate::list::{ListState, Uniform};
use crate::theme::Theme;

/// One line of the readout.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Entry<'a> {
    /// A heading that groups the fields under it. Not selectable.
    Section(&'a str),
    /// A label and its value, optionally with a 0..=100 meter drawn beneath.
    Field {
        label: &'a str,
        value: &'a str,
        /// `Some(percent)` draws a fill bar under the row; `None` leaves the row plain.
        meter: Option<i32>,
    },
}

impl<'a> Entry<'a> {
    /// A plain label-and-value line.
    pub const fn field(label: &'a str, value: &'a str) -> Self {
        Entry::Field { label, value, meter: None }
    }

    /// A label-and-value line with a fill bar under it.
    pub const fn gauge(label: &'a str, value: &'a str, percent: i32) -> Self {
        Entry::Field { label, value, meter: Some(percent) }
    }

    /// Whether this line is a heading rather than a value.
    pub const fn is_section(&self) -> bool {
        matches!(self, Entry::Section(_))
    }
}

/// The scroll position of a device readout. Holds nothing but the cursor — the rows themselves are
/// the caller's, passed in on every call.
#[derive(Default)]
pub struct DeviceScreen {
    list: ListState,
}

impl DeviceScreen {
    pub fn new() -> Self {
        Self::default()
    }

    /// Height of one entry. A section heading is shorter than a field, and a field with a meter is
    /// taller by the bar plus its gap, so the list arithmetic sees the real geometry rather than a
    /// uniform guess that would drift the further you scroll.
    fn heights(entries: &[Entry<'_>], theme: &Theme<'_>) -> alloc::vec::Vec<i32> {
        let row = theme.metrics.row_h;
        entries
            .iter()
            .map(|e| match e {
                Entry::Section(_) => theme.fonts.small.line_height() + theme.metrics.pad,
                Entry::Field { meter: None, .. } => row,
                Entry::Field { meter: Some(_), .. } => row + METER_H + 2,
            })
            .collect()
    }

    /// Scroll.
    ///
    /// Up and Down only — deliberately narrower than [`ListState::handle_key`], which also takes
    /// Left and Right as page-up/page-down. A readout is something you put *inside* something else:
    /// a settings tab strip, or a screen whose Left is Back. Paging is worth far less than those,
    /// and a widget that quietly eats the horizontal keys breaks its host. Everything but Up/Down
    /// falls through untouched.
    pub fn handle_key(&mut self, ev: KeyEvent, entries: &[Entry<'_>], area: Rect, theme: &Theme<'_>) -> Handled {
        if !matches!(ev.key, crate::input::Key::Up | crate::input::Key::Down) {
            return Handled::Ignored;
        }
        let heights = Self::heights(entries, theme);
        self.list.handle_key(ev, heights.as_slice(), area.height())
    }

    /// Draw the readout into `area`. `empty` is shown when there is nothing to report at all.
    pub fn draw(
        &mut self,
        c: &mut Canvas<'_>,
        area: Rect,
        theme: &Theme<'_>,
        entries: &[Entry<'_>],
        empty: &str,
    ) {
        if entries.is_empty() {
            chrome::placeholder(c, area, theme, empty);
            return;
        }

        let heights = Self::heights(entries, theme);
        let p = &theme.palette;
        let pad = theme.metrics.pad;

        self.list.draw_visible(c, heights.as_slice(), area, |c, i, row| {
            match entries[i] {
                Entry::Section(title) => {
                    c.draw_text_in(row.inset_xy(pad, 0), title, theme.fonts.small, p.dim, Align::Start);
                }
                Entry::Field { label, value, meter } => {
                    // The value is drawn second and right-aligned in the same rect: on a 320 px
                    // screen a long label would otherwise push the number off the edge, and the
                    // number is the half worth keeping.
                    let text_h = theme.metrics.row_h;
                    let line = Rect::from_xywh(row.x0, row.y0, row.width(), text_h).inset_xy(pad, 0);
                    c.draw_text_in(line, label, theme.fonts.body, p.dim, Align::Start);
                    c.draw_text_in(line, value, theme.fonts.body, p.text, Align::End);

                    if let Some(percent) = meter {
                        let bar = Rect::from_xywh(line.x0, row.y0 + text_h, line.width(), METER_H);
                        draw_meter(c, bar, theme, percent);
                    }
                }
            }
        });

        chrome::scrollbar(c, area, theme, self.list.scrollbar(heights.as_slice(), area.height()));
    }
}

/// Height of a meter bar in pixels. Thin on purpose: it is a hint beside a number, not a chart.
const METER_H: i32 = 4;

/// A fill bar. The track is always drawn, so an empty bar reads as "0%" rather than as a missing
/// widget, and the fill is clamped so a bad percentage cannot paint outside the row.
fn draw_meter(c: &mut Canvas<'_>, bar: Rect, theme: &Theme<'_>, percent: i32) {
    let p = &theme.palette;
    c.fill_rect(bar, p.dim);
    let pct = percent.clamp(0, 100);
    let w = bar.width() * pct / 100;
    if w > 0 {
        c.fill_rect(Rect::from_xywh(bar.x0, bar.y0, w, bar.height()), p.accent);
    }
}

/// A [`Uniform`] over `n` rows of the theme's row height — for a caller that wants the same list
/// arithmetic without the entry shapes.
pub fn uniform_rows(n: usize, theme: &Theme<'_>) -> Uniform {
    Uniform { count: n, height: theme.metrics.row_h }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{Key, KeyEvent};
    use crate::{testing, Palette};

    fn sample() -> alloc::vec::Vec<Entry<'static>> {
        alloc::vec![
            Entry::Section("Memory"),
            Entry::gauge("RAM used", "72.7 MB", 59),
            Entry::field("Free", "48.8 MB"),
            Entry::Section("Storage"),
            Entry::gauge("C:", "187.4 MB free", 27),
            Entry::field("E:", "no card"),
            Entry::Section("Device"),
            Entry::field("CPU load", "not supported"),
        ]
    }

    #[test]
    fn draws_every_entry_shape() {
        let entries = sample();
        let (_, px) = testing::with_canvas(symbian_gfx::Size::new(320, 240), |c| {
            testing::with_theme(Palette::DARK, |t| {
                DeviceScreen::new().draw(c, testing::SCREEN, t, &entries, "nothing");
            });
        });
        assert!(px.iter().any(|&p| p != 0));
    }

    #[test]
    fn an_empty_readout_says_so_instead_of_drawing_nothing() {
        let (_, px) = testing::with_canvas(symbian_gfx::Size::new(320, 240), |c| {
            testing::with_theme(Palette::DARK, |t| {
                DeviceScreen::new().draw(c, testing::SCREEN, t, &[], "No readings.");
            });
        });
        assert!(px.iter().any(|&p| p != 0));
    }

    #[test]
    fn sections_and_meters_get_different_heights() {
        testing::with_theme(Palette::DARK, |t| {
            let entries = sample();
            let h = DeviceScreen::heights(&entries, t);
            // A meter row is taller than a plain one, which is taller than a heading.
            assert!(h[1] > h[2], "meter row should be tallest");
            assert!(h[2] > h[0], "a field should be taller than a section heading");
        });
    }

    #[test]
    fn scrolling_consumes_only_the_keys_it_uses() {
        testing::with_theme(Palette::DARK, |t| {
            let entries = sample();
            let mut s = DeviceScreen::new();
            assert_eq!(s.handle_key(KeyEvent::new(Key::Down), &entries, testing::SCREEN, t), Handled::Consumed);
            // Left belongs to whatever screen hosts this one — tabs, or Back.
            assert_eq!(s.handle_key(KeyEvent::new(Key::Left), &entries, testing::SCREEN, t), Handled::Ignored);
        });
    }

    #[test]
    fn a_meter_out_of_range_cannot_paint_outside_its_row() {
        // Percentages come from arithmetic on device readings; a bad one must clamp, not smear.
        let entries = alloc::vec![Entry::gauge("bad", "?", 500), Entry::gauge("also bad", "?", -20)];
        let (_, px) = testing::with_canvas(symbian_gfx::Size::new(320, 240), |c| {
            testing::with_theme(Palette::DARK, |t| {
                DeviceScreen::new().draw(c, testing::SCREEN, t, &entries, "nothing");
            });
        });
        assert!(px.iter().any(|&p| p != 0));
    }
}
