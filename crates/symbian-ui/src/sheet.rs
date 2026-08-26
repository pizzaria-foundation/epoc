//! One thing, with its facts laid out and its actions at the bottom.
//!
//! ## The screen this exists to replace
//!
//! Every fact about a package used to live in a row label and a menu:
//!
//! ```text
//! Launcher  0.1.0 → 0.2.0  ⚠           ← version, offer, and a warning, in 40 columns
//!   Options ▸ Install… / Pin / Reopen after install: 45 s
//! ```
//!
//! Which meant the size, the origin, the digest and what would happen on failure had nowhere to be,
//! and the row grew a new symbol each time one was needed. A person about to replace the binary of
//! their home screen was reading punctuation.
//!
//! A sheet is the other half of a list: the list answers *which one*, the sheet answers *what is it
//! and what can I do*. Label-and-value rows, a scrollbar when it does not fit, and the actions named
//! in words at the bottom.
//!
//! ## What it does not do
//!
//! It does not own its data and it does not decide its actions. It is handed [`Row`]s and action
//! labels, and it reports which action was chosen — so it stays testable and every screen using it
//! keeps its own vocabulary. It is a layout, not a controller.

use alloc::string::String;
use alloc::vec::Vec;

use crate::chip::Chip;
use crate::input::{Handled, Key, KeyEvent, Softkey};
use crate::list::ListState;
use crate::theme::Theme;
use crate::chrome;
use symbian_gfx::{Align, Canvas, Rect};

/// One line of the sheet.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Row {
    /// A label on the left, a value on the right. The ordinary line.
    Pair(String, String),
    /// A label with a state, so `origin: not verified` can be *seen* rather than read.
    Chip(String, String, crate::chip::Tone),
    /// A run of prose, wrapped. For the sentence that says what will happen — the one thing on this
    /// screen that is not a fact but a consequence.
    Note(String),
    /// Space, for grouping. Cheaper than a heading and does the same work.
    Gap,
}

impl Row {
    pub fn pair(label: impl Into<String>, value: impl Into<String>) -> Self {
        Row::Pair(label.into(), value.into())
    }

    pub fn chip(label: impl Into<String>, value: impl Into<String>, tone: crate::chip::Tone) -> Self {
        Row::Chip(label.into(), value.into(), tone)
    }

    pub fn note(text: impl Into<String>) -> Self {
        Row::Note(text.into())
    }

    fn height(&self, theme: &Theme<'_>, width: i32) -> i32 {
        match self {
            Row::Pair(..) | Row::Chip(..) => theme.fonts.body.line_height() + 4,
            Row::Note(t) => {
                let lines = wrap(t, theme, width).len().max(1) as i32;
                lines * theme.fonts.small.line_height() + 4
            }
            Row::Gap => theme.metrics.pad,
        }
    }
}

/// What the user did.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum SheetAction {
    /// The action at this index was chosen.
    Chose(usize),
    /// They backed out.
    Back,
    None,
}

/// A detail view of one thing.
pub struct Sheet {
    title: String,
    subtitle: String,
    rows: Vec<Row>,
    /// The actions, in words. Never more than a handful: a sheet that needs eight is two sheets.
    actions: Vec<String>,
    focus: usize,
    scroll: ListState,
}

