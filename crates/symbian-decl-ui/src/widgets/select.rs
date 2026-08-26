//! A drop-down: the chosen option on a settings row, and the popup list it opens into.
//!
//! # It is two nodes, and it has to be
//!
//! Every other control in this catalogue is one widget. This one is a *pair*, and the reason is
//! structural rather than stylistic: **a popup cannot be painted from inside a list row.**
//!
//! [`Widget::overflow_visible`](crate::Widget::overflow_visible) lets a widget paint outside its own
//! rect, and it is not enough, because an ancestor that clips still clips — its own documentation
//! says so, and [`Group::overflow_visible`](crate::widgets::Group::overflow_visible) says it from
//! the other end. A row inside a [`ScrollList`](super::ScrollList) is clipped to the list's band by
//! the list, so a popup drawn by the row would be cut off at the row's own edges no matter what the
//! row declared. That is not a bug to be fixed by adding another flag; it is what makes a scrolling
//! list a scrolling list.
//!
//! So the popup is a sibling at the screen level, on top — which is exactly what
//! [`Stack`](super::Stack) is for:
//!
//! ```ignore
//! let mut popup = None;
//! let form = FocusScope::vertical(slots)
//!     .stop(|f| {
//!         ListItem::new("Theme")
//!             .selected(f)
//!             .trailing(/* the field, below */)
//!             .build()
//!     })
//!     .build();
//!
//! Stack::new(slots).layer(form).layer(popup.expect("Select::field stashes it"))
//! ```
//!
//! and inside the `stop` closure, where the field actually goes:
//!
//! ```ignore
//! Select::new(slots, THEMES, model.theme)
//!     .focused(f)
//!     .out(out.clone(), Msg::SetTheme)
//!     .build()
//!     .field(&mut popup)      // the field goes in the tree, the popup goes in the stack
//! ```
//!
//! [`SelectParts::field`] takes the `&mut Option<Node>` the screen will place, so the two halves
//! arrive from one call and a caller cannot place a popup for a field that does not exist. The
//! other direction — a field whose popup was never placed — cannot be prevented by types, so it is
//! *refused at runtime and counted*: see [`Select::orphaned`].
//!
//! # Where the open flag lives: the slot table
//!
//! Being open is a consequence of having pressed this field, exactly as a scroll offset is a
//! consequence of having drawn this list — so it lives in [`crate::slot`], with the popup's
//! highlight beside it, and the app model never hears about it. What the model owns is the *chosen
//! index*, which is the only part an `update` can act on and the only part a `Cmd` is made of.
//!
//! The obvious objection to that is real and worth writing down. If the flag is in the slot table,
//! how does the *screen* — which is assembling the tree — know whether to add the popup to it? For
//! [`FocusScope`](super::FocusScope) the answer was [`FocusStops`](super::FocusStops): a handle,
//! readable while the tree is still being built. This widget does not need one, because **the popup
//! layer is always in the tree**. Closed, it measures the band, paints nothing and ignores every
//! key; open, it paints and takes the keys. One node, one slot, no branch.
//!
//! That is what makes the slot table the right home rather than merely a possible one. Consider the
//! alternative honestly:
//!
//! * **The flag in the model.** The screen branches on it, so the popup node is *absent* on every
//!   frame the select is closed — and [`crate::slot`] states the consequence plainly: a group not
//!   entered on a frame is dropped with everything under it. The popup's highlight and scroll offset
//!   would be reclaimed the moment it closed. For the highlight that is arguably right (an S60 popup
//!   opens on the current value, which is what this widget does anyway), but the *scroll offset* is
//!   not application state by any reading, and neither is "which of my forty options is under the
//!   cursor mid-scroll". It also puts `Msg::OpenThemePicker` and `Msg::CloseThemePicker` in the
//!   application's enum — two messages per drop-down whose entire content is "the user pressed the
//!   key that was already on the screen", which is the routing this crate exists to delete.
//! * **The flag in the slot table with a conditional popup node.** The worst of both: the branch is
//!   still there, so the node still vanishes and the slot under it is still reclaimed, and now the
//!   condition is read from a handle instead of from the model. This is the shape `FocusStops`
//!   exists for and it is the wrong shape here.
//!
//! What the chosen design costs is one always-present node per select, holding one `Rc` and
//! measuring to the band it is offered. That is the price, it is stated here rather than hidden, and
//! it buys a popup whose state survives being closed and a screen with no popup plumbing in it.
//!
//! # How an open popup wins the keys
//!
//! [`Stack`](super::Stack) offers a key to its **last layer first**, because the last layer is the
//! one painted on top. The popup is the last layer, so while it is open it is asked before the form
//! underneath and it consumes everything — `Up`/`Down` move the highlight, `Select` commits,
//! `Backspace`/`End`/the Back softkey cancel, and every other key is swallowed so a stray press
//! cannot leak to the screen behind. That last part is `symbian_ui::Select`'s rule, not a new one.
//!
//! One lock is not enough for something this easy to get subtly wrong, so there is a second: the
//! *field* declines every key while the popup is open. If a caller stacks the layers the wrong way
//! round, or reaches the field through some path a `Stack` is not on, the row still does not answer
//! for a press that visibly landed on the popup. `a_field_whose_popup_is_open_answers_nothing` is
//! the test.
//!
//! # The one thing the softkey bar takes away
//!
//! [`Screen::handle_key`](super::Screen) offers the key to the softkey bar **first and
//! unconditionally** — see [`crate::keys`] for why that is structural and not an ordering. The bar
//! owns `Select`, `Enter`, `Softkey(..)` and `End`.
//!
//! So: **a screen carrying a select must leave the action slot unlabelled.** A screen with
//! `.on_action("Open", ..)` on it takes every `Select` press before the content is asked, and the
//! drop-down simply never opens — no panic, no warning, a field that does nothing. The same applies
//! to cancelling: with `.on_back("Back", ..)` declared, `Softkey::Right` and `End` go to the bar, so
//! the key that closes the popup on such a screen is `Backspace`, which no bar can claim. Both are
//! properties of the convention rather than of this widget, and neither is discoverable from a
//! failing test of this file, which is why they are written here.
//!
//! # Why the options are `&'static`
//!
//! A view is rebuilt every frame, so anything a widget owns is allocated every frame. `Vec<String>`
//! of options would be one allocation per option per frame for text that never changes, on a device
//! whose allocator we measure — the cost the [`Stepper`](super::Stepper) module rejects `Outbox::wrapped`
//! over, multiplied by the number of options.
//!
//! `&'static [&'static str]` is free, and it fits what a drop-down is *for*: a short, fixed set of
//! alternatives — a theme, a sort order, a refresh interval. A list of choices that comes from
//! runtime data is a different control on this hardware: push a [`ScrollList`](super::ScrollList)
//! screen, which scrolls, filters and has a title saying what is being chosen. `Box::leak` is not
//! the answer to this paragraph.

