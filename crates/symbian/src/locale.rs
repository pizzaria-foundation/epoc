//! What language the phone is set to.
//!
//! One call into the shim and one table over the result. The call is `User::Language()`, which is
//! euser and therefore always linked — no capability, no session, no gate.
//!
//! # The table is the whole point of this file
//!
//! `TLanguage` has around a hundred and sixty values and the two that matter here are not where
//! anybody would guess. Read off `sdk/epoc32/include/e32const.h` rather than remembered:
//!
//! | | |
//! |---|---|
//! | `ELangEnglish` | 1 |
//! | `ELangAmerican` | 10 |
//! | `ELangPortuguese` | 13 — *Portugal* |
//! | **`ELangBrazilianPortuguese`** | **76** |
//! | `ELangEnglish_Apac` | 129 |
//! | `ELangEnglish_Taiwan`, `_HongKong`, `_Prc`, `_Japan`, `_Thailand` | 157, 158, 159, 160, 161 |
//!
//! So "English" is **eight** values, not one, and Brazilian Portuguese is nowhere near Portuguese.
//! A handset in São Paulo answers 76; one in London answers 1; one in New York answers 10. Writing
//! `lang == 1` would work on exactly one of those three and look right in review.
//!
//! # Everything unknown is English
//!
//! A phone set to Spanish gets English, not an empty interface. That is the same fail-open rule
//! `device::in_use` argues for — *"a stop signal has to fail open"* — applied to text: an
//! untranslated screen is usable and a blank one is not.

use symbian_sys::Lang;

/// `ELangPortuguese`, Portugal.
const PORTUGUESE: i32 = 13;
/// `ELangBrazilianPortuguese`. Not adjacent to Portuguese, and the one a handset here reports.
const BRAZILIAN_PORTUGUESE: i32 = 76;

/// The language the platform reports, mapped onto what we can speak.
///
/// Never fails. A shim that cannot answer — the host, where there is no locale to read — returns a
/// negative error code, which is not a language and falls through to the default like any other
/// value nobody translated into.
pub fn language() -> Lang {
    from_platform(raw())
}

/// The raw `TLanguage`, for a caller that wants to record what the phone actually said rather than
/// what we made of it. A probe wants this; an application does not.
pub fn raw() -> i32 {
    // SAFETY: no arguments, no pointers, and it cannot Leave — see shim/src/shim_locale.cpp, which
    // states the exemption rather than assuming it.
    unsafe { symbian_sys::shim_locale_language() }
}

/// The table, as a pure function so a host test can walk every entry that matters.
///
/// Separate from [`language`] for exactly that reason: the call cannot be tested off the phone and
/// the mapping is the part that would be wrong.
pub fn from_platform(code: i32) -> Lang {
    match code {
        PORTUGUESE | BRAZILIAN_PORTUGUESE => Lang::Pt,
        // Everything else, including English's eight values and every error code the shim can
        // return. Listing the English values would add eight lines that change no outcome and
        // would then need to grow with the platform.
        _ => Lang::En,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_portuguese_settings_are_portuguese() {
        // The one a handset here reports is 76, and it is the entry most likely to be dropped by
        // somebody who reads "Portuguese = 13" and stops there.
        assert_eq!(from_platform(BRAZILIAN_PORTUGUESE), Lang::Pt, "Brazil, ELangBrazilianPortuguese");
        assert_eq!(from_platform(PORTUGUESE), Lang::Pt, "Portugal, ELangPortuguese");
    }

    #[test]
    fn all_eight_englishes_are_english() {
        // ELangEnglish, ELangAmerican, ELangEnglish_Apac, and the five regional ones. They fall
        // through the catch-all rather than being listed, so this is what says the catch-all is
        // doing the job the table needs — and would fail if somebody replaced it with `1 => En`.
        for code in [1, 10, 129, 157, 158, 159, 160, 161] {
            assert_eq!(from_platform(code), Lang::En, "TLanguage {code}");
        }
    }

    #[test]
    fn a_language_nobody_translated_into_reads_as_english() {
        // ELangSpanish is 4. An untranslated interface is usable; a blank one is not.
        assert_eq!(from_platform(4), Lang::En, "Spanish");
        assert_eq!(from_platform(999), Lang::En, "a value this platform does not define");
    }

    #[test]
    fn a_shim_that_cannot_answer_is_not_a_language() {
        // The host stub returns SHIM_ERR_NOT_READY, and every shim error is negative. None of them
        // must ever land on Portuguese by arithmetic accident.
        for code in [symbian_sys::SHIM_ERR_NOT_READY, -1, -13, -76, i32::MIN] {
            assert_eq!(from_platform(code), Lang::En, "error code {code}");
        }
    }
}
