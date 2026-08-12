//! The mtm probe's device entry point.
//!
//! Runs once and exits. Everything it knows is in `devdump::probes::mtm`.
#![no_std]
#![no_main]

symbian_app::daemon_entry!(devdump::probes::OneShot::new(
    61,
    "mtm",
    devdump::probes::mtm::run,
));
