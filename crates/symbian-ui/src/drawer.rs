//! A side drawer of sections: the top of an application's hierarchy, and the way back to it.
//!
//! ## The hierarchy this replaces
//!
//! The boot manager had five tabs in one strip, and one of them was a lie:
//!
//! ```text
//! [List | Entry | Setup | Boot | Pkgs]
//!         ^^^^^                  ^^^^
//!         the detail of a row     a door to four more tabs
//!         selected on List
//! ```
//!
//! **`Entry` meant "the row you selected on another tab".** A sibling that is really a child cannot be
//! guessed at, and no label fixes it. `Pkgs` was the opposite problem: a whole second subject wedged
//! into a strip that also carried restart policy, which is how it ended up with four tabs of its own
//! behind a door.
//!
//! So: sections at the top, tabs *inside* a section, and details as sheets. Three levels, each of
//! which answers a different kind of question:
//!
//! ```text
//! Drawer          Boot · Packages · Settings        which subject
//!   └─ Tabs       Order · Last boot                 which view of it
//!        └─ Sheet one entry, its facts, its actions  which thing, and what to do
//! ```
//!
//! ## It has no production caller any more
//!
//! `my-epoc` was the one, and it replaced this with a **root list** — a place you go rather than a
//! panel that covers where you are. The argument was not that a drawer is bad: it is that a drawer
//! is a *touch* idiom, and the one advantage claimed below ("nothing else on the screen moves when
//! it opens") is about animation, on a device that cannot animate it. A list also has one way to
//! close where this has two, and telling those two apart is what [`DrawerAction::Dismissed`] and
//! [`DrawerAction::WentUp`] exist for — after the conflation shipped an application that quit when
//! you pressed `Options`.
//!
//! This stays because it is still the right shape for an app that wants a navigator *over* a screen
//! rather than instead of it, and because `apps/uigallery` demonstrates it. But nothing ships it, so
//! nobody is holding a phone with it — worth knowing before trusting it.
//!
//! ## What a drawer is, and is not
//!
//! It is a list that slides over the screen from the left, marks where you are, and closes on Back
//! without changing anything. It is **not** a menu: a menu offers verbs for the thing in front of you
//! and closes when one is chosen; this offers places, and where you are is part of what it shows.
//!
//! Nothing else on the screen moves when it opens, because a navigator that reflows the thing being
//! navigated is a navigator that hides its own effect.
//!
//! ## Which key opens it is the caller's, and this file used to guess wrong
//!
//! It said "opened from the left softkey, which the SDK's own convention already calls the options
//! slot". `my-epoc` opens it from **Back**, in all three of its sections, and wrote down why:
//! *"Back walks up, and above the boot list is the navigator — which is how the navigator is reached
//! at all. The first version put it on the left softkey and then exempted this section, because Move
//! and Reset were on that key first: the result was an application that started in the one place
//! with no way out of it."*
//!
//! So the opening key is not this widget's to name. What it does own is the **closing**, and there
//! the two softkeys mean different things — see [`DrawerAction::Dismissed`] and
//! [`DrawerAction::WentUp`], which used to be one variant and cost an application that quit when you
//! pressed `Options`.

use alloc::string::String;

use crate::input::{Handled, Key, KeyEvent, Softkey};
use crate::list::{ListState, Uniform};
use crate::theme::Theme;
use crate::{chrome, paint};
use symbian_gfx::{Align, Canvas, Rect};

/// What the user did with the drawer.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum DrawerAction {
    /// Go to the section at this index.
    Went(usize),
    /// Closed without changing anything, and the caller should stay where it was.
    ///
    /// Distinct from [`WentUp`](Self::WentUp) because the two are not the same gesture, and treating
    /// them as one cost a real defect: the host mapped "dismissed" onto "no target" and "no target"
    /// onto *exit the application*, so the key labelled `Options` — or `Move`, or `Reset safe mode`,
    /// depending on what was behind the drawer — quietly closed the app. A person pressing the
    /// options key twice would lose whatever they were doing, and nothing on screen said why.
    Dismissed,
    /// Backed out of the drawer itself, which is one level *up* from wherever the caller is.
    ///
    /// At the top of an application that means leaving it, and that is the caller's decision rather
    /// than this widget's — the same argument `symbian_decl_ui`'s bridge makes for `Cmd::PopScreen`:
    /// *popping the last screen is not exiting*, because a widget that guessed would close an
    /// application on the second of two quick presses, and the second press is the one a person
    /// makes when the first seemed not to take.
    WentUp,
    None,
}

/// One place the application can be.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Section {
    pub label: String,
    /// A short word under the label: what is there, or what is happening there. Optional, and worth
    /// having only when it saves opening the section to find out — `3 in the queue` does, `packages`
    /// does not.
    pub note: String,
}

impl Section {
    pub fn new(label: impl Into<String>) -> Self {
        Self { label: label.into(), note: String::new() }
    }

    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.note = note.into();
        self
    }
}

