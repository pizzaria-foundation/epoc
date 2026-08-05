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

use crate::tokens::{Space, Surface};

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

#[derive(Copy, Clone, Debug)]
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
        scrollbar: Color::hex(0xFFFFFF),
        scrollbar_track: Color::hex(0x000000),
    };

    /// Every palette, in a fixed order, so a settings screen and the preview tool
    /// enumerate the same list.
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

#[derive(Copy, Clone)]
pub struct Theme<'a> {
    pub palette: Palette,
    pub metrics: Metrics,
    pub fonts: Fonts<'a>,
}

impl<'a> Theme<'a> {
    pub fn new(palette: Palette, fonts: Fonts<'a>) -> Self {
        Self { palette, metrics: Metrics::default(), fonts }
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
