//! Which palette this phone's applications use, and where that choice lives.
//!
//! The launcher offers a theme — the phone's own, or one of ours — and **every application on the
//! handset follows it**, with each free to override it locally. This module is the contract: the
//! encoding, the file, and the ladder that turns a preference into a [`Palette`].
//!
//! ```ignore
//! // in the app's `device/src/lib.rs`
//! symbian_app::entry!(myapp::App::new(), palette = symbian_app::theme_pref::current());
//! // and once, at start-up, before the first frame
//! symbian_app::theme_pref::load(myapp::override_choice(), Palette::DARK);
//! ```
//!
//! # Why the contract is here
//!
//! The launcher writes it and seven applications in seven repositories read it. Neither side depends
//! on the other, so the only place they can agree on a path and a format is the SDK they both
//! already depend on — the argument [`symbian::intent`] makes for its own channel, applied again.
//!
//! And specifically **`symbian-app`**, rather than a crate of its own: this is the only crate that
//! already depends on both [`symbian`] (which reads the skin server and the disk) and
//! [`symbian_ui`] (which owns [`Palette`]). It is also the owner of [`crate::entry`], whose
//! `palette` argument is where the answer is consumed.
//!
//! # Why a file and not Publish & Subscribe
//!
//! P&S carries an `i32`, which is all this needs, so the payload is not the reason. Two other things
//! are:
//!
//! * **A property does not survive a reboot.** A theme preference must.
//! * A subscription costs one of the four a process is allowed
//!   (`shim_prop.cpp`'s `KMaxSubs`), and `shim_prop_subscribe` matches on the *key alone, ignoring
//!   the category* — so two unrelated keys that happen to share a number collapse into one
//!   subscription, silently.
//!
//! A file in `C:\Data\` needs no capability to write, which is the same reason `intent` puts its
//! request there, and it is what `C:\Data\replace_main.flag` — the launcher's other public flag —
//! already does.
//!
//! # It fails open
//!
//! The rule is [`symbian::device::in_use`]'s, and it is worth stating in its own words: *"a stop
//! signal has to fail open"*. Here that means **a missing or unreadable file is the application's
//! own default**, never a screen with no theme. Every rung of the ladder below falls through to the
//! default rather than to nothing, and a rejection is *counted* — a silent fallback is a mystery six
//! months later.

use symbian::fs::{self, Fs, ShimFs, Utf16Path};
use symbian_ui::{Color, Palette};

/// Where the system-wide choice lives.
///
/// `C:\Data\` and not either party's private directory: a private directory belongs to one SID and
/// reaching into another's needs `AllFiles`, which is a protected capability and has already stopped
/// a package installing in this project once.
pub const SYSTEM_FILE: &str = "C:\\Data\\theme.pref";

/// What an application, or the system, has chosen.
///
/// One encoding used in both places, because two would be two things to keep in step. The system
/// file never stores [`Follow`](Self::Follow); if it somehow does, it reads as [`Phone`](Self::Phone),
/// which is failing towards the thing the user most likely asked for rather than towards nothing.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum Choice {
    /// Use the system's choice. The default, and only meaningful in an application's own file.
    #[default]
    Follow,
    /// The theme the phone itself is wearing, derived from the skin server.
    Phone,
    /// One of [`Palette::ALL`], by index.
    Builtin(u8),
}

impl Choice {
    /// The byte this is stored as.
    pub fn to_byte(self) -> u8 {
        match self {
            Choice::Follow => 0,
            Choice::Phone => 1,
            // Saturating rather than wrapping: an index past the array would come back as a
            // *different* palette, which is worse than coming back as the last one.
            Choice::Builtin(n) => 2u8.saturating_add(n),
        }
    }

    /// The choice a byte means. Out of range reads as [`Follow`](Self::Follow).
    ///
    /// Tolerant on purpose. A byte from the future — a palette this build does not have — must not
    /// leave the reader with no answer, and `Follow` is the answer that defers to whoever does.
    pub fn from_byte(b: u8) -> Self {
        match b {
            0 => Choice::Follow,
            1 => Choice::Phone,
            n if ((n - 2) as usize) < Palette::ALL.len() => Choice::Builtin(n - 2),
            _ => Choice::Follow,
        }
    }

