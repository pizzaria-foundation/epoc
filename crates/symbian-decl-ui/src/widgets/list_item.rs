//! The row. One builder for every shape a list row takes on this device.
//!
//! [`ScrollList`](super::ScrollList) already answers the hard half — which rows are on screen, where
//! the scrollbar thumb goes, how the band is clipped. What it asks for is a [`Node`] per row, and
//! until now every screen assembled that by hand: a `Row`, a `CrossAlign::Stretch`, a padding, a
//! `Text` with `flex(1)`, a second `Text` aligned to the end. Written out per screen it is six lines
//! that are nearly the same, and the "nearly" is where the pixels drift.
//!
//! ```ignore
//! ListItem::new(&chat.name)
//!     .secondary(&chat.preview)
//!     .selected(i == model.selected)
//!     .leading(Avatar::new(&chat.initials, chat.id))
//!     .trailing(Text::new(&chat.time).font(FontRole::Small))
//!     .build()
//! ```
//!
//! # Two lines, not three columns
//!
//! The obvious reading of a chat row is `avatar | text column | time-and-badge column`. It is wrong,
//! and only the pixels show it: nothing in the hand-written row constrains the preview to a column,
//! so it is allowed to run *under* the timestamp. Modelled as two columns the preview stops short.
//!
//! So a row with a second line is **two stacked lines**, each of which may have its own trailing
//! thing: title with the time, secondary with the badge. That is both faithful to what shipped and,
//! arguably, what the design always was. It is why [`trailing`](ListItem::trailing) and
//! [`trailing_secondary`](ListItem::trailing_secondary) are two methods rather than one column.
//!
//! # `CrossAlign::Stretch` is not decoration
//!
//! A list row is 38 pixels tall and its text is 17. Left at the default the text is anchored to the
//! top of the row and every row on screen draws ten pixels high — which is the single difference a
//! pixel-for-pixel comparison against the hand-written toolkit ever found, and the reason
//! [`CrossAlign`] exists at all. It is applied here so no caller has to remember it.
//!
//! # Who draws the highlight
//!
//! [`ScrollList`] paints the selection band before it draws the row, full-bleed and square, because
//! with no pointer that band is the only thing saying where the user is. A row that painted its own
//! would double it. So [`selected`](ListItem::selected) changes only the **ink**, and the band is the
//! list's.
//!
//! That left a hole, and the gallery is where it turned up: a row inside a
//! [`FocusScope`](super::FocusScope) has no list above it, so **nobody** painted the band and the
//! focused row was distinguishable only by the weight of its font. On a form of four rows that is not
//! a cursor.
//!
//! [`band`](ListItem::band) is the opt-in. Off by default, so a list keeps owning the highlight and
//! cannot end up with two; on for a row in a form, where there is no list to own it.

use alloc::string::String;

use crate::layout::CrossAlign;
use crate::spacing::{Gap, Pad};
use crate::theme::FontRole;
use crate::widget::Widget;
use crate::widgets::{Column, Icon, Ink, Node, Row, Text};

/// One of the row's optional pieces, held unresolved until [`ListItem::build`].
///
/// # Why a glyph and not an `Icon`
///
/// An icon's colour and a value's colour depend on whether the row is selected, and a builder is
/// written in whatever order reads well at the call site — `.leading_icon(..).selected(true)` as
/// often as the reverse. Resolving the ink inside `leading_icon` therefore worked or did not
/// depending on the order the two methods were called in, and the wrong one is a `text`-coloured
/// icon on top of the selection band: legible, plausible, and wrong on exactly the row being looked
/// at.
///
/// So the parts are stored as *what they are* and coloured once, in `build`, when the state is
/// finally known. `the_order_of_selected_and_the_parts_does_not_matter` is the assertion, and it
/// failed on the first run.
enum Part {
    /// Already built by the caller, who is responsible for its own colour.
    Given(Node),
    /// A glyph this row will ink itself.
    Glyph(symbian_ui::icon::Icon),
    /// A setting's current value, set small and dim at the end of the line.
    Value(String),
    /// The chevron that says this row leads somewhere.
    Arrow,
}