impl Sheet {
    pub fn new(title: impl Into<String>, subtitle: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            subtitle: subtitle.into(),
            rows: Vec::new(),
            actions: Vec::new(),
            focus: 0,
            scroll: ListState::new(),
        }
    }

    pub fn row(mut self, row: Row) -> Self {
        self.rows.push(row);
        self
    }

    pub fn rows(mut self, rows: impl IntoIterator<Item = Row>) -> Self {
        self.rows.extend(rows);
        self
    }

    /// Add an action. The order is the order they appear, and the first is the one focused — so the
    /// most likely thing should be first.
    pub fn action(mut self, label: impl Into<String>) -> Self {
        self.actions.push(label.into());
        self
    }

    pub fn action_count(&self) -> usize {
        self.actions.len()
    }

    /// The focused action's label, for the softkey.
    ///
    /// The softkey shows what the centre key will do, which is the S60 convention and the reason a
    /// sheet needs no on-screen buttons: the action row is the label, and the key is the button.
    pub fn action_label(&self) -> Option<&str> {
        self.actions.get(self.focus).map(|s| s.as_str())
    }

    /// Where the cursor is: the focused action, and how far the facts are scrolled.
    ///
    /// Both are consequences of having looked at this sheet rather than facts about the thing it
    /// describes, so an imperative screen keeps one `Sheet` for as long as it is showing and never
    /// needs these. A *declarative* one cannot: `symbian_decl_ui::widgets::DetailSheet` is rebuilt
    /// from the model on every frame, and a sheet rebuilt from scratch starts on action zero at the
    /// top of the facts.
    ///
    /// The defect that costs is small and infuriating: the packages sheet's `Available` row is
    /// filled in by a poll that lands seconds after the sheet opens, so a finger already resting on
    /// `Pin` would find itself on `Install` the moment the version arrived — with the softkey still
    /// reading what the *last* frame focused. Reading the pair out and putting it back is what makes
    /// a rebuilt sheet the same sheet.
    pub fn cursor(&self) -> (usize, usize) {
        (self.focus, self.scroll.selected)
    }

    /// Put the cursor back where [`Sheet::cursor`] found it.
    ///
    /// `focus` is clamped to the actions this sheet actually has, because the two calls straddle a
    /// rebuild and the rebuild is free to hand over a shorter list — a package that finished
    /// installing loses its `Install` action. Clamping here rather than at the call site keeps the
    /// invariant `focus < actions.len()` in the one file that depends on it; without it `draw`
    /// would highlight nothing and `action_label` would report `None` for a sheet that visibly has
    /// actions on it.
    pub fn set_cursor(&mut self, focus: usize, scroll: usize) {
        self.focus = focus.min(self.actions.len().saturating_sub(1));
        self.scroll.selected = scroll;
    }

    pub fn handle_key(&mut self, ev: KeyEvent) -> (Handled, SheetAction) {
        match ev.key {
            Key::Softkey(Softkey::Right) => (Handled::Consumed, SheetAction::Back),
            Key::Select | Key::Softkey(Softkey::Left) => {
                if self.actions.is_empty() {
                    // A sheet with nothing to do is a sheet to read, and Select then means "I am
                    // done reading" rather than nothing at all.
                    return (Handled::Consumed, SheetAction::Back);
                }
                (Handled::Consumed, SheetAction::Chose(self.focus))
            }
            // Up and Down step between actions when there is more than one, and scroll the facts
            // otherwise. Two jobs on one pair of keys, and the rule is which of them can act.
            Key::Down => {
                if self.actions.len() > 1 {
                    self.focus = (self.focus + 1).min(self.actions.len() - 1);
                } else {
                    self.scroll.selected = self.scroll.selected.saturating_add(1);
                }
                (Handled::Consumed, SheetAction::None)
            }
            Key::Up => {
                if self.actions.len() > 1 {
                    self.focus = self.focus.saturating_sub(1);
                } else {
                    self.scroll.selected = self.scroll.selected.saturating_sub(1);
                }
                (Handled::Consumed, SheetAction::None)
            }
            _ => (Handled::Ignored, SheetAction::None),
        }
    }

    pub fn draw(&mut self, c: &mut Canvas<'_>, screen: Rect, theme: &Theme<'_>) {
        let p = &theme.palette;
        let f = chrome::Frame::split(screen, theme, true, true);
        chrome::clear(c, theme);
        chrome::title_bar(
            c,
            f.title,
            theme,
            &self.title,
            (!self.subtitle.is_empty()).then_some(self.subtitle.as_str()),
        );

        // The actions sit at the bottom of the content, above the softkey bar, so a sheet whose facts
        // scroll never scrolls its actions out of reach.
        let action_h = if self.actions.is_empty() {
            0
        } else {
            self.actions.len() as i32 * (theme.fonts.body.line_height() + 4) + theme.metrics.pad
        };
        let (facts, actions) = f.content.split_top(f.content.height() - action_h);

        let inner = facts.inset_xy(theme.metrics.pad, 0);
        let mut y = inner.y0 - self.scroll.selected as i32 * theme.fonts.body.line_height();
        let saved = c.save();
        c.clip_to(facts);
        for row in &self.rows {
            let h = row.height(theme, inner.width());
            let r = Rect::from_xywh(inner.x0, y, inner.width(), h);
            y += h;
            if r.y1 < facts.y0 || r.y0 > facts.y1 {
                continue;
            }
            match row {
                Row::Pair(label, value) => {
                    c.draw_text_in(r, label, theme.fonts.body, p.dim, Align::Start);
                    c.draw_text_in(r, value, theme.fonts.body, p.text, Align::End);
                }
                Row::Chip(label, value, tone) => {
                    c.draw_text_in(r, label, theme.fonts.body, p.dim, Align::Start);
                    Chip::new(value, *tone).draw_right(c, r, theme);
                }
                Row::Note(text) => {
                    let mut ny = r.y0;
                    for line in wrap(text, theme, inner.width()) {
                        let lr = Rect::from_xywh(
                            r.x0,
                            ny,
                            r.width(),
                            theme.fonts.small.line_height(),
                        );
                        c.draw_text_in(lr, line, theme.fonts.small, p.dim, Align::Start);
                        ny += theme.fonts.small.line_height();
                    }
                }
                Row::Gap => {}
            }
        }
        c.restore(saved);

        if !self.actions.is_empty() {
            let rh = theme.fonts.body.line_height() + 4;
            let mut ay = actions.y0 + theme.metrics.pad;
            for (i, label) in self.actions.iter().enumerate() {
                let r = Rect::from_xywh(actions.x0, ay, actions.width(), rh);
                ay += rh;
                if i == self.focus {
                    chrome::selection(c, r, theme);
                }
                let col = if i == self.focus { p.selection_text } else { p.accent };
                c.draw_text_in(
                    r.inset_xy(theme.metrics.pad, 0),
                    label,
                    theme.fonts.strong,
                    col,
                    Align::Start,
                );
            }
        }

        chrome::softkey_bar(
            c,
            f.softkeys,
            theme,
            chrome::Softkeys::new(self.action_label(), None, Some("Back")),
        );
    }
}

