//! The strip S60 puts under the title bar, and the Left/Right it takes from everything below it.
//!
//! [`symbian_ui::Tabs`] already owns the half that is arithmetic and pixels: an active index, the
//! clamped Left/Right, and the rounded-top band the era drew the active cell with. What it is not is
//! a container — it says so itself — and this file does not make it one. See *There is no
//! `TabView`* below, which is the decision this widget is mostly about.
//!
//! ```ignore
//! Column::new()
//!     .align(CrossAlign::Stretch)
//!     .child(Tabs::new(model.tab).tab("General").tab("Apps").tab("Home")
//!         .out(out.clone(), Msg::SetTab))
//!     .node(match model.tab {
//!         0 => general_panel(slots, model),
//!         1 => apps_panel(slots, model),
//!         _ => home_panel(slots, model),
//!     })
//! ```
//!
//! # The active tab is the model's, and it has to be
//!
//! [`FocusScope`](super::FocusScope) keeps its cursor in the slot table and
//! [`ScrollList`](super::ScrollList) keeps its scroll offset there, on the rule that navigation is a
//! consequence of having drawn this screen here rather than something a `Cmd` is made of. A tab
//! index cannot follow that rule, and the reason is structural rather than a matter of taste: `view`
//! runs *before* any key is dispatched, and `view` is what decides which panel to build. A tab index
//! hidden in the slot table would be read one frame after the panel that had to be chosen from it,
//! so pressing Right would move the strip and leave the old panel under it for a frame — and, on a
//! screen that only redraws when the model changes, for ever.
//!
//! So this widget owns nothing, exactly like [`Stepper`](super::Stepper): it is handed the index and
//! it reports a new one through a `fn` pointer. It never steps its own copy.
//!
//! # There is no `TabView`
//!
//! A container that swapped the content was written down, costed, and dropped. Three findings, in
//! the order they arrived:
//!
//! 1. **It has no arithmetic to own.** With the index in the model, the whole of a tabbed screen is
//!    a `Column`, this strip, and a `match` on the index — the recipe above. `tests/settings.rs`
//!    states this crate's rule for exactly this situation: an abstraction needs two callers that
//!    already disagree, and there are none.
//! 2. **It could not hold the panels without breaking a rule the crate is built on.** A container
//!    that swaps content takes either every tab's subtree — built by `view`, every frame, so three
//!    tabs cost three panels' worth of allocation per frame to draw one — or a closure per tab,
//!    which is the `Outbox::wrapped` cost [`Stepper`](super::Stepper) rejects in as many words.
//!    The `match` in the caller builds exactly one panel and holds no closures at all.
//! 3. **It would not fix the key ordering, because the ordering is already right.**
//!    [`crate::layout::dispatch_key_group`] offers a key to children in declaration order and stops
//!    at the first taker, so a strip declared above its panel is asked first — which is the
//!    behaviour wanted, for the reason below. A container would be re-stating in code what the
//!    reading order of the call site already says.
//!
//! # Left and Right belong to the strip, and the collision is deliberate
//!
//! [`Stepper`](super::Stepper), [`Slider`](super::Slider) and [`DateTime`](super::DateTime) all want
//! Left and Right, and a strip above a form of them takes both. That is not an accident to be
//! arbitrated away — it is what S60 does, and this SDK is already built on it: `stepper.rs`'s
//! `select_wraps_past_the_top_because_a_tab_strip_may_own_left_and_right` exists precisely because a
//! stepper on a tabbed screen never sees a horizontal arrow, so `Select` has to be able to drive it
//! on its own.
//!
//! The alternative — content first, strip second, the strip taking only what the panel declined —
//! is worse, and specifically:
//!
//! * A [`Slider`](super::Slider) consumes Left and Right unconditionally and has **no `Select`
//!   fallback at all** (`a_slider_answers_only_the_horizontal_keys` asserts it ignores `Select`). So
//!   the moment the cursor lands on a slider, content-first makes the tabs unreachable by any key on
//!   the phone. A control that must be driven with `Select` is an inconvenience; a screen the user
//!   cannot navigate out of is a bug report.
//! * It would make the meaning of Right depend on which field has the cursor — the same complaint
//!   [`EdgePolicy`](symbian_ui::EdgePolicy) documents about letting a clamped arrow fall through.
//!
//! # And the edges hold, which is the other half of the same argument
//!
//! Left on the first tab is **consumed and moves nothing** — [`EdgePolicy::Stop`], the policy
//! `FocusRing`, `ListState` and `GridState` all take. Consuming it is the load-bearing part: under
//! `Escape` the press would fall past the strip into the panel and step whatever had the cursor, so
//! one key would mean "previous tab" or "one less retry" depending on which tab was showing. That is
//! the failure `EdgePolicy`'s own docs call the phone being broken.
//!
//! Wrapping was the third candidate and is what `EdgePolicy::Wrap` calls "what a short strip of tabs
//! wants". It is not what this one does, because the arithmetic is
//! [`symbian_ui::Tabs::handle_key`]'s and that clamps — and a second, disagreeing answer to "what
//! does Right do at the last tab" living one layer up is the thing this crate exists to not have.
//!
//! # A strip is not a focus stop
//!
//! There is no `focused` flag here, unlike every control in the catalogue. A tab strip is screen
//! chrome that is always live, the way a title bar is: it is added to a
//! [`FocusScope`](super::FocusScope) through [`fixed`](super::FocusScope::fixed) if it is added at
//! all, and no cursor ever lands on it. Giving it a flag would mean a screen where the tabs answer
//! only once the cursor has been walked up to them — which on a form whose first field eats Left and
//! Right is a strip that can never be reached.

