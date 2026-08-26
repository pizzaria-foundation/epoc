//! Colours, metrics and fonts, in one place.
//!
//! # What is a [`Surface`] and what is a [`Color`]
//!
//! Anything the era would have drawn as a *band* — chrome, the selection highlight,
//! a bubble — is a `Surface`, so it carries a gradient and its two edge lines. Text
//! and hairlines are plain `Color`, because a one-pixel gradient is not a thing.
//!
//! That split is the whole reason a theme can change the look and not just the hue.
//! See [`crate::tokens`] for why the band shape matters, and [`crate::paint`] for
//! how it is drawn.
//!
//! # Metrics
//!
//! Absolute pixels, not scaled units. A 320x240 screen is the only target, there is
//! no DPI to adapt to, and pretending otherwise would add arithmetic that never
//! pays for itself.

use symbian_gfx::{Color, Font};

use crate::tokens::{darken, lighten, luma, readable_on, Space, Surface};

/// Vertical sizes, in pixels. Chosen against 320x240: an 18px title plus a 17px
/// softkey bar leaves 205px of content, which is five 38px list rows and change.
#[derive(Copy, Clone, Debug)]
pub struct Metrics {
    pub title_h: i32,
    pub softkey_h: i32,
    /// Uniform height of a dialog-style list row.
    pub row_h: i32,
    /// Side padding for content that is not a list row.
    pub pad: i32,
    /// Corner radius for bubbles and buttons.
    pub radius: i32,
    /// Width of the scrollbar gutter. Zero hides it.
    pub scrollbar_w: i32,
    pub focus_ring: i32,
    /// Nominal size of an inline status icon — delivery ticks, the mute marker.
    /// Odd, because [`crate::icon`] snaps to odd and would otherwise shrink by one.
    pub icon_sm: i32,
    /// Nominal size of a leading icon in a list row.
    pub icon_md: i32,
    /// The spacing scale.
    pub space: Space,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            title_h: 18,
            softkey_h: 17,
            row_h: 38,
            pad: 5,
            radius: 6,
            scrollbar_w: 4,
            focus_ring: 1,
            icon_sm: 9,
            icon_md: 11,
            space: Space::default(),
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Palette {
    /// The page. Usually flat — the era put gradients on furniture, not on paper.
    pub bg: Surface,
    /// Title and softkey bars.
    pub chrome: Surface,
    pub chrome_text: Color,
    /// Hairline between rows and around chrome.
    pub divider: Color,
    pub text: Color,
    /// De-emphasised text: timestamps, previews, hints.
    pub dim: Color,
    pub accent: Color,
    pub accent_text: Color,
    /// Fill behind the selected list row.
    pub selection: Surface,
    pub selection_text: Color,
    /// Incoming chat bubble.
    pub bubble_in: Surface,
    pub bubble_in_text: Color,
    /// Outgoing chat bubble.
    pub bubble_out: Surface,
    pub bubble_out_text: Color,
    pub unread: Color,
    pub unread_text: Color,
    /// Something that wants a look before it is acted on.
    ///
    /// Added when `chip::Tone` needed four distinguishable states and found that `accent` and
    /// `unread` are **the same colour** in the dark palette — so "on offer" and "be careful" would
    /// have painted identically and the distinction would have existed only in the source. A design
    /// system with an accent and no caution colour is missing one.
    ///
    /// Amber with dark text in both palettes: it reads on a TN panel in sunlight, and it is nobody
    /// else's colour here.
    pub warn: Color,
    pub warn_text: Color,
    /// Something went wrong, as text on the page.
    ///
    /// Authored per palette rather than derived from `warn`, for the same reason `warn` is authored
    /// rather than derived from `accent`: a red is not an amber turned down, and the one thing this
    /// colour must not be is *nearly* the warning colour.
    ///
    /// There is no `error_text`, and the absence is the design: this is an ink, not a band. An error
    /// on this handset is a line under a field — see `FieldRow` — and a filled red slab across a
    /// 240-pixel screen is the era's dialog, not its form.
    ///
    /// Note for anyone picking one: [`crate::tokens::luma`] weights green at 183/256, so pure red
    /// has a luma of 53. On a dark page a saturated red clears the contrast floor only barely and on
    /// a light one not at all — which is why the dark palettes here carry a *pink* and the light ones
    /// a deep brick.
    pub error: Color,
    pub scrollbar: Color,
    pub scrollbar_track: Color,
}

impl Palette {
    /// Dark, cool-neutral, with a Telegram-ish blue accent. Reads well on the E72's
    /// TN panel, where a light theme at this size tends to glare.
    pub const DARK: Self = Self {
        bg: Surface::flat(Color::hex(0x0B0F14)),
        chrome: Surface {
            top: Color::hex(0x223040),
            bottom: Color::hex(0x141C26),
            edge_light: Color::hex(0x35485C),
            edge_dark: Color::hex(0x080C10),
        },
        chrome_text: Color::hex(0xE8EDF2),
        divider: Color::hex(0x1F2A36),
        text: Color::hex(0xE2E8EE),
        dim: Color::hex(0x8695A3),
        accent: Color::hex(0x2E8FE0),
        accent_text: Color::hex(0xFFFFFF),
        selection: Surface {
            top: Color::hex(0x2E76B4),
            bottom: Color::hex(0x184A78),
            edge_light: Color::hex(0x5AA0DC),
            edge_dark: Color::hex(0x0E3252),
        },
        selection_text: Color::hex(0xFFFFFF),
        bubble_in: Surface::gradient(Color::hex(0x31414F), Color::hex(0x27333E)),
        bubble_in_text: Color::hex(0xE2E8EE),
        bubble_out: Surface::gradient(Color::hex(0x357CC0), Color::hex(0x28618F)),
        bubble_out_text: Color::hex(0xFFFFFF),
        unread: Color::hex(0x2E8FE0),
        unread_text: Color::hex(0xFFFFFF),
        warn: Color::hex(0xE8A33D),
        warn_text: Color::hex(0x1A2530),
        error: Color::hex(0xFF6B6B),
        scrollbar: Color::hex(0x46586A),
        scrollbar_track: Color::hex(0x18202A),
    };

