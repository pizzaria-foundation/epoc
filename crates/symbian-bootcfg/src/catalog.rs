//! What a repository said it had, the last time anybody asked.
//!
//! The Pkgs screen has to work with the radio off. A phone in a lift is not a phone that should stop
//! listing the update it found this morning, so a check writes what it learned here and the screen
//! reads *this* rather than the network.
//!
//! ## An entry is a promise, not a fact
//!
//! Everything in a [`CatEntry`] comes from the service: a name, a version read off a git tag, a size
//! and a URL. **The UID3 is not there**, and that absence is deliberate. Identity comes from inside
//! the `.sis` — [`crate::sis::parse`] — and it is only knowable once the file has been downloaded.
//! Taking a UID from a JSON field would put the one fact this whole system keys on in the hands of
//! whoever wrote the release notes.
//!
//! So the flow is: the catalogue says *there is probably a 0.2.0 of something called launcher over
//! there*, the download turns that into bytes, and the bytes say what they install. A candidate whose
//! file turns out to install a different application than the row promised is a mismatch worth
//! showing, not a fact worth overwriting.

use alloc::string::String;
use alloc::vec::Vec;

use crate::config::DecodeError;
use crate::crc::crc16;
use crate::pkg::Version;

/// `b"BTCT"` read as a little-endian u32.
pub const MAGIC: u32 = 0x5443_5442;
pub const VERSION: u16 = 1;
pub const HEADER_SIZE: usize = 16;
/// Bytes per record: repo id, three version numbers, the size, and an offset/length pair for each of
/// the three strings. Exactly what the encoder writes — a record size larger than that puts the
/// string blob's offsets past where the strings are, which is how this file first failed to decode
/// its own output.
pub const ENTRY_SIZE: u16 = 28;
/// Refused above this. A phone is not a package archive, and a repository that offers three hundred
/// downloads is one we have misread.
pub const MAX_ENTRIES: usize = 64;

/// One thing a repository offers.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CatEntry {
    /// Which repository said so — [`crate::repo::Repo::id`].
    pub repo_id: u16,
    /// The asset's file name, as published. This is what the row is labelled with until the file has
    /// been downloaded and can say what it really is.
    pub asset: String,
    /// The package name the repository goes by, for grouping rows that belong together.
    pub name: String,
    /// From the release tag. `v0.2.0` and `0.2.0` both land here as 0.2.0.
    pub version: Version,
    /// Where to get it.
    pub url: String,
    /// What the service said it weighs, for the progress bar to have a denominator.
    pub size: u64,
}

impl CatEntry {
    /// The name this file will be saved as, in `C:\Data\_app_install\`.
    ///
    /// The asset's own name, because that is what a person will see in File mgr. next to everything
    /// they sideloaded by hand, and a name we invented would be a second thing to explain.
    pub fn file_name(&self) -> &str {
        &self.asset
    }
}

/// Everything every repository offered, at the last check.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct CatalogDb {
    pub entries: Vec<CatEntry>,
}

impl CatalogDb {
    /// Replace everything one repository contributed, leaving the others alone.
    ///
    /// A check is per repository, so it may only speak for its own rows. Rebuilding the whole
    /// catalogue from one answer is how a repository that happened to be reachable erases one that
    /// was not.
    pub fn replace_repo(&mut self, repo_id: u16, fresh: Vec<CatEntry>) {
        self.entries.retain(|e| e.repo_id != repo_id);
        for e in fresh {
            if self.entries.len() >= MAX_ENTRIES {
                return;
            }
            self.entries.push(e);
        }
    }

    /// Entries from one repository.
    pub fn of_repo(&self, repo_id: u16) -> impl Iterator<Item = &CatEntry> {
        self.entries.iter().filter(move |e| e.repo_id == repo_id)
    }

    pub fn encode(&self) -> Vec<u8> {
        let count = self.entries.len().min(MAX_ENTRIES);
        let mut blob: Vec<u16> = Vec::new();
        let mut out = Vec::with_capacity(HEADER_SIZE + count * ENTRY_SIZE as usize);

        out.extend_from_slice(&MAGIC.to_le_bytes());
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&ENTRY_SIZE.to_le_bytes());
        out.extend_from_slice(&(count as u16).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());

