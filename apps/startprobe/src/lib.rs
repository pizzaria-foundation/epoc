//! When does this phone's boot actually finish — and where in it does our own code get born?
//!
//! The platform publishes its startup state at Publish & Subscribe category `0x101F8766`, key
//! `0x41`, on an enum based at 100. That is not documented in this SDK (it ships no
//! `startupdomainpskeys.h`); it was read out of the emulator's own boot binaries, which are x86 PE
//! and therefore disassemblable on the host — `aiidleint.dll` at `0x401f5b` builds a property
//! observer on exactly that category and key, and its callback compares the value against 104, 109,
//! 110 and 111 before pulling the native Active Idle to the front. A settled E72 reads 109.
//!
//! What we still do not know is the value at the instant our launcher starts, and that is the only
//! value that decides anything. It cannot be read from the host: the phone's Bluetooth stack does
//! not answer until roughly 67 s of uptime, and the launcher's own log shows it already finished a
//! 20-second foreground race with the phone at 38 s. The whole interesting window is over before
//! the remote shell exists. So the instrument has to run on the phone.
//!
//! # What it does
//!
//! Sweeps [`WATCH`] every [`SAMPLE_MS`], starting with one sweep in the constructor — that first
//! sweep, stamped with `monotonic_us`, is the headline result: the platform's state at the earliest
//! moment anything of ours runs. After that it reports transitions.
//!
//! # The trap this probe has to avoid
//!
//! `symbian::log!` appends through `fs::append_capped`, which **starts the file over** when it
//! passes [`symbian::LOG_MAX`] (64 KB). A sweep line is about 90 bytes, so logging every sample for
//! four minutes at 250 ms is ~86 KB — the file would wrap and destroy the early lines, which are
//! the entire point. An instrument that overruns its own recorder does not fail loudly; it comes
//! back with a plausible, complete-looking log of the wrong minutes. That is why what to write and
//! when to stop is a policy decision rather than a loop, and why it lives in [`step`].

#![cfg_attr(not(test), no_std)]

extern crate alloc;

/// The properties to sample, as (category, key, label).
///
/// Key `0x41` is the one that matters — it is what `aiidleint` observes. The rest are the other
/// keys that the emulator's `sysstart.exe` and `SysAp.exe` write in the same category, kept because
/// they cost one read each and because a state machine is much easier to read when you can see the
/// neighbouring markers move too. `0x101F8767/0x501` is the SysAp autolock-status candidate, along
/// for the ride for the same reason.
pub const WATCH: &[(u32, u32, &str)] = &[
    (0x101F_8766, 0x41, "state"),
    (0x101F_8766, 0x01, "k01"),
    (0x101F_8766, 0x02, "k02"),
    (0x101F_8766, 0x11, "k11"),
    (0x101F_8766, 0x31, "k31"),
    (0x101F_8766, 0x42, "k42"),
    (0x101F_8766, 0x43, "k43"),
    (0x101F_8766, 0x45, "k45"),
    (0x101F_8767, 0x501, "lock"),
];

/// How many properties [`WATCH`] holds. A const so the sample is a fixed-size array and this
/// daemon never allocates while sampling.
pub const N: usize = 9;

/// Sampling period. Fast enough to place a transition inside a second, slow enough that the
/// property reads are free next to the boot's own work.
pub const SAMPLE_MS: i32 = 250;

/// The value recorded for a property that is not defined. Distinct from every real state (the enum
/// is based at 100), so "absent" can never be mistaken for a state — the previous attempt at this
/// measurement confused exactly those two things.
pub const MISSING: i32 = i32::MIN;

/// A property that exists but this process may not read: the read policy the owner set at Define
/// time refused our SID. Kept apart from [`MISSING`] because the two demand opposite conclusions —
/// "this ROM does not define the key" sends you looking for another key, "we are not allowed" sends
/// you looking at capabilities. The first run of this probe reported four keys as absent that the
/// remote shell reads happily, which is what this distinction is for.
pub const DENIED: i32 = i32::MIN + 1;

/// The read failed for some third reason. Never silently folded into either of the above.
pub const ERRORED: i32 = i32::MIN + 2;