use alloc::rc::Rc;
use core::cell::Cell;

use symbian_gfx::{Canvas, Rect, Size};
use symbian_ui::{select as ui, Handled, Key, KeyEvent, SelectAction, Theme};

use crate::constraints::Constraints;
use crate::outbox::Outbox;
use crate::slot::SlotTable;
use crate::widget::{hash_i32, hash_str, KeyCtx, Widget, WidgetHash};
use crate::widgets::Node;

/// Where a new choice goes, and how to name it once it gets there.
///
/// An alias for the reason [`Stepper`](super::Stepper)'s is one: the pair is spelled three times,
/// and `Option<(Outbox<M>, fn(usize) -> M)>` in a struct definition reads as machinery rather than
/// as the one thing it is.
type Report<M> = (Outbox<M>, fn(usize) -> M);

/// Everything about a drop-down that outlives the frame that drew it.
///
/// `Copy`, so it can live in a `Cell` rather than a `RefCell` — the choice
/// [`FocusHook`](super::focus::FocusHook) documents: no borrow flag to get wrong and no runtime
/// panic path in a key dispatch, on a device whose whole failure report is a dialog with a number
/// in it.
#[derive(Copy, Clone)]
struct SelectState {
    /// The imperative widget, kept whole rather than picked apart.
    ///
    /// It holds the open flag, the popup's [`ListState`](symbian_ui::ListState) and the popup's
    /// recorded geometry, and it holds the *arithmetic* — what `Down` does at the last option, what
    /// cancelling means, which keys a modal popup must swallow. Keeping the struct means this file
    /// contains none of that. Its own `index` is overwritten from the model on every frame that
    /// reads it, so the copy cannot drift: see [`SelectField::handle_key`].
    inner: ui::Select,
    /// Whether the popup layer has ever been placed in the tree.
    ///
    /// Set by the popup's `measure` and `draw` — the two things only a placed node gets — and never
    /// cleared. See [`Select::orphaned`] for what it is for.
    mounted: bool,
    /// Presses refused because the popup layer was never placed.
    orphaned: u32,
}

/// A drop-down over a fixed set of options: reports the new choice, owns nothing that matters.
///
/// A builder rather than a widget, because it produces two nodes. See [`SelectParts`].
pub struct Select<M> {
    state: Rc<Cell<SelectState>>,
    options: &'static [&'static str],
    /// The chosen index **as the model says it is**. This widget never writes it.
    selected: usize,
    focused: bool,
    out: Option<Report<M>>,
}

impl<M: 'static> Select<M> {
    /// A drop-down showing `options[selected]`.
    ///
    /// Takes the slot table for the open flag and the popup's highlight, exactly as
    /// [`ScrollList`](super::ScrollList) takes it for a scroll offset — and, like that one, this is
    /// a positional slot: see [`crate::slot`] on what makes positional identity stable, and wrap a
    /// conditional select in [`SlotTable::group`](crate::slot::SlotTable::group).
    ///
    /// An out-of-range `selected` is not corrected here. `symbian_ui::Select` clamps where it
    /// matters — the popup opens on `min(selected, len - 1)` and a missing option draws as nothing
    /// — and clamping in the constructor would mean this widget disagreed with the model about what
    /// the model said.
    pub fn new(
        slots: &mut SlotTable,
        options: &'static [&'static str],
        selected: usize,
    ) -> Self {
        let state = slots
            .use_state_with(|| {
                Rc::new(Cell::new(SelectState {
                    inner: ui::Select::new(selected),
                    mounted: false,
                    orphaned: 0,
                }))
            })
            .clone();
        Self { state, options, selected, focused: false, out: None }
    }

    /// Whether this field has the cursor. Only a focused select opens.
    pub fn focused(mut self, on: bool) -> Self {
        self.focused = on;
        self
    }

    /// Where a committed choice goes, and how to say it.
    ///
    /// `msg` receives the *new index*. A `fn` pointer and not `Outbox::wrapped`, for the reason
    /// [`Stepper`](super::Stepper) sets out at length: `wrapped` allocates an `Rc` and boxes a
    /// closure per call, and the call is in `view`, which runs every frame. A tuple-variant
    /// constructor coerces to `fn(usize) -> M`, so the call site reads the same and the widget holds
    /// a `Copy` function pointer.
    pub fn out(mut self, out: Outbox<M>, msg: fn(usize) -> M) -> Self {
        self.out = Some((out, msg));
        self
    }

    /// Whether the popup is showing, readable while the tree is still being built.
    ///
    /// Not needed to place the popup — the popup layer is unconditional, which is the whole point of
    /// the module note on where the flag lives. It is here for the caller
    /// [`FocusStops`](super::FocusStops) exists for: a softkey label that has to say `Cancel` while
    /// a list query is up, decided outside the tree entirely.
    pub fn is_open(&self) -> bool {
        self.state.get().inner.is_open()
    }

    /// Which option the popup is highlighting. Equals the chosen index until the cursor moves.
    pub fn highlight(&self) -> usize {
        self.state.get().inner.highlight()
    }

    /// How many presses were refused because the popup layer was never placed in the tree.
    ///
    /// A field whose popup nobody placed is the one way to misuse this widget that the types cannot
    /// catch: [`SelectParts::field`] hands the popup out, and nothing can force a screen to put it
    /// somewhere. Left unchecked, opening that field would set a flag nobody paints and hand the
    /// popup keys it never receives — a form where one row silently stops navigating.
    ///
    /// So the press is dropped and counted, which is the answer [`OnKey::on`](super::OnKey::on)
    /// already reached for a binding it must refuse: a panic is a dead application on this hardware,
    /// and a field that does not open is a bug you can survive long enough to read a counter.
    ///
    /// **Assert `orphaned() == 0` in a test of any screen with a drop-down on it.** Nothing else
    /// will tell you — the refusal is quiet by design, and its symptom reads as the crate being
    /// broken rather than as the crate having declined you.
    pub fn orphaned(&self) -> u32 {
        self.state.get().orphaned
    }

    /// The two nodes: the field, and the popup that belongs to it.
    pub fn build(self) -> SelectParts {
        let Self { state, options, selected, focused, out } = self;
        SelectParts {
            field: Node::leaf(SelectField {
                state: Rc::clone(&state),
                options,
                selected,
                focused,
            }),
            popup: Node::leaf(SelectPopup { state, options, selected, out }),
        }
    }
}