        for e in self.entries.iter().take(count) {
            let asset = push_str(&mut blob, &e.asset);
            let name = push_str(&mut blob, &e.name);
            let url = push_str(&mut blob, &e.url);

            out.extend_from_slice(&e.repo_id.to_le_bytes());
            out.extend_from_slice(&e.version.major.to_le_bytes());
            out.extend_from_slice(&e.version.minor.to_le_bytes());
            out.extend_from_slice(&e.version.patch.to_le_bytes());
            out.extend_from_slice(&e.size.to_le_bytes());
            out.extend_from_slice(&asset.0.to_le_bytes());
            out.extend_from_slice(&asset.1.to_le_bytes());
            out.extend_from_slice(&name.0.to_le_bytes());
            out.extend_from_slice(&name.1.to_le_bytes());
            out.extend_from_slice(&url.0.to_le_bytes());
            out.extend_from_slice(&url.1.to_le_bytes());
        }

        for u in &blob {
            out.extend_from_slice(&u.to_le_bytes());
        }
        let crc = crc16(&out);
        out[14..16].copy_from_slice(&crc.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < HEADER_SIZE {
            return Err(DecodeError::Truncated);
        }
        if u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) != MAGIC {
            return Err(DecodeError::BadMagic);
        }
        let version = u16::from_le_bytes([bytes[4], bytes[5]]);
        if version > VERSION {
            return Err(DecodeError::BadVersion(version));
        }
        let entry_size = u16::from_le_bytes([bytes[6], bytes[7]]) as usize;
        if entry_size < ENTRY_SIZE as usize {
            return Err(DecodeError::BadLayout);
        }
        let count = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
        if count > MAX_ENTRIES {
            return Err(DecodeError::TooMany(count));
        }
        let table_end = HEADER_SIZE.checked_add(count * entry_size).ok_or(DecodeError::BadLayout)?;
        if bytes.len() < table_end {
            return Err(DecodeError::BadLayout);
        }
        let mut check = Vec::from(bytes);
        let stored = u16::from_le_bytes([bytes[14], bytes[15]]);
        check[14..16].copy_from_slice(&[0, 0]);
        if crc16(&check) != stored {
            return Err(DecodeError::BadCrc);
        }

        let tail = &bytes[table_end..];
        if tail.len() % 2 != 0 {
            return Err(DecodeError::BadLayout);
        }
        let blob: Vec<u16> =
            tail.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();

        let mut entries = Vec::with_capacity(count);
        for i in 0..count {
            let r = &bytes[HEADER_SIZE + i * entry_size..];
            let mut size = [0u8; 8];
            size.copy_from_slice(&r[8..16]);
            entries.push(CatEntry {
                repo_id: u16::from_le_bytes([r[0], r[1]]),
                version: Version::new(
                    u16::from_le_bytes([r[2], r[3]]),
                    u16::from_le_bytes([r[4], r[5]]),
                    u16::from_le_bytes([r[6], r[7]]),
                ),
                size: u64::from_le_bytes(size),
                asset: take_str(&blob, r, 16).ok_or(DecodeError::BadLayout)?,
                name: take_str(&blob, r, 20).ok_or(DecodeError::BadLayout)?,
                url: take_str(&blob, r, 24).ok_or(DecodeError::BadLayout)?,
            });
        }
        Ok(Self { entries })
    }
}

fn push_str(blob: &mut Vec<u16>, s: &str) -> (u16, u16) {
    let units: Vec<u16> = s.encode_utf16().take(u16::MAX as usize).collect();
    let off = blob.len() as u16;
    let len = units.len() as u16;
    blob.extend_from_slice(&units);
    (off, len)
}

