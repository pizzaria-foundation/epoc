//! [`ListItem`] against the hand-written row, on the fonts the handset actually ships.
//!
//! # Why this file exists next to the unit tests that already compare pixels
//!
//! Because those comparisons cannot see text, and finding that out cost three failed negative
//! controls.
//!
//! `symbian_ui::testing::with_theme` loads a **one-glyph test atlas**. Every font role in it reports
//! `line_height() == 12` and `advance() == 5`, and `glyph()` returns `None` for every character in
//! `"Wi-Fi"` — so `draw_text` paints nothing at all. A pixel comparison there proves the fills, the
//! rules, the icons and the *rects*; it cannot distinguish `FontRole::Small` from `FontRole::Body`,
//! and it cannot notice a label drawn at the wrong baseline.
//!
//! That is not a defect in the test atlas. It is what makes the arithmetic tests fast and hermetic,
//! and the crate's own design doc says so in as many words: the unit tests prove the properties a
//! machine can check, and only the real fonts can prove the pixels.
//!
//! What it means is that a test named `..._pixel_for_pixel` running on that atlas is making a
//! promise it cannot keep. So the typography half of the promise lives here, on
//! [`symbian_preview::Atlases`] — the same real `.sbf` files the device links in, chained through
//! `WithFallback` the same way — and through [`symbian_preview::Parity`], which writes a diff map on
//! failure instead of dumping sixty thousand `u16`s into a terminal.

use symbian_decl_ui::layout::{self, CrossAlign};
use symbian_decl_ui::spacing::{Gap, Pad};
use symbian_decl_ui::theme::FontRole;
use symbian_decl_ui::widgets::{Ink, ListItem, Node, Row, Text};
use symbian_decl_ui::UiCache;
use symbian_gfx::{Align, Canvas, Rect, E72_SCREEN};
use symbian_preview::{Atlases, Parity};
use symbian_ui::{chrome, Theme};

/// One row, at the top of the screen, the height the theme says a row is.
fn row_band(theme: &Theme<'_>) -> Rect {
    Rect::from_xywh(0, 0, E72_SCREEN.w, theme.metrics.row_h)
}

/// Draw a declared tree into `band`.
fn draw(c: &mut Canvas<'_>, theme: &Theme<'_>, band: Rect, root: &Node) {
    chrome::clear(c, theme);
    let mut cache = UiCache::with_capacity(root.slot_count());
    layout::draw_frame(root, band, &mut cache, c, theme);
}

/// The hand-written setting item: a stretched row, the toolkit's side margin, a strong label taking
/// the leftover, a small value against the right edge.
///
/// Assembled from `Row` and `Text` with no [`ListItem`] anywhere in it — two independent routes to
/// the same pixels, which is the only comparison worth making.
fn by_hand_setting(label: &str, value: &str) -> Node {
    Node::Group(
        Row::new()
            .align(CrossAlign::Stretch)
            .padding(Pad::xy(Gap::Base, Gap::None))
            .gap(Gap::Base)
            .child(Text::new(label).font(FontRole::Strong).ink(Ink::Text).flex(1))
            .child(Text::new(value).font(FontRole::Small).ink(Ink::Dim).align(Align::End)),
    )
}

/// The hand-written chat row: an avatar, then two stacked lines pushed to the row's two edges.
fn by_hand_two_line(name: &str, preview: &str, time: &str) -> Node {
    use symbian_decl_ui::layout::MainAlign;
    use symbian_decl_ui::widgets::Column;

    Node::Group(
        Row::new()
            .align(CrossAlign::Stretch)
            .padding(Pad::xy(Gap::Base, Gap::None))
            .gap(Gap::Base)
            .group(
                Column::new()
                    .justify(MainAlign::SpaceBetween)
                    .fill(1)
                    .group(
                        Row::new()
                            .align(CrossAlign::Stretch)
                            .child(Text::new(name).font(FontRole::Strong).ink(Ink::Text).flex(1))
                            .child(Text::new(time).font(FontRole::Small).ink(Ink::Dim).align(Align::End)),
                    )
                    // The preview in a row of its own, and that wrapper is the whole point rather
                    // than noise. Written as a bare `Text` with `flex(1)` — which is how the first
                    // version of this reference had it, copied from the implementation — the leaf's
                    // weight is read by the **column's** axis and claims leftover *height*. Inside a
                    // 38-pixel list row that is a couple of pixels and reads as padding; inside a
                    // 205-pixel band it was a 189-pixel row with everything below it off the screen.
                    //
                    // A hand-written row would not have that bug, because a person writing one
                    // anchors the preview and does not ask it to flex vertically. So this is the
                    // reference being corrected, not bent to match the implementation.
                    .group(
                        Row::new()
                            .align(CrossAlign::Stretch)
                            .child(Text::new(preview).font(FontRole::Small).ink(Ink::Dim).flex(1)),
                    ),
            ),
    )
}

