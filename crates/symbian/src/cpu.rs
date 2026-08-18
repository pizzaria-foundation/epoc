//! CPU time, and how to turn it into a load figure.
//!
//! Symbian has no "CPU %" to read. What it has is [`RThread::GetCpuTime`] — the cumulative
//! microseconds one thread has spent on the processor — and utilisation is the difference between
//! two readings divided by the wall-clock time between them. That is the whole method; everything
//! else here is bookkeeping around it.
//!
//! ```text
//!   t0 ── read cpu(app) ──┐
//!                          │  wall clock elapses
//!   t1 ── read cpu(app) ──┘   load = (cpu1 - cpu0) / (t1 - t0)
//! ```
//!
//! **Whether this handset accounts for it at all is a measurement, not an assumption.** On some
//! Symbian 9.x kernels the accounting is a build option and the call answers `KErrNotSupported`;
//! the header carries no documentation either way. `apps/cpuprobe` is what asks, and until it has
//! answered nothing in this SDK should draw a CPU figure. [`Error::NotSupported`] from here is the
//! platform declining, and is worth reporting as such rather than as a zero.
//!
//! Compiled in only under `USE_CPUTIME=1`. Enumerating threads is a plain kernel walk and needs no
//! capability, but it is a new import, so it earns its own gate like every other risky facility.

use alloc::string::String;
use alloc::vec::Vec;

use symbian_sys as sys;

use crate::error::{Error, Result};

/// One sample of how much processor time something has consumed since it started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    /// Cumulative CPU microseconds, summed over every thread that matched.
    pub cpu_us: i64,
    /// How many threads answered. Zero never reaches a caller — that is an error instead.
    pub threads: i32,
    /// The monotonic clock when this was taken, for the denominator.
    pub at_us: u64,
}

impl Sample {
    /// Utilisation between two samples, as a percentage of one processor.
    ///
    /// `None` when the samples are out of order, when no time has passed, or when the CPU figure
    /// went backwards — all of which mean "no answer" rather than "zero", and a task manager that
    /// printed 0% for them would be lying about an idle app.
    ///
    /// The result is deliberately *not* clamped to 100: a device with more than one hardware thread
    /// can legitimately exceed it, and quietly capping would hide that. Callers that want a bar
    /// clamp at the point of drawing.
    pub fn load_percent(&self, later: &Sample) -> Option<i32> {
        Some(self.load_tenths(later)? / 10)
    }

    /// Utilisation in **tenths** of a percent — `37` is 3.7%.
    ///
    /// The resolution the underlying numbers actually have, and the one worth showing. CPU time is
    /// counted in microseconds, so over a ten-second interval a whole percent is a hundred
    /// milliseconds: an app sitting in the background uses a small fraction of that, and rounding
    /// to whole percent turns every one of them into `0%`. That is what the first device build did,
    /// and it made a working measurement look broken.
    ///
    /// Deliberately not clamped, for the same reason as [`load_percent`].
    pub fn load_tenths(&self, later: &Sample) -> Option<i32> {
        let elapsed = later.at_us.checked_sub(self.at_us)?;
        if elapsed == 0 {
            return None;
        }
        let used = later.cpu_us.checked_sub(self.cpu_us)?;
        if used < 0 {
            return None;
        }
        Some(((used as i128 * 1000) / elapsed as i128) as i32)
    }
}

/// Sum the CPU time of every thread whose full name matches `pattern`.
///
/// A Symbian thread's full name is `process[uid]0001::threadname`, so `"foo*::*"` is every thread
/// of every process called `foo`, and `"*::*"` is the whole phone. [`of_process`] builds the usual
/// pattern for you.
pub fn sample(pattern: &str) -> Result<Sample> {
    let units: Vec<u16> = pattern.encode_utf16().collect();
    let mut cpu_us: i64 = 0;
    let mut threads: i32 = 0;
    // SAFETY: `units` outlives the call and its length is passed explicitly; the two out-params are
    // live locals the shim writes at most once each.
    let rc = unsafe {
        sys::shim_cpu_time(units.as_ptr(), units.len() as i32, &mut cpu_us, &mut threads)
    };
    Error::check(rc)?;
    Ok(Sample { cpu_us, threads, at_us: crate::monotonic_us() })
}

