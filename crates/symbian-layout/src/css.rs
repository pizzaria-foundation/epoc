//! The little bit of CSS a page can state without a stylesheet.
//!
//! # Scope, and why it stops here
//!
//! Colours and a handful of declarations, read from **inline `style=` attributes** and from HTML's
//! presentational attributes (`bgcolor`, `color`, `width`). Nothing else: no selectors, no
//! specificity, no inheritance beyond what the tree already does, no `<style>` blocks.
//!
//! That boundary is deliberate. A real cascade — selector matching, specificity, `!important`, the
//! origin order between author, user and UA sheets — is what libcss does, and doing it twice is the
//! one thing the browser plan says not to do. What is here is the part that needs no cascade at all,
//! because an inline declaration has nowhere to conflict with: it is on the element, and it wins.
//!
//! # Why it is worth having before the real thing
//!
//! Because the alternative is not "no colours", it is "**our** colours". Rendering every page in the
//! phone's dark theme inverts the intent of a web written for white paper, and a page that declares
//! its own colour looked exactly as wrong as one that did not. This closes the gap for the
//! declarations that are already sitting in the markup.

use alloc::string::String;

use symbian_gfx::Color;

/// The default page canvas: what HTML means when it says nothing.
///
/// White paper and near-black ink, not the handset's theme. The web is authored against this
/// assumption, so it is the honest default for an unstyled document — a page rendered on a dark
/// canvas is not neutral, it is a page with its contrast inverted. The chrome around the page stays
/// themed; only the paper is the web's.
pub const PAPER: Color = Color::rgb(0xFF, 0xFF, 0xFF);
/// Near-black rather than black: pure black on pure white is harsher than any browser's default.
pub const INK: Color = Color::rgb(0x1A, 0x1A, 0x1A);
/// The blue every browser has used for thirty years. Recognisable is worth more than tasteful.
pub const LINK: Color = Color::rgb(0x00, 0x00, 0xEE);
/// Secondary text: captions, `<small>`, footers.
pub const DIM: Color = Color::rgb(0x60, 0x60, 0x60);

/// Parse a CSS colour. `None` for anything not understood, which leaves the inherited colour alone.
///
/// Four forms, because they are the four that appear: `#rgb`, `#rrggbb`, `rgb(r, g, b)` and the
/// named colours. Refusing the rest is not a gap to fill later — an unparsed colour keeps what the
/// element inherited, which is a readable page. Guessing would not be.
pub fn color(s: &str) -> Option<Color> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix('#') {
        return match hex.len() {
            3 => {
                let r = nib(hex.as_bytes()[0])?;
                let g = nib(hex.as_bytes()[1])?;
                let b = nib(hex.as_bytes()[2])?;
                // `#abc` means `#aabbcc`, not `#a0b0c0`: doubling the nibble is what keeps `#fff`
                // pure white instead of a slightly grey one.
                Some(Color::rgb(r * 17, g * 17, b * 17))
            }
            6 => {
                let b = hex.as_bytes();
                Some(Color::rgb(
                    nib(b[0])? * 16 + nib(b[1])?,
                    nib(b[2])? * 16 + nib(b[3])?,
                    nib(b[4])? * 16 + nib(b[5])?,
                ))
            }
            _ => None,
        };
    }
    if let Some(args) = s.strip_prefix("rgb(").and_then(|t| t.strip_suffix(')')) {
        let mut it = args.split(',');
        let r = byte(it.next()?)?;
        let g = byte(it.next()?)?;
        let b = byte(it.next()?)?;
        return Some(Color::rgb(r, g, b));
    }
    named(s)
}

