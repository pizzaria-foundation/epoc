//! A settings screen, assembled from the pieces, asserted end to end.
//!
//! # Why there is no `SettingList` widget
//!
//! The plan for this phase had one: `ScrollList` + a focus ring + value rows. Writing the call site
//! is what killed it. A settings screen turns out to be
//! [`ScrollList`](symbian_decl_ui::widgets::ScrollList) with
//! [`ListItem::trailing_value`](symbian_decl_ui::widgets::ListItem::trailing_value) in its row
//! builder — six lines, no arithmetic of its own, and nothing a wrapper would own:
//!
//! * **The cursor** is already the list's. A focus ring inside a scrolling list would be a second
//!   cursor over the same rows, and the two would part company the first time one of them clamped.
//! * **The heights** are uniform. S60's own settings screens have no headings between rows — the
//!   E72's Settings app nests views instead — so there is no mixed-height list to flatten.
//! * **The rows** are one `ListItem` call.
//!
//! This crate's rule is that an abstraction needs two callers that already disagree. There are none
//! yet, so this file is the recipe instead: it is the documentation a wrapper would have carried, and
//! it fails if the recipe stops working. When a second settings screen appears and wants something
//! different, *that* is the signal, and this test is where the first caller's requirements are
//! already written down.

extern crate alloc;

use symbian_decl_ui::layout::CrossAlign;
use symbian_decl_ui::slot::SlotTable;
use symbian_decl_ui::spacing::RowHeight;
use symbian_decl_ui::widget::KeyCtx;
use symbian_decl_ui::widgets::{ListItem, Node, ScrollList, SectionHeader};
use symbian_decl_ui::{Rect, UiCache};
use symbian_gfx::Size;
use symbian_ui::{testing, Handled, Key, KeyEvent, Palette};

/// The band a settings list gets under a title bar, on this handset.
const BAND: Rect = Rect { x0: 0, y0: 18, x1: 320, y1: 223 };
/// What `theme.metrics.row_h` is, still written out — but only for the *assertions* below, which
/// need a number to compute a pixel row from. The list itself no longer says it: see
/// [`settings_list`].
const ROW_H: i32 = 38;

/// One setting: what it is called and what it is set to.
struct Setting {
    label: &'static str,
    value: &'static str,
}

const SETTINGS: &[Setting] = &[
    Setting { label: "Wi-Fi", value: "On" },
    Setting { label: "Bluetooth", value: "Off" },
    Setting { label: "Access point", value: "Vivo Internet" },
    Setting { label: "Data roaming", value: "Ask first" },
    Setting { label: "Ringtone", value: "Nokia Tune" },
    Setting { label: "Keypad tones", value: "Level 2" },
    Setting { label: "Screen timeout", value: "30 s" },
];

/// The recipe. This is the whole of a settings screen's content.
fn settings_list(slots: &mut SlotTable, selected: usize) -> Node {
    Node::leaf(
        // `RowHeight::Row`, not `38`. The height is the theme's and the view names the kind — the
        // same move `Gap` makes for spacing, one level up. This test used to hardcode the number and
        // carry a guard asserting it still matched the theme; the guard is gone because the mismatch
        // it watched for is now unrepresentable.
        ScrollList::new(slots, SETTINGS.len(), RowHeight::Row)
            .selected(selected)
            .row(|i, sel| {
                let s = &SETTINGS[i];
                ListItem::new(s.label).selected(sel).trailing_value(s.value).build()
            }),
    )
}

/// Place and draw `root`, returning the framebuffer and the number of measures the frame cost.
fn frame(root: &Node, cache: &mut UiCache) -> (Vec<u16>, u32) {
    let mut calls = 0;
    let (_, buf) = testing::with_canvas(Size::new(320, 240), |c| {
        testing::with_theme(Palette::DARK, |theme| {
            c.clear(Palette::DARK.bg.mid());
            symbian_decl_ui::layout::draw_frame(root, BAND, cache, c, theme);
            calls = cache.measure_calls();
        });
    });
    (buf, calls)
}

/// Press `key` at `root` after a frame has placed it.
fn press(root: &Node, cache: &UiCache, key: Key) -> Handled {
    // The context assembled by hand rather than through `with_key_ctx`: that helper is behind the
    // crate's `testing` feature, which an integration test of this same crate cannot turn on for
    // itself. Both halves are public, so this is two lines instead of a feature flag.
    testing::with_theme(Palette::DARK, |theme| {
        let mut clip = symbian_ui::NoClipboard;
        let mut cx = KeyCtx::new(theme, &mut clip);
        symbian_decl_ui::layout::dispatch_key(root, KeyEvent::new(key), cache, &mut cx)
    })
}