/// The field and its popup, from one call.
///
/// Two nodes and not a tuple, because a tuple of two `Node`s is a pair of identical types in an
/// order nobody can remember, and getting it backwards puts the popup on the settings row and the
/// row on top of the screen.
#[must_use = "the popup must be placed at the screen level or the field will not open"]
pub struct SelectParts {
    /// The closed field: the chosen option, right-aligned. Goes where the value goes — a
    /// [`ListItem::trailing`](super::ListItem), a [`FieldRow`](super::FieldRow), a row of its own.
    pub field: Node,
    /// The popup, which must be the **last layer of a [`Stack`](super::Stack)** covering the content
    /// band. Last, because [`Stack`](super::Stack) paints layers in order and offers keys to the top
    /// one first; anything else and the form underneath answers for a press that landed on the
    /// popup.
    pub popup: Node,
}

impl SelectParts {
    /// The field, stashing the popup in `slot` for the screen to place.
    ///
    /// The call this widget is meant to be used through, because the field is built deep inside a
    /// form — in a [`FocusScope::stop`](super::FocusScope::stop) closure that is the only place the
    /// focus flag exists — while the popup belongs at the top of the screen. One expression puts
    /// each half where it goes and there is no second call to forget:
    ///
    /// ```ignore
    /// .stop(|f| Select::new(slots, THEMES, m.theme).focused(f).out(out.clone(), Msg::Theme)
    ///              .build().field(&mut popup))
    /// ```
    pub fn field(self, slot: &mut Option<Node>) -> Node {
        *slot = Some(self.popup);
        self.field
    }
}

/// The closed field: one line, the chosen option against the right edge.
struct SelectField {
    state: Rc<Cell<SelectState>>,
    options: &'static [&'static str],
    selected: usize,
    focused: bool,
}

impl Widget for SelectField {
    fn focus_state(&self) -> Option<bool> {
        Some(self.focused)
    }

    fn content_hash(&self) -> WidgetHash {
        // The options, because they decide the width — `value_width` reserves the widest one, so a
        // field is the same size whichever is chosen and adding an option genuinely resizes it.
        //
        // Left out, all deliberately:
        //
        // * `selected`, because the reservation is the *widest* option and not the current one. That
        //   is the `STEPPER_W` lesson: a field sized to its own value shuffles the caption beside it
        //   sideways on every commit. Folding it in would re-measure the row on every change to
        //   produce the same number.
        // * `focused` and the open flag, because both pick colours and neither moves a pixel of the
        //   box. The open flag is worse than useless here: it would re-measure every row of the form
        //   the frame a popup appears somewhere else on the screen.
        // * the band, because it arrives as the offer, which the cache already keys on.
        //
        // Never zero: zero means "re-measure me every frame", and one zero here would put the
        // enclosing row — and, through `Group::content_hash`, the whole screen above it — on the slow
        // path for ever.
        let mut h = hash_str(0, "select");
        h = hash_i32(h, self.options.len() as i32);
        for o in self.options {
            h = hash_str(h, o);
        }
        h
    }

    fn measure(&self, constraints: Constraints, theme: &Theme<'_>) -> Size {
        // One line tall, not the band. A field that reported the band's height would still *look*
        // right — `draw_text_in` centres in whatever rect it is handed — and would have lied to
        // every row that asked how big it is, and to every alignment computed from that answer.
        let w = ui::value_width(self.options, theme);
        let h = ui::value_height(constraints.max_h, theme);
        constraints.constrain(Size::new(w, h))
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        // The model's option, never the popup's highlight: the highlight is what the user is
        // pointing at and the field shows what they have chosen. `value_box` against its own rect
        // does the vertical centring, which matters because `CrossAlign::Stretch` on a list row
        // hands this widget the whole 38-pixel band and not the line it measured.
        let label = self.options.get(self.selected).copied().unwrap_or("");
        ui::draw_value(c, ui::value_box(rect, theme), theme, label, self.focused);
    }