    /// How many choices a settings screen can offer, and what each is called.
    ///
    /// `Follow` is index 0 and only belongs in an application's own list — the launcher's list
    /// starts at [`Phone`](Self::Phone), because "follow the system" *is* the system.
    pub fn name(self) -> &'static str {
        match self {
            Choice::Follow => "Follow the system",
            Choice::Phone => "The phone's theme",
            Choice::Builtin(n) => Palette::at(n as usize, None).0,
        }
    }
}

/// Read the system-wide choice, and the phone's palette when one was published with it.
///
/// # The file carries the **answer**, not the question
///
/// It would be tidier to store just the choice and let each application derive the phone's theme
/// for itself. It does not work, and the failure is a link error rather than a subtle one: reading
/// the skin needs `shim_skin_color`, which only exists behind `USE_SKIN=1`, and that flag pulls in
/// `aknskins` — a library `apps/skinprobe/app.conf` says *"nothing resident gets"* lightly, because
/// an import that does not resolve makes an image silently never load.
///
/// `symbian::skin`'s own doc had already written the answer: *"A headless daemon has none, so every
/// read answers `NotReady`. **That is why a launcher reads the theme and tells its daemons rather
/// than each asking.**"* So the launcher — the one binary that carries the flag — measures the four
/// seeds and writes them here, and every reader derives the palette with
/// [`Palette::from_device_seeds`], which is arithmetic in `symbian-ui` and needs no server at all.
pub fn read_system<F: Fs>(fs: &mut F) -> Option<(Choice, Option<Palette>)> {
    let path = Utf16Path::new(SYSTEM_FILE).ok()?;
    let bytes = fs::read(fs, &path).ok().flatten()?;
    let byte = *bytes.first()?;
    // The seeds are optional and come after the choice: a file written before they existed, or by a
    // launcher that could not read the skin, is a valid file with no phone palette in it.
    // `> 16` rather than `>= 1 + 16`: the same number, and clippy is right that the arithmetic
    // spelling only looks self-documenting. One choice byte plus four four-byte seeds.
    let phone = (bytes.len() > 16)
        .then(|| {
            let c = |i: usize| {
                Color::hex(u32::from_le_bytes([
                    bytes[1 + i * 4],
                    bytes[2 + i * 4],
                    bytes[3 + i * 4],
                    bytes[4 + i * 4],
                ]))
            };
            Palette::from_device_seeds(c(0), c(1), c(2), c(3))
        })
        .flatten();
    let choice = match Choice::from_byte(byte) {
        // The system file saying "follow the system" is a file with no answer in it. Reading it as
        // the phone's theme is the fail-open rung: it is what a person who opened this setting at
        // all most likely meant.
        Choice::Follow => Choice::Phone,
        other => other,
    };
    Some((choice, phone))
}

/// Publish the system-wide choice, and the four seeds if the phone's theme was readable.
///
/// The launcher is the only thing that should call this — it is the one binary carrying
/// `USE_SKIN=1`, and the seeds are what save every other application from needing it.
pub fn write_system<F: Fs>(
    fs: &mut F,
    choice: Choice,
    seeds: Option<[Color; 4]>,
) -> symbian::error::Result<()> {
    let path = Utf16Path::new(SYSTEM_FILE)?;
    let mut out = alloc::vec![choice.to_byte()];
    if let Some(s) = seeds {
        for c in s {
            // Rebuilt from the channels rather than stored as one word: `Color` is ARGB and the
            // alpha is not part of a seed. Writing the three that matter is what makes a file
            // written today readable by a build that changes the internal representation.
            out.extend_from_slice(&[c.b(), c.g(), c.r(), 0]);
        }
    }
    fs::write_atomic(fs, &path, &out)
}

