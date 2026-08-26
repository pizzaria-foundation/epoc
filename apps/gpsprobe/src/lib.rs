//! gpsprobe — Marco 0 of the map plan: does this handset answer with a position, and how fast?
//!
//! # The three questions, in the order they have to be asked
//!
//! 1. **Is `lbs.dll` importable?** The devdump sweep says it loads, which is a stronger statement
//!    than "the file exists" and still not the same as "it answers". A binary that vanishes on
//!    launch answers this one, negatively, before any code here runs.
//! 2. **What modules are there?** `RPositionServer::GetNumModules` and the per-module inventory:
//!    an integrated GPS, an assisted one, a Bluetooth receiver, the network. Each carries its own
//!    accuracy and its own advertised time to first fix, and those two numbers are what decide
//!    whether a map can wait for a position or must be usable without one.
//! 3. **Does a fix actually arrive, and when?** The advertised TTFF is the module's opinion. This
//!    probe measures the real one, outdoors, with a stopwatch made of pump ticks.
//!
//! # Satellite info is measured, not assumed
//!
//! `TPositionSatelliteInfo` carries the satellite counts; `TPositionInfo` does not. Whether a given
//! module accepts the richer class is a property of that module, and `shim_lbs.cpp` deliberately
//! does not guess — it takes `want_satellites` as a parameter so this probe can ask both ways and
//! report which one this handset answered. A shim that silently retried would have hidden exactly
//! the fact the map needs.
//!
//! # Nothing here waits
//!
//! A cold GPS start is minutes. Every route to a position is an event, and the probe is a state
//! machine driven by a periodic timer — the same shape as `apps/httpprobe`, and for a stronger
//! reason: `User::WaitForRequest` on a thread with a running scheduler is the stray-signal panic
//! that `shim_tele.cpp` documents and `shim_process.cpp` paid for.
//!
//! # Testable without a phone, and without the sky
//!
//! The state machine is generic over [`Location`], so the tests below replay a refused satellite
//! class, a timeout, and a fix arriving on the tenth tick — none of which needs a device. What
//! needs the device is every number in the report.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use symbian::location::{Fix, Location, Module, ShimLocation};
use symbian::net::RawEvent;
use symbian_report::{push_i64, Report};

/// The pump tick, and the resolution of every duration in the report. One second: a TTFF is tens
/// of seconds at best, so finer would be false precision and coarser would lose the difference
/// between a warm start and a cold one.
const TICK_MS: i32 = 1000;

/// How long a single attempt may run before this probe calls it a failure and moves on.
///
/// Deliberately longer than any TTFF a module advertises. A cold start with no almanac is minutes
/// of a receiver hearing nothing, and a probe that gives up at sixty seconds would report "no GPS
/// on this handset" about a handset whose GPS works — which is the worst thing an instrument can
/// do. Six minutes is long enough to be wrong about, and it is bounded so an unattended run ends.
const ATTEMPT_TICKS: u32 = 360;

/// Ticks to keep running after the report is written, so the log flush lands before the process
/// ends. Same reason as every other probe here.
const LINGER_TICKS: u32 = 3;

/// `TTechnologyType`, from `lbscommon.h`. A bitmask: a module may carry more than one.
pub const TECH_TERMINAL: i32 = 0x01;
/// A fix that came from the network alone — a cell tower, and no satellites by construction.
pub const TECH_NETWORK: i32 = 0x02;
/// A receiver assisted by the network. Still satellites, just helped to them faster.
pub const TECH_ASSISTED: i32 = 0x04;

/// What one fix attempt asks the framework for.
///
/// The two fields are the whole experiment: which position class, and how long the module is
/// allowed to take. See [`plan_attempt`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Attempt {
    /// Ask for `TPositionSatelliteInfo` rather than `TPositionInfo`.
    pub want_satellites: bool,
    /// Passed to the framework as the update timeout. 0 means "take as long as you need", which is
    /// not the same as this probe's own [`ATTEMPT_TICKS`] ceiling: one is the module giving up,
    /// the other is the probe giving up on the module.
    pub timeout_ms: i32,
}