#[test]
fn a_row_is_the_hand_written_row_on_the_real_fonts() {
    let atlases = Atlases::load();
    let mut failures = Vec::new();

    atlases.with_themes(|dark, light| {
        // Both palettes, because the inks a row picks are roles and a role resolves differently in
        // each — a row that hard-coded a colour would pass on one and fail on the other.
        for (name, theme) in [("dark", dark), ("light", light)] {
            let band = row_band(theme);
            let mut parity = Parity::new(symbian_preview::parity::default_out_dir());

            // --- a setting item, unselected and selected
            for (tag, selected) in [("", false), ("-selected", true)] {
                let hand = by_hand_setting("Access point", "Vivo Internet");
                let declared = ListItem::new("Access point")
                    .selected(selected)
                    .trailing_value("Vivo Internet")
                    .build();
                // The selected case is compared against a hand-written row wearing the selection
                // inks *and* the band `ScrollList` paints under it, since a row never draws its own.
                let ok = parity.check(
                    &format!("listitem-setting{tag}-{name}"),
                    theme,
                    |c| {
                        chrome::clear(c, theme);
                        if selected {
                            chrome::selection(c, band, theme);
                        }
                        let hand = if selected {
                            Node::Group(
                                Row::new()
                                    .align(CrossAlign::Stretch)
                                    .padding(Pad::xy(Gap::Base, Gap::None))
                                    .gap(Gap::Base)
                                    .child(
                                        Text::new("Access point")
                                            .font(FontRole::Strong)
                                            .ink(Ink::Selection)
                                            .flex(1),
                                    )
                                    .child(
                                        Text::new("Vivo Internet")
                                            .font(FontRole::Small)
                                            .ink(Ink::Selection)
                                            .align(Align::End),
                                    ),
                            )
                        } else {
                            hand
                        };
                        let mut cache = UiCache::with_capacity(hand.slot_count());
                        layout::draw_frame(&hand, band, &mut cache, c, theme);
                    },
                    |c| {
                        chrome::clear(c, theme);
                        if selected {
                            chrome::selection(c, band, theme);
                        }
                        let mut cache = UiCache::with_capacity(declared.slot_count());
                        layout::draw_frame(&declared, band, &mut cache, c, theme);
                    },
                );
                if !ok {
                    failures.push(format!("setting{tag} on {name}"));
                }
            }

            // --- a two-line row with a timestamp
            let hand = by_hand_two_line("Ana Ribeiro", "see you at eight", "14:32");
            let declared = ListItem::new("Ana Ribeiro")
                .secondary("see you at eight")
                .trailing(Text::new("14:32").font(FontRole::Small).ink(Ink::Dim).align(Align::End))
                .build();
            if !parity.check(
                &format!("listitem-two-line-{name}"),
                theme,
                |c| draw(c, theme, band, &hand),
                |c| draw(c, theme, band, &declared),
            ) {
                failures.push(format!("two-line on {name}"));
            }

            assert_eq!(parity.checked(), 3, "three scenes per palette");
            if !parity.diffs().is_empty() {
                failures.push(parity.report());
            }
        }
    });

    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn the_two_line_row_puts_its_second_line_against_the_bottom() {
    // The geometry the fix settled on, pinned to numbers so the two pixels it moved cannot drift
    // again unnoticed. `SpaceBetween` over two 12-pixel lines in a 38-pixel row: the first line's box
    // at the top, the second's against the bottom, and each text filling its own line rather than a
    // share of the band.
    //
    // Worth pinning rather than trusting the parity above, because the parity compares this crate
    // against a reference in this same file — and a reference that agrees too readily is exactly the
    // mistake this file's own header records.
    let atlases = Atlases::load();
    atlases.with_themes(|dark, _light| {
        let band = row_band(dark);
        let declared = ListItem::new("Ana Ribeiro")
            .secondary("see you at eight")
            .trailing(Text::new("14:32").font(FontRole::Small).ink(Ink::Dim).align(Align::End))
            .build();
        let mut cache = UiCache::with_capacity(declared.slot_count());
        layout::place_frame(&declared, band, &mut cache, dark);

        let row = cache.rect(0).expect("the row");
        let column = cache.rect(1).expect("the column");
        assert_eq!(row.height(), dark.metrics.row_h, "the row is a row, not a band");
        assert_eq!(column.height(), row.height(), "the column fills it");

        // Slot 2 is the first line's box, slot 5 the second's — see `ListItem::line`.
        let first = cache.rect(2).expect("the first line");
        let second = cache.rect(5).expect("the second line");
        assert_eq!(first.y0, row.y0, "the first line against the top");
        assert_eq!(second.y1, row.y1, "the second against the bottom");

        // Each line is exactly as tall as *its own* font, which is the precise statement of "no line
        // claims the leftover". Asserting the two are equal to each other would be wrong and was: the
        // title is `Strong` at 17 pixels and the preview is `Small` at 14, so equality fails for a
        // reason that has nothing to do with the bug.
        assert_eq!(first.height(), dark.fonts.strong.line_height(), "the title's line");
        assert_eq!(second.height(), dark.fonts.small.line_height(), "the preview's line");
        assert!(second.y0 > first.y1, "with clear space between them");
    });
}

#[test]
fn the_comparison_can_see_a_font_role() {
    // The negative control, and the reason this file exists. On the test atlas this assertion is
    // *unprovable*: every role is the same face, so swapping one changes nothing. On the real
    // atlases a value set in body weight instead of small is a different row, and if this ever goes
    // green the comparison has stopped looking at text.
    let atlases = Atlases::load();
    atlases.with_themes(|dark, _light| {
        let band = row_band(dark);
        let wrong = Node::Group(
            Row::new()
                .align(CrossAlign::Stretch)
                .padding(Pad::xy(Gap::Base, Gap::None))
                .gap(Gap::Base)
                .child(Text::new("Access point").font(FontRole::Strong).ink(Ink::Text).flex(1))
                // Body, not Small: the one thing the test atlas cannot tell apart.
                .child(Text::new("Vivo Internet").font(FontRole::Body).ink(Ink::Dim).align(Align::End)),
        );
        let declared = ListItem::new("Access point").trailing_value("Vivo Internet").build();

        let mut parity = Parity::new(symbian_preview::parity::default_out_dir());
        let matched = parity.check(
            "listitem-negative-control",
            dark,
            |c| draw(c, dark, band, &wrong),
            |c| draw(c, dark, band, &declared),
        );
        assert!(!matched, "a wrong font role went unnoticed: the comparison is not reading text");
    });
}

#[test]
fn the_comparison_can_see_a_missing_stretch() {
    // The other half of the same guard, for the setting the module docs call load-bearing. A row
    // without it draws its text ten pixels high in a 38-pixel band.
    let atlases = Atlases::load();
    atlases.with_themes(|dark, _light| {
        let band = row_band(dark);
        let flat = Node::Group(
            Row::new()
                .padding(Pad::xy(Gap::Base, Gap::None))
                .gap(Gap::Base)
                .child(Text::new("Access point").font(FontRole::Strong).ink(Ink::Text).flex(1))
                .child(Text::new("Vivo Internet").font(FontRole::Small).ink(Ink::Dim).align(Align::End)),
        );
        let declared = ListItem::new("Access point").trailing_value("Vivo Internet").build();

        let mut parity = Parity::new(symbian_preview::parity::default_out_dir());
        let matched = parity.check(
            "listitem-negative-stretch",
            dark,
            |c| draw(c, dark, band, &flat),
            |c| draw(c, dark, band, &declared),
        );
        assert!(!matched, "the cross-axis stretch went unnoticed");
    });
}

#[test]
fn the_real_atlas_actually_draws_text() {
    // The assumption everything above rests on, asserted rather than trusted — because the test
    // atlas looked exactly like this from the outside and painted nothing. If this fails, every
    // parity test in this file has been comparing blank screens.
    let atlases = Atlases::load();
    atlases.with_fonts(|fonts| {
        assert!(fonts.body.glyph('W').is_some(), "the body font has no 'W'");
        assert!(fonts.small.glyph('W').is_some(), "the small font has no 'W'");
        assert!(
            fonts.small.line_height() < fonts.body.line_height(),
            "small and body are the same face: {} vs {}",
            fonts.small.line_height(),
            fonts.body.line_height()
        );
        assert!(fonts.body.measure("Wi-Fi") > 0);
    });
}
