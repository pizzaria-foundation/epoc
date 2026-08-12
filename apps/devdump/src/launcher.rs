//! The orchestration state machine: run each probe, survive all of them, record what
//! happened to every one.
//!
//! # What it is defending against
//!
//! Three failures, and the design exists because they are indistinguishable if you are
//! careless:
//!
//! | what happened | how it looks | how this tells |
//! |---|---|---|
//! | the loader refused the image | nothing at all — no file, no panic, no log | `start` returns an error |
//! | the probe ran and died partway | a section file with no END sentinel | [`symbian_report::status`] |
//! | the probe hung | still alive after its deadline | the poll times out and kills it |
//!
//! The first is the one that has cost this project real days (`docs/device-notes.md`, "An
//! import that does not resolve makes the app vanish"), and the only reason it becomes
//! evidence here is that the manifest is written **before** anything is launched. An
//! absence is only a finding if something recorded that it was expected.
//!
//! # Why it is a state machine and not a loop
//!
//! Because it runs on the GUI thread. Avkon owns the loop; a `while` over the probes with
//! blocking waits inside would starve every active object in the process, which is the
//! exact bug `docs/device-notes.md` records under "The pump starved every active object at
//! its own priority". So the launcher advances one step per tick and the screen stays live.
//!
//! # Why it is generic over [`Procs`] and [`Fs`]
//!
//! All three failure modes above happen once, on a handset, at the far end of an install —
//! which is precisely what cannot be reproduced on demand. Behind the traits, the whole
//! machine runs under `cargo test` against a fake that produces any of them on request.

use alloc::string::String;
use alloc::vec::Vec;

use symbian::fs::{Fs, Utf16Path};
use symbian::process::Procs;
use symbian_report::{self as report, Report};

use crate::registry::{self, Probe, PROBES};

/// How a probe ended. Ordered from best to worst so a summary can sort by it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Not reached yet. Written into the manifest up front, so a run that dies mid-way
    /// leaves every unreached probe visibly pending rather than silently missing.
    Pending,
    /// Ran, and closed its section with the END sentinel.
    Ok { pass: u32, fail: u32 },
    /// Started and died before finishing. Its section holds everything up to the fault,
    /// and the last breadcrumb in it names the step.
    Crashed,
    /// Started, rendezvoused, and left no readable section at all.
    NoOutput,
    /// The loader refused the image. On this platform that is what an unsatisfied import
    /// looks like, and the code is the diagnosis.
    Refused(i32),
    /// Still alive when its deadline passed. Killed. The elapsed time is recorded next to
    /// it, because "a timeout is a measurement of your deadline, not of the system".
    TimedOut(i32),
}

impl Outcome {
    /// The word that goes in the manifest.
    pub fn label(&self) -> &'static str {
        match self {
            Outcome::Pending => "pending",
            Outcome::Ok { .. } => "ok",
            Outcome::Crashed => "CRASHED",
            Outcome::NoOutput => "NO OUTPUT",
            Outcome::Refused(_) => "REFUSED",
            Outcome::TimedOut(_) => "TIMED OUT",
        }
    }

    /// Whether this outcome should draw the reader's eye. Everything that is not a clean
    /// completion does — including `Pending`, which after a finished run means the
    /// launcher itself stopped early.
    pub fn is_notable(&self) -> bool {
        !matches!(self, Outcome::Ok { fail: 0, .. })
    }
}

/// Where the launcher is.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Nothing written yet.
    Start,
    /// Probe `index` is being launched this tick.
    Launch(usize),
    /// Probe `index` is running; `waited_ms` is how long it has been.
    Wait { index: usize, waited_ms: i32 },
    /// All probes done; concatenating.
    Merge,
    Done,
}

/// How often [`Launcher::tick`] is expected to be called, in milliseconds.
///
/// The launcher counts elapsed time in ticks rather than reading a clock, so that the
/// deadline logic is exercised deterministically by a test rather than by waiting. On the
/// device the caller arms a timer at this interval.
pub const TICK_MS: i32 = 250;

pub struct Launcher {
    phase: Phase,
    outcomes: Vec<Outcome>,
    report: Report,
    /// Set once the manifest has a path, so a failure to open output is visible on screen
    /// rather than producing a run whose results go nowhere.
    have_output: bool,
}

