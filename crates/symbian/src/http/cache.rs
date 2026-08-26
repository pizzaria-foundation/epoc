//! A fetched response, kept on disk.
//!
//! # What this is, and the thing it deliberately is not
//!
//! It is **not** an HTTP cache yet, and calling it one would be the bug. [`crate::cache`] states its
//! own contract plainly: the key is an id whose bytes never change, and "a mutable key wants an
//! expiry this deliberately does not have". A URL is the mutable key par excellence — the whole
//! point of a web page is that it changes — so storing one under that contract and serving hits from
//! it would hand the reader yesterday's page with no way to ask for today's. A browser that does
//! that is worse than one with no cache at all, because the staleness is invisible.
//!
//! So what this stores is a **snapshot**: this response, as it came off the wire, at a moment. It is
//! the right shape for the three things that actually want it —
//!
//! - Back navigation, where the user is asking for the page they just had;
//! - reading offline, which is the Opera feature the plan calls "salvar página";
//! - and not paying twice for a body on a link metered by the kilobyte.
//!
//! What it must not do is answer an ordinary navigation without asking the server. Making that safe
//! needs validators — `ETag` and `Last-Modified` out, `If-None-Match` and `If-Modified-Since` back
//! in, and a 304 understood — which is the next step and needs the shim to read and set headers it
//! currently does not. The [`Entry::validators`] fields exist and are written when known, so that
//! step is a revalidation path rather than a format change.
//!
//! # Compressed, not decoded
//!
//! What goes to disk is the body exactly as the server sent it, with the flags that describe it.
//! Measured on this handset, one page was 295 KB compressed and 1.3 MB inflated: storing the
//! decoded form would cost four times the flash and throw away the only copy that can be checked
//! against its own CRC.
//!
//! # One file, not two
//!
//! Metadata and body share a blob, because [`crate::fs::write_atomic`] then makes the pair atomic
//! for free. Two files would introduce a state nobody handles — a header whose body never landed,
//! read back later as a complete response.

use alloc::vec::Vec;

use symbian_crypto::sha256::sha256;

use super::Flags;
use crate::blob::{put_str, Reader};
use crate::error::{Error, Result};
use crate::fs::Fs;

/// `HC` and a format version. A stored blob whose magic does not match is not read, which is what
/// lets the format change without a migration: old entries become misses.
const MAGIC: [u8; 4] = *b"HC\x01\x00";

/// The prefix these take in the data cage, keeping them apart from the photo cache sharing it.
const PREFIX: char = 'h';

/// The largest response this will store, compressed.
///
/// Its own limit rather than [`crate::cache`]'s, because this bypasses that module's `put` to write
/// its own blob — so inheriting the bound was not automatic, and a page is a different size from a
/// photo. Two megabytes covers the largest measured page several times over (295 KB compressed) and
/// still means no single entry can take a noticeable bite out of a 250 MB phone shared with every
/// other application.
pub const MAX_ENTRY: usize = 2 * 1024 * 1024;

/// A response to store or one just read back.
///
/// The body is **borrowed**, and that is the whole shape of this module rather than a detail. The
/// first version owned it, and the device answered: after the largest page in the list (295 KB
/// compressed) the run died on the next one, because storing a response meant five copies of the
/// body alive at once — one to own it, one to build the blob, one to read the file, one to decode,
/// one to feed the inflater. Two megabytes of transient allocation and the fragmentation it leaves
/// behind, on a 4 MB heap, for a page already in memory.
///
/// So nothing here copies a body. Writing borrows the caller's; reading borrows the buffer the
/// caller read the file into.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Ref<'a> {
    /// Where the bytes came from — the effective URL, after redirects. This is what a relative link
    /// in the body resolves against, so losing it would make a restored page's links wrong.
    pub url: &'a str,
    pub status: u16,
    /// The flags the response arrived with, so [`super::Body::decode_to`] can be told the truth
    /// about bytes it is seeing for the second time.
    pub flags: Flags,
    /// `ETag`, when the response carried one. Empty otherwise.
    pub etag: &'a str,
    /// `Last-Modified`, when the response carried one. Empty otherwise.
    pub last_modified: &'a str,
    /// The body as it came off the wire, still compressed if it arrived that way.
    pub body: &'a [u8],
}