/// Break `text` into lines that fit `width`, on spaces.
///
/// Its own function so the height and the drawing use the same answer. Measuring one way and drawing
/// another is the bug `viewer.rs` records: scrolling and painting have to use the same rectangle.
fn wrap<'a>(text: &'a str, theme: &Theme<'_>, width: i32) -> Vec<&'a str> {
    let f = theme.fonts.small;
    let mut out = Vec::new();
    if width <= 0 {
        return out;
    }
    let mut start = 0;
    let mut last_space = None;
    for (i, ch) in text.char_indices() {
        if ch == ' ' {
            last_space = Some(i);
        }
        // `char_indices` gives the byte where a character *starts*, so the slice has to end after
        // it. `start..=i` cuts inside a multi-byte character and panics — found by an em-dash in the
        // one sentence on the packages sheet that explains what an install will do.
        let end = i + ch.len_utf8();
        if f.measure(&text[start..end]) > width {
            let cut = last_space.filter(|s| *s > start).unwrap_or(i);
            out.push(text[start..cut].trim_end());
            start = if cut == i { i } else { cut + 1 };
            last_space = None;
        }
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chip::Tone;
    use crate::testing::{with_canvas, with_theme, SCREEN};
    use crate::Palette;
    use symbian_gfx::Size;

    fn sheet() -> Sheet {
        Sheet::new("Launcher", "0.1.0 \u{2192} 0.2.0")
            .row(Row::pair("Installed", "0.1.0"))
            .row(Row::pair("Available", "0.2.0"))
            .row(Row::pair("Size", "313 KB"))
            .row(Row::chip("Origin", "pizzaria/home", Tone::Fresh))
            .row(Row::Gap)
            .row(Row::note(
                "It must report 0.2.0 to count. If not, 0.1.0 comes back. The home screen closes \
                 while this installs.",
            ))
            .action("Install")
            .action("Pin")
    }

    fn press(s: &mut Sheet, k: Key) -> SheetAction {
        s.handle_key(KeyEvent::new(k)).1
    }

    fn draw(s: &mut Sheet, palette: Palette) -> alloc::vec::Vec<u16> {
        with_canvas(Size::new(SCREEN.width(), SCREEN.height()), |c| {
            with_theme(palette, |t| s.draw(c, SCREEN, t));
        })
        .1
    }

    #[test]
    fn the_softkey_says_what_the_centre_key_will_do() {
        // The S60 convention, and the reason a sheet needs no on-screen buttons.
        let mut s = sheet();
        assert_eq!(s.action_label(), Some("Install"));
        press(&mut s, Key::Down);
        assert_eq!(s.action_label(), Some("Pin"));
    }

    #[test]
    fn choosing_reports_the_index_the_caller_gave() {
        let mut s = sheet();
        assert_eq!(press(&mut s, Key::Select), SheetAction::Chose(0));
        press(&mut s, Key::Down);
        assert_eq!(press(&mut s, Key::Select), SheetAction::Chose(1));
        assert_eq!(press(&mut s, Key::Softkey(Softkey::Right)), SheetAction::Back);
    }

    #[test]
    fn the_focus_cannot_walk_off_either_end() {
        let mut s = sheet();
        for _ in 0..5 {
            press(&mut s, Key::Down);
        }
        assert_eq!(press(&mut s, Key::Select), SheetAction::Chose(1), "not past the last");
        for _ in 0..5 {
            press(&mut s, Key::Up);
        }
        assert_eq!(press(&mut s, Key::Select), SheetAction::Chose(0));
    }

    #[test]
    fn a_sheet_with_nothing_to_do_is_a_sheet_to_read() {
        // Select then means "I am done reading" rather than nothing at all — a dead centre key on a
        // screen full of text is the kind of small dishonesty that teaches people to distrust a UI.
        let mut s = Sheet::new("browser", "0.1.0").row(Row::pair("Installed", "0.1.0"));
        assert_eq!(s.action_label(), None);
        assert_eq!(press(&mut s, Key::Select), SheetAction::Back);
        draw(&mut s, Palette::DARK);
    }

    #[test]
    fn with_one_action_the_arrows_scroll_the_facts_instead() {
        // Two jobs on one pair of keys, and the rule is which of them can act.
        let mut s = Sheet::new("t", "s").row(Row::pair("a", "b")).action("Install");
        press(&mut s, Key::Down);
        assert_eq!(s.action_label(), Some("Install"), "the only action stays focused");
        assert_eq!(s.scroll.selected, 1, "and the facts moved");
        press(&mut s, Key::Up);
        assert_eq!(s.scroll.selected, 0);
        press(&mut s, Key::Up);
        assert_eq!(s.scroll.selected, 0, "and it does not go negative");
    }

    #[test]
    fn it_draws_in_both_palettes() {
        for palette in [Palette::DARK, Palette::LIGHT] {
            let mut s = sheet();
            let px = draw(&mut s, palette);
            let blank = with_canvas(Size::new(SCREEN.width(), SCREEN.height()), |_| {}).1;
            assert_ne!(px, blank, "{palette:?}");
        }
    }

    #[test]
    fn the_actions_stay_put_while_the_facts_scroll() {
        // A sheet whose facts scroll must never scroll its actions out of reach: the action row is
        // the button, and a button that leaves the screen is a dead end.
        let mut s = Sheet::new("t", "s").action("Install");
        for _ in 0..40 {
            s = s.row(Row::pair("label", "value"));
        }
        let mut s = s.action("Pin");
        for _ in 0..30 {
            press(&mut s, Key::Down);
        }
        assert!(s.action_label().is_some());
        draw(&mut s, Palette::DARK);
    }

    #[test]
    fn a_note_wraps_and_the_height_agrees_with_the_drawing() {
        // Measuring one way and drawing another is the bug `viewer.rs` records: scrolling and
        // painting have to use the same rectangle.
        with_theme(Palette::DARK, |t| {
            let long = Row::note(
                "It must report 0.2.0 to count. If not, 0.1.0 comes back. The home screen closes \
                 while this installs.",
            );
            let narrow = long.height(t, 100);
            let wide = long.height(t, 300);
            assert!(narrow > wide, "a narrower column needs more lines");

            let lines = wrap("one two three four five six seven eight", t, 60);
            assert!(lines.len() > 1);
            assert!(lines.iter().all(|l| t.fonts.small.measure(l) <= 60), "{lines:?}");
        });
    }

    #[test]
    fn a_multibyte_character_does_not_split_a_slice() {
        // `char_indices` gives where a character starts, not where it ends, and slicing to the wrong
        // one panics. An em-dash in the sentence explaining an install found this.
        with_theme(Palette::DARK, |t| {
            let lines = wrap(
                "It will be installed and left alone \u{2014} not reopened, and not verified.",
                t,
                80,
            );
            assert!(lines.len() > 1);
            assert!(wrap("acentuação é ótimo para achar bug \u{2014} sim", t, 40).len() > 1);
        });
    }

    #[test]
    fn a_word_longer_than_the_column_is_broken_rather_than_lost() {
        with_theme(Palette::DARK, |t| {
            let lines = wrap("https://github.com/pizzaria-foundation/home/releases", t, 40);
            assert!(!lines.is_empty());
            assert!(lines.iter().all(|l| !l.is_empty()));
        });
    }

    #[test]
    fn the_cursor_can_be_carried_across_a_rebuild_and_clamps_to_a_shorter_offer() {
        // What `DetailSheet` needs: a sheet rebuilt from the model every frame must not lose the
        // action a finger is resting on when a poll fills in a row.
        let mut s = sheet();
        press(&mut s, Key::Down);
        assert_eq!(s.cursor(), (1, 0));
        let mut rebuilt = sheet();
        rebuilt.set_cursor(1, 0);
        assert_eq!(rebuilt.action_label(), Some("Pin"), "the rebuild lost the focus");
        assert_eq!(press(&mut rebuilt, Key::Select), SheetAction::Chose(1));

        // And the rebuild is free to be shorter — a package that finished installing has one fewer
        // action. An unclamped focus would highlight nothing and report `None` for a sheet that
        // visibly has an action on it.
        let mut shorter = Sheet::new("Launcher", "0.2.0").action("Pin");
        shorter.set_cursor(1, 0);
        assert_eq!(shorter.action_label(), Some("Pin"));
        assert_eq!(shorter.cursor(), (0, 0));
        // A sheet with no actions at all clamps to zero rather than underflowing.
        let mut none = Sheet::new("t", "s");
        none.set_cursor(3, 7);
        assert_eq!(none.cursor(), (0, 7));
    }

    #[test]
    fn a_screen_with_no_room_does_not_panic() {
        let mut s = sheet();
        with_canvas(Size::new(40, 30), |c| {
            with_theme(Palette::DARK, |t| s.draw(c, Rect::from_xywh(0, 0, 40, 30), t));
        });
    }
}
