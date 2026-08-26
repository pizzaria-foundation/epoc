//! A labelled field, with the one line of help or complaint that goes under it.
//!
//! [`ListItem`](super::ListItem) is the row you *walk past*: a caption on the left, what it is set to
//! on the right, and a press takes you somewhere. This is the field you are *standing in* — the
//! editor is on the screen and the keys are going into it. jQuery Mobile calls it a field container,
//! KaiOS a labelled input, S60 the settings-item edit page, and until now every form in this SDK
//! assembled one by hand: a column, a small dim `Text`, the field, and a third `Text` whose colour
//! depended on whether the last submit had failed.
//!
//! ```ignore
//! FieldRow::new("access point")
//!     .hint("chosen automatically when empty")
//!     .error(model.ap_error.as_deref().unwrap_or_default())   // replaces the hint
//!     .control(TextField::new(slots).focused(here))
//!     .focused(here)
//!     .build()
//! ```
//!
//! # The label goes above, not beside
//!
//! Beside is the shape a desktop form takes and it is wrong at 320 pixels. `access point` is about
//! eighty pixels of small text, and the caption column has to be as wide as the *longest* caption on
//! the form or the fields do not line up — so a form with `preferred connection` in it spends half
//! its width on words and leaves the editor a hundred and forty pixels. A hundred and forty pixels is
//! four or five characters short of a URL, and [`chrome::text_field`](symbian_ui::chrome::text_field)
//! answers that by scrolling the text horizontally under the caret: correct, and it means the user
//! types an address they can only ever see the tail of.
//!
//! Above also happens to be what the device does. S60's settings-item edit page puts the caption on
//! its own line and gives the editor the full width, and the beside-shape already exists here as
//! [`ListItem::trailing_value`](super::ListItem::trailing_value) — for the row you walk past, where
//! the value is a word and not something being typed. Two builders for one shape would be two places
//! to fix a margin.
//!
//! # An error takes the hint's place
//!
//! It does not stack under it, and the reason is the 240-pixel screen. A title bar and a softkey bar
//! leave about two hundred pixels of body; a field is a caption, an editor and a help line, so four
//! of them fill the form exactly. Errors do not arrive one at a time — a form validates on submit and
//! several fields fail together — so a stacked error line grows three or four fields at once, by a
//! line each, at the precise moment the user needs to look at one of them. The field they were
//! standing in slides down or off the bottom.
//!
//! Replacing keeps a field the same height in every state, so nothing moves and the cursor stays
//! under the eye. It also loses nothing worth keeping: a hint says how to fill the field in and an
//! error says why what is in it will not do, and once there is an error the hint is advice about a
//! problem that already happened.
//!
//! The two are held in **separate fields** and resolved in [`build`](FieldRow::build) rather than
//! being one `enum` that can only hold the last one set. One field would make
//! `.hint(..).error(..)` and `.error(..).hint(..)` different rows, which is the defect
//! [`Part`](super::list_item) was invented for one file over — a builder whose result depends on the
//! order it reads well in. `the_order_of_hint_and_error_does_not_matter` is the assertion.
//!
//! # `focused` is the only thing saying which field is live
//!
//! [`chrome::text_field`](symbian_ui::chrome::text_field) bands every field the same way whether it
//! has the keyboard or not. The single difference focus makes down there is the **caret** — one pixel
//! wide, and on a form of four fields that is not a cue anybody finds. So the caption carries it, and
//! it carries it in [`Ink::Accent`], which is the colour that caret is drawn in: one signal in two
//! places rather than two signals to reconcile.
//!
//! What it does **not** do is draw a frame. The control already paints its own band, and a box around
//! a box is [`ListItem`](super::ListItem)'s doubled-selection-band lesson arriving through a different
//! door.
//!
//! It used to not reach the control either. The control arrives already built, as `impl Widget`, so
//! there was no way back through the trait to tell it anything, and the caller passed the same bool
//! twice — `.control(TextField::new(slots).focused(here)).focused(here)`. This file called that a
//! duplication it could flag and not remove, and it was wrong twice over: it cost two real bugs in
//! this SDK's own gallery, one of which shipped, and it *was* removable. The row now **asks**, through
//! [`Widget::focus_state`](crate::Widget::focus_state), and the control's answer wins because the
//! control is what actually takes the keys. `.focused()` here is the fallback for a control that does
//! not take focus at all.
//!
//! The symptom is worth remembering, because it is what a duplicated flag looks like when it goes
//! wrong: the caption lit in the accent, the field drew identically to every other field, and the
//! only true signal on screen was the **absence** of a one-pixel caret — which reads as a thin caret,
//! not as a dead control. That second half is fixed too; see `chrome::text_field`.
//!
//! # The error ink
//!
//! [`Ink::Error`] carries it. Until recently it did not exist, and this file plus two screens in `tg`
//! all wrote their complaint in [`Ink::Unread`] — the nearest thing the palette had, and wrong for a
//! reason worth keeping: `unread` is the colour of a *count*, chosen to separate from both row
//! states, and a palette free to make that a bright green would render every error in it.
//!
//! What none of them did was reach for a literal [`Color`](symbian_gfx::Color), and that restraint is
//! why closing the gap was a one-line change. A view is built with no theme in hand, so a colour
//! written as a number is a colour no palette can ever change: the error line would have stayed that
//! exact red on a light theme, on a high-contrast theme, and on whatever comes after them.

