//! Bitmap fonts.
//!
//! Text is the one part of a UI you cannot fake, and it is also the part Symbian
//! would happily do for us: `CFont` gives real system metrics and full UCS-2
//! coverage for free. We still define our own atlas format, for two reasons.
//! First, it lets the whole toolkit be developed and tested on the host with
//! byte-identical output. Second, it makes glyph lookup a binary search over a
//! `&[u8]` with no allocation and no Symbian handle, which is what the device
//! wants anyway.
//!
//! So `Font` is a trait with two intended implementations: [`BitmapFont`] over an
//! embedded `.sbf` atlas, and (on device) a shim-backed system font. Widgets only
//! ever see the trait.
//!
//! # The `.sbf` container
//!
//! Little-endian throughout. A 16-byte header, then a codepoint-sorted index of
//! 16-byte records, then the coverage blob.
//!
//! ```text
//! header   0  4  magic "SBF1"
//!          4  2  line_height       u16
//!          6  2  ascent            i16   baseline to top of the em box
//!          8  2  descent           i16   positive = below the baseline
//!         10  2  glyph_count       u16
//!         12  1  flags             u8    bit 0 set = 8-bit coverage
//!         13  1  fallback_advance  u8    width charged for a missing glyph
//!         14  2  reserved
//!
//! record   0  4  codepoint         u32
//!          4  4  data_offset       u32   from the start of the blob
//!          8  1  width             u8
//!          9  1  height            u8
//!         10  1  advance           u8
//!         11  1  reserved
//!         12  2  bearing_x         i16   pen to left edge of the ink
//!         14  2  bearing_y         i16   baseline up to top of the ink
//! ```
//!
//! Every offset is bounds-checked once, in [`BitmapFont::new`]. After that
//! `glyph()` slices without re-validating.

use crate::geom::Size;

pub const MAGIC: &[u8; 4] = b"SBF1";
const HEADER_LEN: usize = 16;
const RECORD_LEN: usize = 16;
/// Set when coverage is one byte per pixel. A clear bit would mean a 1bpp packed
/// bitmap, which the current writer does not emit.
pub const FLAG_AA: u8 = 1 << 0;

/// A single rendered glyph, borrowed from the atlas.
#[derive(Clone, Copy, Debug)]
pub struct Glyph<'a> {
    pub bearing_x: i32,
    pub bearing_y: i32,
    pub width: i32,
    pub height: i32,
    pub advance: i32,
    /// `width * height` bytes of coverage, row-major. Empty for glyphs with no
    /// ink of their own, such as the space.
    pub coverage: &'a [u8],
}

impl Glyph<'_> {
    #[inline]
    pub fn size(&self) -> Size {
        Size::new(self.width, self.height)
    }
}

/// The result of fitting a string into a fixed width.
#[derive(Clone, Copy, Debug)]
pub struct Fitted<'a> {
    /// The prefix that fits. Always ends on a `char` boundary.
    pub text: &'a str,
    /// Width of `text` alone, excluding any ellipsis.
    pub width: i32,
    /// True when `text` is shorter than the input and the caller should draw an
    /// ellipsis after it.
    pub ellipsized: bool,
}

pub trait Font {
    /// Distance between consecutive baselines.
    fn line_height(&self) -> i32;
    /// Baseline to the top of the em box, positive upwards.
    fn ascent(&self) -> i32;
    /// Baseline to the bottom of the em box, positive downwards.
    fn descent(&self) -> i32;
    fn glyph(&self, ch: char) -> Option<Glyph<'_>>;
    /// Width charged for a codepoint this font has no glyph for.
    fn fallback_advance(&self) -> i32;

    #[inline]
    fn advance(&self, ch: char) -> i32 {
        self.glyph(ch).map_or_else(|| self.fallback_advance(), |g| g.advance)
    }

    fn measure(&self, s: &str) -> i32 {
        s.chars().map(|c| self.advance(c)).sum()
    }