/// The phone's own theme, derived from four colours measured off the skin server.
///
/// `None` when there is no skin (a headless process has none — the instance belongs to the
/// application's Avkon UI), when a seed is refused, or when the derived palette fails
/// [`Palette::check`]. Every one of those is logged, because an instrument that only speaks when
/// things go wrong cannot confirm that they went right.
///
/// The four indices are **measured**, not read out of a header: `docs/reference/skinprobe.txt`
/// found them and `docs/reference/skin/themes.md` corrected them.
/// **Only a binary with `USE_SKIN=1` may call this.** Everything else reads the seeds this one
/// publishes; see [`read_system`].
///
/// # Why these four, and not the four that were here first
///
/// The first four were picked by *appearance*: skinprobe dumped every index, the dump was sorted by
/// luma, and the entries that looked like a page, a chrome, an accent and a warn were taken. Three
/// of them were saturated, which felt like the right thing to look for and was in fact the way to
/// find the half of the table a theme never touches.
///
/// The E72 keeps two families. The **saturated** entries are platform constants: applying Golden and
/// then a second, pink theme left `QsnOtherColors[8]` at `0x0099cc` and `QsnComponentColors[24]` at
/// `0x751001` across both switches. The **neutral** entries are what a theme repaints — twenty of
/// them moved on both switches. Reading four saturated indices meant reading, precisely, the
/// immobile half: under Golden not one of the original four moved, so the phone theme derived a
/// palette identical to the default one and the feature looked broken while working exactly as
/// written.
///
/// So three of the four now come from indices that were *measured moving*, and the fourth
/// deliberately does not:
///
/// | seed | index | default | Golden | pink theme |
/// |---|---|---|---|---|
/// | page | `QsnComponentColors[5]` | `0x030510` | `0x000000` | `0xffffff` |
/// | chrome | `QsnOtherColors[10]` | `0x797979` | `0x87796d` | `0xcbaab1` |
/// | accent | `QsnHighlightColors[2]` | `0xfffdff` | `0xffffff` | `0x8d636d` |
/// | warn | `QsnComponentColors[24]` | `0x751001` | `0x751001` | `0x751001` |
///
/// `page` carries the one thing a palette most needs from a theme, which is whether the theme is
/// light or dark — it flips to white under the pink theme, and `from_device_seeds` derives text by
/// contrast from it. Its value under the default theme is *the same* `0x030510` the old
/// `QsnComponentColors[18]` read, which is the corroboration that it is a background and not an
/// inverted foreground: two independent indices agreeing on the page colour, only one of them
/// following the theme.
///
/// `accent` is the highlight colour, so it is the accent by role rather than by resemblance — the
/// band a selected row is painted with is the one colour a theme is *for*.
///
/// `warn` stays platform-fixed, and that is the point rather than a gap. There is no theme-driven red
/// in the twenty, and there should not be one: an error colour a theme can move is an error colour a
/// theme can make illegible. `Palette::error` already derives a legible red from the page, and
/// `Ink::Error`'s guard test holds it to a 70-luma delta on all three grounds.
pub fn phone_seeds() -> Option<[Color; 4]> {
    let p = phone_palette_and_seeds()?;
    Some(p.1)
}

/// The palette and the seeds it came from. `USE_SKIN=1` only.
pub fn phone_palette_and_seeds() -> Option<(Palette, [Color; 4])> {
    let (p, s) = phone_inner()?;
    Some((p, s))
}

fn phone_inner() -> Option<(Palette, [Color; 4])> {
    use symbian::skin::{self, Table};

    let raw = |t, i| match skin::color(t, i) {
        Ok(c) => {
            symbian::log!("theme: {:?}[{}] = {:#08x}", t, i, c);
            Some(Color::hex(c))
        }
        Err(e) => {
            symbian::log!("theme: {:?}[{}] refused {}", t, i, e.code());
            None
        }
    };

    // Three measured moving across two theme switches, and one measured *not* moving on purpose.
    // See this module's docs for the table and for why the previous four read the immobile half.
    let seeds = (
        raw(Table::Component, 5),
        raw(Table::Other, 10),
        raw(Table::Highlight, 2),
        raw(Table::Component, 24),
    );
    let (Some(page), Some(chrome), Some(accent), Some(warn)) = seeds else {
        symbian::log!("theme: no skin here — the built-ins are the whole offer");
        return None;
    };

    match Palette::from_device_seeds(page, chrome, accent, warn) {
        Some(p) => Some((p, [page, chrome, accent, warn])),
        None => {
            symbian::log!("theme: derived but NOT legible; falling back");
            None
        }
    }
}

