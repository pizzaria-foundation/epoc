//! Does the phone's own theme have colours we can read, and what are they?
//!
//! # Why this is a throwaway binary and not a feature
//!
//! `aknskins` is not in the base library set, and on Symbian an import that does not resolve does
//! **not** fail the build or report an error — the image silently never loads. So "does this link and
//! run on the E72?" cannot be answered by reasoning, only by a binary nobody minds losing. That is
//! the argument `USE_AKNICON` makes in `tools/symbuild` word for word, and this is the probe it says
//! to write first.
//!
//! It answers four questions, in order, because each one makes the next worth asking:
//!
//! 1. **Did it load at all?** If the app opens, the import resolved. Nothing on screen is needed for
//!    that answer — the answer is that there is a screen.
//! 2. **Is `AknsUtils::SkinInstance()` non-null?** A GUI process should have one, because
//!    `shim_app.cpp` constructs the app UI with `EAknEnableSkin`. If it is null, everything after is
//!    a discussion about nothing.
//! 3. **Which indices does this handset actually fill, and with what?** `AknsConstants.h` comments
//!    every index (`EAknsCIQsnTextColorsCG6 = 5, // text #6 main area main area texts`), but a
//!    comment is a promise from Nokia's SDK, not a measurement of this phone with this theme. The
//!    probe walks all six tables and reports every answer *including the refusals*, because a gap in
//!    a table is the finding that would otherwise become a palette derived from an empty slot.
//! 4. **Does it change with the theme?** Change it in Settings, reopen, compare. Only the human
//!    holding the phone can do this one.
//!
//! # Everything goes to the log as well as the screen
//!
//! The screen holds one table at a time; the log holds all of it, and the log is what gets pulled off
//! the phone and pasted into `docs/reference/skinprobe.txt`. A probe whose findings only exist as
//! pixels is a probe that has to be re-run to be cited.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use symbian::skin::{self, Background, Table};
use symbian_gfx::{Align, Color};
use symbian_ui::{chrome, App, Canvas, Handled, Key, KeyEvent, Rect, Softkey, Theme};

/// The four themed backgrounds, with the name to print.
///
/// These matter more than the colour tables did, and that is the probe's own finding: the tables came
/// back as 126 greys. An S60 theme keeps its hue in its furniture, and the furniture is these.
const BACKGROUNDS: &[(Background, &str)] = &[
    (Background::Screen, "QsnBgScreen"),
    (Background::Status, "QsnBgAreaStatus"),
    (Background::Control, "QsnBgAreaControl"),
    (Background::Main, "QsnBgAreaMain"),
];

/// The six colour tables, with the name to print. In the order `aknsconstants.hrh` declares them, so
/// a reader comparing the log against the header reads down both at once.
const TABLES: &[(Table, &str)] = &[
    (Table::Component, "QsnComponentColors"),
    (Table::Icon, "QsnIconColors"),
    (Table::Text, "QsnTextColors"),
    (Table::Line, "QsnLineColors"),
    (Table::Other, "QsnOtherColors"),
    (Table::Highlight, "QsnHighlightColors"),
];

/// How many entries of one table to try.
///
/// 64, because `AknsConstants.h` documents `EAknsCIQsnTextColorsCG63 = 62` and the E72 fills every
/// index of that table.
///
/// The first run used 40 and the text table answered "40 of 40 filled, first gap -1" — which reads as
/// a full table and was really a short ruler. A probe whose cap is inside the answer measures its own
/// cap.
const PROBE_DEPTH: i32 = 64;

/// What one index answered.
enum Entry {
    /// A colour, as `0x00RRGGBB`.
    Color(u32),
    /// The platform refused, with its own code. Kept rather than skipped: a hole in the middle of a
    /// table is a finding, and "index 7 answered -1" is a different fact from "index 7 was not tried".
    Refused(i32),
}

/// One table's worth of answers.
struct Probed {
    name: &'static str,
    entries: Vec<(i32, Entry)>,
}

impl Probed {
    /// How many indices answered with a colour.
    fn filled(&self) -> usize {
        self.entries.iter().filter(|(_, e)| matches!(e, Entry::Color(_))).count()
    }

    /// The first index that refused, if any — the table's apparent end.
    fn first_gap(&self) -> Option<i32> {
        self.entries.iter().find(|(_, e)| matches!(e, Entry::Refused(_))).map(|(i, _)| *i)
    }
}

pub struct Skinprobe {
    tables: Vec<Probed>,
    /// What each themed background answered. `None` means it refused.
    backgrounds: Vec<(&'static str, Option<skin::Samples>)>,
    /// Which table is on screen.
    showing: usize,
    /// How far down that table the screen is scrolled, in entries.
    scroll: usize,
    exit: bool,
}

impl Skinprobe {
    pub fn new() -> Self {
        let mut me =
            Self { tables: Vec::new(), backgrounds: Vec::new(), showing: 0, scroll: 0, exit: false };
        me.probe();
        me
    }

