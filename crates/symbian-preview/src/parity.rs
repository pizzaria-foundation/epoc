//! One screen, drawn two ways, compared pixel for pixel.
//!
//! ```ignore
//! let mut p = Parity::new("parity-out");
//! atlases.with_themes(|dark, light| {
//!     for (name, theme) in [("dark", dark), ("light", light)] {
//!         p.check(&format!("chats-{name}"), theme,
//!                 |c| by_hand.draw(c, &store, theme),
//!                 |c| declared(c, &store, theme));
//!     }
//! });
//! p.finish();   // panics with a report if anything differed
//! ```
//!
//! # Why this is a crate feature and not an example
//!
//! Because it is the acceptance criterion for every screen that gets rewritten, and it was living
//! inside one application's example — three hundred lines of band attribution and diff mapping that
//! the *second* screen to be migrated would have had to copy. A comparison harness that is copied
//! is a comparison harness that drifts: the copy loses the case the original was written to catch.
//!
//! # What it is evidence of, and what it is not
//!
//! A green check means: for **these scenes**, the two implementations put the same bytes in the
//! frame buffer. It says nothing about the scenes nobody wrote. That distinction is not pedantry —
//! it is the whole finding from the first migration in this SDK: a dialog list declared "identical"
//! had been compared in exactly one state (one store, one theme, selection zero, scroll zero), and
//! the states nobody rendered hid a real divergence in the scrollbar gutter. So [`Parity::check`]
//! counts what it checked and [`Parity::finish`] prints it, because a suite that quietly runs one
//! scene reads exactly like a suite that runs twelve.
//!
//! The reference side is what ships. That makes it the standard, not the correct answer: where the
//! two differ, read the difference before nudging either. Adjusting the new side until the numbers
//! agree proves only that two things can be made identical, which nobody doubted.

use std::path::{Path, PathBuf};

use symbian_gfx::{Canvas, Color, Rect, Size, E72_SCREEN};
use symbian_ui::Theme;

use crate::Sheet;

/// A pair of renders that disagreed, described well enough to act on.
pub struct Diff {
    /// The scene's name, as given to [`Parity::check`].
    pub scene: String,
    /// How many pixels differ, out of the whole screen.
    pub pixels: usize,
    /// Where the first difference is, in screen coordinates.
    pub first: (i32, i32),
    /// Differing pixels per band: title, content, softkeys.
    pub by_band: [usize; 3],
    /// The first differing row within each band, when there is one.
    pub first_row: [Option<i32>; 3],
}

impl Diff {
    /// A report a person can act on without opening the PNGs.
    pub fn describe(&self) -> String {
        let (x, y) = self.first;
        let mut s = format!(
            "{}: {} pixels differ, first at ({x}, {y}) {}\n    title {}, content {}, softkeys {}",
            self.scene,
            self.pixels,
            BANDS[band_of_row(y, &self.first_row)],
            self.by_band[0],
            self.by_band[1],
            self.by_band[2],
        );
        for (i, row) in self.first_row.iter().enumerate() {
            if let Some(y) = row {
                s.push_str(&format!("\n    first differing row in {}: y={y}", BANDS[i]));
            }
        }
        s
    }
}

const BANDS: [&str; 3] = ["the title bar", "the content", "the softkey bar"];

/// Which band a row belongs to, from the bands the diff already measured.
fn band_of_row(y: i32, first_row: &[Option<i32>; 3]) -> usize {
    first_row.iter().position(|r| *r == Some(y)).unwrap_or(1)
}

/// A run of comparisons, and what came of them.
pub struct Parity {
    out: PathBuf,
    checked: Vec<String>,
    diffs: Vec<Diff>,
    /// Whether to write the PNGs of scenes that matched, as well as the ones that did not.
    keep_matching: bool,
}

impl Parity {
    /// Compare into `out_dir`, where the PNGs of anything that differs are written.
    pub fn new(out_dir: impl Into<PathBuf>) -> Self {
        Self { out: out_dir.into(), checked: Vec::new(), diffs: Vec::new(), keep_matching: false }
    }

    /// Also write the PNGs of scenes that matched — for building a contact sheet rather than
    /// chasing a difference.
    pub fn keep_matching(mut self, on: bool) -> Self {
        self.keep_matching = on;
        self
    }

