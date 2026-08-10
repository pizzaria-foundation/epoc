//! Ogg pages, Opus packets: the container a Telegram voice message arrives in.
//!
//! A voice note is `audio/ogg` — Opus frames wrapped in Ogg, per RFC 7845. Nothing on this
//! handset can open one: the SDK's FourCC list (`mmf/common/mmffourcc.h`) stops at AMR, AAC
//! and MP3, and Opus is from 2012 against a phone from 2008. So both halves have to be
//! done here — unwrap the container, then decode the frames — and this file is the first
//! half.
//!
//! # Why a demuxer at all, rather than handing the file to libopus
//!
//! libopus decodes *packets*. It has no idea what Ogg is; `opus_decode` takes one Opus
//! packet and gives back PCM. Finding the packet boundaries is the container's job, and in
//! Ogg those boundaries are not framed in the data — they are in a lacing table in each
//! page header, and a single packet may be split across pages. So this is not a formality
//! that could be skipped by scanning for a magic number.
//!
//! # What it deliberately does not do
//!
//! **No CRC check.** Every page carries a CRC32 of itself, and verifying it means a 1 KB
//! table or a bitwise loop over every byte of a file that has already crossed an encrypted,
//! checksummed MTProto connection. The failure it protects against — a corrupt page that
//! still has a valid header — is one libopus itself rejects a moment later.
//!
//! **No seeking, no chained streams.** A voice message is one logical bitstream, played
//! from the start.

use alloc::vec::Vec;

/// `OggS`, at the start of every page. RFC 3533 calls it the capture pattern, and it is
/// what lets a parser resynchronise — which is why finding it is a search rather than an
/// assertion.
const CAPTURE: &[u8; 4] = b"OggS";

/// Bytes before the segment table: capture(4) + version(1) + type(1) + granule(8) +
/// serial(4) + sequence(4) + crc(4) + segment count(1).
const HEADER_LEN: usize = 27;

/// `header_type` bit 0: this page begins with the continuation of a packet left unfinished
/// on the previous page.
const CONTINUED: u8 = 0x01;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// No `OggS` where one was required.
    NotOgg,
    /// A page header claims more data than the file holds.
    Truncated,
    /// An Ogg version this parser does not know. Only 0 has ever existed.
    Version(u8),
    /// The first packet is not an `OpusHead`, so this is Ogg but not Opus.
    NotOpus,
    /// An `OpusHead` that is too short, or claims no channels.
    BadHead,
}

pub type Result<T> = core::result::Result<T, Error>;

/// The `OpusHead` identification packet, RFC 7845 §5.1.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Head {
    pub channels: u8,
    /// Samples to discard from the front of the decoded stream, **at 48 kHz** regardless of
    /// the original rate. Encoders prepend padding because Opus needs a moment to converge;
    /// playing it produces an audible click at the start of every voice message.
    pub pre_skip: u16,
    /// The rate of the audio *before* encoding. Informational only: Opus always decodes at
    /// 48 kHz, and this field exists so a player can report the original.
    pub input_rate: u32,
    /// Output gain in Q7.8 dB. Almost always zero; applying it is the decoder's business.
    pub output_gain: i16,
    /// 0 = mono/stereo with no mapping table, which is everything a voice message is.
    pub mapping_family: u8,
}

/// One Ogg page, located inside a buffer.
struct Page<'a> {
    /// Segment lacing values: the packet-boundary information.
    lacing: &'a [u8],
    body: &'a [u8],
    continued: bool,
    /// Total samples decodable up to the end of this page, counted at 48 kHz and including
    /// the pre-skip. The last page's value is therefore the length of the stream.
    granule: u64,
    /// Where the next page starts.
    end: usize,
}