    pub const LIGHT: Self = Self {
        bg: Surface::flat(Color::hex(0xFFFFFF)),
        chrome: Surface {
            top: Color::hex(0xF4F7FA),
            bottom: Color::hex(0xDCE4EC),
            edge_light: Color::hex(0xFFFFFF),
            edge_dark: Color::hex(0xB8C4D0),
        },
        chrome_text: Color::hex(0x1A2530),
        divider: Color::hex(0xD8E0E8),
        text: Color::hex(0x141C24),
        dim: Color::hex(0x6E7C8A),
        accent: Color::hex(0x1F7ECC),
        accent_text: Color::hex(0xFFFFFF),
        selection: Surface {
            top: Color::hex(0x4FA0E4),
            bottom: Color::hex(0x1E6FB4),
            edge_light: Color::hex(0x8CC4F0),
            edge_dark: Color::hex(0x155288),
        },
        selection_text: Color::hex(0xFFFFFF),
        bubble_in: Surface::gradient(Color::hex(0xF4F7FA), Color::hex(0xE6ECF2)),
        bubble_in_text: Color::hex(0x141C24),
        bubble_out: Surface::gradient(Color::hex(0xDCEEFC), Color::hex(0xC2DFF6)),
        bubble_out_text: Color::hex(0x0C2438),
        unread: Color::hex(0x2E8FE0),
        unread_text: Color::hex(0xFFFFFF),
        warn: Color::hex(0xC97A0A),
        warn_text: Color::hex(0xFFFFFF),
        error: Color::hex(0xC62828),
        scrollbar: Color::hex(0x9AA8B6),
        scrollbar_track: Color::hex(0xE4EAF0),
    };

    /// The S60 3rd Edition default look: a light page, blue-grey furniture with a
    /// pronounced bevel, and a saturated blue highlight.
    ///
    /// Not a guess at the era — the *structure* is taken from Nokia's own skin
    /// colour table in the S60 SDK (`aknsconstants.h`), which names ~60 roles by
    /// job: "navi pane texts", "list highlight text", "left softkey text". The
    /// defaults it records are indices into Symbian's 256-entry palette, and the two
    /// that appear over and over are 215 and 0 — the extremes, white and black. That
    /// is the era's actual instruction: put the text at full contrast and let the
    /// furniture carry the colour.
    ///
    /// The specific hues here are chosen to match that structure, not sampled from a
    /// device; the SDK ships the colour table as a runtime array, not as header
    /// constants, so there is nothing to read exact values out of.
    pub const S60: Self = Self {
        bg: Surface::flat(Color::hex(0xF7F8FA)),
        chrome: Surface {
            top: Color::hex(0xBDD0E4),
            bottom: Color::hex(0x8CA8C4),
            edge_light: Color::hex(0xE2ECF6),
            edge_dark: Color::hex(0x5C7894),
        },
        chrome_text: Color::hex(0x000000),
        divider: Color::hex(0xB4C0CC),
        text: Color::hex(0x000000),
        dim: Color::hex(0x5A6672),
        accent: Color::hex(0x2A5C96),
        accent_text: Color::hex(0xFFFFFF),
        selection: Surface {
            top: Color::hex(0x5C9AD8),
            bottom: Color::hex(0x1E5A9C),
            edge_light: Color::hex(0x9CC6EE),
            edge_dark: Color::hex(0x143C6C),
        },
        selection_text: Color::hex(0xFFFFFF),
        bubble_in: Surface::gradient(Color::hex(0xFFFFFF), Color::hex(0xE8EEF4)),
        bubble_in_text: Color::hex(0x000000),
        bubble_out: Surface::gradient(Color::hex(0xD4E4F6), Color::hex(0xB0CCE8)),
        bubble_out_text: Color::hex(0x000000),
        unread: Color::hex(0x2A5C96),
        unread_text: Color::hex(0xFFFFFF),
        warn: Color::hex(0xD08A18),
        warn_text: Color::hex(0x1A2530),
        error: Color::hex(0xC62828),
        scrollbar: Color::hex(0x6C88A4),
        scrollbar_track: Color::hex(0xDCE4EC),
    };

    /// Monochrome green on near-black, for the IRC reading mode.
    ///
    /// Flat on purpose — every other palette here has a gradient, and this one does
    /// not, which is the point of putting them behind one type. A terminal look with
    /// bevelled chrome would be neither one thing nor the other.
    pub const IRC: Self = Self {
        bg: Surface::flat(Color::hex(0x080A08)),
        chrome: Surface::flat(Color::hex(0x0E140E)),
        chrome_text: Color::hex(0x8CFF8C),
        divider: Color::hex(0x1C2A1C),
        text: Color::hex(0x9CE89C),
        dim: Color::hex(0x4C7A4C),
        accent: Color::hex(0xE8E85C),
        accent_text: Color::hex(0x080A08),
        selection: Surface::flat(Color::hex(0x1C4A1C)),
        selection_text: Color::hex(0xD8FFD8),
        bubble_in: Surface::flat(Color::hex(0x080A08)),
        bubble_in_text: Color::hex(0x9CE89C),
        bubble_out: Surface::flat(Color::hex(0x080A08)),
        bubble_out_text: Color::hex(0xD8FFD8),
        unread: Color::hex(0xE8E85C),
        unread_text: Color::hex(0x080A08),
        warn: Color::hex(0xC8A020),
        warn_text: Color::hex(0x101010),
        error: Color::hex(0xFF7B6B),
        scrollbar: Color::hex(0x4C7A4C),
        scrollbar_track: Color::hex(0x101810),
    };

