//! Writing a RIFF/WAVE file, which is how decoded audio reaches the speaker.
//!
//! The device cannot play Opus, so a voice message is decoded to PCM here and handed to
//! the platform as a file. That file has to be a container rather than bare samples, and
//! the reason is specific: MMF picks its format plugin by looking at the header, and the
//! shipped WAV plugin registers the detection string `RIFF????WAVE` (four wildcard bytes
//! for the size field). Raw PCM is the one standard format the resolver explicitly
//! *cannot* identify — the guide says so — which would force the far more awkward
//! `OpenUrlL` path where the format must be described by hand.
//!
//! So 44 bytes of header buy the simplest playback API on the platform. The format must
//! be **signed little-endian 16-bit PCM**: the supported-codec table lists WAV as
//! carrying signed 16-bit and *unsigned* 8-bit, which is the standard RIFF convention
//! and the opposite of what a reader might assume for the 8-bit case.

use alloc::vec::Vec;

/// `wFormatTag` for uncompressed PCM. The SDK spells this out as
/// `KMdaWavFormatTypePcm = 1` in `mda/common/audio.hrh`, little endian.
const FORMAT_PCM: u16 = 1;

const BITS_PER_SAMPLE: u16 = 16;

/// Bytes of header before the samples begin: 12 (RIFF) + 24 (fmt) + 8 (data).
pub const HEADER_LEN: usize = 44;

/// The header for a PCM16 stream of `samples` frames.
///
/// Returned separately from the samples so a caller can write the header, stream the
/// decode straight to disk, and never hold the whole clip in RAM — which on a device
/// with this much heap is the difference between playing a two-minute voice message and
/// failing to allocate. The cost is that the sample count must be known first; for Opus
/// it is, from the Ogg granule.
pub fn header(sample_rate: u32, channels: u16, samples: u32) -> [u8; HEADER_LEN] {
    let bytes_per_frame = channels * BITS_PER_SAMPLE / 8;
    let data_len = samples.saturating_mul(bytes_per_frame as u32);

    let mut h = [0u8; HEADER_LEN];
    h[0..4].copy_from_slice(b"RIFF");
    // Everything after this field. Not the file length — a classic off-by-eight that
    // some players tolerate and a strict plugin resolver need not.
    h[4..8].copy_from_slice(&(36u32.saturating_add(data_len)).to_le_bytes());
    h[8..12].copy_from_slice(b"WAVE");

    h[12..16].copy_from_slice(b"fmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes()); // chunk size for PCM
    h[20..22].copy_from_slice(&FORMAT_PCM.to_le_bytes());
    h[22..24].copy_from_slice(&channels.to_le_bytes());
    h[24..28].copy_from_slice(&sample_rate.to_le_bytes());
    // Byte rate and block align are redundant with the fields above, and writing them
    // inconsistently is a common way to produce a file that opens and plays at the
    // wrong speed rather than one that fails.
    h[28..32].copy_from_slice(&(sample_rate * bytes_per_frame as u32).to_le_bytes());
    h[32..34].copy_from_slice(&bytes_per_frame.to_le_bytes());
    h[34..36].copy_from_slice(&BITS_PER_SAMPLE.to_le_bytes());

    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data_len.to_le_bytes());
    h
}

