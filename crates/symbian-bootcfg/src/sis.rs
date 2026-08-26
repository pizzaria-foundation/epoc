//! Reading a package's identity out of the package.
//!
//! What version is this `.sis`, and what does it install? The only honest place to ask is the file,
//! and until this module existed the answer came from the *file name* — `launcher-0.2.0.sisx` —
//! which is a convention a person has to remember and can rename away. A build pushed as
//! `launcher.sisx` was simply invisible.
//!
//! So this reads it. The layout below was measured against this repo's own packages
//! (`apps/bootctl/build/bootctl.sisx`, `apps/bootd/build/bootd.sis`, `dist-amigo/launcher.sisx`),
//! not taken from a document — and the fixture in the tests is the first 128 bytes of a real one.
//!
//! ```text
//! 0x00  UID1 = 0x10201A7A     the SIS magic
//! 0x04  UID2
//! 0x08  UID3                  the package UID — the same identity the boot list uses
//! 0x0c  UID checksum
//! 0x10  field: SISContents (12)
//!         field: SISControllerChecksum (34)
//!         field: SISDataChecksum (35)
//!         field: SISCompressed (3)   algorithm u32, uncompressed size u64, then zlib
//!           └── inflates to:
//!               field: SISController (13)
//!                 field: SISInfo (14)
//!                   field: SISUid (9)         u32, and it agrees with 0x08
//!                   field: SISString (1)      the unique vendor name
//!                   field: SISArray (2)       package names, by language
//!                     field: SISString (1)    "launcher", UTF-16LE
//!                   field: SISArray (2)       vendor names
//!                   field: SISVersion (4)     three int32: major, minor, build
//! ```
//!
//! Every field is `type: u32`, `length`, `data`, padded to a four-byte boundary.
//!
//! ## What this deliberately does not do
//!
//! It is not a SIS reader. It walks as far as the version and stops — no data section, no
//! signatures, no install blocks, no verification of anything. A package this refuses is a package
//! we do not offer, which is the safe direction; a package it accepts has told us three facts, and
//! the installer remains the thing that decides whether to install it.
//!
//! Nothing here trusts a length. Every field is bounded by its parent, the recursion is bounded by
//! a depth, the inflate is bounded by [`MAX_CONTROLLER`], and a package whose controller lies past
//! [`HEAD_BYTES`] is refused rather than chased — a boot manager must not be the thing that runs a
//! phone out of memory reading a file somebody put on a card.

use alloc::string::String;
use alloc::vec::Vec;

use crate::pkg::Version;

/// `UID1` of every SIS file. Not a heuristic — it is the file's own type marker.
pub const SIS_UID1: u32 = 0x1020_1A7A;

/// How much of a candidate needs to be read to answer the question.
///
/// The controller sits immediately after a 16-byte header and two checksums, and in this repo's
/// packages it is 300 bytes to 2 KB compressed. 64 KB is a wide margin that still bounds the read
/// to something a daemon can hold, and a file needing more is refused with [`SisError::TooBig`]
/// rather than read in full.
pub const HEAD_BYTES: usize = 64 * 1024;

/// Ceiling on the inflated controller. Signatures and install blocks live in there, so it is
/// legitimately larger than the compressed form; it is not legitimately a megabyte.
pub const MAX_CONTROLLER: usize = 512 * 1024;

/// How deep the field walk will go. The path to a version is four levels; anything claiming more is
/// either not a SIS or is trying to make us recurse.
const MAX_DEPTH: u8 = 6;

// Field types, from the walk above. Only the ones on the path are named.
const F_STRING: u32 = 1;
const F_ARRAY: u32 = 2;
const F_COMPRESSED: u32 = 3;
const F_VERSION: u32 = 4;
const F_UID: u32 = 9;
const F_CONTENTS: u32 = 12;
const F_CONTROLLER: u32 = 13;
const F_INFO: u32 = 14;
/// The largest field type this format defines. A number past it is how a walk that has wandered off
/// the structure notices, rather than following a length made of file data.
const F_MAX: u32 = 45;

/// What a package says it is.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SisInfo {
    /// The application UID3 the package installs, from `SISUid` — cross-checked against the file
    /// header, because two places saying different things is a file to refuse rather than pick from.
    pub uid3: u32,
    pub version: Version,
    /// The package name, from the first entry of the name array. This is what a row is labelled
    /// with when the package is one we have never seen before.
    pub name: String,
}