    /// The ellipsis to draw after truncated text: a real U+2026 when the font has
    /// one, otherwise three periods.
    fn ellipsis(&self) -> &'static str {
        if self.glyph('\u{2026}').is_some() {
            "\u{2026}"
        } else {
            "..."
        }
    }

    /// Longest prefix of `s` that fits in `max` pixels, leaving room for an
    /// ellipsis when truncation is needed.
    fn fit<'a>(&self, s: &'a str, max: i32) -> Fitted<'a> {
        let full = self.measure(s);
        if full <= max {
            return Fitted { text: s, width: full, ellipsized: false };
        }
        let budget = max - self.measure(self.ellipsis());
        if budget <= 0 {
            return Fitted { text: "", width: 0, ellipsized: max > 0 };
        }
        let mut width = 0;
        let mut end = 0;
        for (i, c) in s.char_indices() {
            let a = self.advance(c);
            if width + a > budget {
                end = i;
                break;
            }
            width += a;
            end = i + c.len_utf8();
        }
        Fitted { text: &s[..end], width, ellipsized: true }
    }

    /// Greedy word wrap, reporting each line through `out`.
    ///
    /// Lines are borrowed from the input so this stays allocation-free. Words too
    /// long to fit are broken mid-word rather than allowed to overflow, which
    /// matters for URLs in a chat log.
    fn wrap<'a>(&self, s: &'a str, width: i32, out: &mut dyn FnMut(&'a str)) {
        if width <= 0 {
            return;
        }
        for line in s.split('\n') {
            let mut start = 0usize;
            let mut used = 0i32;
            // Byte index and used-width at the most recent breakable point.
            let mut last_break: Option<(usize, i32)> = None;

            for (i, c) in line.char_indices() {
                if c == ' ' {
                    last_break = Some((i, used));
                }
                let a = self.advance(c);
                if used + a > width && i > start {
                    let (cut, w) = match last_break {
                        // Break at the space and swallow it.
                        Some((b, w)) if b > start => (b, w),
                        // Nothing to break on: hard-break mid-word.
                        _ => (i, used),
                    };
                    let _ = w;
                    out(&line[start..cut]);
                    start = if line.as_bytes().get(cut) == Some(&b' ') { cut + 1 } else { cut };
                    used = self.measure(&line[start..i + c.len_utf8()]);
                    last_break = None;
                } else {
                    used += a;
                }
            }
            out(&line[start..]);
        }
    }
}

/// Two atlases as one font: whatever the primary lacks comes from the fallback.
///
/// This exists for emoji, and the shape of the problem is why it is a run-time chain
/// rather than more glyphs in each atlas. Emoji have no bold — Noto Emoji ships one
/// weight — so adding them to the regular *and* the bold atlas stores two byte-identical
/// copies of every one. On a device where `.rodata` is already the largest thing in the
/// image, paying twice for the same pixels is not a rounding error.
///
/// Metrics come from the primary, always. A chained font must lay out exactly as the
/// text atlas alone does, or adding emoji support would move every line in the
/// application. The fallback contributes glyphs and nothing else — which also means its
/// bearings must have been computed against the primary's ascent when it was built; see
/// `--ascent` in `tools/mkfont.py`.
///
/// `fallback_advance` likewise comes from the primary: a codepoint neither atlas has is
/// still charged the primary's space, so the missing-glyph behaviour is unchanged.
#[derive(Clone, Copy)]
pub struct WithFallback<P, F> {
    primary: P,
    fallback: F,
}

impl<P: Font, F: Font> WithFallback<P, F> {
    pub fn new(primary: P, fallback: F) -> Self {
        Self { primary, fallback }
    }
}

impl<P: core::fmt::Debug, F: core::fmt::Debug> core::fmt::Debug for WithFallback<P, F> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("WithFallback")
            .field("primary", &self.primary)
            .field("fallback", &self.fallback)
            .finish()
    }
}

impl<P: Font, F: Font> Font for WithFallback<P, F> {
    fn line_height(&self) -> i32 {
        self.primary.line_height()
    }
    fn ascent(&self) -> i32 {
        self.primary.ascent()
    }
    fn descent(&self) -> i32 {
        self.primary.descent()
    }
    fn glyph(&self, ch: char) -> Option<Glyph<'_>> {
        match self.primary.glyph(ch) {
            Some(g) => Some(g),
            None => self.fallback.glyph(ch),
        }
    }
    fn fallback_advance(&self) -> i32 {
        self.primary.fallback_advance()
    }
}

