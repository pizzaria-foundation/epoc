//! The ncn probe's device entry point. Alone in its image on purpose — see the module docs.
#![no_std]
#![no_main]

symbian_app::daemon_entry!(devdump::probes::OneShot::new(
    62,
    "ncn",
    devdump::probes::ncn::run,
));
