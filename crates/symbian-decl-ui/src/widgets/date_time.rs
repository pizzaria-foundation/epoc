//! A date or a time, as the row of `‹ N ›` spinners the D-pad can actually drive.
//!
//! ```ignore
//! FieldRow::new("starts")
//!     .control(
//!         DateTime::date(slots, model.starts)
//!             .focused(here)
//!             .labels(Part::Month, &model.month_names)      // the caller's words, never ours
//!             .out(out.clone(), Msg::SetStart),
//!     )
//!     .focused(here)
//!     .build()
//! ```
//!
//! # Three [`Stepper`]s and no arithmetic
//!
//! Each field is the catalogue's [`Stepper`] block — the same 46 pixels, the same chevrons, the same
//! `symbian_ui::stepper` free functions — so a day field and a retry count are the same object on
//! screen, and a change to one moves both. What this widget adds is only the two things a single
//! stepper cannot know: **which spinner has the cursor**, and **what the whole date becomes when one
//! of them moves**.
//!
//! The second is the interesting one and it is not here either. It is
//! [`symbian_ui::calendar::Stamp`], which cannot hold 31 February: the widget reports
//! `stamp.with_part(part, value)` and the clamp happens inside that call, in a `no_std` module with
//! its own tests. See that module for why the day is clamped rather than the month step refused —
//! and note the consequence for this file, which is that **there is no branch below about
//! February**. A picker that grew one would be a second calendar, and the first thing to disagree
//! with it would be the sync that writes the same dates from the network.
//!
//! # The axes are swapped, deliberately
//!
//! ```text
//!    Left / Right    change the value under the cursor      ‹ 28 ›  ‹ 2 ›  ‹ 2024 ›
//!    Up / Down       move the cursor to the next field       ↑            the cursor
//!    Select          nothing — it belongs to the screen
//! ```
//!
//! The handset's own date editor is the other way round: Left/Right walk the fields and Up/Down spin
//! the numbers. This one is inverted because a field here *is* a [`Stepper`], and every stepper in
//! this SDK is driven with Left/Right — a settings screen where `‹ 3 ›` answers Left and a date
//! screen where the identical-looking `‹ 3 ›` ignores it is worse than either convention, because
//! nothing on screen says which kind of block you are looking at.
//!
//! Having given Left and Right away, the cursor needs a key, and the D-pad has exactly one pair
//! left. **`Select` was considered and rejected**: the crate's own convention makes the D-pad centre
//! *the screen's action* — [`OnKey`](super::OnKey) refuses to let a widget bind it at all — so a
//! picker that advanced its cursor on `Select` would shadow the Save the softkey bar is advertising,
//! which is the label-lies-about-the-key defect in a new costume. `Select` is therefore declined
//! here (`Handled::Ignored`), which is a real difference from [`Stepper`], where `Select` steps the
//! value with a wrap. That wrap exists for tab-strip screens that have no Left and Right to give;
//! this widget is unusable on such a screen anyway, since its values need them, so the wrap buys
//! nothing and costs the action key.
//!
//! # Up and Down at the ends: [`EdgePolicy::Escape`]
//!
//! Taking the vertical keys is what makes this widget dangerous, because vertical keys are how every
//! enclosing thing in the crate moves *its* cursor: a [`FocusScope`](super::FocusScope) form, a
//! [`ScrollList`](super::ScrollList) of rows. A picker that consumed every `Down` would be the row
//! nobody can get past — the trap [`Switch`](super::Switch) and [`Stepper`] both document, arriving
//! here through the one key they were careful to leave alone.
//!
//! So the default is [`EdgePolicy::Escape`]: the vertical keys move the cursor while there is
//! another field to move to, and the press that runs off the end is **handed back**, so the form
//! outside moves on. The cost is that a three-field date is three stops of the form's own cursor
//! rather than one, and that is the honest price of putting three independent spinners on one line —
//! there are three things to point at.
//!
//! It is a parameter because the other two are right somewhere:
//!
//! - [`EdgePolicy::Wrap`] for a **full-screen picker**, where nothing encloses the widget and the
//!   only way out is a softkey. There, `Escape` means `Up` on the first field does nothing at all,
//!   and cycling day → month → year → day is what the user expects of three fields on their own.
//! - [`EdgePolicy::Stop`] where something *else* on the screen owns the vertical keys and must not
//!   see them — the same reason [`crate::widgets::Grid`] and a list consume a clamped arrow.
//!
//! Note which edge that policy is about. The group's **Left/Right** edges are the *values'* ends,
//! and they belong to the field: a day at 1 consumes `Left` and stays, which is
//! `symbian_ui::Stepper`'s answer and is what stops the cursor sliding sideways off a field that is
//! merely at its limit. The policy here governs only the cursor axis.
//!
//! # A day field's maximum is not 31
//!
//! It is [`Stamp::bounds`], which asks the calendar — so in February the day field simply cannot be
//! stepped to 29, and there is no state in which the user is looking at a number the model will
//! silently move. This is the half of the invalid-date problem that clamping in `update` cannot
//! solve on its own: a picker that offered 31 and let the model quietly correct it would move a
//! value under a cursor that did not move.
//!
//! # No text, including the month
//!
//! This crate ships none, and a month name is text — see `symbian_ui::toggle`'s note, which says so
//! for a switch's caption. [`labels`](DateTime::labels) takes the names from the caller and the
//! block is sized to the **longest** of them by
//! [`chevron_width`](symbian_ui::stepper::chevron_width), so stepping May → September does not
//! shuffle the year field sideways. With no labels a field shows its number, which is a perfectly
//! good month picker and the only one this crate could ship on its own.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::Cell;