/// How one attempt ended.
#[derive(Clone, Debug, PartialEq)]
pub struct AttemptRow {
    pub attempt: Attempt,
    /// Ticks from the request going out to the completion arriving, which is the measured TTFF.
    pub ticks: u32,
    /// The completion code: 0 for a fix, the platform's own otherwise.
    pub status: i32,
    /// Filled only on success.
    pub fix: Option<Fix>,
    /// True when the probe's own ceiling ended this, rather than a completion.
    pub abandoned: bool,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Phase {
    /// Nothing has happened yet; the first tick reads the inventory.
    Start,
    /// A request is out. The tick it went out on is kept in `attempt_started`.
    Waiting,
    /// The list is done and the report is written.
    Finished,
}

/// Choose what an attempt should ask for, from what the inventory said.
///
/// Called once per attempt, with the modules the framework reported and the attempts already made.
/// Returning `None` ends the probe.
///
/// # Two attempts at most, and the second one is conditional
///
/// The richer class goes first: a fix carrying satellite counts answers both questions at once,
/// and if the module refuses the class it refuses immediately — an argument error costs no sky
/// time, unlike a fix, which costs minutes. Asking cheap-first would have inverted that and spent
/// the cold start before learning anything about the class.
///
/// The second attempt exists only to separate two failures that look alike from the outside: a
/// module that refused `TPositionSatelliteInfo` and a module that could not see the sky. So it
/// runs exactly when the first attempt asked for satellites and did not get a fix. After a
/// successful first attempt there is nothing left to learn, and a second cold start outdoors is
/// minutes spent re-measuring a number already in the report.
///
/// Termination is off `done`, not off a counter: `begin_next_attempt` calls this again after every
/// row, including after a start the framework refused, and a plan that counted its own calls would
/// loop forever on a refusal.
pub fn plan_attempt(modules: &[Module], done: &[AttemptRow]) -> Option<Attempt> {
    match done {
        // First attempt. Satellite counts are worth asking for only where something could report
        // them, and `technology` is a BITMASK, not an enum of values:
        //
        //   0x01 ETechnologyTerminal   the handset's own receiver
        //   0x02 ETechnologyNetwork    the network told it where it is
        //   0x04 ETechnologyAssisted   a receiver helped by the network — A-GPS
        //
        // A purely network-based fix is a cell tower and has no satellites by construction.
        // Terminal and assisted both do, so the question is whether ANY module carries a bit
        // other than Network. This was `!= 4` at first, on the guess that 4 meant network-based;
        // the E72 answered with `Assisted GPS tech=4` and `Network based tech=2`, which is the
        // opposite, and the guess had inverted exactly the module it meant to exclude.
        [] => {
            let terminal = modules
                .iter()
                .any(|m| m.technology & (TECH_TERMINAL | TECH_ASSISTED) != 0);
            // timeout_ms 0: no ceiling on the module's side. ATTEMPT_TICKS is this probe's own
            // bound, and having only one of the two means a report can say which side gave up —
            // a platform timeout and an abandoned attempt are different findings.
            Some(Attempt { want_satellites: terminal, timeout_ms: 0 })
        }
        // Second attempt, and only for the one case that is still ambiguous.
        [first] if first.attempt.want_satellites && first.fix.is_none() => {
            Some(Attempt { want_satellites: false, timeout_ms: 0 })
        }
        _ => None,
    }
}

/// The probe.
pub struct GpsProbe<L: Location> {
    location: L,
    phase: Phase,
    ticks: u32,
    /// The tick the outstanding request went out on.
    attempt_started: u32,
    /// What the outstanding request asked for.
    current: Option<Attempt>,
    modules: Vec<Module>,
    /// The framework's own count, kept separately from `modules.len()`: a count that disagrees
    /// with the number of entries actually readable is a fact, and averaging them away would lose
    /// it.
    module_count: i32,
    /// The error from asking for the inventory at all, if it failed. `Some(0)` means it worked.
    inventory_status: Option<i32>,
    rows: Vec<AttemptRow>,
    reported: bool,
    finished_at: u32,
    report_path: String,
    exit: bool,
}

impl GpsProbe<ShimLocation> {
    pub fn new() -> Self {
        // Arming the timer is what makes the probe run at all, and it happens here rather than in
        // `with` because `with` is what the host tests use: a test drives ticks itself, and a
        // constructor that reached for the platform clock would make every one of them need a
        // phone.
        let _ = symbian::timer_every(TICK_MS);
        Self::with(ShimLocation)
    }
}

impl Default for GpsProbe<ShimLocation> {
    fn default() -> Self {
        Self::new()
    }
}

impl<L: Location> GpsProbe<L> {
    pub fn with(location: L) -> Self {
        Self {
            location,
            phase: Phase::Start,
            ticks: 0,
            attempt_started: 0,
            current: None,
            modules: Vec::new(),
            module_count: 0,
            inventory_status: None,
            rows: Vec::new(),
            reported: false,
            finished_at: 0,
            report_path: String::new(),
            exit: false,
        }
    }