/// Blanket impl so `&F` and `Box<F>` are usable wherever a `Font` is wanted.
impl<F: Font + ?Sized> Font for &F {
    fn line_height(&self) -> i32 {
        (**self).line_height()
    }
    fn ascent(&self) -> i32 {
        (**self).ascent()
    }
    fn descent(&self) -> i32 {
        (**self).descent()
    }
    fn glyph(&self, ch: char) -> Option<Glyph<'_>> {
        (**self).glyph(ch)
    }
    fn fallback_advance(&self) -> i32 {
        (**self).fallback_advance()
    }
}

/// A font backed by an in-memory `.sbf` atlas.
#[derive(Clone, Copy)]
pub struct BitmapFont<'a> {
    index: &'a [u8],
    blob: &'a [u8],
    count: usize,
    line_height: i32,
    ascent: i32,
    descent: i32,
    fallback_advance: i32,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum FontError {
    TooShort,
    BadMagic,
    /// The index, or a glyph's coverage, runs past the end of the data.
    Truncated,
    /// Records are not in ascending codepoint order, which would break the binary
    /// search in `glyph()`.
    Unsorted,
    /// Only 8-bit coverage is implemented.
    UnsupportedFlags(u8),
}

#[inline]
fn u16le(d: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([d[at], d[at + 1]])
}

#[inline]
fn i16le(d: &[u8], at: usize) -> i16 {
    u16le(d, at) as i16
}

#[inline]
fn u32le(d: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([d[at], d[at + 1], d[at + 2], d[at + 3]])
}

impl<'a> BitmapFont<'a> {
    /// Parse and fully validate an atlas.
    ///
    /// Validation is O(glyph_count) and happens exactly once, so the hot lookup
    /// path can slice the blob without further checks.
    pub fn new(data: &'a [u8]) -> Result<Self, FontError> {
        if data.len() < HEADER_LEN {
            return Err(FontError::TooShort);
        }
        if &data[0..4] != MAGIC {
            return Err(FontError::BadMagic);
        }
        let flags = data[12];
        if flags & FLAG_AA == 0 {
            return Err(FontError::UnsupportedFlags(flags));
        }
        let count = u16le(data, 10) as usize;
        let index_end = HEADER_LEN.checked_add(count * RECORD_LEN).ok_or(FontError::Truncated)?;
        if data.len() < index_end {
            return Err(FontError::Truncated);
        }
        let index = &data[HEADER_LEN..index_end];
        let blob = &data[index_end..];

        let mut prev: Option<u32> = None;
        for i in 0..count {
            let r = &index[i * RECORD_LEN..(i + 1) * RECORD_LEN];
            let cp = u32le(r, 0);
            if let Some(p) = prev {
                if cp <= p {
                    return Err(FontError::Unsorted);
                }
            }
            prev = Some(cp);

            let off = u32le(r, 4) as usize;
            let need = r[8] as usize * r[9] as usize;
            let end = off.checked_add(need).ok_or(FontError::Truncated)?;
            if end > blob.len() {
                return Err(FontError::Truncated);
            }
        }

        Ok(Self {
            index,
            blob,
            count,
            line_height: u16le(data, 4) as i32,
            ascent: i16le(data, 6) as i32,
            descent: i16le(data, 8) as i32,
            fallback_advance: data[13] as i32,
        })
    }

    #[inline]
    fn record(&self, i: usize) -> &'a [u8] {
        &self.index[i * RECORD_LEN..(i + 1) * RECORD_LEN]
    }

    /// Binary search by codepoint; `new` guaranteed the index is sorted.
    fn find(&self, cp: u32) -> Option<usize> {
        let (mut lo, mut hi) = (0usize, self.count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match u32le(self.record(mid), 0).cmp(&cp) {
                core::cmp::Ordering::Less => lo = mid + 1,
                core::cmp::Ordering::Greater => hi = mid,
                core::cmp::Ordering::Equal => return Some(mid),
            }
        }
        None
    }

    pub fn glyph_count(&self) -> usize {
        self.count
    }
}

/// Hand-written so a failed assertion prints metrics rather than tens of
/// kilobytes of coverage bytes.
impl core::fmt::Debug for BitmapFont<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("BitmapFont")
            .field("glyphs", &self.count)
            .field("line_height", &self.line_height)
            .field("ascent", &self.ascent)
            .field("descent", &self.descent)
            .field("blob_bytes", &self.blob.len())
            .finish()
    }
}