    fn handle_key(&self, ev: KeyEvent, _rect: Rect, _cx: &mut KeyCtx<'_>) -> Handled {
        if !self.focused {
            // Two selects on one form and one press: without the flag both would open.
            return Handled::Ignored;
        }
        let mut s = self.state.get();
        if s.inner.is_open() {
            // The second lock on the door the module note describes. The popup is the top layer of
            // a `Stack` and is asked first, so this is unreachable on a screen assembled the way
            // the docs say — and reachable on one that stacked its layers the other way round,
            // where the row answering for a press that landed on the popup would be far harder to
            // see than a popup that does not respond.
            return Handled::Ignored;
        }
        if !s.mounted {
            // Nobody placed the popup. Opening would set a flag no node paints and hand keys to a
            // widget that is not in the tree — a row that stops navigating and shows nothing. See
            // `Select::orphaned`.
            if matches!(ev.key, Key::Select) {
                s.orphaned += 1;
                self.state.set(s);
            }
            return Handled::Ignored;
        }
        // The model's index goes in before the key is offered, so the copy in the slot cannot drift
        // from what the application believes: the popup opens on the model's choice even if the last
        // commit was rejected by `update`.
        s.inner.set(self.selected);
        let (handled, _) = s.inner.handle_key(ev, self.options);
        self.state.set(s);
        // Closed, `symbian_ui::Select` consumes `Select` and declines everything else — which is
        // what lets the enclosing `FocusScope` keep navigating. A field that ate `Down` would trap
        // the cursor on the one row nobody can get past.
        handled
    }
}

/// The popup layer: nothing at all until it is open, and then the whole band's worth of keys.
struct SelectPopup<M> {
    state: Rc<Cell<SelectState>>,
    options: &'static [&'static str],
    selected: usize,
    out: Option<Report<M>>,
}

impl<M> SelectPopup<M> {
    /// Note that a placed node exists, so the field is allowed to open. See [`Select::orphaned`].
    ///
    /// Called from both `measure` and `draw` on purpose: `measure` is skipped on a cache hit, and
    /// `draw` is not — but `draw` runs after the first key of a frame in some hosts, and `measure`
    /// runs before every one of them on the first frame. Between the two, a placed popup is marked
    /// before any key can reach the field that opens it.
    fn mount(&self) {
        let mut s = self.state.get();
        if !s.mounted {
            s.mounted = true;
            self.state.set(s);
        }
    }
}