    pub fn rows(&self) -> &[AttemptRow] {
        &self.rows
    }

    pub fn modules(&self) -> &[Module] {
        &self.modules
    }

    /// Read the whole module inventory. Failures are recorded rather than fatal: a framework that
    /// will not enumerate might still answer a position request, and finding that out is worth
    /// more than a clean exit.
    fn read_inventory(&mut self) {
        match self.location.module_count() {
            Ok(n) => {
                self.inventory_status = Some(0);
                self.module_count = n;
                for i in 0..n {
                    if let Ok(m) = self.location.module(i) {
                        self.modules.push(m);
                    }
                }
            }
            Err(e) => {
                self.inventory_status = Some(e.code());
                symbian::log!("[gpsprobe] module_count failed: {}", e.code());
            }
        }
    }

    /// Ask [`plan_attempt`] for the next thing to try, and start it. Ends the probe when there is
    /// nothing left to ask or the request itself is refused.
    fn begin_next_attempt(&mut self) {
        let Some(attempt) = plan_attempt(&self.modules, &self.rows) else {
            self.phase = Phase::Finished;
            return;
        };

        // interval 0: a single fix. A stream would keep the receiver powered for the life of the
        // probe and would measure the same TTFF once, which is not worth a battery.
        // Module 0: the framework's own choice. This probe is asking what the handset does
        // by default, which is the question an application inherits unless it decides otherwise.
        match self.location.start(0, attempt.timeout_ms, attempt.want_satellites, 0) {
            Ok(()) => {
                symbian::log!(
                    "[gpsprobe] attempt: satellites={} timeout_ms={}",
                    attempt.want_satellites as i32,
                    attempt.timeout_ms
                );
                self.current = Some(attempt);
                self.attempt_started = self.ticks;
                self.phase = Phase::Waiting;
            }
            Err(e) => {
                // A refused start is a result, not an error to swallow: SHIM_ERR_ACCESS_DENIED
                // here would mean the requestor declaration or the capability, and either is the
                // answer this probe exists to find.
                symbian::log!("[gpsprobe] start refused: {}", e.code());
                self.rows.push(AttemptRow {
                    attempt,
                    ticks: 0,
                    status: e.code(),
                    fix: None,
                    abandoned: false,
                });
                self.begin_next_attempt();
            }
        }
    }

    /// One attempt ended, for any reason. Closes the subscription and moves on.
    fn finish_attempt(&mut self, status: i32, fix: Option<Fix>, abandoned: bool) {
        let Some(attempt) = self.current.take() else {
            return;
        };
        self.location.stop();
        let ticks = self.ticks.saturating_sub(self.attempt_started);
        symbian::log!(
            "[gpsprobe] attempt ended: status={} ticks={} abandoned={}",
            status,
            ticks as i32,
            abandoned as i32
        );
        self.rows.push(AttemptRow { attempt, ticks, status, fix, abandoned });
        self.begin_next_attempt();
    }

    fn on_tick(&mut self) {
        self.ticks = self.ticks.saturating_add(1);

        match self.phase {
            Phase::Start => {
                self.read_inventory();
                self.begin_next_attempt();
            }
            Phase::Waiting => {
                // The probe's own ceiling, distinct from the module's timeout. A module that
                // neither fixes nor times out is a real outcome — it is what an indoor cold start
                // looks like — and it has to be bounded from this side or an unattended run never
                // ends.
                if self.ticks.saturating_sub(self.attempt_started) >= ATTEMPT_TICKS {
                    self.finish_attempt(symbian_sys::SHIM_ERR_TIMED_OUT, None, true);
                }
            }
            Phase::Finished => {
                if self.reported && self.ticks.saturating_sub(self.finished_at) >= LINGER_TICKS {
                    self.exit = true;
                }
            }
        }
    }