use alloc::string::String;
use alloc::vec::Vec;

use symbian_gfx::{Canvas, Rect, Size};
use symbian_ui::{tabs as ui, Handled, KeyEvent, Theme};

use crate::constraints::Constraints;
use crate::outbox::Outbox;
use crate::spacing::RowHeight;
use crate::widget::{hash_str, KeyCtx, Widget, WidgetHash};

/// Where a new index goes, and how to name it once it gets there.
///
/// The same alias [`Stepper`](super::Stepper) keeps, for the same reason: `Option<(Outbox<M>,
/// fn(usize) -> M)>` in a struct definition reads as machinery rather than as the one thing it is.
type Report<M> = (Outbox<M>, fn(usize) -> M);

/// A row of tabs across the top of a screen, showing the model's active index.
pub struct Tabs<M> {
    labels: Vec<String>,
    /// What the model says is active, **unclamped**. Clamping needs the label count, which is not
    /// final until the last `.tab(..)` call — so it happens in [`Tabs::active`] and every read goes
    /// through there. Clamping in `new` would pin the index against a count of zero.
    active: usize,
    height: RowHeight,
    out: Option<Report<M>>,
}

impl<M> Tabs<M> {
    /// A strip with no tabs in it yet, showing `active`.
    ///
    /// An out-of-range index is tolerated rather than panicked on, because a `view` runs on a phone
    /// whose entire failure report is a dialog with a number in it — and because a model that loses
    /// a tab between two frames produces exactly that, for one frame, through no fault of its own.
    pub fn new(active: usize) -> Self {
        Self {
            labels: Vec::new(),
            active,
            // A heading's height, borrowed as a role rather than as its pixels. A strip and a
            // `SectionHeader` are the same kind of band — a line of small text introducing what is
            // under it — and naming the role is what keeps them from drifting apart by a pixel when
            // the theme moves. See `RowHeight::Header`.
            height: RowHeight::Header,
            out: None,
        }
    }

    /// Add a tab.
    pub fn tab(mut self, label: impl Into<String>) -> Self {
        self.labels.push(label.into());
        self
    }

    /// Add every tab at once, for a strip whose labels come from the model.
    pub fn tabs(mut self, labels: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.labels.extend(labels.into_iter().map(Into::into));
        self
    }