impl Part {
    /// Turn into a node, coloured for a row in this state.
    fn build(self, quiet: Ink) -> Node {
        match self {
            Part::Given(n) => n,
            Part::Glyph(g) => Node::leaf(Icon::new(g).ink(quiet)),
            Part::Value(v) => Node::leaf(
                Text::new(v).font(FontRole::Small).ink(quiet).align(symbian_gfx::Align::End),
            ),
            Part::Arrow => Node::leaf(Icon::arrow().ink(quiet)),
        }
    }
}

/// A row of a list, built from the pieces a row actually has.
///
/// Everything but the title is optional, and the shape follows from what was asked for: no
/// `secondary` is a single line, and a `leading` is a column of its own to the left of it. One
/// builder rather than the eight named types the reference libraries offer, because on this screen
/// they are the same row with different parts filled in — and eight types is eight places to forget
/// the `Stretch`.
pub struct ListItem {
    title: String,
    title_font: FontRole,
    secondary: Option<String>,
    selected: bool,
    leading: Option<Part>,
    trailing: Option<Part>,
    trailing_secondary: Option<Part>,
    /// A hairline along the bottom, as a row means it: a property, not a child. See
    /// [`Group::border_bottom`](super::Group::border_bottom).
    divider: bool,
    /// Whether this row paints its own selection band. See [`ListItem::band`].
    band: bool,
    pad: Pad,
    gap: Gap,
}