/// What the sampling loop should do with a sweep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Write this sweep to the log, and keep sampling.
    Log,
    /// Keep sampling, write nothing. The sweep still updates the "last seen" state, so a later
    /// transition is still detected against it.
    Skip,
    /// Stop: write a final line and let the process exit, which frees `\sys\bin` so the package
    /// can be uninstalled.
    Stop,
}

/// Decide what to do with one sweep.
///
/// Pure on purpose: it is the only judgement in this probe, so it is the only thing worth testing
/// on the host, and it can be tested without a phone.
///
/// - `sample` — how many sweeps have happened, the first being 1.
/// - `uptime_ms` — the handset's uptime, not this process's age.
/// - `changed` — whether any watched property differs from the previous sweep.
///
/// The budget is about 700 lines before the 64 KB log wraps, and the window that matters is the
/// first tens of seconds. So the density is graded rather than uniform:
///
/// - Below [`DENSE_UNTIL_MS`] every sweep is written. This is the answer we came for, and 240 lines
///   is a third of the budget for the whole of it at full resolution.
/// - After that, only transitions — plus a heartbeat every [`HEARTBEAT_EVERY`] sweeps. The
///   heartbeat is not decoration: total silence is ambiguous, because "nothing changed" and "this
///   process died" look identical in a log, and this probe runs during the one period when dying is
///   plausible.
/// - At [`STOP_AFTER_MS`] it stops and the process exits. Late enough that a transition arriving
///   after the boot settles is still caught, early enough that the file never wraps.
pub fn step(sample: u32, uptime_ms: u64, changed: bool) -> Step {
    if uptime_ms >= STOP_AFTER_MS {
        return Step::Stop;
    }
    if changed || uptime_ms < DENSE_UNTIL_MS || sample.is_multiple_of(HEARTBEAT_EVERY) {
        return Step::Log;
    }
    Step::Skip
}

/// Full resolution below this uptime. The launcher's own log has it finishing a 20-second
/// foreground race with the phone at 38 s, so the interesting window is comfortably inside this.
pub const DENSE_UNTIL_MS: u64 = 60_000;

/// Sweeps between heartbeats once the dense phase is over — 40 × 250 ms, so one line every 10 s.
pub const HEARTBEAT_EVERY: u32 = 40;

/// Stop here. Past this the boot is long settled, and every further line is budget spent on
/// nothing at the risk of wrapping away the lines that matter.
pub const STOP_AFTER_MS: u64 = 240_000;

/// One sweep of [`WATCH`].
pub type Sample = [i32; N];

pub struct Startprobe {
    ticker: Option<i32>,
    last: Sample,
    samples: u32,
    done: bool,
}

impl Startprobe {
    pub fn new() -> Self {
        let first = sweep();
        symbian::log!(
            "[startprobe] first sweep at uptime {} ms: {}",
            uptime_ms(),
            render(&first)
        );
        Self { ticker: symbian::timer_after(SAMPLE_MS).ok(), last: first, samples: 1, done: false }
    }

    /// Take a sweep, hand it to [`step`], and act on the answer.
    fn tick(&mut self) {
        let now = sweep();
        self.samples = self.samples.saturating_add(1);
        let changed = now != self.last;
        let up = uptime_ms();

        match step(self.samples, up, changed) {
            Step::Log => {
                if changed {
                    symbian::log!(
                        "[startprobe] {} ms CHANGED {} (was {})",
                        up,
                        render(&now),
                        render(&self.last)
                    );
                } else {
                    symbian::log!("[startprobe] {} ms {}", up, render(&now));
                }
            }
            Step::Skip => {}
            Step::Stop => {
                symbian::log!(
                    "[startprobe] done at {} ms after {} sweeps: {}",
                    up,
                    self.samples,
                    render(&now)
                );
                self.ticker = None;
                self.done = true;
                return;
            }
        }
        self.last = now;
        self.ticker = symbian::timer_after(SAMPLE_MS).ok();
    }
}

impl Default for Startprobe {
    fn default() -> Self {
        Self::new()
    }
}