    /// Read every table, and write the whole lot to the log.
    ///
    /// Done once, in the constructor, rather than per frame: `GetCachedColor` answers from a cache the
    /// skin server already handed over, so it is cheap — but a probe that re-read on every repaint
    /// would make its own log unreadable, and the question is what the values *are*, not whether they
    /// are stable across frames.
    fn probe(&mut self) {
        symbian::log!("skinprobe: reading the active theme");
        symbian::log!("major = {:#x} (EAknsMajorSkin)", skin::MAJOR_SKIN);

        for (table, name) in TABLES {
            let mut entries = Vec::new();
            for index in 0..PROBE_DEPTH {
                match skin::color(*table, index) {
                    Ok(c) => entries.push((index, Entry::Color(c))),
                    Err(e) => entries.push((index, Entry::Refused(e.code()))),
                }
            }
            let probed = Probed { name, entries };

            symbian::log!(
                "--- {} (minor {:#x}): {} of {} filled, first gap {}",
                name,
                *table as i32,
                probed.filled(),
                PROBE_DEPTH,
                probed.first_gap().unwrap_or(-1)
            );
            for (index, entry) in &probed.entries {
                match entry {
                    Entry::Color(c) => symbian::log!("  [{:>2}] {:#08x}", index, c),
                    // Logged too, and this is deliberate: the run where *everything* refuses is the
                    // one that matters most, and a log that printed only successes would be empty and
                    // say nothing about why.
                    Entry::Refused(code) => symbian::log!("  [{:>2}] refused {}", index, code),
                }
            }
            self.tables.push(probed);
        }

        // The backgrounds, which is where the colour actually is.
        for (which, name) in BACKGROUNDS {
            match skin::background(*which) {
                Ok(s) => {
                    symbian::log!(
                        "--- {} (minor {:#x}): {}x{}, {} samples, mean {}",
                        name,
                        *which as i32,
                        s.width,
                        s.height,
                        s.count,
                        match s.mean() {
                            Some(m) => format!("{m:#08x}"),
                            None => String::from("none"),
                        }
                    );
                    for (i, p) in s.pixels[..s.count].iter().enumerate() {
                        symbian::log!("  <{:>2}> {:#08x}", i, p);
                    }
                    self.backgrounds.push((name, Some(s)));
                }
                Err(e) => {
                    // Logged as a refusal with its code, because "this theme has no background
                    // bitmap" and "the call is not reachable" are different findings.
                    symbian::log!("--- {} (minor {:#x}): refused {}", name, *which as i32, e.code());
                    self.backgrounds.push((name, None));
                }
            }
        }

        let total: usize = self.tables.iter().map(Probed::filled).sum();
        symbian::log!("skinprobe: {total} colours read in total");
        if total == 0 {
            // Named as the specific finding it is, because "nothing" has three causes and they are not
            // the same news: no skin instance (headless or Avkon did not construct one), a theme with
            // no colour table, or an import that resolved to a stub.
            symbian::log!("skinprobe: NOTHING was readable — see the refusal codes above");
        }
    }

    /// The label and the lines for the table on screen.
    fn page(&self) -> (String, Vec<(String, Option<Color>)>) {
        let Some(t) = self.tables.get(self.showing) else {
            return (String::from("no tables"), Vec::new());
        };
        let head = format!(
            "{}  {}/{} filled",
            t.name,
            t.filled(),
            t.entries.len()
        );
        let lines = t
            .entries
            .iter()
            .skip(self.scroll)
            .map(|(i, e)| match e {
                Entry::Color(c) => (format!("[{i:>2}]  {c:#08x}"), Some(Color::hex(*c))),
                Entry::Refused(code) => (format!("[{i:>2}]  refused {code}"), None),
            })
            .collect();
        (head, lines)
    }
}

impl Default for Skinprobe {
    fn default() -> Self {
        Self::new()
    }
}

impl App for Skinprobe {
    fn title(&self) -> &str {
        "Skin probe"
    }

    fn handle_key(&mut self, ev: KeyEvent, _theme: &Theme<'_>, _screen: Rect) -> Handled {
        match ev.key {
            Key::Down => {
                self.scroll = (self.scroll + 1).min(PROBE_DEPTH as usize - 1);
                Handled::Consumed
            }
            Key::Up => {
                self.scroll = self.scroll.saturating_sub(1);
                Handled::Consumed
            }
            // Left and right change table, which is the axis a reader actually wants: the tables are
            // the six answers and the indices within one are a list.
            Key::Right | Key::Select => {
                self.showing = (self.showing + 1) % TABLES.len();
                self.scroll = 0;
                Handled::Consumed
            }
            Key::Left => {
                self.showing = (self.showing + TABLES.len() - 1) % TABLES.len();
                self.scroll = 0;
                Handled::Consumed
            }
            Key::Softkey(Softkey::Right) | Key::End => {
                self.exit = true;
                Handled::Consumed
            }
            _ => Handled::Ignored,
        }
    }

