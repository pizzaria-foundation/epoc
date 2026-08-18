//! Finding links in plain text, and reading a scheme off one.
//!
//! # Why this is in the SDK and not in whoever needs it
//!
//! Two processes need it and they need different halves. The chat client scans message text to
//! decide what the D-pad can land on; the launcher reads the scheme off the result to decide which
//! application handles it. Put in either one, the other grows a second copy — and the two copies
//! would disagree about where a link ends, which shows up as a link that highlights one way and
//! opens another.
//!
//! Pure: no allocation beyond the returned ranges, no I/O, no platform. It runs and is tested on
//! the host.
//!
//! # Text, not entities
//!
//! Telegram sends message *entities* alongside message text — byte ranges the server already marked
//! as links, including the case where the visible text and the destination differ. This module does
//! not use them, because [`crate`]'s caller does not carry them: the message model here is a
//! `String` and nothing else. Scanning text is what works on the messages already in the cache, and
//! it needs nothing from the protocol.
//!
//! The cost is worth stating plainly, because it is a security property and not a rough edge: with
//! entities, `click here` can point anywhere, and text scanning cannot see that at all. It also
//! cannot see it *coming* — a text-scanned link always points where it reads. So this is the safe
//! half of the feature, and the day entities arrive, the destination has to become visible before
//! anything opens it.

use alloc::vec::Vec;
use core::ops::Range;

/// The prefixes that start a link, longest first.
///
/// Longest first matters: `http` is a prefix of `https`, and matching in declaration order would
/// find `http://` inside `https://` and stop the scheme one character early. The scan below relies
/// on this ordering rather than re-deriving it.
const PREFIXES: [&str; 5] = ["https://", "http://", "mailto:", "tg://", "www."];

/// Characters that end a link when they are the last thing in it.
///
/// Sentence punctuation, and closing brackets and quotes. `veja isto: https://exemplo.com.` does
/// not end at the full stop, and neither does `(https://exemplo.com)`. Trailing, not forbidden: a
/// full stop *inside* a URL is a hostname separator and a `)` inside is a real path character.
const TRAILING: [char; 10] = ['.', ',', ';', ':', '!', '?', ']', '}', '\'', '"'];

/// Every link in `text`, as byte ranges into it, in order and non-overlapping.
///
/// Ranges rather than substrings so the caller can map a link back to where it sits — the chat
/// transcript wraps text into byte ranges per line and has to intersect the two to underline a link
/// that spans a line break.
///
/// ```
/// # use symbian::url::find_links;
/// let t = "veja https://exemplo.com/a, e tg://resolve?d=x.";
/// let found: Vec<&str> = find_links(t).into_iter().map(|r| &t[r]).collect();
/// assert_eq!(found, ["https://exemplo.com/a", "tg://resolve?d=x"]);
/// ```
pub fn find_links(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut i = 0usize;

    while i < text.len() {
        // Only look at character boundaries; a multi-byte character's tail can never start a link
        // and slicing at one would panic.
        if !text.is_char_boundary(i) {
            i += 1;
            continue;
        }
        let rest = &text[i..];
        let Some(prefix) = PREFIXES.iter().find(|p| starts_with_ignore_case(rest, p)) else {
            i += 1;
            continue;
        };
        // A link starts at a word boundary. Without this, `shttp://x` matches from index 1 and the
        // user sees a link drawn over part of a word.
        if text[..i].chars().next_back().is_some_and(is_word_char) {
            i += 1;
            continue;
        }

        let end = scan_end(text, i + prefix.len());
        // A prefix with nothing after it is not a link, it is the word "www." at the end of a
        // sentence. Requiring a body is what keeps `mailto:` alone from becoming a target you can
        // focus and press.
        if end > i + prefix.len() {
            out.push(i..end);
            i = end;
        } else {
            i += prefix.len();
        }
    }
    out
}