use symbian_gfx::{Canvas, Rect, Size};
use symbian_ui::calendar::{Part, Stamp};
use symbian_ui::focus::{EdgePolicy, FocusAxis, FocusRing};
use symbian_ui::{stepper as ui, Handled, Key, KeyEvent, Theme};

use crate::constraints::Constraints;
use crate::outbox::Outbox;
use crate::slot::SlotTable;
use crate::spacing::Gap;
use crate::widget::{hash_i32, hash_str, KeyCtx, Widget, WidgetHash};
use crate::widgets::Stepper;

/// Where a new stamp goes, and how to name it once it gets there.
///
/// The pair is spelled twice — the field and [`DateTime::out`] — and
/// `Option<(Outbox<M>, fn(Stamp) -> M)>` in a struct definition reads as machinery rather than as
/// the one thing it is.
type Report<M> = (Outbox<M>, fn(Stamp) -> M);

/// How many spinners one picker can hold: year, month, day, hour, minute.
///
/// A fixed array rather than a `Vec`, because a `view` is rebuilt every frame and a heap allocation
/// per picker per frame to hold five two-byte enums is the cost this crate keeps counting. Five is
/// not a limit anybody will reach — it is every field [`Part`] has.
const MAX_FIELDS: usize = 5;

/// A row of spinners over one [`Stamp`], reporting whole stamps and owning nothing.
pub struct DateTime<M> {
    /// The model's value, already normalised by [`Stamp`]'s own constructors — so nothing below has
    /// to consider what a 31 February would draw as.
    stamp: Stamp,
    parts: [Part; MAX_FIELDS],
    n: usize,
    /// Per *field*, not per [`Part`]: the words a field shows instead of its number. `None` is a
    /// numeric field, which is the default and the only thing this crate can offer with no text.
    labels: [Option<Vec<String>>; MAX_FIELDS],
    /// Which field has the cursor, in the slot table so it survives the tree being rebuilt.
    ///
    /// The same choice [`FocusScope`](super::FocusScope) makes and for the same reason, down to the
    /// `Cell`: [`FocusRing`] is `Copy`, so there is no borrow flag to get wrong inside a key
    /// dispatch on a device whose whole failure report is a dialog with a number in it.
    cursor: Rc<Cell<FocusRing>>,
    /// Whether the *picker* has the enclosing form's cursor. Nothing answers a key without it.
    focused: bool,
    policy: EdgePolicy,
    gap: Gap,
    out: Option<Report<M>>,
}

impl<M: 'static> DateTime<M> {
    /// Day, month and year, in that order.
    ///
    /// Day first because that is the order this SDK's device is set to and the order the calendar it
    /// syncs with writes; a locale that disagrees passes its own order to [`parts`](Self::parts)
    /// rather than being second-guessed here. Field *order* is as much a locale's business as field
    /// *names* are, and this crate does not own either.
    pub fn date(slots: &mut SlotTable, stamp: Stamp) -> Self {
        Self::parts(slots, stamp, &[Part::Day, Part::Month, Part::Year])
    }

    /// Hour and minute — the two-field case the module title admits to.
    ///
    /// Twenty-four hours, because a twelve-hour field needs an am/pm control and "am" is text.
    pub fn time(slots: &mut SlotTable, stamp: Stamp) -> Self {
        Self::parts(slots, stamp, &[Part::Hour, Part::Minute])
    }

    /// Whichever fields, in whichever order — a month-and-year picker, a time with the hour last.
    ///
    /// More than [`MAX_FIELDS`] parts are dropped rather than panicked on: this is called from
    /// `view`, and a panic there is a dead application on a phone with no console. A repeated part
    /// is honoured as two spinners over the same number, which is silly and harmless — both show
    /// the model's value, so they cannot disagree.
    pub fn parts(slots: &mut SlotTable, stamp: Stamp, parts: &[Part]) -> Self {
        let cursor = slots.use_state_with(|| Rc::new(Cell::new(FocusRing::new()))).clone();
        let mut fields = [Part::Year; MAX_FIELDS];
        let n = parts.len().min(MAX_FIELDS);
        fields[..n].copy_from_slice(&parts[..n]);

        // Clamped here, on the way in, for the reason `FocusScope::build` clamps: the field count
        // comes from the caller and can shrink between two frames — a picker that dropped its year
        // field would otherwise leave the cursor past the end, focusing nothing, and the symptom is
        // a screen where no key does anything.
        let mut ring = cursor.get();
        ring.clamp(n);
        cursor.set(ring);

        Self {
            stamp,
            parts: fields,
            n,
            labels: core::array::from_fn(|_| None),
            cursor,
            focused: false,
            // See the module docs: the only policy that cannot trap an enclosing form's cursor.
            policy: EdgePolicy::Escape,
            // `Tight`, because the three blocks are one control and not three: at `Base` the gaps
            // read as a gap between separate fields, and the row is 150 pixels of a 320-pixel screen
            // before the label beside it.
            gap: Gap::Tight,
            out: None,
        }
    }

    /// Whether the picker has the cursor. Only a focused picker answers a key.
    pub fn focused(mut self, on: bool) -> Self {
        self.focused = on;
        self
    }

    /// What `Up` and `Down` do at the first and last field. Defaults to [`EdgePolicy::Escape`] —
    /// see the module docs, which argue that the default is the only one safe inside a form.
    pub fn policy(mut self, policy: EdgePolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Space between the blocks.
    pub fn gap(mut self, g: impl Into<Gap>) -> Self {
        self.gap = g.into();
        self
    }

    /// Show words instead of numbers in `part`'s field — month names, most obviously.
    ///
    /// `labels[0]` is what the field's *minimum* shows, so twelve month names line up with 1..=12
    /// with no offset for the caller to get wrong. A list of the wrong length is not an error and
    /// not a blank: the value falls back to its number, because a month field showing nothing at all
    /// is unusable and a shipped app should not lose its date screen to a short array.
    ///
    /// Naming a part the picker is not showing does nothing, deliberately — a caller that passes the
    /// same month names to a date picker and a time picker should not have to branch.
    pub fn labels(mut self, part: Part, labels: &[&str]) -> Self {
        if let Some(i) = self.parts[..self.n].iter().position(|p| *p == part) {
            self.labels[i] = Some(labels.iter().map(|s| String::from(*s)).collect());
        }
        self
    }

    /// Where a new stamp goes, and how to name it.
    ///
    /// `msg` receives the **whole stamp**, already legal — not the one field that moved. That is the
    /// difference from [`Stepper::out`], which reports an `i32`, and it is the point: the day's
    /// clamp is a function of the other two fields, so a message carrying one number would push the
    /// invalid-date problem into every app's `update` and into a different branch in each.
    ///
    /// A `fn` pointer rather than a closure or an [`Outbox::wrapped`], for the reason
    /// [`Stepper`](super::Stepper)'s module docs set out at length: `wrapped` allocates an `Rc` and
    /// boxes a closure *per call*, and this is called from `view`, every frame.
    pub fn out(mut self, out: Outbox<M>, msg: fn(Stamp) -> M) -> Self {
        self.out = Some((out, msg));
        self
    }

    /// Which field has the cursor.
    ///
    /// For the caller assembling a softkey label out of the tree's own state — "Set day" over the
    /// first block — which is [`FocusScope::stops`](super::FocusScope::stops)' case, one level down.
    pub fn cursor(&self) -> usize {
        self.cursor.get().cursor()
    }

    /// Which [`Part`] the cursor is on, or `None` for a picker with no fields.
    pub fn focused_part(&self) -> Option<Part> {
        self.field(self.cursor())
    }

    /// The part at field `i`, if there is one.
    fn field(&self, i: usize) -> Option<Part> {
        (i < self.n).then(|| self.parts[i])
    }

    /// How wide field `i`'s block is.
    ///
    /// [`STEPPER_W`](symbian_ui::stepper::STEPPER_W) for a numeric field — the catalogue's own
    /// reservation, so a day block and a retry-count block are the same object — and the widest
    /// label for a labelled one. Neither depends on the *value*, which is what stops the year field
    /// moving when the month steps from May to September, and what lets
    /// [`content_hash`](Widget::content_hash) leave the stamp out.
    fn field_width(&self, i: usize, theme: &Theme<'_>) -> i32 {
        match &self.labels[i] {
            Some(words) => ui::chevron_width(theme, words.iter().map(String::as_str)),
            None => ui::STEPPER_W,
        }
    }

    /// The word field `i` shows for the value it holds, or `None` to show the number.
    fn label_at(&self, i: usize, part: Part) -> Option<&str> {
        let words = self.labels[i].as_ref()?;
        let (min, _) = self.stamp.bounds(part);
        let index = usize::try_from(self.stamp.part(part) - min).ok()?;
        words.get(index).map(String::as_str)
    }
}

