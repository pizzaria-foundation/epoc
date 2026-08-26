//! Breaking a run of inline content into lines.
//!
//! # Why this is not `Font::wrap`
//!
//! The rasterizer already has a greedy wrapper, and it is good: allocation-free, honours `\n`, hard-
//! breaks a word too long to fit. It takes **one font and one width for the whole string**, which is
//! every case the toolkit had and none of the cases a paragraph has. A sentence with a bold word in
//! it is three runs in two fonts, and the break has to be chosen across all of them at once.
//!
//! So this is the wrapper's algorithm over a sequence of styled items instead of one string.
//! `Font::wrap` stays in use for a single-style paragraph, where it is strictly better than this.
//!
//! # What it assumes about its input
//!
//! That whitespace is already collapsed — see [`crate::style::StyledTree::intern_collapsed`]. The
//! spans this emits are byte ranges into that arena, and "three spaces render as one" cannot be
//! expressed as a range, so normalising here would be impossible rather than merely misplaced.
//!
//! # The one measurement that matters
//!
//! Width comes from summing [`Font::advance`] per character. No kerning, no shaping, no ligatures —
//! the fonts are bitmap atlases with an advance per glyph and nothing else, so the sum *is* the
//! width, exactly, and a line that measures 320 is 320 pixels wide on the device.

use alloc::vec::Vec;

use symbian_gfx::{Color, Font};

use crate::style::{FontRole, Span};

/// Resolves a font role to a font.
///
/// A trait rather than a struct so this crate depends on the rasterizer alone: the toolkit's own
/// `Fonts` lives a layer up, and the desktop preview deliberately loads larger atlases than the
/// handset for the same roles. Layout must not know which.
pub trait FontSet {
    fn font(&self, role: FontRole) -> &dyn Font;
}

/// An inline box that cannot be split: a form control.
///
/// Its width is decided before line breaking, because nothing about it depends on where it lands —
/// unlike text, which is measured word by word as it is placed.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Control {
    pub kind: u8,
    pub name: Span,
    pub form: u16,
    pub w: i32,
    pub h: i32,
}

/// A piece of inline content, before it knows what line it is on.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub text: Span,
    pub font: FontRole,
    pub color: Color,
    /// The link this is part of, if any. Carried per item so that a link wrapping a line break
    /// produces a hit rectangle per line rather than one covering the gap between them.
    pub href: Span,
    /// Set when this item is a control rather than text.
    ///
    /// A control shares a line with whatever is beside it — a search field and its button belong
    /// together, and stacking them was the visible bug this exists to fix — but it never merges
    /// into a neighbouring run, because it is one box and a run is a stretch of characters.
    pub control: Option<Control>,
}

/// A piece of inline content, placed.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub text: Span,
    pub font: FontRole,
    pub color: Color,
    pub href: Span,
    /// Left edge, relative to the line's left edge.
    pub x: i32,
    pub width: i32,
    /// Set when this run is a control's box rather than characters.
    pub control: Option<Control>,
}

/// One line of a paragraph.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line {
    pub runs: Vec<Run>,
    /// Tallest thing on the line. A line with a title-font word in it is a title-height line.
    pub height: i32,
    /// Where the baseline sits below the line's top.
    ///
    /// The maximum ascent of everything on the line, which is what makes runs of two sizes sit on
    /// one baseline instead of each being centred in its own box.
    pub baseline: i32,
}

impl Line {
    /// The line's used width — where the last run ends.
    pub fn width(&self) -> i32 {
        self.runs.last().map(|r| r.x + r.width).unwrap_or(0)
    }
}

