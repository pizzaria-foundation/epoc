//! A JSON reader that is exactly big enough to read a GitHub release.
//!
//! It exists because the package manager's repositories are GitHub Releases and nothing in this
//! workspace could read JSON. It is deliberately not a general-purpose library: no serialisation, no
//! borrowing tricks, no number tower. It parses a document into a tree and lets a caller walk it.
//!
//! ## Bounded, because the input is somebody else's
//!
//! A release payload arrives over the network from a service this phone does not control. Every
//! limit here is a limit on *that*, and the measurement behind them is in `docs/device-notes.md`: a
//! `/releases/latest` for `rust-lang/rust` is 3433 bytes on the wire and **10012 decoded**. So the
//! document fits in memory comfortably and this can be a tree rather than a stream — a decision made
//! from a number rather than from taste.
//!
//! - [`MAX_DEPTH`] bounds nesting, so a document of ten thousand `[` cannot recurse this into the
//!   stack. It is checked on the way *in*, not discovered on the way out.
//! - The caller bounds the byte length by choosing what to hand over. `Body::decode_to` already
//!   forces that decision one layer up.
//! - Every failure names a byte offset, because "the JSON was bad" is not something anybody can act
//!   on and `at 4172` is.
//!
//! ## Numbers
//!
//! Integers are kept as `i64` and only what looks like a real is parsed as `f64`. GitHub's release
//! payload has no reals in any field this project reads — `size` and `id` are integers — and keeping
//! them integral means an asset size survives the trip exactly. A `f64` would be fine up to 2^53 and
//! is still there for a document that needs it; the point is that the common case does not go
//! through it.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

/// How deep nesting may go. The path to an asset's download URL is four levels; a document claiming
/// hundreds is either not what we asked for or is trying to make us recurse.
pub const MAX_DEPTH: u8 = 32;

/// A parsed document.
///
/// Objects keep their pairs in a `Vec` rather than a map: a release object has a few dozen keys, a
/// linear scan over that is faster than hashing on this CPU, and it keeps the order the server sent —
/// which matters the day somebody has to compare a payload against a log.
#[derive(Clone, PartialEq, Debug)]
pub enum Json {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

/// Why a document was refused. Every variant carries the byte offset where the parser stopped.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct JsonError {
    pub kind: ErrorKind,
    /// Byte offset into the input. Not a line and column: the payload is one line.
    pub at: usize,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum ErrorKind {
    /// Ran out of input in the middle of a value.
    Truncated,
    /// A byte that cannot start or continue a value here.
    Unexpected,
    /// Nesting past [`MAX_DEPTH`].
    TooDeep,
    /// A `\u` escape that is not four hex digits, or a lone surrogate.
    BadEscape,
    /// A number the parser could not make sense of.
    BadNumber,
    /// Valid JSON, and then more bytes after it.
    Trailing,
}

impl JsonError {
    fn new(kind: ErrorKind, at: usize) -> Self {
        Self { kind, at }
    }
}

/// Parse a whole document. Trailing bytes other than whitespace are an error, not something to
/// ignore: a truncated response that happens to end on a valid value would otherwise parse as a
/// shorter document and be believed.
pub fn parse(bytes: &[u8]) -> Result<Json, JsonError> {
    let mut p = Parser { b: bytes, i: 0 };
    let v = p.value(0)?;
    p.spaces();
    if p.i < p.b.len() {
        return Err(JsonError::new(ErrorKind::Trailing, p.i));
    }
    Ok(v)
}

impl Json {
    /// The value at `key`, for an object. `None` for anything else, which is what lets a caller
    /// chain without checking each step.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The `i`th element, for an array.
    pub fn at(&self, i: usize) -> Option<&Json> {
        match self {
            Json::Arr(v) => v.get(i),
            _ => None,
        }
    }

    /// The elements, for an array; empty for anything else. A caller iterating assets should not
    /// have to decide what a missing `assets` key means twice.
    pub fn items(&self) -> &[Json] {
        match self {
            Json::Arr(v) => v,
            _ => &[],
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    /// An integer, and only from an integer. A real is not silently truncated — a size of `4.7` is a
    /// payload to distrust, not to round.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Json::Int(n) => Some(*n),
            _ => None,
        }
    }

