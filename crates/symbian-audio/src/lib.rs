//! Audio the handset cannot open on its own.
//!
//! S60 3rd Edition FP2 knows AMR, AAC and MP3 — its FourCC list (`mmf/common/mmffourcc.h`)
//! stops there, and Opus is from 2012 against a phone from 2008. So an Opus voice message
//! has to be taken apart and turned into something the platform will play, and this crate is
//! the whole of that path:
//!
//! ```text
//! bytes ──[ogg]──▶ Opus packets ──[opus]──▶ PCM i16 ──[wav]──▶ RIFF/WAVE ──▶ MMF plays it
//! ```
//!
//! Written for the Telegram client's voice notes and lifted out of it unchanged, because
//! none of it is about Telegram: [`wav`] in particular encodes a fact about this platform
//! that cost a device round trip to find — MMF picks its format plugin by matching the
//! header against a detection string, and the shipped WAV plugin registers
//! `RIFF????WAVE`, so the container is not optional.
//!
//! `no_std`, host-testable, and the decode path is exercised against a real voice message in
//! `src/testdata/`.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod ogg;
pub mod opus;
pub mod wav;
