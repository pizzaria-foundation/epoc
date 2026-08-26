//! What a list says when it has nothing in it.
//!
//! ```ignore
//! if self.chats.is_empty() {
//!     Node::leaf(EmptyState::new("No conversations yet").fill(1))
//! } else {
//!     Node::Group(ScrollList::new(..))
//! }
//! ```
//!
//! # An empty list must say something
//!
//! A screen with a title bar, two softkeys and nothing between them is indistinguishable from a
//! screen that failed to load — and on this handset, where a list is often waiting on a daemon that
//! polls with backoff, the two happen at the same moment. The centred line is the difference between
//! "there is nothing here" and "something is broken", and a person who cannot tell them apart
//! reboots the phone.
//!
//! # This is `chrome::placeholder` and nothing else
//!
//! [`symbian_ui::chrome::placeholder`] already centres a dim line of body text in an area, with the
//! toolkit's side margin, and every hand-written screen in the SDK uses it. This widget adds a box
//! around it so a declared screen can put it in a column beside other things instead of reaching for
//! the raw canvas. Reimplementing the centring here would be a second empty state that agrees with
//! the first until one of them is touched.
//!
//! # It is dim, and that is the message
//!
//! The line is drawn in `palette.dim` because it is not content — it is the absence of content
//! described. A placeholder set in the ordinary text colour reads as the first row of a list, which
//! is exactly the reading it exists to prevent.
//!
//! # Why it does not take the line by default
//!
//! Because it usually should, and "usually" is not "always" — and a widget that answers a measure
//! with the whole offer is the [`Slider`](super::Slider) defect: the layout's first pass offers
//! *every* fixed child the whole line, so a greedy fixed child leaves nothing for the flexible ones.
//! The intrinsic box here is the sentence plus the toolkit's margins; `.fill(1)` is how a screen says
//! "you are the content area", which is what centres the line horizontally as well as vertically.

use alloc::string::String;

use symbian_gfx::{Canvas, Rect, Size};
use symbian_ui::{chrome, Theme};

use crate::constraints::Constraints;
use crate::spacing::Gap;
use crate::widget::{hash_i32, hash_str, Widget, WidgetHash};

/// A centred, de-emphasised line for a list with nothing in it.
pub struct EmptyState {
    text: String,
    /// The side margin the sentence is inset by, on each edge.
    ///
    /// A [`Gap`] role rather than a number, for the reason [`crate::spacing`] exists: a `view` is
    /// built without a theme, so a margin written as `6` agrees with the toolkit until the toolkit
    /// moves.
    ///
    /// [`Gap::Base`] — a list row's side margin — rather than `metrics.pad`, which is the number
    /// `chrome::placeholder` actually insets by and is one pixel smaller. The role is the honest
    /// thing to write in a `view`, and it must be the *larger* of the two or the reservation would be
    /// short: `measure` promises the box the sentence fits in and `placeholder` spends the margin
    /// inside it, so a role that resolved below `metrics.pad` would hand the draw a line box narrower
    /// than the text and the sentence would come out clipped at both ends — which reads as a font
    /// fault rather than as a layout one. A test below pins that inequality.
    pad: Gap,
    /// This widget's share of its parent's leftover space. `0` is the sentence's own box; `1` is "I
    /// am the content area", which is what a screen usually wants.
    fill: i32,
}