    /// Render both sides of one scene and compare them.
    ///
    /// Returns whether they matched. Both closures are handed a canvas over a fresh screen-sized
    /// buffer — fresh, so a scene cannot pass by inheriting pixels the previous one drew, which is
    /// the failure mode of comparing into a shared buffer.
    ///
    /// On a difference: three PNGs (`<scene>-by-hand`, `<scene>-declared`, `<scene>-diff`) and a
    /// [`Diff`] recorded for [`Parity::finish`].
    pub fn check(
        &mut self,
        scene: &str,
        theme: &Theme<'_>,
        by_hand: impl FnOnce(&mut Canvas<'_>),
        declared: impl FnOnce(&mut Canvas<'_>),
    ) -> bool {
        self.checked.push(scene.to_string());

        let mut left = Sheet::new(E72_SCREEN);
        by_hand(&mut left.canvas());
        let mut right = Sheet::new(E72_SCREEN);
        declared(&mut right.canvas());

        match compare(scene, left.pixels(), right.pixels(), theme, E72_SCREEN) {
            None => {
                if self.keep_matching {
                    left.save(&self.out, &format!("{scene}-by-hand"));
                    right.save(&self.out, &format!("{scene}-declared"));
                }
                true
            }
            Some(diff) => {
                left.save(&self.out, &format!("{scene}-by-hand"));
                right.save(&self.out, &format!("{scene}-declared"));
                diff_map(left.pixels(), right.pixels(), E72_SCREEN)
                    .save(&self.out, &format!("{scene}-diff"));
                self.diffs.push(diff);
                false
            }
        }
    }

    /// How many scenes were compared.
    ///
    /// Worth asserting in a test. A refactor that accidentally stops building scenes turns a suite
    /// into a green light for nothing, and the count is the only thing that notices.
    pub fn checked(&self) -> usize {
        self.checked.len()
    }

    pub fn diffs(&self) -> &[Diff] {
        &self.diffs
    }

    /// The whole run as text: what was compared, and what disagreed.
    pub fn report(&self) -> String {
        let mut s = format!("{} scene(s) compared: {}", self.checked.len(), self.checked.join(", "));
        if self.diffs.is_empty() {
            s.push_str("\nall identical");
            return s;
        }
        for d in &self.diffs {
            s.push('\n');
            s.push_str(&d.describe());
        }
        s.push_str(&format!("\nPNGs in {}", self.out.display()));
        s
    }

    /// Print the report, and panic if anything differed.
    ///
    /// The panic is the point: this is called from a `#[test]`, and a comparison that reports a
    /// difference without failing the build is a comparison nobody will read twice.
    pub fn finish(self) {
        let report = self.report();
        println!("{report}");
        assert!(self.diffs.is_empty(), "{report}");
    }
}

/// Compare two buffers, attributing the difference to the bands `Frame::split` produces.
fn compare(
    scene: &str,
    left: &[u16],
    right: &[u16],
    theme: &Theme<'_>,
    size: Size,
) -> Option<Diff> {
    let w = size.w as usize;
    // From the theme, never from memory. These were hardcoded in the first version of this
    // comparison — 26 and 214, against real values of 18 and 223 — and an hour went into a
    // difference "in the chrome" that was content wearing the wrong label. An instrument that
    // guesses does not fail; it misdirects.
    let title_end = theme.metrics.title_h;
    let keys_start = size.h - theme.metrics.softkey_h;

    let mut pixels = 0usize;
    let mut first = None;
    let mut by_band = [0usize; 3];
    let mut first_row = [None; 3];

    for (i, (p, q)) in left.iter().zip(right).enumerate() {
        if p == q {
            continue;
        }
        pixels += 1;
        let (x, y) = ((i % w) as i32, (i / w) as i32);
        if first.is_none() {
            first = Some((x, y));
        }
        let band = if y < title_end {
            0
        } else if y < keys_start {
            1
        } else {
            2
        };
        by_band[band] += 1;
        if first_row[band].is_none() {
            first_row[band] = Some(y);
        }
    }

    first.map(|first| Diff {
        scene: scene.to_string(),
        pixels,
        first,
        by_band,
        first_row,
    })
}

/// A map of where two renders disagree: differing pixels in red, agreeing ones dimmed.
///
/// Far faster to read than a coordinate, and it shows the *shape* of a difference — a row shifted
/// by two pixels and a missing divider look nothing alike on a map and identical in a count.
fn diff_map(left: &[u16], right: &[u16], size: Size) -> Sheet {
    let mut sheet = Sheet::new(size);
    {
        let mut c = sheet.canvas();
        for (i, (p, q)) in left.iter().zip(right).enumerate() {
            let (x, y) = ((i % size.w as usize) as i32, (i / size.w as usize) as i32);
            let col = if p == q {
                // RGB565 back to components, halved, so the agreeing picture stays legible
                // underneath without competing with the marks.
                let (r, g, b) = ((*p >> 11) as u8, ((*p >> 5) & 0x3f) as u8, (*p & 0x1f) as u8);
                Color::rgb(r << 2, g << 1, b << 2)
            } else {
                Color::rgb(255, 0, 0)
            };
            c.fill_rect(Rect::from_xywh(x, y, 1, 1), col);
        }
    }
    sheet
}

/// Where the PNGs of a failing comparison go by default, beside the crate that ran it.
pub fn default_out_dir() -> PathBuf {
    Path::new("parity-out").to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian_gfx::Color;

    /// A theme over the test atlas, which is all `compare` needs: it reads two metrics.
    fn with_theme<R>(f: impl FnOnce(&Theme<'_>) -> R) -> R {
        symbian_ui::testing::with_theme(symbian_ui::Palette::DARK, f)
    }

    fn out() -> PathBuf {
        std::env::temp_dir().join("epoc-parity-selftest")
    }

    #[test]
    fn two_identical_renders_match() {
        let mut p = Parity::new(out());
        with_theme(|t| {
            let same = |c: &mut Canvas<'_>| c.fill_rect(Rect::from_xywh(0, 0, 10, 10), Color::hex(0x00FF00));
            assert!(p.check("same", t, same, same));
        });
        assert_eq!(p.diffs().len(), 0);
        assert_eq!(p.checked(), 1);
    }

    #[test]
    fn a_single_pixel_of_difference_is_caught_and_placed() {
        // The property the whole harness rests on: it must fail on a difference far too small to
        // see. One pixel, in the content band, reported at its own coordinate.
        let mut p = Parity::new(out());
        with_theme(|t| {
            let plain = |_c: &mut Canvas<'_>| {};
            let one_dot = |c: &mut Canvas<'_>| {
                c.fill_rect(Rect::from_xywh(17, 100, 1, 1), Color::hex(0xFF0000));
            };
            assert!(!p.check("one-dot", t, plain, one_dot));
        });
        let d = &p.diffs()[0];
        assert_eq!(d.pixels, 1);
        assert_eq!(d.first, (17, 100));
        assert_eq!(d.by_band, [0, 1, 0], "y=100 is the content band");
        assert!(d.describe().contains("(17, 100)"));
    }

    #[test]
    fn a_difference_is_attributed_to_the_band_it_is_in() {
        // So a report says "the title bar" rather than a number. The bands come from the theme's
        // own metrics, which is what stops this from being two hardcoded row numbers that go stale.
        let mut p = Parity::new(out());
        with_theme(|t| {
            let keys_y = 240 - t.metrics.softkey_h;
            let plain = |_c: &mut Canvas<'_>| {};
            let marks = move |c: &mut Canvas<'_>| {
                c.fill_rect(Rect::from_xywh(0, 0, 2, 1), Color::hex(0xFF0000));
                c.fill_rect(Rect::from_xywh(0, keys_y, 3, 1), Color::hex(0xFF0000));
            };
            assert!(!p.check("bands", t, plain, marks));
        });
        let d = &p.diffs()[0];
        assert_eq!(d.by_band, [2, 0, 3]);
        assert_eq!(d.first_row[0], Some(0));
        assert!(d.first_row[1].is_none(), "nothing differed in the content");
        assert!(d.describe().contains("the title bar"));
    }

    #[test]
    fn the_report_names_every_scene_it_compared() {
        // A suite that quietly stops building scenes reads exactly like a suite that runs them all.
        // The count and the names are the only things that notice.
        let mut p = Parity::new(out());
        with_theme(|t| {
            let plain = |_c: &mut Canvas<'_>| {};
            p.check("empty", t, plain, plain);
            p.check("also-empty", t, plain, plain);
        });
        assert_eq!(p.checked(), 2);
        let r = p.report();
        assert!(r.contains("2 scene(s)"), "{r}");
        assert!(r.contains("empty, also-empty"), "{r}");
        assert!(r.contains("all identical"), "{r}");
    }

    #[test]
    fn each_scene_gets_a_fresh_buffer() {
        // Two scenes compared into a shared buffer can pass by inheriting the previous scene's
        // pixels. Here the second scene draws nothing at all on both sides and must still match,
        // which it cannot do if the first scene's ink is still there on one side only.
        let mut p = Parity::new(out());
        with_theme(|t| {
            let inked = |c: &mut Canvas<'_>| c.clear(Color::hex(0x123456));
            assert!(p.check("first", t, inked, inked));
            let plain = |_c: &mut Canvas<'_>| {};
            assert!(p.check("second", t, plain, plain));
        });
        assert!(p.diffs().is_empty());
    }
}
