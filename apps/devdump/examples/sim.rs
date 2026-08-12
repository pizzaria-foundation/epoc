//! The launcher on the desktop.
//!
//! It draws and takes keys here, but the run itself does nothing: every shim call behind
//! `ShimProcs` and `ShimFs` is a host stub. What is actually worth testing — the state
//! machine — is covered by `cargo test -p devdump`, against fakes that refuse to load,
//! crash and hang on demand.
fn main() {
    symbian_sim::run(devdump::DevDump::new()).unwrap();
}