/// Find and parse the page beginning at or after `from`.
///
/// Scans for the capture pattern rather than requiring it at `from`, because that is what
/// makes a stream with junk in front of it — or one whose first bytes were lost — parse
/// anyway. RFC 3533 designs the pattern for exactly this.
fn page_at(data: &[u8], from: usize) -> Result<Page<'_>> {
    let start = find_capture(data, from).ok_or(Error::NotOgg)?;
    if start + HEADER_LEN > data.len() {
        return Err(Error::Truncated);
    }
    let version = data[start + 4];
    if version != 0 {
        return Err(Error::Version(version));
    }
    let continued = data[start + 5] & CONTINUED != 0;
    let mut g = [0u8; 8];
    g.copy_from_slice(&data[start + 6..start + 14]);
    let granule = u64::from_le_bytes(g);
    let segments = data[start + 26] as usize;

    let lacing_at = start + HEADER_LEN;
    let body_at = lacing_at + segments;
    if body_at > data.len() {
        return Err(Error::Truncated);
    }
    let lacing = &data[lacing_at..body_at];
    // The body is exactly the sum of the lacing values — the page header is what says how
    // long the page is, so a file cannot be walked without reading it.
    let body_len: usize = lacing.iter().map(|v| *v as usize).sum();
    let end = body_at + body_len;
    if end > data.len() {
        return Err(Error::Truncated);
    }
    Ok(Page { lacing, body: &data[body_at..end], continued, granule, end })
}

fn find_capture(data: &[u8], from: usize) -> Option<usize> {
    if from >= data.len() {
        return None;
    }
    data[from..]
        .windows(CAPTURE.len())
        .position(|w| w == CAPTURE)
        .map(|i| from + i)
}

/// Walks packets out of an Ogg stream.
///
/// Packets rather than pages, because packets are what a decoder consumes. Reassembling
/// them is the whole job: a lacing value of 255 means "this segment is full and the packet
/// continues", so a packet ends at the first segment shorter than 255 — and if a page ends
/// while a packet is still open, the packet resumes on the next one.
pub struct Packets<'a> {
    data: &'a [u8],
    pos: usize,
    /// A packet begun on an earlier page and not yet finished.
    partial: Vec<u8>,
    /// Set once a page has been read whose lacing left a packet open.
    carrying: bool,
    /// Packets completed by the page just read, in order, and how many have been handed out.
    ///
    /// A page holds many packets — an Opus encoder packs 20 ms frames and a page spans
    /// hundreds of milliseconds — so a whole page is decoded at once and drained. Returning
    /// one packet per page-parse would re-walk the same lacing table for every frame.
    ready: Vec<Vec<u8>>,
    handed: usize,
}

impl<'a> Packets<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Packets {
            data,
            pos: 0,
            partial: Vec::new(),
            carrying: false,
            ready: Vec::new(),
            handed: 0,
        }
    }

    /// The next complete packet, or `None` at the end of the stream.
    ///
    /// A truncated final page yields whatever was complete before it and then `None`: a
    /// voice message cut short should play what arrived rather than fail entirely.
    pub fn next_packet(&mut self) -> Option<Vec<u8>> {
        loop {
            if self.handed < self.ready.len() {
                let p = core::mem::take(&mut self.ready[self.handed]);
                self.handed += 1;
                return Some(p);
            }

            let page = page_at(self.data, self.pos).ok()?;
            self.pos = page.end;
            self.ready.clear();
            self.handed = 0;

            // A page that does not continue us, while we are carrying, means a page was
            // lost. What is half-assembled is not a packet, and handing it to a decoder as
            // if it were would turn a dropped page into a decode error somewhere else.
            if self.carrying && !page.continued {
                self.partial.clear();
                self.carrying = false;
            }

            let mut offset = 0usize;
            for &lace in page.lacing {
                let seg = &page.body[offset..offset + lace as usize];
                offset += lace as usize;
                self.partial.extend_from_slice(seg);
                if lace < 255 {
                    // A segment shorter than 255 ends the packet. That is the only
                    // boundary marker Ogg has.
                    self.ready.push(core::mem::take(&mut self.partial));
                    self.carrying = false;
                } else {
                    self.carrying = true;
                }
            }
            // If the page produced nothing, it was one packet still open; read the next.
        }
    }
}

/// How many samples the stream decodes to, at 48 kHz, or `None` if no page parses.
///
/// Read from the last page's granule position rather than counted by decoding, so a
/// player knows the length of a voice message before spending a second decoding it — and
/// so the transcript can show `0:07` on a row nobody has opened.
///
/// The value includes the pre-skip, which is padding rather than audio, so
/// [`Head::pre_skip`] is subtracted for a duration a person would recognise.
pub fn granule_end(data: &[u8]) -> Option<u64> {
    let mut pos = 0usize;
    let mut last = None;
    while let Ok(p) = page_at(data, pos) {
        last = Some(p.granule);
        pos = p.end;
    }
    last
}