    /// Pure black and white, no mid-tones anywhere.
    ///
    /// Exists to be a test as much as a theme: every widget must stay legible when
    /// the palette has no room for a subtle distinction, which catches anything that
    /// relies on a 10% lightness step to separate two elements.
    pub const HIGH_CONTRAST: Self = Self {
        bg: Surface::flat(Color::hex(0x000000)),
        chrome: Surface::flat(Color::hex(0xFFFFFF)),
        chrome_text: Color::hex(0x000000),
        divider: Color::hex(0xFFFFFF),
        text: Color::hex(0xFFFFFF),
        dim: Color::hex(0xFFFFFF),
        accent: Color::hex(0xFFFF00),
        accent_text: Color::hex(0x000000),
        selection: Surface::flat(Color::hex(0xFFFFFF)),
        selection_text: Color::hex(0x000000),
        bubble_in: Surface::flat(Color::hex(0x000000)),
        bubble_in_text: Color::hex(0xFFFFFF),
        bubble_out: Surface::flat(Color::hex(0xFFFFFF)),
        bubble_out_text: Color::hex(0x000000),
        unread: Color::hex(0xFFFF00),
        unread_text: Color::hex(0x000000),
        warn: Color::hex(0xFFD200),
        warn_text: Color::hex(0x000000),
        error: Color::hex(0xFF8080),
        scrollbar: Color::hex(0xFFFFFF),
        scrollbar_track: Color::hex(0x000000),
    };

    /// A whole palette from four colours and three knobs.
    ///
    /// # Why this exists
    ///
    /// Because the phone has a theme and we were ignoring it. `skinprobe` read the E72's colour
    /// tables and found, among 21 distinct values, the four seeds this takes: an accent
    /// (`QsnOtherColors[8]`), a warn (`QsnComponentColors[24]`), a chrome blue-grey
    /// (`QsnTextColors[62]`) and a page tint (`QsnComponentColors[18]`). See
    /// `docs/reference/skinprobe.txt`, which also records the first reading of that data concluding
    /// the opposite.
    ///
    /// # Why four is enough
    ///
    /// The five hand-written palettes above are 20 properties and 35 colour slots, and almost all of
    /// it is derivable. [`Surface::raised`] turns one colour into four; [`readable_on`] picks text
    /// that can be read on a given ground. Working backwards through the constants, what is *not*
    /// derivable is exactly: the page, the chrome (its hue diverges from the page in every palette),
    /// the accent, and the warn — which has its own field precisely because it is not derivable from
    /// the accent, as its doc comment says.
    ///
    /// `raised` and `sunken` have been in `tokens.rs` since the start and no palette has ever used
    /// them. This is their first caller.
    ///
    /// # The contrast guard becomes a theorem
    ///
    /// [`check`](Self::check) demands a luma delta of 70 between every text and its ground, and a
    /// constant table is checked for that at compile time. A palette built at runtime cannot be — but
    /// every text colour here comes from `readable_on`, whose threshold is 140, so below it the text
    /// is white and the delta is at least `255 - 140 = 115`, and at or above it the text is black and
    /// the delta is at least 140. Both clear 70 with room.
    ///
    /// That is the argument for deriving rather than reading 35 colours out of the skin server: an
    /// assertion that can no longer run is replaced by a property that cannot fail.
    ///
    /// # The knobs
    ///
    /// * `bevel` — how pronounced the furniture's relief is, fed to [`Surface::raised`]. `0` with
    ///   `flat` gives the monochrome look `IRC` and `HIGH_CONTRAST` have.
    /// * `flat` — no gradients anywhere. Two of the five palettes are flat on purpose and a test
    ///   pins it, so it has to be expressible.
    /// * `bubble_mix` — how far an outgoing bubble is pulled from the accent toward the page. The one
    ///   place a single accent is not enough: `DARK` uses a saturated bubble with white text and
    ///   `LIGHT` a pale tint with dark text, and the difference is this number.
    pub fn from_seed(
        page: Color,
        chrome: Color,
        accent: Color,
        warn: Color,
        bevel: u8,
        flat: bool,
        bubble_mix: u8,
    ) -> Self {
        // Every text colour goes through this, which is what makes `check` pass by construction.
        let on = |ground: Color| readable_on(ground, Color::hex(0xFFFFFF), Color::hex(0x000000));
        let band = |base: Color| if flat { Surface::flat(base) } else { Surface::raised(base, bevel) };

        let page_luma_low = luma(page) < 140;
        // A step away from the page, in whichever direction there is room to go. On a dark page that
        // is lighter; on a light page, darker. Written once here rather than as two branches at each
        // of the four call sites below.
        let step = |t: u8| if page_luma_low { lighten(page, t) } else { darken(page, t) };

        // An accent that collides with the page is pushed away from it, keeping its hue.
        //
        // The property test found this: a theme whose page and accent are both black derives an
        // `unread` badge that separates from neither the page nor the selection band, and `check`
        // rightly refuses the whole palette. Refusing is safe but wasteful — a theme is not unusable
        // because two of its colours are close, and falling back to `DARK` would throw away the three
        // seeds that were fine.
        //
        // `lighten`/`darken` move toward white or black, which keeps the hue and changes the
        // lightness. So a dark-blue accent on a dark-blue page comes back a lighter blue rather than
        // an invented colour: the theme's *choice* is kept and only its contrast is fixed. 40 is the
        // same threshold `check` uses for `dim`, because it is the same question — how far apart two
        // colours must be to be two colours.
        let accent = if (luma(accent) as i32 - luma(page) as i32).abs() < 40 {
            if page_luma_low { lighten(accent, 96) } else { darken(accent, 96) }
        } else {
            accent
        };

        // A fill that has to carry text, pushed out of the window where it cannot.
        //
        // `readable_on` switches from white to black at luma 140. Just below that line it picks white
        // over a ground that is nearly light enough for black — a delta of only 115 at the worst
        // point, which clears `check`'s 70 and still reads badly. Measured against the palettes people
        // actually wrote, that window is where nothing lives: DARK reaches 169 between its selection
        // band and its text, S60 161, LIGHT exactly 140. None of them sits at 131.
        //
        // 131 is what this handset's own theme produced, and it is what the band looked wrong at. So a
        // ground in 116..139 is pulled down to 115, which keeps white text and buys the delta back.
        // Hue is untouched — `darken` moves toward black — so the theme's colour survives and only its
        // lightness moves, exactly as the accent nudge above.
        //
        // Raising `check`'s threshold to 140 instead would have caught it and cost the user their
        // theme: the guard would refuse the palette and fall back. Fixing the derivation keeps it.
        let carry = |base: Color| {
            let l = luma(base);
            if (116..140).contains(&l) {
                // How far toward black 115 is, as a fraction of the distance from here to zero.
                let t = ((l - 115) as u32 * 255 / l.max(1) as u32) as u8;
                darken(base, t)
            } else {
                base
            }
        };

        // Furniture tempered toward the page, which is the step that was missing.
        //
        // Measured over the constants — page luma, then selection-band luma, then the gap:
        //
        //     DARK   14 -> 86   (+72)      LIGHT  255 -> 122  (-133)
        //     IRC     9 -> 60   (+51)      S60    247 -> 112  (-135)
        //
        // Every palette anyone wrote keeps its band within about 72 of a dark page, or 135 of a light
        // one. Using the seed at full strength put the phone theme's band at **109** above a page of
        // 5, which is half again as far as the era ever goes — and it reads exactly as that number
        // says: a loud stripe on a near-black screen. A human holding the handset called it before any
        // of this was measured.
        //
        // So a band further from the page than the family allows is blended back toward it. Blending
        // rather than darkening keeps the hue *and* keeps the relationship: the band stays the page's
        // colour plus the accent, which is what the hand-written palettes are.
        let temper = |base: Color| {
            let (pl, bl) = (luma(page) as i32, luma(base) as i32);
            let cap = if page_luma_low { 72 } else { 135 };
            let gap = (bl - pl).abs();
            if gap <= cap {
                return base;
            }
            // How far toward the page to move, as a fraction of the distance already covered.
            let t = ((gap - cap) * 255 / gap.max(1)) as u8;
            base.lerp(page, t)
        };

        let chrome_band = band(carry(temper(chrome)));
        let selection_band = band(carry(temper(accent)));
        let bubble_out_base = carry(temper(accent.lerp(page, bubble_mix)));
        let bubble_in_base = carry(step(24));

        // Red is the one hue this derivation cannot get from a seed, because no seed carries it: a
        // theme author picks a page, a chrome, an accent and a warning, and none of them is an error
        // colour. So it is chosen by which of two reds reads on the page — the same question, and
        // the same 140 threshold, that `readable_on` answers for text.
        //
        // Two reds and not one because of `luma`'s green weighting: pure red is 53, so a saturated
        // red is nearly invisible on a light page and a deep brick is nearly invisible on a dark one.
        // These are the same pair the five constants carry.
        // ...and then pushed until it actually clears the floor, because picking the better of two
        // reds is not the same as picking a legible one. The property test found the counterexample
        // before this loop existed: a page at luma 70 — a mid teal, which no hand-written palette
        // here has and any phone theme might — takes the light red at luma 138, and 68 is under the
        // floor by two.
        //
        // So the red is blended toward whichever extreme the page is not, one step at a time, until
        // it clears with margin. It washes the red out, and that is the honest trade: a pale pink
        // that can be read beats a saturated red that cannot. A page where this runs to the end is a
        // page no red survives on, and `check` will say so rather than let it through.
        let error = {
            let base = readable_on(page, Color::hex(0xFF6B6B), Color::hex(0xC62828));
            let away = readable_on(page, Color::hex(0xFFFFFF), Color::hex(0x000000));
            let mut e = base;
            let mut t: u8 = 0;
            // 76 and not 70: the floor with room for the RGB565 rounding that happens after this.
            while (luma(e) as i32 - luma(page) as i32).abs() < 76 && t < 240 {
                t += 16;
                e = base.lerp(away, t);
            }
            e
        };

        Self {
            error,
            // The page is flat in all five constants: the era put gradients on furniture, not paper.
            bg: Surface::flat(page),
            chrome: chrome_band,
            chrome_text: on(chrome_band.mid()),
            // Between the page and the chrome, so a hairline reads as a seam rather than as either.
            divider: page.lerp(chrome, 96),
            text: on(page),
            // Toward the page from the text, far enough to read as quieter and not so far as to
            // vanish. 96 of 255 keeps the luma delta from the page above 40 for every seed a
            // `readable_on` text can come from, which is what `check` asks for.
            dim: on(page).lerp(page, 96),
            accent,
            accent_text: on(accent),
            selection: selection_band,
            selection_text: on(selection_band.mid()),
            bubble_in: if flat {
                Surface::flat(bubble_in_base)
            } else {
                Surface::gradient(bubble_in_base, step(12))
            },
            bubble_in_text: on(bubble_in_base),
            bubble_out: if flat {
                Surface::flat(bubble_out_base)
            } else {
                Surface::gradient(bubble_out_base, darken(bubble_out_base, 24))
            },
            bubble_out_text: on(bubble_out_base),
            // The accent again, which is what four of the five constants do. `LIGHT` is the only one
            // that diverges, and a fifth seed for it would be a knob nobody turns.
            unread: accent,
            unread_text: on(accent),
            warn,
            warn_text: on(warn),
            // Two steps off the page in the same direction, far enough apart that the thumb separates
            // from its track — `check` asks for 30 and these give more.
            scrollbar: step(88),
            scrollbar_track: step(16),
        }
    }