use alloc::string::String;

use crate::layout::CrossAlign;
use crate::spacing::{Gap, Pad};
use crate::theme::FontRole;
use crate::widget::Widget;
use crate::widgets::{Column, Ink, Node, Row, Text};

/// The optional thing at the end of the caption line, held unresolved until
/// [`FieldRow::build`].
///
/// # Why it is not coloured where it is set
///
/// Its ink depends on `focused`, and a builder is written in whatever order reads well at the call
/// site — `.note("0/160").focused(here)` as often as the reverse. Resolving inside `note` would work
/// or not depending on which of the two was called first, and the wrong answer is a dim counter above
/// the field the user is typing in: legible, plausible, and wrong on exactly the field being looked
/// at. That is [`ListItem`](super::ListItem)'s `Part` and this is the same bug, so it is the same
/// answer — store what it *is*, colour it once, in `build`, when the state is finally known.
enum Part {
    /// Already built by the caller, who owns its colour.
    Given(Node),
    /// A short word or count set small at the end of the caption: `0/160`, `MB`, `required`.
    Note(String),
}

impl Part {
    /// Turn into a node, coloured for a field in this state.
    fn build(self, quiet: Ink) -> Node {
        match self {
            Part::Given(n) => n,
            Part::Note(s) => Node::leaf(
                Text::new(s).font(FontRole::Small).ink(quiet).align(symbian_gfx::Align::End),
            ),
        }
    }
}

/// One field of a form: a caption, the thing being edited, and a line of help under it.
///
/// Everything but the caption is optional, and the shape follows from what was asked for — no hint
/// and no error is two lines rather than three, and a field with no control at all is a caption over
/// a value, which is what a read-only entry on the same form looks like.
pub struct FieldRow {
    label: String,
    /// The caption's weight. [`FontRole::Small`], as S60 sets a settings caption: smaller than the
    /// value it introduces, because it is the name of the thing and not the thing. A caption that
    /// wants to be a heading is a [`SectionHeader`](super::SectionHeader), which is also not a focus
    /// stop — the distinction that matters between the two.
    label_font: FontRole,
    control: Option<Node>,
    /// Quiet advice on how to fill the field in. Outranked by `error`; see the module docs.
    hint: Option<String>,
    /// Why what is in the field will not do. Never merged with the hint, never stored in the same
    /// place as it.
    error: Option<String>,
    focused: bool,
    label_end: Option<Part>,
    /// How many lines the help gets. One, deliberately — see [`FieldRow::help_lines`].
    help_lines: usize,
    pad: Pad,
    gap: Gap,
}

