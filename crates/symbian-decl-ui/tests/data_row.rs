//! A label and a value, on one line or on two — and why that is not a widget.
//!
//! # Why there is no `DataRow`
//!
//! The plan for this phase had one, borrowed from jQuery Mobile's *table reflow*: a row of a table
//! that turns into two stacked lines once the viewport drops below 320 pixels. Writing the call site
//! is what killed it, exactly as it killed `SettingList` — see `tests/settings.rs`, which this file
//! is the sibling of.
//!
//! Four findings, in the order they arrived:
//!
//! 1. **Both states already exist on [`ListItem`], and they are one method call each.**
//!    `.trailing_value(v)` is the unreflowed row — label left, value right, one line. `.secondary(v)`
//!    is the reflowed one — label above, value below. A `DataRow` would be a third name for two
//!    things that are already named.
//! 2. **There is no breakpoint to switch on.** jQuery Mobile reflows at 320 pixels because a browser
//!    has a viewport that varies from a phone to a desktop. This device is an E72: `E72_SCREEN` is
//!    320x240 and there is no second width, in this SDK or on the handset. A condition with one
//!    possible answer is a constant, and a widget built around it would be a widget that always
//!    takes the same branch.
//! 3. **The content-dependent version cannot be a builder.** "Reflow when the label and the value do
//!    not both fit" is a real question and its answer needs a font, so it cannot be decided in
//!    `build()` — a `view` is constructed without a theme, deliberately, which is the reason
//!    [`Ink`](symbian_decl_ui::widgets::Ink) and `Gap` exist at all. Deciding it at draw time means
//!    a `Widget`, and a `Widget` here means re-implementing the two-lines-not-two-columns rule, the
//!    `CrossAlign::Stretch`, the unwrapped single line and the `Part` ordering trap behind a leaf:
//!    a second copy of `list_item.rs`, agreeing with the first on the day it was written.
//! 4. **A row inside a [`ScrollList`] cannot reflow anyway.** Rows there are `RowHeight::Row` by
//!    construction, so a row that decided for itself to become two lines would be measured at 38
//!    pixels and clipped — silently, which is the worst kind. The height of a reflowed row is the
//!    *list's* to know, which means the decision has to be made where the list is built. Which is
//!    the caller.
//!
//! This crate's rule is that an abstraction needs two callers that already disagree. There are none:
//! settings screens want one line and details screens want two, and each of them knows which. So
//! this file is the recipe instead — the documentation a widget would have carried, failing if the
//! recipe stops working.
//!
//! # The recipe
//!
//! ```ignore
//! // Unreflowed: a settings row. The value is short and the label is what is being navigated.
//! ListItem::new("Access point").trailing_value("Vivo Internet").build()
//!
//! // Reflowed: a details row. The value is long, or is the thing being read.
//! ListItem::new("Access point").secondary("Vivo Internet").build()
//! ```
//!
//! Choose by what the row is *for*, not by measuring it. The assertion below is what the choice
//! buys: on one line the value keeps its measured width and the label lives on what is left, so a
//! longer value is a shorter label; on two lines both get the whole row.

use symbian_decl_ui::layout;
use symbian_decl_ui::theme::FontRole;
use symbian_decl_ui::widgets::{ListItem, Node};
use symbian_decl_ui::{Rect, UiCache};
use symbian_gfx::E72_SCREEN;
use symbian_preview::Atlases;
use symbian_ui::Theme;

/// A row at the top of the handset's screen, the height the theme says a row is.
fn band(theme: &Theme<'_>) -> Rect {
    Rect::from_xywh(0, 0, E72_SCREEN.w, theme.metrics.row_h)
}

/// Place `root` in `at` and report every descendant's rect, in slot order.
fn rects(theme: &Theme<'_>, root: &Node, at: Rect) -> Vec<Rect> {
    let mut cache = UiCache::with_capacity(root.slot_count());
    layout::place_frame(root, at, &mut cache, theme);
    (0..root.slot_count())
        .map(|s| cache.rect(s).unwrap_or(Rect::from_xywh(0, 0, 0, 0)))
        .collect()
}

