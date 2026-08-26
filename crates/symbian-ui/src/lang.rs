//! Which language the interface is drawn in, and the macro that declares what it says.
//!
//! # The toolkit reads the language; it neither holds it nor fetches it
//!
//! Three crates, and each has exactly the part it can have:
//!
//! | | |
//! |---|---|
//! | `symbian` | *fetches* it. Reading `User::Language()` means calling the shim |
//! | `symbian-sys` | *holds* it. A mutable static with no atomics needs an `UnsafeCell`, and this crate carries `#![forbid(unsafe_code)]` |
//! | here | *reads* it, in [`strings!`] |
//!
//! So [`current`] and [`set`] are re-exported from `symbian_sys::lang` rather than defined here, and
//! a caller writes `symbian_ui::lang::current()` without having to know that.
//!
//! The value is **pushed in**: whoever can read the phone calls [`set`] once at start-up
//! (`symbian_app::entry!` does), and every widget reads [`current`]. That is the shape the theme
//! settled on for the same reason — the toolkit has no business opening a server session, and a
//! widget wants a question a static can answer inside a draw call, without an argument threaded
//! through forty signatures to reach the one row that needed it.
//!
//! # The default is English, and that matters more than it sounds
//!
//! An application that never calls [`set`] draws English rather than nothing — the same rule as the
//! palette one crate over: *"Dark until `load` says otherwise, so an application that forgets to
//! call it draws a theme rather than nothing"*. A missing translation should degrade to readable,
//! never to blank.

pub use symbian_sys::lang::{current, set};

/// Declare what the interface says, in every language it says it in.
///
/// ```
/// symbian_ui::strings! {
///     save = { en: "Save", pt: "Salvar" },
///     back = { en: "Back", pt: "Voltar" },
/// }
///
/// symbian_ui::lang::set(symbian_ui::Lang::Pt);
/// assert_eq!(save(), "Salvar");
/// ```
///
/// Each entry becomes a function returning `&'static str`, so a call site reads
/// `chrome::softkeys(c, s::save(), s::back(), theme)` and costs a load and a branch.
///
/// # Why a macro, when a `match` written by hand does the same thing
///
/// Three things it gives that a hand-written table does not, and the first is the one worth having:
///
/// 1. **A missing language does not compile.** The macro arm requires both `en` and `pt`; there is
///    no shape of this table that forgets one. When a third language is added, every table in every
///    repository stops compiling until it is translated — which is a feature, because the
///    alternative is a screen that silently falls back and nobody notices for a year.
///
///    Asserted rather than claimed — this is the guarantee the whole design rests on, so it is
///    written as a test that fails to build:
///
///    ```compile_fail
///    symbian_ui::strings! {
///        save = { en: "Save" },
///    }
///    ```
/// 2. **A phrase exists once.** Two screens cannot translate "Save" two different ways, which is
///    exactly what happened to the softkeys before this existed.
/// 3. **The table is the translator's list.** One file, greppable, with no code in it.
///
/// # Why there is no `t!(save)`
///
/// Because it would be a macro whose entire job is to rename a function call. `save()` is already
/// as short, already resolves through ordinary name resolution, already jumps to definition in an
/// editor, and needs nobody to learn it. The macro that earns its place is the one that *declares*
/// the table.
///
/// # A trailing comma is allowed in both places
///
/// Inside the braces as well as between entries, because a long entry wraps onto its own lines and
/// then wants one — and rustfmt adds it. Found by the first table long enough to wrap, which failed
/// with `no rules expected \`,\`` and pointed at the string rather than at the macro.
///
/// # What it does not do
///
/// Interpolation. `"3 events"` needs an argument, and a macro that takes arguments *and* lets word
/// order differ per language is a bigger thing than this. Compose at the call site —
/// `format!("{} {}", n, s::events())` — and know that the order happens to match between English and
/// Portuguese and will not match everywhere.
#[macro_export]
macro_rules! strings {
    ($( $(#[$meta:meta])* $name:ident = { en: $en:expr, pt: $pt:expr $(,)? } ),* $(,)?) => {
        $(
            $(#[$meta])*
            pub fn $name() -> &'static str {
                match $crate::lang::current() {
                    $crate::Lang::Pt => $pt,
                    $crate::Lang::En => $en,
                }
            }
        )*
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian_sys::Lang;

    crate::strings! {
        save = { en: "Save", pt: "Salvar" },
        back = { en: "Back", pt: "Voltar" },
    }

    #[test]
    fn a_string_follows_the_language() {
        set(Lang::En);
        assert_eq!(save(), "Save");
        set(Lang::Pt);
        assert_eq!(save(), "Salvar");
        set(Lang::En);
    }

    #[test]
    fn every_entry_is_translated_and_not_copied() {
        // The failure this catches is the boring one: a table filled in by copying the English into
        // the `pt` slot to make it compile. Two entries, both languages, four distinct strings.
        set(Lang::En);
        let (en_save, en_back) = (save(), back());
        set(Lang::Pt);
        let (pt_save, pt_back) = (save(), back());
        set(Lang::En);
        assert_ne!(en_save, pt_save);
        assert_ne!(en_back, pt_back);
        assert_ne!(en_save, en_back, "and the two entries are not each other");
    }

    #[test]
    fn the_default_is_english_rather_than_nothing() {
        // Not a tautology about the constant: it is the promise an application relies on when it
        // forgets to call `set`, which is what a headless build does — it has no start-up to hook.
        assert_eq!(Lang::default(), Lang::En);
    }
}