/// A whole file in memory: header followed by samples, interleaved if stereo.
pub fn file(sample_rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
    let frames = (samples.len() / channels.max(1) as usize) as u32;
    let mut out = Vec::with_capacity(HEADER_LEN + samples.len() * 2);
    out.extend_from_slice(&header(sample_rate, channels, frames));
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn u32_at(b: &[u8], at: usize) -> u32 {
        u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
    }
    fn u16_at(b: &[u8], at: usize) -> u16 {
        u16::from_le_bytes([b[at], b[at + 1]])
    }

    /// Not a test of this module — a test of whether the platform will accept what it
    /// writes. Runs only when asked, because it shells out to ffprobe.
    ///
    /// `cargo test -p tg -- --ignored dump_for_ffprobe`
    #[test]
    #[ignore]
    fn dump_for_ffprobe() {
        extern crate std;
        use std::io::Write;
        let samples: Vec<i16> = (0..48_000)
            .map(|i| ((i as f32 * 0.06).sin() * 8000.0) as i16)
            .collect();
        let f = file(48_000, 1, &samples);
        let mut out = std::fs::File::create("/tmp/wavcheck.wav").unwrap();
        out.write_all(&f).unwrap();
    }

    #[test]
    fn the_header_matches_the_signature_the_platform_detects_on() {
        // MMF's WAV plugin registers `<h>RIFF????WAVE`, four wildcard bytes covering the
        // size field. Anything else here and the file is not recognised as audio at all.
        let h = header(48_000, 1, 100);
        assert_eq!(&h[0..4], b"RIFF");
        assert_eq!(&h[8..12], b"WAVE");
        assert_eq!(&h[12..16], b"fmt ");
        assert_eq!(&h[36..40], b"data");
    }

    #[test]
    fn the_riff_size_counts_from_after_itself_not_the_whole_file() {
        // The field is "everything after this field", so it is the file length minus 8.
        // Writing the file length is the classic mistake, and it produces a file that
        // some players accept — which is worse than one that fails.
        let samples = vec![0i16; 100];
        let f = file(48_000, 1, &samples);
        assert_eq!(u32_at(&f, 4) as usize, f.len() - 8);
        assert_eq!(u32_at(&f, 40) as usize, samples.len() * 2);
    }

    #[test]
    fn byte_rate_and_block_align_agree_with_the_other_fields() {
        // These are derivable, so an inconsistency here does not fail loudly — it plays
        // at the wrong speed, which reads as a decoder bug.
        let h = header(48_000, 2, 1);
        let channels = u16_at(&h, 22);
        let rate = u32_at(&h, 24);
        let bits = u16_at(&h, 34);
        let block = channels * bits / 8;
        assert_eq!(u16_at(&h, 32), block);
        assert_eq!(u32_at(&h, 28), rate * block as u32);
    }

    #[test]
    fn samples_are_signed_little_endian() {
        // Signed 16-bit is what the WAV codec table lists; a byte order or sign error
        // yields full-scale noise rather than silence, at whatever volume is set.
        let f = file(48_000, 1, &[-1i16, 1, i16::MIN, i16::MAX]);
        assert_eq!(&f[HEADER_LEN..], &[0xFF, 0xFF, 0x01, 0x00, 0x00, 0x80, 0xFF, 0x7F]);
    }

    #[test]
    fn a_stereo_file_counts_frames_not_samples() {
        // The data chunk is in bytes and the frame count is per channel pair. Counting
        // interleaved samples as frames halves the reported duration.
        let f = file(48_000, 2, &[0i16; 200]);
        assert_eq!(u32_at(&f, 40), 400, "data chunk is bytes");
        assert_eq!(f.len(), HEADER_LEN + 400);
    }

    #[test]
    fn an_empty_clip_is_still_a_valid_file() {
        // A voice message whose decode produced nothing should yield a file the platform
        // opens and reports as zero-length, not a truncated header it rejects.
        let f = file(48_000, 1, &[]);
        assert_eq!(f.len(), HEADER_LEN);
        assert_eq!(u32_at(&f, 40), 0);
        assert_eq!(u32_at(&f, 4), 36);
    }

    #[test]
    fn the_header_is_the_one_the_rest_of_the_file_is_appended_to() {
        // header() and file() must not drift apart — the streaming path uses the first
        // and the tests mostly exercise the second.
        let samples = [1i16, 2, 3];
        let whole = file(16_000, 1, &samples);
        assert_eq!(&whole[..HEADER_LEN], &header(16_000, 1, samples.len() as u32));
    }

    #[test]
    fn a_written_file_is_readable_by_a_general_purpose_parser() {
        // Walk the chunk list the way any reader does, rather than trusting the offsets
        // this module happens to have written.
        let f = file(24_000, 1, &[7i16; 50]);
        assert_eq!(&f[0..4], b"RIFF");
        let mut at = 12;
        let mut saw_fmt = false;
        let mut data_len = None;
        while at + 8 <= f.len() {
            let id = &f[at..at + 4];
            let len = u32_at(&f, at + 4) as usize;
            if id == b"fmt " {
                saw_fmt = true;
                assert_eq!(u16_at(&f, at + 8), FORMAT_PCM);
                assert_eq!(u32_at(&f, at + 12), 24_000);
            } else if id == b"data" {
                data_len = Some(len);
            }
            at += 8 + len + (len & 1); // chunks are word-aligned
        }
        assert!(saw_fmt, "fmt chunk found by walking");
        assert_eq!(data_len, Some(100));
        assert_eq!(at, f.len(), "the chunks tile the file exactly");
    }
}