/// The scheme of `url`, lowercase and without the separator, or `None`.
///
/// `www.exemplo.com` has no scheme and answers `None` rather than guessing `http` — guessing is the
/// caller's decision, and the launcher's registry is keyed on what was actually written.
///
/// ```
/// # use symbian::url::scheme_of;
/// assert_eq!(scheme_of("HTTPS://a.com").as_deref(), Some("https"));
/// assert_eq!(scheme_of("mailto:a@b.c").as_deref(), Some("mailto"));
/// assert_eq!(scheme_of("www.a.com"), None);
/// ```
pub fn scheme_of(url: &str) -> Option<alloc::string::String> {
    let colon = url.find(':')?;
    let scheme = &url[..colon];
    if scheme.is_empty() || !scheme.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.') {
        return None;
    }
    // A scheme must start with a letter — `4:30` is a time, not a link.
    if !scheme.chars().next()?.is_ascii_alphabetic() {
        return None;
    }
    Some(scheme.to_ascii_lowercase())
}

/// Where the link starting before `from` ends.
///
/// Runs to the first whitespace or control character, then walks back over trailing punctuation.
fn scan_end(text: &str, from: usize) -> usize {
    let mut end = from;
    for (off, ch) in text[from..].char_indices() {
        if ch.is_whitespace() || ch.is_control() {
            break;
        }
        end = from + off + ch.len_utf8();
    }
    trim_trailing(&text[..end], from)
}

/// Walk back over punctuation that belongs to the sentence rather than to the link.
///
/// `)` is handled separately from the rest, and it is the case that makes this function more than a
/// `trim_end_matches`. A closing bracket ends the link in `(veja https://exemplo.com)` and belongs
/// to it in `https://xn.wiki/Rust_(linguagem)` — so it survives only when there is an unmatched `(`
/// inside the link to pair it with. Wikipedia is the whole reason anyone notices.
fn trim_trailing(link: &str, floor: usize) -> usize {
    let mut end = link.len();
    while end > floor {
        let ch = link[..end].chars().next_back().unwrap_or(' ');
        let cut = if ch == ')' {
            let body = &link[floor..end - 1];
            body.matches('(').count() <= body.matches(')').count()
        } else {
            TRAILING.contains(&ch)
        };
        if !cut {
            break;
        }
        end -= ch.len_utf8();
    }
    end
}

/// ASCII-insensitive prefix test, without allocating a lowercase copy.
///
/// Schemes are case-insensitive per RFC 3986 and people do type `HTTP://`.
fn starts_with_ignore_case(hay: &str, needle: &str) -> bool {
    hay.len() >= needle.len()
        && hay.as_bytes()[..needle.len()].eq_ignore_ascii_case(needle.as_bytes())
}

