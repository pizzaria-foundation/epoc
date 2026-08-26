//! Which language this phone's applications speak, and where that choice lives.
//!
//! The launcher offers a language — the phone's own, or one of ours — and **every application on the
//! handset follows it**, with each free to override it locally. Same contract as
//! [`crate::theme_pref`], same file shape, same ladder, and deliberately so: two mechanisms that
//! differed only in their subject would be two things to keep in step.
//!
//! # What is simpler here than for the theme
//!
//! The theme file carries the **answer** — four measured seeds — because deriving a palette needs
//! the skin server, and only the one binary with `USE_SKIN=1` can ask it.
//!
//! Nothing here needs deriving. `User::Language()` is euser, so every process on the phone can ask
//! it directly and get the same answer. The file therefore stores only the *question*: a choice
//! byte, and nothing else. An application that finds no file asks the phone itself rather than
//! going without.
//!
//! That is why this module is a third the size of its sibling. The difference is worth stating
//! because the sibling's shape looks like ceremony until you know what it was working around.
//!
//! # It fails open, twice
//!
//! The rule is [`symbian::device::in_use`]'s — *"a stop signal has to fail open"* — and it applies
//! at both rungs: no file means ask the phone, and a phone that cannot answer means English. An
//! untranslated interface is usable. A blank one is not.

use symbian::fs::{self, Fs, ShimFs, Utf16Path};
use symbian_ui::Lang;

/// Where the system-wide choice lives.
///
/// `C:\Data\` and not either party's private directory, for the reason [`crate::theme_pref`] gives:
/// a private directory belongs to one SID, and reaching into another's needs `AllFiles`.
pub const SYSTEM_FILE: &str = "C:\\Data\\lang.pref";

/// What an application, or the system, has chosen.
///
/// One encoding used in both places. The system file never stores [`Follow`](Self::Follow); if it
/// somehow does, it reads as [`System`](Self::System) — failing towards what the user most likely
/// meant rather than towards nothing.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum Choice {
    /// Use the system's choice. The default, and only meaningful in an application's own file.
    #[default]
    Follow,
    /// Whatever language the handset itself is set to.
    System,
    /// Pinned, whatever the handset says.
    Fixed(Lang),
}

/// The languages a settings screen can offer, in the order it offers them.
///
/// A list rather than deriving from the enum, for the reason `Palette::ALL` exists: a screen that
/// cycled with `% something.len()` and a variant living outside that array is a variant the cycler
/// can never reach, **with no compile error**. One array, and everything counts from it.
pub const FIXED: [Lang; 2] = [Lang::En, Lang::Pt];

impl Choice {
    /// The byte this is stored as.
    pub fn to_byte(self) -> u8 {
        match self {
            Choice::Follow => 0,
            Choice::System => 1,
            Choice::Fixed(l) => 2 + FIXED.iter().position(|&f| f == l).unwrap_or(0) as u8,
        }
    }

    /// The choice a byte means. Out of range reads as [`Follow`](Self::Follow).
    ///
    /// Tolerant on purpose, the same way the theme's is: a byte from a build that speaks a language
    /// this one does not must not leave the reader with no answer, and `Follow` defers to whoever
    /// does know.
    pub fn from_byte(b: u8) -> Self {
        match b {
            0 => Choice::Follow,
            1 => Choice::System,
            n if ((n - 2) as usize) < FIXED.len() => Choice::Fixed(FIXED[(n - 2) as usize]),
            _ => Choice::Follow,
        }
    }

    /// What a settings row calls it.
    ///
    /// Not translated, and that is on purpose: a language picker that renamed itself when you
    /// changed the language would hide the row you need to change it back with. Every phone's own
    /// language menu does the same.
    pub fn name(self) -> &'static str {
        match self {
            Choice::Follow => "Follow the system",
            Choice::System => "The phone's language",
            Choice::Fixed(Lang::En) => "English",
            Choice::Fixed(Lang::Pt) => "Português",
        }
    }
}

/// Read the system-wide choice, if one has been published.
pub fn read_system<F: Fs>(fs: &mut F) -> Option<Choice> {
    let path = Utf16Path::new(SYSTEM_FILE).ok()?;
    let bytes = fs::read(fs, &path).ok().flatten()?;
    Some(match Choice::from_byte(*bytes.first()?) {
        // The system file has nobody to defer to.
        Choice::Follow => Choice::System,
        other => other,
    })
}

/// Publish the system-wide choice. The launcher's job, and nobody else's.
pub fn write_system<F: Fs>(fs: &mut F, choice: Choice) -> Result<(), symbian::Error> {
    let path = Utf16Path::new(SYSTEM_FILE)?;
    fs::write_atomic(fs, &path, &[choice.to_byte()])
}