    /// How tall the strip is. Defaults to [`RowHeight::Header`].
    pub fn height(mut self, h: impl Into<RowHeight>) -> Self {
        self.height = h.into();
        self
    }

    /// Where a new index goes, and how to say it.
    ///
    /// `msg` receives the index of the tab that Left or Right moved to, already inside the strip. A
    /// `fn` pointer rather than an [`Outbox<usize>`](Outbox) for the reason `stepper.rs` sets out at
    /// length: `Outbox::wrapped` boxes a closure and allocates an `Rc` per call, and a `view` is
    /// rebuilt every frame.
    pub fn out(mut self, out: Outbox<M>, msg: fn(usize) -> M) -> Self {
        self.out = Some((out, msg));
        self
    }

    /// How many tabs there are.
    pub fn count(&self) -> usize {
        self.labels.len()
    }

    /// Which tab is drawn active — the model's index, pulled inside the strip.
    ///
    /// Never the raw number. A model holding 5 against three tabs would otherwise paint no active
    /// cell at all, which reads as a strip that has lost its place rather than as a model that has.
    pub fn active(&self) -> usize {
        match self.labels.len() {
            0 => 0,
            n => self.active.min(n - 1),
        }
    }

    /// The imperative strip, built from the model's numbers for one call and dropped.
    ///
    /// Every question about tabs — where the cells fall, what Right does at the end, which colour
    /// the active one takes — is answered by [`symbian_ui::Tabs`] and by nothing written here. That
    /// is the same bargain [`Stepper`](super::Stepper) makes, and it is why "the edges hold" is
    /// stated in the module docs as an observation about that type rather than as a rule this one
    /// implements.
    fn probe(&self) -> ui::Tabs {
        let mut t = ui::Tabs::new();
        t.set_active(self.active(), self.labels.len());
        t
    }
}

