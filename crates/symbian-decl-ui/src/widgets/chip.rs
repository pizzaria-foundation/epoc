//! A state, as a coloured pill at the end of a row.
//!
//! ```ignore
//! ListItem::new(&pkg.name)
//!     .selected(sel)
//!     .trailing(Chip::warn("UID clash").selected(sel))
//!     .build()
//! ```
//!
//! # It is the pill, not the row
//!
//! [`symbian_ui::Chip`] is already only the pill — unlike `Toggle`, which draws a whole settings row
//! — so there was nothing to extract and nothing to reimplement. What this adds is the one thing the
//! imperative chip cannot do: be *measured* alongside everything else on the line, so a name and a
//! state can be laid out against each other instead of the state being spelled into the end of the
//! name and pushed off a 320-pixel screen.
//!
//! # The width is asked, never reconstructed
//!
//! [`symbian_ui::Chip::width`] is the authority and [`symbian_ui::chip::height`] is the other half of
//! the box. Both are the numbers [`symbian_ui::chrome::badge`] actually paints with.
//!
//! Copying that arithmetic here instead would be [`Badge`](super::Badge)'s defect exactly: its
//! `measure` computed `text + h` where its `draw` used `text + 8`, and the symptom was not a fat pill
//! — the draw was right — but a row **truncating a character early**, because the measured width is
//! what the layout divides the line by. A widget whose box and whose ink come from two different
//! calculations fails at the neighbour, not at itself.
//!
//! # Colour is never the only signal
//!
//! Every chip carries a word, and on the selection band all four tones collapse to one legible pair —
//! see [`symbian_ui::Chip::draw_right_on`]. That collapse is affordable *because* of the word, which
//! is why [`Chip::new`] has no way to make a wordless one.

use alloc::string::String;

use symbian_gfx::{Canvas, Rect, Size};
use symbian_ui::chip;
use symbian_ui::{Chip as Pill, Theme, Tone};

use crate::constraints::Constraints;
use crate::widget::{hash_i32, hash_str, Widget, WidgetHash};

/// A word and a tone, as a box in the layout.
pub struct Chip {
    /// Owned, because a `view` is built each frame from a model whose strings it does not outlive —
    /// the same reason [`Text`](super::Text) and [`Badge`](super::Badge) own theirs. The imperative
    /// [`symbian_ui::Chip`] borrows because it is constructed and drawn in one statement.
    text: String,
    tone: Tone,
    /// Whether this sits on the selected row. Colours only; the box is identical.
    selected: bool,
}

impl Chip {
    pub fn new(text: impl Into<String>, tone: Tone) -> Self {
        Self { text: text.into(), tone, selected: false }
    }

    /// Nothing to do. Quiet on purpose — a list where every row is coloured is a list where nothing
    /// stands out.
    pub fn calm(text: impl Into<String>) -> Self {
        Self::new(text, Tone::Calm)
    }

    /// Something is on offer, or something arrived.
    pub fn fresh(text: impl Into<String>) -> Self {
        Self::new(text, Tone::Fresh)
    }

    /// Worth a look before acting.
    pub fn warn(text: impl Into<String>) -> Self {
        Self::new(text, Tone::Warn)
    }

    /// Something is happening right now.
    pub fn busy(text: impl Into<String>) -> Self {
        Self::new(text, Tone::Busy)
    }

    /// Whether this chip is on the selected row.
    ///
    /// Passed in rather than discovered, exactly like [`Badge::new`](super::Badge::new)'s second
    /// argument and [`Switch::focused`](super::Switch::focused): a widget cannot see the band behind
    /// it, and a chip that guessed would be wrong on whichever screen guessed differently.
    pub fn selected(mut self, on: bool) -> Self {
        self.selected = on;
        self
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn tone(&self) -> Tone {
        self.tone
    }

    /// The imperative chip this is a shell over. One place the two are tied together, so `measure`
    /// and `draw` cannot end up asking different objects.
    fn pill(&self) -> Pill<'_> {
        Pill::new(&self.text, self.tone)
    }
}

impl Widget for Chip {
    fn content_hash(&self) -> WidgetHash {
        // The text, because it is what the width is measured from. The tone, because it need not stay
        // size-neutral — a theme that gave `Warn` a bolder face would change the box, and a digest
        // that ignored the tone would keep the lighter one's measurement.
        //
        // `selected` is *not* in here: it chooses two colours and moves no edge. It is out for the
        // same reason `Switch` leaves `on` out — a row that re-measured every time the cursor passed
        // over it would put the whole list on the slow path while somebody scrolls.
        //
        // Never zero: `hash_str` is seeded, so even an empty label digests to something.
        hash_i32(hash_str(0, &self.text), self.tone as u8 as i32)
    }

