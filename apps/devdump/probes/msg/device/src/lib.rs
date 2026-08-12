//! The msg probe's device entry point.
//!
//! Runs once and exits. Everything it knows is in `devdump::probes::msg`; this file
//! exists to be the thing the linker starts, and to name which probe this binary is.
//!
//! The order and section name come from `devdump::registry`, so a binary cannot write
//! into a file the launcher is not expecting to read.
#![no_std]
#![no_main]

symbian_app::daemon_entry!(devdump::probes::OneShot::new(
    60,
    "msg",
    devdump::probes::msg::run,
));