/// Turn a choice into a palette, walking down to `fallback` rather than to nothing.
///
/// Pure but for the skin read, and the skin read is the only part a host cannot do — which is why
/// `phone` is passed in rather than fetched here. [`load`] is what fetches it, once.
pub fn resolve(app: Choice, system: Option<Choice>, phone: Option<Palette>, fallback: Palette) -> Palette {
    let choice = match app {
        Choice::Follow => system.unwrap_or(Choice::Follow),
        other => other,
    };
    match choice {
        // Either nobody has expressed a preference, or the phone's theme was not usable. Both end
        // at the application's own default, which is the rung that must never fail.
        Choice::Follow => fallback,
        Choice::Phone => phone.unwrap_or(fallback),
        Choice::Builtin(n) => Palette::at(n as usize, None).1,
    }
}

/// The resolved palette, cached. This is what [`crate::entry`]'s `palette` argument should call.
///
/// It is a plain read of a static because `entry!` evaluates that expression **twice per step** and
/// outside the application — there is no `&Model` in scope — so it must be cheap and must not touch
/// the disk or the skin server.
pub fn current() -> Palette {
    CURRENT.load()
}

/// Resolve once, at start-up, and remember the answer.
///
/// Call this before the first frame. `app` is this application's own override — most read it from
/// wherever they already keep settings — and `fallback` is what it wears when nothing else has an
/// opinion.
pub fn load<F: Fs>(fs: &mut F, app: Choice, fallback: Palette) {
    let (system, phone) = match read_system(fs) {
        Some((c, p)) => (Some(c), p),
        None => (None, None),
    };
    let wants_phone = matches!(app, Choice::Phone)
        || (matches!(app, Choice::Follow) && matches!(system, Some(Choice::Phone) | None));
    if wants_phone && phone.is_none() {
        symbian::log!("theme: wanted the phone's, none published — using the built-in default");
    }
    CURRENT.store(resolve(app, system, phone, fallback));
}

/// The device convenience: read from the real file system.
pub fn load_from_disk(app: Choice, fallback: Palette) {
    let mut fs = ShimFs;
    load(&mut fs, app, fallback);
}

struct PaletteCell(core::cell::UnsafeCell<Palette>);

// SAFETY: single-threaded by construction. Every access is from the GUI thread — `rust_step` and the
// `entry!` expression it evaluates — because that is the only thread that draws. The worker thread
// `entry!` can start never sees this.
//
// `AtomicUsize` would be tidier and is not available: this target has no atomics. The same note is
// on `apps/uigallery`'s copy, which this replaces — the point of putting it here is that each
// application no longer writes its own `unsafe`.
unsafe impl Sync for PaletteCell {}

impl PaletteCell {
    const fn new(p: Palette) -> Self {
        Self(core::cell::UnsafeCell::new(p))
    }
    fn load(&self) -> Palette {
        // SAFETY: see the `Sync` impl.
        unsafe { *self.0.get() }
    }
    fn store(&self, p: Palette) {
        // SAFETY: see the `Sync` impl.
        unsafe { *self.0.get() = p }
    }
}

/// Dark until [`load`] says otherwise — so an application that forgets to call it draws a theme
/// rather than nothing, which is the same fail-open rule one level down.
static CURRENT: PaletteCell = PaletteCell::new(Palette::DARK);

#[cfg(test)]
mod tests {
    use super::*;
    use symbian::fs::MemFs;

    /// The three themes actually measured off the E72, run through the derivation the device runs.
    ///
    /// `(name, page, chrome, accent)`; `warn` is the same for all three because the index it comes
    /// from does not move — see [`phone_inner`]'s docs. The numbers are from
    /// `docs/reference/skin/themes.md`, which is the log of two theme switches on the phone.
    const MEASURED: [(&str, u32, u32, u32); 3] = [
        ("default", 0x030510, 0x797979, 0xfffdff),
        ("Golden", 0x000000, 0x87796d, 0xffffff),
        ("pink", 0xffffff, 0xcbaab1, 0x8d636d),
    ];
    const MEASURED_WARN: u32 = 0x751001;

