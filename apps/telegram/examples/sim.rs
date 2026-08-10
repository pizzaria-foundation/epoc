//! `cargo run -p tg --example sim` — the client in the simulator, on the desktop.
//!
//! Five lines, and that is the point: [`symbian_sim::run`] takes anything implementing
//! `symbian_ui::App`, so a project's runner is this file with one name changed. It lives
//! under `examples/` with `symbian-sim` in `[dev-dependencies]`, which keeps the windowing
//! library away from the device build entirely.

fn main() {
    if let Err(e) = symbian_sim::run(tg::App::mock()) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
