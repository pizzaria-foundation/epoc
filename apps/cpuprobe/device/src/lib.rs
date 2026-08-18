//! The CPU probe's device entry point.
#![no_std]
#![no_main]

symbian_app::entry!(cpuprobe::Cpuprobe::new());