    fn on_fix_event(&mut self, ev: &RawEvent) {
        if self.phase != Phase::Waiting {
            return;
        }
        let fix = if ev.status == 0 { self.location.read().ok() } else { None };
        self.finish_attempt(ev.status, fix, false);
    }

    fn report_if_finished(&mut self) {
        if self.phase != Phase::Finished || self.reported {
            return;
        }
        self.reported = true;
        self.finished_at = self.ticks;
        let mut fs = symbian::fs::ShimFs;
        self.write_report(&mut fs);
        symbian::log!("[gpsprobe] report written, closing in {} ticks", LINGER_TICKS);
    }

    /// The report. Written once, when the list is done.
    pub fn write_report<F: symbian::fs::Fs>(&mut self, fs: &mut F) {
        let mut r = Report::new("gpsprobe");
        r.head("Position, through the Location Acquisition API");
        r.line("");
        r.line("Ticks are pump ticks of 1000 ms — the stopwatch. A tick count against an");
        r.line("attempt is the measured time to fix, which is the number the map's UI hangs on.");
        r.line("");

        r.entering(fs, "inventory");
        match self.inventory_status {
            Some(0) => {
                r.check("RPositionServer::Connect and GetNumModules", true);
                r.num("modules reported", self.module_count as i64);
                r.num("modules readable", self.modules.len() as i64);
            }
            Some(code) => {
                r.check_note("RPositionServer::Connect and GetNumModules", false, "see code");
                r.num("error", code as i64);
            }
            None => r.check_note("inventory", false, "never attempted"),
        }

        for m in &self.modules {
            let mut line = String::new();
            line.push_str(&m.name);
            line.push_str("  uid=0x");
            symbian_report::push_hex(&mut line, m.uid as u32, 8);
            line.push_str(if m.available { "  available" } else { "  UNAVAILABLE" });
            line.push_str("  tech=");
            push_i64(&mut line, m.technology as i64);
            line.push_str(" loc=");
            push_i64(&mut line, m.device_location as i64);
            line.push_str("  h_acc_mm=");
            push_i64(&mut line, m.horizontal_accuracy_mm as i64);
            line.push_str("  ttff_ms=");
            push_i64(&mut line, m.time_to_first_fix_ms as i64);
            line.push_str(" ttnf_ms=");
            push_i64(&mut line, m.time_to_next_fix_ms as i64);
            r.line(&line);
        }
        r.line("");
        r.line("tech is a BITMASK: 1 terminal (own receiver), 2 network (cell tower, no");
        r.line("satellites), 4 assisted (receiver helped by the network). A module may carry more");
        r.line("than one. loc: 1 internal to the device, 2 external. ttff is the module's claim.");
        r.line("");

        r.entering(fs, "fixes");
        if self.rows.is_empty() {
            r.check_note("any attempt made", false, "plan_attempt returned nothing");
        }
        for row in &self.rows {
            let mut line = String::from(if row.attempt.want_satellites {
                "satellite-info"
            } else {
                "position-only "
            });
            line.push_str("  timeout_ms=");
            push_i64(&mut line, row.attempt.timeout_ms as i64);
            line.push_str("  ");
            match (&row.fix, row.abandoned) {
                (Some(f), _) => {
                    line.push_str("FIX in ");
                    push_i64(&mut line, (row.ticks as i64) * (TICK_MS as i64));
                    line.push_str(" ms  ");
                    push_degrees(&mut line, f.lat);
                    line.push_str(", ");
                    push_degrees(&mut line, f.lon);
                    if let Some(a) = f.accuracy_m {
                        line.push_str("  +/-");
                        push_i64(&mut line, a as i64);
                        line.push('m');
                    }
                    match f.satellites_used {
                        Some(n) => {
                            line.push_str("  sats=");
                            push_i64(&mut line, n as i64);
                            if let Some(v) = f.satellites_in_view {
                                line.push('/');
                                push_i64(&mut line, v as i64);
                            }
                        }
                        None => line.push_str("  sats=not reported"),
                    }
                }
                (None, true) => {
                    line.push_str("ABANDONED after ");
                    push_i64(&mut line, (row.ticks as i64) * (TICK_MS as i64));
                    line.push_str(" ms — neither a fix nor a timeout arrived");
                }
                (None, false) => {
                    line.push_str("ERR ");
                    push_i64(&mut line, row.status as i64);
                    line.push_str(" after ");
                    push_i64(&mut line, (row.ticks as i64) * (TICK_MS as i64));
                    line.push_str(" ms");
                }
            }
            r.line(&line);
        }

        r.line("");
        r.line("A satellite-info attempt that fails where position-only succeeds means the");
        r.line("module refuses TPositionSatelliteInfo. That is a fact about the module, and the");
        r.line("map should then stop asking for satellite counts rather than retry blindly.");
        r.line("");
        r.line("-46 is a capability refused. -21 is access denied, which for this API usually");
        r.line("means SetRequestor was never called — a precondition, not a capability.");
        r.line("An indoor run reporting only timeouts has measured nothing about the receiver.");

        r.open_output(fs, "", "gpsprobe.txt");
        r.finish(fs);
        self.report_path = String::from(r.path_label());
    }
}

/// Degrees with six decimal places, which is about 11 cm — finer than any module here and coarse
/// enough to read. Written by hand because this is `no_std` and there is no float formatting.
fn push_degrees(out: &mut String, v: f64) {
    let neg = v < 0.0;
    let v = if neg { -v } else { v };
    let whole = v as i64;
    // The fraction, scaled and rounded. Done in i64 rather than by formatting the float, because
    // `no_std` has no float formatter and this needs none.
    let frac = ((v - whole as f64) * 1_000_000.0 + 0.5) as i64;
    if neg {
        out.push('-');
    }
    push_i64(out, whole);
    out.push('.');
    // Leading zeros the integer push would drop: 0.05 is not 0.5.
    let mut scale = 100_000i64;
    while scale > 1 && frac < scale {
        out.push('0');
        scale /= 10;
    }
    push_i64(out, frac);
}

impl<L: Location> symbian_app::DaemonApp for GpsProbe<L> {
    fn handle_raw(&mut self, ev: &RawEvent) {
        if ev.kind == symbian_sys::SHIM_EV_TIMER {
            self.on_tick();
            self.report_if_finished();
            return;
        }
        if ev.kind == symbian_sys::SHIM_EV_GPS_FIX {
            self.on_fix_event(ev);
            self.report_if_finished();
        }
    }

