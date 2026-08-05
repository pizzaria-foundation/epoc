//! `cargo run -p sim` — the reference app in the simulator.
//!
//! Ten lines, and that is the point: [`symbian_sim::run`] takes anything implementing
//! `symbian_ui::App`, so a new project's runner is this file with one name changed.
//!
//! A project outside this workspace would put the same thing in `examples/sim.rs` and
//! declare `symbian-sim` under `[dev-dependencies]`, which keeps the windowing library
//! away from the device build entirely.

fn main() {
    if let Err(e) = symbian_sim::run(tg::App::mock()) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