    /// A non-negative integer, which is what a size or a count is.
    pub fn as_u64(&self) -> Option<u64> {
        self.as_i64().filter(|n| *n >= 0).map(|n| n as u64)
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Json::Null)
    }
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Parser<'a> {
    fn spaces(&mut self) {
        while let Some(c) = self.b.get(self.i) {
            match c {
                b' ' | b'\t' | b'\n' | b'\r' => self.i += 1,
                _ => return,
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn err(&self, kind: ErrorKind) -> JsonError {
        JsonError::new(kind, self.i)
    }

    fn lit(&mut self, word: &[u8]) -> bool {
        if self.b[self.i..].starts_with(word) {
            self.i += word.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: u8) -> Result<Json, JsonError> {
        // Checked before descending rather than after: the point of a depth limit is to not have
        // recursed yet.
        if depth > MAX_DEPTH {
            return Err(self.err(ErrorKind::TooDeep));
        }
        self.spaces();
        let Some(c) = self.peek() else { return Err(self.err(ErrorKind::Truncated)) };
        match c {
            b'{' => self.object(depth),
            b'[' => self.array(depth),
            b'"' => self.string().map(Json::Str),
            b't' if self.lit(b"true") => Ok(Json::Bool(true)),
            b'f' if self.lit(b"false") => Ok(Json::Bool(false)),
            b'n' if self.lit(b"null") => Ok(Json::Null),
            b'-' | b'0'..=b'9' => self.number(),
            _ => Err(self.err(ErrorKind::Unexpected)),
        }
    }

    fn object(&mut self, depth: u8) -> Result<Json, JsonError> {
        self.i += 1; // '{'
        let mut pairs = Vec::new();
        self.spaces();
        if self.peek() == Some(b'}') {
            self.i += 1;
            return Ok(Json::Obj(pairs));
        }
        loop {
            self.spaces();
            if self.peek() != Some(b'"') {
                return Err(self.err(ErrorKind::Unexpected));
            }
            let key = self.string()?;
            self.spaces();
            if self.peek() != Some(b':') {
                return Err(self.err(ErrorKind::Unexpected));
            }
            self.i += 1;
            let v = self.value(depth + 1)?;
            pairs.push((key, v));
            self.spaces();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Json::Obj(pairs));
                }
                Some(_) => return Err(self.err(ErrorKind::Unexpected)),
                None => return Err(self.err(ErrorKind::Truncated)),
            }
        }
    }

    fn array(&mut self, depth: u8) -> Result<Json, JsonError> {
        self.i += 1; // '['
        let mut out = Vec::new();
        self.spaces();
        if self.peek() == Some(b']') {
            self.i += 1;
            return Ok(Json::Arr(out));
        }
        loop {
            out.push(self.value(depth + 1)?);
            self.spaces();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b']') => {
                    self.i += 1;
                    return Ok(Json::Arr(out));
                }
                Some(_) => return Err(self.err(ErrorKind::Unexpected)),
                None => return Err(self.err(ErrorKind::Truncated)),
            }
        }
    }

    /// A string, with the escapes unescaped.
    ///
    /// `\uXXXX` is decoded, including a surrogate pair — GitHub release notes carry emoji, and a
    /// pair read as two lone surrogates would produce two replacement characters where one glyph
    /// belongs. A lone surrogate is refused rather than replaced: it is a payload that is not what it
    /// says it is, and this project's other codecs refuse rather than guess.
    fn string(&mut self) -> Result<String, JsonError> {
        self.i += 1; // '"'
        let mut out = String::new();
        loop {
            let Some(c) = self.peek() else { return Err(self.err(ErrorKind::Truncated)) };
            self.i += 1;
            match c {
                b'"' => return Ok(out),
                b'\\' => {
                    let Some(e) = self.peek() else { return Err(self.err(ErrorKind::Truncated)) };
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => out.push(self.unicode()?),
                        _ => return Err(self.err(ErrorKind::BadEscape)),
                    }
                }
                // Raw UTF-8 passes through a byte at a time; the collected bytes are validated as a
                // whole below. A control byte inside a string is malformed JSON, but accepting it
                // costs nothing and refusing it has cost real parsers real payloads.
                _ => out.push_str(&self.raw_utf8(c)),
            }
        }
    }

    /// One source byte into the output, decoding UTF-8 continuation bytes with it.
    fn raw_utf8(&mut self, first: u8) -> String {
        let extra = match first {
            0x00..=0x7F => 0,
            0xC0..=0xDF => 1,
            0xE0..=0xEF => 2,
            0xF0..=0xF7 => 3,
            // A continuation byte with no lead, or an invalid lead. Not worth refusing a whole
            // document over a byte inside a release note nobody reads.
            _ => return String::from(char::REPLACEMENT_CHARACTER),
        };
        let end = (self.i + extra).min(self.b.len());
        let mut buf = Vec::with_capacity(extra + 1);
        buf.push(first);
        buf.extend_from_slice(&self.b[self.i..end]);
        self.i = end;
        match core::str::from_utf8(&buf) {
            Ok(s) => String::from(s),
            Err(_) => String::from(char::REPLACEMENT_CHARACTER),
        }
    }

    fn hex4(&mut self) -> Result<u16, JsonError> {
        if self.i + 4 > self.b.len() {
            return Err(self.err(ErrorKind::Truncated));
        }
        let mut v: u16 = 0;
        for k in 0..4 {
            let d = (self.b[self.i + k] as char)
                .to_digit(16)
                .ok_or(JsonError::new(ErrorKind::BadEscape, self.i + k))?;
            v = v * 16 + d as u16;
        }
        self.i += 4;
        Ok(v)
    }

    fn unicode(&mut self) -> Result<char, JsonError> {
        let hi = self.hex4()?;
        // Not a surrogate: one unit, one char.
        if !(0xD800..0xE000).contains(&hi) {
            return char::from_u32(hi as u32).ok_or(self.err(ErrorKind::BadEscape));
        }
        // A high surrogate must be followed by `\uXXXX` holding its low half.
        if hi >= 0xDC00 {
            return Err(self.err(ErrorKind::BadEscape));
        }
        if !self.lit(b"\\u") {
            return Err(self.err(ErrorKind::BadEscape));
        }
        let lo = self.hex4()?;
        if !(0xDC00..0xE000).contains(&lo) {
            return Err(self.err(ErrorKind::BadEscape));
        }
        let c = 0x1_0000 + ((hi as u32 - 0xD800) << 10) + (lo as u32 - 0xDC00);
        char::from_u32(c).ok_or(self.err(ErrorKind::BadEscape))
    }

    /// A number. Integral unless it carries a `.` or an exponent — see the module docs for why that
    /// distinction is kept.
    fn number(&mut self) -> Result<Json, JsonError> {
        let start = self.i;
        let mut real = false;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        while let Some(c) = self.peek() {
            match c {
                b'0'..=b'9' => self.i += 1,
                b'.' | b'e' | b'E' => {
                    real = true;
                    self.i += 1;
                }
                b'+' | b'-' if real => self.i += 1,
                _ => break,
            }
        }
        let text = core::str::from_utf8(&self.b[start..self.i])
            .map_err(|_| JsonError::new(ErrorKind::BadNumber, start))?;
        if real {
            text.parse::<f64>()
                .map(Json::Float)
                .map_err(|_| JsonError::new(ErrorKind::BadNumber, start))
        } else {
            text.parse::<i64>()
                .map(Json::Int)
                // An integer too big for i64 is not a number this project can act on, and silently
                // becoming a float would make a size comparison quietly approximate.
                .map_err(|_| JsonError::new(ErrorKind::BadNumber, start))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;

    #[test]
    fn the_shapes_a_release_payload_is_made_of() {
        let v = parse(br#"{"tag_name":"v0.2.0","draft":false,"id":373931400,"assets":[]}"#).unwrap();
        assert_eq!(v.get("tag_name").unwrap().as_str(), Some("v0.2.0"));
        assert_eq!(v.get("draft").unwrap().as_bool(), Some(false));
        assert_eq!(v.get("id").unwrap().as_i64(), Some(373_931_400));
        assert!(v.get("assets").unwrap().items().is_empty());
        assert!(v.get("nothing").is_none(), "a missing key is None, not an error");
    }

    #[test]
    fn nesting_and_walking() {
        let v = parse(
            br#"{"assets":[{"name":"launcher.sisx","size":320484,
                 "browser_download_url":"https://example/launcher.sisx"}]}"#,
        )
        .unwrap();
        let a = &v.get("assets").unwrap().items()[0];
        assert_eq!(a.get("name").unwrap().as_str(), Some("launcher.sisx"));
        assert_eq!(a.get("size").unwrap().as_u64(), Some(320_484));
        assert!(a.get("browser_download_url").unwrap().as_str().unwrap().starts_with("https://"));
    }

    #[test]
    fn escapes_including_a_surrogate_pair() {
        // Release notes carry emoji, and a pair read as two lone surrogates would put two
        // replacement characters where one glyph belongs.
        let src = r#"{"body":"line\nquote \" slash \\ tab\t \u00e9 \ud83d\ude80"}"#;
        let v = parse(src.as_bytes()).unwrap();
        assert_eq!(v.get("body").unwrap().as_str(), Some("line\nquote \" slash \\ tab\t é 🚀"));
    }

    #[test]
    fn a_lone_surrogate_is_refused_rather_than_replaced() {
        // It is a payload that is not what it says it is, and this project's other codecs refuse
        // rather than guess.
        let e = parse(br#"{"s":"\ud83d"}"#).unwrap_err();
        assert_eq!(e.kind, ErrorKind::BadEscape);
        assert!(parse(br#"{"s":"\udc00 alone"}"#).is_err());
        assert!(parse(br#"{"s":"\u00zz"}"#).is_err());
        assert!(parse(br#"{"s":"\q"}"#).is_err());
    }

    #[test]
    fn integers_stay_integral_and_reals_stay_real() {
        // An asset size has to survive the trip exactly; going through f64 would make a comparison
        // quietly approximate.
        assert_eq!(parse(b"9007199254740993").unwrap().as_i64(), Some(9_007_199_254_740_993));
        assert_eq!(parse(b"-42").unwrap().as_i64(), Some(-42));
        assert_eq!(parse(b"1.5").unwrap(), Json::Float(1.5));
        assert_eq!(parse(b"2e3").unwrap(), Json::Float(2000.0));
        assert_eq!(parse(b"1.5").unwrap().as_i64(), None, "a real is not truncated into an int");
        assert_eq!(parse(b"-0.0").unwrap().as_u64(), None);
    }

    #[test]
    fn an_integer_too_big_for_i64_is_refused_not_rounded() {
        let e = parse(b"99999999999999999999999").unwrap_err();
        assert_eq!(e.kind, ErrorKind::BadNumber);
    }

    #[test]
    fn truncation_anywhere_is_an_error_with_an_offset() {
        let whole = br#"{"a":[1,{"b":"c"}],"d":true}"#;
        for cut in 1..whole.len() {
            let e = parse(&whole[..cut]).expect_err("a cut document must not parse");
            assert!(e.at <= cut, "offset {} is past the input {cut}", e.at);
        }
    }

    #[test]
    fn valid_json_followed_by_rubbish_is_refused() {
        // A truncated response that happens to end on a valid value would otherwise parse as a
        // shorter document and be believed.
        let e = parse(br#"{"a":1} {"b":2}"#).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Trailing);
        assert_eq!(parse(b"  {}  ").unwrap(), Json::Obj(alloc::vec![]), "trailing space is fine");
    }

    #[test]
    fn nesting_past_the_limit_is_refused_before_it_recurses() {
        let deep = format!("{}1{}", "[".repeat(200), "]".repeat(200));
        let e = parse(deep.as_bytes()).unwrap_err();
        assert_eq!(e.kind, ErrorKind::TooDeep);

        // And just inside the limit still works, so the bound is a bound and not a wall.
        let ok = format!("{}1{}", "[".repeat(MAX_DEPTH as usize), "]".repeat(MAX_DEPTH as usize));
        assert!(parse(ok.as_bytes()).is_ok());
    }

    #[test]
    fn rubbish_is_refused_at_the_byte_it_went_wrong() {
        assert_eq!(parse(b"").unwrap_err().kind, ErrorKind::Truncated);
        assert_eq!(parse(b"tru").unwrap_err().kind, ErrorKind::Unexpected);
        assert_eq!(parse(br#"{"a" 1}"#).unwrap_err().kind, ErrorKind::Unexpected);
        assert_eq!(parse(br#"{a:1}"#).unwrap_err().kind, ErrorKind::Unexpected);
        assert_eq!(parse(br#"[1,,2]"#).unwrap_err().kind, ErrorKind::Unexpected);
        // The offset is the useful half: "the JSON was bad" is not actionable and "at 4" is.
        assert_eq!(parse(br#"[1,]"#).unwrap_err().at, 3);
    }

    #[test]
    fn empty_containers_and_nulls() {
        assert_eq!(parse(b"{}").unwrap(), Json::Obj(alloc::vec![]));
        assert_eq!(parse(b"[]").unwrap(), Json::Arr(alloc::vec![]));
        assert!(parse(b"null").unwrap().is_null());
        let v = parse(br#"{"body":null,"assets":[]}"#).unwrap();
        assert!(v.get("body").unwrap().is_null(), "GitHub sends null bodies");
    }

    #[test]
    fn a_duplicate_key_keeps_the_first_and_both_are_still_there() {
        // Order is preserved on purpose, which matters the day a payload has to be compared against
        // a log. `get` answering with the first is the same choice every JSON reader makes.
        let v = parse(br#"{"a":1,"a":2}"#).unwrap();
        assert_eq!(v.get("a").unwrap().as_i64(), Some(1));
        assert!(matches!(&v, Json::Obj(p) if p.len() == 2));
    }

    /// A release payload this handset actually fetched, saved by `apps/ghprobe` and pulled off the
    /// phone. `rust-lang/rust` 1.98.0: 10012 bytes, 21 keys, **zero assets**, and an 8014-byte body
    /// of release notes — four fifths of the payload is prose full of escapes and punctuation.
    const REAL_NOTES: &[u8] = include_bytes!("../tests/ghprobe.json");
    /// The same, for `BurntSushi/ripgrep` 15.2.0: 50 KB and **28 assets**, which is what a release
    /// that actually ships binaries looks like.
    const REAL_ASSETS: &[u8] = include_bytes!("../tests/release_assets.json");

    #[test]
    fn a_payload_this_phone_really_fetched() {
        // The point of a real fixture: one written by the same person who writes the parser proves
        // only that the two agree with each other. This one has an author block, node_ids, an 8 KB
        // body of markdown, and every field nobody asked for.
        let v = parse(REAL_NOTES).expect("a payload the handset fetched");
        assert_eq!(v.get("tag_name").unwrap().as_str(), Some("1.98.0"));
        assert_eq!(v.get("draft").unwrap().as_bool(), Some(false));
        assert_eq!(v.get("prerelease").unwrap().as_bool(), Some(false));
        assert!(v.get("assets").unwrap().items().is_empty(), "this release ships none");
        assert!(v.get("body").unwrap().as_str().unwrap().len() > 8_000);
        assert!(v.get("author").unwrap().get("login").is_some(), "nested objects walk");
    }

    #[test]
    fn a_release_that_actually_ships_binaries() {
        let v = parse(REAL_ASSETS).expect("a payload the handset fetched");
        assert_eq!(v.get("tag_name").unwrap().as_str(), Some("15.2.0"));
        let assets = v.get("assets").unwrap().items();
        assert_eq!(assets.len(), 28);

        let first = &assets[0];
        assert!(first.get("name").unwrap().as_str().unwrap().starts_with("ripgrep-"));
        assert!(first.get("size").unwrap().as_u64().unwrap() > 1_000_000);
        assert!(first
            .get("browser_download_url")
            .unwrap()
            .as_str()
            .unwrap()
            .starts_with("https://github.com/"));

        // Real releases carry sidecars. Whatever picks an asset has to cope with more than one
        // candidate per platform, which is why a name filter is not optional.
        let sha_sidecars =
            assets.iter().filter(|a| a.get("name").unwrap().as_str().unwrap().ends_with(".sha256"));
        assert!(sha_sidecars.count() > 0);
    }

    #[test]
    fn utf8_in_a_string_survives_without_an_escape() {
        let v = parse("{\"name\":\"Calendário 🚀\"}".as_bytes()).unwrap();
        assert_eq!(v.get("name").unwrap().as_str(), Some("Calendário 🚀"));
    }
}
