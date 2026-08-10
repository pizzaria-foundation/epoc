//! A minimal PNG writer, so the preview tool has no dependencies.
//!
//! Compression is deliberately absent: the zlib stream is built from *stored*
//! deflate blocks, which is legal and trivially correct. A 320x240 screenshot is
//! ~230 KB that way, which is irrelevant for a developer-facing preview and saves
//! pulling in a deflate implementation we would otherwise never use.

use std::io::{self, Write};

fn crc32(data: &[u8]) -> u32 {
    // Bitwise rather than table-driven: a screenshot is small and this avoids a
    // lazily-initialised static.
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn chunk(w: &mut impl Write, kind: &[u8; 4], data: &[u8]) -> io::Result<()> {
    w.write_all(&(data.len() as u32).to_be_bytes())?;
    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(data);
    w.write_all(kind)?;
    w.write_all(data)?;
    w.write_all(&crc32(&crc_input).to_be_bytes())
}

/// Wrap raw bytes in a zlib stream made of stored deflate blocks.
fn zlib_stored(raw: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01]; // deflate, 32K window, fastest
    if raw.is_empty() {
        out.extend_from_slice(&[0x01, 0x00, 0x00, 0xFF, 0xFF]);
    }
    let mut chunks = raw.chunks(0xFFFF).peekable();
    while let Some(c) = chunks.next() {
        let final_block = chunks.peek().is_none();
        out.push(if final_block { 1 } else { 0 });
        let len = c.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(c);
    }
    out.extend_from_slice(&adler32(raw).to_be_bytes());
    out
}

/// Write an RGB565 buffer as an 8-bit RGB PNG, scaled by `scale` (nearest
/// neighbour, so each device pixel stays a crisp block).
pub fn write_rgb565(
    path: &str,
    buf: &[u16],
    width: usize,
    height: usize,
    stride: usize,
    scale: usize,
) -> io::Result<()> {
    assert!(scale >= 1);
    let (ow, oh) = (width * scale, height * scale);

    // Filter byte 0 (None) per scanline, then RGB triplets.
    let mut raw = Vec::with_capacity(oh * (1 + ow * 3));
    for y in 0..oh {
        raw.push(0);
        let src_row = &buf[(y / scale) * stride..];
        for x in 0..ow {
            let px = src_row[x / scale];
            // Replicate the high bits into the low ones so 0x1F maps to 0xFF
            // rather than 0xF8; otherwise every preview reads slightly dark.
            let r = ((px >> 11) & 0x1F) as u8;
            let g = ((px >> 5) & 0x3F) as u8;
            let b = (px & 0x1F) as u8;
            raw.push((r << 3) | (r >> 2));
            raw.push((g << 2) | (g >> 4));
            raw.push((b << 3) | (b >> 2));
        }
    }

    let f = std::fs::File::create(path)?;
    let mut w = io::BufWriter::new(f);
    w.write_all(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A])?;

    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(ow as u32).to_be_bytes());
    ihdr.extend_from_slice(&(oh as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8bpc, truecolour RGB
    chunk(&mut w, b"IHDR", &ihdr)?;
    chunk(&mut w, b"IDAT", &zlib_stored(&raw))?;
    chunk(&mut w, b"IEND", &[])?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_the_known_check_value() {
        // The standard CRC-32 check value for "123456789".
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn adler32_matches_the_rfc_example() {
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    #[test]
    fn zlib_stream_is_well_formed() {
        let raw = vec![0xABu8; 70000]; // forces two stored blocks
        let z = zlib_stored(&raw);
        assert_eq!(&z[0..2], &[0x78, 0x01]);
        // First block not final, second final.
        assert_eq!(z[2], 0);
        let len = u16::from_le_bytes([z[3], z[4]]);
        assert_eq!(len, 0xFFFF);
        assert_eq!(u16::from_le_bytes([z[5], z[6]]), !0xFFFFu16);
        let second = 2 + 5 + 0xFFFF;
        assert_eq!(z[second], 1, "second block must be final");
        assert_eq!(&z[z.len() - 4..], &adler32(&raw).to_be_bytes());
    }
}