impl FieldRow {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            label_font: FontRole::Small,
            control: None,
            hint: None,
            error: None,
            focused: false,
            label_end: None,
            help_lines: 1,
            pad: Pad::xy(Gap::Base, Gap::None),
            // `Snug` is the scale's own name for the distance between stacked lines of text, which is
            // what this is in two of its three joins. One gap and not two: a column has one, so a
            // different distance above the control than below it would mean a second column and a
            // slot per field, and no form on this screen reads differently for it.
            gap: Gap::Snug,
        }
    }

    /// The thing being edited: a [`TextField`](super::TextField), a
    /// [`Switch`](super::Switch), a [`Button`](super::Button).
    ///
    /// It arrives already built, so **it does not learn about [`focused`](Self::focused) from here** —
    /// tell it yourself. See the module docs.
    pub fn control(mut self, w: impl Widget + 'static) -> Self {
        self.control = Some(Node::leaf(w));
        self
    }

    /// A control assembled as a group rather than a leaf — a row of two spinners, a pair of buttons.
    pub fn control_node(mut self, n: Node) -> Self {
        self.control = Some(n);
        self
    }

    /// Quiet help under the field: what happens if it is left empty, what format is expected.
    ///
    /// An empty string is *no hint* rather than a blank line, because the common call site is
    /// `.hint(model.hint_for(field))` and a model with nothing to say would otherwise cost the form a
    /// line of air per field.
    pub fn hint(mut self, text: impl Into<String>) -> Self {
        self.hint = Some(text.into()).filter(|s| !s.is_empty());
        self
    }

    /// Why the field's contents will not do. Takes the hint's place; see the module docs.
    ///
    /// Empty is *no error*, for the reason [`hint`](Self::hint) gives and one more: the natural call
    /// site is `.error(model.error.as_deref().unwrap_or_default())`, and if empty meant "an error with
    /// nothing in it" that line would silently suppress the hint of every valid field on the form.
    pub fn error(mut self, text: impl Into<String>) -> Self {
        self.error = Some(text.into()).filter(|s| !s.is_empty());
        self
    }

    /// Whether this field has the keyboard. Changes the ink and nothing else — no frame, and nothing
    /// reaches the control. See the module docs.
    pub fn focused(mut self, on: bool) -> Self {
        self.focused = on;
        self
    }

    /// Something at the end of the caption line: a counter, a unit, a badge.
    pub fn label_end(mut self, w: impl Widget + 'static) -> Self {
        self.label_end = Some(Part::Given(Node::leaf(w)));
        self
    }

    pub fn label_end_node(mut self, n: Node) -> Self {
        self.label_end = Some(Part::Given(n));
        self
    }

    /// A short count or unit at the end of the caption line, inked to match the field's state.
    ///
    /// Separate from [`label_end`](Self::label_end) for the reason
    /// [`ListItem::trailing_value`](super::ListItem::trailing_value) is separate from `trailing`: the
    /// caller has no way to know the field's state at the moment it builds a `Text`, so one handed in
    /// through `label_end` stays dim above the field being typed in.
    pub fn note(mut self, text: impl Into<String>) -> Self {
        self.label_end = Some(Part::Note(text.into()));
        self
    }

    /// Let the help line wrap to `n` lines. One by default, and the default is the considered answer.
    ///
    /// A wrapping help line makes a field as tall as its message, which is the thing the module docs
    /// argue against: the messages arrive together on submit, so the form grows under the cursor. One
    /// line truncates with the font's ellipsis, and an error that does not fit in three hundred pixels
    /// of small text — call it forty characters — is not a field-level error; it is a note or a dialog
    /// wearing a field's clothes.
    ///
    /// This is here because a two-field form on a screen with room to spare is a real case and would
    /// otherwise have to stop using the builder to get it. `0` is read as `1`.
    pub fn help_lines(mut self, n: usize) -> Self {
        self.help_lines = n.max(1);
        self
    }

    /// Override the side padding. [`Gap::Base`] across by default — the same margin a list row uses,
    /// so a caption lines up with the rows of a list on the same screen rather than sitting a few
    /// pixels in from them.
    ///
    /// Nothing down the main axis by default: the space *between* two fields belongs to the column
    /// holding them, and a field that also padded itself would double it.
    pub fn pad(mut self, pad: Pad) -> Self {
        self.pad = pad;
        self
    }

    /// The distance between the caption, the control and the help line.
    pub fn gap(mut self, gap: impl Into<Gap>) -> Self {
        self.gap = gap.into();
        self
    }

    /// The inks this field's state resolves to: one for the caption, one for everything quieter.
    ///
    /// Accent on the caption rather than [`Ink::Text`] because accent is the colour the caret is drawn
    /// in — see the module docs. `Dim` on both when the field is asleep: an unfocused caption is
    /// subordinate to the value under it, which is how S60 sets one.
    /// Whether this row should paint its focus cue.
    ///
    /// **The control decides, when it can.** It is the thing that actually takes the keys, so it is
    /// the only honest answer — and asking it is what makes `.focused()` on this row impossible to
    /// disagree with. See [`Widget::focus_state`](crate::Widget::focus_state) for the two bugs that
    /// argument is made of.
    ///
    /// `self.focused` remains the fallback, for a control that does not take focus at all: a row
    /// whose control is a `Spacer` still wants its caption to light when the cursor is on it.
    fn is_focused(&self) -> bool {
        self.control
            .as_ref()
            .and_then(|c| c.focus_state())
            .unwrap_or(self.focused)
    }

    fn inks(&self) -> (Ink, Ink) {
        if self.is_focused() {
            (Ink::Accent, Ink::Text)
        } else {
            (Ink::Dim, Ink::Dim)
        }
    }

    /// The help line, if there is anything to say. The error wins.
    ///
    /// The error's ink does **not** follow focus, unlike the hint's: a field is as wrong when the
    /// cursor is elsewhere as when it is here, and a complaint that dimmed when you navigated away
    /// from it would be a complaint you could lose by pressing Down.
    fn help(&self, quiet: Ink) -> Option<Node> {
        let (text, ink) = match (&self.error, &self.hint) {
            (Some(e), _) => (e, Ink::Error),
            (None, Some(h)) => (h, quiet),
            (None, None) => return None,
        };
        Some(Node::leaf(
            Text::new(text.as_str()).font(FontRole::Small).ink(ink).max_lines(self.help_lines),
        ))
    }

    /// The caption line: some text, and optionally something at its end.
    ///
    /// `None` returns the text **unwrapped**, which is [`ListItem::line`](super::ListItem)'s saving
    /// and its reason — an intermediate box is where a stretch stops — plus a slot a form of six
    /// fields would spend six times on nothing.
    ///
    /// # The `flex(1)` does not survive the transcription
    ///
    /// `ListItem` gives its unwrapped title `flex(1)`, and copying that here is a real bug rather than
    /// a harmless extra. A leaf's weight is read by *whichever axis its parent runs*: in `ListItem`
    /// the parent is a row and `flex(1)` means "take the leftover width", but the caption's parent
    /// here is a **column**, so the same call means "take the leftover height" — and a field placed in
    /// a band taller than its content gets a caption two hundred pixels tall with the control and the
    /// help squeezed against the bottom. Inside the caption's own `Row` the flex is correct and
    /// necessary, which is what pushes the note out to the field's right edge.
    fn line(text: Text, end: Option<Node>) -> Node {
        match end {
            // Stretched, so a caption beside a taller note is centred against it rather than hung
            // from the top of the line — the ten-pixels-high symptom, one axis over.
            Some(e) => {
                Node::Group(Row::new().align(CrossAlign::Stretch).child(text.flex(1)).node(e))
            }
            None => Node::leaf(text),
        }
    }

    pub fn build(self) -> Node {
        let (caption, quiet) = self.inks();
        let help = self.help(quiet);
        let label = Text::new(&self.label).font(self.label_font).ink(caption);
        let end = self.label_end.map(|p| p.build(quiet));

        let mut col = Column::new()
            // Load-bearing, and for a different reason than in a list row. The cross axis of a column
            // is *width*: without this the caption line is as wide as its words, so a note aligned to
            // its end sits touching the caption instead of above the field's right edge, and a control
            // that measures its own width — a `Switch`, a `Button` — is a stub under a full-width
            // label. `TextField` happens to measure the width it is offered and would have hidden all
            // of that until the first form with a switch in it.
            .align(CrossAlign::Stretch)
            .padding(self.pad)
            .gap(self.gap)
            .node(Self::line(label, end));
        if let Some(control) = self.control {
            col = col.node(control);
        }
        if let Some(help) = help {
            col = col.node(help);
        }
        Node::Group(col)
    }
}