impl<'a> Ref<'a> {
    /// Whether this entry could be revalidated with a conditional request.
    ///
    /// False means the only honest options are refetching or showing it as a snapshot the user
    /// asked for — never serving it as though it were current.
    pub fn has_validator(&self) -> bool {
        !self.etag.is_empty() || !self.last_modified.is_empty()
    }
}

/// The cage key for a URL: the first eight bytes of its SHA-256.
///
/// Hashed rather than sanitised because a URL is not a filename — it holds `/`, `?`, `:` and can be
/// longer than the filesystem allows — and truncating or escaping one is how two different pages end
/// up sharing an entry. Eight bytes of SHA-256 is far past what a per-device cache can collide in.
pub fn key(url: &str) -> i64 {
    let d = sha256(url.as_bytes());
    i64::from_be_bytes([d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]])
}

/// The exact size [`encode_into`] will produce, so the buffer is allocated once.
///
/// Not a micro-optimisation: a `Vec` that grows by doubling holds the old and new buffers at the
/// same time, so encoding a 300 KB body peaks near a megabyte on a heap that has to survive it.
fn encoded_len(e: &Ref<'_>) -> usize {
    MAGIC.len() + 2 + 4 + (2 + e.url.len()) + (2 + e.etag.len()) + (2 + e.last_modified.len()) + 4
        + e.body.len()
}

/// Serialise into `out`, which is cleared first and allocated exactly once.
pub fn encode_into(e: &Ref<'_>, out: &mut Vec<u8>) {
    out.clear();
    out.reserve_exact(encoded_len(e));
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&e.status.to_le_bytes());
    out.extend_from_slice(&e.flags.0.to_le_bytes());
    put_str(out, e.url);
    put_str(out, e.etag);
    put_str(out, e.last_modified);
    out.extend_from_slice(&(e.body.len() as u32).to_le_bytes());
    out.extend_from_slice(e.body);
}

/// Parse a blob, borrowing from it.
///
/// `None` for anything that is not one — a short read, a foreign magic, a truncated body — because
/// every one of those is a cache miss and none is worth an error type.
pub fn decode_ref(bytes: &[u8]) -> Option<Ref<'_>> {
    let mut r = Reader::new(bytes);
    if r.take(4)? != MAGIC {
        return None;
    }
    let status = r.u16()?;
    let flags = Flags(r.i32()?);
    let url = r.str()?;
    let etag = r.str()?;
    let last_modified = r.str()?;
    let body_len = r.u32()? as usize;
    let body = r.take(body_len)?;
    Some(Ref { url, status, flags, etag, last_modified, body })
}

/// Store a response under its URL. See the module note on why a hit is not simply served.
///
/// `scratch` is the caller's encode buffer, passed in so a caller storing many responses allocates
/// once. It is cleared.
pub fn put<F: Fs>(fs: &mut F, e: &Ref<'_>, scratch: &mut Vec<u8>) -> Result<()> {
    if e.body.is_empty() {
        // A zero-length entry reads back as a hit and would be handed on as a complete page.
        return Err(Error::Argument);
    }
    if e.body.len() > MAX_ENTRY {
        return Err(Error::Overflow);
    }
    encode_into(e, scratch);
    let p = crate::cache::path_kind(fs, PREFIX, key(e.url)).ok_or(Error::Argument)?;
    crate::fs::write_atomic(fs, &p, scratch)
}

/// Read the stored blob for `url` into `buf`, ready for [`decode_ref`].
///
/// Two calls rather than one returning an entry, because an entry borrows the bytes: the caller has
/// to own the buffer. That is the API admitting where the one unavoidable copy is — reading a file
/// means holding it — instead of hiding a second one behind a convenience.
pub fn load<F: Fs>(fs: &mut F, url: &str, buf: &mut Vec<u8>) -> bool {
    buf.clear();
    let Some(p) = crate::cache::path_kind(fs, PREFIX, key(url)) else { return false };
    let Ok(Some(bytes)) = crate::fs::read(fs, &p) else { return false };
    *buf = bytes;
    // The stored URL is checked against the one asked for. Eight bytes of SHA-256 will not collide
    // on a phone, but a key that ever did would serve a different site's page, and that is not a
    // failure to discover in the field.
    match decode_ref(buf) {
        Some(e) if e.url == url => true,
        _ => {
            buf.clear();
            false
        }
    }
}