impl Font for BitmapFont<'_> {
    #[inline]
    fn line_height(&self) -> i32 {
        self.line_height
    }

    #[inline]
    fn ascent(&self) -> i32 {
        self.ascent
    }

    #[inline]
    fn descent(&self) -> i32 {
        self.descent
    }

    #[inline]
    fn fallback_advance(&self) -> i32 {
        self.fallback_advance
    }

    fn glyph(&self, ch: char) -> Option<Glyph<'_>> {
        let r = self.record(self.find(ch as u32)?);
        let off = u32le(r, 4) as usize;
        let w = r[8] as usize;
        let h = r[9] as usize;
        Some(Glyph {
            bearing_x: i16le(r, 12) as i32,
            bearing_y: i16le(r, 14) as i32,
            width: w as i32,
            height: h as i32,
            advance: r[10] as i32,
            coverage: &self.blob[off..off + w * h],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;
    use alloc::vec::Vec;

    /// Build a synthetic atlas: each glyph is `adv` wide with solid coverage.
    fn atlas(glyphs: &[(char, u8)]) -> Vec<u8> {
        let mut sorted: Vec<(char, u8)> = glyphs.to_vec();
        sorted.sort_by_key(|(c, _)| *c as u32);

        let mut header = Vec::new();
        header.extend_from_slice(MAGIC);
        header.extend_from_slice(&12u16.to_le_bytes()); // line_height
        header.extend_from_slice(&9i16.to_le_bytes()); // ascent
        header.extend_from_slice(&3i16.to_le_bytes()); // descent
        header.extend_from_slice(&(sorted.len() as u16).to_le_bytes());
        header.push(FLAG_AA);
        header.push(4); // fallback_advance
        header.extend_from_slice(&0u16.to_le_bytes());

        let mut index = Vec::new();
        let mut blob = Vec::new();
        for (c, adv) in &sorted {
            let (w, h) = (*adv, 8u8);
            index.extend_from_slice(&(*c as u32).to_le_bytes());
            index.extend_from_slice(&(blob.len() as u32).to_le_bytes());
            index.push(w);
            index.push(h);
            index.push(*adv);
            index.push(0);
            index.extend_from_slice(&0i16.to_le_bytes()); // bearing_x
            index.extend_from_slice(&8i16.to_le_bytes()); // bearing_y
            blob.extend(core::iter::repeat(0xFFu8).take(w as usize * h as usize));
        }

        let mut out = header;
        out.extend_from_slice(&index);
        out.extend_from_slice(&blob);
        out
    }

    fn font6() -> Vec<u8> {
        let mut g: Vec<(char, u8)> = ('a'..='z').map(|c| (c, 6u8)).collect();
        g.push((' ', 4));
        g.push(('\u{2026}', 9));
        atlas(&g)
    }

    #[test]
    fn a_fallback_supplies_only_what_the_primary_lacks() {
        // The emoji case: a text atlas with no emoji, chained to an emoji atlas with no
        // letters. Every glyph must come from exactly one of them.
        let text = font6();
        let extra = atlas(&[('\u{1F600}', 11), ('\u{2764}', 10)]);
        let primary = BitmapFont::new(&text).unwrap();
        let fallback = BitmapFont::new(&extra).unwrap();
        let chained = WithFallback::new(primary, fallback);

        assert!(chained.glyph('a').is_some(), "from the primary");
        assert!(chained.glyph('\u{1F600}').is_some(), "from the fallback");
        assert!(chained.glyph('\u{2764}').is_some());
        assert!(chained.glyph('\u{1F4A9}').is_none(), "in neither");
        // And the primary wins where both have it, so the fallback can never change the
        // look of ordinary text.
        assert_eq!(chained.advance('a'), primary.advance('a'));
    }

    #[test]
    fn a_fallback_does_not_change_the_line_box() {
        // Load-bearing: if metrics came from whichever atlas answered, adding emoji
        // support would move every line in every screen. They come from the primary,
        // always — note the two atlases here disagree on all three.
        let text = font6();
        let extra = {
            let mut d = atlas(&[('\u{1F600}', 11)]);
            d[4..6].copy_from_slice(&99u16.to_le_bytes()); // line_height
            d[6..8].copy_from_slice(&80i16.to_le_bytes()); // ascent
            d[8..10].copy_from_slice(&19i16.to_le_bytes()); // descent
            d
        };
        let primary = BitmapFont::new(&text).unwrap();
        let fallback = BitmapFont::new(&extra).unwrap();
        assert_eq!(fallback.line_height(), 99, "the fixture really does disagree");

        let chained = WithFallback::new(primary, fallback);
        assert_eq!(chained.line_height(), primary.line_height());
        assert_eq!(chained.ascent(), primary.ascent());
        assert_eq!(chained.descent(), primary.descent());
        // Including what a codepoint neither atlas has costs, so the missing-glyph
        // behaviour is exactly what it was before chaining.
        assert_eq!(chained.fallback_advance(), primary.fallback_advance());
        assert_eq!(chained.advance('\u{1F4A9}'), primary.fallback_advance());
    }

    #[test]
    fn a_chained_font_measures_and_wraps_across_both_atlases() {
        // measure/fit/wrap are trait defaults built on `advance`, so they inherit the
        // chain for free — but only if `advance` really goes through `glyph`.
        let text = font6();
        let extra = atlas(&[('\u{1F600}', 11)]);
        let primary = BitmapFont::new(&text).unwrap();
        let fallback = BitmapFont::new(&extra).unwrap();
        let chained = WithFallback::new(primary, fallback);

        // "ab" is 6+6, the emoji is 11.
        assert_eq!(chained.measure("ab"), 12);
        assert_eq!(chained.measure("ab\u{1F600}"), 23);
        // Without the chain the emoji would be charged the fallback advance instead.
        assert_ne!(chained.measure("ab\u{1F600}"), primary.measure("ab\u{1F600}"));
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(BitmapFont::new(&[]).unwrap_err(), FontError::TooShort);
        assert_eq!(BitmapFont::new(&[0; 32]).unwrap_err(), FontError::BadMagic);

        // glyph_count claiming more records than the buffer holds
        let mut d = font6();
        d[10] = 0xFF;
        d[11] = 0xFF;
        assert_eq!(BitmapFont::new(&d).unwrap_err(), FontError::Truncated);
    }

    #[test]
    fn rejects_unsorted_index() {
        let mut d = atlas(&[('a', 6), ('b', 6)]);
        // Swap the two codepoints so the index descends.
        d[HEADER_LEN..HEADER_LEN + 4].copy_from_slice(&('b' as u32).to_le_bytes());
        d[HEADER_LEN + RECORD_LEN..HEADER_LEN + RECORD_LEN + 4]
            .copy_from_slice(&('a' as u32).to_le_bytes());
        assert_eq!(BitmapFont::new(&d).unwrap_err(), FontError::Unsorted);
    }

    #[test]
    fn rejects_coverage_running_past_the_blob() {
        let mut d = atlas(&[('a', 6)]);
        // Inflate the glyph's height so width*height exceeds the blob.
        d[HEADER_LEN + 9] = 200;
        assert_eq!(BitmapFont::new(&d).unwrap_err(), FontError::Truncated);
    }

    #[test]
    fn lookup_finds_every_glyph_and_rejects_absent_ones() {
        let d = font6();
        let f = BitmapFont::new(&d).unwrap();
        for c in 'a'..='z' {
            let g = f.glyph(c).expect("present");
            assert_eq!(g.advance, 6);
            assert_eq!(g.coverage.len(), (g.width * g.height) as usize);
        }
        assert!(f.glyph('Z').is_none());
        assert_eq!(f.advance('Z'), f.fallback_advance());
    }

    #[test]
    fn measure_sums_advances() {
        let d = font6();
        let f = BitmapFont::new(&d).unwrap();
        assert_eq!(f.measure("abc"), 18);
        assert_eq!(f.measure("a b"), 16); // 6 + 4 + 6
        assert_eq!(f.measure(""), 0);
    }

    #[test]
    fn fit_returns_input_untouched_when_it_already_fits() {
        let d = font6();
        let f = BitmapFont::new(&d).unwrap();
        let r = f.fit("abc", 100);
        assert_eq!(r.text, "abc");
        assert!(!r.ellipsized);
        assert_eq!(r.width, 18);
    }

    #[test]
    fn fit_leaves_room_for_the_ellipsis() {
        let d = font6();
        let f = BitmapFont::new(&d).unwrap();
        // 5 glyphs = 30px. Cap at 21: ellipsis is 9, leaving 12px = 2 glyphs.
        let r = f.fit("abcde", 21);
        assert!(r.ellipsized);
        assert_eq!(r.text, "ab");
        assert_eq!(r.width, 12);
        assert!(r.width + f.measure(f.ellipsis()) <= 21);
    }

    #[test]
    fn fit_degrades_to_empty_rather_than_overflowing() {
        let d = font6();
        let f = BitmapFont::new(&d).unwrap();
        let r = f.fit("abcde", 3);
        assert_eq!(r.text, "");
        assert_eq!(r.width, 0);
    }

    #[test]
    fn fit_never_splits_a_multibyte_char() {
        // Two-byte chars, so a naive byte-index cut would land inside one.
        let g: Vec<(char, u8)> = "áéíóú… ".chars().map(|c| (c, 6u8)).collect();
        let d = atlas(&g);
        let f = BitmapFont::new(&d).unwrap();
        for max in 0..60 {
            let r = f.fit("áéíóú", max);
            // Slicing off a boundary would already have panicked; also assert the
            // result really is a prefix.
            assert!("áéíóú".starts_with(r.text), "max={max} gave {:?}", r.text);
        }
    }

    fn wrapped(f: &dyn Font, s: &str, w: i32) -> Vec<String> {
        let mut out = Vec::new();
        f.wrap(s, w, &mut |line| out.push(String::from(line)));
        out
    }

    #[test]
    fn wrap_breaks_on_spaces_and_swallows_them() {
        let d = font6();
        let f = BitmapFont::new(&d).unwrap();
        // "aaa bbb" = 18 + 4 + 18 = 40. At 24px only one word fits per line.
        assert_eq!(wrapped(&f, "aaa bbb", 24), ["aaa", "bbb"]);
    }

    #[test]
    fn wrap_honours_explicit_newlines() {
        let d = font6();
        let f = BitmapFont::new(&d).unwrap();
        assert_eq!(wrapped(&f, "ab\ncd", 1000), ["ab", "cd"]);
    }

    #[test]
    fn wrap_hard_breaks_a_word_too_long_to_fit() {
        let d = font6();
        let f = BitmapFont::new(&d).unwrap();
        // A single 8-glyph word (48px) in 18px must be cut, not overflow.
        let lines = wrapped(&f, "aaaaaaaa", 18);
        assert!(lines.len() > 1, "expected a hard break, got {lines:?}");
        for l in &lines {
            assert!(f.measure(l) <= 18, "line {l:?} overflows");
        }
        assert_eq!(lines.concat(), "aaaaaaaa", "hard break must not lose text");
    }

    #[test]
    fn wrap_never_loses_or_duplicates_characters() {
        let d = font6();
        let f = BitmapFont::new(&d).unwrap();
        let text = "the quick brown fox jumps over the lazy dog";
        // Spaces at a break point are swallowed, and a hard break inserts none,
        // so the invariant is over the non-space characters: every one survives,
        // in order, exactly once.
        let expect: String = text.chars().filter(|c| *c != ' ').collect();

        for w in [12, 18, 30, 60, 120] {
            let lines = wrapped(&f, text, w);
            let got: String = lines.concat().chars().filter(|c| *c != ' ').collect();
            assert_eq!(got, expect, "width {w} corrupted the text: {lines:?}");
            for l in &lines {
                assert!(f.measure(l) <= w, "width {w}: line {l:?} overflows");
            }
        }
    }

    #[test]
    fn wrap_keeps_whole_words_when_they_fit() {
        let d = font6();
        let f = BitmapFont::new(&d).unwrap();
        // 60px = 10 glyphs, so "the quick" (9 glyphs + space = 58px) stays whole.
        let lines = wrapped(&f, "the quick brown fox", 60);
        assert!(
            lines.iter().all(|l| !l.is_empty() && !l.starts_with(' ') && !l.ends_with(' ')),
            "break points should swallow their space: {lines:?}"
        );
        assert_eq!(lines.join(" "), "the quick brown fox");
    }

    #[test]
    fn wrap_with_nonpositive_width_emits_nothing() {
        let d = font6();
        let f = BitmapFont::new(&d).unwrap();
        assert!(wrapped(&f, "abc", 0).is_empty());
    }
}