/// The side drawer.
pub struct Drawer {
    list: ListState,
    /// Where the application currently is, marked so the drawer says where you are as well as where
    /// you could go.
    current: usize,
}

impl Drawer {
    /// Open it, with the cursor on the section the application is in.
    ///
    /// Opening on the current section rather than on the first is the difference between a navigator
    /// and a list: the first thing somebody wants to know when they open this is where they are.
    pub fn open(current: usize) -> Self {
        let mut list = ListState::new();
        list.selected = current;
        Self { list, current }
    }

    pub fn selected(&self) -> usize {
        self.list.selected
    }

    /// How wide the panel is. Two thirds of the screen: wide enough for a label and a note, narrow
    /// enough that the screen behind it is still visibly there — which is what says this is a layer
    /// and not a new place.
    pub fn width(screen: Rect) -> i32 {
        // `min` after `max`, not `clamp`: on a screen narrower than the floor the two bounds cross,
        // and `clamp` panics when they do. A degenerate screen is a test harness, not a phone, and it
        // should get a narrow drawer rather than an abort.
        (screen.width() * 2 / 3).max(96).min(screen.width())
    }

    pub fn handle_key(
        &mut self,
        ev: KeyEvent,
        sections: &[Section],
        theme: &Theme<'_>,
        screen: Rect,
    ) -> (Handled, DrawerAction) {
        // Both softkeys close it — the options key because the key that opened it closing it again
        // is what everybody tries first, and a drawer that ignored its own key would be a drawer
        // people learn to distrust.
        //
        // But they close it *differently*, and that distinction is the whole point of the two
        // variants. Back means "up a level", which at the top of an application means leaving it.
        // The options key means "never mind" and must land you back where you were. They used to
        // report the same thing, and the host could only read it one way — so `Options` closed the
        // application.
        if let Key::Softkey(Softkey::Right) | Key::End = ev.key {
            return (Handled::Consumed, DrawerAction::WentUp);
        }
        if let Key::Softkey(Softkey::Left) = ev.key {
            return (Handled::Consumed, DrawerAction::Dismissed);
        }
        // The two horizontal keys are answered **before** the list sees them. `ListState` consumes
        // Left and Right for its own paging, so a drawer that offered the list first would swallow
        // the two gestures a thumb reaches for on a side panel and answer neither.
        match ev.key {
            Key::Select | Key::Right => {
                return (Handled::Consumed, DrawerAction::Went(self.list.selected))
            }
            // Left is "close this panel" on a left-hand drawer, and it is the one a thumb finds
            // without being told.
            Key::Left => return (Handled::Consumed, DrawerAction::Dismissed),
            _ => {}
        }
        let rows = Uniform { count: sections.len(), height: Self::row_height(theme) };
        let area = Self::panel(screen, theme);
        if self.list.handle_key(ev, &rows, area.height()) == Handled::Consumed {
            return (Handled::Consumed, DrawerAction::None);
        }
        // Modal: nothing behind it may act while it is open, or the screen would answer a question
        // nobody asked. Every key that reaches here is consumed and reports nothing.
        (Handled::Consumed, DrawerAction::None)
    }

    fn row_height(theme: &Theme<'_>) -> i32 {
        theme.fonts.body.line_height() + theme.fonts.small.line_height() + 6
    }

    fn panel(screen: Rect, theme: &Theme<'_>) -> Rect {
        let f = chrome::Frame::split(screen, theme, true, true);
        let (panel, _) = f.content.split_left(Self::width(screen));
        panel
    }

