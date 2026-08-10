//! Bytes fetched once, kept on disk between openings.
//!
//! The reason is the link, not the disk. A handset like this one runs over GPRS, metered by
//! the kilobyte, and a photo takes tens of seconds to arrive. Backing out of a picture and
//! opening it again must not pay for it twice — and without a cache it did, every time,
//! because the only thing the download path wrote was a single scratch file that the next
//! download overwrote.
//!
//! # Everything lives in the data cage
//!
//! `C:\private\<UID3>\` is the one location an unsigned application can write to with no
//! capability at all, so that is where this goes — see [`crate::fs::private_path`]. Files are
//! flat rather than in a subdirectory: creating a directory is another call that can fail,
//! and a prefix in the name achieves the same separation.
//!
//! # Keyed by id, and when that is safe
//!
//! The key is a `u64` the caller owns, and the contract is that the bytes behind one never
//! change. That holds for content-addressed or immutable ids — a service that gives edited
//! content a new id — and it is what lets a hit be served with no validation at all. A
//! mutable key wants an expiry this deliberately does not have.
//!
//! Written for the Telegram client's photo and document cache and lifted out of it unchanged.
//! Its own reason for existing survives the move: what expires there is the *reference*
//! needed to fetch a file, never the file, so the cache is the only part of that path that
//! cannot fail on an expired reference.

use alloc::vec::Vec;

use crate::fs::{self, Fs, Utf16Path};

/// How much a single cached file may be. Above this it is not written at all, so one video
/// cannot fill the cage — the phone has 250 MB and other applications live there too.
const MAX_CACHED: usize = 1024 * 1024;

/// The `m` prefix separates these from `session` and the trace log, which share the
/// directory. The extension is not the real format: nothing reads it, and the decoder sniffs
/// content rather than trusting a name. It is there so the files are recognisable when
/// pulled off the phone during development.
pub fn path<F: Fs>(fs: &mut F, id: i64) -> Option<Utf16Path> {
    path_kind(fs, 'm', id)
}

/// The same, under an explicit one-character prefix.
///
/// A photo has one id and two cached things behind it — the downloaded file and the inline
/// preview that came free with the message — and they are different bytes at different
/// sizes. Sharing a name would mean whichever was written last is served to both readers,
/// which is a corrupted picture, not a stale one.
pub fn path_kind<F: Fs>(fs: &mut F, prefix: char, id: i64) -> Option<Utf16Path> {
    let dir = fs::private_path(fs).ok()?;
    // The id is formatted unsigned so a negative one does not put a '-' in a filename;
    // Symbian accepts it, but it reads as a flag to every tool that touches the file.
    Utf16Path::join(dir.as_units(), &alloc::format!("{prefix}{:016x}.bin", id as u64)).ok()
}

/// The spilled inline preview for `id`, or `None`.
pub fn get_preview<F: Fs>(fs: &mut F, id: i64) -> Option<Vec<u8>> {
    let p = path_kind(fs, 'p', id)?;
    match fs::read(fs, &p) {
        Ok(Some(bytes)) if !bytes.is_empty() => Some(bytes),
        _ => None,
    }
}

/// Spill an inline preview to disk so it can be dropped from the heap.
///
/// Returns whether the bytes are safely on disk. Unlike [`put`], the caller *must* look:
/// the preview arrived inside the message and cannot be fetched again on its own, so
/// dropping it from memory after a failed write loses it until the whole conversation is
/// re-fetched.
pub fn put_preview<F: Fs>(fs: &mut F, id: i64, bytes: &[u8]) -> bool {
    if bytes.is_empty() || bytes.len() > MAX_CACHED {
        return false;
    }
    let Some(p) = path_kind(fs, 'p', id) else { return false };
    fs::write_atomic(fs, &p, bytes).is_ok()
}

/// The cached bytes for `id`, or `None` when it has not been downloaded.
///
/// A read error is `None` too, deliberately: a cache that cannot be read is
/// indistinguishable from a cache miss as far as the caller should care, and turning it into
/// an error would make a corrupt file block the download that would fix it.
pub fn get<F: Fs>(fs: &mut F, id: i64) -> Option<Vec<u8>> {
    let p = path(fs, id)?;
    match fs::read(fs, &p) {
        Ok(Some(bytes)) if !bytes.is_empty() => Some(bytes),
        _ => None,
    }
}

/// Store `bytes` under `id`. Failure is ignored by design — see [`put_result`].
pub fn put<F: Fs>(fs: &mut F, id: i64, bytes: &[u8]) {
    let _ = put_result(fs, id, bytes);
}

/// Store, reporting what happened.
///
/// The caller that ignores this is right to: the photo is already decoded and on screen, so
/// a full disk means the next opening is slow, not that anything visible failed. Reporting
/// "could not cache" over a picture the user is looking at would be noise.
pub fn put_result<F: Fs>(fs: &mut F, id: i64, bytes: &[u8]) -> crate::Result<()> {
    if bytes.is_empty() || bytes.len() > MAX_CACHED {
        return Err(crate::Error::Overflow);
    }
    let p = path(fs, id).ok_or(crate::Error::Argument)?;
    // Atomic, so an interrupted write leaves the previous file rather than a truncated one
    // that would later be served as a complete download.
    fs::write_atomic(fs, &p, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::MemFs;

    #[test]
    fn a_stored_file_comes_back() {
        let mut fs = MemFs::new();
        assert_eq!(get(&mut fs, 42), None, "nothing cached yet");
        put(&mut fs, 42, &[1, 2, 3]);
        assert_eq!(get(&mut fs, 42).as_deref(), Some(&[1u8, 2, 3][..]));
    }

    #[test]
    fn different_ids_do_not_share_a_file() {
        // The bug this replaces: every download went to one scratch name, so opening a
        // second photo overwrote the first and the cache hit rate was zero by construction.
        let mut fs = MemFs::new();
        put(&mut fs, 1, &[0xAA]);
        put(&mut fs, 2, &[0xBB]);
        assert_eq!(get(&mut fs, 1).as_deref(), Some(&[0xAAu8][..]));
        assert_eq!(get(&mut fs, 2).as_deref(), Some(&[0xBBu8][..]));
    }

    #[test]
    fn a_negative_id_is_a_legal_file_name() {
        // Telegram ids are signed and routinely negative. Formatted as a signed decimal the
        // name would start with '-', which Symbian accepts and every command-line tool
        // reads as an option.
        let mut fs = MemFs::new();
        put(&mut fs, -7, &[9]);
        assert_eq!(get(&mut fs, -7).as_deref(), Some(&[9u8][..]));

        let p = path(&mut fs, -7).unwrap();
        let name: alloc::string::String =
            char::decode_utf16(p.as_units().iter().copied()).map(|c| c.unwrap()).collect();
        assert!(!name.contains('-'), "the name must not start a filename with a dash: {name}");
        assert!(name.contains("mfffffffffffffff9"), "hex, unsigned: {name}");
    }

    #[test]
    fn an_oversized_file_is_refused_rather_than_filling_the_cage() {
        let mut fs = MemFs::new();
        let big = alloc::vec![0u8; MAX_CACHED + 1];
        assert!(put_result(&mut fs, 5, &big).is_err());
        assert_eq!(get(&mut fs, 5), None, "and nothing was written");
    }

    #[test]
    fn an_empty_write_is_not_a_cache_entry() {
        // A zero-length file would otherwise read back as a hit and be handed to the decoder
        // as a complete image.
        let mut fs = MemFs::new();
        assert!(put_result(&mut fs, 6, &[]).is_err());
        assert_eq!(get(&mut fs, 6), None);
    }
}
