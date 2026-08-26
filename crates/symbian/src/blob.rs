//! Length-prefixed fields, for the small blobs this project keeps in the data cage.
//!
//! Every stored record here has the same shape — a few integers and a few strings, written once and
//! read back on the next run — and the same two hazards: a device that loses power mid-write, and a
//! format that has to survive the next version of the code. This is the encoding both are answered
//! with, in one place.
//!
//! # Why length-prefixed and not delimited
//!
//! Because the strings are URLs. A URL can contain anything, and a delimiter is a parser waiting to
//! be confused by content — a title with a newline in it, an address with a NUL. A length says
//! exactly how far the field runs and nothing inside it can lie about that.
//!
//! # Why every read returns `Option`
//!
//! So that decoding a record is a chain of `?` and a truncated file is `None` rather than a panic.
//! A cage file is ours, but it lives on a phone whose battery comes out — a half-written record is
//! an ordinary thing to find, not an error condition worth a type. The house rule this follows is
//! `handlers.rs`: **a truncated or corrupt tail is dropped rather than refused.**
//!
//! Lifted out of `http::cache`, which had it first and privately. Two copies of an encoding drift
//! the first time either changes, and the symptom is a file that reads back as absent.

use alloc::string::String;
use alloc::vec::Vec;

/// Append a string with a 16-bit length in front of it.
///
/// Longer than 64 KiB is clamped rather than refused: the callers store URLs and titles, where the
/// ceiling is already far past anything real, and losing the tail of an absurd title is better than
/// losing the record it belongs to.
pub fn put_str(out: &mut Vec<u8>, s: &str) {
    let n = s.len().min(u16::MAX as usize);
    // Clamped on a char boundary, so what comes back is still UTF-8. Truncating mid-sequence would
    // make the *reader* reject a record this side thought it had written.
    let n = floor_boundary(s, n);
    out.extend_from_slice(&(n as u16).to_le_bytes());
    out.extend_from_slice(&s.as_bytes()[..n]);
}

/// The largest `i <= n` that is a character boundary in `s`.
fn floor_boundary(s: &str, n: usize) -> usize {
    let mut i = n.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// A cursor over a blob, borrowing from it.
pub struct Reader<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { b: bytes, p: 0 }
    }

    /// How many bytes are left, for a caller deciding whether another record could fit.
    pub fn remaining(&self) -> usize {
        self.b.len().saturating_sub(self.p)
    }

    pub fn at_end(&self) -> bool {
        self.remaining() == 0
    }

    pub fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.p.checked_add(n)?;
        if end > self.b.len() {
            return None;
        }
        let s = &self.b[self.p..end];
        self.p = end;
        Some(s)
    }

    pub fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    pub fn u16(&mut self) -> Option<u16> {
        let s = self.take(2)?;
        Some(u16::from_le_bytes([s[0], s[1]]))
    }

    pub fn u32(&mut self) -> Option<u32> {
        let s = self.take(4)?;
        Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }

    pub fn i32(&mut self) -> Option<i32> {
        let s = self.take(4)?;
        Some(i32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }

    /// A length-prefixed string, borrowed.
    ///
    /// `None` for a length that runs off the end **or** for bytes that are not UTF-8 — a damaged
    /// record rather than something to salvage.
    pub fn str(&mut self) -> Option<&'a str> {
        let n = self.u16()? as usize;
        let s = self.take(n)?;
        core::str::from_utf8(s).ok()
    }

    /// The same, owned, for a decoder that keeps its records.
    pub fn string(&mut self) -> Option<String> {
        self.str().map(String::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_string_round_trips() {
        let mut out = Vec::new();
        put_str(&mut out, "https://example.com/a?b=c");
        let mut r = Reader::new(&out);
        assert_eq!(r.str(), Some("https://example.com/a?b=c"));
        assert!(r.at_end());
    }

    #[test]
    fn an_empty_string_is_a_record_not_a_gap() {
        let mut out = Vec::new();
        put_str(&mut out, "");
        put_str(&mut out, "after");
        let mut r = Reader::new(&out);
        assert_eq!(r.str(), Some(""));
        assert_eq!(r.str(), Some("after"), "the empty one did not swallow the next");
    }

    #[test]
    fn a_truncated_field_is_none_not_a_panic() {
        let mut out = Vec::new();
        put_str(&mut out, "hello");
        out.truncate(out.len() - 2); // battery out mid-write
        assert_eq!(Reader::new(&out).str(), None);
    }

    #[test]
    fn a_length_running_off_the_end_is_none() {
        // A length field that claims more than the file holds — the shape a corrupt byte takes.
        let bytes = [0xff, 0xff, b'a', b'b'];
        assert_eq!(Reader::new(&bytes).str(), None);
    }

    #[test]
    fn non_utf8_is_a_damaged_record() {
        let mut out = Vec::new();
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&[0xff, 0xfe]);
        assert_eq!(Reader::new(&out).str(), None);
    }

    #[test]
    fn a_clamped_string_is_still_utf8() {
        // Multi-byte characters exactly across the 64 KiB clamp: cutting mid-sequence would make
        // the reader reject what the writer thought it wrote.
        let s: String = "é".repeat(40_000);
        let mut out = Vec::new();
        put_str(&mut out, &s);
        let back = Reader::new(&out).str();
        assert!(back.is_some(), "clamped on a boundary, so it still parses");
        assert!(back.unwrap().len() <= u16::MAX as usize);
    }

    #[test]
    fn integers_round_trip_in_order() {
        let mut out = Vec::new();
        out.extend_from_slice(&7u16.to_le_bytes());
        out.extend_from_slice(&9u32.to_le_bytes());
        out.extend_from_slice(&(-3i32).to_le_bytes());
        let mut r = Reader::new(&out);
        assert_eq!(r.u16(), Some(7));
        assert_eq!(r.u32(), Some(9));
        assert_eq!(r.i32(), Some(-3));
        assert!(r.at_end());
    }
}