/// The thread-name pattern matching every thread of a process, given its name.
///
/// The name is the executable's, without the drive or extension — `TFindThread` matches against
/// the full name, whose first component is exactly that.
pub fn of_process(name: &str) -> String {
    let mut p = String::with_capacity(name.len() + 4);
    p.push_str(name);
    p.push_str("*::*");
    p
}

/// The thread-name pattern matching every thread of the process running app `uid3`.
///
/// By UID rather than by executable name, because the name the kernel reports is not always the one
/// the source spells — the probe filtered itself out by name and its own row appeared anyway. A
/// Symbian full name embeds the UID in lower-case hex between brackets, so this cannot mismatch.
pub fn of_uid(uid3: u32) -> String {
    alloc::format!("*[{uid3:08x}]*::*")
}

/// Every thread on the phone, **including the kernel's idle thread**.
///
/// On its own this is not a load figure: the idle thread exists precisely to consume whatever the
/// processor is not otherwise doing, so this total sits at 100% of wall-clock for ever. It is the
/// denominator, not the answer. [`busy_percent`] is the answer.
pub fn sample_all() -> Result<Sample> {
    sample("*::*")
}

/// The name EKA2 gives the kernel's idle thread.
///
/// Not in any SDK header — this is the platform's convention, and the reason it is a named constant
/// rather than a literal is that it is a *measurement*: if it ever stops matching, [`sample_idle`]
/// fails and the caller reports "unknown" instead of quietly calling the phone 100% busy.
pub const IDLE_THREAD: &str = "*::Null";

/// The kernel's idle thread — the time the processor spent doing nothing.
pub fn sample_idle() -> Result<Sample> {
    sample(IDLE_THREAD)
}

/// How busy the whole phone actually is, between two pairs of samples.
///
/// Busy time is total minus idle. Taking the totals alone gives 100% every time, which is true and
/// useless — the first run of the probe reported exactly that, which is how this function came to
/// exist. `None` when either pair cannot be differenced, because "we could not tell" must not
/// render as "idle".
pub fn busy_percent(all: (&Sample, &Sample), idle: (&Sample, &Sample)) -> Option<i32> {
    let elapsed = all.1.at_us.checked_sub(all.0.at_us)?;
    if elapsed == 0 {
        return None;
    }
    let total = all.1.cpu_us.checked_sub(all.0.cpu_us)?;
    let idled = idle.1.cpu_us.checked_sub(idle.0.cpu_us)?;
    let busy = total.checked_sub(idled)?;
    if busy < 0 {
        return None;
    }
    Some((((busy as i128) * 100) / elapsed as i128) as i32)
}

/// The full name of the nth running process, as the kernel spells it: `name[uid]0001`.
///
/// Walks from the start each time, so this is for a one-pass enumeration, not random access — the
/// process list is short and this keeps the shim free of a cursor whose lifetime nobody owns.
pub fn process_at(index: i32) -> Result<String> {
    // A Symbian full name maxes out at 256 units.
    let mut buf = [0u16; 272];
    let mut len: i32 = 0;
    // SAFETY: `buf` and `len` are live locals; the shim writes at most `buf.len()` units.
    let rc = unsafe { sys::shim_process_at(index, buf.as_mut_ptr(), buf.len() as i32, &mut len) };
    Error::check(rc)?;
    let n = (len.max(0) as usize).min(buf.len());
    Ok(String::from_utf16_lossy(&buf[..n]))
}

/// Every running process's full name.
pub fn processes() -> Vec<String> {
    let mut out = Vec::new();
    // A phone runs on the order of forty processes; the ceiling is a runaway guard, not a limit.
    for i in 0..256 {
        match process_at(i) {
            Ok(name) => out.push(name),
            Err(_) => break,
        }
    }
    out
}