impl EmptyState {
    /// The message. A sentence, not a word: "No conversations yet" tells a person the list is
    /// working, and "Empty" tells them a programmer ran out of screen.
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into(), pad: Gap::Base, fill: 0 }
    }

    /// Take a share of the parent's leftover space — `fill(1)` for the content area of a screen.
    ///
    /// Without it the box is the sentence's own, and where that box lands is the enclosing column's
    /// business. With it the sentence is centred in whatever is left after the bars, which is what
    /// "an empty screen" means. See the module docs for why this is opt-in.
    pub fn fill(mut self, weight: i32) -> Self {
        self.fill = weight.max(0);
        self
    }

    /// The margin the sentence keeps from each edge. [`Gap::Base`] unless told otherwise.
    pub fn pad(mut self, pad: impl Into<Gap>) -> Self {
        self.pad = pad.into();
        self
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

impl Widget for EmptyState {
    fn content_hash(&self) -> WidgetHash {
        // The text, because the intrinsic width is measured from it. The padding *role*, not its
        // pixels — resolving it needs a theme, which `content_hash` does not have and should not,
        // since a digest is about the description. `crate::spacing::Gap::hash` records that rule.
        //
        // `fill` is in it because it changes the box: filled answers the offer, plain answers the
        // sentence. Never zero — `hash_str` is seeded, so even an empty message digests to something,
        // and zero would mean "re-measure me every frame".
        let h = self.pad.hash(hash_str(0, &self.text));
        hash_i32(h, self.fill)
    }

    fn measure(&self, constraints: Constraints, theme: &Theme<'_>) -> Size {
        let f = theme.fonts.body;
        let pad = self.pad.resolve(theme);
        // The sentence plus the margin `chrome::placeholder` will inset it by, so a plain (unfilled)
        // empty state is a box the text actually fits in. Measuring the bare text width and then
        // drawing with a margin inside it is how a centred line comes out clipped at both ends.
        let w = if self.fill > 0 { constraints.max_w } else { f.measure(&self.text) + pad * 2 };
        // One line's height. `draw` centres inside whatever it is handed, so a taller rect is not a
        // taller box — it is the same sentence in the middle of more room.
        constraints.constrain(Size::new(w, f.line_height()))
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        // `chrome::placeholder` does the vertical centring itself, which is what carries this through
        // the `CrossAlign::Stretch` a column applies by default: handed a 200-pixel content area it
        // puts the line in the middle of it, and handed exactly one line's height it puts the line
        // there. One routine for both, so a declared empty screen and a hand-written one cannot
        // disagree about where the sentence sits.
        chrome::placeholder(c, rect, theme, &self.text);
    }

    fn flex_weight(&self) -> i32 {
        self.fill
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian_ui::{testing, Palette};

    const PANEL: Rect = Rect { x0: 0, y0: 0, x1: 160, y1: 100 };

    /// A canvas with the sentence drawn on it, and the same canvas with nothing — the negative
    /// control every pixel assertion here needs, since the test atlas has one glyph.
    fn paint_in(palette: Palette, w: &EmptyState, rect: Rect) -> Vec<u16> {
        let (_, buf) = testing::with_canvas(Size::new(160, 100), |c| {
            testing::with_theme(palette, |t| {
                c.clear(palette.bg.mid());
                w.draw(c, rect, t);
            });
        });
        buf
    }

    /// Which rows have ink on them. The atlas `with_theme` loads has exactly one glyph — lowercase
    /// `a` — so every message in these tests is spelled out of it. A sentence without an `a` paints
    /// nothing at all and every assertion below would pass vacuously.
    fn inked_rows(buf: &[u16], palette: Palette) -> Vec<usize> {
        let bg = palette.bg.mid().to_rgb565().0;
        (0..100).filter(|&y| (0..160).any(|x| buf[y * 160 + x] != bg)).collect()
    }

    #[test]
    fn a_plain_empty_state_is_the_sentence_and_its_margins_not_the_whole_line() {
        // The `Slider` defect, which this would have repeated for free: the layout's first pass
        // offers every fixed child the whole line, so an empty state that answered with the offer
        // would take a sibling's room with it.
        testing::with_theme(Palette::DARK, |t| {
            let got = EmptyState::new("aaa").measure(Constraints::loose(320, 200), t);
            let pad = Gap::Base.resolve(t);
            assert_eq!(got, Size::new(t.fonts.body.measure("aaa") + pad * 2, t.fonts.body.line_height()));
            assert!(got.w < 320, "it took the whole line");
            // A longer sentence is a wider box; the height is one line either way.
            let long = EmptyState::new("aaaaaaaaaaaaaa").measure(Constraints::loose(320, 200), t);
            assert!(long.w > got.w);
            assert_eq!(long.h, got.h);
        });
    }

    #[test]
    fn the_intrinsic_box_has_room_for_the_margin_the_draw_will_inset_by() {
        // `chrome::placeholder` insets by `metrics.pad` on both sides. A box measured as the bare text
        // width would hand it `width - pad*2` to centre in — a sentence clipped at both ends, which
        // looks like a font bug rather than a layout one.
        testing::with_theme(Palette::DARK, |t| {
            let text = "aaaaaa";
            let got = EmptyState::new(text).measure(Constraints::loose(320, 200), t);
            assert!(
                Gap::Base.resolve(t) >= t.metrics.pad,
                "the reserved margin is smaller than the one the draw spends"
            );
            assert!(
                got.w - t.metrics.pad * 2 >= t.fonts.body.measure(text),
                "the sentence does not fit inside its own box"
            );
            // The negative control: a role that resolves to nothing does *not* reserve the margin, so
            // the assertion above is testing the reservation rather than a tautology of the arithmetic.
            let none = EmptyState::new(text).pad(Gap::None).measure(Constraints::loose(320, 200), t);
            assert!(none.w - t.metrics.pad * 2 < t.fonts.body.measure(text));
        });
    }

    #[test]
    fn a_filled_empty_state_takes_the_content_area() {
        testing::with_theme(Palette::DARK, |t| {
            let got = EmptyState::new("aaa").fill(1).measure(Constraints::loose(320, 200), t);
            assert_eq!(got.w, 320);
            assert_eq!(got.h, t.fonts.body.line_height(), "still one line tall; `draw` centres it");
            assert_eq!(EmptyState::new("aaa").fill(1).flex_weight(), 1);
            assert_eq!(EmptyState::new("aaa").flex_weight(), 0);
            assert_eq!(EmptyState::new("aaa").fill(-2).flex_weight(), 0, "a negative share is none");
        });
    }

    #[test]
    fn it_never_answers_wider_than_the_offer() {
        testing::with_theme(Palette::DARK, |t| {
            let long = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
            assert_eq!(EmptyState::new(long).measure(Constraints::loose(40, 40), t).w, 40);
            assert_eq!(EmptyState::new(long).measure(Constraints::loose(0, 0), t), Size::new(0, 0));
        });
    }

    #[test]
    fn a_taller_rect_centres_the_sentence_instead_of_stretching_it() {
        // The trap every widget in this catalogue shares, arriving here as the ordinary case: an
        // empty state is normally handed a whole 200-pixel content area, not the one line it measured.
        let one_line = testing::with_theme(Palette::DARK, |t| t.fonts.body.line_height());
        let tall = inked_rows(&paint_in(Palette::DARK, &EmptyState::new("aaa"), PANEL), Palette::DARK);
        assert!(!tall.is_empty(), "nothing painted — the atlas has one glyph and it was not used");
        assert!(
            (tall.len() as i32) <= one_line,
            "the sentence grew with its rect: {} rows for a {one_line}-pixel line",
            tall.len()
        );
        let mid = 100 / 2;
        assert!(
            tall[0] < mid && *tall.last().unwrap() >= mid - one_line as usize,
            "the sentence is not near the middle of the panel: {tall:?}"
        );
        // The negative control that makes the bound above mean something: given only one line's
        // height, the same sentence sits at the top.
        let short = inked_rows(
            &paint_in(Palette::DARK, &EmptyState::new("aaa"), Rect::from_xywh(0, 0, 160, one_line)),
            Palette::DARK,
        );
        assert!(!short.is_empty());
        assert!(short[0] < tall[0], "the sentence did not move when the rect grew: {short:?} {tall:?}");
    }

    #[test]
    fn the_sentence_is_visible_and_de_emphasised_in_every_palette() {
        // De-emphasised because a placeholder set in the ordinary text colour reads as the first row
        // of a list, which is the reading it exists to prevent. Asserted as "not the background and
        // not the text colour" rather than as a pixel count keyed to `dim` — a count keyed to one
        // named colour is measuring an answer rather than the property, and two such tests in this
        // repo went red the moment the answer changed for a good reason.
        for (name, palette) in Palette::ALL {
            let buf = paint_in(palette, &EmptyState::new("aaa"), PANEL);
            let bg = palette.bg.mid().to_rgb565().0;
            let text = palette.text.to_rgb565().0;
            let ink: Vec<u16> = buf.iter().copied().filter(|&p| p != bg).collect();
            assert!(!ink.is_empty(), "{name}: the sentence painted nothing");
            // De-emphasised *where the palette has a de-emphasis to offer*. On `HIGH_CONTRAST`,
            // `dim` and `text` are the same white — the same fact `chrome::control_colors` records
            // for the switch that became a black dot on a white band — so the placeholder there is
            // set in the reading colour and cannot be otherwise. Asserting it unconditionally is
            // asserting a property of four palettes and calling it five, which is how a sweep that
            // exists to catch the odd palette ends up excluding it.
            if palette.dim.to_rgb565().0 != text {
                assert!(
                    ink.iter().all(|&p| p != text),
                    "{name}: the placeholder is set in the reading colour"
                );
            }
            // The negative control: an empty state with no message paints nothing, so the "not
            // empty" check above is not passing on the clear alone.
            let blank = paint_in(palette, &EmptyState::new(""), PANEL);
            assert!(
                blank.iter().all(|&p| p == bg),
                "{name}: something was painted with no message to paint"
            );
        }
    }

    #[test]
    fn the_message_and_the_fill_are_in_the_digest_and_it_is_never_zero() {
        assert_ne!(EmptyState::new("aaa").content_hash(), EmptyState::new("bbb").content_hash());
        assert_ne!(
            EmptyState::new("aaa").content_hash(),
            EmptyState::new("aaa").fill(1).content_hash(),
            "filled and plain are two different boxes"
        );
        assert_ne!(
            EmptyState::new("aaa").content_hash(),
            EmptyState::new("aaa").pad(Gap::Wide).content_hash(),
            "the margin is part of the intrinsic width"
        );
        assert_ne!(EmptyState::new("aaa").content_hash(), 0);
        assert_ne!(EmptyState::new("").content_hash(), 0, "even no message is not the slow path");
    }

    #[test]
    fn a_degenerate_rect_is_a_no_op_rather_than_a_panic() {
        testing::with_canvas(Size::new(10, 10), |c| {
            testing::with_theme(Palette::DARK, |t| {
                EmptyState::new("aaa").draw(c, Rect::from_xywh(0, 0, 0, 0), t);
                EmptyState::new("aaa").draw(c, Rect::from_xywh(0, 0, 2, 2), t);
            });
        });
    }

    #[test]
    fn a_filled_empty_state_is_handed_the_screen_it_asked_for() {
        // End to end, through the layout the screen actually uses: the empty state flexes and the
        // rect it gets back is the content area rather than a sentence-shaped box at the top.
        use crate::widgets::{Column, Node};
        let root = Node::Group(Column::new().child(EmptyState::new("aaa").fill(1)));
        let got = testing::with_theme(Palette::DARK, |theme| {
            let mut cache = crate::UiCache::with_capacity(root.slot_count());
            crate::layout::place_frame(&root, PANEL, &mut cache, theme);
            cache.rect(1).expect("the empty state was placed")
        });
        assert_eq!(got.width(), 160, "it did not get the width");
        assert_eq!(got.height(), 100, "it did not get the content area's height: {got:?}");
    }
}