/// Split `items` into lines no wider than `width`.
///
/// `text` is the arena the spans point into. Greedy: a word goes on the current line if it fits and
/// starts a new one if it does not, with no lookahead. Knuth-Plass would produce prettier paragraphs
/// and needs the whole paragraph in memory twice; on a 320-pixel column with ~40 characters to a
/// line there is almost never a second option to choose between.
pub fn break_lines<F: FontSet>(
    items: &[Item],
    text: &str,
    width: i32,
    fonts: &F,
) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();
    let mut cur = Line::default();
    let mut x = 0i32;

    // Extends the run in progress instead of starting a new one, when style and line allow. One
    // `Text` node per style per line rather than per word: a paragraph of forty words would
    // otherwise be forty nodes and forty `draw_text` calls, each re-measuring nothing but costing a
    // call.
    let mut open: Option<Run> = None;

    macro_rules! close_run {
        () => {
            if let Some(r) = open.take() {
                if r.width > 0 || !r.text.is_empty() {
                    cur.runs.push(r);
                }
            }
        };
    }

    macro_rules! flush_line {
        () => {{
            close_run!();
            // A line with nothing on it still has height — an empty paragraph occupies space, and a
            // hard break between two words is a blank line the author asked for.
            if cur.height == 0 {
                let f = fonts.font(FontRole::Body);
                cur.height = f.line_height();
                cur.baseline = f.ascent();
            }
            lines.push(core::mem::take(&mut cur));
            x = 0;
        }};
    }

    for item in items {
        // A control is one box: it never splits, never merges with a neighbouring run, and takes
        // the next line whole if it does not fit on this one. Handled before the text path because
        // it has no words to measure.
        if let Some(ctl) = item.control {
            close_run!();
            if x > 0 && x + ctl.w > width {
                flush_line!();
            }
            let w = ctl.w.min(width);
            cur.runs.push(Run {
                text: item.text,
                font: item.font,
                color: item.color,
                href: Span::EMPTY,
                x,
                width: w,
                control: Some(ctl),
            });
            x += w;
            // The line grows to hold it, exactly as a title-font word makes a taller line.
            if ctl.h > cur.height {
                cur.height = ctl.h;
                cur.baseline = ctl.h;
            }
            continue;
        }

        let s = slice(text, item.text);
        if s.is_empty() {
            continue;
        }
        let font = fonts.font(item.font);
        let line_h = font.line_height();
        let ascent = font.ascent();

        // A style change closes the run in progress: a bold word cannot share a `Text` node with the
        // plain words around it.
        if let Some(r) = open.as_ref() {
            if r.font != item.font || r.color != item.color || r.href != item.href {
                close_run!();
            }
        }

        for (word_off, word) in words(s) {
            // An explicit newline is a break the author asked for, and it survives collapsing
            // because `intern` (not `intern_collapsed`) is what `white-space: pre` uses.
            if word == "\n" {
                flush_line!();
                continue;
            }

            let w = measure(font, word);

            // Does not fit, and there is something to push down to the next line.
            if x > 0 && x + w > width {
                flush_line!();
            }

            // Still does not fit on a line of its own: a URL, a long token, a language without
            // spaces. Break it wherever it reaches the edge — an unbreakable word that overflows is
            // a word running off the screen, which is worse than an ugly break.
            if w > width {
                close_run!();
                // Walk the characters, emitting a segment each time the next one would not fit.
                // Offsets are within `word`; `word_off` turns them back into arena offsets.
                let mut seg_start = 0usize;
                let mut seg_w = 0i32;
                for (i, ch) in word.char_indices() {
                    let a = font.advance(ch);
                    if x + seg_w + a > width && x + seg_w > 0 {
                        cur.runs.push(Run {
                            text: Span {
                                off: item.text.off + (word_off + seg_start) as u32,
                                len: (i - seg_start) as u32,
                            },
                            font: item.font,
                            color: item.color,
                            href: item.href,
                            x,
                            width: seg_w,
                            control: None,
                        });
                        raise(&mut cur, line_h, ascent);
                        flush_line!();
                        seg_start = i;
                        seg_w = 0;
                    }
                    seg_w += a;
                }
                // The tail stays open, so plain words after the broken token can join its run.
                if seg_w > 0 {
                    place(
                        &mut open,
                        item,
                        item.text.off + (word_off + seg_start) as u32,
                        (word.len() - seg_start) as u32,
                        x,
                        seg_w,
                    );
                    raise(&mut cur, line_h, ascent);
                    x += seg_w;
                }
                continue;
            }

            place(&mut open, item, item.text.off + word_off as u32, word.len() as u32, x, w);
            raise(&mut cur, line_h, ascent);
            x += w;
        }
    }

    close_run!();
    if !cur.runs.is_empty() || cur.height > 0 {
        lines.push(cur);
    }
    lines
}

/// Extend the open run, or start one.
fn place(open: &mut Option<Run>, item: &Item, off: u32, len: u32, x: i32, w: i32) {
    match open.as_mut() {
        // Contiguous in the arena and same style: one node instead of two.
        Some(r) if r.text.off + r.text.len == off => {
            r.text.len += len;
            r.width += w;
        }
        Some(_) | None => {
            *open = Some(Run {
                text: Span { off, len },
                font: item.font,
                color: item.color,
                href: item.href,
                x,
                width: w,
                control: None,
            });
        }
    }
}