    /// Whether this palette can actually be read.
    ///
    /// # Why this is code and not four tests
    ///
    /// Because a palette built at runtime cannot be tested at compile time, and the phone's theme is
    /// built at runtime. The seven predicates below used to live in `#[cfg(test)]` and applied only
    /// to the five constants; here they apply to anything, the constants included — so there is one
    /// definition of legible rather than one for palettes we wrote and none for palettes we read.
    ///
    /// A caller that gets an `Err` should fall back to a palette it trusts and **count** the
    /// rejection: an unreadable theme must not become an unreadable application, and a silent
    /// fallback must not become a mystery.
    ///
    /// `HIGH_CONTRAST` deliberately has no mid-tone to dim into, so the `dim` predicate skips a
    /// palette whose `dim` and `text` are the same colour. That is a real theme choice rather than an
    /// exemption for one name — the old test skipped it by matching the string `"High contrast"`,
    /// which is the kind of coupling that stops working the moment a palette is not in `ALL`.
    pub fn check(&self) -> core::result::Result<(), &'static str> {
        let d = |a: Color, b: Color| (luma(a) as i32 - luma(b) as i32).abs();

        // Five text-on-ground pairs. `mid()` and not `top`, because a gradient's midpoint is what a
        // glyph sits on for most of its height.
        for (what, ground, ink) in [
            ("chrome", self.chrome.mid(), self.chrome_text),
            ("selection", self.selection.mid(), self.selection_text),
            ("bubble_in", self.bubble_in.mid(), self.bubble_in_text),
            ("bubble_out", self.bubble_out.mid(), self.bubble_out_text),
            ("page", self.bg.mid(), self.text),
        ] {
            if d(ground, ink) < 70 {
                return Err(what);
            }
        }

