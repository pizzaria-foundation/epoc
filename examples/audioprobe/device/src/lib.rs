//! The device build of the audio probe.

#![no_std]
#![no_main]

extern crate alloc;

symbian_app::entry!(audioprobe::AudioProbe::new());
