//! The device build of the image probe.

#![no_std]
#![no_main]

extern crate alloc;

symbian_app::entry!(imgprobe::ImgProbe::new());