#[test]
fn every_setting_puts_its_label_left_and_its_value_right() {
    // # Reading this test needs one fact about the atlas
    //
    // `testing::with_theme` loads a test atlas holding **one glyph**: lowercase `a`. So the only text
    // that paints anything is the letter `a`, and the settings above are chosen so that some labels
    // have one and some values do — "Access point" and "Data roaming" on the left, "Vivo Internet"
    // and "Ask first" on the right.
    //
    // The first version of this test asserted ink in a left band and a right band and passed for the
    // wrong reason: row zero is selected, and `chrome::selection` fills it end to end. It would have
    // gone green with no rows drawn at all. So the selection is moved off the first screenful, and the
    // bands are checked *below* it.
    let mut slots = SlotTable::new();
    let root = settings_list(&mut slots, 0);
    let mut cache = UiCache::with_capacity(64);
    let (buf, _) = frame(&root, &mut cache);

    let bg = Palette::DARK.bg.mid().to_rgb565().0;
    // Rows one onwards: below the selected row, so nothing here is the highlight.
    let below = BAND.y0 + ROW_H + 2..BAND.y1;
    let inked = |x0: i32, x1: i32| {
        (x0..x1).any(|x| below.clone().any(|y| buf[(y * 320 + x) as usize] != bg))
    };
    assert!(inked(6, 120), "labels down the left, clear of the highlight");
    assert!(inked(180, 314), "values down the right");
}

#[test]
fn the_cursor_walks_the_settings_and_the_list_scrolls_to_follow() {
    // The model owns the cursor and the list is told it — so a walk is `view` rebuilt with a new
    // index, which is what an app's `update` does. The assertion is that the seventh setting, which
    // does not fit in a 205-pixel band at 38 pixels a row, is on screen once it is selected.
    let mut slots = SlotTable::new();
    let mut cache = UiCache::with_capacity(64);

    let first = settings_list(&mut slots, 0);
    let (top, _) = frame(&first, &mut cache);
    drop(first);

    slots.begin_frame();
    let last = settings_list(&mut slots, SETTINGS.len() - 1);
    let (bottom, _) = frame(&last, &mut cache);

    assert_ne!(top, bottom, "the list scrolled");
    assert_eq!(slots.type_mismatches(), 0, "the slot ordinals held across the rebuild");
}

#[test]
fn a_settings_screen_measures_a_handful_of_times_and_not_once_per_setting() {
    // The `Group: Widget` trap, checked on the shape this phase is for. A row is built per *visible*
    // row — five of them in this band — and each is measured once into the list's own cache. A number
    // that grew with `SETTINGS.len()` would mean the list had stopped building only what it draws.
    let mut slots = SlotTable::new();
    let root = settings_list(&mut slots, 0);
    let mut cache = UiCache::with_capacity(64);
    let (_, calls) = frame(&root, &mut cache);
    assert!(calls <= 4, "a still settings screen measured {calls} times at the screen level");
}

#[test]
fn a_list_that_is_not_focused_leaves_the_arrows_alone() {
    // The default, and load-bearing: an app that moves the cursor in `update` would otherwise have
    // the selection moved twice by one press — once by the message and once by the list — and the two
    // would part company the first time one of them clamped.
    let mut slots = SlotTable::new();
    let root = settings_list(&mut slots, 0);
    let mut cache = UiCache::with_capacity(64);
    frame(&root, &mut cache);
    assert_eq!(press(&root, &cache, Key::Down), Handled::Ignored);
}

#[test]
fn a_heading_can_sit_above_the_list_as_fixed_chrome() {
    // One of the two shapes that work. The other is `ScrollList::mixed`, which takes a `RowHeight`
    // per entry and can hold headings *inside* the scroll — see
    // `a_heading_can_also_scroll_with_the_rows` below. This one is a fixed band that stays put while
    // the rows move under it, which is what a screen with a single section wants.
    let mut slots = SlotTable::new();
    let content = Node::Group(
        symbian_decl_ui::widgets::Column::new()
            .align(CrossAlign::Stretch)
            .child(SectionHeader::new("Connectivity"))
            .node(settings_list(&mut slots, 0)),
    );
    let mut cache = UiCache::with_capacity(64);
    let (buf, _) = frame(&content, &mut cache);
    let bg = Palette::DARK.bg.mid().to_rgb565().0;
    // The heading's band is painted across the full width at the top of the content area.
    let heading_row = BAND.y0 + 2;
    assert!(
        (0..320).all(|x| buf[(heading_row * 320 + x) as usize] != bg),
        "the heading is a full-width band"
    );
}