    fn draw(&mut self, c: &mut Canvas<'_>, theme: &Theme<'_>) {
        let screen = Rect::from_size(c.size());
        let frame = chrome::Frame::split(screen, theme, true, true);
        chrome::clear(c, theme);

        let (head, lines) = self.page();
        chrome::title_bar(c, frame.title, theme, "Skin probe", Some(&head));
        chrome::softkey_bar(c, frame.softkeys, theme, chrome::Softkeys::action("Table", "Exit"));

        let lh = theme.fonts.body.line_height();
        let pad = theme.metrics.pad;
        let mut y = frame.content.y0;
        for (text, colour) in &lines {
            if y + lh > frame.content.y1 {
                break;
            }
            // The swatch first and to the left, because the number beside it is only useful once you
            // have seen that the two agree — a probe whose swatch and hex could disagree would be
            // reporting its own rendering rather than the theme.
            if let Some(col) = colour {
                c.fill_rect(Rect::from_xywh(pad, y + 2, lh - 4, lh - 4), *col);
            }
            let at = Rect::new(pad + lh, y, frame.content.x1 - pad, y + lh);
            c.draw_text_in(at, text, theme.fonts.body, theme.palette.text, Align::Start);
            y += lh;
        }
    }

    fn should_exit(&self) -> bool {
        self.exit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_the_host_every_index_refuses_and_the_probe_says_so_rather_than_panicking() {
        // The host has no skin instance, so this is the shape of the run where the import resolved and
        // the answer is still "nothing" — which is a real possible outcome on the phone and must not be
        // a crash.
        let p = Skinprobe::new();
        assert_eq!(p.tables.len(), TABLES.len());
        for t in &p.tables {
            assert_eq!(t.filled(), 0, "{} answered on the host", t.name);
            assert_eq!(t.first_gap(), Some(0), "{} should refuse from the first index", t.name);
        }
    }

    #[test]
    fn every_background_is_recorded_including_the_refusals() {
        // Four entries whatever happened, so a log that shows three means one call never ran.
        let p = Skinprobe::new();
        assert_eq!(p.backgrounds.len(), BACKGROUNDS.len());
        for (name, s) in &p.backgrounds {
            assert!(s.is_none(), "{name} answered on the host");
        }
    }

    #[test]
    fn the_depth_reaches_past_the_deepest_table_the_header_documents() {
        // `EAknsCIQsnTextColorsCG63 = 62`. A cap at or below the real end measures the cap: the first
        // run used 40 and reported the text table full with no gap, which was the ruler and not the
        // table.
        const {
            assert!(PROBE_DEPTH > 62, "the text table ends at 62 and the probe stops too early")
        };
    }

    #[test]
    fn every_index_is_recorded_including_the_refusals() {
        // A log that printed only successes would be empty on the run that matters most. The entry
        // count is the depth, not the fill count.
        let p = Skinprobe::new();
        for t in &p.tables {
            assert_eq!(t.entries.len(), PROBE_DEPTH as usize);
        }
    }

    #[test]
    fn the_tables_cycle_in_both_directions_and_come_back() {
        let mut p = Skinprobe::new();
        for _ in 0..TABLES.len() {
            p.handle_key_right();
        }
        assert_eq!(p.showing, 0);
        p.handle_key_left();
        assert_eq!(p.showing, TABLES.len() - 1, "and wraps backwards rather than underflowing");
    }

    #[test]
    fn scrolling_cannot_run_off_either_end() {
        let mut p = Skinprobe::new();
        for _ in 0..PROBE_DEPTH * 2 {
            p.scroll_down();
        }
        assert_eq!(p.scroll, PROBE_DEPTH as usize - 1);
        for _ in 0..PROBE_DEPTH * 2 {
            p.scroll_up();
        }
        assert_eq!(p.scroll, 0);
    }

    #[test]
    fn changing_table_returns_to_the_top() {
        // Otherwise a table with fewer entries than the scroll position shows an empty screen and reads
        // as a table with nothing in it.
        let mut p = Skinprobe::new();
        for _ in 0..10 {
            p.scroll_down();
        }
        p.handle_key_right();
        assert_eq!(p.scroll, 0);
    }

    #[test]
    fn it_draws_without_panicking_when_nothing_was_readable() {
        // The whole screen, on the run where every index refused — which is exactly the run a probe
        // must survive to report anything at all.
        let mut p = Skinprobe::new();
        symbian_ui::testing::with_canvas(symbian_gfx::Size::new(320, 240), |c| {
            symbian_ui::testing::with_theme(symbian_ui::Palette::DARK, |t| p.draw(c, t));
        });
    }

    // Small helpers so the tests press keys rather than reaching into fields.
    impl Skinprobe {
        fn press(&mut self, k: Key) {
            symbian_ui::testing::with_theme(symbian_ui::Palette::DARK, |t| {
                self.handle_key(KeyEvent::new(k), t, symbian_ui::testing::SCREEN);
            });
        }
        fn handle_key_right(&mut self) {
            self.press(Key::Right)
        }
        fn handle_key_left(&mut self) {
            self.press(Key::Left)
        }
        fn scroll_down(&mut self) {
            self.press(Key::Down)
        }
        fn scroll_up(&mut self) {
            self.press(Key::Up)
        }
    }
}