    #[test]
    fn every_measured_phone_theme_derives_a_legible_palette() {
        // The test the device cannot give cheaply and the host can give exactly: the derivation is
        // pure, so real skin colours prove it without a phone in the loop. Its predecessor was
        // "coloquei golden no celular e nada mudou" — a whole round trip through a .sisx to learn
        // something arithmetic.
        for (name, page, chrome, accent) in MEASURED {
            let p = Palette::from_device_seeds(
                Color::hex(page),
                Color::hex(chrome),
                Color::hex(accent),
                Color::hex(MEASURED_WARN),
            );
            assert!(p.is_some(), "{name}: refused seeds the phone really reports");
        }
    }

    #[test]
    fn a_different_phone_theme_is_a_different_palette() {
        // The defect, stated as a test. The old indices passed the legibility check above under
        // every theme and still produced *one* palette, because all four were platform constants:
        // switching to Golden moved none of them. Legible is not the property that was missing;
        // **distinct** is.
        let derive = |(_, page, chrome, accent): (&str, u32, u32, u32)| {
            Palette::from_device_seeds(
                Color::hex(page),
                Color::hex(chrome),
                Color::hex(accent),
                Color::hex(MEASURED_WARN),
            )
            .expect("legibility is the other test's job")
        };
        for (n, left) in MEASURED.iter().enumerate() {
            for right in &MEASURED[n + 1..] {
                let (a, b) = (derive(*left), derive(*right));
                assert_ne!(
                    a.bg.mid().to_rgb565(),
                    b.bg.mid().to_rgb565(),
                    "{} and {} derive the same page — the seeds are not following the theme",
                    left.0,
                    right.0
                );
            }
        }
    }

    #[test]
    fn every_choice_round_trips_through_its_byte() {
        let mut all = alloc::vec![Choice::Follow, Choice::Phone];
        for n in 0..Palette::ALL.len() as u8 {
            all.push(Choice::Builtin(n));
        }
        for c in all {
            assert_eq!(Choice::from_byte(c.to_byte()), c, "{c:?}");
        }
    }

    #[test]
    fn a_byte_from_the_future_defers_instead_of_guessing() {
        // A palette this build does not have. Answering `Follow` hands the question to whoever does
        // know; answering `Builtin(99)` would index past the array, and answering nothing would
        // leave a caller with no palette at all.
        let past_the_end = 2 + Palette::ALL.len() as u8;
        assert_eq!(Choice::from_byte(past_the_end), Choice::Follow);
        assert_eq!(Choice::from_byte(255), Choice::Follow);
    }

    #[test]
    fn the_system_file_saying_follow_reads_as_the_phones_theme() {
        // A file with no answer in it. `Follow` there is a loop — "use the system's choice" written
        // *in* the system's file — and the way out is the thing a person who opened this setting at
        // all most likely meant.
        let mut fs = MemFs::default();
        write_system(&mut fs, Choice::Follow, None).expect("write");
        assert_eq!(read_system(&mut fs).map(|(c, _)| c), Some(Choice::Phone));
    }

    #[test]
    fn no_file_is_not_an_answer_and_not_an_error() {
        assert!(read_system(&mut MemFs::default()).is_none());
    }

    #[test]
    fn the_ladder_always_ends_at_the_applications_own_default() {
        // The fail-open rule, rung by rung. Every one of these is a phone with no launcher of ours,
        // or a theme nobody can read, or a setting nobody has touched — and none of them may produce
        // a screen with no theme.
        let d = Palette::LIGHT; // a distinctive fallback, so a wrong answer is obvious
        assert_eq!(resolve(Choice::Follow, None, None, d), d, "nobody has an opinion");
        assert_eq!(resolve(Choice::Phone, None, None, d), d, "wanted the phone's, no skin");
        assert_eq!(
            resolve(Choice::Follow, Some(Choice::Phone), None, d),
            d,
            "the system wanted the phone's, and it was not legible"
        );
    }