impl ListItem {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            // Strong, because the title of a row is what the eye lands on and every row in this SDK
            // that shipped sets it that way. A row that wants body weight says so.
            title_font: FontRole::Strong,
            secondary: None,
            selected: false,
            leading: None,
            trailing: None,
            trailing_secondary: None,
            divider: false,
            band: false,
            // `Base` across, nothing down: the row's height comes from the list, and padding on the
            // main axis of a fixed-height row eats the text rather than moving it.
            pad: Pad::xy(Gap::Base, Gap::None),
            gap: Gap::Base,
        }
    }

    /// The second line — a preview, a subtitle, an address.
    pub fn secondary(mut self, text: impl Into<String>) -> Self {
        self.secondary = Some(text.into());
        self
    }

    /// Whether the selection band is under this row. Changes the ink and nothing else.
    pub fn selected(mut self, on: bool) -> Self {
        self.selected = on;
        self
    }

    /// Set the title in body weight rather than strong — a row that is information rather than a
    /// name.
    pub fn plain(mut self) -> Self {
        self.title_font = FontRole::Body;
        self
    }

    /// Something to the left of the text: an avatar, an icon, a checkbox.
    pub fn leading(mut self, w: impl Widget + 'static) -> Self {
        self.leading = Some(Part::Given(Node::leaf(w)));
        self
    }

    /// A leading position built as a group rather than a leaf.
    pub fn leading_node(mut self, n: Node) -> Self {
        self.leading = Some(Part::Given(n));
        self
    }

    /// An icon to the left, coloured to match the row's state.
    ///
    /// Separate from [`leading`](Self::leading) because an icon's ink depends on the selection and
    /// the caller does not have the row's state to hand when it builds one — passing
    /// `Icon::new(g)` through `leading` would leave it the palette's `text` colour on top of the
    /// selection band.
    pub fn leading_icon(mut self, glyph: symbian_ui::icon::Icon) -> Self {
        self.leading = Some(Part::Glyph(glyph));
        self
    }

    /// Something at the end of the title's line: a timestamp, a value, a chip.
    pub fn trailing(mut self, w: impl Widget + 'static) -> Self {
        self.trailing = Some(Part::Given(Node::leaf(w)));
        self
    }

    pub fn trailing_node(mut self, n: Node) -> Self {
        self.trailing = Some(Part::Given(n));
        self
    }

    /// The current value of a setting, at the end of the title's line.
    ///
    /// The S60 setting item: label on the left, what it is set to on the right. Dim, because the
    /// label is the thing being navigated and the value is what it says.
    pub fn trailing_value(mut self, value: impl Into<String>) -> Self {
        self.trailing = Some(Part::Value(value.into()));
        self
    }

    /// The chevron that says this row leads somewhere.
    pub fn trailing_arrow(mut self) -> Self {
        self.trailing = Some(Part::Arrow);
        self
    }

    /// Something at the end of the *second* line: an unread badge, a size, a status.
    ///
    /// Its own method rather than a second column, because the second line is a line. See the module
    /// docs.
    pub fn trailing_secondary(mut self, w: impl Widget + 'static) -> Self {
        self.trailing_secondary = Some(Part::Given(Node::leaf(w)));
        self
    }

    pub fn trailing_secondary_node(mut self, n: Node) -> Self {
        self.trailing_secondary = Some(Part::Given(n));
        self
    }

    /// A hairline under the row, skipped when the row is selected.
    ///
    /// Skipped because the selection band already separates it from its neighbours, and a rule drawn
    /// across the band reads as a crack in it. That conditional is the hand-written row's, kept.
    pub fn divider(mut self, on: bool) -> Self {
        self.divider = on;
        self
    }

    /// Paint the selection band behind this row when it is selected.
    ///
    /// For a row in a form, where there is no [`ScrollList`](super::ScrollList) above it to paint one.
    /// Leave it off inside a list, which paints the band itself — two of them is one band drawn twice
    /// and the second one is drawn *over* the first row's own background.
    ///
    /// It is `chrome::selection`, the same full-bleed square band the toolkit uses everywhere, rather
    /// than a rounded inset pill: with no pointer this band is the cursor, and a rounded one reads as
    /// a button you press.
    pub fn band(mut self, on: bool) -> Self {
        self.band = on;
        self
    }

    /// Override the side padding. `Gap::Base` by default, which is the row margin the toolkit uses.
    pub fn pad(mut self, pad: Pad) -> Self {
        self.pad = pad;
        self
    }

    /// Space between the leading thing, the text and the trailing thing.
    pub fn gap(mut self, gap: impl Into<Gap>) -> Self {
        self.gap = gap.into();
        self
    }

    /// The inks this row's state resolves to: one for the title, one for everything quieter.
    fn inks(&self) -> (Ink, Ink) {
        if self.selected {
            // Both the same on a selection band: `dim` is a contrast decision made against the
            // *background*, and on top of the highlight it is the one colour that stops being
            // readable. The hand-written row makes the same choice.
            (Ink::Selection, Ink::Selection)
        } else {
            (Ink::Text, Ink::Dim)
        }
    }

    /// One line: some text taking the space, and optionally something at its end.
    ///
    /// `None` returns the text **unwrapped**, and that is not only about the slot it saves. A row of
    /// one line with nothing at its end has to be pixel-identical to the hand-written row, which puts
    /// its `Text` straight into the stretched row — and a `Row` in between is a box the outer stretch
    /// stops at, leaving the text anchored to the top of its own 17-pixel line inside a 38-pixel band.
    /// Every row on the screen drawn ten pixels high, which is exactly the defect `CrossAlign` was
    /// added to fix, arriving through a new door.
    fn line(text: Text, trailing: Option<Node>, in_column: bool) -> Node {
        match trailing {
            // Stretched too: the inner row is a box, and a box that does not pass the band down is
            // where the alignment stops.
            // `fill` only when this row's parent is a **row**, and that condition is the same trap
            // the comment below describes, met from the other side.
            //
            // The flex is needed: without it this box is a fixed child, so it is measured against
            // the whole line and reports the whole line back — while placement gives it the line
            // *less the leading widget and its gap*, and clamps the shortfall out of its last child.
            // The trailing widget paid, always, and exactly one `Gap::Base` of it: a `crash loop`
            // chip asking 58 pixels was placed in 52 and drew as `rash loop`. Nobody saw it because
            // a chip that loses its first letter still looks like a chip.
            //
            // But in the two-line case the parent is a `Column`, and there the very same `fill(1)`
            // claims leftover **height** — the title's line went from 17 pixels to 24 the moment it
            // was applied unconditionally, which `list_item_parity` caught on the first run.
            Some(t) => Node::Group(
                Row::new()
                    .align(CrossAlign::Stretch)
                    .fill(if in_column { 0 } else { 1 })
                    .child(text.flex(1))
                    .node(t),
            ),
            // `in_column` decides whether the text may be handed over bare, and getting it wrong is
            // not cosmetic — it was a row 189 pixels tall.
            //
            // A leaf's weight is read by whichever axis its **parent** runs. In the single-line case
            // the parent is this row, so `flex(1)` claims leftover *width*, which is what a title
            // taking the line means. In the two-line case the parent is a `Column`, so the very same
            // `flex(1)` claims leftover *height* — and inside a `FocusScope` on a 205-pixel band that
            // is 177 pixels of empty secondary line, with the two rows below it squeezed to nothing
            // and off the screen.
            //
            // It never showed inside a `ScrollList` because a row there is 38 pixels by construction,
            // so "all the leftover height" is a few pixels and looks like padding. The gallery put the
            // same row in a column and it was immediately, obviously wrong.
            //
            // So in a column the line is wrapped in a `Row`: the text still flexes for width inside
            // it, and the row itself takes no share of the column's height. Bare is kept for the
            // single-line case because an intermediate box is where the outer `Stretch` stops — which
            // is the *other* defect this function has already had.
            None if in_column => Node::Group(Row::new().align(CrossAlign::Stretch).child(text.flex(1))),
            None => Node::leaf(text.flex(1)),
        }
    }

    pub fn build(self) -> Node {
        let (fg, dim) = self.inks();
        let title = Text::new(&self.title).font(self.title_font).ink(fg);

        let trailing = self.trailing.map(|p| p.build(dim));
        let trailing_secondary = self.trailing_secondary.map(|p| p.build(dim));
        let text = match &self.secondary {
            None => Self::line(title, trailing, false),
            Some(second) => Node::Group(
                Column::new()
                    // The two lines pushed to the two edges of the row, which is what the
                    // hand-written row expresses as `r.y0 + 3` and `r.y1 - 4` — four rects anchored
                    // to two different edges, and four places to get an inset wrong.
                    .justify(crate::layout::MainAlign::SpaceBetween)
                    .node(Self::line(title, trailing, true))
                    .node(Self::line(
                        Text::new(second).font(FontRole::Small).ink(dim),
                        trailing_secondary,
                        true,
                    ))
                    .fill(1),
            ),
        };

        let mut row = Row::new()
            // The one setting that turns a declared row into an S60 row. See the module docs.
            .align(CrossAlign::Stretch)
            .padding(self.pad)
            .gap(self.gap);
        if self.band && self.selected {
            row = row.selection_band(true);
        }
        if let Some(lead) = self.leading {
            row = row.node(lead.build(dim));
        }
        row = row.node(text);
        if self.divider && !self.selected {
            // Inset to the left of the text, not to the edge of the screen, so the rule starts where
            // the content does — which is what makes a list of rows read as a list rather than as a
            // stack of boxes.
            row = row.border_bottom(Ink::Divider, self.pad.left);
        }
        Node::Group(row)
    }
}