impl<M: 'static> Widget for DateTime<M> {
    fn focus_state(&self) -> Option<bool> {
        Some(self.focused)
    }

    fn content_hash(&self) -> WidgetHash {
        // In: how many fields, which parts they are, and every label — because a labelled block is
        // sized to the longest word, so *all* of them move the width and not just the one showing.
        // The gap's role goes in as a role, which is `Gap::hash`'s rule.
        //
        // Out, on purpose:
        //   * the stamp. A block is the same width whatever number is in it — that is what
        //     `STEPPER_W` and `chevron_width`-of-the-longest buy — so folding the value in would
        //     re-measure the row on every keypress to produce the number it already had.
        //   * `focused` and the cursor, which choose a colour.
        //   * the policy and the outbox, which are behaviour and have no pixels at all.
        //
        // Not zero. Zero means "re-measure me every frame", and through `Group::content_hash` that
        // would put the whole enclosing form on the slow path for ever, not just this widget.
        let mut h = hash_str(0, "date_time");
        h = hash_i32(h, self.n as i32);
        for i in 0..self.n {
            h = hash_i32(h, self.parts[i] as i32);
            match &self.labels[i] {
                None => h = hash_i32(h, -1),
                Some(words) => {
                    h = hash_i32(h, words.len() as i32);
                    for w in words {
                        h = crate::widget::hash_str(h, w);
                    }
                }
            }
        }
        self.gap.hash(h)
    }

    fn measure(&self, constraints: Constraints, theme: &Theme<'_>) -> Size {
        // One line tall, never the band — `stepper_height`'s note says why, and the consequence is
        // the `Stretch` case below: a picker that reported 38 pixels would have lied to every row
        // that asked it how big it is, and to every alignment computed from that answer.
        let h = ui::stepper_height(constraints.max_h, theme);
        let gaps = self.gap.resolve(theme) * (self.n.max(1) as i32 - 1);
        let w: i32 = (0..self.n).map(|i| self.field_width(i, theme)).sum();
        constraints.constrain(Size::new(w + gaps, h))
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        // Centred in whatever band we were handed, because `CrossAlign::Stretch` on a list row hands
        // this widget the whole 38-pixel band and not the one line it measured.
        let h = ui::stepper_height(rect.height(), theme);
        let y0 = rect.y0 + (rect.height() - h) / 2;
        let gap = self.gap.resolve(theme);
        let cursor = self.cursor.get();

        let mut x = rect.x0;
        for i in 0..self.n {
            let part = self.parts[i];
            let w = self.field_width(i, theme);
            let slot = Rect::from_xywh(x, y0, w, h);
            let lit = self.focused && cursor.is_focused(i);
            match self.label_at(i, part) {
                Some(word) => ui::draw_chevrons(c, slot, theme, word, lit),
                // The catalogue's own block, drawn by the catalogue's own widget: a numeric field
                // here and a `Stepper` in a settings row cannot be two different steppers.
                None => {
                    let (min, max) = self.stamp.bounds(part);
                    Stepper::<M>::new(self.stamp.part(part), min, max).focused(lit).draw(c, slot, theme);
                }
            }
            x += w + gap;
        }
    }

    fn handle_key(&self, ev: KeyEvent, _rect: Rect, _cx: &mut KeyCtx<'_>) -> Handled {
        if !self.focused || self.n == 0 {
            // Two pickers on one form and one press: without the flag both would report a stamp, and
            // the model would keep the second one's.
            return Handled::Ignored;
        }
        match ev.key {
            Key::Up | Key::Down => {
                // The ring's own arithmetic, policy included — the wrap and the
                // consumed-but-clamped case are `symbian_ui::focus`'s to decide, and a second copy
                // here is how this widget and a `FocusScope` would come to disagree about what
                // `Down` means at the end.
                let mut ring = self.cursor.get();
                let (handled, _edge) = ring.handle_key(ev, FocusAxis::Vertical, self.n, self.policy);
                self.cursor.set(ring);
                handled
            }
            Key::Left | Key::Right => {
                let Some(part) = self.focused_part() else { return Handled::Ignored };
                let was = self.stamp.part(part);
                let (min, max) = self.stamp.bounds(part);
                // A throwaway imperative stepper does the stepping and is dropped here, exactly as
                // `Stepper::handle_key` does it: one implementation of "what does Right do at the
                // maximum" in the tree, and the bounds it clamps against came from the calendar.
                let mut probe = symbian_ui::Stepper::new(was, min, max);
                probe.handle_key(ev);
                if probe.value() != was {
                    if let Some((out, msg)) = &self.out {
                        // The clamp lives in `with_part`. Stepping the month off a 31st is where
                        // that matters, and there is deliberately no `if` about it here.
                        out.push(msg(self.stamp.with_part(part, probe.value())));
                    }
                }
                // Consumed even when nothing moved: a day sitting at 1 must not let `Left` slide the
                // cursor onto whatever is beside the picker, which is `symbian_ui::Stepper`'s answer
                // and the reason the row's navigation works at all. No message, though — an
                // `update` and a repaint per press for a value that did not move.
                Handled::Consumed
            }
            // Everything else, `Select` included. See the module docs: the D-pad centre is the
            // screen's action, and a picker that took it would shadow the softkey bar's own label.
            _ => Handled::Ignored,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout;
    use crate::widget::with_key_ctx;
    use crate::widgets::{Button, FocusScope, Node};
    use crate::UiCache;
    use symbian_ui::calendar::{YEAR_MAX, YEAR_MIN};
    use symbian_ui::{testing, Palette};

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Msg {
        Set(Stamp),
        Save,
    }

    /// Portuguese month names, from a *test*: the crate ships none, and neither does this file.
    const MESES: [&str; 12] = [
        "Jan", "Fev", "Mar", "Abr", "Mai", "Jun", "Jul", "Ago", "Set", "Out", "Nov", "Dez",
    ];

    /// A list row's band: wide enough for three blocks, and 38 pixels tall — the `Stretch` a row
    /// applies.
    const ROW: Rect = Rect { x0: 0, y0: 0, x1: 200, y1: 38 };

    fn press(w: &DateTime<Msg>, key: Key) -> Handled {
        testing::with_theme(Palette::DARK, |_t| {
            with_key_ctx(|cx| w.handle_key(KeyEvent::new(key), ROW, cx))
        })
    }

    /// A focused day/month/year picker over `stamp`, wired to an outbox.
    fn wired(slots: &mut SlotTable, stamp: Stamp) -> (Outbox<Msg>, DateTime<Msg>) {
        let out = Outbox::new();
        let w = DateTime::date(slots, stamp).focused(true).out(out.clone(), Msg::Set);
        (out, w)
    }

    /// The one stamp most of these tests start from: a 31st, so the February trap is one press away.
    fn jan31() -> Stamp {
        Stamp::date(2023, 1, 31)
    }

    /// The *real* device atlases, not the one-glyph test atlas.
    ///
    /// `testing::with_theme` loads an atlas containing exactly one glyph — lowercase 'a' — and a
    /// date field is made entirely of digits and chevrons, so under it this widget paints **nothing
    /// at all** and every pixel assertion would pass whatever `draw` did. This is the same reason
    /// [`Stepper`](super::super::Stepper)'s tests load the real fonts, and it is not a theoretical
    /// worry: it is why `the_real_atlas_paints...` below exists as a control.
    fn with_real_theme<R>(f: impl FnOnce(&Theme<'_>) -> R) -> R {
        let atlases = symbian_preview::Atlases::load();
        atlases.with_fonts(|fonts| f(&symbian_ui::Theme::dark(fonts)))
    }

    /// Paint a picker over the row band and hand back the buffer.
    fn paint(t: &Theme<'_>, w: &DateTime<Msg>) -> alloc::vec::Vec<u16> {
        let (_, buf) = testing::with_canvas(Size::new(ROW.width(), ROW.height()), |c| {
            c.clear(t.palette.bg.mid());
            w.draw(c, ROW, t);
        });
        buf
    }

    /// Which rows of a `ROW`-sized buffer have any ink in them.
    fn inked_rows(t: &Theme<'_>, buf: &[u16]) -> alloc::vec::Vec<i32> {
        let bg = t.palette.bg.mid().to_rgb565().0;
        let w = ROW.width();
        (0..ROW.height()).filter(|&y| (0..w).any(|x| buf[(y * w + x) as usize] != bg)).collect()
    }

    #[test]
    fn left_and_right_move_only_the_field_under_the_cursor() {
        // The whole promise of the cursor. Three spinners, one press, one field changed — and the
        // *other two* asserted, because a picker that rebuilt the stamp from the wrong part would
        // still look right on the field being watched.
        let mut slots = SlotTable::new();
        let (out, w) = wired(&mut slots, Stamp::date(2023, 6, 15));
        assert_eq!(press(&w, Key::Right), Handled::Consumed);
        assert_eq!(out.take(), alloc::vec![Msg::Set(Stamp::date(2023, 6, 16))]);

        press(&w, Key::Down);
        press(&w, Key::Right);
        assert_eq!(out.take(), alloc::vec![Msg::Set(Stamp::date(2023, 7, 15))]);

        press(&w, Key::Down);
        press(&w, Key::Left);
        assert_eq!(out.take(), alloc::vec![Msg::Set(Stamp::date(2022, 6, 15))]);
    }

    #[test]
    fn the_cursor_moves_down_the_fields_and_back_up_them() {
        let mut slots = SlotTable::new();
        let (_, w) = wired(&mut slots, jan31());
        assert_eq!((w.cursor(), w.focused_part()), (0, Some(Part::Day)));
        assert_eq!(press(&w, Key::Down), Handled::Consumed);
        assert_eq!(w.focused_part(), Some(Part::Month));
        assert_eq!(press(&w, Key::Down), Handled::Consumed);
        assert_eq!(w.focused_part(), Some(Part::Year));
        assert_eq!(press(&w, Key::Up), Handled::Consumed);
        assert_eq!(w.focused_part(), Some(Part::Month));
    }

    #[test]
    fn the_widget_owns_nothing_and_reports_a_whole_stamp() {
        // The rule the crate runs on. A picker that stepped its own copy would show the 16th for one
        // frame and then be overwritten by a `view` built from a model that still says the 15th.
        let mut slots = SlotTable::new();
        let (out, w) = wired(&mut slots, Stamp::date(2023, 6, 15));
        press(&w, Key::Right);
        assert_eq!(out.take(), alloc::vec![Msg::Set(Stamp::date(2023, 6, 16))]);
        assert_eq!(w.stamp, Stamp::date(2023, 6, 15), "it still shows what the model said");
    }

    #[test]
    fn stepping_the_month_onto_february_does_not_leave_a_thirty_first() {
        // The consequence, through the widget: one press on the month field of a 31 January, and the
        // *message* carries a date that exists. There is no branch about February in this file — the
        // clamp is `Stamp::with_part`'s, and this is the test that says the widget goes through it.
        let mut slots = SlotTable::new();
        let (out, w) = wired(&mut slots, jan31());
        press(&w, Key::Down);
        press(&w, Key::Right);
        assert_eq!(out.take(), alloc::vec![Msg::Set(Stamp::date(2023, 2, 28))]);

        // And in a leap year the same press stops one day later.
        let mut slots = SlotTable::new();
        let (out, w) = wired(&mut slots, Stamp::date(2024, 1, 31));
        press(&w, Key::Down);
        press(&w, Key::Right);
        assert_eq!(out.take(), alloc::vec![Msg::Set(Stamp::date(2024, 2, 29))]);
    }

    #[test]
    fn a_day_field_in_february_cannot_be_stepped_to_the_twenty_ninth() {
        // The other half of the invalid-date answer, and the half a clamp in `update` cannot give:
        // the bound comes from the calendar, so the user is never looking at a number the model is
        // about to move under them.
        let mut slots = SlotTable::new();
        let (out, w) = wired(&mut slots, Stamp::date(2023, 2, 28));
        assert_eq!(press(&w, Key::Right), Handled::Consumed, "consumed, or the cursor slides off");
        assert!(out.is_empty(), "nothing moved, so nothing was reported");
        // One year later there is a 29th, and the identical press reaches it.
        let mut slots = SlotTable::new();
        let (out, w) = wired(&mut slots, Stamp::date(2024, 2, 28));
        press(&w, Key::Right);
        assert_eq!(out.take(), alloc::vec![Msg::Set(Stamp::date(2024, 2, 29))]);
    }

    #[test]
    fn a_step_past_a_bound_is_swallowed_rather_than_reported_or_handed_back() {
        // Both halves matter. `Consumed`, because a year at its floor must not let `Left` slide the
        // cursor onto whatever is beside the picker; and no message, because "set it to what it
        // already is" is an `update` and a repaint per keypress for nothing.
        let mut slots = SlotTable::new();
        let (out, w) = wired(&mut slots, Stamp::date(YEAR_MIN, 1, 1));
        press(&w, Key::Down);
        press(&w, Key::Down);
        assert_eq!(press(&w, Key::Left), Handled::Consumed);
        assert!(out.is_empty());

        let mut slots = SlotTable::new();
        let (out, w) = wired(&mut slots, Stamp::date(YEAR_MAX, 12, 31));
        press(&w, Key::Down);
        press(&w, Key::Down);
        assert_eq!(press(&w, Key::Right), Handled::Consumed);
        assert!(out.is_empty());
    }

    #[test]
    fn the_vertical_keys_are_handed_back_at_the_ends_so_a_form_can_move_on() {
        // `Escape`, and the reason it is the default: a picker that consumed every `Down` would be
        // the row nobody can get past.
        let mut slots = SlotTable::new();
        let (_, w) = wired(&mut slots, jan31());
        assert_eq!(press(&w, Key::Up), Handled::Ignored, "first field, nowhere up to go");
        for _ in 0..2 {
            assert_eq!(press(&w, Key::Down), Handled::Consumed);
        }
        assert_eq!(press(&w, Key::Down), Handled::Ignored, "last field, handed back");
        assert_eq!(w.focused_part(), Some(Part::Year), "and it keeps its place");
    }

    #[test]
    fn an_enclosing_form_still_reaches_the_row_below_the_picker() {
        // The property the escape exists for, asserted through a real `FocusScope` rather than
        // through the widget's return value: three presses walk the picker's own fields and the
        // fourth moves the form's cursor onto the button. Innermost-first dispatch is what makes it
        // work, and this is the test that fails if either half stops holding.
        let mut slots = SlotTable::new();
        let picker = DateTime::<Msg>::date(&mut slots, jan31());
        let scope = FocusScope::vertical(&mut slots)
            .stop(|f| Node::leaf(picker.focused(f)))
            .stop(|f| Node::leaf(Button::new("Save", Msg::Save).focused(f)));
        let stops = scope.stops();
        let root = scope.build();

        let hit = |key: Key| {
            testing::with_theme(Palette::DARK, |theme| {
                let mut cache = UiCache::with_capacity(root.slot_count());
                layout::place_frame(&root, testing::SCREEN, &mut cache, theme);
                with_key_ctx(|cx| layout::dispatch_key(&root, KeyEvent::new(key), &cache, cx))
            })
        };
        for _ in 0..2 {
            assert_eq!(hit(Key::Down), Handled::Consumed);
            assert_eq!(stops.cursor(), 0, "still inside the picker");
        }
        assert_eq!(hit(Key::Down), Handled::Consumed, "the form takes the one the picker declined");
        assert_eq!(stops.cursor(), 1, "and the button has it");
    }

    #[test]
    fn wrap_cycles_the_fields_and_stop_holds_the_last_one() {
        // The two policies a full-screen picker and a screen that owns the vertical keys want. Both
        // consume the edge press, which is exactly why neither can be the default inside a form.
        let mut slots = SlotTable::new();
        let w = DateTime::<Msg>::date(&mut slots, jan31()).focused(true).policy(EdgePolicy::Wrap);
        assert_eq!(press(&w, Key::Up), Handled::Consumed);
        assert_eq!(w.focused_part(), Some(Part::Year), "day → year, the long way round");
        assert_eq!(press(&w, Key::Down), Handled::Consumed);
        assert_eq!(w.focused_part(), Some(Part::Day));

        let mut slots = SlotTable::new();
        let w = DateTime::<Msg>::date(&mut slots, jan31()).focused(true).policy(EdgePolicy::Stop);
        assert_eq!(press(&w, Key::Up), Handled::Consumed);
        assert_eq!(w.focused_part(), Some(Part::Day), "held, and the press eaten");
    }

    #[test]
    fn select_is_left_to_the_screen_that_labelled_it() {
        // The deliberate difference from `Stepper`, where `Select` steps the value with a wrap. Here
        // the D-pad centre is the screen's action and a picker that took it would shadow the Save
        // the softkey bar is advertising.
        let mut slots = SlotTable::new();
        let (out, w) = wired(&mut slots, jan31());
        assert_eq!(press(&w, Key::Select), Handled::Ignored);
        assert!(out.is_empty());
        for key in [Key::Backspace, Key::Delete, Key::Char('7'), Key::Enter] {
            assert_eq!(press(&w, key), Handled::Ignored, "{key:?}");
        }
        assert!(out.is_empty());
    }

    #[test]
    fn an_unfocused_picker_answers_nothing_at_all() {
        // Two pickers on one form — a start date and an end date — and one press: without the flag
        // both would report, and the model would keep the second one's.
        let mut slots = SlotTable::new();
        let out = Outbox::new();
        let w = DateTime::date(&mut slots, jan31()).out(out.clone(), Msg::Set);
        for key in [Key::Left, Key::Right, Key::Up, Key::Down, Key::Select] {
            assert_eq!(press(&w, key), Handled::Ignored, "{key:?}");
        }
        assert!(out.is_empty());
        assert_eq!(w.cursor(), 0, "and its cursor did not move either");
    }

    #[test]
    fn a_picker_with_nowhere_to_send_still_consumes_the_value_keys() {
        // `Switch`'s note applies unchanged: handing the press back because the caller forgot the
        // channel would move the enclosing form's cursor instead, which reads as a date field that
        // navigates.
        let mut slots = SlotTable::new();
        let w = DateTime::<Msg>::date(&mut slots, jan31()).focused(true);
        assert_eq!(press(&w, Key::Right), Handled::Consumed);
    }

    #[test]
    fn the_cursor_survives_the_tree_being_rebuilt() {
        // Why it lives in the slot table: `view` runs again on every change, and a cursor held in
        // the widget would jump back to the day field mid-edit.
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let (_, first) = wired(&mut slots, jan31());
        press(&first, Key::Down);
        assert_eq!(first.cursor(), 1);
        drop(first);

        slots.begin_frame();
        let (_, second) = wired(&mut slots, jan31());
        assert_eq!(second.cursor(), 1);
        assert_eq!(slots.type_mismatches(), 0);
    }

    #[test]
    fn a_cursor_left_past_a_field_that_went_away_is_pulled_back_in() {
        // The field count comes from the caller and can shrink between two frames — a date picker
        // that becomes a month-and-year one. A cursor left past the end focuses nothing, and the
        // symptom is a screen where no key does anything.
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let (_, w) = wired(&mut slots, jan31());
        press(&w, Key::Down);
        press(&w, Key::Down);
        assert_eq!(w.cursor(), 2);
        drop(w);

        slots.begin_frame();
        let w = DateTime::<Msg>::time(&mut slots, jan31());
        assert_eq!(w.cursor(), 1, "clamped into the two fields that are left");
        assert_eq!(w.focused_part(), Some(Part::Minute));
    }

    #[test]
    fn a_time_picker_is_two_fields_and_wraps_neither_into_the_other() {
        // 23:59 and one press on the minute: the hour must not roll over. Carrying is arithmetic
        // nobody asked for here, and a spinner the user is not looking at moving under a press is
        // how a 23:59 alarm ends up at midnight.
        let mut slots = SlotTable::new();
        let out = Outbox::new();
        let w = DateTime::time(&mut slots, Stamp::new(2023, 6, 15, 23, 59))
            .focused(true)
            .out(out.clone(), Msg::Set);
        assert_eq!(w.focused_part(), Some(Part::Hour));
        press(&w, Key::Down);
        assert_eq!(press(&w, Key::Right), Handled::Consumed);
        assert!(out.is_empty(), "the minute is at its ceiling and nothing carried");
        press(&w, Key::Left);
        assert_eq!(out.take(), alloc::vec![Msg::Set(Stamp::new(2023, 6, 15, 23, 58))]);
    }

    #[test]
    fn more_parts_than_a_stamp_has_are_dropped_rather_than_panicked_on() {
        // This is called from `view`, where a panic is a dead application on a phone whose whole
        // failure report is a dialog with a number in it.
        let mut slots = SlotTable::new();
        let parts = [Part::Day, Part::Month, Part::Year, Part::Hour, Part::Minute, Part::Day];
        let w = DateTime::<Msg>::parts(&mut slots, jan31(), &parts);
        assert_eq!(w.n, MAX_FIELDS);
        // And no fields at all: legal, silent, and it must not answer a key.
        let mut slots = SlotTable::new();
        let w = DateTime::<Msg>::parts(&mut slots, jan31(), &[]).focused(true);
        assert_eq!(w.focused_part(), None);
        for key in [Key::Left, Key::Right, Key::Up, Key::Down] {
            assert_eq!(press(&w, key), Handled::Ignored, "{key:?}");
        }
    }

    #[test]
    fn labels_come_from_the_caller_and_a_short_list_falls_back_to_the_number() {
        // The crate ships no text, so a month name can only arrive as a parameter. A list of the
        // wrong length must not blank the field: a month spinner showing nothing is unusable, and a
        // shipped app should not lose its date screen to a short array.
        let mut slots = SlotTable::new();
        let w = DateTime::<Msg>::date(&mut slots, Stamp::date(2023, 2, 10)).labels(Part::Month, &MESES);
        assert_eq!(w.label_at(1, Part::Month), Some("Fev"), "labels[0] is the field's minimum");
        assert_eq!(w.label_at(0, Part::Day), None, "an unlabelled field shows its number");

        let short = DateTime::<Msg>::date(&mut slots, Stamp::date(2023, 12, 10))
            .labels(Part::Month, &MESES[..3]);
        assert_eq!(short.label_at(1, Part::Month), None, "December fell back to its number");

        // Naming a part that is not on the picker does nothing rather than panicking or shifting a
        // label onto the wrong field.
        let time = DateTime::<Msg>::time(&mut slots, jan31()).labels(Part::Month, &MESES);
        assert!(time.labels.iter().all(Option::is_none));
    }

    #[test]
    fn the_digest_is_not_zero_and_ignores_everything_but_the_shape() {
        // Zero would mean "re-measure me every frame", and through `Group::content_hash` that puts
        // the whole enclosing form on the slow path — not just this widget.
        let mut slots = SlotTable::new();
        let a = DateTime::<Msg>::date(&mut slots, jan31());
        assert_ne!(a.content_hash(), 0);

        // The stamp, the focus, the cursor and the policy are all out: none of them changes a
        // block's width, and folding one in would re-measure the row on every keypress.
        let b = DateTime::<Msg>::date(&mut slots, Stamp::date(2024, 12, 29))
            .focused(true)
            .policy(EdgePolicy::Wrap);
        press(&b, Key::Down);
        assert_eq!(a.content_hash(), b.content_hash());

        // The shape is in: which fields, in which order, with which words, and how far apart.
        let mut differ = alloc::vec![
            DateTime::<Msg>::time(&mut slots, jan31()).content_hash(),
            DateTime::<Msg>::parts(&mut slots, jan31(), &[Part::Year, Part::Month, Part::Day])
                .content_hash(),
            DateTime::<Msg>::date(&mut slots, jan31()).labels(Part::Month, &MESES).content_hash(),
            DateTime::<Msg>::date(&mut slots, jan31())
                .labels(Part::Month, &["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l"])
                .content_hash(),
            DateTime::<Msg>::date(&mut slots, jan31()).gap(Gap::Wide).content_hash(),
            a.content_hash(),
        ];
        let n = differ.len();
        differ.sort_unstable();
        differ.dedup();
        assert_eq!(differ.len(), n, "two different shapes share a digest");
    }

    #[test]
    fn it_measures_a_line_of_blocks_and_not_the_band_it_was_offered() {
        // The `Stretch` trap's arithmetic half. A picker that reported 38 pixels would have been
        // believed by every row that asked it how big it is.
        testing::with_theme(Palette::DARK, |t| {
            let mut slots = SlotTable::new();
            let w = DateTime::<Msg>::date(&mut slots, jan31());
            let got = w.measure(Constraints::loose(320, 38), t);
            let gap = Gap::Tight.resolve(t);
            assert_eq!(got, Size::new(ui::STEPPER_W * 3 + gap * 2, ui::stepper_height(38, t)));
            assert_eq!(got.h, t.fonts.body.line_height());
            assert!(got.h < 38, "a line, not the band");

            // A time picker is two blocks and one gap, so the sum is not a coincidence of threes.
            let two = DateTime::<Msg>::time(&mut slots, jan31()).measure(Constraints::loose(320, 38), t);
            assert_eq!(two.w, ui::STEPPER_W * 2 + gap);
        });
    }

    #[test]
    fn a_labelled_block_is_measured_from_the_longest_word_it_can_show() {
        // What stops the year field shuffling sideways between May and September — and the reason
        // the digest can leave the value out while folding every label in.
        with_real_theme(|t| {
            let mut slots = SlotTable::new();
            let months = ["Jan", "Setembro"];
            let wide = DateTime::<Msg>::parts(&mut slots, jan31(), &[Part::Month])
                .labels(Part::Month, &months);
            let expected = ui::chevron_width(t, months);
            assert_eq!(wide.measure(Constraints::loose(320, 38), t).w, expected);
            // The same picker showing "Jan" measures the same, because the width is the maximum.
            let jan = DateTime::<Msg>::parts(&mut slots, Stamp::date(2023, 1, 1), &[Part::Month])
                .labels(Part::Month, &months);
            assert_eq!(jan.measure(Constraints::loose(320, 38), t).w, expected);
            assert!(expected > ui::chevron_width(t, ["Jan"]), "the long name is what decides");
        });
    }

    #[test]
    fn the_real_atlas_paints_the_fields_so_the_pixel_tests_below_can_fail() {
        // The negative control, and this widget needs one more than most: the test atlas has exactly
        // one glyph — lowercase 'a' — and a date is digits and chevrons, so every pixel assertion
        // here would be vacuously true under it. This is the test that says the buffers are real ink
        // and that the values, the labels and the cursor all reach the canvas.
        with_real_theme(|t| {
            let bg = t.palette.bg.mid().to_rgb565().0;
            let mut slots = SlotTable::new();
            let base = paint(t, &DateTime::<Msg>::date(&mut slots, jan31()));
            assert!(base.iter().any(|&p| p != bg), "nothing was painted at all");
            assert_ne!(
                base,
                paint(t, &DateTime::<Msg>::date(&mut slots, Stamp::date(2023, 1, 12))),
                "the day does not reach the canvas"
            );
            assert_ne!(
                base,
                paint(t, &DateTime::<Msg>::date(&mut slots, jan31()).focused(true)),
                "the cursor does not change the ink"
            );
            assert_ne!(
                paint(t, &DateTime::<Msg>::date(&mut slots, jan31()).labels(Part::Month, &MESES)),
                base,
                "the labels do not reach the canvas"
            );
        });
    }

    #[test]
    fn the_stretch_a_list_row_applies_does_not_stretch_the_blocks() {
        // `CrossAlign::Stretch` hands this widget the whole 38-pixel band, not the line it measured.
        // Asserted as containment rather than as "the ink is centred", because `draw_text_in` centres
        // in whatever rect it is given — a picker drawn straight into the band would land in the same
        // place while still having reported the wrong size.
        with_real_theme(|t| {
            let mut slots = SlotTable::new();
            let w = DateTime::<Msg>::date(&mut slots, jan31());
            let rows = inked_rows(t, &paint(t, &w));
            let h = ui::stepper_height(ROW.height(), t);
            let y0 = ROW.y0 + (ROW.height() - h) / 2;
            assert!(!rows.is_empty());
            assert!(h < ROW.height(), "or the containment below proves nothing");
            assert!(
                rows.iter().all(|&y| y >= y0 && y < y0 + h),
                "ink at rows {rows:?} escaped the line {y0}..{}",
                y0 + h
            );

            // And that `inked_rows` tracks the geometry at all: a band six pixels lower moves every
            // inked row three pixels with it, so the containment above is a measurement and not a
            // coincidence of a full canvas.
            let lower = Rect { y0: ROW.y0 + 6, ..ROW };
            let (_, buf) = testing::with_canvas(Size::new(ROW.width(), ROW.height()), |c| {
                c.clear(t.palette.bg.mid());
                w.draw(c, lower, t);
            });
            assert_eq!(
                inked_rows(t, &buf),
                rows.iter().map(|y| y + 3).collect::<alloc::vec::Vec<_>>()
            );
        });
    }

    #[test]
    fn a_numeric_field_is_the_catalogue_stepper_block_pixel_for_pixel() {
        // The point of building this out of `Stepper` rather than out of `draw_text`: a day block and
        // a retry-count block are the same object, and this is what stops a future edit from making
        // them two. The first field of the picker against a lone stepper over the same number.
        with_real_theme(|t| {
            let mut slots = SlotTable::new();
            let w = DateTime::<Msg>::parts(&mut slots, Stamp::date(2023, 6, 15), &[Part::Day])
                .focused(true);
            let block = Rect::from_xywh(0, 0, ui::STEPPER_W, ROW.height());
            let (_, mine) = testing::with_canvas(Size::new(ui::STEPPER_W, ROW.height()), |c| {
                c.clear(t.palette.bg.mid());
                w.draw(c, block, t);
            });
            let (_, theirs) = testing::with_canvas(Size::new(ui::STEPPER_W, ROW.height()), |c| {
                c.clear(t.palette.bg.mid());
                Stepper::<Msg>::new(15, 1, 30).focused(true).draw(c, block, t);
            });
            assert_eq!(mine, theirs);
        });
    }

    #[test]
    fn only_the_field_with_the_cursor_is_lit_and_it_is_lit_in_both_palettes() {
        // Not a colour assertion — the palette owns that — but a "the cursor is wired to the ink"
        // one. Three blocks that all looked the same would leave nothing on screen saying which
        // spinner the next Left is going to move, which on a keypad phone is the whole interface.
        for (name, palette) in Palette::ALL {
            let atlases = symbian_preview::Atlases::load();
            atlases.with_fonts(|fonts| {
                let t = symbian_ui::Theme::new(palette, fonts);
                let mut slots = SlotTable::new();
                let w = DateTime::<Msg>::date(&mut slots, jan31()).focused(true);
                let day = paint(&t, &w);
                press(&w, Key::Down);
                assert_ne!(day, paint(&t, &w), "{name}: moving the cursor changed no pixels");
            });
        }
    }
}