/// The real device atlases, not the one-glyph test atlas.
///
/// Everything below is about *widths of text*, and `symbian_ui::testing::with_theme` loads an atlas
/// whose every role is the same face with one renderable glyph. A measurement there would be
/// arithmetic on a fiction — see `tests/list_item_parity.rs`, which paid for this note.
fn with_real_theme<R>(f: impl FnOnce(&Theme<'_>) -> R) -> R {
    let atlases = Atlases::load();
    atlases.with_fonts(|fonts| f(&Theme::dark(fonts)))
}

/// A long value and a long label, so the two genuinely compete for one line.
const LABEL: &str = "Access point";
const VALUE: &str = "Vivo Internet Direto";

#[test]
fn the_real_atlas_measures_text_so_the_widths_below_mean_something() {
    // The negative control for every assertion in this file. Under the test atlas every string of
    // the same length measures the same, so "the value kept its width" would be true of a layout
    // that had thrown the text away.
    with_real_theme(|t| {
        assert!(FontRole::Small.measure(t, VALUE) > FontRole::Small.measure(t, "V"));
        assert!(FontRole::Strong.measure(t, LABEL) > 0);
    });
}

#[test]
fn on_one_line_the_label_gives_way_and_the_value_keeps_its_width() {
    // What the unreflowed row costs, stated as a measurement rather than as a warning. The value is
    // measured and placed at the end; the label takes the leftover through `flex(1)`, so a longer
    // value is a shorter label — and past some length the label is an ellipsis. That is the whole
    // reason a reflow exists in jQuery Mobile, and it is the whole reason to reach for `.secondary`
    // here.
    with_real_theme(|t| {
        let row = ListItem::new(LABEL).trailing_value(VALUE).build();
        // row, line, title, value — the shape `list_item.rs` asserts for a line with something at
        // its end. Named by slot rather than found by filtering, so this test fails loudly if the
        // tree changes under it instead of quietly measuring the wrong rect.
        let got = rects(t, &row, band(t));
        assert_eq!(got.len(), 4, "row, line, title, value: {got:?}");
        let (title, value) = (got[2], got[3]);

        assert_eq!(value.width(), FontRole::Small.measure(t, VALUE), "the value was squeezed");
        assert!(title.x1 <= value.x0, "the label ran under the value");
        // And the label is the one that pays for it: the same label beside a short value gets more
        // room. That is the dependency the reflow removes — stated as a comparison rather than as
        // "the label was truncated", because at 320 pixels these two strings both still fit and the
        // cost only becomes a truncation further along the same slope.
        let short = rects(t, &ListItem::new(LABEL).trailing_value("On").build(), band(t));
        assert!(
            short[2].width() > title.width(),
            "the label's share does not depend on the value at all"
        );
    });
}

#[test]
fn on_two_lines_both_the_label_and_the_value_get_the_whole_row() {
    // The reflowed state, and the property that makes it worth choosing: neither line is shortened
    // by the other, because they are two lines and not two columns. `list_item.rs`'s
    // `a_second_line_is_a_line_and_not_a_column` is the same finding from the other side.
    with_real_theme(|t| {
        let row = ListItem::new(LABEL).secondary(VALUE).build();
        // row, column, line, title, line, secondary.
        let got = rects(t, &row, band(t));
        assert_eq!(got.len(), 6, "row, column, line, title, line, secondary: {got:?}");
        let (title_line, value_line) = (got[2], got[4]);

        assert_eq!(title_line.width(), value_line.width(), "one line was cut short by the other");
        assert_eq!(title_line.x0, value_line.x0, "the second line is indented under the first");
        assert!(value_line.y0 >= title_line.y1, "the two lines overlap");
        // Wider than either line of the one-line form, which is the point of reflowing.
        let one_line = rects(t, &ListItem::new(LABEL).trailing_value(VALUE).build(), band(t));
        assert!(
            value_line.width() > one_line[2].width(),
            "the reflow bought the label nothing"
        );
    });
}

#[test]
fn the_reflowed_row_still_fits_the_band_a_list_reserves_for_it() {
    // The fourth finding, asserted: a two-line row is still 38 pixels tall, so it can go in a
    // `ScrollList` as an ordinary `RowHeight::Row`. That is what makes "the caller chooses" a
    // workable rule rather than a trap — the choice costs the list nothing, as long as the *whole
    // list* makes the same one.
    with_real_theme(|t| {
        let at = band(t);
        let row = ListItem::new(LABEL).secondary(VALUE).build();
        let got = rects(t, &row, at);
        assert_eq!(got[0], at, "the row grew past the band the list reserved");
        assert!(got.iter().all(|r| r.y1 <= at.y1), "a line escaped the row: {got:?}");
    });
}

#[test]
fn there_is_exactly_one_viewport_to_reflow_at() {
    // The second finding, pinned. A widget that switched at 320 pixels would need a second width to
    // switch *to*; if a portrait or a larger panel ever ships, this assertion is what fails and this
    // whole file is what gets reconsidered.
    assert_eq!((E72_SCREEN.w, E72_SCREEN.h), (320, 240));
}