    pub fn draw(
        &mut self,
        c: &mut Canvas<'_>,
        screen: Rect,
        theme: &Theme<'_>,
        sections: &[Section],
    ) {
        let p = &theme.palette;
        let area = Self::panel(screen, theme);
        paint::band(c, area, &p.chrome);
        // One hairline down the open edge, so the drawer has a boundary rather than merging into the
        // screen it is covering.
        c.vline(area.x1, area.y0, area.y1, p.divider);

        let rh = Self::row_height(theme);
        let rows = Uniform { count: sections.len(), height: rh };
        let sel = self.list.selected;
        let current = self.current;
        let pad = theme.metrics.pad;
        self.list.draw_visible(c, &rows, area, |c, i, row| {
            let s = &sections[i];
            if i == sel {
                chrome::selection(c, row, theme);
            }
            let fg = if i == sel { p.selection_text } else { p.chrome_text };
            let cell = row.inset_xy(pad, 2);
            let (first, rest) = cell.split_top(theme.fonts.body.line_height());

            // The section the application is actually in is marked, because a navigator that only
            // says where you could go is half a navigator.
            let label = if i == current {
                alloc::format!("\u{2022} {}", s.label)
            } else {
                alloc::format!("  {}", s.label)
            };
            let font = if i == current { theme.fonts.strong } else { theme.fonts.body };
            c.draw_text_in(first, &label, font, fg, Align::Start);
            if !s.note.is_empty() {
                let dim = if i == sel { p.selection_text } else { p.dim };
                c.draw_text_in(rest, &s.note, theme.fonts.small, dim, Align::Start);
            }
        });
        chrome::scrollbar(c, area, theme, self.list.scrollbar(&rows, area.height()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{with_canvas, with_theme, SCREEN};
    use crate::Palette;
    use symbian_gfx::Size;

    fn sections() -> Vec<Section> {
        alloc::vec![
            Section::new("Boot").note("4 at boot"),
            Section::new("Packages").note("2 in the queue"),
            Section::new("Settings"),
        ]
    }

    fn press(d: &mut Drawer, k: Key) -> DrawerAction {
        with_theme(Palette::DARK, |t| d.handle_key(KeyEvent::new(k), &sections(), t, SCREEN).1)
    }

    #[test]
    fn it_opens_on_the_section_the_application_is_in() {
        // The first thing somebody wants to know when they open a navigator is where they are.
        let d = Drawer::open(1);
        assert_eq!(d.selected(), 1);
    }

    #[test]
    fn choosing_reports_the_section() {
        let mut d = Drawer::open(0);
        press(&mut d, Key::Down);
        assert_eq!(press(&mut d, Key::Select), DrawerAction::Went(1));
    }

    #[test]
    fn right_goes_in_and_left_closes() {
        // The gestures a thumb finds on a left-hand panel without being told.
        let mut d = Drawer::open(2);
        assert_eq!(press(&mut d, Key::Right), DrawerAction::Went(2));
        assert_eq!(press(&mut d, Key::Left), DrawerAction::Dismissed);
    }

    #[test]
    fn the_key_that_opened_it_closes_it_but_the_two_softkeys_do_not_mean_the_same_thing() {
        // Both close it — everybody tries the opening key first, and a drawer that ignored its own
        // key is one people learn to distrust. What changed is that they no longer report the same
        // thing, and this test used to be where that conflation was written down as if it were the
        // contract.
        //
        // It was not: the host read the single answer as "went nowhere" and turned that into *exit
        // the application*, so the key labelled `Options` closed the app. `Dismissed` means stay,
        // `WentUp` means the caller decides what is above — see the variants' own docs.
        let mut d = Drawer::open(0);
        assert_eq!(press(&mut d, Key::Softkey(Softkey::Left)), DrawerAction::Dismissed);
        assert_eq!(press(&mut d, Key::Softkey(Softkey::Right)), DrawerAction::WentUp);
        assert_eq!(press(&mut d, Key::End), DrawerAction::WentUp, "the red key is Back");
        assert_eq!(press(&mut d, Key::Left), DrawerAction::Dismissed, "and so is a thumb's Left");
    }

    #[test]
    fn it_is_modal_while_it_is_open() {
        // Nothing behind it may act, or the screen answers a question nobody asked.
        let mut d = Drawer::open(0);
        with_theme(Palette::DARK, |t| {
            let (h, a) = d.handle_key(KeyEvent::new(Key::Char('x')), &sections(), t, SCREEN);
            assert_eq!(h, Handled::Consumed);
            assert_eq!(a, DrawerAction::None);
        });
    }

    #[test]
    fn the_cursor_cannot_walk_off_either_end() {
        let mut d = Drawer::open(0);
        for _ in 0..8 {
            press(&mut d, Key::Down);
        }
        assert_eq!(press(&mut d, Key::Select), DrawerAction::Went(2));
        for _ in 0..8 {
            press(&mut d, Key::Up);
        }
        assert_eq!(press(&mut d, Key::Select), DrawerAction::Went(0));
    }

    #[test]
    fn the_panel_leaves_the_screen_behind_it_visible() {
        // Which is what says this is a layer and not a new place.
        let w = Drawer::width(SCREEN);
        assert!(w < SCREEN.width(), "a full-width drawer is just another screen");
        assert!(w > SCREEN.width() / 2, "and a narrow one cannot hold a label and a note");
    }

    #[test]
    fn it_draws_in_both_palettes_and_marks_where_you_are() {
        for palette in [Palette::DARK, Palette::LIGHT] {
            let mut d = Drawer::open(1);
            let (_, px) = with_canvas(Size::new(SCREEN.width(), SCREEN.height()), |c| {
                with_theme(palette, |t| d.draw(c, SCREEN, t, &sections()));
            });
            let blank = with_canvas(Size::new(SCREEN.width(), SCREEN.height()), |_| {}).1;
            assert_ne!(px, blank, "{palette:?}");
        }
    }

    #[test]
    fn an_empty_drawer_and_a_tiny_screen_do_not_panic() {
        let mut d = Drawer::open(0);
        with_canvas(Size::new(40, 30), |c| {
            with_theme(Palette::DARK, |t| {
                d.draw(c, Rect::from_xywh(0, 0, 40, 30), t, &[]);
                d.draw(c, Rect::from_xywh(0, 0, 40, 30), t, &sections());
            });
        });
    }
}