impl Launcher {
    pub fn new() -> Self {
        Launcher {
            phase: Phase::Start,
            outcomes: alloc::vec![Outcome::Pending; PROBES.len()],
            report: Report::new(registry::MANIFEST_NAME),
            have_output: false,
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn outcomes(&self) -> &[Outcome] {
        &self.outcomes
    }

    pub fn is_done(&self) -> bool {
        self.phase == Phase::Done
    }

    /// Where the manifest landed, for the screen. Empty until [`Launcher::tick`] has run
    /// once.
    pub fn output_label(&self) -> &str {
        self.report.path_label()
    }

    /// A one-line status for the screen, so a run that is standing still says which probe
    /// it is standing still on.
    pub fn status(&self) -> String {
        let mut s = String::new();
        match self.phase {
            Phase::Start => s.push_str("starting"),
            Phase::Launch(i) | Phase::Wait { index: i, .. } => {
                s.push_str(PROBES[i].name);
                if let Phase::Wait { waited_ms, .. } = self.phase {
                    s.push_str(" (");
                    report::push_i64(&mut s, (waited_ms / 1000) as i64);
                    s.push_str("s)");
                }
            }
            Phase::Merge => s.push_str("merging"),
            Phase::Done => {
                let bad = self.outcomes.iter().filter(|o| o.is_notable()).count();
                report::push_i64(&mut s, (PROBES.len() - bad) as i64);
                s.push_str(" of ");
                report::push_i64(&mut s, PROBES.len() as i64);
                s.push_str(" clean");
            }
        }
        s
    }

    /// Advance one step. Call every [`TICK_MS`] until [`Launcher::is_done`].
    ///
    /// Does at most one blocking thing per call — one launch, or one liveness poll — so
    /// the GUI thread is never held for longer than a probe's rendezvous.
    pub fn tick<F: Fs, P: Procs>(&mut self, fs: &mut F, procs: &mut P) {
        match self.phase {
            Phase::Start => {
                self.write_manifest(fs);
                self.phase = if PROBES.is_empty() { Phase::Merge } else { Phase::Launch(0) };
            }
            Phase::Launch(i) => self.launch(fs, procs, i),
            Phase::Wait { index, waited_ms } => self.poll(fs, procs, index, waited_ms),
            Phase::Merge => {
                self.merge(fs);
                self.phase = Phase::Done;
            }
            Phase::Done => {}
        }
    }

    /// The manifest, written before a single probe is launched.
    ///
    /// This is the load-bearing step. Every probe is listed as `pending` here, so that a
    /// probe whose image the loader refuses — which leaves no file, no panic and no log —
    /// still has a line in the report that later gets marked REFUSED. Without it, "the
    /// probe was never run" and "the probe vanished" would look the same, and the second
    /// is the finding.
    fn write_manifest<F: Fs>(&mut self, fs: &mut F) {
        let name = registry::filename(registry::MANIFEST_ORDER, registry::MANIFEST_NAME);
        self.report.open_output(fs, registry::DIR, &name);
        // `reachable`, not "did it write". The private-cage rung always writes and is always
        // useless: the file manager cannot see it, and since the cage is per-UID3 every
        // probe lands in a different one, so the sections cannot even be assembled.
        self.have_output = self.report.reachable();

        self.report.head("probes");
        for pr in PROBES {
            let mut line = String::from(pr.name);
            line.push_str(" [");
            line.push_str(pr.exe);
            line.push(']');
            self.report.info(&line, pr.about);
        }
        // The screen belongs here rather than in the system probe: `shim_screen_size` and
        // `shim_probe_pixel_layout` live in `shim_gfx.cpp`, which a headless probe does not
        // compile, and the launcher is the one GUI binary in the fleet.
        self.report.head("screen");
        let (mut w, mut h) = (0i32, 0i32);
        // SAFETY: both are live locals.
        if unsafe { symbian_sys::shim_screen_size(&mut w, &mut h) } >= 0 {
            self.report.num("width", w as i64);
            self.report.num("height", h as i64);
        }
        let mut fmt = 0i32;
        // SAFETY: live local.
        if unsafe { symbian_sys::shim_screen_format(&mut fmt) } >= 0 {
            self.report.num("TDisplayMode", fmt as i64);
        }
        // The raw word a pure red pixel becomes. Byte order is a fact here rather than a
        // guess, which is the only reason the blit path was ever settled.
        let mut word = 0u32;
        // SAFETY: live local.
        if unsafe { symbian_sys::shim_probe_pixel_layout(&mut word) } >= 0 {
            let mut s = String::from("0x");
            report::push_hex(&mut s, word, 8);
            self.report.info("pure red reads back as", &s);
        }

        self.report.head("run");
        self.report.flush(fs);
    }

    fn launch<F: Fs, P: Procs>(&mut self, fs: &mut F, procs: &mut P, i: usize) {
        let pr = &PROBES[i];
        // Written before the launch, not after: if this probe takes the launcher down
        // with it, the last line on disk names it. Same discipline as a probe's own
        // breadcrumbs, for the same reason.
        self.report.entering(fs, pr.name);

        let path = registry::exe_path(pr.exe);
        let Ok(path) = Utf16Path::new(&path) else {
            self.finish_probe(fs, i, Outcome::Refused(symbian_sys::SHIM_ERR_ARGUMENT));
            return;
        };
        match procs.start(&path, pr.deadline_ms) {
            Ok(()) => self.phase = Phase::Wait { index: i, waited_ms: 0 },
            // The image would not load. This is the case the manifest exists for, and it
            // arrives as a number rather than as silence.
            Err(e) => self.finish_probe(fs, i, Outcome::Refused(code_of(e))),
        }
    }

    fn poll<F: Fs, P: Procs>(&mut self, fs: &mut F, procs: &mut P, i: usize, waited_ms: i32) {
        let pr = &PROBES[i];
        if procs.is_running(pr.uid3) {
            let waited = waited_ms + TICK_MS;
            if waited >= pr.deadline_ms {
                // Killing it is the launcher's own start-with-timeout having already
                // armed a deadline on the rendezvous; past that point the probe is a
                // running process nobody is waiting for, and its section keeps whatever
                // it flushed.
                self.finish_probe(fs, i, Outcome::TimedOut(waited));
            } else {
                self.phase = Phase::Wait { index: i, waited_ms: waited };
            }
            return;
        }
        // Not running any more. Whether it *finished* is a different question, and only
        // what it left on disk can answer it.
        let outcome = match self.read_section(fs, pr) {
            Some(report::Status::Complete { pass, fail }) => Outcome::Ok { pass, fail },
            Some(report::Status::Truncated) => Outcome::Crashed,
            Some(report::Status::Malformed) | None => Outcome::NoOutput,
        };
        self.finish_probe(fs, i, outcome);
    }

    fn read_section<F: Fs>(&self, fs: &mut F, pr: &Probe) -> Option<report::Status> {
        let text = self.read_section_text(fs, pr)?;
        Some(report::status(&text))
    }

    fn read_section_text<F: Fs>(&self, fs: &mut F, pr: &Probe) -> Option<String> {
        let name = registry::filename(pr.order, pr.name);
        // The report's real directory, never its label: when the ladder falls through to
        // the private cage the label is prose, and an earlier version searched for sibling
        // sections in a path made of English.
        let mut full = String::from(self.report.dir());
        full.push_str(&name);
        let p = Utf16Path::new(&full).ok()?;
        let bytes = symbian::fs::read(fs, &p).ok()??;
        String::from_utf8(bytes).ok()
    }

    fn finish_probe<F: Fs>(&mut self, fs: &mut F, i: usize, outcome: Outcome) {
        self.outcomes[i] = outcome;
        let pr = &PROBES[i];

        let mut note = String::from(outcome.label());
        match outcome {
            Outcome::Ok { pass, fail } => {
                note.push_str("  ok=");
                report::push_i64(&mut note, pass as i64);
                note.push_str(" fail=");
                report::push_i64(&mut note, fail as i64);
            }
            Outcome::Refused(code) => {
                note.push_str("  err ");
                report::push_i64(&mut note, code as i64);
                note.push_str("  (the loader would not start the image — an unsatisfied import looks exactly like this)");
            }
            Outcome::TimedOut(ms) => {
                note.push_str("  after ");
                report::push_i64(&mut note, ms as i64);
                note.push_str(" ms of a ");
                report::push_i64(&mut note, pr.deadline_ms as i64);
                note.push_str(" ms deadline");
            }
            Outcome::Crashed => {
                note.push_str("  (section has no END sentinel; its last breadcrumb names the step)");
            }
            Outcome::NoOutput => {
                note.push_str("  (started, then left nothing readable)");
            }
            Outcome::Pending => {}
        }
        // A probe that merely fails checks is not a launcher failure: the launcher's
        // verdict is about whether the probe *ran*, and conflating the two would make a
        // handset that answers "no" look like a broken harness.
        let ran_cleanly = matches!(outcome, Outcome::Ok { .. });
        self.report.check_note(pr.name, ran_cleanly, &note);
        self.report.flush(fs);

        self.phase = if i + 1 < PROBES.len() { Phase::Launch(i + 1) } else { Phase::Merge };
    }

    /// Concatenate every section into one file. Best-effort, and last.
    ///
    /// The per-probe files are the authoritative artifact; this is a convenience so that
    /// one `grep FAIL` covers the whole run. If the launcher dies before this point,
    /// nothing measured is lost — which is the reason it happens at the end rather than
    /// incrementally.
    fn merge<F: Fs>(&mut self, fs: &mut F) {
        self.report.head("summary");
        let clean = self.outcomes.iter().filter(|o| matches!(o, Outcome::Ok { .. })).count();
        self.report.num("probes", PROBES.len() as i64);
        self.report.num("ran to completion", clean as i64);
        self.report.num("did not", (PROBES.len() - clean) as i64);
        self.report.finish(fs);

        if !self.have_output {
            return;
        }
        let dir = String::from(self.report.dir());

        let mut merged = String::new();
        merged.push_str(self.report.text());
        for pr in PROBES {
            merged.push('\n');
            match self.read_section_text(fs, pr) {
                Some(text) => merged.push_str(&text),
                None => {
                    // Recorded in the merged file too, so that reading only this one file
                    // still shows the gap. A silent omission here would undo the whole
                    // point of the manifest.
                    merged.push_str("== BEGIN ");
                    merged.push_str(pr.name);
                    merged.push_str("\n  FAIL section missing  (see the run manifest above)\n");
                }
            }
        }
        let mut full = dir;
        full.push_str(&registry::filename(registry::MERGED_ORDER, registry::MERGED_NAME));
        if let Ok(p) = Utf16Path::new(&full) {
            let _ = symbian::fs::write_atomic(fs, &p, merged.as_bytes());
        }
    }
}

impl Default for Launcher {
    fn default() -> Self {
        Self::new()
    }
}

fn code_of(e: symbian::Error) -> i32 {
    use symbian::Error as E;
    match e {
        E::NotFound => symbian_sys::SHIM_ERR_NOT_FOUND,
        E::PathNotFound => symbian_sys::SHIM_ERR_NOT_FOUND,
        E::AlreadyExists => symbian_sys::SHIM_ERR_ALREADY_EXISTS,
        E::NoMemory => symbian_sys::SHIM_ERR_NO_MEMORY,
        E::AccessDenied => symbian_sys::SHIM_ERR_PERMISSION,
        E::InUse => symbian_sys::SHIM_ERR_IN_USE,
        E::Argument => symbian_sys::SHIM_ERR_ARGUMENT,
        E::Overflow => symbian_sys::SHIM_ERR_OVERFLOW,
        E::NotReady => symbian_sys::SHIM_ERR_NOT_READY,
        E::UnexpectedEof => symbian_sys::SHIM_ERR_EOF,
        E::Platform(c) => c,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian::fs::MemFs;

    /// What a probe does when the launcher starts it.
    #[derive(Clone)]
    enum Behaviour {
        /// Writes a complete section and exits after `ticks` polls.
        Completes { ticks: i32, pass: u32, fail: u32 },
        /// Writes a BEGIN and a breadcrumb, then dies.
        Crashes,
        /// Rendezvouses and never exits.
        Hangs,
        /// The loader refuses the image: no process, no file.
        Refused(i32),
        /// Runs, exits, writes nothing.
        Silent,
    }

    /// A fake device: probes that behave as told, and a filesystem they write into.
    ///
    /// This is the point of the traits. Every behaviour above is something that happens
    /// once, on a handset, at the end of an install — and here they all happen in a
    /// millisecond, on demand, in whatever combination the test wants.
    struct Fake {
        behaviours: Vec<Behaviour>,
        /// Ticks remaining before the currently running probe exits.
        remaining: i32,
        running: Option<usize>,
        pending_write: Option<(usize, String)>,
    }

    impl Fake {
        fn new(behaviours: Vec<Behaviour>) -> Self {
            Fake { behaviours, remaining: 0, running: None, pending_write: None }
        }

        fn index_of(&self, path: &str) -> usize {
            PROBES
                .iter()
                .position(|p| path.ends_with(p.exe))
                .expect("launcher asked for an executable no probe declares")
        }
    }

    impl Procs for Fake {
        fn start(&mut self, path: &Utf16Path, _timeout_ms: i32) -> symbian::Result<()> {
            let s: String = char::decode_utf16(path.as_units().iter().copied())
                .map(|c| c.unwrap())
                .collect();
            let i = self.index_of(&s);
            match self.behaviours[i].clone() {
                Behaviour::Refused(code) => Err(symbian::Error::Platform(code)),
                Behaviour::Completes { ticks, pass, fail } => {
                    let pr = &PROBES[i];
                    let mut text = String::from("== BEGIN ");
                    text.push_str(pr.name);
                    text.push_str("\n== END ");
                    text.push_str(pr.name);
                    text.push_str(" ok=");
                    report::push_i64(&mut text, pass as i64);
                    text.push_str(" fail=");
                    report::push_i64(&mut text, fail as i64);
                    text.push('\n');
                    self.pending_write = Some((i, text));
                    self.remaining = ticks;
                    self.running = Some(i);
                    Ok(())
                }
                Behaviour::Crashes => {
                    let pr = &PROBES[i];
                    let mut text = String::from("== BEGIN ");
                    text.push_str(pr.name);
                    text.push_str("\n\n-- entering the step that killed it\n");
                    self.pending_write = Some((i, text));
                    self.remaining = 1;
                    self.running = Some(i);
                    Ok(())
                }
                Behaviour::Silent => {
                    self.remaining = 1;
                    self.running = Some(i);
                    Ok(())
                }
                Behaviour::Hangs => {
                    self.remaining = i32::MAX;
                    self.running = Some(i);
                    Ok(())
                }
            }
        }

        fn is_running(&mut self, uid3: u32) -> bool {
            let Some(i) = self.running else { return false };
            if PROBES[i].uid3 != uid3 {
                return false;
            }
            if self.remaining > 0 {
                self.remaining -= 1;
                return true;
            }
            self.running = None;
            false
        }
    }

    /// Runs a launcher to completion against a fake, writing the fake's section files into
    /// `fs` as each probe "exits".
    fn run(behaviours: Vec<Behaviour>) -> (Launcher, MemFs) {
        let mut fs = MemFs::new();
        let mut fake = Fake::new(behaviours);
        let mut l = Launcher::new();
        // Generous: the hang cases burn a tick each up to their deadline.
        for _ in 0..200_000 {
            if l.is_done() {
                break;
            }
            // The fake's section lands as soon as the probe is launched, which is when a
            // real probe would have opened its output too.
            if let Some((i, text)) = fake.pending_write.take() {
                let pr = &PROBES[i];
                let mut full = String::from("C:\\Data\\");
                full.push_str(registry::DIR);
                full.push_str(&registry::filename(pr.order, pr.name));
                let p = Utf16Path::new(&full).unwrap();
                symbian::fs::write_atomic(&mut fs, &p, text.as_bytes()).unwrap();
            }
            l.tick(&mut fs, &mut fake);
        }
        assert!(l.is_done(), "launcher did not finish");
        (l, fs)
    }

    fn all(b: Behaviour) -> Vec<Behaviour> {
        alloc::vec![b; PROBES.len()]
    }

    #[test]
    fn a_clean_run_marks_every_probe_ok() {
        let (l, _) = run(all(Behaviour::Completes { ticks: 2, pass: 5, fail: 0 }));
        for (pr, o) in PROBES.iter().zip(l.outcomes()) {
            assert_eq!(*o, Outcome::Ok { pass: 5, fail: 0 }, "{}", pr.name);
        }
    }

    /// The case the whole design is for: an image the loader refuses leaves no file, no
    /// panic and no log — and must still become a line in the report.
    #[test]
    fn a_refused_image_is_recorded_rather_than_silent() {
        let mut b = all(Behaviour::Completes { ticks: 1, pass: 1, fail: 0 });
        b[0] = Behaviour::Refused(-1);
        let (l, fs) = run(b);
        assert_eq!(l.outcomes()[0], Outcome::Refused(-1));
        // And the others still ran: one bad probe does not end the sweep.
        assert!(matches!(l.outcomes()[1], Outcome::Ok { .. }));

        let manifest = fs
            .contents("C:\\Data\\dump-00-launcher.txt")
            .map(|b| core::str::from_utf8(b).unwrap().to_string())
            .expect("no manifest written");
        assert!(manifest.contains("REFUSED"), "{manifest}");
        assert!(manifest.contains(PROBES[0].name));
    }

    /// A probe that dies mid-write is a different finding from one that never ran, and the
    /// difference is visible only in what it left behind.
    #[test]
    fn a_crashed_probe_is_distinguished_from_one_that_wrote_nothing() {
        let mut b = all(Behaviour::Completes { ticks: 1, pass: 1, fail: 0 });
        b[0] = Behaviour::Crashes;
        b[1] = Behaviour::Silent;
        let (l, _) = run(b);
        assert_eq!(l.outcomes()[0], Outcome::Crashed);
        assert_eq!(l.outcomes()[1], Outcome::NoOutput);
    }

    /// A probe that hangs must cost its own deadline and nothing more.
    #[test]
    fn a_hung_probe_times_out_and_the_run_continues() {
        let mut b = all(Behaviour::Completes { ticks: 1, pass: 1, fail: 0 });
        b[0] = Behaviour::Hangs;
        let (l, _) = run(b);
        match l.outcomes()[0] {
            Outcome::TimedOut(ms) => {
                assert!(ms >= PROBES[0].deadline_ms, "gave up early at {ms} ms");
                assert!(ms < PROBES[0].deadline_ms + TICK_MS * 2, "overran at {ms} ms");
            }
            other => panic!("expected a timeout, got {other:?}"),
        }
        for o in &l.outcomes()[1..] {
            assert!(matches!(o, Outcome::Ok { .. }));
        }
    }

    /// Every probe hanging is the worst case, and it still terminates.
    #[test]
    fn a_run_where_everything_hangs_still_finishes() {
        let (l, _) = run(all(Behaviour::Hangs));
        for o in l.outcomes() {
            assert!(matches!(o, Outcome::TimedOut(_)), "{o:?}");
        }
    }

    /// The manifest lists every probe before any of them runs, so an absence is evidence.
    #[test]
    fn the_manifest_is_written_before_anything_is_launched() {
        let mut fs = MemFs::new();
        let mut fake = Fake::new(all(Behaviour::Hangs));
        let mut l = Launcher::new();
        l.tick(&mut fs, &mut fake); // Start only
        let manifest = fs.contents("C:\\Data\\dump-00-launcher.txt").unwrap();
        let text = core::str::from_utf8(manifest).unwrap();
        for pr in PROBES {
            assert!(text.contains(pr.name), "{} missing from the manifest", pr.name);
            assert!(text.contains(pr.exe), "{} missing from the manifest", pr.exe);
        }
    }

    /// A missing section has to be visible in the merged file too, or reading only that
    /// file would show a tidy report with a silent hole in it.
    #[test]
    fn the_merge_records_the_sections_it_could_not_find() {
        let mut b = all(Behaviour::Completes { ticks: 1, pass: 2, fail: 0 });
        b[0] = Behaviour::Refused(-1);
        let (_, fs) = run(b);
        let merged = fs.contents("C:\\Data\\dump-99-merged.txt").expect("no merged file");
        let text = core::str::from_utf8(merged).unwrap();
        assert!(text.contains("section missing"), "{text}");
        // And it still carries the sections that do exist.
        assert!(text.contains("== END "), "{text}");
    }

    #[test]
    fn the_status_line_names_the_probe_being_waited_on() {
        let mut fs = MemFs::new();
        let mut fake = Fake::new(all(Behaviour::Hangs));
        let mut l = Launcher::new();
        l.tick(&mut fs, &mut fake); // manifest
        l.tick(&mut fs, &mut fake); // launch probe 0
        l.tick(&mut fs, &mut fake); // one poll
        assert!(l.status().starts_with(PROBES[0].name), "{}", l.status());
    }

    #[test]
    fn a_probe_that_fails_its_own_checks_still_counts_as_having_run() {
        let (l, _) = run(all(Behaviour::Completes { ticks: 1, pass: 3, fail: 4 }));
        assert_eq!(l.outcomes()[0], Outcome::Ok { pass: 3, fail: 4 });
        // Notable, so the screen draws attention to it — but not a launcher failure.
        assert!(l.outcomes()[0].is_notable());
    }

}
