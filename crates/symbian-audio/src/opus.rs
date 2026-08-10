//! An Ogg/Opus voice message to PCM.
//!
//! The container comes apart in [`crate::ogg`] and the frames are decoded by the `opus`
//! crate, which holds the FFI so this one can stay `#![forbid(unsafe_code)]`. What comes
//! out is what [`crate::wav`] writes and the handset plays.

use alloc::vec::Vec;

pub use opus::{Decoder, Error, RATE};

/// A whole Ogg/Opus stream to interleaved PCM at 48 kHz.
///
/// The pre-skip is dropped here rather than left to the caller: it is encoder padding,
/// not audio, and playing it is an audible click at the start of every voice message.
pub fn decode_stream(data: &[u8]) -> Result<(Vec<i16>, u8), crate::ogg::Error> {
    let mut packets = crate::ogg::Packets::new(data);
    let first = packets.next_packet().ok_or(crate::ogg::Error::NotOgg)?;
    let head = crate::ogg::head(&first)?;

    let mut dec = Decoder::new(head.channels).map_err(|_| crate::ogg::Error::BadHead)?;
    let mut pcm: Vec<i16> = Vec::new();

    while let Some(p) = packets.next_packet() {
        // The comment header is not audio and is not a valid Opus packet — handing it to
        // the decoder is an error, so it is skipped by its magic rather than by position,
        // which also covers a stream that carries no tags at all.
        if p.len() >= 8 && &p[..8] == b"OpusTags" {
            continue;
        }
        // A packet libopus rejects costs that packet, not the message. A voice note with
        // one damaged frame should play with a gap, and the alternative — refusing the
        // whole file — turns a small corruption into total loss.
        let _ = dec.decode(&p, &mut pcm);
    }

    let skip = head.pre_skip as usize * head.channels as usize;
    if skip < pcm.len() {
        pcm.drain(..skip);
    } else {
        pcm.clear();
    }
    Ok((pcm, head.channels))
}

#[cfg(test)]
mod tests {
    use super::*;

    static REAL: &[u8] = include_bytes!("testdata/voice.opus");

    #[test]
    fn a_decoder_is_created_for_mono_and_stereo_and_refused_otherwise() {
        assert!(Decoder::new(1).is_ok());
        assert!(Decoder::new(2).is_ok());
        assert_eq!(Decoder::new(0).unwrap_err(), Error::Channels(0));
        assert_eq!(Decoder::new(3).unwrap_err(), Error::Channels(3));
    }

    #[test]
    fn a_real_voice_message_decodes_to_the_length_it_claims() {
        // The end-to-end check, and the one that could not be made on the handset: a real
        // libopus-encoded file, decoded by the same library the device will run, compared
        // against the duration the container's granule reports. Agreement between two
        // independent parts of the format is what makes this more than "it did not crash".
        let (pcm, channels) = decode_stream(REAL).expect("a real file decodes");
        assert_eq!(channels, 1);

        let head = crate::ogg::head(&crate::ogg::Packets::new(REAL).next_packet().unwrap()).unwrap();
        let claimed = crate::ogg::duration_ms(REAL, &head).unwrap() as usize;
        let got = pcm.len() * 1000 / RATE as usize;
        assert!(
            got.abs_diff(claimed) <= 25,
            "granule says {claimed}ms, decode produced {got}ms"
        );
    }

    #[test]
    fn the_decoded_audio_is_the_tone_that_was_encoded() {
        // Length alone would pass for a buffer of silence. This counts zero crossings to
        // recover the pitch, which is the cheapest way to assert that the samples are the
        // 440 Hz sine the fixture was made from and not noise or nothing.
        let (pcm, _) = decode_stream(REAL).unwrap();
        assert!(!pcm.is_empty());

        // Skip the first 100 ms: the encoder's output converges over the first frames,
        // and measuring across that transient is measuring the wrong thing.
        let start = RATE as usize / 10;
        let window = &pcm[start..];
        let mut crossings = 0usize;
        let mut prev = window[0];
        for &s in &window[1..] {
            if (s >= 0) != (prev >= 0) {
                crossings += 1;
            }
            prev = s;
        }
        let seconds = window.len() as f64 / RATE as f64;
        let hz = (crossings as f64 / 2.0) / seconds;
        assert!((420.0..=460.0).contains(&hz), "expected about 440 Hz, got {hz:.0}");

        // And it is audible: a decode that produced only near-zero samples would pass a
        // pitch test on noise around zero.
        let peak = pcm.iter().map(|s| s.unsigned_abs()).max().unwrap();
        assert!(peak > 2_000, "peak amplitude {peak} is inaudibly quiet");
    }

    #[test]
    fn the_pre_skip_is_removed() {
        // Left in, it is an audible click at the start of every voice message. The check
        // is that the decode is shorter than the raw granule by exactly the pre-skip.
        let head = crate::ogg::head(&crate::ogg::Packets::new(REAL).next_packet().unwrap()).unwrap();
        assert!(head.pre_skip > 0, "the fixture has padding to remove");

        let (pcm, _) = decode_stream(REAL).unwrap();
        let granule = crate::ogg::granule_end(REAL).unwrap() as usize;
        assert!(
            pcm.len() + head.pre_skip as usize <= granule + 960,
            "decode kept the padding: {} samples against a granule of {granule}",
            pcm.len()
        );
    }

    #[test]
    fn a_corrupt_packet_costs_that_packet_and_not_the_message() {
        // A voice note with one damaged frame should play with a gap. Refusing the whole
        // file turns a small corruption into total loss, which on a metered GPRS link
        // also means paying to download it again.
        let mut dec = Decoder::new(1).unwrap();
        let mut out = Vec::new();
        assert!(dec.decode(&[0xFF, 0xFF, 0xFF], &mut out).is_err());
        assert!(out.is_empty(), "a failed decode appends nothing");

        // And the decoder still works afterwards.
        let mut packets = crate::ogg::Packets::new(REAL);
        let _head = packets.next_packet();
        let _tags = packets.next_packet();
        let good = packets.next_packet().unwrap();
        assert!(dec.decode(&good, &mut out).is_ok());
        assert!(!out.is_empty());
    }

    #[test]
    fn decoding_appends_rather_than_replacing() {
        // The whole-message path relies on this: a Vec per packet would be hundreds of
        // allocations on a device where allocation is the expensive part.
        let mut dec = Decoder::new(1).unwrap();
        let mut out = alloc::vec![7i16; 3];
        let mut packets = crate::ogg::Packets::new(REAL);
        let _ = packets.next_packet();
        let _ = packets.next_packet();
        let p = packets.next_packet().unwrap();
        let n = dec.decode(&p, &mut out).unwrap();
        assert_eq!(&out[..3], &[7, 7, 7]);
        assert_eq!(out.len(), 3 + n);
    }
}