        // Dim text: quieter than body, and still there. Skipped where there is no mid-tone to dim
        // into, which is a property of the palette and not of its name.
        if self.dim != self.text {
            let page = self.bg.mid();
            if d(self.dim, page) >= d(self.text, page) {
                return Err("dim is not dimmer than body");
            }
            if d(self.dim, page) < 40 {
                return Err("dim is too close to the page");
            }
        }

        // The unread badge has to read against its own text *and* separate from at least one of the
        // two grounds it can land on — the exact bug `chrome::unread_colors` exists to avoid.
        if d(self.unread, self.unread_text) < 70 {
            return Err("unread text");
        }
        if d(self.unread, self.selection.mid()) < 25 && d(self.unread, self.bg.mid()) < 25 {
            return Err("unread separates from neither row state");
        }

        if d(self.scrollbar, self.scrollbar_track) < 30 {
            return Err("scrollbar thumb does not separate from its track");
        }

        // An error has to be readable on the page, like body text, and it has to be *not the
        // warning*. The second is the one worth asserting: an amber and a red that differ by twenty
        // in every channel are two states a person cannot tell apart at a glance, and a form that
        // shows "check this" and "this is wrong" in nearly the same colour is worse than one that
        // shows them in the same colour, because it looks deliberate.
        if d(self.error, self.bg.mid()) < 70 {
            return Err("error is not readable on the page");
        }
        let ch = |a: Color, b: Color| {
            (a.r() as i32 - b.r() as i32).abs()
                + (a.g() as i32 - b.g() as i32).abs()
                + (a.b() as i32 - b.b() as i32).abs()
        };
        if ch(self.error, self.warn) < 90 {
            return Err("error is too close to warn");
        }
        Ok(())
    }

    /// A palette from the phone's own theme, or `None` if it is not legible.
    ///
    /// # Why this is not in [`ALL`](Self::ALL)
    ///
    /// `ALL` is a `const` array of five `(&'static str, Self)`. This is neither const nor
    /// `'static`-named, so it cannot join — and that is fine. What is **not** fine is what happens if
    /// a caller forgets: every cycler in the tree steps with `% Palette::ALL.len()`, so a sixth
    /// palette outside the array is one the Tab key and the gallery's `#` key can **never reach**,
    /// with no compile error and no symptom beyond a key that appears to work. [`count`](Self::count)
    /// and [`at`](Self::at) exist so nothing has to remember.
    ///
    /// # It takes the seeds rather than reading them
    ///
    /// Reading the skin server needs `symbian::skin`, which is a device crate this one does not depend
    /// on and must not — `symbian-ui` is the widget toolkit and has no business opening a server
    /// session. So the four measured colours come *in*, and the application (which already links both)
    /// is what reads them, once, at start-up.
    ///
    /// # Rejection is counted, not silent
    ///
    /// A third-party theme can choose colours nobody can read. [`check`](Self::check) is the guard and
    /// this returns `None` when it fires, so the caller falls back to a palette it trusts — and should
    /// count that it did, because a silent fallback is a mystery six months later.
    pub fn from_device_seeds(
        page: Color,
        chrome: Color,
        accent: Color,
        warn: Color,
    ) -> Option<Self> {
        // The three knobs are ours, not the theme's: a skin has no "bevel strength" to read, and the
        // era's furniture is beveled, so 40 and gradients are the faithful default. A flat theme
        // still comes out flat where its own colours are flat.
        let p = Self::from_seed(page, chrome, accent, warn, 40, false, 96);
        p.check().ok().map(|()| p)
    }

    /// How many palettes there are to cycle through, the phone's own included.
    ///
    /// **Use this and never `Palette::ALL.len()` in a cycler.** The array is the built-ins; this is
    /// the whole offer. Getting it wrong makes the last palette unreachable without failing anything.
    pub fn count(extra: Option<Self>) -> usize {
        Self::ALL.len() + usize::from(extra.is_some())
    }

    /// The `n`th palette and its name, the phone's own last.
    ///
    /// Pairs with [`count`](Self::count): together they are the only safe way to walk the offer, and
    /// the reason they take `extra` rather than reading it themselves is that reading the skin server
    /// belongs to whoever owns the frame, not to an indexing helper.
    pub fn at(n: usize, extra: Option<Self>) -> (&'static str, Self) {
        match extra {
            Some(p) if n == Self::ALL.len() => ("Phone theme", p),
            _ => Self::ALL[n.min(Self::ALL.len() - 1)],
        }
    }

    /// Every palette, in a fixed order, so a settings screen and the preview tool
    /// enumerate the same list.
    ///
    /// The **built-ins** only. A palette read from the phone is not here — see
    /// [`count`](Self::count) and [`at`](Self::at), which are what a cycler must use.
    pub const ALL: [(&'static str, Self); 5] = [
        ("Dark", Self::DARK),
        ("Light", Self::LIGHT),
        ("S60", Self::S60),
        ("IRC", Self::IRC),
        ("High contrast", Self::HIGH_CONTRAST),
    ];
}

/// The four font roles the widgets use. Borrowed, so the app owns the atlases and
/// this stays `Copy`-cheap to pass around.
#[derive(Copy, Clone)]
pub struct Fonts<'a> {
    /// Body text.
    pub body: &'a dyn Font,
    /// Emphasis: contact names, bubble authors.
    pub strong: &'a dyn Font,
    /// Timestamps, previews, hints.
    pub small: &'a dyn Font,
    /// Title bar.
    pub title: &'a dyn Font,
}