fn take_str(blob: &[u16], r: &[u8], at: usize) -> Option<String> {
    let off = u16::from_le_bytes([r[at], r[at + 1]]) as usize;
    let len = u16::from_le_bytes([r[at + 2], r[at + 3]]) as usize;
    let slice = blob.get(off..off.checked_add(len)?)?;
    Some(String::from_utf16_lossy(slice))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn entry(repo_id: u16, asset: &str, v: (u16, u16, u16)) -> CatEntry {
        CatEntry {
            repo_id,
            asset: String::from(asset),
            name: String::from("launcher"),
            version: Version::new(v.0, v.1, v.2),
            url: alloc::format!("https://github.com/x/y/releases/download/v1/{asset}"),
            size: 320_484,
        }
    }

    #[test]
    fn the_record_size_matches_what_the_encoder_writes() {
        // The bug this file had: `ENTRY_SIZE` said 32 while the encoder wrote 28, so the string
        // blob's offsets pointed past the strings and the file would not decode its own output. With
        // no strings the blob is empty, which makes the stride arithmetic instead of a number
        // somebody typed.
        let mut d = CatalogDb::default();
        for i in 0..3 {
            d.entries.push(CatEntry {
                repo_id: 1,
                asset: String::new(),
                name: String::new(),
                version: Version::new(0, 0, i),
                url: String::new(),
                size: 0,
            });
        }
        assert_eq!(d.encode().len(), HEADER_SIZE + 3 * ENTRY_SIZE as usize);
    }

    #[test]
    fn a_catalogue_round_trips() {
        let d = CatalogDb {
            entries: vec![entry(1, "launcher.sisx", (0, 2, 0)), entry(2, "cal.sis", (0, 3, 1))],
        };
        assert_eq!(CatalogDb::decode(&d.encode()).unwrap(), d);
    }

    #[test]
    fn an_empty_catalogue_is_valid() {
        let back = CatalogDb::decode(&CatalogDb::default().encode()).unwrap();
        assert!(back.entries.is_empty());
    }

    #[test]
    fn one_flipped_byte_is_refused() {
        let mut b = CatalogDb { entries: vec![entry(1, "launcher.sisx", (0, 2, 0))] }.encode();
        let last = b.len() - 1;
        b[last] ^= 0xFF;
        assert_eq!(CatalogDb::decode(&b), Err(DecodeError::BadCrc));
    }

    #[test]
    fn a_check_speaks_only_for_its_own_repository() {
        // A repository that happened to be reachable must not erase one that was not: the phone
        // would silently stop offering an update it already knew about.
        let mut d = CatalogDb {
            entries: vec![entry(1, "launcher.sisx", (0, 1, 0)), entry(2, "cal.sis", (0, 3, 0))],
        };
        d.replace_repo(1, vec![entry(1, "launcher.sisx", (0, 2, 0))]);
        assert_eq!(d.entries.len(), 2);
        assert_eq!(d.of_repo(1).next().unwrap().version, Version::new(0, 2, 0));
        assert_eq!(d.of_repo(2).next().unwrap().version, Version::new(0, 3, 0), "untouched");
    }

    #[test]
    fn a_repository_that_now_offers_nothing_clears_its_rows() {
        // Deleting a release should make the row go away, not linger for ever.
        let mut d = CatalogDb { entries: vec![entry(1, "launcher.sisx", (0, 1, 0))] };
        d.replace_repo(1, vec![]);
        assert!(d.entries.is_empty());
    }

    #[test]
    fn a_flood_of_entries_is_capped_rather_than_believed() {
        let mut d = CatalogDb::default();
        let many: Vec<CatEntry> =
            (0..500).map(|i| entry(1, "a.sis", (0, 0, i as u16))).collect();
        d.replace_repo(1, many);
        assert_eq!(d.entries.len(), MAX_ENTRIES);
    }

    #[test]
    fn the_file_a_download_will_be_saved_as_is_the_assets_own_name() {
        // What a person sees in File mgr. next to everything they sideloaded by hand. A name we
        // invented would be a second thing to explain.
        assert_eq!(entry(1, "launcher-0.2.0.sisx", (0, 2, 0)).file_name(), "launcher-0.2.0.sisx");
    }
}