    #[test]
    fn an_applications_own_choice_beats_the_systems() {
        let d = Palette::DARK;
        let system = Some(Choice::Builtin(0));
        assert_eq!(resolve(Choice::Builtin(3), system, None, d), Palette::ALL[3].1);
        // And `Follow` is what defers — the only value that lets the system through.
        assert_eq!(resolve(Choice::Follow, system, None, d), Palette::ALL[0].1);
    }

    #[test]
    fn the_phones_theme_is_used_when_it_is_there() {
        let d = Palette::DARK;
        let phone = Palette::HIGH_CONTRAST; // stands in for a derived one
        assert_eq!(resolve(Choice::Phone, None, Some(phone), d), phone);
        assert_eq!(resolve(Choice::Follow, Some(Choice::Phone), Some(phone), d), phone);
    }

    #[test]
    fn load_resolves_once_and_current_reads_it_back() {
        let mut fs = MemFs::default();
        write_system(&mut fs, Choice::Builtin(2), None).expect("write");
        load(&mut fs, Choice::Follow, Palette::DARK);
        assert_eq!(current(), Palette::ALL[2].1);
        // And an application's own override wins on the next load.
        load(&mut fs, Choice::Builtin(4), Palette::DARK);
        assert_eq!(current(), Palette::ALL[4].1);
    }

    #[test]
    fn the_seeds_travel_with_the_choice_so_a_reader_needs_no_skin_server() {
        // The whole reason the file carries an answer instead of a question. An application without
        // `USE_SKIN=1` cannot call `shim_skin_color` — it does not link — so the phone's theme has
        // to arrive already measured. This is that round trip.
        let mut fs = MemFs::default();
        // The four the E72 measured, from `docs/reference/skinprobe.txt`.
        let seeds = [
            Color::hex(0x030510),
            Color::hex(0x4b5879),
            Color::hex(0x0099cc),
            Color::hex(0x751001),
        ];
        write_system(&mut fs, Choice::Phone, Some(seeds)).expect("write");
        let (choice, phone) = read_system(&mut fs).expect("read");
        assert_eq!(choice, Choice::Phone);
        let phone = phone.expect("the seeds derive a legible palette");
        assert_eq!(phone, Palette::from_device_seeds(seeds[0], seeds[1], seeds[2], seeds[3]).unwrap());

        load(&mut fs, Choice::Follow, Palette::DARK);
        assert_eq!(current(), phone, "and an app that follows wears it");
    }

    #[test]
    fn a_choice_written_without_seeds_still_reads() {
        // A launcher that could not read the skin, or a file from before the seeds existed. It must
        // be a valid file with no phone palette in it, not an unreadable one.
        let mut fs = MemFs::default();
        write_system(&mut fs, Choice::Phone, None).expect("write");
        assert_eq!(read_system(&mut fs), Some((Choice::Phone, None)));
        load(&mut fs, Choice::Follow, Palette::LIGHT);
        assert_eq!(current(), Palette::LIGHT, "and it falls to the app's own default");
    }

    #[test]
    fn resolving_before_publishing_is_what_goes_wrong() {
        // The bug this ordering exists to prevent, written as the sequence that produced it. The
        // launcher chose the phone's theme, called `load` to resolve it, and *then* wrote the file —
        // so `load` read the file as it was before the change, found no seeds, and fell back. The
        // phone stayed dark while the log reported the choice and all four colours.
        let seeds = [
            Color::hex(0x030510),
            Color::hex(0x4b5879),
            Color::hex(0x0099cc),
            Color::hex(0x751001),
        ];
        let phone = Palette::from_device_seeds(seeds[0], seeds[1], seeds[2], seeds[3]).unwrap();

        // Wrong order: resolve, then publish.
        let mut fs = MemFs::default();
        load(&mut fs, Choice::Phone, Palette::DARK);
        write_system(&mut fs, Choice::Phone, Some(seeds)).expect("write");
        assert_eq!(current(), Palette::DARK, "this is the defect, pinned so it stays visible");

        // Right order: publish, then resolve.
        let mut fs = MemFs::default();
        write_system(&mut fs, Choice::Phone, Some(seeds)).expect("write");
        load(&mut fs, Choice::Phone, Palette::DARK);
        assert_eq!(current(), phone, "the file is the source of truth, so it must exist first");
    }
}