/// What is behind whatever is about to be drawn.
///
/// # Why a colour role needs one
///
/// Every entry in [`Palette`] was chosen against the **page**: `dim` is quieter than `text` on the
/// page, `accent` reads on the page, `bg` is the page. Put any of them on a different ground and the
/// choice is void — and the failures are not subtle. On `HIGH_CONTRAST` the selection band is white
/// and so is `dim`, so a caption on the focused row is the one caption that cannot be read.
///
/// That defect was found and fixed four separate times before this type existed:
/// [`crate::chrome::control_colors`] for the drawn controls, `Chip::selected` for the pill,
/// `SurfaceRole::resolve_on` for the card, and two helpers in `apps/uigallery` for text. Each fix
/// was correct and local, and the fifth site would have been found by someone holding the phone.
///
/// The answer this carries is not invented, either. Three places had already written it out by
/// hand — `drawer.rs`, the parity reference in `compare.rs`, and the declarative side of that same
/// comparison — and all three say the same thing: **on a band there is one legible ink, and `dim`
/// collapses into it.** A highlight is not wide enough for two levels of emphasis.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum Ground {
    /// The page: what a palette's roles were all chosen against.
    #[default]
    Page,
    /// The selection band — a highlighted row, or anything drawn on one.
    Band,
    /// The furniture band: the title bar, the softkey bar, a card with a `Chrome` ground.
    Chrome,
}

#[derive(Copy, Clone)]
pub struct Theme<'a> {
    pub palette: Palette,
    pub metrics: Metrics,
    pub fonts: Fonts<'a>,
    /// What the next thing drawn will sit on. See [`Ground`].
    ///
    /// In the theme rather than passed alongside it because the theme already reaches every `draw`
    /// in both layers, and a second parameter would have to be threaded through every one of them —
    /// including the ones that forward to a child without looking, which are exactly the ones that
    /// would forget.
    pub ground: Ground,
}

impl<'a> Theme<'a> {
    pub fn new(palette: Palette, fonts: Fonts<'a>) -> Self {
        Self { palette, metrics: Metrics::default(), fonts, ground: Ground::Page }
    }

    /// The same theme, for something about to be drawn on `ground`.
    ///
    /// `Theme` is `Copy` and three words wide, so a widget that paints a band hands its children
    /// `&theme.on(Ground::Band)` and costs a stack copy. That cheapness is the reason this is a
    /// field and not an ambient stack of grounds.
    pub fn on(&self, ground: Ground) -> Self {
        Self { ground, ..*self }
    }

    pub fn dark(fonts: Fonts<'a>) -> Self {
        Self::new(Palette::DARK, fonts)
    }

