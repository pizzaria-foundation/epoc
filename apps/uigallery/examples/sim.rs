//! `cargo run -p uigallery --example sim` — this app in a window, on your desktop.
//!
//! Same widgets, same rasterizer, same 320x240 canvas as the device. What it does not
//! reproduce is timing: the device runs this from a `CIdle` on a 600 MHz ARM1136, so the
//! moment a question is about speed it has to go back to the phone.

fn main() {
    if let Err(e) = symbian_sim::run(uigallery::Uigallery::new()) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