/// Why a file is not a package we will offer. Every variant means "not a candidate", never a guess.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum SisError {
    /// Shorter than a SIS header.
    Truncated,
    /// `UID1` is not [`SIS_UID1`]. Not a SIS file at all.
    NotSis,
    /// A field's length runs past its parent, or the structure does not nest.
    BadField,
    /// The controller is past [`HEAD_BYTES`], or claims to inflate past [`MAX_CONTROLLER`].
    TooBig,
    /// The controller is compressed with something that is not zlib deflate.
    BadCompression,
    /// The zlib stream did not inflate.
    Inflate,
    /// Walked the controller and found no `SISInfo`.
    NoInfo,
    /// Found the info block and no version in it.
    NoVersion,
    /// The header's UID3 and `SISUid` disagree.
    UidMismatch,
}

/// One field header: its type, where its data starts, and where the next field starts.
struct Field {
    kind: u32,
    start: usize,
    end: usize,
    next: usize,
}

/// Read the field at `at`, bounded by `limit`.
///
/// A field whose data runs past `limit` is refused, and that is what keeps a walk inside its parent
/// rather than following a length made of file contents.
fn field(buf: &[u8], at: usize, limit: usize) -> Option<Field> {
    let f = header(buf, at, limit)?;
    (f.end <= limit).then_some(f)
}

/// The field header at `at`, without checking that its *data* fits.
///
/// Split out for exactly one caller. `SISContents` is the outermost field and its length describes
/// the whole package — 37 KB for the smallest thing this repo builds — while we deliberately hold
/// only the head. Refusing it for not fitting would refuse every package there is; the answer is to
/// read its header and then clamp its end to what we actually have, which [`parse`] does. Every
/// field *inside* it goes through [`field`] and is bounded normally.
///
/// The length is 32 bits, or 64 when the top bit of the first word is set — the format's own
/// variable-width length.
fn header(buf: &[u8], at: usize, limit: usize) -> Option<Field> {
    if at.checked_add(8)? > limit {
        return None;
    }
    let kind = u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]);
    let lo = u32::from_le_bytes([buf[at + 4], buf[at + 5], buf[at + 6], buf[at + 7]]);
    let (len, hdr) = if lo & 0x8000_0000 == 0 {
        (lo as u64, 8usize)
    } else {
        if at.checked_add(12)? > limit {
            return None;
        }
        let hi = u32::from_le_bytes([buf[at + 8], buf[at + 9], buf[at + 10], buf[at + 11]]);
        (((lo & 0x7FFF_FFFF) as u64) | ((hi as u64) << 31), 12usize)
    };
    if kind == 0 || kind > F_MAX {
        return None;
    }
    let start = at.checked_add(hdr)?;
    let len = usize::try_from(len).ok()?;
    let end = start.checked_add(len)?;
    // Fields are four-byte aligned, so the next one starts at the padded end.
    let next = end.checked_add(3)? & !3;
    Some(Field { kind, start, end, next })
}

/// Every field directly inside `[from, to)`.
fn children(buf: &[u8], from: usize, to: usize) -> Vec<Field> {
    let mut out = Vec::new();
    let mut at = from;
    while at < to {
        match field(buf, at, to) {
            Some(f) => {
                at = f.next;
                out.push(f);
            }
            None => break,
        }
    }
    out
}

/// Find the first field of `kind` at any depth inside `[from, to)`.
fn find(buf: &[u8], from: usize, to: usize, kind: u32, depth: u8) -> Option<Field> {
    if depth == 0 {
        return None;
    }
    for f in children(buf, from, to) {
        if f.kind == kind {
            return Some(f);
        }
        // Only containers are descended into. Descending into a string would read its characters as
        // field headers, which is how a parser finds structure in noise.
        if matches!(f.kind, F_CONTENTS | F_CONTROLLER | F_INFO | F_ARRAY) {
            if let Some(hit) = find(buf, f.start, f.end, kind, depth - 1) {
                return Some(hit);
            }
        }
    }
    None
}