    fn measure(&self, constraints: Constraints, theme: &Theme<'_>) -> Size {
        // Both numbers asked of `symbian_ui`, neither reconstructed. See the module docs on what
        // reconstructing one cost `Badge`.
        constraints.constrain(Size::new(self.pill().width(theme), chip::height(theme)))
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        // `draw_right_on` right-aligns against `rect.x1` and centres vertically inside it, which is
        // what carries this widget through `CrossAlign::Stretch`: a list row hands over its whole
        // 38-pixel band, and a pill anchored to the band's top edge would float above the text it
        // annotates.
        //
        // Right-aligned rather than filling: the rect is already the measured width in the ordinary
        // case, so the two agree — and when the line was too narrow to grant it, a pill that grew to
        // its rect would be a different shape from the one every other row shows.
        self.pill().draw_right_on(c, rect, theme, self.selected);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian_ui::{testing, Palette};

    const ROW: Rect = Rect { x0: 0, y0: 0, x1: 160, y1: 38 };

    fn paint_in(palette: Palette, chip: &Chip, rect: Rect) -> Vec<u16> {
        let (_, buf) = testing::with_canvas(Size::new(160, 38), |c| {
            testing::with_theme(palette, |t| {
                c.clear(palette.bg.mid());
                chip.draw(c, rect, t);
            });
        });
        buf
    }

    #[test]
    fn the_width_reported_is_the_width_the_toolkit_paints() {
        // The `Badge` defect, which is what this whole widget is careful about: a `measure` that
        // reconstructed the arithmetic would disagree with the draw, and the symptom would be the row
        // *beside* it truncating a character early rather than a misshapen pill.
        for (name, palette) in Palette::ALL {
            testing::with_theme(palette, |t| {
                for c in [Chip::calm("ok"), Chip::warn("UID clash"), Chip::busy("a")] {
                    let got = c.measure(Constraints::loose(320, 38), t);
                    assert_eq!(
                        got,
                        Size::new(Pill::new(c.text(), c.tone()).width(t), chip::height(t)),
                        "{name}: {}",
                        c.text()
                    );
                }
            });
        }
    }

    #[test]
    fn a_wider_word_makes_a_wider_pill_and_never_a_taller_one() {
        testing::with_theme(Palette::DARK, |t| {
            let offer = Constraints::loose(320, 38);
            let short = Chip::calm("a").measure(offer, t);
            let long = Chip::warn("aaaaaaaaaaaa").measure(offer, t);
            assert!(long.w > short.w, "{long:?} is no wider than {short:?}");
            assert_eq!(long.h, short.h, "the height is the font's, not the text's");
            assert!(short.w >= short.h, "a two-letter chip must stay a pill, not become a slot");
        });
    }

    #[test]
    fn it_never_answers_wider_than_the_line_it_was_offered() {
        // A fixed child that answers with more than the offer is a child the layout has to correct,
        // and the correction shows as a neighbour losing room it was already promised.
        testing::with_theme(Palette::DARK, |t| {
            assert_eq!(Chip::warn("aaaaaaaaaaaaaaaa").measure(Constraints::loose(20, 38), t).w, 20);
            assert_eq!(Chip::warn("a").measure(Constraints::loose(0, 0), t), Size::new(0, 0));
        });
    }

    #[test]
    fn the_stretch_a_list_row_applies_does_not_stretch_the_pill() {
        // The trap every control in this catalogue shares: `CrossAlign::Stretch` hands over the whole
        // 38-pixel band, and a pill drawn from the band's top edge floats above the text it annotates.
        let buf = paint_in(Palette::DARK, &Chip::fresh("a"), ROW);
        let bg = Palette::DARK.bg.mid().to_rgb565().0;
        let rows: Vec<i32> =
            (0..38).filter(|&y| (0..160).any(|x| buf[(y * 160 + x) as usize] != bg)).collect();
        let h = testing::with_theme(Palette::DARK, chip::height);
        assert_eq!(rows.len() as i32, h, "the pill took the band's height");
        assert_eq!(rows[0], (38 - h) / 2, "and was not centred in it");
    }

    #[test]
    fn the_pill_lands_inside_the_rect_the_layout_gave_it() {
        // The measured width is what the row divided the line by, so a pill that painted outside its
        // rect would be painting on its neighbour. `Badge` deliberately overflows *vertically* and
        // says so with `overflow_visible`; this one has no such licence horizontally.
        let w = testing::with_theme(Palette::DARK, |t| {
            Chip::calm("a").measure(Constraints::loose(160, 38), t).w
        });
        let rect = Rect::from_xywh(160 - w, 0, w, 38);
        let buf = paint_in(Palette::DARK, &Chip::calm("a"), rect);
        let bg = Palette::DARK.bg.mid().to_rgb565().0;
        let mut any = false;
        for y in 0..38 {
            for x in 0..160 {
                if buf[y * 160 + x] != bg {
                    any = true;
                    assert!((rect.x0 as usize..rect.x1 as usize).contains(&x), "ink at {x},{y}");
                }
            }
        }
        assert!(any, "nothing was painted, so the bound above proved nothing");
    }

    #[test]
    fn the_declared_chip_paints_what_the_imperative_one_paints() {
        // Parity, and cheap because both go through `Chip::draw_right_on`. Worth asserting anyway:
        // the point is that these can never become two chips, and a test is what keeps a future edit
        // from putting a second sizing rule into one of them.
        for (name, palette) in Palette::ALL {
            for selected in [false, true] {
                let mine = {
                    let (_, b) = testing::with_canvas(Size::new(160, 38), |c| {
                        testing::with_theme(palette, |t| {
                            c.clear(palette.bg.mid());
                            Chip::warn("a").selected(selected).draw(c, ROW, t);
                        });
                    });
                    b
                };
                let theirs = {
                    let (_, b) = testing::with_canvas(Size::new(160, 38), |c| {
                        testing::with_theme(palette, |t| {
                            c.clear(palette.bg.mid());
                            Pill::warn("a").draw_right_on(c, ROW, t, selected);
                        });
                    });
                    b
                };
                assert_eq!(mine, theirs, "{name} selected={selected}");
            }
            // The negative control: the two states are different pictures, so the equalities above
            // are not comparing a blank canvas with itself.
            assert_ne!(
                paint_in(palette, &Chip::warn("a"), ROW),
                paint_in(palette, &Chip::warn("a").selected(true), ROW),
                "{name}: the selection band changed nothing"
            );
        }
    }

    #[test]
    fn the_four_tones_are_four_pictures_off_the_band() {
        // If two tones painted alike the semantic distinction would exist only in the source. Note
        // the qualifier: *off* the band. On it they collapse deliberately — see the module docs.
        for (name, palette) in Palette::ALL {
            let shots: Vec<_> = [Tone::Calm, Tone::Fresh, Tone::Warn, Tone::Busy]
                .iter()
                .map(|&t| paint_in(palette, &Chip::new("a", t), ROW))
                .collect();
            for i in 0..shots.len() {
                for j in i + 1..shots.len() {
                    assert_ne!(shots[i], shots[j], "{name}: tones {i} and {j} paint the same");
                }
            }
        }
    }

    #[test]
    fn selection_changes_the_colours_and_not_the_size() {
        testing::with_theme(Palette::DARK, |t| {
            let offer = Constraints::loose(320, 38);
            assert_eq!(
                Chip::calm("a").measure(offer, t),
                Chip::calm("a").selected(true).measure(offer, t)
            );
        });
        // And the digest agrees, because re-measuring a row as the cursor passes over it would put
        // the whole list on the slow path while somebody scrolls.
        assert_eq!(Chip::calm("a").content_hash(), Chip::calm("a").selected(true).content_hash());
    }

    #[test]
    fn the_text_and_the_tone_are_in_the_digest_and_it_is_never_zero() {
        assert_ne!(Chip::calm("ok").content_hash(), Chip::calm("rebuild").content_hash());
        assert_ne!(Chip::calm("ok").content_hash(), Chip::warn("ok").content_hash());
        assert_ne!(Chip::calm("ok").content_hash(), 0);
        assert_ne!(Chip::calm("").content_hash(), 0, "even an empty label is not the slow path");
    }

    #[test]
    fn a_row_with_no_room_does_not_panic() {
        testing::with_canvas(Size::new(8, 8), |c| {
            testing::with_theme(Palette::DARK, |t| {
                Chip::calm("ok").draw(c, Rect::from_xywh(0, 0, 4, 4), t);
                Chip::calm("ok").draw(c, Rect::from_xywh(0, 0, 0, 0), t);
            });
        });
    }

    #[test]
    fn a_name_beside_a_chip_keeps_its_room() {
        // The defect at the level it appeared at, and the reason this widget exists: before it, every
        // screen put the state at the end of the label and a long name pushed it off the screen.
        use crate::widgets::{ListItem, Node};
        let root = ListItem::new("gpsprobe")
            .trailing_node(Node::leaf(Chip::warn("UID clash")))
            .build();
        let (label, chip) = testing::with_theme(Palette::DARK, |theme| {
            let mut cache = crate::UiCache::with_capacity(root.slot_count());
            crate::layout::place_frame(&root, Rect { x0: 0, y0: 0, x1: 320, y1: 38 }, &mut cache, theme);
            (cache.rect(2).expect("the label was placed"), cache.rect(3).expect("the chip was placed"))
        });
        assert!(label.width() > 200, "the label got {}px of 320", label.width());
        let want = testing::with_theme(Palette::DARK, |t| Pill::warn("UID clash").width(t));
        assert_eq!(chip.width(), want, "the chip got a box that is not the one it paints");
    }
}