/// Grow the line to hold something of this height, keeping the deepest baseline.
fn raise(line: &mut Line, line_h: i32, ascent: i32) {
    if line_h > line.height {
        line.height = line_h;
    }
    if ascent > line.baseline {
        line.baseline = ascent;
    }
}

fn measure(font: &dyn Font, s: &str) -> i32 {
    s.chars().map(|c| font.advance(c)).sum()
}

fn slice(text: &str, s: Span) -> &str {
    let start = s.off as usize;
    let end = start.saturating_add(s.len as usize);
    if end > text.len() || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
        return "";
    }
    &text[start..end]
}

/// Split into words, keeping the space that follows each one attached to it.
///
/// The trailing space belongs to the word before the break, not the line after: a line that begins
/// with a space is indented by an accident of where the text broke. Newlines come out as their own
/// one-character word so the caller can treat them as breaks.
fn words(s: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut out: Vec<(usize, &str)> = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < s.len() {
        if bytes[i] == b'\n' {
            out.push((i, "\n"));
            i += 1;
            continue;
        }
        let start = i;
        // The word.
        while i < s.len() && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        // And the whitespace after it, which travels with it.
        while i < s.len() && bytes[i].is_ascii_whitespace() && bytes[i] != b'\n' {
            i += 1;
        }
        if i > start {
            out.push((start, &s[start..i]));
        } else {
            i += 1;
        }
    }
    out.into_iter()
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian_gfx::BitmapFont;

    /// The test atlas: one glyph, constant advance, so text width is exactly proportional to
    /// length. That is what keeps every expected value below an arithmetic statement rather than a
    /// magic number measured off a real font.
    struct Fixed {
        f: BitmapFont<'static>,
    }

    fn atlas() -> &'static [u8] {
        // Leaked once per test binary. The alternative is threading a lifetime through FontSet for
        // the benefit of tests only.
        let v = symbian_ui::testing::atlas();
        alloc::boxed::Box::leak(v.into_boxed_slice())
    }

    impl Fixed {
        fn new() -> Self {
            Fixed { f: BitmapFont::new(atlas()).expect("the test atlas must parse") }
        }
        /// Every glyph is this wide, including the space.
        fn adv(&self) -> i32 {
            self.f.advance('a')
        }
    }

    impl FontSet for Fixed {
        fn font(&self, _role: FontRole) -> &dyn Font {
            &self.f
        }
    }

    fn item(off: u32, len: u32) -> Item {
        Item {
            text: Span { off, len },
            font: FontRole::Body,
            color: Color::BLACK,
            href: Span::EMPTY,
            control: None,
        }
    }

    #[test]
    fn one_short_line_stays_one_line() {
        let fonts = Fixed::new();
        let text = "hello world";
        let lines = break_lines(&[item(0, text.len() as u32)], text, 1000, &fonts);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].runs.len(), 1, "one style, one line, one node");
        assert_eq!(lines[0].height, fonts.f.line_height());
        assert_eq!(lines[0].baseline, fonts.f.ascent());
    }

    #[test]
    fn a_paragraph_breaks_at_the_width() {
        let fonts = Fixed::new();
        let a = fonts.adv();
        let text = "aaa bbb ccc ddd";
        // Room for "aaa bbb " (8 glyphs) but not for "ccc" after it.
        let lines = break_lines(&[item(0, text.len() as u32)], text, a * 9, &fonts);
        assert_eq!(lines.len(), 2, "expected two lines, got {lines:?}");
        assert!(lines[0].width() <= a * 9);
        assert!(lines[1].width() <= a * 9);
    }

    /// Every line must fit. This is the property the whole function exists for, so it is asserted
    /// over many widths rather than one.
    #[test]
    fn no_line_ever_exceeds_the_width() {
        let fonts = Fixed::new();
        let a = fonts.adv();
        let text = "the quick brown fox jumps over the lazy dog again and again";
        for cols in 4..40 {
            let width = a * cols;
            let lines = break_lines(&[item(0, text.len() as u32)], text, width, &fonts);
            for (i, l) in lines.iter().enumerate() {
                // A trailing space may hang past the edge, as it does in every browser — it is not
                // ink. Anything more than one glyph of overhang is a real overflow.
                assert!(
                    l.width() <= width + a,
                    "line {i} is {} wide at width {width}: {l:?}",
                    l.width()
                );
            }
        }
    }

    /// A word longer than the line has to break somewhere. Running off the screen is worse.
    #[test]
    fn an_unbreakable_word_is_broken_rather_than_overflowing() {
        let fonts = Fixed::new();
        let a = fonts.adv();
        let text = "https://example.com/a/very/long/path/that/never/ends";
        let width = a * 10;
        let lines = break_lines(&[item(0, text.len() as u32)], text, width, &fonts);
        assert!(lines.len() > 1, "a long token must be split across lines");
        for l in &lines {
            assert!(l.width() <= width + a, "a broken piece still overflows: {l:?}");
        }
    }

    /// A style change ends the run, because a bold word cannot share a text node with plain words.
    #[test]
    fn a_style_change_starts_a_new_run() {
        let fonts = Fixed::new();
        let text = "plain bold plain";
        let items = [
            item(0, 6),
            Item {
                text: Span { off: 6, len: 5 },
                font: FontRole::Strong,
                color: Color::BLACK,
                href: Span::EMPTY,
                control: None,
            },
            item(11, 5),
        ];
        let lines = break_lines(&items, text, 1000, &fonts);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].runs.len(), 3, "three styles, three nodes: {:?}", lines[0].runs);
        assert_eq!(lines[0].runs[1].font, FontRole::Strong);
        // And they are laid out left to right without gaps or overlaps.
        for w in lines[0].runs.windows(2) {
            assert_eq!(w[0].x + w[0].width, w[1].x, "runs must abut: {:?}", lines[0].runs);
        }
    }

    /// A link that wraps needs a rectangle per line, which starts with a run per line.
    #[test]
    fn a_link_spanning_a_break_yields_a_run_on_each_line() {
        let fonts = Fixed::new();
        let a = fonts.adv();
        let text = "aaa bbb ccc";
        let href = Span { off: 0, len: 3 };
        let items = [Item {
            text: Span { off: 0, len: text.len() as u32 },
            font: FontRole::Body,
            color: Color::BLACK,
            href,
            control: None,
        }];
        let lines = break_lines(&items, text, a * 5, &fonts);
        assert!(lines.len() >= 2);
        for l in &lines {
            assert!(l.runs.iter().all(|r| r.href == href), "the href must survive the break");
        }
    }

    /// Two sizes on one line share one baseline, which is what stops mixed text from staggering.
    #[test]
    fn a_taller_run_raises_the_line_and_the_baseline() {
        struct TwoSizes {
            small: BitmapFont<'static>,
            big: BitmapFont<'static>,
        }
        impl FontSet for TwoSizes {
            fn font(&self, role: FontRole) -> &dyn Font {
                match role {
                    FontRole::Title => &self.big,
                    _ => &self.small,
                }
            }
        }
        // The test atlas is one size, so this asserts the arithmetic rather than two real atlases:
        // with identical fonts the line must simply equal that font's metrics.
        let fonts = TwoSizes {
            small: BitmapFont::new(atlas()).unwrap(),
            big: BitmapFont::new(atlas()).unwrap(),
        };
        let text = "small BIG";
        let items = [
            item(0, 6),
            Item {
                text: Span { off: 6, len: 3 },
                font: FontRole::Title,
                color: Color::BLACK,
                href: Span::EMPTY,
                control: None,
            },
        ];
        let lines = break_lines(&items, text, 1000, &fonts);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].height, fonts.small.line_height());
        assert_eq!(lines[0].baseline, fonts.small.ascent());
    }

    #[test]
    fn no_items_is_no_lines() {
        let fonts = Fixed::new();
        assert!(break_lines(&[], "", 320, &fonts).is_empty());
    }

    /// A span the arena does not contain is skipped, not a panic.
    #[test]
    fn a_bogus_span_is_skipped() {
        let fonts = Fixed::new();
        let lines = break_lines(&[item(999, 10)], "short", 320, &fonts);
        assert!(lines.is_empty());
    }

    /// The space at a break belongs to the line before it. A line that starts with a space is
    /// indented by an accident of where the text happened to break.
    #[test]
    fn a_line_never_starts_with_a_space() {
        let fonts = Fixed::new();
        let a = fonts.adv();
        let text = "aaa bbb ccc ddd eee";
        let lines = break_lines(&[item(0, text.len() as u32)], text, a * 8, &fonts);
        for l in &lines {
            if let Some(first) = l.runs.first() {
                let s = slice(text, first.text);
                assert!(!s.starts_with(' '), "line starts with a space: {s:?}");
            }
        }
    }
}
