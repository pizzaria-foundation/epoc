//! Link the vendored libopus.
//!
//! Built by `vendor/libopus/build.sh`, not here: the flags there are load bearing and
//! documented in place, and a second copy of them in this file would drift from them
//! silently. This only points the linker at whichever of the two builds matches the
//! target — the host one so `cargo test` can decode a real voice message, the cross one
//! for the handset.

use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root");
    let vendor = root.join("vendor/libopus");

    // The device target is a custom JSON one, so anything that is not a normal host
    // triple wants the cross build.
    let target = std::env::var("TARGET").unwrap_or_default();
    let device = target.contains("symbian") || target.contains("none");
    let dir = vendor.join(if device { "build/arm" } else { "build/host" });

    println!("cargo:rerun-if-changed={}", vendor.join("build.sh").display());
    println!("cargo:rerun-if-changed={}", dir.join("libopus.a").display());

    if !dir.join("libopus.a").exists() {
        // A clear instruction beats a linker error listing forty undefined Opus symbols.
        println!(
            "cargo:warning=libopus not built for this target; run: bash vendor/libopus/build.sh {}",
            if device { "device" } else { "host" }
        );
        return;
    }
    println!("cargo:rustc-link-search=native={}", dir.display());
    println!("cargo:rustc-link-lib=static=opus");
}