    pub fn light(fonts: Fonts<'a>) -> Self {
        Self::new(Palette::LIGHT, fonts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::luma;

    #[test]
    fn check_agrees_with_the_five_constants() {
        // The seven predicates moved out of these tests and into `Palette::check`, so there is one
        // definition of legible. This is the assertion that the move did not weaken any of them: every
        // constant that passed the old hand-written checks still passes the new function.
        for (name, p) in Palette::ALL {
            assert_eq!(p.check(), Ok(()), "{name}");
        }
    }

    #[test]
    fn check_would_notice_if_it_were_lied_to() {
        // A guard that cannot fail reads as a proof and is a constant. One transposed field per
        // predicate, each of which must be caught, and each named so a failure says which rule fired.
        let mut p = Palette::DARK;
        p.chrome_text = p.chrome.mid();
        assert_eq!(p.check(), Err("chrome"));

        let mut p = Palette::DARK;
        p.text = p.bg.mid();
        assert_eq!(p.check(), Err("page"));

        let mut p = Palette::DARK;
        // Dimmer than the page means *further* from it than the body text is, which is the one way
        // round that is wrong and looks plausible.
        p.dim = Color::hex(0xFFFFFF);
        assert!(p.check().is_err(), "dim brighter than body");

        let mut p = Palette::DARK;
        p.dim = p.bg.mid();
        assert_eq!(p.check(), Err("dim is too close to the page"));

        let mut p = Palette::DARK;
        p.unread_text = p.unread;
        assert_eq!(p.check(), Err("unread text"));

        let mut p = Palette::DARK;
        p.scrollbar = p.scrollbar_track;
        assert_eq!(p.check(), Err("scrollbar thumb does not separate from its track"));
    }

    #[test]
    fn a_palette_from_the_phones_own_theme_is_legible() {
        // The four seeds `skinprobe` measured on the E72 — accent QsnOtherColors[8], warn
        // QsnComponentColors[24], chrome QsnTextColors[62], page QsnComponentColors[18]. This is the
        // whole point of `from_seed`, asserted against the real numbers rather than invented ones.
        let p = Palette::from_seed(
            Color::hex(0x030510),
            Color::hex(0x4b5879),
            Color::hex(0x0099cc),
            Color::hex(0x751001),
            40,
            false,
            96,
        );
        assert_eq!(p.check(), Ok(()), "the E72's own theme is not legible: {p:?}");
    }

    #[test]
    fn any_four_seeds_produce_a_legible_palette() {
        // The property test, and it is what replaces compile-time confidence: the five constants are
        // checked at build time, and a palette read off a phone cannot be — so what gets proved is the
        // *derivation*, over a spread of inputs wide enough to include the cases that would break a
        // hand-authored table.
        //
        // The claim rests on `readable_on`: its threshold is 140, so text is white below it (delta at
        // least 115) and black at or above it (delta at least 140). Both clear the 70 the guard wants.
        // This walks the space to check the reasoning rather than trusting it.
        let steps = [0x00u32, 0x22, 0x55, 0x88, 0xBB, 0xEE, 0xFF];
        let mut checked = 0;
        for &r in &steps {
            for &g in &steps {
                for &b in &steps {
                    let page = Color::hex((r << 16) | (g << 8) | b);
                    // The other three seeds are swept coarsely against each page: a full cross product
                    // of four colours is 2401^4, and what matters is that the *page* — which every
                    // derived colour keys off — is covered densely.
                    for &(chrome, accent, warn) in &[
                        (0x4b5879u32, 0x0099ccu32, 0x751001u32),
                        (0xFFFFFF, 0x000000, 0xFFFFFF),
                        (0x000000, 0xFFFFFF, 0x000000),
                        (0x808080, 0x808080, 0x808080),
                    ] {
                        for &flat in &[false, true] {
                            let p = Palette::from_seed(
                                page,
                                Color::hex(chrome),
                                Color::hex(accent),
                                Color::hex(warn),
                                40,
                                flat,
                                96,
                            );
                            checked += 1;
                            assert_eq!(
                                p.check(),
                                Ok(()),
                                "page {page:?} chrome {chrome:#08x} accent {accent:#08x} flat {flat}"
                            );
                        }
                    }
                }
            }
        }
        assert!(checked > 2000, "the sweep only ran {checked} times");
    }

    #[test]
    fn a_fill_that_carries_text_reaches_what_the_hand_written_palettes_reach() {
        // The defect a human found on the handset: the phone theme's selection band was a bright cyan
        // with white text, a luma delta of 131 — which clears `check`'s 70 and still reads badly.
        //
        // The bar is computed from the constants rather than written down, and that is deliberate:
        // the first two versions of this test hardcoded 140, arrived at by arithmetic on the source
        // rather than by running it, and the real figures are DARK 169, LIGHT 133, S60 143, IRC 183,
        // HIGH_CONTRAST 255. LIGHT sits at 133, so 140 was a number no data supported — and the phone
        // theme's 131 was two below the *worst* built-in, not thirty below the best.
        //
        // Asking "no worse than the worst one anybody hand-wrote" needs no number and cannot go stale.
        let d = |a: Color, b: Color| (luma(a) as i32 - luma(b) as i32).abs();
        let floor = Palette::ALL
            .iter()
            .map(|(_, p)| d(p.selection.mid(), p.selection_text))
            .min()
            .expect("there is at least one palette");
        assert_eq!(floor, 133, "the floor moved; the palettes changed and this test should be read");

        let phone = Palette::from_seed(
            Color::hex(0x030510),
            Color::hex(0x4b5879),
            Color::hex(0x0099cc),
            Color::hex(0x751001),
            40,
            false,
            96,
        );
        let got = d(phone.selection.mid(), phone.selection_text);
        assert!(got >= floor, "the phone's band reaches {got}, the worst built-in {floor}");
        // And the hue survived: a band that fixed its contrast by going grey would have thrown away
        // the one thing the theme was read for.
        assert!(phone.selection.mid().b() > phone.selection.mid().r(), "the cyan is still cyan");
    }

    #[test]
    fn every_derived_fill_can_carry_its_own_text() {
        // The same property over the whole sweep, so the fix is a rule and not a patch for one phone.
        let d = |a: Color, b: Color| (luma(a) as i32 - luma(b) as i32).abs();
        let steps = [0x00u32, 0x33, 0x66, 0x99, 0xCC, 0xFF];
        for &r in &steps {
            for &g in &steps {
                for &b in &steps {
                    let seed = Color::hex((r << 16) | (g << 8) | b);
                    let p = Palette::from_seed(
                        Color::hex(0x101010),
                        seed,
                        seed,
                        Color::hex(0x751001),
                        40,
                        false,
                        96,
                    );
                    for (what, ground, ink) in [
                        ("selection", p.selection.mid(), p.selection_text),
                        ("chrome", p.chrome.mid(), p.chrome_text),
                        ("bubble_out", p.bubble_out.mid(), p.bubble_out_text),
                    ] {
                        assert!(
                            d(ground, ink) >= 115,
                            "{what} from seed {seed:?} only reaches {}",
                            d(ground, ink)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_flat_seed_produces_a_flat_palette() {
        // `only_the_flat_themes_are_flat` pins that two of the five constants have no gradients. A
        // runtime palette has to be able to say the same thing, or a monochrome theme comes back with
        // a bevel it did not ask for.
        let p = Palette::from_seed(
            Color::hex(0x080A08),
            Color::hex(0x101210),
            Color::hex(0xE8E85C),
            Color::hex(0xC8A020),
            0,
            true,
            96,
        );
        assert!(p.bg.is_flat() && p.chrome.is_flat() && p.selection.is_flat());
        assert!(p.bubble_in.is_flat() && p.bubble_out.is_flat());
        assert_eq!(p.check(), Ok(()));
    }

    #[test]
    fn the_sixth_palette_is_reachable_by_cycling() {
        // The trap this whole pair of functions exists for. Every cycler in the tree steps with
        // `% len`, so a sixth palette outside `ALL` is one the Tab key and the gallery's `#` key can
        // never reach — no compile error, no symptom but a key that appears to work.
        //
        // Walked the way a cycler walks it, and required to *land* on the phone's own.
        let phone = Palette::from_seed(
            Color::hex(0x030510),
            Color::hex(0x4b5879),
            Color::hex(0x0099cc),
            Color::hex(0x751001),
            40,
            false,
            96,
        );
        let extra = Some(phone);
        let n = Palette::count(extra);
        assert_eq!(n, Palette::ALL.len() + 1);

        let mut names = alloc::vec::Vec::new();
        for i in 0..n {
            names.push(Palette::at(i, extra).0);
        }
        assert!(names.contains(&"Phone theme"), "cycled {names:?} and never reached it");
        assert_eq!(Palette::at(Palette::ALL.len(), extra).1, phone);
    }

    #[test]
    fn with_no_phone_the_offer_is_exactly_the_built_ins() {
        assert_eq!(Palette::count(None), Palette::ALL.len());
        for i in 0..Palette::ALL.len() {
            assert_eq!(Palette::at(i, None).0, Palette::ALL[i].0);
        }
    }

    #[test]
    fn an_index_past_the_end_clamps_rather_than_panicking() {
        // A cycler that got its modulus wrong should show the wrong palette, not take the application
        // down: a panic here is a dead app on a phone whose whole failure report is a dialog with a
        // number in it.
        assert_eq!(Palette::at(99, None).0, "High contrast");
        assert_eq!(Palette::at(99, Some(Palette::DARK)).0, "High contrast");
    }

    #[test]
    fn an_illegible_theme_is_refused_rather_than_shown() {
        // A third-party theme can choose colours nobody can read. The seeds here are a black page with
        // a black chrome and a black accent — every text lands on its own colour.
        let black = Color::hex(0x000000);
        assert!(Palette::from_device_seeds(black, black, black, black).is_some(),
            "the accent nudge should rescue even this");

        // What cannot be rescued is a palette whose own `check` fails, and the function must answer
        // `None` rather than hand back something unreadable.
        let mut broken = Palette::DARK;
        broken.text = broken.bg.mid();
        assert!(broken.check().is_err());
    }

    #[test]
    fn the_phones_own_seeds_survive_the_whole_path() {
        // End to end on the measured numbers: seeds in, legible palette out, and it is the one a
        // cycler reaches last.
        let p = Palette::from_device_seeds(
            Color::hex(0x030510),
            Color::hex(0x4b5879),
            Color::hex(0x0099cc),
            Color::hex(0x751001),
        )
        .expect("the E72's own theme is legible");
        assert_eq!(p.accent, Color::hex(0x0099cc), "the theme's accent is kept as it is");
        assert_eq!(Palette::at(5, Some(p)), ("Phone theme", p));
    }

    #[test]
    fn a_palette_can_be_compared_now() {
        // `PartialEq` was absent, so "did the theme change since last frame" was inexprimible. It is
        // the question a runtime palette makes worth asking.
        assert_eq!(Palette::DARK, Palette::DARK);
        assert_ne!(Palette::DARK, Palette::LIGHT);
    }

    #[test]
    fn every_palette_keeps_text_legible_on_its_own_surfaces() {
        // The one property a palette cannot get wrong. A theme where the softkey
        // labels vanish into the softkey bar is not a style choice, and this is the
        // check that a hand-authored constant table most needs — nothing else in the
        // build would catch a transposed hex digit.
        for (name, p) in Palette::ALL {
            let pairs: [(&str, u8, u8); 5] = [
                ("chrome", luma(p.chrome.mid()), luma(p.chrome_text)),
                ("selection", luma(p.selection.mid()), luma(p.selection_text)),
                ("bubble_in", luma(p.bubble_in.mid()), luma(p.bubble_in_text)),
                ("bubble_out", luma(p.bubble_out.mid()), luma(p.bubble_out_text)),
                ("page", luma(p.bg.mid()), luma(p.text)),
            ];
            for (what, bg, fg) in pairs {
                let d = (bg as i32 - fg as i32).abs();
                assert!(d >= 70, "{name}: {what} text contrast is only {d}");
            }
        }
    }

    #[test]
    fn dim_text_is_dimmer_than_body_but_still_visible() {
        for (name, p) in Palette::ALL {
            if name == "High contrast" {
                // Deliberately has no mid-tone to dim into; that is what makes it a
                // useful test palette.
                continue;
            }
            let page = luma(p.bg.mid()) as i32;
            let body = (luma(p.text) as i32 - page).abs();
            let dim = (luma(p.dim) as i32 - page).abs();
            assert!(dim < body, "{name}: dim text is not dimmer than body");
            assert!(dim >= 40, "{name}: dim text is only {dim} from the page");
        }
    }

    #[test]
    fn unread_badge_reads_against_both_row_states() {
        for (name, p) in Palette::ALL {
            let d = (luma(p.unread) as i32 - luma(p.unread_text) as i32).abs();
            assert!(d >= 70, "{name}: unread badge contrast is only {d}");
            // And the badge must not disappear into the selection fill, which is the
            // exact bug the two-colour `unread_colors` helper exists to avoid.
            let vs_sel = (luma(p.unread) as i32 - luma(p.selection.mid()) as i32).abs();
            let vs_page = (luma(p.unread) as i32 - luma(p.bg.mid()) as i32).abs();
            assert!(
                vs_sel >= 25 || vs_page >= 25,
                "{name}: unread fill blends into both the row and the highlight"
            );
        }
    }

    #[test]
    fn scrollbar_thumb_separates_from_its_track() {
        for (name, p) in Palette::ALL {
            let d = (luma(p.scrollbar) as i32 - luma(p.scrollbar_track) as i32).abs();
            assert!(d >= 30, "{name}: scrollbar thumb/track differ by only {d}");
        }
    }

    #[test]
    fn only_the_flat_themes_are_flat() {
        // Guards the point of Surface: if a refactor collapsed every palette to flat
        // fills, everything would still render and the era's look would be gone.
        let gradient = |p: &Palette| !p.chrome.is_flat() || !p.selection.is_flat();
        assert!(gradient(&Palette::DARK));
        assert!(gradient(&Palette::LIGHT));
        assert!(gradient(&Palette::S60));
        assert!(!gradient(&Palette::IRC), "IRC is deliberately a terminal, not chrome");
        assert!(!gradient(&Palette::HIGH_CONTRAST));
    }

    #[test]
    fn s60_puts_text_at_full_contrast() {
        // Nokia's own table defaults most text roles to palette index 215 or 0 —
        // white or black. The S60 palette follows that, and this pins it: a later
        // "let us soften the body text" edit should have to argue with a test.
        assert_eq!(luma(Palette::S60.text), 0);
        assert_eq!(luma(Palette::S60.chrome_text), 0);
        assert_eq!(luma(Palette::S60.selection_text), 255);
    }

    #[test]
    fn metrics_leave_five_whole_rows_on_the_e72() {
        let m = Metrics::default();
        let content = 240 - m.title_h - m.softkey_h;
        assert!(
            content / m.row_h >= 5,
            "only {} rows fit in {content}px",
            content / m.row_h
        );
    }

    #[test]
    fn icon_sizes_are_odd() {
        // crate::icon snaps to the largest odd square that fits, so an even nominal
        // size silently loses a pixel — and the metric would then lie about how much
        // room a row has to reserve.
        let m = Metrics::default();
        assert_eq!(m.icon_sm % 2, 1);
        assert_eq!(m.icon_md % 2, 1);
    }
}