/// The ladder: an application's own choice, then the system's, then the phone, then English.
///
/// Pure, so a host test can walk every rung — which is the whole reason it is not folded into
/// [`load`].
pub fn resolve(app: Choice, system: Option<Choice>, phone: Lang) -> Lang {
    let effective = match app {
        Choice::Follow => system.unwrap_or(Choice::System),
        pinned => pinned,
    };
    match effective {
        Choice::Fixed(l) => l,
        // Both `System` and a `Follow` that fell through here mean the same thing: ask the phone.
        _ => phone,
    }
}

/// Resolve once and tell the toolkit, so every `strings!` table answers in it.
///
/// Called by [`crate::adopt_language`], which [`crate::entry!`] runs before the application is
/// constructed. An application passes its own override; most pass [`Choice::Follow`].
pub fn load<F: Fs>(fs: &mut F, app: Choice) {
    let lang = resolve(app, read_system(fs), symbian::locale::language());
    symbian::log!("lang: app={app:?} -> {lang:?}");
    symbian_ui::lang::set(lang);
}

/// The device-side convenience: [`load`] against the real filesystem.
pub fn load_system(app: Choice) {
    let mut fs = ShimFs;
    load(&mut fs, app);
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian::fs::MemFs;

    #[test]
    fn every_choice_round_trips_through_its_byte() {
        let mut all = alloc::vec![Choice::Follow, Choice::System];
        for l in FIXED {
            all.push(Choice::Fixed(l));
        }
        for c in all {
            assert_eq!(Choice::from_byte(c.to_byte()), c, "{c:?}");
        }
    }

    #[test]
    fn a_byte_from_a_build_that_speaks_more_languages_defers() {
        // Not an error and not a guess: a build that knows a third language writes a byte this one
        // has never seen, and the honest reading is "somebody else decided" rather than a language
        // picked by arithmetic.
        assert_eq!(Choice::from_byte(9), Choice::Follow);
        assert_eq!(Choice::from_byte(255), Choice::Follow);
    }

    #[test]
    fn no_file_means_ask_the_phone() {
        let mut fs = MemFs::new();
        assert_eq!(read_system(&mut fs), None);
        assert_eq!(resolve(Choice::Follow, None, Lang::Pt), Lang::Pt, "the phone answers");
    }

    #[test]
    fn the_ladder_ends_where_the_phone_says() {
        // Follow -> system says System -> the phone. Three rungs, and the phone is the floor.
        assert_eq!(resolve(Choice::Follow, Some(Choice::System), Lang::Pt), Lang::Pt);
        assert_eq!(resolve(Choice::Follow, Some(Choice::System), Lang::En), Lang::En);
    }

    #[test]
    fn an_applications_own_choice_beats_the_systems() {
        assert_eq!(
            resolve(Choice::Fixed(Lang::En), Some(Choice::Fixed(Lang::Pt)), Lang::Pt),
            Lang::En,
            "the application pinned English on a Portuguese phone with a Portuguese system"
        );
    }

    #[test]
    fn the_system_file_saying_follow_reads_as_the_phones_language() {
        // Nothing above the system file to defer to, so `Follow` there would be a loop. Written
        // rather than assumed, because the byte is reachable: a launcher build with an off-by-one
        // in its settings list would write it.
        let mut fs = MemFs::new();
        let path = Utf16Path::new(SYSTEM_FILE).unwrap();
        fs::write_atomic(&mut fs, &path, &[0]).unwrap();
        assert_eq!(read_system(&mut fs), Some(Choice::System));
    }

    #[test]
    fn a_pinned_language_survives_the_round_trip_through_disk() {
        let mut fs = MemFs::new();
        write_system(&mut fs, Choice::Fixed(Lang::Pt)).unwrap();
        assert_eq!(read_system(&mut fs), Some(Choice::Fixed(Lang::Pt)));
        // And it wins over a phone set to something else, which is the point of pinning.
        assert_eq!(resolve(Choice::Follow, read_system(&mut fs), Lang::En), Lang::Pt);
    }

    #[test]
    fn load_resolves_and_the_toolkit_reads_it_back() {
        // The end of the chain: the thing every `strings!` table actually asks.
        let mut fs = MemFs::new();
        write_system(&mut fs, Choice::Fixed(Lang::Pt)).unwrap();
        load(&mut fs, Choice::Follow);
        assert_eq!(symbian_ui::lang::current(), Lang::Pt);
        symbian_ui::lang::set(Lang::En);
    }
}