/// Forget the entry for `url`. Used when a revalidation says the stored copy is wrong, and by the
/// caller that clears history.
pub fn remove<F: Fs>(fs: &mut F, url: &str) {
    if let Some(p) = crate::cache::path_kind(fs, PREFIX, key(url)) {
        let _ = fs.delete(p.as_units());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::MemFs;
    use alloc::vec;

    const FLAGS: i32 = symbian_sys::SHIM_HTTP_GZIP | symbian_sys::SHIM_HTTP_GZIP_MAGIC;

    fn r<'a>(url: &'a str, body: &'a [u8]) -> Ref<'a> {
        Ref {
            url,
            status: 200,
            flags: Flags(FLAGS),
            etag: "\"abc\"",
            last_modified: "",
            body,
        }
    }

    #[test]
    fn a_stored_response_comes_back_whole() {
        let mut fs = MemFs::new();
        let mut scratch = Vec::new();
        let e = r("https://example.com/", b"\x1f\x8b compressed bytes");
        put(&mut fs, &e, &mut scratch).unwrap();

        let mut buf = Vec::new();
        assert!(load(&mut fs, "https://example.com/", &mut buf));
        assert_eq!(decode_ref(&buf), Some(e));
    }

    #[test]
    fn a_url_that_was_never_stored_is_a_miss() {
        let mut fs = MemFs::new();
        let mut buf = Vec::new();
        assert!(!load(&mut fs, "https://example.com/", &mut buf));
        assert!(buf.is_empty(), "a miss must not leave bytes behind to be decoded");
    }

    /// The flags travel with the body, because a second reader has to know whether these bytes are
    /// still compressed. Losing them would mean handing deflate to a parser.
    #[test]
    fn the_flags_survive_the_round_trip() {
        let mut fs = MemFs::new();
        let mut scratch = Vec::new();
        put(&mut fs, &r("https://example.com/", b"xx"), &mut scratch).unwrap();
        let mut buf = Vec::new();
        assert!(load(&mut fs, "https://example.com/", &mut buf));
        assert!(decode_ref(&buf).unwrap().flags.needs_inflate());
    }

    /// Two URLs are two entries, including two that differ only in the query.
    #[test]
    fn different_urls_do_not_share_an_entry() {
        let mut fs = MemFs::new();
        let mut scratch = Vec::new();
        for (u, b) in [("https://e.com/a", &b"AAA"[..]), ("https://e.com/b", b"BBB"),
                       ("https://e.com/a?x=1", b"CCC")] {
            put(&mut fs, &r(u, b), &mut scratch).unwrap();
        }
        for (u, b) in [("https://e.com/a", &b"AAA"[..]), ("https://e.com/b", b"BBB"),
                       ("https://e.com/a?x=1", b"CCC")] {
            let mut buf = Vec::new();
            assert!(load(&mut fs, u, &mut buf), "{u} should be stored");
            assert_eq!(decode_ref(&buf).unwrap().body, b, "wrong body for {u}");
        }
    }

    /// A URL is not a filename, and the key must not try to make it one.
    #[test]
    fn the_key_is_a_hash_and_not_the_url() {
        let a = key("https://example.com/a?q=1&r=2#frag");
        let b = key("https://example.com/a?q=1&r=3#frag");
        assert_ne!(a, b, "one character of query must change the key");
        assert_eq!(a, key("https://example.com/a?q=1&r=2#frag"), "and it must be stable");
    }

    /// A truncated or foreign blob is a miss, never a panic and never a partial page.
    #[test]
    fn a_damaged_blob_is_a_miss() {
        let mut scratch = Vec::new();
        encode_into(&r("https://example.com/", b"hello"), &mut scratch);
        assert!(decode_ref(&scratch).is_some());

        for cut in 0..scratch.len() {
            assert!(decode_ref(&scratch[..cut]).is_none(), "a {cut}-byte prefix decoded");
        }

        let mut wrong = scratch.clone();
        wrong[0] = b'X';
        assert!(decode_ref(&wrong).is_none(), "a foreign magic must not be read");
    }

    /// An empty body is refused, because it would read back as a complete zero-length page.
    #[test]
    fn an_empty_body_is_not_an_entry() {
        let mut fs = MemFs::new();
        let mut scratch = Vec::new();
        assert!(put(&mut fs, &r("https://example.com/", b""), &mut scratch).is_err());
        let mut buf = Vec::new();
        assert!(!load(&mut fs, "https://example.com/", &mut buf));
    }

    /// A body past the cap is refused rather than filling the cage.
    #[test]
    fn an_oversized_response_is_refused() {
        let mut fs = MemFs::new();
        let mut scratch = Vec::new();
        let big = vec![0u8; MAX_ENTRY + 1];
        assert_eq!(put(&mut fs, &r("https://e.com/huge", &big), &mut scratch), Err(Error::Overflow));
        let mut buf = Vec::new();
        assert!(!load(&mut fs, "https://e.com/huge", &mut buf), "and nothing was written");
    }

    #[test]
    fn removing_an_entry_makes_it_a_miss() {
        let mut fs = MemFs::new();
        let mut scratch = Vec::new();
        put(&mut fs, &r("https://e.com/", b"x"), &mut scratch).unwrap();
        let mut buf = Vec::new();
        assert!(load(&mut fs, "https://e.com/", &mut buf));
        remove(&mut fs, "https://e.com/");
        assert!(!load(&mut fs, "https://e.com/", &mut buf));
    }

    /// Whether a conditional request is even possible is the caller's question to ask.
    #[test]
    fn an_entry_knows_whether_it_can_be_revalidated() {
        let body = b"x";
        assert!(r("https://e.com/", body).has_validator(), "an ETag is a validator");

        let mut e = r("https://e.com/", body);
        e.etag = "";
        assert!(!e.has_validator());
        e.last_modified = "Sat, 23 Aug 2026 00:00:00 GMT";
        assert!(e.has_validator(), "so is Last-Modified");
    }

    /// A long URL is still one entry, because the key is fixed width.
    #[test]
    fn a_very_long_url_is_storable() {
        let mut fs = MemFs::new();
        let mut scratch = Vec::new();
        let mut url = alloc::string::String::from("https://e.com/");
        for _ in 0..300 {
            url.push('x');
        }
        put(&mut fs, &r(&url, b"body"), &mut scratch).unwrap();
        let mut buf = Vec::new();
        assert!(load(&mut fs, &url, &mut buf));
        assert_eq!(decode_ref(&buf).unwrap().body, b"body");
    }

    /// A body may be large; the format must not have a 64 KB limit hiding in it.
    #[test]
    fn a_body_past_sixty_four_kilobytes_round_trips() {
        let big = vec![0x5Au8; 300 * 1024];
        let mut scratch = Vec::new();
        encode_into(&r("https://e.com/big", &big), &mut scratch);
        assert_eq!(decode_ref(&scratch).unwrap().body.len(), big.len());
    }

    /// The encode buffer is allocated once, at the exact size. This is the lesson the handset
    /// taught: a doubling `Vec` holds two buffers at the peak, and that peak is what killed a run.
    #[test]
    fn encoding_allocates_exactly_once() {
        let big = vec![0u8; 200 * 1024];
        let e = r("https://e.com/big", &big);
        let mut scratch = Vec::new();
        encode_into(&e, &mut scratch);
        assert_eq!(scratch.len(), encoded_len(&e));
        assert_eq!(scratch.capacity(), scratch.len(), "reserve_exact, so no slack and no realloc");
    }

    /// The scratch buffer is reusable across entries, which is why it is a parameter.
    #[test]
    fn the_scratch_buffer_is_reused() {
        let mut fs = MemFs::new();
        let mut scratch = Vec::new();
        put(&mut fs, &r("https://e.com/1", b"aaaa"), &mut scratch).unwrap();
        let after_first = scratch.capacity();
        put(&mut fs, &r("https://e.com/2", b"bb"), &mut scratch).unwrap();
        assert!(scratch.capacity() >= after_first.min(scratch.capacity()));

        let mut buf = Vec::new();
        assert!(load(&mut fs, "https://e.com/1", &mut buf));
        assert_eq!(decode_ref(&buf).unwrap().body, b"aaaa", "the first entry survived the second");
    }
}