impl symbian_app::DaemonApp for Startprobe {
    fn handle_raw(&mut self, ev: &symbian_sys::ShimEvent) {
        if ev.kind == symbian_sys::SHIM_EV_TIMER && Some(ev.handle) == self.ticker {
            self.tick();
        }
    }

    fn should_exit(&self) -> bool {
        self.done
    }
}

/// Read every watched property once, keeping the three ways a read can fail apart.
fn sweep() -> Sample {
    let mut out = [MISSING; N];
    for (i, (cat, key, _)) in WATCH.iter().enumerate().take(N) {
        out[i] = match symbian::prop::get(*cat, *key) {
            Ok(v) => v,
            Err(symbian::Error::NotFound) => MISSING,
            Err(symbian::Error::AccessDenied) => DENIED,
            Err(_) => ERRORED,
        };
    }
    out
}

/// The handset's uptime in milliseconds. `monotonic_us` counts from boot, so this is the phone's
/// clock and not this process's age — which is the whole point of the measurement.
fn uptime_ms() -> u64 {
    symbian::monotonic_us() / 1_000
}

/// `state=109 k01=100 … lock=1`, with `-` for a property that is not defined, `!` for one we are
/// not allowed to read, and `?` for any other failure.
fn render(s: &Sample) -> alloc::string::String {
    let mut out = alloc::string::String::new();
    for (i, (_, _, label)) in WATCH.iter().enumerate().take(N) {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(label);
        out.push('=');
        match s[i] {
            MISSING => out.push('-'),
            DENIED => out.push('!'),
            ERRORED => out.push('?'),
            v => out.push_str(&alloc::format!("{}", v)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watch_and_n_agree() {
        // N is what makes the sample a fixed-size array; a mismatch would silently drop the tail
        // of WATCH from every sweep, which is the sort of quiet truncation this SDK has paid for.
        assert_eq!(WATCH.len(), N);
    }

    #[test]
    fn missing_cannot_be_mistaken_for_a_state() {
        // The enum is based at 100 and aiidleint's values run to 111. MISSING must be nowhere near.
        for state in 100..=127 {
            assert_ne!(MISSING, state);
        }
    }

    #[test]
    fn render_marks_absent_properties() {
        let mut s = [100i32; N];
        s[0] = 109;
        s[N - 1] = MISSING;
        let text = render(&s);
        assert!(text.starts_with("state=109 "), "{text}");
        assert!(text.ends_with("lock=-"), "{text}");
    }
}

#[cfg(test)]
mod policy_tests {
    use super::*;

    #[test]
    fn dense_phase_logs_everything() {
        for s in 1..=240u32 {
            assert_eq!(step(s, (s as u64) * 250, false), Step::Log, "sample {s}");
        }
    }

    #[test]
    fn quiet_phase_skips_but_never_misses_a_change() {
        assert_eq!(step(300, 75_000, false), Step::Skip);
        assert_eq!(step(300, 75_000, true), Step::Log, "a transition must always be written");
    }

    #[test]
    fn heartbeat_proves_the_probe_is_alive() {
        assert_eq!(step(HEARTBEAT_EVERY * 3, 75_000, false), Step::Log);
        assert_eq!(step(HEARTBEAT_EVERY * 3 + 1, 75_000, false), Step::Skip);
    }

    #[test]
    fn stop_wins_over_everything_including_a_change() {
        assert_eq!(step(9_999, STOP_AFTER_MS, true), Step::Stop);
    }

    /// The whole point of grading the density: the run must fit the log with room to spare.
    #[test]
    fn the_run_fits_in_the_log() {
        let mut lines = 0u64;
        let mut sample = 0u32;
        loop {
            sample += 1;
            let up = (sample as u64) * SAMPLE_MS as u64;
            match step(sample, up, false) {
                Step::Stop => break,
                Step::Log => lines += 1,
                Step::Skip => {}
            }
        }
        // ~90 bytes a line, against symbian::LOG_MAX of 64 KB.
        let bytes = lines * 90;
        assert!(bytes < 64 * 1024, "{lines} lines = {bytes} bytes would wrap the log");
        assert!(lines > 200, "only {lines} lines: too coarse to place a transition");
    }
}