fn nib(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn byte(s: &str) -> Option<u8> {
    let t = s.trim();
    // Percentages appear in `rgb(100%, 0%, 0%)`. Cheap to support and confusing to refuse.
    if let Some(p) = t.strip_suffix('%') {
        let v: u32 = p.trim().parse().ok()?;
        return Some((v.min(100) * 255 / 100) as u8);
    }
    let v: u32 = t.parse().ok()?;
    Some(v.min(255) as u8)
}

/// The named colours worth carrying.
///
/// The CSS list is 148 entries; these are the ones that appear in markup written by hand, which is
/// where inline styles come from. A name not here keeps the inherited colour.
fn named(s: &str) -> Option<Color> {
    let mut lower = String::with_capacity(s.len());
    for c in s.chars() {
        lower.push(c.to_ascii_lowercase());
    }
    let c = match lower.as_str() {
        "black" => (0x00, 0x00, 0x00),
        "white" => (0xFF, 0xFF, 0xFF),
        "red" => (0xFF, 0x00, 0x00),
        "green" => (0x00, 0x80, 0x00),
        "lime" => (0x00, 0xFF, 0x00),
        "blue" => (0x00, 0x00, 0xFF),
        "navy" => (0x00, 0x00, 0x80),
        "yellow" => (0xFF, 0xFF, 0x00),
        "orange" => (0xFF, 0xA5, 0x00),
        "purple" => (0x80, 0x00, 0x80),
        "gray" | "grey" => (0x80, 0x80, 0x80),
        "lightgray" | "lightgrey" => (0xD3, 0xD3, 0xD3),
        "darkgray" | "darkgrey" => (0xA9, 0xA9, 0xA9),
        "silver" => (0xC0, 0xC0, 0xC0),
        "maroon" => (0x80, 0x00, 0x00),
        "olive" => (0x80, 0x80, 0x00),
        "teal" => (0x00, 0x80, 0x80),
        "aqua" | "cyan" => (0x00, 0xFF, 0xFF),
        "fuchsia" | "magenta" => (0xFF, 0x00, 0xFF),
        "transparent" => return Some(Color::TRANSPARENT),
        _ => return None,
    };
    Some(Color::rgb(c.0, c.1, c.2))
}

/// One declaration out of a `style=` attribute.
///
/// Splits on `;` and `:` and trims. Does not handle a `;` inside a quoted value or a `url(...)`,
/// which appear in `background-image` and `content` — both of which this ignores anyway, so the
/// failure is a declaration skipped rather than a value mangled.
pub fn declarations(style: &str) -> impl Iterator<Item = (&str, &str)> {
    style.split(';').filter_map(|d| {
        let (k, v) = d.split_once(':')?;
        let k = k.trim();
        let v = v.trim();
        if k.is_empty() || v.is_empty() {
            None
        } else {
            Some((k, v))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    #[test]
    fn hex_colours_parse() {
        assert_eq!(color("#000000"), Some(Color::rgb(0, 0, 0)));
        assert_eq!(color("#FFFFFF"), Some(Color::rgb(255, 255, 255)));
        assert_eq!(color("#ff8000"), Some(Color::rgb(0xFF, 0x80, 0x00)));
        assert_eq!(color("  #123456  "), Some(Color::rgb(0x12, 0x34, 0x56)));
    }

    /// `#fff` is white, not a slightly grey white. Doubling the nibble is the rule; multiplying by
    /// 16 is the bug.
    #[test]
    fn short_hex_doubles_the_nibble() {
        assert_eq!(color("#fff"), Some(Color::rgb(255, 255, 255)));
        assert_eq!(color("#000"), Some(Color::rgb(0, 0, 0)));
        assert_eq!(color("#abc"), Some(Color::rgb(0xAA, 0xBB, 0xCC)));
    }

    #[test]
    fn rgb_functions_parse_including_percentages() {
        assert_eq!(color("rgb(1,2,3)"), Some(Color::rgb(1, 2, 3)));
        assert_eq!(color("rgb( 10 , 20 , 30 )"), Some(Color::rgb(10, 20, 30)));
        assert_eq!(color("rgb(100%, 0%, 50%)"), Some(Color::rgb(255, 0, 127)));
        assert_eq!(color("rgb(999,0,0)"), Some(Color::rgb(255, 0, 0)), "clamped, not wrapped");
    }

    #[test]
    fn names_parse_case_insensitively() {
        assert_eq!(color("red"), Some(Color::rgb(255, 0, 0)));
        assert_eq!(color("RED"), Some(Color::rgb(255, 0, 0)));
        assert_eq!(color("Grey"), color("gray"));
        assert_eq!(color("transparent"), Some(Color::TRANSPARENT));
    }

    /// An unparsed colour must be `None`, so the caller keeps what the element inherited. Returning
    /// a default here would repaint a readable page in a colour nobody chose.
    #[test]
    fn what_is_not_understood_is_refused() {
        assert_eq!(color(""), None);
        assert_eq!(color("#12345"), None);
        assert_eq!(color("rebeccapurple"), None);
        assert_eq!(color("var(--brand)"), None);
        assert_eq!(color("linear-gradient(red, blue)"), None);
        assert_eq!(color("#gg0000"), None);
    }

    #[test]
    fn declarations_split_and_trim() {
        let got: Vec<(&str, &str)> = declarations("color: red; background : #fff ;").collect();
        assert_eq!(got, [("color", "red"), ("background", "#fff")]);
    }

    #[test]
    fn malformed_declarations_are_skipped_not_guessed() {
        let got: Vec<(&str, &str)> = declarations("color; :red; a:; b:2").collect();
        assert_eq!(got, [("b", "2")]);
    }

    /// The paper is the web's default, not the handset's theme. Asserted because it is a product
    /// decision and a later refactor could quietly re-theme it.
    #[test]
    fn the_page_canvas_is_white_paper_and_dark_ink() {
        assert_eq!(PAPER, Color::rgb(0xFF, 0xFF, 0xFF));
        assert!(INK.r() < 0x40 && INK.g() < 0x40 && INK.b() < 0x40);
        assert!(LINK.b() > LINK.r() && LINK.b() > LINK.g(), "links are blue");
    }
}
