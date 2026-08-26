//! The colours of the theme the *user* chose.
//!
//! S60 themes are user-installable, and the platform keeps their colours in a table the skin server
//! owns. This reads one entry of it, through `shim_skin_color` →
//! `AknsUtils::GetCachedColor`.
//!
//! # Why an application would want it
//!
//! `symbian_ui::Palette` ships five hand-authored constants, and the one called `S60` admits in its
//! own doc comment that its colours "were chosen to match that structure, not sampled from a device".
//! It is a good interpretation of the era. Reading the real table is the difference between looking
//! like our idea of the phone and looking like the phone.
//!
//! # The IDs are data here, not sixty functions there
//!
//! The shim exports **one** accessor taking `(major, minor, index)`. Everything that gives those
//! numbers meaning lives in this file, in Rust, where it is a table a host test can walk — the same
//! argument `symbian::hal` makes about `shim_hal_get`. Sixty exported C++ functions would be sixty
//! chances to transpose a constant, in the one language here that has no test harness on the host.
//!
//! # Needs a UI
//!
//! The skin instance is the *application's*, created by Avkon during app-UI construction. A headless
//! daemon has none, so every read answers [`Error::NotReady`]. That is why a launcher reads the theme
//! and tells its daemons rather than each asking.
//!
//! # `USE_SKIN=1`
//!
//! `aknskins` is not in the base library set. A build that omits the flag does not fail to compile —
//! it fails to *load*, silently, which is why `tools/symbuild` gates it and why the skin probe
//! exists to take that risk first.

use crate::error::{Error, Result};

/// The major half of every skin item ID: the AknSkins UID.
///
/// `aknsconstants.hrh:40` — `EAknsMajorSkin = 0x10005a26`. As `i32` because that is what the ABI
/// takes and what `TAknsItemID::iMajor` is; the value is a UID and its top bit is clear, so the
/// cast is not a reinterpretation.
pub const MAJOR_SKIN: i32 = 0x1000_5a26;

/// Which of the theme's colour tables an entry comes from.
///
/// The minors are `aknsconstants.hrh:470-590`. Only the five that name *colours* are here; the major
/// class has dozens more for bitmaps and layouts, and a table this file cannot read is a name that
/// would only ever be wrong.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum Table {
    /// `EAknsMinorQsnComponentColors` — the parts: scrollbars, sliders, indicators.
    Component = 0x3000,
    /// `EAknsMinorQsnIconColors` — the colour a themed icon is tinted with.
    Icon = 0x3200,
    /// `EAknsMinorQsnTextColors` — every text role the platform names.
    Text = 0x3300,
    /// `EAknsMinorQsnLineColors` — rules and separators.
    Line = 0x3400,
    /// `EAknsMinorQsnOtherColors` — what did not fit anywhere else, which on S60 is a real category.
    Other = 0x3500,
    /// `EAknsMinorQsnHighlightColors` — the selection band and its text.
    Highlight = 0x3600,
}

/// One entry of the active theme's colour table, as `0x00RRGGBB`.
///
/// `index` is the entry within `table` — `EAknsCIQsnTextColorsCG6` is `5`, and so on. The indices are
/// deliberately **not** named in this crate yet: `AknsConstants.h` comments each one
/// (`EAknsCIQsnTextColorsCG6 = 5, // text #6 main area main area texts #215`), but which indices the
/// E72 actually fills, and with what, is a measurement — and naming a role after a comment before
/// measuring it is how a palette ends up derived from an index nobody populates.
///
/// The skin probe dumps every index of every table with its return code. The names go here once
/// that has run, and each will cite the measurement rather than the header.
pub fn color(table: Table, index: i32) -> Result<u32> {
    let mut out: u32 = 0;
    // SAFETY: `out` is a live `u32` for the duration of the call, which is the whole contract — the
    // shim writes it only on success and reads nothing through it.
    let rc = unsafe { symbian_sys::shim_skin_color(MAJOR_SKIN, table as i32, index, &mut out) };
    if rc < 0 {
        return Err(Error::from_code(rc));
    }
    Ok(out)
}