impl<M: 'static> Widget for SelectPopup<M> {
    fn content_hash(&self) -> WidgetHash {
        // A constant, and nothing folded in — not the options, not the open flag. This node measures
        // to whatever it is offered whether it is showing anything or not, which is deliberate: a
        // popup that measured to its own content would change the size of the layer it is in, and a
        // `Stack` layer that resized would move the form underneath it the frame a drop-down opened.
        //
        // Not zero: a volatile child makes its whole ancestry volatile — `Group::node` says so — so
        // a zero here would put every screen carrying a drop-down on the slow path.
        hash_str(0, "select-popup")
    }

    fn measure(&self, constraints: Constraints, _theme: &Theme<'_>) -> Size {
        self.mount();
        // Everything it is offered, exactly as a `Stack` layer does — see `Stack::measure` on why a
        // layer that shrank to its content would move the band its callers are trying to hold still.
        constraints.constrain(Size::new(constraints.max_w, constraints.max_h))
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        self.mount();
        let mut s = self.state.get();
        if !s.inner.is_open() {
            // Closed, this node is a hole in the layer stack: no ink, so the form underneath is
            // simply what the screen looks like. It is still measured and still placed, which is
            // what keeps the highlight and the scroll offset alive across the close.
            return;
        }
        // `draw_popup` records the viewport and row height as it paints, so it needs the state
        // written back. That is also why this cannot take `&self` state by value and forget it.
        s.inner.draw_popup(c, ui::popup_box(rect, self.options.len(), theme), theme, self.options);
        self.state.set(s);
    }

    fn handle_key(&self, ev: KeyEvent, rect: Rect, cx: &mut KeyCtx<'_>) -> Handled {
        let mut s = self.state.get();
        if !s.inner.is_open() {
            // Closed, the layer declines everything — which is what lets the form underneath keep
            // its navigation. A layer that consumed keys while invisible would be a screen where
            // nothing works and nothing shows why.
            return Handled::Ignored;
        }
        // The geometry from the rect this layer was placed at, not from the last paint. `draw_popup`
        // records it too, and relying on that alone means the first key after opening is answered
        // against a one-pixel viewport — see `set_popup_metrics` for what that looks like on screen.
        let area = ui::popup_box(rect, self.options.len(), cx.theme);
        s.inner.set_popup_metrics(area.height(), cx.theme.metrics.row_h);
        // The model's index, so `Changed` fires on a genuine change and not on a commit that agrees
        // with what the application already believes.
        s.inner.set(self.selected);
        let (handled, action) = s.inner.handle_key(ev, self.options);
        self.state.set(s);
        if let SelectAction::Changed(i) = action {
            if let Some((out, msg)) = &self.out {
                out.push(msg(i));
            }
        }
        // Open, `symbian_ui::Select` consumes everything: the popup is modal and a key that leaked
        // past it would move the form underneath while a list query was on top of it.
        handled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout;
    use crate::widget::with_key_ctx;
    use crate::widgets::Stack;
    use crate::UiCache;
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use symbian_gfx::Size as GSize;
    use symbian_ui::{testing, Palette, Softkey};

    const OPTS: &[&str] = &["Dark", "Light", "S60", "IRC"];
    /// Options whose labels the one-glyph test atlas can actually paint. See
    /// `the_real_atlas_paints_the_option_so_the_pixel_tests_below_can_fail` for why that matters.
    const AAA: &[&str] = &["a", "aa", "aaa"];

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Msg {
        SetTheme(usize),
    }

    /// A list row's band, and the `Stretch` a row applies.
    const ROW: Rect = Rect { x0: 0, y0: 0, x1: 120, y1: 38 };
    /// A content band, the shape a `Stack` layer gets.
    const BAND: Rect = Rect { x0: 0, y0: 0, x1: 240, y1: 200 };

    /// One select, its two nodes, and the outbox it reports to — over a table that survives frames.
    struct Rig {
        slots: SlotTable,
        out: Outbox<Msg>,
        selected: usize,
        focused: bool,
        options: &'static [&'static str],
    }

    impl Rig {
        fn new(selected: usize) -> Self {
            Self {
                slots: SlotTable::new(),
                out: Outbox::new(),
                selected,
                focused: true,
                options: OPTS,
            }
        }

        /// Build the pair for one frame and hand both nodes to `f`, having measured and placed each
        /// of them the way a screen would.
        ///
        /// Two separate placements rather than one tree, because that is the truth of this widget:
        /// the field is somewhere inside a form and the popup is a layer over the whole band, and a
        /// test that put them in one row would be testing a screen nobody writes.
        fn frame<R>(&mut self, f: impl FnOnce(&Node, &Node) -> R) -> R {
            self.slots.begin_frame();
            let sel = Select::<Msg>::new(&mut self.slots, self.options, self.selected)
                .focused(self.focused)
                .out(self.out.clone(), Msg::SetTheme);
            let parts = sel.build();
            testing::with_theme(Palette::DARK, |theme| {
                let mut fc = UiCache::with_capacity(parts.field.slot_count());
                layout::place_frame(&parts.field, ROW, &mut fc, theme);
                let mut pc = UiCache::with_capacity(parts.popup.slot_count());
                layout::place_frame(&parts.popup, BAND, &mut pc, theme);
                f(&parts.field, &parts.popup)
            })
        }

        /// Press a key at the popup first and then, if it declined, at the field — the order a
        /// `Stack` produces. Returns who took it.
        fn press(&mut self, key: Key) -> Handled {
            self.frame(|field, popup| {
                with_key_ctx(|cx| {
                    let ev = KeyEvent::new(key);
                    match popup.handle_key_at(ev, BAND, cx) {
                        Handled::Consumed => Handled::Consumed,
                        Handled::Ignored => field.handle_key_at(ev, ROW, cx),
                    }
                })
            })
        }

        /// The state, read the way a screen reads it: through a freshly built builder.
        fn probe<R>(&mut self, f: impl FnOnce(&Select<Msg>) -> R) -> R {
            self.slots.begin_frame();
            let sel = Select::<Msg>::new(&mut self.slots, self.options, self.selected);
            f(&sel)
        }

        fn is_open(&mut self) -> bool {
            self.probe(|s| s.is_open())
        }

        fn highlight(&mut self) -> usize {
            self.probe(|s| s.highlight())
        }
    }

    /// `Node::handle_key` for a test: a node is a leaf or a group and both answer keys, but only
    /// through the layout pass. This is the two-line version of what a `Stack` does per layer.
    trait NodeKey {
        fn handle_key_at(&self, ev: KeyEvent, rect: Rect, cx: &mut KeyCtx<'_>) -> Handled;
    }

    impl NodeKey for Node {
        fn handle_key_at(&self, ev: KeyEvent, rect: Rect, cx: &mut KeyCtx<'_>) -> Handled {
            let mut cache = UiCache::with_capacity(self.slot_count());
            layout::place_frame(self, rect, &mut cache, cx.theme);
            layout::dispatch_key_node(self, 0, ev, &cache, cx)
        }
    }

    // ------------------------------------------------------------------ the closed field

    #[test]
    fn a_closed_select_shows_the_option_the_model_chose() {
        // Under the real atlases, because the one-glyph test atlas paints nothing for "Dark" and a
        // comparison there would pass whatever `draw` did.
        with_real_theme(|theme| {
            let bg = theme.palette.bg.mid().to_rgb565().0;
            let first = paint_field(theme, 0);
            let third = paint_field(theme, 2);
            assert!(first.iter().any(|&p| p != bg), "nothing was painted at all");
            assert_ne!(first, third, "the chosen option does not reach the canvas");
        });
    }

    #[test]
    fn the_field_reserves_the_widest_option_so_the_caption_beside_it_cannot_shuffle() {
        // The `STEPPER_W` lesson, measured instead of constant: every choice is the same width, so a
        // label to the left of a drop-down does not move when the user commits.
        with_real_theme(|theme| {
            let widths: Vec<i32> = (0..OPTS.len())
                .map(|i| field_of(i).measure(Constraints::loose(320, 38), theme).w)
                .collect();
            assert!(widths.windows(2).all(|w| w[0] == w[1]), "widths differ: {widths:?}");
            // And the reservation is the widest option's width and not some other number — the
            // negative control, without which the assertion above is satisfied by returning zero.
            assert_eq!(widths[0], ui::value_width(OPTS, theme));
            assert!(widths[0] > theme.fonts.body.measure("S60"), "it reserved the narrowest");
        });
    }

    #[test]
    fn the_stretch_a_list_row_applies_does_not_stretch_the_value() {
        // `CrossAlign::Stretch` hands the field the whole 38-pixel band, not the line it measured.
        // Asserted as containment rather than as "the ink is centred", for the reason `stepper.rs`
        // gives: `draw_text_in` centres in whatever rect it is given, so a value drawn straight into
        // the band lands in the same place while still reporting the wrong size.
        with_real_theme(|theme| {
            let slot = ui::value_box(ROW, theme);
            assert!(slot.height() < ROW.height(), "the containment below would prove nothing");
            let rows = inked_rows(theme, &paint_field(theme, 0));
            assert!(!rows.is_empty());
            assert!(
                rows.iter().all(|&y| y >= slot.y0 && y < slot.y1),
                "ink at rows {rows:?} escaped the slot {slot:?}"
            );
            // The other half of the control: that `inked_rows` tracks the geometry at all. Six
            // pixels lower and every inked row moves three, because the box is centred in what is
            // left.
            let lower = Rect { y0: ROW.y0 + 6, ..ROW };
            let (_, buf) = testing::with_canvas(GSize::new(ROW.width(), ROW.height()), |c| {
                c.clear(theme.palette.bg.mid());
                field_of(0).draw(c, lower, theme);
            });
            let moved = inked_rows(theme, &buf);
            assert_eq!(moved, rows.iter().map(|y| y + 3).collect::<Vec<_>>());
        });
    }

    #[test]
    fn the_field_measures_one_line_and_not_the_band_it_was_offered() {
        with_real_theme(|theme| {
            let got = field_of(0).measure(Constraints::loose(320, 38), theme);
            assert_eq!(got.h, ui::value_height(38, theme));
            assert_eq!(got.h, theme.fonts.body.line_height());
            assert!(got.h < 38);
        });
    }

    #[test]
    fn the_digest_is_not_zero_and_moves_only_with_the_options() {
        // Not zero, because zero means "re-measure me every frame" and would put the whole screen
        // above this row on the slow path.
        let a = field_of(0);
        assert_ne!(a.content_hash(), 0);
        // The chosen option does not change the size, so it must not change the digest: the
        // reservation is the widest option either way.
        assert_eq!(a.content_hash(), field_of(3).content_hash());
        // Focus is a colour.
        assert_eq!(a.content_hash(), field_focused(0, false).content_hash());
        // A different set of options is a different width, and that must be seen.
        let other = SelectField {
            state: Rc::new(Cell::new(fresh_state(0))),
            options: AAA,
            selected: 0,
            focused: true,
        };
        assert_ne!(a.content_hash(), other.content_hash());
    }

    #[test]
    fn a_closed_select_leaves_navigation_alone() {
        // What keeps the cursor able to leave the row. A field that consumed `Down` would trap the
        // focus on the one row nobody can get past.
        let mut rig = Rig::new(1);
        for key in [Key::Up, Key::Down, Key::Left, Key::Right, Key::Backspace] {
            assert_eq!(rig.press(key), Handled::Ignored, "{key:?}");
        }
        assert!(!rig.is_open());
        assert!(rig.out.is_empty());
    }

    #[test]
    fn an_unfocused_select_ignores_keys() {
        // Two drop-downs on one form and one press: without the flag both would open.
        let mut rig = Rig::new(0);
        rig.focused = false;
        assert_eq!(rig.press(Key::Select), Handled::Ignored);
        assert!(!rig.is_open());
        assert!(rig.out.is_empty());
    }

    // ------------------------------------------------------------------ opening and committing

    #[test]
    fn opening_highlights_the_option_the_model_chose() {
        let mut rig = Rig::new(2);
        assert_eq!(rig.press(Key::Select), Handled::Consumed);
        assert!(rig.is_open());
        assert_eq!(rig.highlight(), 2, "a popup that opened on the wrong row");
        assert!(rig.out.is_empty(), "opening is not a change");
    }

    #[test]
    fn up_and_down_move_the_highlight_without_committing() {
        let mut rig = Rig::new(0);
        rig.press(Key::Select);
        assert_eq!(rig.press(Key::Down), Handled::Consumed);
        assert_eq!(rig.press(Key::Down), Handled::Consumed);
        assert_eq!(rig.highlight(), 2);
        assert_eq!(rig.press(Key::Up), Handled::Consumed);
        assert_eq!(rig.highlight(), 1);
        // Nothing reported, and the model untouched: a drop-down that pushed a message per arrow
        // would run `update` — and any `Cmd` it returned — four times for one choice.
        assert!(rig.out.is_empty());
        assert!(rig.is_open());
    }

    #[test]
    fn the_centre_key_commits_the_highlight_and_reports_the_new_index() {
        let mut rig = Rig::new(0);
        rig.press(Key::Select);
        rig.press(Key::Down);
        rig.press(Key::Down);
        assert_eq!(rig.press(Key::Select), Handled::Consumed);
        assert_eq!(rig.out.take(), alloc::vec![Msg::SetTheme(2)]);
        assert!(!rig.is_open(), "committing closes the popup");
        // And the widget did not write the model: the field still shows what the model says, which
        // is the rule the whole crate runs on. A widget that changed its own copy would show the new
        // option for one frame and then be overwritten by a `view` built from the old model.
        assert_eq!(rig.selected, 0);
    }

    #[test]
    fn committing_the_option_that_is_already_chosen_reports_nothing() {
        // A message saying "set it to what it already is" is an `update`, a `Cmd` and a redraw for
        // nothing — the rule `Stepper` states for a step that does not move.
        let mut rig = Rig::new(1);
        rig.press(Key::Select);
        assert_eq!(rig.press(Key::Select), Handled::Consumed);
        assert!(rig.out.is_empty());
        assert!(!rig.is_open());
    }

    #[test]
    fn cancel_closes_without_reporting() {
        // Both keys, because the red key and Backspace both mean "get me out" on this hardware, and
        // because a screen that labels its Back softkey takes `End` before the content is asked —
        // see the module note. Backspace is the one that always arrives.
        for key in [Key::End, Key::Backspace, Key::Softkey(Softkey::Right)] {
            let mut rig = Rig::new(1);
            rig.press(Key::Select);
            rig.press(Key::Down);
            assert_eq!(rig.highlight(), 2, "{key:?}: nothing to cancel");
            assert_eq!(rig.press(key), Handled::Consumed, "{key:?}");
            assert!(!rig.is_open(), "{key:?} did not close the popup");
            assert!(rig.out.is_empty(), "{key:?} committed the highlight");
        }
    }

    #[test]
    fn a_field_whose_popup_is_open_answers_nothing() {
        // The second lock. On a correctly assembled screen the popup is asked first and this is
        // unreachable; on one that stacked its layers the wrong way round, a row answering for a
        // press that landed on the popup is far harder to see than a popup that does not respond.
        let mut rig = Rig::new(0);
        rig.press(Key::Select);
        let taken = rig.frame(|field, _popup| {
            with_key_ctx(|cx| field.handle_key_at(KeyEvent::new(Key::Down), ROW, cx))
        });
        assert_eq!(taken, Handled::Ignored);
        assert_eq!(rig.highlight(), 0, "the field moved the popup's highlight");
    }

    // ------------------------------------------------------------------ the stack

    #[test]
    fn an_open_popup_does_not_let_the_row_underneath_answer() {
        // The property the whole two-node design exists for, through a real `Stack`: the popup is
        // the last layer, `Stack` offers keys to the top layer first, so while it is open the form
        // underneath never hears the D-pad.
        let seen = Rc::new(RefCell::new(Vec::<Key>::new()));
        let mut slots = SlotTable::new();

        // Frame one: closed. The negative control, and it must fire — without it "the layer
        // underneath saw nothing" is satisfied by a stack that never dispatches at all.
        let closed = stack_press(&mut slots, &seen, 0, Key::Down);
        assert_eq!(closed, Handled::Consumed, "the layer underneath was never asked");
        assert_eq!(&*seen.borrow(), &[Key::Down], "the control did not fire");
        seen.borrow_mut().clear();

        // Open it, through the field — which is under the popup, and reachable because a closed
        // popup declines.
        assert_eq!(stack_press(&mut slots, &seen, 0, Key::Select), Handled::Consumed);
        assert_eq!(&*seen.borrow(), &[] as &[Key], "the row under the field took the press");

        // Frame two: open. Same stack, same key, and now nothing below hears it.
        assert_eq!(stack_press(&mut slots, &seen, 0, Key::Down), Handled::Consumed);
        assert!(seen.borrow().is_empty(), "the row underneath answered: {:?}", seen.borrow());
    }

    #[test]
    fn a_field_whose_popup_was_never_placed_refuses_to_open_and_counts_it() {
        // The one misuse the types cannot catch — see `Select::orphaned`. Opening a field whose
        // popup nobody placed would set a flag no node paints and hand keys to a widget that is not
        // in the tree: a form where one row silently stops navigating.
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let sel = Select::<Msg>::new(&mut slots, OPTS, 0).focused(true);
        let parts = sel.build();
        // The popup is dropped here, exactly as a screen that forgot it would drop it.
        let field = parts.field;
        let taken = with_key_ctx(|cx| field.handle_key_at(KeyEvent::new(Key::Select), ROW, cx));
        assert_eq!(taken, Handled::Ignored, "it opened with nothing to open into");

        slots.begin_frame();
        let after = Select::<Msg>::new(&mut slots, OPTS, 0);
        assert!(!after.is_open());
        assert_eq!(after.orphaned(), 1, "the refusal was not counted");
    }

    #[test]
    fn a_closed_popup_paints_nothing_and_an_open_one_paints_over_the_band() {
        // Under the real atlases: the popup is a frame, a fill and four labels, and only the labels
        // are text — but a test that could not see the text would not notice a popup drawn empty.
        with_real_theme(|theme| {
            let bg = theme.palette.bg.mid().to_rgb565().0;
            let mut slots = SlotTable::new();
            let closed = paint_popup(&mut slots, theme, false);
            assert!(closed.iter().all(|&p| p == bg), "a closed popup painted something");
            let open = paint_popup(&mut slots, theme, true);
            assert!(open.iter().any(|&p| p != bg), "an open popup painted nothing");

            // And it painted where `popup_box` says, not over the whole band: the rows above the
            // popup are untouched, which is what keeps the screen behind a list query visible.
            let area = ui::popup_box(BAND, OPTS.len(), theme);
            assert!(area.y0 > BAND.y0, "the popup filled the band and proves nothing");
            let w = BAND.width();
            for y in 0..area.y0 {
                assert!(
                    (0..w).all(|x| open[(y * w + x) as usize] == bg),
                    "the popup painted at row {y}, above its own box {area:?}"
                );
            }
        });
    }

    #[test]
    fn the_popup_measures_the_band_it_was_offered_and_its_digest_is_not_zero() {
        // A layer that shrank to its content would move the form underneath the frame a drop-down
        // opened — the defect `Stack::measure` documents. And a zero digest would make every screen
        // carrying a select volatile through `Group::node`.
        with_real_theme(|theme| {
            let mut slots = SlotTable::new();
            slots.begin_frame();
            let parts = Select::<Msg>::new(&mut slots, OPTS, 0).build();
            let popup = match parts.popup {
                Node::Leaf(w) => w,
                Node::Group(_) => panic!("the popup is a leaf"),
            };
            assert_eq!(
                popup.measure(Constraints::tight(240, 200), theme),
                Size::new(240, 200)
            );
            assert_ne!(popup.content_hash(), 0);
        });
    }

    #[test]
    fn the_real_atlas_paints_the_option_so_the_pixel_tests_below_can_fail() {
        // The negative control every pixel assertion in this file leans on. `testing::with_theme`
        // loads an atlas holding exactly one glyph — lowercase 'a' — so "Dark", "Light", "S60" and
        // "IRC" paint nothing at all under it and every comparison would be vacuously equal. This is
        // the test that says the buffers above are real ink.
        with_real_theme(|theme| {
            let bg = theme.palette.bg.mid().to_rgb565().0;
            let painted = paint_field(theme, 0);
            assert!(painted.iter().any(|&p| p != bg), "the real atlas painted nothing");
        });
        // And the other half, so a future reader does not have to take the paragraph above on
        // trust: under the test atlas "S60" paints nothing whatsoever.
        //
        // "Dark" is the sharper illustration and the reason this is asserted on option 2. It
        // contains an 'a', so the test atlas paints exactly one letter of it in the middle of a
        // four-letter word — ink, in about the right place, from a font that has none of the rest of
        // the alphabet. A pixel test looking only for "something was drawn" would pass.
        testing::with_theme(Palette::DARK, |theme| {
            let bg = theme.palette.bg.mid().to_rgb565().0;
            assert!(paint_field(theme, 2).iter().all(|&p| p == bg), "the test atlas grew a font");
            let dark = paint_field(theme, 0);
            assert!(dark.iter().any(|&p| p != bg), "not even the 'a' of \"Dark\" arrived");
            assert!(
                paint_field(theme, 1).iter().all(|&p| p == bg),
                "\"Light\" has no 'a' in it and must be as blank as \"S60\""
            );
        });
    }

    // ------------------------------------------------------------------ scaffolding

    fn fresh_state(selected: usize) -> SelectState {
        SelectState { inner: ui::Select::new(selected), mounted: true, orphaned: 0 }
    }

    fn field_focused(selected: usize, focused: bool) -> SelectField {
        SelectField {
            state: Rc::new(Cell::new(fresh_state(selected))),
            options: OPTS,
            selected,
            focused,
        }
    }

    fn field_of(selected: usize) -> SelectField {
        field_focused(selected, true)
    }

    /// The *real* device atlases, not the one-glyph test atlas. See
    /// `the_real_atlas_paints_the_option_so_the_pixel_tests_below_can_fail`.
    fn with_real_theme<R>(f: impl FnOnce(&Theme<'_>) -> R) -> R {
        let atlases = symbian_preview::Atlases::load();
        atlases.with_fonts(|fonts| f(&symbian_ui::Theme::dark(fonts)))
    }

    /// Paint the closed field over a row band.
    fn paint_field(theme: &Theme<'_>, selected: usize) -> Vec<u16> {
        let (_, buf) = testing::with_canvas(GSize::new(ROW.width(), ROW.height()), |c| {
            c.clear(theme.palette.bg.mid());
            field_of(selected).draw(c, ROW, theme);
        });
        buf
    }

    /// Paint the popup layer over a content band, open or closed.
    fn paint_popup(slots: &mut SlotTable, theme: &Theme<'_>, open: bool) -> Vec<u16> {
        slots.begin_frame();
        let sel = Select::<Msg>::new(slots, OPTS, 0).focused(true);
        let parts = sel.build();
        if open {
            // Place the popup before pressing the field, exactly as a screen does — a field whose
            // popup has never been placed refuses to open, which is the whole of `Select::orphaned`.
            let mut cache = UiCache::with_capacity(parts.popup.slot_count());
            layout::place_frame(&parts.popup, BAND, &mut cache, theme);
            with_key_ctx(|cx| parts.field.handle_key_at(KeyEvent::new(Key::Select), ROW, cx));
        }
        let (_, buf) = testing::with_canvas(GSize::new(BAND.width(), BAND.height()), |c| {
            c.clear(theme.palette.bg.mid());
            let mut cache = UiCache::with_capacity(parts.popup.slot_count());
            layout::draw_frame(&parts.popup, BAND, &mut cache, c, theme);
        });
        buf
    }

    /// Which rows of a row-band buffer have ink in them.
    fn inked_rows(theme: &Theme<'_>, buf: &[u16]) -> Vec<i32> {
        let bg = theme.palette.bg.mid().to_rgb565().0;
        let w = ROW.width();
        (0..ROW.height()).filter(|&y| (0..w).any(|x| buf[(y * w + x) as usize] != bg)).collect()
    }

    /// A widget that consumes every key and records it — the row underneath the popup.
    struct Taker(Rc<RefCell<Vec<Key>>>);

    impl Widget for Taker {
        fn content_hash(&self) -> WidgetHash {
            hash_str(0, "taker")
        }
        fn measure(&self, c: Constraints, _t: &Theme<'_>) -> Size {
            c.constrain(Size::new(c.max_w, c.max_h))
        }
        fn draw(&self, _c: &mut Canvas<'_>, _r: Rect, _t: &Theme<'_>) {}
        fn handle_key(&self, ev: KeyEvent, _r: Rect, _cx: &mut KeyCtx<'_>) -> Handled {
            self.0.borrow_mut().push(ev.key);
            Handled::Consumed
        }
    }

    /// Build one frame of `Stack { Taker, field, popup }` — the shape a screen has, with the row
    /// underneath standing in for the form — and press `key` at it.
    fn stack_press(
        slots: &mut SlotTable,
        seen: &Rc<RefCell<Vec<Key>>>,
        selected: usize,
        key: Key,
    ) -> Handled {
        slots.begin_frame();
        let mut popup = None;
        let field = Select::<Msg>::new(slots, OPTS, selected)
            .focused(true)
            .build()
            .field(&mut popup);
        let stack = Stack::new(slots)
            .child(Taker(Rc::clone(seen)))
            .layer(field)
            .layer(popup.expect("build stashed it"));
        testing::with_theme(Palette::DARK, |theme| {
            // Placed and drawn first, as a screen does: the popup layer learns it is in the tree.
            let (_, _buf) = testing::with_canvas(GSize::new(BAND.width(), BAND.height()), |c| {
                stack.draw(c, BAND, theme);
            });
            let mut clip = symbian_ui::NoClipboard;
            let mut cx = KeyCtx::new(theme, &mut clip);
            stack.handle_key(KeyEvent::new(key), BAND, &mut cx)
        })
    }
}