/// Duration in milliseconds, or `None` when the stream does not parse.
pub fn duration_ms(data: &[u8], head: &Head) -> Option<u32> {
    let samples = granule_end(data)?.saturating_sub(head.pre_skip as u64);
    // Opus always decodes at 48 kHz whatever the input rate was, so the divisor is fixed.
    Some((samples * 1000 / 48_000) as u32)
}

/// Parse `OpusHead` out of the first packet of the stream.
pub fn head(packet: &[u8]) -> Result<Head> {
    if packet.len() < 19 || &packet[..8] != b"OpusHead" {
        return Err(Error::NotOpus);
    }
    let channels = packet[9];
    if channels == 0 {
        return Err(Error::BadHead);
    }
    Ok(Head {
        channels,
        pre_skip: u16::from_le_bytes([packet[10], packet[11]]),
        input_rate: u32::from_le_bytes([packet[12], packet[13], packet[14], packet[15]]),
        output_gain: i16::from_le_bytes([packet[16], packet[17]]),
        mapping_family: packet[18],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// Build one Ogg page. `lacing` is given explicitly so a test can express the exact
    /// segmentation it means to exercise — which is the whole of what this parser reads.
    fn page(serial: u32, seq: u32, continued: bool, lacing: &[u8], body: &[u8]) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(CAPTURE);
        p.push(0); // version
        p.push(if continued { CONTINUED } else { 0 });
        p.extend_from_slice(&0u64.to_le_bytes()); // granule
        p.extend_from_slice(&serial.to_le_bytes());
        p.extend_from_slice(&seq.to_le_bytes());
        p.extend_from_slice(&0u32.to_le_bytes()); // crc, not checked
        p.push(lacing.len() as u8);
        p.extend_from_slice(lacing);
        p.extend_from_slice(body);
        p
    }

    fn opus_head(channels: u8, pre_skip: u16, rate: u32) -> Vec<u8> {
        let mut h = Vec::new();
        h.extend_from_slice(b"OpusHead");
        h.push(1); // version
        h.push(channels);
        h.extend_from_slice(&pre_skip.to_le_bytes());
        h.extend_from_slice(&rate.to_le_bytes());
        h.extend_from_slice(&0i16.to_le_bytes()); // gain
        h.push(0); // mapping family
        h
    }

    #[test]
    fn a_page_of_short_segments_is_one_packet_each() {
        let body = [1u8, 2, 3, 4, 5];
        let data = page(7, 0, false, &[2, 3], &body);
        let mut p = Packets::new(&data);
        assert_eq!(p.next_packet().as_deref(), Some(&[1u8, 2][..]));
        assert_eq!(p.next_packet().as_deref(), Some(&[3u8, 4, 5][..]));
        assert_eq!(p.next_packet(), None);
    }

    #[test]
    fn a_packet_spanning_two_pages_is_rejoined() {
        // The only boundary marker Ogg has is "a segment shorter than 255", so a 255-byte
        // segment at the end of a page means the packet continues on the next one. Getting
        // this wrong splits one Opus frame into two invalid ones, and libopus would report
        // a corrupt packet rather than a container bug.
        let first: Vec<u8> = (0..255u16).map(|i| i as u8).collect();
        let second = [0xAAu8, 0xBB];
        let mut data = page(7, 0, false, &[255], &first);
        data.extend_from_slice(&page(7, 1, true, &[2], &second));

        let mut p = Packets::new(&data);
        let got = p.next_packet().unwrap();
        assert_eq!(got.len(), 257);
        assert_eq!(&got[..255], &first[..]);
        assert_eq!(&got[255..], &second[..]);
        assert_eq!(p.next_packet(), None);
    }

    #[test]
    fn a_packet_that_is_an_exact_multiple_of_255_needs_a_zero_segment() {
        // The awkward case in the format: a 255-byte packet cannot be expressed by one
        // 255 lacing value, because that means "continues". It takes a trailing zero.
        // A parser that treats zero as "nothing here" loses the packet entirely.
        let body: Vec<u8> = (0..255u16).map(|i| i as u8).collect();
        let data = page(7, 0, false, &[255, 0], &body);
        let mut p = Packets::new(&data);
        let got = p.next_packet().unwrap();
        assert_eq!(got.len(), 255);
        assert_eq!(p.next_packet(), None);
    }

    #[test]
    fn a_zero_length_packet_is_still_a_packet() {
        // Opus does not emit them, but the container permits it, and silently dropping one
        // would shift every subsequent packet's position in the stream.
        let data = page(7, 0, false, &[0, 1], &[9]);
        let mut p = Packets::new(&data);
        assert_eq!(p.next_packet().as_deref(), Some(&[][..]));
        assert_eq!(p.next_packet().as_deref(), Some(&[9u8][..]));
        assert_eq!(p.next_packet(), None);
    }

    #[test]
    fn a_dropped_page_discards_the_half_packet_rather_than_emitting_it() {
        // Page 1 leaves a packet open; page 2 does not claim to continue it. Concatenating
        // them anyway would hand the decoder a frame that is two unrelated halves — which
        // decodes to noise rather than failing, and noise is worse than silence.
        let first: Vec<u8> = vec![1u8; 255];
        let mut data = page(7, 0, false, &[255], &first);
        data.extend_from_slice(&page(7, 2, false, &[3], &[7, 8, 9]));

        let mut p = Packets::new(&data);
        assert_eq!(p.next_packet().as_deref(), Some(&[7u8, 8, 9][..]));
        assert_eq!(p.next_packet(), None);
    }

    #[test]
    fn junk_before_the_first_page_is_skipped() {
        // RFC 3533 designs the capture pattern for resynchronisation, and a stream whose
        // first bytes were lost should still play.
        let mut data = vec![0xDE, 0xAD, 0xBE, 0xEF];
        data.extend_from_slice(&page(7, 0, false, &[2], &[1, 2]));
        let mut p = Packets::new(&data);
        assert_eq!(p.next_packet().as_deref(), Some(&[1u8, 2][..]));
    }

    #[test]
    fn a_truncated_final_page_yields_what_was_complete() {
        // A voice note cut short by a dropped connection should play what arrived. The
        // alternative — failing the whole file — turns a partial download into silence.
        let mut data = page(7, 0, false, &[2], &[1, 2]);
        let mut half = page(7, 1, false, &[9], &[0; 9]);
        half.truncate(half.len() - 4);
        data.extend_from_slice(&half);

        let mut p = Packets::new(&data);
        assert_eq!(p.next_packet().as_deref(), Some(&[1u8, 2][..]));
        assert_eq!(p.next_packet(), None, "the truncated page is not half-emitted");
    }

    #[test]
    fn the_identification_packet_is_read() {
        let h = opus_head(1, 312, 48_000);
        let got = head(&h).unwrap();
        assert_eq!(
            got,
            Head {
                channels: 1,
                pre_skip: 312,
                input_rate: 48_000,
                output_gain: 0,
                mapping_family: 0
            }
        );
    }

    #[test]
    fn something_that_is_not_opus_is_refused_rather_than_misread() {
        // Ogg carries Vorbis, FLAC, Theora and more. Reading a Vorbis header as OpusHead
        // would produce a plausible channel count and a nonsense pre-skip.
        let mut vorbis = vec![1u8];
        vorbis.extend_from_slice(b"vorbis");
        vorbis.extend_from_slice(&[0; 24]);
        assert_eq!(head(&vorbis), Err(Error::NotOpus));
        assert_eq!(head(b"OpusHead"), Err(Error::NotOpus), "too short to be a header");
    }

    #[test]
    fn a_head_claiming_no_channels_is_refused() {
        // Zero channels would make every downstream buffer size zero and the decode a
        // divide by zero, far from here.
        let mut h = opus_head(0, 0, 48_000);
        h[9] = 0;
        assert_eq!(head(&h), Err(Error::BadHead));
    }

    /// A real Ogg/Opus file, encoded by libopus through ffmpeg at voice settings.
    ///
    /// Every other test in this file builds its own pages, which proves only that the
    /// parser agrees with the test's idea of the format. This one is the known-good
    /// artifact — the same technique `docs/device-notes.md` used to find the `codeBase`
    /// bug, where diffing against a genuine Symbian binary answered in one line what two
    /// days of reasoning had not.
    static REAL: &[u8] = include_bytes!("testdata/voice.opus");

    #[test]
    fn a_real_opus_file_walks() {
        // Counted independently, straight from the lacing rule, by a script that shares no
        // code with the parser: 4 pages, 73 packets.
        let mut p = Packets::new(REAL);

        let first = p.next_packet().expect("identification packet");
        let h = head(&first).expect("real files start with OpusHead");
        assert_eq!(h.channels, 1);
        assert_eq!(h.input_rate, 48_000);
        assert!(h.pre_skip > 0, "an encoder always prepends convergence padding");
        assert_eq!(h.mapping_family, 0);

        let second = p.next_packet().expect("comment packet");
        assert_eq!(&second[..8], b"OpusTags");

        let mut frames = 0;
        while let Some(f) = p.next_packet() {
            assert!(!f.is_empty(), "an audio frame is never empty");
            frames += 1;
        }
        assert_eq!(frames + 2, 73, "packet count must match the independent walk");
    }

    #[test]
    fn a_real_file_reports_the_duration_it_was_encoded_with() {
        // Encoded as 1.4 s. The granule is what lets a transcript print a length without
        // decoding, so being wrong here is a row that lies about a message.
        let h = head(&Packets::new(REAL).next_packet().unwrap()).unwrap();
        let ms = duration_ms(REAL, &h).expect("a real file has a granule");
        assert!((1350..=1450).contains(&ms), "expected about 1400 ms, got {ms}");

        // And the raw granule includes the pre-skip, which the duration must not.
        let raw = granule_end(REAL).unwrap();
        assert_eq!(ms, ((raw - h.pre_skip as u64) * 1000 / 48_000) as u32);
    }

    #[test]
    fn a_real_file_truncated_mid_stream_still_yields_its_whole_packets() {
        // What a dropped connection leaves. The count must simply be lower, not zero, and
        // nothing half-assembled may escape.
        let cut = &REAL[..REAL.len() * 2 / 3];
        let mut p = Packets::new(cut);
        let mut n = 0;
        while p.next_packet().is_some() {
            n += 1;
        }
        assert!(n > 2, "the headers and some frames survive");
        assert!(n < 73, "but not all of them");
    }

    #[test]
    fn a_whole_voice_message_shaped_stream_walks() {
        // The realistic shape: OpusHead, OpusTags, then pages of 20 ms frames. What this
        // pins is the count — a demuxer that loses or duplicates one frame produces audio
        // that is subtly the wrong length, which is much harder to notice than a failure.
        let head_p = opus_head(1, 312, 48_000);
        let mut tags = Vec::from(&b"OpusTags"[..]);
        tags.extend_from_slice(&0u32.to_le_bytes()); // vendor length
        tags.extend_from_slice(&0u32.to_le_bytes()); // comment count

        let mut data = page(1, 0, false, &[head_p.len() as u8], &head_p);
        data.extend_from_slice(&page(1, 1, false, &[tags.len() as u8], &tags));

        // 50 frames of 40 bytes each, spread over two pages.
        let frame = [0x5Au8; 40];
        let lacing: Vec<u8> = core::iter::repeat(40u8).take(25).collect();
        let body: Vec<u8> = core::iter::repeat(frame).take(25).flatten().collect();
        data.extend_from_slice(&page(1, 2, false, &lacing, &body));
        data.extend_from_slice(&page(1, 3, false, &lacing, &body));

        let mut p = Packets::new(&data);
        let first = p.next_packet().unwrap();
        assert_eq!(head(&first).unwrap().channels, 1);
        let second = p.next_packet().unwrap();
        assert_eq!(&second[..8], b"OpusTags");

        let mut frames = 0;
        while let Some(f) = p.next_packet() {
            assert_eq!(f.len(), 40);
            frames += 1;
        }
        assert_eq!(frames, 50);
    }
}