impl From<ListItem> for Node {
    fn from(i: ListItem) -> Node {
        i.build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::UiCache;
    use crate::layout;
    use crate::outbox::Outbox;
    use crate::widgets::{Avatar, Checkbox, Switch};
    use symbian_gfx::{Align, Rect, Size};
    use symbian_ui::{testing, Palette};

    const ROW: Rect = Rect { x0: 0, y0: 0, x1: 200, y1: 38 };

    /// Place `root` in a row-sized band and report every descendant's rect, in slot order.
    fn rects(root: &Node) -> Vec<Rect> {
        testing::with_theme(Palette::DARK, |theme| {
            let mut cache = UiCache::with_capacity(root.slot_count());
            layout::place_frame(root, ROW, &mut cache, theme);
            (0..root.slot_count()).map(|s| cache.rect(s).unwrap_or(Rect::from_xywh(0, 0, 0, 0))).collect()
        })
    }

    /// Draw `root` into a row-sized canvas.
    fn painted(root: &Node) -> Vec<u16> {
        let (_, buf) = testing::with_canvas(Size::new(200, 38), |c| {
            testing::with_theme(Palette::DARK, |theme| {
                c.clear(Palette::DARK.bg.mid());
                let mut cache = UiCache::with_capacity(root.slot_count());
                layout::draw_frame(root, ROW, &mut cache, c, theme);
            });
        });
        buf
    }

    #[test]
    fn a_row_places_its_parts_where_the_hand_written_row_does() {
        // # What this proves, and what it cannot
        //
        // `testing::with_theme` loads a test atlas containing **one glyph**: lowercase `a`, four
        // pixels by six. Every font role in it is the same face at the same size. So a pixel
        // comparison here sees fills, rules, icons, and the *position of the letter `a`* — which is
        // enough to catch a row whose parts are in the wrong place, and not enough to catch a wrong
        // font role or a wrong baseline.
        //
        // The name of this test used to say `pixel_for_pixel`, and that was a promise it could not
        // keep. The typography half lives in `tests/list_item_parity.rs`, on the real `.sbf` atlases,
        // with a negative control that swaps a font role and requires the comparison to notice.
        //
        // The reference is **`examples/compare.rs`'s `declared_row`, transcribed** — not a shape
        // invented here. That distinction cost a run: the first version built its reference out of the
        // same nested rows the builder produces, so it reproduced the builder's own bug and passed
        // while the text sat ten pixels high. A scene written to be compared agrees a little too
        // readily, and `the_comparison_would_notice_if_it_were_lied_to` is what caught it.
        let by_hand = Node::Group(
            Row::new()
                .align(CrossAlign::Stretch)
                .padding(Pad::xy(Gap::Base, Gap::None))
                .gap(Gap::Base)
                .child(Text::new("Ana Ribeiro").font(FontRole::Strong).ink(Ink::Text).flex(1))
                .child(Text::new("14:32").font(FontRole::Small).ink(Ink::Dim).align(Align::End)),
        );
        let declared = ListItem::new("Ana Ribeiro")
            .trailing(Text::new("14:32").font(FontRole::Small).ink(Ink::Dim).align(Align::End))
            .build();
        assert_eq!(painted(&declared), painted(&by_hand));
    }

    #[test]
    fn the_comparison_would_notice_if_it_were_lied_to() {
        // A parity test that cannot fail reads as a proof and is a constant. Drop the one setting the
        // module docs call load-bearing and the pixels must part company — which works on this atlas
        // because the stretch moves the one glyph it has *vertically*, and a moved `a` is visible even
        // when a re-weighted one is not.
        let without_stretch = Node::Group(
            Row::new()
                .padding(Pad::xy(Gap::Base, Gap::None))
                .gap(Gap::Base)
                .child(Text::new("Ana Ribeiro").font(FontRole::Strong).ink(Ink::Text).flex(1)),
        );
        let declared = ListItem::new("Ana Ribeiro").build();
        assert_ne!(painted(&declared), painted(&without_stretch), "the stretch is doing something");
    }

    #[test]
    fn the_title_is_vertically_centred_and_not_ten_pixels_high() {
        // The concrete symptom of a missing `Stretch`: in a 38-pixel row a 17-pixel line of text
        // anchored to the top is every row on the screen drawn wrong, and it looks plausible.
        let root = ListItem::new("Ana").build();
        let got = rects(&root);
        let title = *got.last().expect("a title was placed");
        assert_eq!(title.height(), 38, "the text is given the whole band to centre itself in");
    }

    #[test]
    fn a_second_line_is_a_line_and_not_a_column() {
        // The finding the `tg` port cost six differences to reach: modelled as columns, the preview
        // stops short of the timestamp. Modelled as lines, it is free to run under it — so the
        // secondary line is as wide as the row, not as wide as what is left beside the trailing.
        let root = ListItem::new("Ana")
            .secondary("a preview long enough to reach")
            .trailing(Text::new("14:32").font(FontRole::Small))
            .build();
        let got = rects(&root);
        let title_line = got.iter().find(|r| r.height() < 38 && r.width() > 100).copied();
        assert!(title_line.is_some(), "the two lines each got a band");
        // Both lines start at the same x: nothing indents the second under the first.
        let lines: Vec<Rect> = got.iter().filter(|r| r.width() > 150 && r.height() < 38).copied().collect();
        assert!(lines.len() >= 2, "two lines, not one: {lines:?}");
        assert!(lines.windows(2).all(|w| w[0].x0 == w[1].x0));
    }

    #[test]
    fn the_two_lines_are_pushed_to_the_two_edges_of_the_row() {
        // `SpaceBetween`, which is what replaces the hand-written row's `r.y0 + 3` and `r.y1 - 4`.
        // Asserted on slots rather than by filtering rects by size: the tree of a two-line row with
        // nothing at either end is exactly `row, column, line, title, line, secondary`, and a filter
        // that guessed at which rect was which would keep passing after the structure changed under it.
        //
        // Six slots and not four: each line is wrapped in a `Row` so its text cannot claim the
        // column's *height* — see `line`'s note on the 189-pixel row.
        let root = ListItem::new("Ana").secondary("preview").build();
        let got = rects(&root);
        assert_eq!(got.len(), 6, "row, column, line, title, line, secondary: {got:?}");
        assert_eq!(got[2].y0, 0, "the title's line against the top of the row");
        assert_eq!(got[3].height(), got[2].height(), "and the title fills its line, not the band");
        assert_eq!(got[4].y1, 38, "the secondary's line against the bottom");
    }

    #[test]
    fn a_selected_row_changes_its_ink_and_draws_no_band_of_its_own() {
        // `ScrollList` paints the highlight. A row that painted its own would double it — so the only
        // difference between the two states must be the colour of the text.
        let plain = painted(&ListItem::new("Ana").build());
        let sel = painted(&ListItem::new("Ana").selected(true).build());
        assert_ne!(plain, sel, "the ink changed");
        // Nothing was filled: the background is untouched everywhere the text is not.
        let bg = Palette::DARK.bg.mid().to_rgb565().0;
        let plain_bg = plain.iter().filter(|&&p| p == bg).count();
        let sel_bg = sel.iter().filter(|&&p| p == bg).count();
        let diff = plain_bg.abs_diff(sel_bg);
        assert!(diff * 20 < plain_bg, "a band would have covered the row: {plain_bg} vs {sel_bg}");
    }

    #[test]
    fn a_leading_thing_sits_left_of_the_text() {
        let root = ListItem::new("Ana").leading(Avatar::new("AN", 3).size(28)).build();
        let got = rects(&root);
        let avatar = got.iter().find(|r| r.width() == 28).copied().expect("the avatar was placed");
        let text_start = got.iter().filter(|r| r.x0 > avatar.x1 - 1).count();
        assert!(text_start > 0, "something is to the right of the avatar");
        assert_eq!(avatar.x0, 6, "and the avatar starts at the row's own margin");
    }

    #[test]
    fn a_selected_row_recolours_its_icon_and_its_value_too() {
        // The reason `leading_icon` and `trailing_value` exist rather than being passed through
        // `leading`/`trailing`: the caller has no way to know the row's state when it builds an icon,
        // so an icon handed in stays the palette's `text` colour on top of the highlight.
        let plain = painted(&ListItem::new("Wi-Fi").leading_icon(symbian_ui::icon::Icon::Lock).trailing_value("On").build());
        let sel = painted(
            &ListItem::new("Wi-Fi")
                .selected(true)
                .leading_icon(symbian_ui::icon::Icon::Lock)
                .trailing_value("On")
                .build(),
        );
        assert_ne!(plain, sel);
    }

    #[test]
    fn the_order_of_selected_and_the_parts_does_not_matter() {
        // `leading_icon` reads `self.selected`, so calling it before `selected` would have silently
        // built the wrong ink. Asserted because it is the kind of builder bug nothing else catches.
        let a = ListItem::new("Wi-Fi").selected(true).leading_icon(symbian_ui::icon::Icon::Lock).build();
        let b = ListItem::new("Wi-Fi").leading_icon(symbian_ui::icon::Icon::Lock).selected(true).build();
        assert_eq!(painted(&a), painted(&b));
    }

    #[test]
    fn a_row_paints_its_own_band_only_when_it_is_asked_to() {
        // The hole the gallery found: a row inside a `FocusScope` has no list above it, so nobody
        // painted the band and the focused row was distinguishable only by the weight of its font.
        // Off by default, because inside a list the band is the list's and two of them is one drawn
        // twice — the second over the first row's own background.
        let plain = painted(&ListItem::new("Ana").selected(true).build());
        let banded = painted(&ListItem::new("Ana").selected(true).band(true).build());
        assert_ne!(plain, banded, "the band was painted");

        let bg = Palette::DARK.bg.mid().to_rgb565().0;
        let plain_bg = plain.iter().filter(|&&p| p == bg).count();
        let banded_bg = banded.iter().filter(|&&p| p == bg).count();
        assert!(banded_bg * 4 < plain_bg, "a band covers the row: {plain_bg} vs {banded_bg}");
    }

    #[test]
    fn an_unselected_row_never_paints_a_band_however_it_was_asked() {
        // `band(true)` says "paint one when selected", not "paint one". A row that banded itself
        // unconditionally would put the cursor on every row at once.
        assert_eq!(
            painted(&ListItem::new("Ana").band(true).build()),
            painted(&ListItem::new("Ana").build())
        );
    }

    #[test]
    fn a_divider_is_drawn_except_under_the_selection() {
        // The hand-written row's conditional, kept: a rule across the highlight reads as a crack in
        // it.
        let with = painted(&ListItem::new("Ana").divider(true).build());
        let without = painted(&ListItem::new("Ana").build());
        assert_ne!(with, without, "the rule was drawn");
        let selected_with = painted(&ListItem::new("Ana").selected(true).divider(true).build());
        let selected_without = painted(&ListItem::new("Ana").selected(true).build());
        assert_eq!(selected_with, selected_without, "and skipped on the selected row");
    }

    #[test]
    fn a_row_costs_the_slots_it_needs_and_no_more() {
        // Slots are per row and a list has two hundred of them. A group wrapped around a single
        // flexible child would be a slot per row spent on nothing.
        // Two, not three: a line with nothing at its end is the text itself rather than a row around
        // it. That saving is why a single-line row reaches parity with the hand-written one — the extra
        // box was also where the stretch used to stop.
        assert_eq!(ListItem::new("Ana").build().slot_count(), 2, "row, title");
        assert_eq!(
            ListItem::new("Ana").trailing(Text::new("14:32")).build().slot_count(),
            4,
            "row, line, title, timestamp — the line is a box once it has two things in it"
        );
        // And a two-line row is six: the column, plus a line-box and a text for each line. The extra
        // box per line is what stops the text claiming the column's height.
        assert_eq!(ListItem::new("Ana").secondary("p").build().slot_count(), 6);
    }

    #[test]
    fn the_digest_moves_with_the_text_and_the_state() {
        let a = ListItem::new("Ana").build();
        let b = ListItem::new("Bea").build();
        let c = ListItem::new("Ana").selected(true).build();
        assert_ne!(a.content_hash(), b.content_hash());
        // Selection is deliberately *absent* from the digest: it changes the ink and nothing else, and
        // `Text` leaves colour out of its own hash for the same reason. A selected row and an
        // unselected one are the same size, so sharing a cache entry is correct rather than a hazard —
        // and folding it in would re-measure every visible row on every press of Down.
        assert_eq!(a.content_hash(), c.content_hash(), "selection moves no pixel of the box");
        assert_ne!(a.content_hash(), 0, "a row that always re-measures is two hundred re-measures");
    }

    /// Place `root` in a row band, then push one key at it the way the engine does.
    ///
    /// Placing first is not ceremony: `dispatch_key` matches a key against the rect a widget was
    /// **drawn** at, so a dispatch into an empty cache reaches nobody — and it would look like a dead
    /// keypad rather than like a missing layout pass.
    fn press(root: &Node, key: symbian_ui::Key) -> symbian_ui::Handled {
        testing::with_theme(Palette::DARK, |theme| {
            let mut cache = UiCache::with_capacity(root.slot_count() + 4);
            layout::place_frame(root, ROW, &mut cache, theme);
            let mut clip = symbian_ui::NoClipboard;
            let mut cx = crate::widget::KeyCtx::new(theme, &mut clip);
            layout::dispatch_key(root, symbian_ui::KeyEvent::new(key), &cache, &mut cx)
        })
    }

    #[test]
    fn a_control_in_a_selected_row_actually_receives_the_key() {
        // Fourteen tests in this file and not one of them pressed anything. That is how a container
        // ships a control nobody can reach: every assertion asked the tree what it looked like, and
        // looking is exactly what cannot tell a live control from a dead one. `FieldRow` shipped that
        // bug to a handset — see `field_row.rs` — and this file is the other half of the same class.
        #[derive(Clone, Debug, PartialEq, Eq)]
        enum Msg {
            Flip,
        }
        let out = Outbox::new();
        let row = ListItem::new("Notify me")
            .selected(true)
            .trailing(Switch::new(false).focused(true).out(out.clone(), Msg::Flip))
            .build();
        assert_eq!(press(&row, symbian_ui::Key::Select), symbian_ui::Handled::Consumed);
        assert_eq!(out.take(), alloc::vec![Msg::Flip]);
    }

    #[test]
    fn a_control_in_a_row_without_the_cursor_stays_quiet() {
        // The negative control, and the thing a container must not get wrong: two rows on a settings
        // screen both holding a switch, and a press flipping both.
        #[derive(Clone, Debug, PartialEq, Eq)]
        enum Msg {
            Flip,
        }
        let out = Outbox::new();
        let row = ListItem::new("Notify me")
            .selected(false)
            .trailing(Switch::new(false).focused(false).out(out.clone(), Msg::Flip))
            .build();
        assert_eq!(press(&row, symbian_ui::Key::Select), symbian_ui::Handled::Ignored);
        assert!(out.is_empty());
    }

    #[test]
    fn both_ends_of_a_row_can_hold_a_control_and_only_the_focused_one_answers() {
        // This is the fact that keeps `ListItem`'s `selected` *and* its controls' `focused` as two
        // separate flags, where `FieldRow`'s were collapsed into one. A field row has a single
        // control slot, so its two flags could only ever agree and a disagreement was the bug. This
        // has two, `leading` and `trailing`, and only one of them can hold the cursor — so here the
        // duplication carries a fact, and removing it would remove the ability to say which end is
        // live. Asserted rather than remembered, because the next reader will meet the two designs
        // side by side and wonder why they differ.
        #[derive(Clone, Debug, PartialEq, Eq)]
        enum Msg {
            Left,
            Right,
        }
        let out = Outbox::new();
        let row = ListItem::new("Every day")
            .selected(true)
            .leading(Checkbox::radio(false).focused(false).out(out.clone(), Msg::Left))
            .trailing(Switch::new(false).focused(true).out(out.clone(), Msg::Right))
            .build();
        assert_eq!(press(&row, symbian_ui::Key::Select), symbian_ui::Handled::Consumed);
        assert_eq!(out.take(), alloc::vec![Msg::Right], "only the focused end answers");
    }
}
