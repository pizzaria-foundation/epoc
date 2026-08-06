//! The device build of the self test.

#![no_std]
#![no_main]

extern crate alloc;

symbian_app::entry!(selftest::SelfTest::new(), work = selftest::modpow_job);