    fn should_exit(&self) -> bool {
        self.exit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use symbian::location::MemLocation;
    use symbian_app::DaemonApp;

    fn module(name: &str, available: bool, tech: i32, ttff_ms: i32) -> Module {
        Module {
            name: String::from(name),
            uid: 0x10281d45,
            available,
            technology: tech,
            device_location: 1,
            time_to_first_fix_ms: ttff_ms,
            ..Module::default()
        }
    }

    fn probe(queued: Vec<symbian::error::Result<Fix>>, modules: Vec<Module>) -> GpsProbe<MemLocation> {
        let mut loc = MemLocation::new(queued);
        loc.modules = modules;
        GpsProbe::with(loc)
    }

    fn tick(p: &mut GpsProbe<MemLocation>) {
        p.handle_raw(&RawEvent { kind: symbian_sys::SHIM_EV_TIMER, ..Default::default() });
    }

    fn fix_event(p: &mut GpsProbe<MemLocation>, status: i32) {
        p.handle_raw(&RawEvent {
            kind: symbian_sys::SHIM_EV_GPS_FIX,
            status,
            ..Default::default()
        });
    }

    #[test]
    fn the_inventory_is_read_on_the_first_tick() {
        let mut p = probe(vec![], vec![module("Integrated GPS", true, 1, 120_000)]);
        tick(&mut p);
        assert_eq!(p.modules().len(), 1);
        assert_eq!(p.modules()[0].time_to_first_fix_ms, 120_000);
    }

    #[test]
    fn a_fix_is_timed_in_ticks() {
        let here = Fix { lat: -8.05, lon: -34.9, accuracy_m: Some(9.0), ..Fix::default() };
        let mut p = probe(vec![Ok(here)], vec![module("Integrated GPS", true, 1, 120_000)]);
        tick(&mut p); // inventory, first attempt starts
        for _ in 0..9 {
            tick(&mut p);
        }
        fix_event(&mut p, 0);
        let row = &p.rows()[0];
        assert_eq!(row.ticks, 9);
        assert_eq!(row.fix.map(|f| f.lat), Some(-8.05));
    }

    #[test]
    fn an_attempt_that_never_completes_is_abandoned_not_hung() {
        let mut p = probe(vec![], vec![module("Integrated GPS", true, 1, 120_000)]);
        tick(&mut p);
        for _ in 0..ATTEMPT_TICKS {
            tick(&mut p);
        }
        assert!(p.rows().iter().any(|r| r.abandoned));
        assert_eq!(p.rows()[0].status, symbian_sys::SHIM_ERR_TIMED_OUT);
    }


    #[test]
    fn the_first_attempt_asks_for_satellites_when_a_terminal_module_exists() {
        let mods = vec![module("Integrated GPS", true, TECH_TERMINAL, 80_000)];
        let a = plan_attempt(&mods, &[]).unwrap();
        assert!(a.want_satellites);
    }

    #[test]
    fn a_network_only_handset_is_not_asked_for_satellites() {
        // tech=2 is ETechnologyNetwork: a cell tower, with no satellites to count. The first
        // version of this test passed `4` here and passed for the wrong reason — the code and the
        // test shared one wrong belief about the enum, which is how a heuristic ships inverted.
        let mods = vec![module("Network based", true, TECH_NETWORK, 12_000)];
        let a = plan_attempt(&mods, &[]).unwrap();
        assert!(!a.want_satellites);
    }

    #[test]
    fn assisted_gps_does_have_satellites_to_report() {
        // The case the old `!= 4` test got backwards: A-GPS is a real receiver the network helps,
        // so it can report satellites, and excluding it would have thrown away the module with
        // the shortest time to fix.
        let mods = vec![module("Assisted GPS", true, TECH_ASSISTED, 60_000)];
        let a = plan_attempt(&mods, &[]).unwrap();
        assert!(a.want_satellites);
    }

    #[test]
    fn the_real_e72_inventory_asks_for_satellites() {
        // Exactly what the handset reported on 25 August 2026, in its own order.
        let mods = vec![
            module("Bluetooth GPS", false, TECH_TERMINAL, 80_000),
            module("Assisted GPS", true, TECH_ASSISTED, 60_000),
            module("Integrated GPS", true, TECH_TERMINAL, 80_000),
            module("Network based", true, TECH_NETWORK, 12_000),
        ];
        assert!(plan_attempt(&mods, &[]).unwrap().want_satellites);
    }

    #[test]
    fn a_module_carrying_two_bits_is_still_a_receiver() {
        // The mask is why this matters: a terminal receiver that is also assisted reads 5, which
        // equals neither constant and must not fall through to "no satellites".
        let mods = vec![module("Hybrid", true, TECH_TERMINAL | TECH_ASSISTED, 40_000)];
        assert!(plan_attempt(&mods, &[]).unwrap().want_satellites);
    }

    #[test]
    fn a_refused_satellite_class_earns_a_plain_retry() {
        let mods = vec![module("Integrated GPS", true, 1, 120_000)];
        let first = AttemptRow {
            attempt: Attempt { want_satellites: true, timeout_ms: 0 },
            ticks: 0,
            status: symbian_sys::SHIM_ERR_ARGUMENT,
            fix: None,
            abandoned: false,
        };
        let second = plan_attempt(&mods, core::slice::from_ref(&first)).unwrap();
        assert!(!second.want_satellites);
        // And it stops there: two rows is the whole experiment.
        assert!(plan_attempt(&mods, &[first, AttemptRow {
            attempt: second,
            ticks: 3,
            status: 0,
            fix: Some(Fix::default()),
            abandoned: false,
        }])
        .is_none());
    }

    #[test]
    fn a_successful_first_attempt_ends_the_probe() {
        let mods = vec![module("Integrated GPS", true, 1, 120_000)];
        let first = AttemptRow {
            attempt: Attempt { want_satellites: true, timeout_ms: 0 },
            ticks: 40,
            status: 0,
            fix: Some(Fix::default()),
            abandoned: false,
        };
        assert!(plan_attempt(&mods, &[first]).is_none());
    }

    #[test]
    fn degrees_keep_their_leading_zeros() {
        let mut s = String::new();
        push_degrees(&mut s, -8.05);
        assert_eq!(s, "-8.050000");
        s.clear();
        push_degrees(&mut s, 34.000001);
        assert_eq!(s, "34.000001");
    }
}