/// Which themed background bitmap to sample.
///
/// The minors are `aknsconstants.hrh:83-104` — the four the platform draws behind everything else.
///
/// Measured on the E72: **all four return NULL** from `GetCachedBitmap`, which reads a cache nothing
/// in the process had filled. Kept because the finding is "these IDs are not cached here", not
/// "sampling is impossible", and a theme that ships bitmaps would answer. The palette does not need
/// it — the colour table carries hue, which `docs/reference/skinprobe.txt` records along with the
/// first reading of that data getting it backwards.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum Background {
    /// `EAknsMinorQsnBgScreen` — behind the whole screen.
    Screen = 0x1000,
    /// `EAknsMinorQsnBgAreaStatus` — behind the status pane, which is our title bar.
    Status = 0x1010,
    /// `EAknsMinorQsnBgAreaControl` — behind the control pane, which is our softkey bar.
    Control = 0x1020,
    /// `EAknsMinorQsnBgAreaMain` — behind the main area, which is our page.
    Main = 0x1100,
}

/// Pixels sampled from a themed background, and the bitmap they came from.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Samples {
    /// Up to 16 colours, `0x00RRGGBB`, on an even grid across the bitmap.
    pub pixels: [u32; 16],
    /// How many of `pixels` are real.
    pub count: usize,
    /// The bitmap's own size, so "no such bitmap" is distinguishable from "a bitmap of nothing".
    pub width: i32,
    pub height: i32,
}

impl Samples {
    /// The average of the samples, as `0x00RRGGBB`.
    ///
    /// The mean rather than the median or the most common, and the choice is arguable — which is
    /// exactly why it is here rather than in the shim. A background is usually a gradient, and the
    /// mean of an even grid over a gradient is its midpoint, which is what
    /// [`Surface::mid`](symbian_ui::Surface::mid) means by a surface's colour. A median would answer
    /// the same for a gradient and differently for a background with a logo in it; if that turns out
    /// to matter, this is the one function that changes.
    ///
    /// `None` when nothing was sampled, rather than black: a theme with no background bitmap and a
    /// theme whose background is black are different findings.
    pub fn mean(&self) -> Option<u32> {
        if self.count == 0 {
            return None;
        }
        let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
        for p in &self.pixels[..self.count] {
            r += (p >> 16) & 0xFF;
            g += (p >> 8) & 0xFF;
            b += p & 0xFF;
        }
        let n = self.count as u32;
        Some(((r / n) << 16) | ((g / n) << 8) | (b / n))
    }
}

/// Sample a themed background bitmap on an even grid.
pub fn background(which: Background) -> Result<Samples> {
    let mut pixels = [0u32; 16];
    let (mut width, mut height) = (0i32, 0i32);
    // SAFETY: all four pointers are live for the call and the shim writes at most `cap` entries,
    // which is the array's own length.
    let rc = unsafe {
        symbian_sys::shim_skin_samples(
            MAJOR_SKIN,
            which as i32,
            pixels.as_mut_ptr(),
            pixels.len() as i32,
            &mut width,
            &mut height,
        )
    };
    if rc < 0 {
        return Err(Error::from_code(rc));
    }
    Ok(Samples { pixels, count: (rc as usize).min(pixels.len()), width, height })
}

