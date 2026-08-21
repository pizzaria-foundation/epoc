//! A ZIP writer with no compression, because the job is to get files off a phone.
//!
//! Store-only: every entry goes in verbatim, method 0. That is a real choice and not a shortcut —
//! a deflate encoder would be a few hundred lines of table-driven code to save perhaps half of a
//! payload measured in tens of kilobytes, on a device where the transfer afterwards is Bluetooth
//! OBEX and the bottleneck is the pairing, not the bytes. Store-only archives open in every
//! unarchiver ever written, which is the property that actually matters here.
//!
//! No ZIP64 and no data descriptors: sizes are known before writing because the caller hands over
//! whole files. Entries beyond 4 GB, or more than 65535 of them, are simply out of scope for a
//! settings dump and the writer does not pretend otherwise.

use alloc::string::String;
use alloc::vec::Vec;

/// CRC-32 (the IEEE polynomial, reflected), which the ZIP format requires per entry.
///
/// Computed bit by bit rather than from a 1 KiB table: this runs a handful of times over a few
/// kilobytes each, and the table would be the largest thing in the binary for no measurable gain.
pub fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &byte in data {
        c ^= byte as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

struct Entry {
    name: String,
    crc: u32,
    size: u32,
    /// Offset of this entry's local header, for the central directory.
    offset: u32,
}

/// Accumulates entries and emits one archive.
pub struct ZipWriter {
    out: Vec<u8>,
    entries: Vec<Entry>,
}

impl ZipWriter {
    pub fn new() -> Self {
        Self { out: Vec::new(), entries: Vec::new() }
    }

    /// Number of entries added so far.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Add one file. `name` is the path inside the archive and is written as-is.
    pub fn add(&mut self, name: &str, data: &[u8]) {
        let offset = self.out.len() as u32;
        let crc = crc32(data);
        let size = data.len() as u32;

        self.out.extend_from_slice(&0x0403_4b50u32.to_le_bytes()); // local file header
        self.out.extend_from_slice(&10u16.to_le_bytes()); // version needed
        self.out.extend_from_slice(&0u16.to_le_bytes()); // flags
        self.out.extend_from_slice(&0u16.to_le_bytes()); // method: store
        // No timestamps. The phone's clock has been months wrong in this repo, and a wrong date
        // stamped on evidence is worse than no date; zero is the conventional "unset".
        self.out.extend_from_slice(&0u16.to_le_bytes()); // mod time
        self.out.extend_from_slice(&0u16.to_le_bytes()); // mod date
        self.out.extend_from_slice(&crc.to_le_bytes());
        self.out.extend_from_slice(&size.to_le_bytes()); // compressed
        self.out.extend_from_slice(&size.to_le_bytes()); // uncompressed
        self.out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        self.out.extend_from_slice(&0u16.to_le_bytes()); // extra field length
        self.out.extend_from_slice(name.as_bytes());
        self.out.extend_from_slice(data);

        self.entries.push(Entry { name: String::from(name), crc, size, offset });
    }

    /// Finish the archive: central directory, then the end record.
    pub fn finish(mut self) -> Vec<u8> {
        let dir_start = self.out.len() as u32;
        for e in &self.entries {
            self.out.extend_from_slice(&0x0201_4b50u32.to_le_bytes()); // central directory header
            self.out.extend_from_slice(&10u16.to_le_bytes()); // version made by
            self.out.extend_from_slice(&10u16.to_le_bytes()); // version needed
            self.out.extend_from_slice(&0u16.to_le_bytes()); // flags
            self.out.extend_from_slice(&0u16.to_le_bytes()); // method
            self.out.extend_from_slice(&0u16.to_le_bytes()); // mod time
            self.out.extend_from_slice(&0u16.to_le_bytes()); // mod date
            self.out.extend_from_slice(&e.crc.to_le_bytes());
            self.out.extend_from_slice(&e.size.to_le_bytes());
            self.out.extend_from_slice(&e.size.to_le_bytes());
            self.out.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
            self.out.extend_from_slice(&0u16.to_le_bytes()); // extra
            self.out.extend_from_slice(&0u16.to_le_bytes()); // comment
            self.out.extend_from_slice(&0u16.to_le_bytes()); // disk number
            self.out.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
            self.out.extend_from_slice(&0u32.to_le_bytes()); // external attrs
            self.out.extend_from_slice(&e.offset.to_le_bytes());
            self.out.extend_from_slice(e.name.as_bytes());
        }
        let dir_size = self.out.len() as u32 - dir_start;
        let count = self.entries.len() as u16;

        self.out.extend_from_slice(&0x0605_4b50u32.to_le_bytes()); // end of central directory
        self.out.extend_from_slice(&0u16.to_le_bytes()); // this disk
        self.out.extend_from_slice(&0u16.to_le_bytes()); // disk with directory
        self.out.extend_from_slice(&count.to_le_bytes());
        self.out.extend_from_slice(&count.to_le_bytes());
        self.out.extend_from_slice(&dir_size.to_le_bytes());
        self.out.extend_from_slice(&dir_start.to_le_bytes());
        self.out.extend_from_slice(&0u16.to_le_bytes()); // comment length
        self.out
    }
}

impl Default for ZipWriter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_the_known_vector() {
        // "123456789" -> 0xCBF43926 is the standard CRC-32 check value. If this is wrong every
        // archive this writer produces is refused by every unarchiver, so it is worth pinning.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn an_archive_has_the_signatures_and_the_count_an_unarchiver_looks_for() {
        let mut z = ZipWriter::new();
        z.add("rom_101f8766.txt", b"key value\n");
        z.add("cur_101f8766.cre", &[0u8, 1, 2, 3]);
        assert_eq!(z.len(), 2);
        let out = z.finish();

        assert_eq!(&out[0..4], &0x0403_4b50u32.to_le_bytes(), "starts with a local file header");
        // The end record is last and fixed width with no comment, so it can be read from the tail.
        let eocd = &out[out.len() - 22..];
        assert_eq!(&eocd[0..4], &0x0605_4b50u32.to_le_bytes());
        assert_eq!(u16::from_le_bytes([eocd[10], eocd[11]]), 2, "entry count");
        let dir_size = u32::from_le_bytes([eocd[12], eocd[13], eocd[14], eocd[15]]) as usize;
        let dir_start = u32::from_le_bytes([eocd[16], eocd[17], eocd[18], eocd[19]]) as usize;
        assert_eq!(dir_start + dir_size, out.len() - 22, "the directory ends where the record begins");
        assert_eq!(&out[dir_start..dir_start + 4], &0x0201_4b50u32.to_le_bytes());
    }

    #[test]
    fn the_payload_goes_in_verbatim() {
        let mut z = ZipWriter::new();
        let payload = b"0x1 0x2 int 42";
        z.add("a.txt", payload);
        let out = z.finish();
        // Store-only, so the bytes appear untouched — which is the property that lets a person
        // check an archive by eye when an unarchiver disagrees with them.
        assert!(out.windows(payload.len()).any(|w| w == payload));
    }

    #[test]
    fn an_empty_archive_is_still_a_valid_one() {
        let out = ZipWriter::new().finish();
        assert_eq!(out.len(), 22);
        assert_eq!(&out[0..4], &0x0605_4b50u32.to_le_bytes());
    }
}