/// Whether this character would make a link start mid-word.
///
/// The Unicode answer, not a byte test. The byte version — "any byte with the high bit set is part
/// of a word" — is cheaper and wrong in both directions at once: it blocks a link after `»`, `—` or
/// a curly quote, which are separators, while claiming to handle alphabets it never decodes. Its
/// own test caught it, on the `»https://a.com` case.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn links(t: &str) -> Vec<&str> {
        find_links(t).into_iter().map(|r| &t[r]).collect()
    }

    #[test]
    fn a_bare_link_is_the_whole_thing() {
        assert_eq!(links("https://exemplo.com/a/b?c=1&d=2"), ["https://exemplo.com/a/b?c=1&d=2"]);
        assert_eq!(links("http://a.com"), ["http://a.com"]);
        assert_eq!(links("tg://resolve?domain=x"), ["tg://resolve?domain=x"]);
        assert_eq!(links("mailto:alguem@exemplo.com"), ["mailto:alguem@exemplo.com"]);
        assert_eq!(links("www.exemplo.com"), ["www.exemplo.com"]);
    }

    #[test]
    fn a_link_at_the_end_of_a_sentence_does_not_eat_the_full_stop() {
        // The single most common case, and the one a naive "run to whitespace" gets wrong on every
        // message that ends with a link.
        assert_eq!(links("veja isto: https://exemplo.com."), ["https://exemplo.com"]);
        assert_eq!(links("é aqui https://exemplo.com/a, e mais"), ["https://exemplo.com/a"]);
        assert_eq!(links("sério?? https://exemplo.com!?"), ["https://exemplo.com"]);
    }

    #[test]
    fn a_bracket_that_belongs_to_the_link_survives() {
        // The pair of cases that makes trailing-punctuation trimming more than one line.
        assert_eq!(links("(veja https://exemplo.com)"), ["https://exemplo.com"]);
        assert_eq!(
            links("https://xn.wiki/Rust_(linguagem)"),
            ["https://xn.wiki/Rust_(linguagem)"],
            "an unmatched ( inside pairs with the trailing )"
        );
        assert_eq!(
            links("(https://xn.wiki/Rust_(linguagem))"),
            ["https://xn.wiki/Rust_(linguagem)"],
            "and the outer one still goes"
        );
    }

    #[test]
    fn two_links_on_one_line_are_two_links() {
        assert_eq!(
            links("a https://um.com b http://dois.com c"),
            ["https://um.com", "http://dois.com"]
        );
    }

    #[test]
    fn a_link_never_starts_in_the_middle_of_a_word() {
        // Without the boundary check these match from inside the word and the user gets a link
        // drawn over half of it.
        assert!(links("shttp://x.com").is_empty());
        assert!(links("xwww.a.com").is_empty());
        // But punctuation before it is a boundary, which is what makes the bracket case work.
        assert_eq!(links("(www.a.com"), ["www.a.com"]);
        assert_eq!(links("»https://a.com"), ["https://a.com"], "and so is a multi-byte separator");
    }

    #[test]
    fn a_scheme_with_nothing_after_it_is_not_a_link() {
        // Otherwise the word "www." at the end of a sentence becomes something the D-pad stops on
        // and Select opens.
        assert!(links("termina em www.").is_empty());
        assert!(links("mailto:").is_empty());
        assert!(links("http://").is_empty());
    }

    #[test]
    fn text_with_no_link_costs_nothing_and_finds_nothing() {
        assert!(links("uma mensagem comum, sem endereço nenhum").is_empty());
        assert!(links("").is_empty());
        assert!(links("são 4:30 e o preço é 10:1").is_empty(), "a colon is not a scheme");
    }

    #[test]
    fn the_scan_is_case_insensitive_because_people_type_caps() {
        assert_eq!(links("HTTPS://EXEMPLO.COM"), ["HTTPS://EXEMPLO.COM"]);
        assert_eq!(scheme_of("HTTPS://a").as_deref(), Some("https"));
    }

    #[test]
    fn multibyte_text_does_not_panic_and_does_not_shift_the_ranges() {
        // Byte ranges into text with accents are where an off-by-one becomes a panic rather than a
        // wrong highlight, so this asserts the slice as well as the content.
        let t = "olá, çéô https://exemplo.com — até";
        let r = find_links(t);
        assert_eq!(r.len(), 1);
        assert_eq!(&t[r[0].clone()], "https://exemplo.com");
    }

    #[test]
    fn ranges_are_ordered_and_do_not_overlap() {
        // What the caller relies on to walk them with a cursor.
        let t = "a https://um.com/x b www.dois.com c mailto:a@b.co";
        let r = find_links(t);
        assert_eq!(r.len(), 3);
        for w in r.windows(2) {
            assert!(w[0].end <= w[1].start, "{:?} overlaps {:?}", w[0], w[1]);
        }
    }

    #[test]
    fn the_scheme_is_what_was_written_and_nothing_is_guessed() {
        assert_eq!(scheme_of("http://a").as_deref(), Some("http"));
        assert_eq!(scheme_of("tg://resolve").as_deref(), Some("tg"));
        // No scheme is not "probably http": the registry is keyed on what the text says, and a
        // guess here would silently pick a handler the user never chose.
        assert_eq!(scheme_of("www.a.com"), None);
        assert_eq!(scheme_of("a.com"), None);
        assert_eq!(scheme_of("4:30"), None);
        assert_eq!(scheme_of(""), None);
        assert_eq!(scheme_of(":nada"), None);
    }

    #[test]
    fn every_prefix_in_the_table_is_actually_reachable() {
        // A table where a longer entry hides a shorter one is the bug this ordering exists to
        // prevent, and it would be invisible: `https://x` would still be found, as `http` plus a
        // stray `s`. Asserting the scheme is what catches it.
        assert_eq!(scheme_of(links("https://x.com")[0]).as_deref(), Some("https"));
        assert_eq!(scheme_of(links("http://x.com")[0]).as_deref(), Some("http"));
        for p in PREFIXES {
            let t = alloc::format!("{p}exemplo.com");
            assert_eq!(links(&t), vec![t.as_str()], "prefix {p:?} must be reachable");
        }
    }
}