/// Every index of `table` from `0` until the first that is not populated, paired with its colour.
///
/// For a probe rather than for an application: it stops at the first refusal, which is the right
/// answer for "how big is this table" and the wrong one for "give me role 12" — a table with a hole
/// in it would be cut short. `color` is what an application calls.
///
/// The cap is 64 because `AknsConstants.h` documents `EAknsCIQsnTextColorsCG63 = 62` — the text table
/// is the deep one, and the E72 fills every index of it. An earlier cap of 40 hid that: the table
/// answered "40 of 40 filled, no gap", which reads as a full table and was really a short ruler.
pub fn walk(table: Table, out: &mut [(i32, u32)]) -> usize {
    let mut n = 0;
    for index in 0..64 {
        if n >= out.len() {
            break;
        }
        match color(table, index) {
            Ok(c) => {
                out[n] = (index, c);
                n += 1;
            }
            Err(_) => break,
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_major_is_the_aknskins_uid() {
        // `aknsconstants.hrh:40`. Pinned as a literal because a transposed digit here reads every
        // colour out of a table that does not exist, and the failure is a black screen rather than an
        // error — `GetCachedColor` would simply answer KErrNotFound for everything.
        assert_eq!(MAJOR_SKIN, 0x1000_5a26);
    }

    #[test]
    fn every_table_minor_matches_the_header() {
        // `aknsconstants.hrh:470-590`, transcribed. This is the whole reason the table lives in Rust:
        // it is data, and data can be checked against its source in a test that runs on every build.
        assert_eq!(Table::Component as i32, 0x3000);
        assert_eq!(Table::Icon as i32, 0x3200);
        assert_eq!(Table::Text as i32, 0x3300);
        assert_eq!(Table::Line as i32, 0x3400);
        assert_eq!(Table::Other as i32, 0x3500);
        assert_eq!(Table::Highlight as i32, 0x3600);
    }

    #[test]
    fn the_minors_are_distinct() {
        // Two tables sharing a minor would read one and be named the other, and every colour from the
        // second would silently be the first's. Cheap to assert, invisible otherwise.
        let all = [
            Table::Component as i32,
            Table::Icon as i32,
            Table::Text as i32,
            Table::Line as i32,
            Table::Other as i32,
            Table::Highlight as i32,
        ];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b, "two tables share a minor");
            }
        }
    }

    #[test]
    fn every_background_minor_matches_the_header() {
        // `aknsconstants.hrh:83-104`.
        assert_eq!(Background::Screen as i32, 0x1000);
        assert_eq!(Background::Status as i32, 0x1010);
        assert_eq!(Background::Control as i32, 0x1020);
        assert_eq!(Background::Main as i32, 0x1100);
    }

    #[test]
    fn the_mean_of_a_gradient_is_its_midpoint() {
        // What `Surface::mid` means by a surface's colour, which is why the mean is the choice here.
        let mut s = Samples { pixels: [0; 16], count: 2, width: 10, height: 10 };
        s.pixels[0] = 0x00_00_00;
        s.pixels[1] = 0xFF_FF_FF;
        assert_eq!(s.mean(), Some(0x7F_7F_7F));
    }

    #[test]
    fn the_mean_averages_each_channel_and_not_the_packed_number() {
        // Averaging the `u32`s would mix red into green. Pure red and pure blue average to a dark
        // purple, not to something in the middle of the integer range.
        let mut s = Samples { pixels: [0; 16], count: 2, width: 4, height: 4 };
        s.pixels[0] = 0xFF_00_00;
        s.pixels[1] = 0x00_00_FF;
        assert_eq!(s.mean(), Some(0x7F_00_7F));
    }

    #[test]
    fn no_samples_is_none_rather_than_black() {
        // A theme with no background bitmap and a theme whose background is black are different
        // findings, and a caller deriving a palette must be able to tell them apart.
        let s = Samples { pixels: [0; 16], count: 0, width: 0, height: 0 };
        assert_eq!(s.mean(), None);
    }

    #[test]
    fn on_the_host_there_is_no_background_either() {
        assert!(background(Background::Main).is_err());
    }

    #[test]
    fn on_the_host_there_is_no_skin_and_it_says_so() {
        // The host stub answers NotReady, which is also what a headless daemon on the device gets. A
        // caller that treated an error as "black" would paint a black screen on both.
        assert!(color(Table::Text, 5).is_err());
    }

    #[test]
    fn a_walk_with_no_device_finds_nothing_rather_than_looping() {
        let mut buf = [(0i32, 0u32); 8];
        assert_eq!(walk(Table::Text, &mut buf), 0);
    }

    #[test]
    fn a_walk_cannot_overrun_the_buffer_it_was_given() {
        // The cap that matters is the caller's, not the loop's: a probe with a four-entry buffer must
        // get four entries even if the device answers sixty.
        let mut buf = [(0i32, 0u32); 0];
        assert_eq!(walk(Table::Text, &mut buf), 0);
    }
}