/// Read what `bytes` — the first [`HEAD_BYTES`] of a `.sis` or `.sisx`, or all of it if smaller —
/// says about itself.
pub fn parse(bytes: &[u8]) -> Result<SisInfo, SisError> {
    if bytes.len() < 20 {
        return Err(SisError::Truncated);
    }
    if u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) != SIS_UID1 {
        return Err(SisError::NotSis);
    }
    let header_uid = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);

    // The SISContents field, whose own length usually describes the whole file — so it is bounded
    // by what we actually hold rather than by what it claims.
    let limit = bytes.len();
    let contents = header(bytes, 16, limit).ok_or(SisError::Truncated)?;
    if contents.kind != F_CONTENTS || contents.start > limit {
        return Err(SisError::BadField);
    }
    let body_end = contents.end.min(limit);

    // Either the controller is compressed — and then everything below is read out of the inflated
    // copy — or it is where it was found, and the file itself is the view. One `view` and one range
    // afterwards, so the field walk has no idea which case it is in.
    let inflated = match read_controller(bytes, contents.start, body_end)? {
        Controller::Inline => None,
        Controller::Inflated(v) => Some(v),
    };
    let (view, from, to): (&[u8], usize, usize) = match inflated.as_deref() {
        Some(v) => (v, 0, v.len()),
        None => (bytes, contents.start, body_end),
    };

    let info = find(view, from, to, F_INFO, MAX_DEPTH).ok_or(SisError::NoInfo)?;
    let kids = children(view, info.start, info.end);

    let uid3 = kids
        .iter()
        .find(|f| f.kind == F_UID)
        .filter(|f| f.end - f.start >= 4)
        .map(|f| {
            u32::from_le_bytes([
                view[f.start],
                view[f.start + 1],
                view[f.start + 2],
                view[f.start + 3],
            ])
        })
        .ok_or(SisError::NoInfo)?;
    if uid3 != header_uid {
        return Err(SisError::UidMismatch);
    }

    let version = kids
        .iter()
        .find(|f| f.kind == F_VERSION)
        .filter(|f| f.end - f.start >= 12)
        .map(|f| read_version(&view[f.start..f.start + 12]))
        .ok_or(SisError::NoVersion)?;

    Ok(SisInfo { uid3, version, name: read_name(view, &kids) })
}

enum Controller {
    /// The controller is not compressed; it is where it was found.
    Inline,
    Inflated(Vec<u8>),
}

/// Get at the controller: inflate it if it is compressed, otherwise say so.
fn read_controller(bytes: &[u8], from: usize, to: usize) -> Result<Controller, SisError> {
    for f in children(bytes, from, to) {
        match f.kind {
            // Found uncompressed. Older packages and `makesis` with compression off.
            F_CONTROLLER => return Ok(Controller::Inline),
            F_COMPRESSED => {
                if f.end - f.start < 12 {
                    return Err(SisError::BadField);
                }
                let algo = u32::from_le_bytes([
                    bytes[f.start],
                    bytes[f.start + 1],
                    bytes[f.start + 2],
                    bytes[f.start + 3],
                ]);
                let mut raw = [0u8; 8];
                raw.copy_from_slice(&bytes[f.start + 4..f.start + 12]);
                let want = u64::from_le_bytes(raw);
                // 0 is "stored" in this format; 1 is deflate. Anything else is a package built by
                // something we have not met, and reading it wrong is worse than not reading it.
                if algo == 0 {
                    return Ok(Controller::Inline);
                }
                if algo != 1 {
                    return Err(SisError::BadCompression);
                }
                if want > MAX_CONTROLLER as u64 {
                    return Err(SisError::TooBig);
                }
                // The compressed data may be cut off by HEAD_BYTES. That is a refusal and not a
                // partial inflate: half a controller can still contain a plausible version.
                if f.end > bytes.len() {
                    return Err(SisError::TooBig);
                }
                let out = symbian_crypto::inflate::inflate_zlib(
                    &bytes[f.start + 12..f.end],
                    MAX_CONTROLLER,
                )
                .map_err(|_| SisError::Inflate)?;
                return Ok(Controller::Inflated(out));
            }
            _ => {}
        }
    }
    Err(SisError::NoInfo)
}

/// Three little-endian `int32`s. Negative is not a version; it is a file being read as one.
fn read_version(b: &[u8]) -> Version {
    let n = |o: usize| {
        let v = i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        v.clamp(0, u16::MAX as i32) as u16
    };
    Version::new(n(0), n(4), n(8))
}