impl<M: 'static> Widget for Tabs<M> {
    /// The height role, and deliberately nothing else.
    ///
    /// The labels are out because the strip does not size to them: the cells are equal divisions of
    /// whatever width it is offered, so "General" and "Personalisation" occupy the same box and a
    /// fourth tab makes the strip no taller. The active index is out because it chooses which cell
    /// gets the band — ink, not geometry, exactly as `ListItem` leaves `selected` out of its own
    /// digest.
    ///
    /// Not zero, and this one matters more than most: a strip sits at the top of a `Column` that
    /// holds the whole screen, and `Group::content_hash` returns zero if *any* child is volatile. A
    /// zero here would put every tabbed screen's entire layout on the re-measure-everything path for
    /// ever.
    fn content_hash(&self) -> WidgetHash {
        self.height.hash(hash_str(0, "tabs"))
    }

    /// As wide as offered, one heading's worth tall.
    ///
    /// Wide because a strip that shrank to its labels would be a stripe ending mid-screen — the
    /// defect `SectionHeader` documents for the same reason. Short because the height is the band's,
    /// not the screen's: a widget that answered with the offer would have lied to the column
    /// dividing the space under it, which is [`Avatar`](super::Avatar)'s 180-pixel bug and
    /// [`Slider`](super::Slider)'s eaten label in one.
    fn measure(&self, constraints: Constraints, theme: &Theme<'_>) -> Size {
        constraints.constrain(Size::new(constraints.max_w, self.height.resolve(theme)))
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        if self.labels.is_empty() {
            return;
        }
        // The band carved back to the height this widget *measured*, anchored to the top of whatever
        // rect it was handed. `CrossAlign::Stretch` on a row would otherwise give a strip the whole
        // 205-pixel content band, and `symbian_ui::Tabs::draw` fills what it is given — so the tabs
        // would become a page-tall gradient with three words floating in it. That is
        // `stepper_box`'s trap, met on the other axis, and the containment is asserted below.
        let h = self.height.resolve(theme).min(rect.height());
        let band = Rect { y1: rect.y0 + h, ..rect };
        // One `Vec` of borrows per frame, per strip, and it is the price of not owning a second
        // implementation of the cell arithmetic. Worth distinguishing from the allocation
        // `stepper.rs` refuses: that one is per *widget* per frame in a form of many, this is one
        // small vector for the single strip a screen has.
        let labels: Vec<&str> = self.labels.iter().map(String::as_str).collect();
        self.probe().draw(c, band, theme, &labels);
    }

    /// Left and Right, always consumed, reported only when the tab actually changed.
    ///
    /// No `focused` check: see the module docs on why a strip is not a focus stop. Everything that
    /// is not a horizontal arrow comes back `Ignored` from the imperative strip, which is what
    /// leaves Up, Down and Select to the panel underneath.
    fn handle_key(&self, ev: KeyEvent, _rect: Rect, _cx: &mut KeyCtx<'_>) -> Handled {
        let mut probe = self.probe();
        let before = probe.active();
        let handled = probe.handle_key(ev, self.labels.len());
        if handled == Handled::Consumed && probe.active() != before {
            // Only on a real move. Left on the first tab is still `Consumed` — that is the whole
            // point of the edge holding — but a message saying "select the tab already selected"
            // would be an `update` and a full rebuild of the panel on every press against the end.
            if let Some((out, msg)) = &self.out {
                out.push(msg(probe.active()));
            }
        }
        handled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::UiCache;
    use crate::layout::{self, CrossAlign};
    use crate::widget::with_key_ctx;
    use crate::widgets::{Column, Node, Stepper};
    use symbian_gfx::Size as GSize;
    use symbian_ui::{testing, Key, Palette};

    /// The messages a tabbed settings screen sends.
    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Msg {
        SetTab(usize),
        SetRetries(i32),
    }

    /// The content band of the E72 under a title bar.
    const BAND: Rect = Rect { x0: 0, y0: 0, x1: 320, y1: 205 };

    fn strip(active: usize) -> (Outbox<Msg>, Tabs<Msg>) {
        let out = Outbox::new();
        let t = Tabs::new(active)
            .tab("Ana")
            .tab("Bea")
            .tab("Cara")
            .out(out.clone(), Msg::SetTab);
        (out, t)
    }

    fn press(t: &Tabs<Msg>, key: Key) -> Handled {
        testing::with_theme(Palette::DARK, |_| {
            with_key_ctx(|cx| t.handle_key(KeyEvent::new(key), BAND, cx))
        })
    }

    /// The *real* device atlases. `testing::with_theme` has one glyph — lowercase `a` — so every
    /// label in this file contains one, and anything asserting that a label reached the canvas has
    /// to come through here. See `tests/list_item_parity.rs`.
    fn with_real_theme<R>(f: impl FnOnce(&Theme<'_>) -> R) -> R {
        let atlases = symbian_preview::Atlases::load();
        atlases.with_fonts(|fonts| f(&symbian_ui::Theme::dark(fonts)))
    }

    /// Paint one strip over `rect` in a 320x205 canvas.
    fn painted(t: &Theme<'_>, w: &Tabs<Msg>, rect: Rect) -> Vec<u16> {
        let (_, buf) = testing::with_canvas(GSize::new(320, 205), |c| {
            c.clear(t.palette.bg.mid());
            w.draw(c, rect, t);
        });
        buf
    }

    /// Which rows of a 320-wide buffer have any ink in them.
    fn inked_rows(t: &Theme<'_>, buf: &[u16]) -> Vec<i32> {
        let bg = t.palette.bg.mid().to_rgb565().0;
        (0..205).filter(|&y| (0..320).any(|x| buf[(y * 320 + x) as usize] != bg)).collect()
    }

    #[test]
    fn right_and_left_move_the_active_tab_and_report_the_new_index() {
        // The rule the whole crate runs on: the widget reports, the model decides. A strip that
        // moved its own index would show the new tab for one frame over the *old* panel, because
        // `view` chose that panel from the model before the key ever arrived.
        let (out, t) = strip(0);
        assert_eq!(press(&t, Key::Right), Handled::Consumed);
        assert_eq!(out.take(), alloc::vec![Msg::SetTab(1)]);
        assert_eq!(t.active(), 0, "it still shows what the model said");

        let (out, t) = strip(2);
        press(&t, Key::Left);
        assert_eq!(out.take(), alloc::vec![Msg::SetTab(1)]);
    }

    #[test]
    fn an_arrow_at_the_end_is_swallowed_rather_than_reported_or_handed_back() {
        // Both halves matter and they fail differently. `Consumed`, because under
        // `EdgePolicy::Escape` this press would fall into the panel below and step whatever had the
        // cursor — one key meaning "previous tab" or "one less retry" depending on which tab was
        // showing. And no message, because "select tab 0" when tab 0 is selected rebuilds the whole
        // panel for nothing, once per press against the end.
        let (out, t) = strip(0);
        assert_eq!(press(&t, Key::Left), Handled::Consumed);
        assert!(out.is_empty(), "nothing moved, so nothing was reported");

        let (out, t) = strip(2);
        assert_eq!(press(&t, Key::Right), Handled::Consumed);
        assert!(out.is_empty());
    }

    #[test]
    fn the_edges_hold_rather_than_wrapping_because_the_imperative_strip_does() {
        // Pinned to `symbian_ui::Tabs` rather than re-decided here. `EdgePolicy::Wrap` calls
        // wrapping "what a short strip of tabs wants" and this strip deliberately does not do it —
        // a second, disagreeing answer to "what does Right do at the last tab", living one layer
        // up, is the thing this crate exists to not have.
        let mut imperative = ui::Tabs::new();
        imperative.set_active(2, 3);
        imperative.handle_key(KeyEvent::new(Key::Right), 3);
        assert_eq!(imperative.active(), 2, "the toolkit clamps");

        let (out, t) = strip(2);
        press(&t, Key::Right);
        assert!(out.is_empty(), "and so, by construction, does this");
    }

    #[test]
    fn a_strip_never_takes_the_keys_the_panel_below_it_navigates_with() {
        // What keeps a tabbed screen usable at all. Up and Down move the form's cursor, Select
        // drives the focused control — a strip that consumed either would leave a screen whose
        // tabs work and whose contents cannot be reached.
        let (out, t) = strip(1);
        for key in [Key::Up, Key::Down, Key::Select, Key::Backspace, Key::Char('a')] {
            assert_eq!(press(&t, key), Handled::Ignored, "{key:?}");
        }
        assert!(out.is_empty());
    }

    #[test]
    fn a_strip_with_no_tabs_answers_nothing_at_all() {
        // Reachable from a model whose tab list arrived empty, and the arithmetic underneath is
        // `count == 0`. Consuming here would swallow both arrows on a screen where they have
        // nowhere to go.
        let out = Outbox::new();
        let t = Tabs::new(0).out(out.clone(), Msg::SetTab);
        for key in [Key::Left, Key::Right] {
            assert_eq!(press(&t, key), Handled::Ignored, "{key:?}");
        }
        assert!(out.is_empty());
    }

    #[test]
    fn an_index_the_model_cannot_justify_is_pulled_inside_the_strip() {
        // A model that loses a tab between two frames holds an index past the end for one frame,
        // through no fault of its own. Left unclamped, no cell would be drawn active — which reads
        // as a strip that has lost its place rather than as a model that has.
        let t: Tabs<Msg> = Tabs::new(9).tab("Ana").tab("Bea");
        assert_eq!(t.active(), 1);
        // And an empty strip has no index to clamp to, rather than underflowing on `n - 1`.
        assert_eq!(Tabs::<Msg>::new(9).active(), 0);
    }

    #[test]
    fn a_clamped_index_moves_from_where_it_is_drawn_and_not_from_where_it_came() {
        // The consequence of clamping on read: a model holding 9 against three tabs paints the
        // third, and Left on it must report 1 — not 8. Stepping from the raw number would push a
        // value that fails the model's own invariant on the very next frame.
        let out = Outbox::new();
        let t = Tabs::new(9).tab("Ana").tab("Bea").tab("Cara").out(out.clone(), Msg::SetTab);
        press(&t, Key::Left);
        assert_eq!(out.take(), alloc::vec![Msg::SetTab(1)]);
    }

    #[test]
    fn a_strip_with_nowhere_to_send_still_consumes_the_key() {
        // `Stepper`'s note applies unchanged: handing the press back because the caller forgot the
        // channel would let it reach the panel instead, which reads as a strip that navigates the
        // form.
        let t: Tabs<Msg> = Tabs::new(0).tab("Ana").tab("Bea");
        assert_eq!(press(&t, Key::Right), Handled::Consumed);
    }

    #[test]
    fn it_measures_a_band_and_not_the_screen_it_was_offered() {
        // The `Avatar`/`Slider` trap. A strip that answered with the offered height would have been
        // believed by the column dividing the space under it, and the panel would have got nothing.
        testing::with_theme(Palette::DARK, |t| {
            let (_, w) = strip(0);
            let got = w.measure(Constraints::loose(320, 205), t);
            assert_eq!(got, Size::new(320, RowHeight::Header.resolve(t)));
            assert!(got.h < 205, "it took the whole band");
            // As wide as offered rather than as wide as its labels: a strip that shrank to fit
            // would be a stripe ending mid-screen.
            assert_eq!(w.measure(Constraints::loose(120, 205), t).w, 120);
            // And an overridden height is honoured, or the role would be decoration.
            let tall = Tabs::<Msg>::new(0).tab("Ana").height(30);
            assert_eq!(tall.measure(Constraints::loose(320, 205), t).h, 30);
        });
    }

    #[test]
    fn the_stretch_a_column_applies_does_not_stretch_the_strip() {
        // `CrossAlign::Stretch` hands a widget the whole band, and `symbian_ui::Tabs::draw` fills
        // what it is given — so a strip drawn straight into the rect it was handed becomes a
        // 205-pixel gradient with three words floating in it. Containment is the property asserted,
        // rather than "the ink is at the top", because a filled band is at the top either way.
        with_real_theme(|t| {
            let (_, w) = strip(1);
            let rows = inked_rows(t, &painted(t, &w, BAND));
            let h = RowHeight::Header.resolve(t);
            assert!(!rows.is_empty(), "nothing was painted at all");
            assert!(
                rows.iter().all(|&y| y < h),
                "ink at rows {rows:?} escaped the {h}-pixel strip"
            );
            // And the control that makes the containment a measurement rather than a coincidence:
            // hand the strip a band six pixels lower and every inked row moves with it.
            let lower = Rect { y0: 6, ..BAND };
            let moved = inked_rows(t, &painted(t, &w, lower));
            assert_eq!(moved, rows.iter().map(|y| y + 6).collect::<Vec<_>>());
        });
    }

    #[test]
    fn the_real_atlas_paints_the_labels_so_the_pixel_tests_can_fail() {
        // The negative control. Under the one-glyph test atlas a strip is three coloured cells and
        // nothing else, so a comparison of two label sets would pass whatever `draw` did — and the
        // test above would still pass if the labels never reached the canvas.
        with_real_theme(|t| {
            let bg = t.palette.bg.mid().to_rgb565().0;
            let (_, ana) = strip(0);
            let one = painted(t, &ana, BAND);
            assert!(one.iter().any(|&p| p != bg), "nothing was painted");

            let other: Tabs<Msg> = Tabs::new(0).tab("Zoe").tab("Bea").tab("Cara");
            assert_ne!(one, painted(t, &other, BAND), "the labels do not reach the canvas");

            let (_, second) = strip(1);
            assert_ne!(one, painted(t, &second, BAND), "the active band does not move");
        });
    }

    #[test]
    fn a_strip_above_a_stepper_takes_the_horizontal_keys_and_leaves_the_rest() {
        // The collision, asserted end to end on the shape it actually appears in: a strip over a
        // settings row. Right belongs to the tabs — that is S60's behaviour and the reason
        // `symbian_ui::Stepper` wraps on `Select` — and `Select` still reaches the stepper, which is
        // the only thing that makes the arrangement usable.
        let tab_out = Outbox::new();
        let step_out = Outbox::new();
        let root = Node::Group(
            Column::new()
                .align(CrossAlign::Stretch)
                .stretch_width()
                .child(Tabs::new(0).tab("Ana").tab("Bea").out(tab_out.clone(), Msg::SetTab))
                .child(
                    Stepper::new(3, 0, 9)
                        .focused(true)
                        .out(step_out.clone(), Msg::SetRetries),
                ),
        );

        let mut cache = UiCache::with_capacity(root.slot_count());
        testing::with_theme(Palette::DARK, |theme| {
            layout::place_frame(&root, BAND, &mut cache, theme);
        });
        let dispatch = |key: Key| {
            with_key_ctx(|cx| layout::dispatch_key_node(&root, 0, KeyEvent::new(key), &cache, cx))
        };

        assert_eq!(dispatch(Key::Right), Handled::Consumed);
        assert_eq!(tab_out.take(), alloc::vec![Msg::SetTab(1)]);
        assert!(step_out.is_empty(), "the stepper stepped behind the strip's back");

        // Select is the stepper's, and it is the whole of its escape route.
        assert_eq!(dispatch(Key::Select), Handled::Consumed);
        assert!(tab_out.is_empty());
        assert_eq!(step_out.take(), alloc::vec![Msg::SetRetries(4)]);
    }

    #[test]
    fn the_stepper_below_a_strip_would_have_taken_the_key_if_it_had_been_offered_it() {
        // The negative control for the test above. Without it, "the stepper did not fire" would be
        // satisfied by a stepper that was never reachable at all — a broken screen passing as a
        // correct one.
        let out = Outbox::new();
        let s = Stepper::new(3, 0, 9).focused(true).out(out.clone(), Msg::SetRetries);
        assert_eq!(press_stepper(&s, Key::Right), Handled::Consumed);
        assert_eq!(out.take(), alloc::vec![Msg::SetRetries(4)]);
    }

    fn press_stepper(s: &Stepper<Msg>, key: Key) -> Handled {
        with_key_ctx(|cx| s.handle_key(KeyEvent::new(key), BAND, cx))
    }

    #[test]
    fn the_digest_is_the_height_role_and_is_never_zero() {
        // Constant across labels and across the active index, because neither moves a pixel of the
        // *box*: the cells are equal divisions of the offered width. Not zero, because a strip sits
        // at the top of the column holding the whole screen and `Group::content_hash` returns zero
        // if any child is volatile — a zero here re-measures every tabbed screen, every frame.
        let a: Tabs<Msg> = Tabs::new(0).tab("Ana").tab("Bea");
        let b: Tabs<Msg> = Tabs::new(1).tab("Cara").tab("Dora").tab("Eva");
        assert_eq!(a.content_hash(), b.content_hash());
        assert_ne!(a.content_hash(), 0);
        // The height is in, because it is the one property that does change the box.
        assert_ne!(a.content_hash(), Tabs::<Msg>::new(0).height(30).content_hash());
    }

    #[test]
    fn it_draws_in_every_palette() {
        for (name, palette) in Palette::ALL {
            let (_, w) = strip(1);
            let (_, px) = testing::with_canvas(GSize::new(320, 20), |c| {
                testing::with_theme(palette, |t| {
                    c.clear(palette.bg.mid());
                    w.draw(c, Rect::from_xywh(0, 0, 320, 18), t);
                });
            });
            let bg = palette.bg.mid().to_rgb565().0;
            assert!(px.iter().any(|&p| p != bg), "{name}: the strip is invisible");
        }
    }
}