impl From<FieldRow> for Node {
    fn from(f: FieldRow) -> Node {
        f.build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::UiCache;
    use crate::layout;
    use crate::slot::SlotTable;
    use crate::widgets::{Spacer, TextField};
    use symbian_gfx::{Rect, Size};
    use symbian_ui::{testing, Palette};

    /// A form's width, and more height than a field wants, so a child that claimed the leftover would
    /// be caught claiming it.
    const BAND: Rect = Rect { x0: 0, y0: 0, x1: 320, y1: 240 };

    /// Place `root` in a form-sized band and report every descendant's rect, in slot order.
    fn rects(root: &Node) -> Vec<Rect> {
        testing::with_theme(Palette::DARK, |theme| {
            let mut cache = UiCache::with_capacity(root.slot_count());
            layout::place_frame(root, BAND, &mut cache, theme);
            (0..root.slot_count())
                .map(|s| cache.rect(s).unwrap_or(Rect::from_xywh(0, 0, 0, 0)))
                .collect()
        })
    }

    /// Draw `root` into a form-sized canvas.
    ///
    /// # What a pixel comparison can see here
    ///
    /// `testing::with_theme` loads a test atlas holding **one glyph**: lowercase `a`. Every font role
    /// in it is the same face at the same size, so a comparison sees fills, rules, and the *position
    /// and colour of the letter `a`* — enough to catch a part in the wrong place or inked wrong, and
    /// blind to a wrong font role. Every string in these tests is lower case and contains an `a` for
    /// exactly that reason: `"Access point"` has no lowercase `a` in it and would paint nothing at all,
    /// which is how a pixel assertion turns into a constant that passes for ever.
    fn painted(root: &Node) -> Vec<u16> {
        let (_, buf) = testing::with_canvas(Size::new(320, 240), |c| {
            testing::with_theme(Palette::DARK, |theme| {
                c.clear(Palette::DARK.bg.mid());
                let mut cache = UiCache::with_capacity(root.slot_count());
                layout::draw_frame(root, BAND, &mut cache, c, theme);
            });
        });
        buf
    }

    fn base() -> i32 {
        testing::with_theme(Palette::DARK, |t| Gap::Base.resolve(t))
    }

    fn line_h() -> i32 {
        testing::with_theme(Palette::DARK, |t| FontRole::Small.line_height(t))
    }

    #[test]
    fn the_label_sits_above_the_control_and_the_help_under_it() {
        // The shape the module docs argue for, asserted on slots rather than by sniffing rects for
        // sizes: the tree of a full field is exactly `column, caption, control, help`, and a filter
        // guessing which rect was which would keep passing after the structure moved under it.
        let root = FieldRow::new("access point")
            .control(Spacer::new().height(20))
            .hint("chosen automatically when empty")
            .build();
        let got = rects(&root);
        assert_eq!(got.len(), 4, "column, caption, control, help: {got:?}");
        let (caption, control, help) = (got[1], got[2], got[3]);
        assert!(caption.y1 <= control.y0, "the caption is above the control");
        assert!(control.y1 <= help.y0, "and the help under it");
        // All three start at the same x. A caption indented from its own field reads as a heading for
        // the field below it rather than as its name.
        assert_eq!((caption.x0, control.x0, help.x0), (base(), base(), base()));
    }

    #[test]
    fn a_control_is_given_the_fields_whole_width() {
        // What `CrossAlign::Stretch` buys on a column, and the case `TextField` hides: a `Spacer` that
        // measures ten pixels is a switch or a button that measures its own width, and left unstretched
        // it is a stub sitting under a full-width caption.
        let root = FieldRow::new("aa").control(Spacer::new().width(10).height(20)).build();
        let control = rects(&root)[2];
        assert_eq!(control.width(), 320 - 2 * base(), "the control fills the field: {control:?}");
    }

    #[test]
    fn the_comparison_would_notice_if_the_stretch_were_dropped() {
        // A layout assertion that cannot fail is a constant with a test's name on it. Drop the one
        // setting the module docs call load-bearing and the same field must come out differently.
        let root = Node::Group(
            Column::new()
                .padding(Pad::xy(Gap::Base, Gap::None))
                .gap(Gap::Snug)
                .child(Text::new("aa").font(FontRole::Small).ink(Ink::Dim))
                .child(Spacer::new().width(10).height(20)),
        );
        let unstretched = rects(&root);
        assert_eq!(unstretched[2].width(), 10, "without the stretch the control keeps its own width");
        let declared = FieldRow::new("aa").control(Spacer::new().width(10).height(20)).build();
        assert_ne!(rects(&declared), unstretched, "the stretch is doing something");
    }

    #[test]
    fn the_caption_does_not_claim_the_columns_leftover_height() {
        // The `flex(1)` that does not survive being transcribed from `list_item.rs`. A leaf's weight is
        // read by whichever axis its parent runs, and this parent is a column — so a flexed caption
        // eats two hundred pixels and pins the control and the help to the bottom of the band.
        let root = FieldRow::new("aa")
            .control(Spacer::new().height(20))
            .hint("aa")
            .build();
        let got = rects(&root);
        assert_eq!(got[1].height(), line_h(), "one line of small text, not the whole band: {got:?}");
        assert_eq!(got[3].height(), line_h(), "and the help line is a line too");

        // The negative control: the same tree with the caption flexed, which must place differently or
        // the assertion above is measuring nothing.
        let flexed = Node::Group(
            Column::new()
                .align(CrossAlign::Stretch)
                .padding(Pad::xy(Gap::Base, Gap::None))
                .gap(Gap::Snug)
                .child(Text::new("aa").font(FontRole::Small).ink(Ink::Dim).flex(1))
                .child(Spacer::new().height(20))
                .child(Text::new("aa").font(FontRole::Small).ink(Ink::Dim)),
        );
        assert!(
            rects(&flexed)[1].height() > line_h(),
            "the negative control did not fire: a flexed caption should swallow the band"
        );
    }

    #[test]
    fn an_error_takes_the_hints_place_rather_than_pushing_the_row_taller() {
        // The 240-pixel screen's decision. A form validates on submit and several fields fail at once,
        // so a stacked error line grows three fields by a line each at the moment the user needs to
        // look at one of them — and the field they were standing in slides off the bottom.
        let with_hint = FieldRow::new("aa")
            .control(Spacer::new().height(20))
            .hint("aa aa aa")
            .build();
        let with_both = FieldRow::new("aa")
            .control(Spacer::new().height(20))
            .hint("aa aa aa")
            .error("aa aa")
            .build();
        assert_eq!(with_hint.slot_count(), with_both.slot_count(), "one help line, not two");
        let (a, b) = (rects(&with_hint), rects(&with_both));
        assert_eq!(a.len(), b.len());
        // Same boxes in the same places: only what is written in the help line and its colour moved.
        assert_eq!(a, b, "an error must not move a single rect of the field");
        // And it really is the error that is showing, not the hint it replaced.
        assert_ne!(painted(&with_hint), painted(&with_both), "the help line says something else now");
    }

    #[test]
    fn the_order_of_hint_and_error_does_not_matter() {
        // The bug `Part` exists for, one field over: two pieces of state resolved at call time make a
        // builder whose result depends on the order it reads well in. Kept apart until `build`, both
        // spellings are the same field.
        let a = FieldRow::new("aa").hint("aa aa").error("aa").build();
        let b = FieldRow::new("aa").error("aa").hint("aa aa").build();
        assert_eq!(painted(&a), painted(&b));
    }

    #[test]
    fn the_order_of_focused_and_the_note_does_not_matter() {
        // `note` is inked from `self.focused`, so reading it inside the setter would have quietly built
        // a dim counter above the field being typed in.
        let a = FieldRow::new("aa").focused(true).note("aa").build();
        let b = FieldRow::new("aa").note("aa").focused(true).build();
        assert_eq!(painted(&a), painted(&b));
    }

    #[test]
    fn an_empty_error_is_no_error_and_leaves_the_hint_alone() {
        // The call site this protects is `.error(model.error.as_deref().unwrap_or_default())`. If empty
        // meant "an error saying nothing", that one line would suppress the hint of every valid field
        // on the form and leave a blank line where it was.
        let hint_only = FieldRow::new("aa").hint("aa aa").build();
        let with_empty = FieldRow::new("aa").hint("aa aa").error("").build();
        assert_eq!(painted(&hint_only), painted(&with_empty));
        // Two slots: the column and its caption. Not three — an empty hint is no help line at all,
        // where a blank one would leave a line of air under every valid field on the form.
        assert_eq!(FieldRow::new("aa").hint("").build().slot_count(), 2, "no line of air either");
    }

    #[test]
    fn focus_changes_the_ink_and_draws_no_frame() {
        // The control paints its own band; a frame here would be a box around a box, which is
        // `ListItem`'s doubled selection band arriving through a different door.
        let asleep = FieldRow::new("aa").control(Spacer::new().height(20)).hint("aa").build();
        let live = FieldRow::new("aa")
            .control(Spacer::new().height(20))
            .hint("aa")
            .focused(true)
            .build();
        assert_eq!(rects(&asleep), rects(&live), "focus moved a rect");
        assert_eq!(asleep.slot_count(), live.slot_count());

        let (a, b) = (painted(&asleep), painted(&live));
        assert_ne!(a, b, "the ink changed");
        // Exactly as many pixels are off the background in both. A frame or a fill would add some; a
        // recolour cannot.
        let bg = Palette::DARK.bg.mid().to_rgb565().0;
        let inked = |px: &[u16]| px.iter().filter(|&&p| p != bg).count();
        assert_eq!(inked(&a), inked(&b), "something was painted that was not there before");
        assert!(inked(&a) > 0, "nothing was painted at all, so this test proves nothing");
    }

    #[test]
    fn a_note_is_pushed_out_to_the_fields_right_edge() {
        // What the `flex(1)` inside the caption's own `Row` is for. Without it the counter sits
        // touching the caption, which reads as part of it.
        let root = FieldRow::new("aa").note("aa").control(Spacer::new().height(20)).build();
        let got = rects(&root);
        assert_eq!(got.len(), 5, "column, caption line, caption, note, control: {got:?}");
        let (caption, note) = (got[2], got[3]);
        assert_eq!(note.x1, 320 - base(), "the note ends at the field's margin: {note:?}");
        assert!(caption.x1 <= note.x0, "and the caption stops before it");
        assert!(note.x0 - caption.x0 > 100, "the note is out at the edge, not beside the caption");
    }

    #[test]
    fn a_field_costs_the_slots_it_needs_and_no_more() {
        // A form is six fields deep inside a scroll; a group wrapped around a single child is a slot
        // per field spent on nothing, and it is also where a stretch stops.
        assert_eq!(FieldRow::new("aa").build().slot_count(), 2, "column, caption");
        assert_eq!(
            FieldRow::new("aa").control(Spacer::new()).build().slot_count(),
            3,
            "column, caption, control"
        );
        assert_eq!(
            FieldRow::new("aa").control(Spacer::new()).hint("aa").build().slot_count(),
            4,
            "column, caption, control, help"
        );
        assert_eq!(
            FieldRow::new("aa").note("aa").build().slot_count(),
            4,
            "column, caption line, caption, note — the line becomes a box once it has two things in it"
        );
    }

    #[test]
    fn the_digest_moves_with_the_words_and_ignores_the_state() {
        let a = FieldRow::new("aa").hint("aa").build();
        assert_ne!(a.content_hash(), FieldRow::new("ab").hint("aa").build().content_hash(), "label");
        assert_ne!(a.content_hash(), FieldRow::new("aa").hint("ab").build().content_hash(), "hint");
        assert_ne!(a.content_hash(), FieldRow::new("aa").build().content_hash(), "the help line");
        assert_ne!(a.content_hash(), 0, "a field that always re-measures is a form re-measured");

        // Focus is deliberately absent: it changes the ink and nothing else, so a focused field and a
        // sleeping one are the same box and sharing a measurement is correct rather than a hazard.
        // Folding it in would re-measure every field on the form on every press of Down.
        assert_eq!(
            a.content_hash(),
            FieldRow::new("aa").hint("aa").focused(true).build().content_hash(),
            "focus moves no pixel of the box"
        );
    }

    #[test]
    fn an_error_and_a_hint_of_the_same_words_are_the_same_box() {
        // The digest answers "could the *size* have changed". An error and a hint of the same words are
        // the same line of the same font in the same place, differing only in colour — so they must
        // share a measurement, exactly as `Text` leaves its own ink out of its hash.
        let hint = FieldRow::new("aa").hint("aa aa").build();
        let error = FieldRow::new("aa").error("aa aa").build();
        assert_eq!(hint.content_hash(), error.content_hash());
        // Which is not to say they look alike.
        assert_ne!(painted(&hint), painted(&error), "the error is not in the hint's colour");
    }

    #[test]
    fn help_lines_is_the_way_out_and_one_is_the_default() {
        // A wrapping help line makes a field as tall as its message, which is what the module docs
        // argue against — so it is asked for rather than given.
        let long = "aa aa aa aa aa aa aa aa aa aa aa aa aa aa aa aa aa aa aa aa aa aa aa aa aa aa";
        let one = FieldRow::new("aa").hint(long).build();
        let two = FieldRow::new("aa").hint(long).help_lines(2).build();
        assert_eq!(rects(&one)[2].height(), line_h(), "one line by default however long the text");
        assert_eq!(rects(&two)[2].height(), 2 * line_h(), "and two when asked for");
        assert_eq!(FieldRow::new("aa").hint("aa").help_lines(0).build().slot_count(), 3);
    }

    #[test]
    fn the_caption_follows_the_control_and_cannot_disagree_with_it() {
        // The bug this exists to make unrepresentable: a row told it was focused, holding a control
        // that was not, painting a lit caption over a field no key could reach. Both call sites in
        // this SDK's own gallery had it, one of them for months, and the host had nothing to say
        // because every test inspected the tree instead of pressing a key.
        let mut slots = SlotTable::new();
        let lit = |row: FieldRow| row.inks().0 == Ink::Accent;

        // The row says yes, the control says no. The control wins, so the caption stays dark —
        // which is the truth, and is what a reviewer sees on the first contact sheet.
        assert!(!lit(
            FieldRow::new("aa").focused(true).control(TextField::new(&mut slots).focused(false))
        ));
        // And the other way round: a control with the keyboard lights the caption even if nobody
        // remembered to tell the row.
        assert!(lit(
            FieldRow::new("aa").control(TextField::new(&mut slots).focused(true))
        ));
    }

    #[test]
    fn a_control_that_does_not_take_focus_leaves_the_row_in_charge() {
        // `Spacer` answers `None`, so the row's own flag still decides. Without this the fallback
        // would be untested and a form of plain rows would lose its caption cue entirely.
        let mut slots = SlotTable::new();
        let _ = &mut slots;
        assert_eq!(FieldRow::new("aa").focused(true).control(Spacer::new()).inks().0, Ink::Accent);
        assert_eq!(FieldRow::new("aa").control(Spacer::new()).inks().0, Ink::Dim);
    }

    #[test]
    fn the_control_in_a_field_row_actually_receives_a_character() {
        // The test this file did not have, and the one a person on a handset had to be instead.
        // Sixteen tests here inspected the tree — its shape, its measures, its inks, its digest — and
        // a tree is exactly what cannot tell a live field from a dead one. The row lit its caption in
        // the accent, the field drew identically to every other field, and no key arrived.
        //
        // Placing before dispatching is load-bearing: `dispatch_key` matches a key against the rect a
        // widget was *drawn* at, so a dispatch into an empty cache reaches nobody.
        let mut slots = SlotTable::new();
        let root = FieldRow::new("Phone number")
            .control(TextField::new(&mut slots).focused(true))
            .build();
        let band = Rect { x0: 0, y0: 0, x1: 240, y1: 80 };
        let handled = testing::with_theme(Palette::DARK, |theme| {
            let mut cache = UiCache::with_capacity(root.slot_count() + 4);
            layout::place_frame(&root, band, &mut cache, theme);
            let mut clip = symbian_ui::NoClipboard;
            let mut cx = crate::widget::KeyCtx::new(theme, &mut clip);
            layout::dispatch_key(
                &root,
                symbian_ui::KeyEvent::new(symbian_ui::Key::Char('a')),
                &cache,
                &mut cx,
            )
        });
        assert_eq!(handled, symbian_ui::Handled::Consumed, "a focused field takes a character");
    }

    #[test]
    fn a_field_row_whose_control_is_asleep_takes_nothing() {
        // The negative control. Without it the test above would keep passing if `dispatch_key` ever
        // started consuming everything, and "the key was taken" would stop meaning "by the field".
        let mut slots = SlotTable::new();
        let root = FieldRow::new("Phone number")
            .control(TextField::new(&mut slots).focused(false))
            .build();
        let band = Rect { x0: 0, y0: 0, x1: 240, y1: 80 };
        let handled = testing::with_theme(Palette::DARK, |theme| {
            let mut cache = UiCache::with_capacity(root.slot_count() + 4);
            layout::place_frame(&root, band, &mut cache, theme);
            let mut clip = symbian_ui::NoClipboard;
            let mut cx = crate::widget::KeyCtx::new(theme, &mut clip);
            layout::dispatch_key(
                &root,
                symbian_ui::KeyEvent::new(symbian_ui::Key::Char('a')),
                &cache,
                &mut cx,
            )
        });
        assert_eq!(handled, symbian_ui::Handled::Ignored);
    }
}
