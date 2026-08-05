//! The device build of the network probe.
//!
//! `work = modpow_job` is what puts the worker thread into the link. Without a caller
//! reaching `shim_work_submit`, `--gc-sections` sweeps the whole thread facility and the
//! build succeeds while proving nothing — which it did, once.

#![no_std]
#![no_main]

extern crate alloc;

symbian_app::entry!(netprobe::NetProbe::new(), work = netprobe::modpow_job);