/// The package name: the first string in the first name array, decoded from UTF-16LE.
///
/// Best effort by design. A package with no readable name is still a package — the UID and the
/// version are what an install needs — so this returns an empty string rather than failing, and the
/// UI falls back to the hex UID the way it already does for a boot-list row.
fn read_name(view: &[u8], kids: &[Field]) -> String {
    for f in kids.iter().filter(|f| f.kind == F_ARRAY) {
        for s in children(view, f.start, f.end) {
            if s.kind != F_STRING {
                continue;
            }
            let units: Vec<u16> = view[s.start..s.end]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            let name = String::from_utf16_lossy(&units);
            if !name.is_empty() {
                return name;
            }
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// The head of a real package, built by this repo's own toolchain: `apps/bootd/build/bootd.sis`,
    /// bytes 0..361 — the header plus the whole compressed controller, which is everything
    /// [`parse`] reads.
    ///
    /// Bytes in the repo rather than a path to a build artefact, because a test that needs
    /// `epoc build` to have run is a test that does not run. And a real package rather than a
    /// hand-written one, because the fixture has to prove the inflate: a fixture somebody wrote to
    /// match the parser proves only that they agree with each other.
    fn real_package() -> Vec<u8> {
        include_bytes!("../tests/bootd_head.bin").to_vec()
    }

    #[test]
    fn a_real_package_gives_up_its_uid_version_and_name() {
        let info = parse(&real_package()).expect("a package this repo built");
        assert_eq!(info.uid3, 0xE0AA_0011);
        assert_eq!(info.version, Version::new(0, 1, 0));
        assert_eq!(info.name, "bootd");
    }

    /// `apps/bootctl/build/bootctl.sisx`, bytes 0..1754. A *signed* package: its controller carries
    /// a certificate chain, so it is several times larger and the info block is no longer the only
    /// thing in it. Worth its own fixture — a parser that only ever met the unsigned shape is a
    /// parser that has not met the shape it will actually be handed.
    fn signed_package() -> Vec<u8> {
        include_bytes!("../tests/bootctl_head.bin").to_vec()
    }

    #[test]
    fn a_signed_package_reads_the_same_way() {
        let info = parse(&signed_package()).expect("the package this repo ships");
        assert_eq!(info.uid3, 0xE0AA_0010);
        assert_eq!(info.version, Version::new(0, 1, 0));
        assert_eq!(info.name, "bootctl");
    }

    #[test]
    fn the_name_comes_from_the_package_and_not_from_the_file_it_was_saved_as() {
        // The whole point of reading the file: `bootctl.sisx` renamed to `x.sis`, or pushed with no
        // version in its name at all, still says what it is.
        assert_eq!(parse(&signed_package()).unwrap().name, "bootctl");
        assert_eq!(parse(&real_package()).unwrap().name, "bootd");
    }

    #[test]
    fn anything_that_is_not_a_sis_is_refused_and_named_as_such() {
        assert_eq!(parse(&[]), Err(SisError::Truncated));
        assert_eq!(parse(&[0u8; 64]), Err(SisError::NotSis));
        // A JPEG that somebody renamed.
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0];
        jpeg.resize(64, 0);
        assert_eq!(parse(&jpeg), Err(SisError::NotSis));
    }

    #[test]
    fn a_truncated_package_is_refused_rather_than_half_read() {
        let full = real_package();
        for cut in [20, 32, 0x44, 0x80, full.len() - 8] {
            let err = parse(&full[..cut]).expect_err("a cut package must not parse");
            assert!(
                matches!(
                    err,
                    SisError::Truncated
                        | SisError::BadField
                        | SisError::TooBig
                        | SisError::Inflate
                        | SisError::NoInfo
                ),
                "cut at {cut} gave {err:?}"
            );
        }
    }

    #[test]
    fn a_header_that_disagrees_with_the_controller_is_refused() {
        // The one internal cross-check available, and the case it catches is a header edited to
        // make one package look like another — which would install over the wrong application.
        let mut b = real_package();
        b[8] = 0x99;
        assert_eq!(parse(&b), Err(SisError::UidMismatch));
    }

    #[test]
    fn a_corrupted_controller_does_not_produce_a_version() {
        let mut b = real_package();
        let last = b.len() - 1;
        b[last] ^= 0xFF;
        assert!(matches!(parse(&b), Err(SisError::Inflate)), "a broken stream is not a package");
    }

    #[test]
    fn a_controller_claiming_to_inflate_forever_is_refused_before_it_allocates() {
        let mut b = real_package();
        // The uncompressed-size field of the SISCompressed at 0x30: data starts at 0x38, so the
        // u64 size sits at 0x3c.
        b[0x3c..0x44].copy_from_slice(&(MAX_CONTROLLER as u64 + 1).to_le_bytes());
        assert_eq!(parse(&b), Err(SisError::TooBig));
    }

    #[test]
    fn an_unknown_compression_is_refused_rather_than_read_as_deflate() {
        let mut b = real_package();
        b[0x38..0x3c].copy_from_slice(&7u32.to_le_bytes());
        assert_eq!(parse(&b), Err(SisError::BadCompression));
    }

    #[test]
    fn a_field_length_running_past_its_parent_stops_the_walk() {
        let mut b = real_package();
        // SISContents' own length, at 0x14: made shorter than the fields inside it.
        b[0x14..0x18].copy_from_slice(&8u32.to_le_bytes());
        assert!(parse(&b).is_err());
    }

    #[test]
    fn a_version_is_never_negative_however_the_bytes_read() {
        let b: [u8; 12] = [
            0xFF, 0xFF, 0xFF, 0xFF, // -1
            0x02, 0x00, 0x00, 0x00, //  2
            0xFF, 0xFF, 0x00, 0x00, //  65535
        ];
        assert_eq!(read_version(&b), Version::new(0, 2, u16::MAX));
    }
}
