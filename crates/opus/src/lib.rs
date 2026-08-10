//! Decoding Opus packets to PCM, by way of libopus.
//!
//! The caller unwraps the container; this turns the packets it yields into signed 16-bit
//! samples a WAV writer can hand to the platform.
//!
//! This crate exists separately from the client because the client is `#![forbid(unsafe_code)]`
//! — every `unsafe` in this repository lives in a crate whose job is to hold it, the same
//! arrangement as `symbian-sys` under `symbian`. What leaves here is a safe API.
//!
//! # Why a C library rather than a decoder written here
//!
//! Opus is two codecs — SILK for speech, CELT for music — plus a hybrid mode that runs
//! both and crosses over, and the bitstream is defined by RFC 6716 largely *in terms of*
//! the reference implementation. A from-scratch decoder would be thousands of lines
//! whose only test is whether it agrees with libopus, which is an odd amount of work to
//! do in order to avoid using libopus.
//!
//! It is built decode-only, `FIXED_POINT`, no floating point at all — this handset is
//! soft-float, so every float operation would be a library call. See
//! `vendor/libopus/build.sh` for the flags and the reasoning behind each.
//!
//! # Rate
//!
//! Opus always decodes at 48 kHz whatever the audio was before encoding. `OpusHead`
//! carries the original rate, but only so a player can report it — the samples that come
//! out of here are 48 kHz, and asking libopus for another rate makes it resample
//! internally, which costs time on a 600 MHz ARM to produce something the device may not
//! want anyway. Whether the device plays 48 kHz is what `examples/audioprobe` row B
//! exists to answer.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::vec::Vec;

/// What Opus decodes at, always. Not a configuration choice.
pub const RATE: u32 = 48_000;

/// The longest frame Opus defines is 120 ms. At 48 kHz stereo that is 11 520 samples,
/// and a decode buffer smaller than the largest legal frame is a buffer that fails on
/// valid input rather than on corrupt input.
const MAX_FRAME_SAMPLES: usize = 5760 * 2;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// libopus refused to create a decoder for this channel count or rate.
    Init(i32),
    /// A packet libopus would not decode. Carries its error code.
    Decode(i32),
    /// More channels than a voice message has any business carrying.
    Channels(u8),
}

// The three entry points this needs. Declared here rather than generated, because three
// stable C functions do not justify a bindgen dependency in a build that cross-compiles
// to a 2008 handset.
extern "C" {
    fn opus_decoder_get_size(channels: i32) -> i32;
    fn opus_decoder_init(st: *mut u8, fs: i32, channels: i32) -> i32;
    fn opus_decode(
        st: *mut u8,
        data: *const u8,
        len: i32,
        pcm: *mut i16,
        frame_size: i32,
        decode_fec: i32,
    ) -> i32;
}

/// A decoder, and the buffer libopus keeps its state in.
///
/// `opus_decoder_init` into memory we own, rather than `opus_decoder_create`, because
/// create calls malloc inside libopus. Owning the allocation keeps it on the same heap
/// as everything else in the process — which is what makes the handset's memory figures
/// mean anything — and removes the only reason this file would need libopus's allocator
/// wired up at all.
#[derive(Debug)]
pub struct Decoder {
    state: Vec<u8>,
    channels: u8,
}

impl Decoder {
    pub fn new(channels: u8) -> Result<Self, Error> {
        if channels != 1 && channels != 2 {
            return Err(Error::Channels(channels));
        }
        let size = unsafe { opus_decoder_get_size(channels as i32) };
        if size <= 0 {
            return Err(Error::Init(size));
        }
        let mut state = alloc::vec![0u8; size as usize];
        let rc = unsafe { opus_decoder_init(state.as_mut_ptr(), RATE as i32, channels as i32) };
        if rc != 0 {
            return Err(Error::Init(rc));
        }
        Ok(Decoder { state, channels })
    }

    pub fn channels(&self) -> u8 {
        self.channels
    }

    /// Decode one packet, appending interleaved samples to `out`.
    ///
    /// Returns how many samples per channel were produced. Appending rather than
    /// returning a fresh buffer because a voice message is hundreds of packets and a
    /// `Vec` per packet is hundreds of allocations on a device where allocation is the
    /// expensive part.
    pub fn decode(&mut self, packet: &[u8], out: &mut Vec<i16>) -> Result<usize, Error> {
        let base = out.len();
        out.resize(base + MAX_FRAME_SAMPLES, 0);
        let n = unsafe {
            opus_decode(
                self.state.as_mut_ptr(),
                packet.as_ptr(),
                packet.len() as i32,
                out[base..].as_mut_ptr(),
                (MAX_FRAME_SAMPLES / self.channels as usize) as i32,
                0,
            )
        };
        if n < 0 {
            out.truncate(base);
            return Err(Error::Decode(n));
        }
        let produced = n as usize * self.channels as usize;
        out.truncate(base + produced);
        Ok(n as usize)
    }
}