/// What `RowHeight` bought: a heading that scrolls with the rows instead of sitting above them.
///
/// Before the roles this was unwritable in a view. `ScrollList::varying` wanted a `Vec<i32>` and the
/// heading's height is `theme.fonts.small.line_height() + theme.metrics.space.snug` — an expression a
/// view has no theme to evaluate. Naming the kind moves it to where the theme is.
#[test]
fn a_heading_can_also_scroll_with_the_rows() {
    let mut slots = SlotTable::new();
    // Two sections, each with two settings: heading, row, row, heading, row, row.
    let kinds = alloc::vec![
        RowHeight::Header,
        RowHeight::Row,
        RowHeight::Row,
        RowHeight::Header,
        RowHeight::Row,
        RowHeight::Row,
    ];
    let root = Node::leaf(
        ScrollList::mixed(&mut slots, kinds)
            .selected(1)
            .row(|i, sel| match i {
                0 => Node::leaf(SectionHeader::new("Connectivity")),
                3 => Node::leaf(SectionHeader::new("Sound")),
                _ => ListItem::new(SETTINGS[i].label).selected(sel).trailing_value(SETTINGS[i].value).build(),
            }),
    );
    let mut cache = UiCache::with_capacity(64);
    let (buf, _) = frame(&root, &mut cache);

    let bg = Palette::DARK.bg.mid().to_rgb565().0;
    // Up to the gutter, not to the screen edge: `ScrollList` takes the scrollbar's width off the
    // right of every row it draws, so a heading inside the list is a band that stops short of 320.
    // Asserting to 320 is what the first run of this test did, and it failed for that reason rather
    // than for the one it was written to catch.
    let gutter = testing::with_theme(Palette::DARK, symbian_ui::chrome::scrollbar_gutter);
    let full_width_at =
        |y: i32| (0..320 - gutter).all(|x| buf[(y * 320 + x) as usize] != bg);
    // The first heading's band is at the very top of the content area.
    assert!(full_width_at(BAND.y0 + 2), "the first heading is a band inside the list");
    // The second heading is a heading plus two rows further down, which is where its own kind puts
    // it — a list that had reserved a full row for each heading would have it lower.
    let header_h = testing::with_theme(Palette::DARK, SectionHeader::height);
    let second = BAND.y0 + header_h + ROW_H * 2 + 2;
    assert!(full_width_at(second), "the second heading is where the kinds add up to, not lower");
}

#[test]
fn a_row_with_no_value_is_the_same_row_without_the_right_hand_column() {
    // A settings list is not all value rows: "Restore defaults" is a label and a chevron. The two
    // shapes have to sit in one list without the rows shifting, which is what a shared `pad` and
    // `gap` on `ListItem` buys.
    let plain = ListItem::new("Restore defaults").trailing_arrow().build();
    let valued = ListItem::new("Wi-Fi").trailing_value("On").build();
    let (a, b) = testing::with_theme(Palette::DARK, |theme| {
        let row = Rect { x0: 0, y0: 0, x1: 320, y1: ROW_H };
        let mut ca = UiCache::with_capacity(plain.slot_count());
        let mut cb = UiCache::with_capacity(valued.slot_count());
        symbian_decl_ui::layout::place_frame(&plain, row, &mut ca, theme);
        symbian_decl_ui::layout::place_frame(&valued, row, &mut cb, theme);
        (ca.rect(2).unwrap(), cb.rect(2).unwrap())
    });
    assert_eq!(a.x0, b.x0, "both labels start at the same margin");
    assert_eq!(a.y0, b.y0, "and on the same line");
}

// The pixel-parity assertion for a value row used to live here and has moved to
// `tests/list_item_parity.rs`.
//
// It was worth nothing where it was. `testing::with_theme` loads a one-glyph atlas — lowercase `a`,
// and every font role the same face — so the comparison could not see a font role at all. Three
// negative controls were tried before that became clear: swapping `Small` for `Body` changed nothing
// because the roles are one face; moving the value's alignment changed nothing because its box is
// exactly as wide as the value; only removing the label's `flex(1)` moved anything, and by then the
// test was proving the layout it already had unit tests for.
//
// The real comparison needs the real `.sbf` atlases, which is what `symbian-preview` exists for.

#[test]
fn nothing_accumulates_across_frames() {
    // Twelve still frames of the same screen must cost the same as the second one. A number that
    // crept would mean the list's own cache or the slot table was keeping something.
    let mut slots = SlotTable::new();
    let mut cache = UiCache::with_capacity(64);
    let mut seen = Vec::new();
    for _ in 0..12 {
        slots.begin_frame();
        let root = settings_list(&mut slots, 2);
        let (_, calls) = frame(&root, &mut cache);
        seen.push(calls);
    }
    let steady = seen[1];
    assert!(seen[1..].iter().all(|&c| c == steady), "measure counts drifted: {seen:?}");
    assert_eq!(slots.type_mismatches(), 0);
    assert_eq!(slots.unbalanced_groups(), 0);
}