/// The executable name from a kernel full name — `"foo[e0aa0000]0001"` becomes `"foo"`.
///
/// Useful because [`of_process`] wants the bare name, and because it is what a person reads.
pub fn short_name(full: &str) -> &str {
    match full.find('[') {
        Some(i) => &full[..i],
        None => full.split("::").next().unwrap_or(full),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(cpu_us: i64, at_us: u64) -> Sample {
        Sample { cpu_us, threads: 1, at_us }
    }

    #[test]
    fn load_is_cpu_time_over_wall_time() {
        // Half a second of CPU in one second of wall clock is 50%.
        assert_eq!(s(0, 0).load_percent(&s(500_000, 1_000_000)), Some(50));
        assert_eq!(s(0, 0).load_percent(&s(1_000_000, 1_000_000)), Some(100));
        assert_eq!(s(0, 0).load_percent(&s(0, 1_000_000)), Some(0));
    }

    #[test]
    fn a_busy_second_processor_is_not_capped() {
        // More than 100% is a real reading on multi-core hardware, and hiding it would be a lie
        // about the machine. Clamping belongs to whoever draws a bar.
        assert_eq!(s(0, 0).load_percent(&s(2_000_000, 1_000_000)), Some(200));
    }

    #[test]
    fn nonsense_pairs_answer_nothing_rather_than_zero() {
        // Zero would read as "this app is idle", which is a claim. None is the absence of one.
        assert_eq!(s(0, 1_000).load_percent(&s(0, 1_000)), None, "no time passed");
        assert_eq!(s(0, 2_000).load_percent(&s(0, 1_000)), None, "samples out of order");
        assert_eq!(s(500, 0).load_percent(&s(100, 1_000)), None, "cpu time went backwards");
    }

    #[test]
    fn a_long_uptime_does_not_overflow_the_multiply() {
        // Cumulative CPU time on a phone left on for weeks is in the tens of billions of µs; the
        // percentage is computed in i128 precisely so this cannot wrap into a negative load.
        let a = s(0, 0);
        let b = s(50_000_000_000, 100_000_000_000);
        assert_eq!(a.load_percent(&b), Some(50));
    }

    #[test]
    fn tenths_keep_the_resolution_a_whole_percent_throws_away() {
        // The measured shape: a background app using 40 ms over a ten-second window. As a whole
        // percent that is 0 — which is what made every row read "0%" on the first device build.
        let a = s(0, 0);
        let b = s(40_000, 10_000_000);
        assert_eq!(a.load_tenths(&b), Some(4), "0.4%");
        assert_eq!(a.load_percent(&b), Some(0), "and 0 when rounded to whole percent");
    }

    #[test]
    fn busy_is_total_minus_idle() {
        // The measured shape on the E72: every thread together accounts for the whole wall clock,
        // because the idle thread soaks up the remainder. Reporting that total as load gave a
        // permanent "Device: 100%", which is what this function exists to stop.
        let a0 = s(0, 0);
        let a1 = s(1_000_000, 1_000_000);
        let i0 = s(0, 0);
        let i1 = s(800_000, 1_000_000);
        assert_eq!(busy_percent((&a0, &a1), (&i0, &i1)), Some(20));
    }

    #[test]
    fn a_fully_idle_phone_is_zero_not_a_hundred() {
        let a0 = s(0, 0);
        let a1 = s(1_000_000, 1_000_000);
        assert_eq!(busy_percent((&a0, &a1), (&a0, &a1)), Some(0));
    }

    #[test]
    fn unmeasurable_busy_is_none_rather_than_idle() {
        let a0 = s(0, 0);
        let a1 = s(1_000_000, 1_000_000);
        // No time passed.
        assert_eq!(busy_percent((&a1, &a1), (&a0, &a0)), None);
        // Idle exceeding total cannot happen on a sane kernel, and if it does the honest answer is
        // "no idea" rather than a negative bar.
        let i1 = s(2_000_000, 1_000_000);
        assert_eq!(busy_percent((&a0, &a1), (&a0, &i1)), None);
    }

    #[test]
    fn the_process_pattern_matches_every_thread() {
        assert_eq!(of_process("launcher"), "launcher*::*");
    }

    #[test]
    fn the_uid_pattern_is_lower_case_hex_in_brackets() {
        // The shape the kernel writes into a full name. Upper case would match nothing, and
        // matching nothing reads as "this app is using no CPU" rather than as a typo.
        assert_eq!(of_uid(0xE0AA_0000), "*[e0aa0000]*::*");
        assert_eq!(of_uid(1), "*[00000001]*::*");
    }

    #[test]
    fn a_full_name_reduces_to_the_executable() {
        assert_eq!(short_name("launcher[e0aa0000]0001"), "launcher");
        assert_eq!(short_name("launcher[e0aa0000]0001::Main"), "launcher");
        assert_eq!(short_name("plain"), "plain");
        assert_eq!(short_name("plain::Main"), "plain");
    }

    #[test]
    fn off_device_sampling_fails_rather_than_reporting_idle() {
        // The host shim is a stub. It must not look like a phone with nothing running.
        assert!(sample_all().is_err());
        assert!(processes().is_empty());
    }
}
