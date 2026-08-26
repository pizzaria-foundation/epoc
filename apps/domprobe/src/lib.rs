//! domprobe — does the DOM bridge run on this handset, and where does it stop?
//!
//! # Why this exists, and why it should have existed first
//!
//! The bridge was wired straight into the browser, which has a window. A GUI application cannot be
//! replaced while it runs, so every attempt cost somebody closing it and opening it again — and the
//! bridge needed several attempts, because three plausible causes had to be eliminated before the
//! real one could be found. That made a person part of a debugging loop that does not need one.
//!
//! `apps/httpprobe` had already established the pattern and the reason: headless, so it holds no
//! window group, closes itself, and can be pushed over and re-run from the desktop with nobody in
//! the room. This is that, pointed at the bridge.
//!
//! It answers one question per document: how far into `dom_build` execution got. The breadcrumb in
//! `C:\Data\domstage.txt` names the last call reached, so a crash localises to one function instead
//! of to a span containing three candidates.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use symbian_report::{push_i64, Report};

/// Documents to try, smallest first.
///
/// Ordered so the report reads as a bisect: if the one-element document fails, nothing about the
/// larger ones is informative. Each adds exactly one thing the previous did not have.
pub const CASES: &[(&str, &str)] = &[
    ("bare", "<p>hello</p>"),
    ("html-wrapped", "<html><body><p>hello</p></body></html>"),
    ("with-doctype", "<!DOCTYPE html><html><body><p>hi</p></body></html>"),
    ("nested", "<div><div><div><p>deep</p></div></div></div>"),
    ("inline-style", "<p style=\"color:red\">styled</p>"),
    // The first case with a stylesheet, which is the first to build a real select context with an
    // author sheet in it.
    ("style-block", "<style>p { color: #00ff00 }</style><p>green</p>"),
    ("a-link", "<p>see <a href=\"/x\">this</a></p>"),
    ("an-image", "<img src=\"a.png\" width=\"64\" height=\"48\">"),
    ("a-list", "<ul><li>one</li><li>two</li></ul>"),
    ("a-table", "<table><tr><td>a</td><td>b</td></tr></table>"),
    ("entities", "<p>a &amp; b &mdash; c</p>"),
    ("script", "<script>var x = 1 < 2;</script><p>after</p>"),
];

/// The bare-`PushL` step, which is the probe's own rather than the bridge's.
///
/// First, because it is the narrowest: everything the bridge does that touches platform C++ goes
/// through the cleanup stack, so if this fails nothing above it means anything.
pub const STEP_CLEANUP: usize = 0;

/// The same push with no TRAP of its own — the shape every call from a job actually has.
pub const STEP_CLEANUP_BARE: usize = 1;

/// How many steps this probe owns before the bridge's list begins.
const OWN_STEPS: usize = 2;

/// How many steps the bisect runs: the bridge's list plus the cleanup probe in front.
pub fn step_count() -> usize {
    symbian_dom::SELFTEST_STEPS.len() + OWN_STEPS
}

/// The name of one step, in the probe's numbering.
pub fn step_name(step: usize) -> &'static str {
    match step {
        STEP_CLEANUP => "pushl_in_trap",
        STEP_CLEANUP_BARE => "pushl_bare",
        _ => symbian_dom::SELFTEST_STEPS[step - OWN_STEPS],
    }
}

/// The opcode the worker phase uses. Above `OP_APP_BASE`, as an app-defined job must be.
pub const OP_PARSE: i32 = symbian::work::OP_APP_BASE;

/// Run one primitive of the bisect on the worker. The payload is the step index.
pub const OP_SELFTEST: i32 = symbian::work::OP_APP_BASE + 1;

/// The heap ceiling and buffer size the **browser** asks for, copied deliberately.
///
/// The point of the worker phase is to differ from the browser in nothing but the document, so a
/// number tuned here would make a pass meaningless.
pub const WORKER_HEAP: usize = 6 * 1024 * 1024;
pub const WORKER_CAP: usize = 2 * 1024 * 1024;

/// The stack the worker gets, and the candidate this run is testing.
///
/// The default is 64 KB, chosen when the deepest thing on a worker was a bignum ladder over fixed
/// arrays. `dom_hubbub_parser_create` builds libhubbub's tokeniser and treebuilder and libdom's
/// document, and it is the call the worker dies inside — with a document of `<p>hello</p>`, which
/// rules out anything about the content.
///
/// 128 KB and not more: a committed stack is real memory, and the platform has a ceiling of its own
/// well below what looks reasonable. `RThread::Create` with 256 KB answers `KErrTooBig` (-40) on this
/// handset — measured, and it presents as a job that never runs rather than as a stack too small,
/// which is a confusing way to find out.
pub const WORKER_STACK: usize = 80 * 1024;

/// Stack sizes to try, largest first.
///
/// Measured rather than chosen, because two guesses were already wrong: 256 KB and 128 KB both come
/// back `KErrTooBig` (-40) from `RThread::Create`, while the 64 KB default creates the thread fine.
/// So the platform's ceiling is somewhere in between and nothing in the SDK's headers says where.
///
/// A first run measured the ceiling between 80 and 88 KB: 128, 112, 96 and 88 KB all came back
/// `KErrTooBig`, and 80 KB created the thread. The range here narrows that and confirms it, since
/// the number decides how much stack the bisect below — and eventually the browser — can ask for.
pub const STACKS: &[usize] = &[
    88 * 1024,
    84 * 1024,
    80 * 1024,
    72 * 1024,
    64 * 1024,
];

/// The worker's side of the parse.
///
/// Identical to what the browser's layout job does, minus the layout: the same call, the same
/// buffer size, on a thread with its own heap. Every case that passed on the main thread runs again
/// here, and the only thing that changed is the thread.
pub fn worker_dispatch(opcode: i32, input: &[u8], out: &mut [u8]) -> i32 {
    if out.len() < 8 {
        return -2;
    }
    if opcode == OP_SELFTEST {
        let step = *input.first().unwrap_or(&0) as usize;
        let err = match step {
            STEP_CLEANUP => symbian::work::cleanup_probe(),
            STEP_CLEANUP_BARE => symbian::work::cleanup_probe_bare(),
            _ => match symbian_dom::selftest(step - OWN_STEPS) {
                Ok(()) => 0i32,
                Err(e) => code_of(e),
            },
        };
        out[..4].copy_from_slice(&0u32.to_le_bytes());
        out[4..8].copy_from_slice(&(err as u32).to_le_bytes());
        // Zero for the same reason as the parse below: the job succeeded at running the step, and
        // whether the step itself worked is the payload, not the job's status.
        return 0;
    }
    if opcode != OP_PARSE {
        return -1;
    }
    let pal = symbian_dom::Palette::default();
    match symbian_dom::parse_with_cap(input, 320, pal, WORKER_CAP) {
        Ok(tree) => {
            let n = tree.len() as u32;
            out[..4].copy_from_slice(&n.to_le_bytes());
            out[4..8].copy_from_slice(&0u32.to_le_bytes());
            0
        }
        Err(e) => {
            out[..4].copy_from_slice(&0u32.to_le_bytes());
            out[4..8].copy_from_slice(&(code_of(e) as u32).to_le_bytes());
            // Zero, not the error: a job that returns non-zero is reported as a failed job, and this
            // one succeeded at finding out that the parse failed. The distinction matters — a failed
            // job means the thread died, and that is the thing being looked for.
            0
        }
    }
}

/// What one case produced.
#[derive(Clone, Debug)]
pub struct Row {
    pub name: &'static str,
    /// Node count on success, or 0.
    pub nodes: usize,
    /// The bridge's error code, or 0.
    pub err: i32,
    /// Free RAM after, in KB.
    pub free_kb: u32,
    /// Node count when the same case ran on the worker thread, or 0.
    pub worker_nodes: usize,
    /// The bridge's error on the worker, 0 for none.
    pub worker_err: i32,
    /// Set when the worker never answered at all — which is the failure the browser showed, and the
    /// one that no error code can describe.
    pub worker_silent: bool,
    /// Whether the worker phase reached this case at all.
    ///
    /// Its own field because the first run printed eleven rows as "worker: 0 nodes" when the phase
    /// had ended after the first case — defaults rendered as measurements, which is the same mistake
    /// this project has now made three times in three different reports. A row nobody ran has to say
    /// so.
    pub worker_ran: bool,
}

/// Which part of the probe is running.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Phase {
    /// Every case on this thread.
    Main,
    /// One document at each stack size, to find the platform's ceiling.
    Stacks,
    /// Each primitive, alone, on the worker — the bisect.
    Selftest,
    /// Every case again, on the worker, at whatever stack worked.
    Worker,
    Reporting,
}

/// What one bisect step did, on each thread.
#[derive(Clone, Debug)]
pub struct SelfRow {
    pub step: usize,
    /// The step's own result on this thread, or 0.
    pub main_err: i32,
    pub main_ran: bool,
    pub worker_err: i32,
    /// False means the worker never answered — the step took the thread with it.
    pub worker_ran: bool,
    /// How the thread died, when it did: exit type, reason, and category.
    ///
    /// Asked of the kernel rather than guessed. `KERN-EXEC 3` and `E32USER-CBase 69` are different
    /// bugs with different fixes, and "the job never answered" does not distinguish them — which is
    /// how five wrong diagnoses got made before anything asked.
    pub exit_type: i32,
    pub exit_reason: i32,
    pub exit_cat: String,
    /// Whether this step was submitted to the worker at all.
    ///
    /// The first run without it printed `KILLED THE THREAD` for two steps that never ran, because
    /// the run ends at the first kill and their defaults read as measurements. Same trap as three
    /// earlier ones in this probe, in a new place: a field that is not filled must not be printable
    /// as a result.
    pub worker_tried: bool,
}

/// What one stack size did.
#[derive(Copy, Clone, Debug)]
pub struct StackRow {
    pub bytes: usize,
    /// The error from submitting, or 0. `-40` is `KErrTooBig` from `RThread::Create`.
    pub submit_err: i32,
    /// Whether the job answered.
    pub answered: bool,
    /// The parse's own result, when it answered.
    pub parse_err: i32,
    pub nodes: usize,
}

pub struct Probe {
    rows: Vec<Row>,
    at: usize,
    phase: Phase,
    started: bool,
    done: bool,
    path: String,
    job: symbian::work::Job,
    /// Ticks the current worker case has been waiting.
    waited: u32,
    stacks: Vec<StackRow>,
    stack_at: usize,
    selfs: Vec<SelfRow>,
    self_at: usize,
    /// The largest stack that both created a thread and answered. Zero if none did.
    good_stack: usize,
}

impl Probe {
    pub fn new() -> Self {
        // A one-shot on the first tick, like every probe here: bring-up before the first tick has
        // nowhere to report a failure.
        let _ = symbian::timer_after(1);
        Probe {
            rows: Vec::new(),
            at: 0,
            phase: Phase::Main,
            started: false,
            done: false,
            path: String::new(),
            // The largest case plus a header, and the browser's output size. Held for the run: the
            // shim keeps raw pointers into these while a job is out.
            job: symbian::work::Job::with_capacity(4096, 64),
            waited: 0,
            stacks: Vec::new(),
            stack_at: 0,
            selfs: Vec::new(),
            self_at: 0,
            good_stack: 0,
        }
    }

    /// Run one case. Separate from the loop so a crash names the case it was on: the breadcrumb file
    /// carries the *stage*, and the report carries every case that finished before it.
    fn run_one(&mut self) {
        let (name, html) = CASES[self.at];
        symbian::log!("[domprobe] case {} starting", name);

        let pal = symbian_dom::Palette::default();
        // A megabyte rather than the two-megabyte default: this runs on the GUI thread, where the
        // process heap is already carrying the report, and the documents are tiny. If the allocation
        // itself is the fault, a smaller one says so.
        let row = match symbian_dom::parse_with_cap(html.as_bytes(), 320, pal, 256 * 1024) {
            Ok(tree) => Row {
                name,
                nodes: tree.len(),
                err: 0,
                free_kb: symbian::mem::free_kb().unwrap_or(0),
                worker_nodes: 0,
                worker_err: 0,
                worker_silent: false,
                worker_ran: false,
            },
            Err(e) => Row {
                name,
                nodes: 0,
                err: code_of(e),
                free_kb: symbian::mem::free_kb().unwrap_or(0),
                worker_nodes: 0,
                worker_err: 0,
                worker_silent: false,
                worker_ran: false,
            },
        };
        symbian::log!("[domprobe] case {} -> nodes={} err={}", name, row.nodes, row.err);
        self.rows.push(row);
        self.at += 1;
    }

    /// Submit the smallest document at the stack size under test.
    fn submit_stack(&mut self) {
        let bytes = STACKS[self.stack_at];
        self.waited = 0;
        self.job.set_worker_heap(WORKER_HEAP);
        self.job.set_worker_stack(bytes);
        symbian::log!("[domprobe] stack {} bytes", bytes);
        // The cheapest primitive, not a parse. The first version submitted the parse here, and the
        // parse is the thing that hangs — so the one stack size the platform did accept came back
        // "never answered", a job that cannot be abandoned, and a Job unusable for the bisect that
        // was the whole point. This probe asks one question at a time.
        let mut row =
            StackRow { bytes, submit_err: 0, answered: false, parse_err: 0, nodes: 0 };
        if let Err(e) = self.job.submit_bytes(OP_SELFTEST, &[0u8], 8) {
            row.submit_err = e.code();
            symbian::log!("[domprobe] stack {} refused: {}", bytes, e.code());
            self.stacks.push(row);
            self.next_stack();
            return;
        }
        self.stacks.push(row);
    }

    fn next_stack(&mut self) {
        self.stack_at += 1;
        if self.stack_at < STACKS.len() {
            self.submit_stack();
        } else {
            // The largest stack `RThread::Create` *accepted*, which is the question this phase
            // asks. Not "the largest that answered": whether the job then completed is a separate
            // measurement, and conflating the two made the one accepted size read as a failure.
            self.good_stack =
                self.stacks.iter().filter(|r| r.submit_err == 0).map(|r| r.bytes).max().unwrap_or(0);
            symbian::log!("[domprobe] best stack {}", self.good_stack);
            if self.good_stack == 0 {
                // No thread at all, so neither the bisect nor the twelve cases can say anything.
                // Skipped, and the report says so rather than printing zeros as measurements.
                self.phase = Phase::Reporting;
                let _ = symbian::timer_after(1);
            } else {
                // Each primitive on this thread first: a step that fails on the GUI thread too is a
                // broken step, not a thread problem, and the report has to be able to tell them
                // apart.
                for step in 0..step_count() {
                    let err = match step {
                        STEP_CLEANUP => symbian::work::cleanup_probe(),
                        STEP_CLEANUP_BARE => symbian::work::cleanup_probe_bare(),
                        _ => match symbian_dom::selftest(step - OWN_STEPS) {
                            Ok(()) => 0,
                            Err(e) => code_of(e),
                        },
                    };
                    self.selfs.push(SelfRow {
                        step,
                        main_err: err,
                        main_ran: true,
                        worker_err: 0,
                        worker_ran: false,
                        exit_type: 0,
                        exit_reason: 0,
                        exit_cat: String::new(),
                        worker_tried: false,
                    });
                }
                self.phase = Phase::Selftest;
                self.self_at = 0;
                self.submit_self();
                let _ = symbian::timer_every(1);
            }
        }
    }

    /// Submit one bisect step to the worker.
    fn submit_self(&mut self) {
        self.waited = 0;
        self.job.set_worker_heap(WORKER_HEAP);
        self.job.set_worker_stack(self.good_stack);
        let step = self.self_at;
        symbian::log!("[domprobe] selftest {} on worker", step_name(step));
        self.selfs[step].worker_tried = true;
        if let Err(e) = self.job.submit_bytes(OP_SELFTEST, &[step as u8], 8) {
            self.selfs[step].worker_err = e.code();
            self.next_self();
        }
    }

    fn next_self(&mut self) {
        self.self_at += 1;
        if self.self_at < self.selfs.len() {
            self.submit_self();
        } else {
            self.phase = Phase::Worker;
            self.at = 0;
            self.submit_worker();
            let _ = symbian::timer_every(1);
        }
    }

    /// Submit one case to the worker.
    fn submit_worker(&mut self) {
        let (name, html) = CASES[self.at];
        self.waited = 0;
        self.job.set_worker_heap(WORKER_HEAP);
        // The largest the platform actually gave us, found above.
        self.job.set_worker_stack(if self.good_stack > 0 { self.good_stack } else { WORKER_STACK });
        self.rows[self.at].worker_ran = true;
        symbian::log!("[domprobe] worker case {} submitting", name);
        if let Err(e) = self.job.submit_bytes(OP_PARSE, html.as_bytes(), 8) {
            self.rows[self.at].worker_err = e.code();
            symbian::log!("[domprobe] worker case {} refused: {}", name, e.code());
            self.next_worker();
        }
    }

    fn next_worker(&mut self) {
        self.at += 1;
        if self.at < CASES.len() {
            self.submit_worker();
        } else {
            self.phase = Phase::Reporting;
            let _ = symbian::timer_after(1);
        }
    }

    /// The worker answered.
    fn worker_event(&mut self, ev: &symbian_sys::ShimEvent) {
        let Some(result) = self.job.on_event(ev) else { return };

        if self.phase == Phase::Selftest {
            let step = self.self_at;
            match result {
                Ok(bytes) if bytes.len() >= 8 => {
                    self.selfs[step].worker_ran = true;
                    self.selfs[step].worker_err =
                        i32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
                }
                Ok(_) => self.selfs[step].worker_err = -7,
                Err(e) => {
                    self.selfs[step].worker_ran = true;
                    self.selfs[step].worker_err = e.code();
                }
            }
            symbian::log!(
                "[domprobe] selftest {} -> {}",
                step_name(step),
                self.selfs[step].worker_err
            );
            self.next_self();
            return;
        }

        if self.phase == Phase::Stacks {
            let i = self.stacks.len().saturating_sub(1);
            match result {
                Ok(bytes) if bytes.len() >= 8 => {
                    self.stacks[i].answered = true;
                    self.stacks[i].nodes =
                        u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
                    self.stacks[i].parse_err =
                        i32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
                }
                Ok(_) => self.stacks[i].parse_err = -7,
                Err(e) => {
                    self.stacks[i].answered = true;
                    self.stacks[i].parse_err = e.code();
                }
            }
            symbian::log!(
                "[domprobe] stack {} -> nodes={} err={}",
                self.stacks[i].bytes,
                self.stacks[i].nodes,
                self.stacks[i].parse_err
            );
            self.next_stack();
            return;
        }

        let i = self.at.min(self.rows.len().saturating_sub(1));
        match result {
            Ok(bytes) if bytes.len() >= 8 => {
                let nodes = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                let err = i32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
                self.rows[i].worker_nodes = nodes as usize;
                self.rows[i].worker_err = err;
                symbian::log!(
                    "[domprobe] worker case {} -> nodes={} err={}",
                    CASES[i].0,
                    nodes,
                    err
                );
            }
            Ok(_) => self.rows[i].worker_err = -7,
            Err(e) => {
                self.rows[i].worker_err = e.code();
                symbian::log!("[domprobe] worker case {} job failed: {}", CASES[i].0, e.code());
            }
        }
        self.next_worker();
    }

    fn write_report(&mut self) {
        let mut r = Report::new("domprobe");
        r.head("The DOM bridge: libhubbub, libdom and libcss on the handset");
        r.line("");
        r.line("Each case adds one thing the previous did not have, so the last one that");
        r.line("succeeded names what the bridge can do. C:\\Data\\domstage.txt carries the");
        r.line("last call reached, which is what a case that never returns leaves behind.");
        r.line("");

        for row in &self.rows {
            let mut line = String::from(row.name);
            while line.len() < 16 {
                line.push(' ');
            }
            if row.err == 0 {
                line.push_str("ok, ");
                push_i64(&mut line, row.nodes as i64);
                line.push_str(" nodes");
            } else {
                line.push_str("FAILED ");
                push_i64(&mut line, row.err as i64);
                line.push_str(match row.err {
                    -1 => " (argument)",
                    -2 => " (out of memory)",
                    -3 => " (parse)",
                    -4 => " (select context)",
                    -5 => " (does not fit)",
                    -100 => " (no bridge in this build)",
                    _ => "",
                });
            }
            line.push_str("   worker: ");
            if !row.worker_ran {
                line.push_str("not reached");
            } else if row.worker_silent {
                line.push_str("NEVER ANSWERED");
            } else if row.worker_err != 0 {
                line.push_str("err ");
                push_i64(&mut line, row.worker_err as i64);
            } else {
                push_i64(&mut line, row.worker_nodes as i64);
                line.push_str(" nodes");
            }
            line.push_str("   ");
            push_i64(&mut line, row.free_kb as i64);
            line.push_str(" KB free");
            r.line(&line);
        }

        r.line("");
        r.head("worker stack: what this handset will give a thread");
        for st in &self.stacks {
            let mut line = String::new();
            push_i64(&mut line, (st.bytes / 1024) as i64);
            line.push_str(" KB  ");
            if st.submit_err != 0 {
                line.push_str("thread refused, err ");
                push_i64(&mut line, st.submit_err as i64);
                if st.submit_err == -40 {
                    line.push_str(" (KErrTooBig)");
                }
            } else if !st.answered {
                line.push_str("thread created, job NEVER ANSWERED");
            } else if st.parse_err != 0 {
                line.push_str("thread created, job err ");
                push_i64(&mut line, st.parse_err as i64);
            } else {
                line.push_str("thread created, job ok");
            }
            r.line(&line);
        }
        r.num("largest stack RThread::Create accepted, KB", (self.good_stack / 1024) as i64);
        r.line("");

        r.head("one primitive at a time, GUI thread then worker");
        for sr in &self.selfs {
            let mut line = String::new();
            line.push_str(step_name(sr.step));
            while line.len() < 16 {
                line.push(' ');
            }
            line.push_str("main ");
            if !sr.main_ran {
                line.push_str("not run");
            } else if sr.main_err == 0 {
                line.push_str("ok");
            } else {
                push_i64(&mut line, sr.main_err as i64);
            }
            line.push_str("   worker ");
            if !sr.worker_tried {
                line.push_str("not attempted");
                r.line(&line);
                continue;
            }
            if !sr.worker_ran {
                if sr.worker_err != 0 {
                    line.push_str("refused ");
                    push_i64(&mut line, sr.worker_err as i64);
                } else if sr.exit_cat.is_empty() {
                    line.push_str("KILLED THE THREAD, no exit info");
                } else {
                    // The kernel's own words. The category names the subsystem and the reason names
                    // the panic, and together they say which bug this is.
                    line.push_str("died ");
                    line.push_str(&sr.exit_cat);
                    line.push(' ');
                    push_i64(&mut line, sr.exit_reason as i64);
                    line.push_str(" (type ");
                    push_i64(&mut line, sr.exit_type as i64);
                    line.push(')');
                }
            } else if sr.worker_err == 0 {
                line.push_str("ok");
            } else {
                push_i64(&mut line, sr.worker_err as i64);
            }
            r.line(&line);
        }
        r.line("");

        let ok = self.rows.iter().filter(|x| x.err == 0).count();
        let w_ok = self
            .rows
            .iter()
            .filter(|x| x.worker_ran && !x.worker_silent && x.worker_err == 0)
            .count();
        let w_silent = self.rows.iter().filter(|x| x.worker_silent).count();
        r.line("");
        r.check_note(
            "the same cases pass on the worker thread",
            w_ok == ok && w_silent == 0,
            "the browser's only difference from this probe is the thread",
        );
        r.num("cases parsed on the worker", w_ok as i64);
        r.num("cases the worker never answered", w_silent as i64);
        r.num(
            "cases the worker phase never reached",
            self.rows.iter().filter(|x| !x.worker_ran).count() as i64,
        );
        r.line("");
        r.check_note(
            "the bridge runs at all",
            ok > 0,
            "if zero, nothing about libdom or libcss on this handset is known yet",
        );
        r.check_note("every case parsed", ok == CASES.len(), "the first failure is the boundary");
        r.num("cases run", self.rows.len() as i64);
        r.num("cases parsed", ok as i64);
        r.num("cases attempted", CASES.len() as i64);

        let mut fs = symbian::fs::ShimFs;
        r.open_output(&mut fs, "", "domprobe.txt");
        r.finish(&mut fs);
        self.path = String::from(r.path_label());
        symbian::log!("[domprobe] report at {}", self.path.as_str());
    }
}

fn code_of(e: symbian_dom::Error) -> i32 {
    use symbian_dom::Error as E;
    match e {
        E::Argument => -1,
        E::NoMemory => -2,
        E::Parse => -3,
        E::Css => -4,
        E::TooLarge => -5,
        E::Malformed => -6,
        E::Internal(c) => c,
        E::NotAvailable => -100,
    }
}

impl Default for Probe {
    fn default() -> Self {
        Self::new()
    }
}

impl symbian_app::DaemonApp for Probe {
    fn handle_raw(&mut self, ev: &symbian_sys::ShimEvent) {
        if self.done {
            return;
        }
        // Kind first, phase second — a phase check cannot rescue an event a kind filter dropped.
        if ev.kind == symbian_sys::SHIM_EV_WORK_DONE {
            self.worker_event(ev);
            return;
        }
        if ev.kind != symbian_sys::SHIM_EV_TIMER {
            return;
        }
        if !self.started {
            self.started = true;
            symbian::log!("[domprobe] {} cases", CASES.len());
        }

        // One case per tick, not all in a loop.
        //
        // Deliberate: a case that hangs must not take the ones after it with it, and a case that
        // faults leaves every earlier row already in memory. It also means the report is written
        // even when a later case would have killed the process — which is the whole point of a
        // bisect.
        match self.phase {
            Phase::Main => {
                if self.at < CASES.len() {
                    self.run_one();
                    let _ = symbian::timer_after(1);
                    return;
                }
                // The same cases again, on a thread with its own heap. This is the only difference
                // between a probe that passed and a browser that did not.
                symbian::log!("[domprobe] main thread done; stack phase");
                self.phase = Phase::Stacks;
                self.stack_at = 0;
                self.submit_stack();
                let _ = symbian::timer_every(1);
            }
            Phase::Stacks => {
                self.waited = self.waited.saturating_add(1);
                if self.waited > 60 {
                    symbian::log!("[domprobe] stack {} never answered", STACKS[self.stack_at]);
                    if self.job.abandon().is_err() {
                        // A thread genuinely still running holds this Job's buffers, so nothing
                        // further can be submitted. Reported rather than pretended past.
                        symbian::log!("[domprobe] worker still running; ending the stack phase");
                        self.phase = Phase::Reporting;
                        let _ = symbian::timer_after(1);
                        return;
                    }
                    self.next_stack();
                }
                // Nothing else: `next_stack` walks to the bisect once the sizes run out.
            }
            Phase::Selftest => {
                self.waited = self.waited.saturating_add(1);
                if self.waited > 60 {
                    // The step took the thread with it. That is the answer, so it is recorded as
                    // such and the bisect stops: every later step needs a thread this one killed.
                    // Asked before `abandon`, while the dead thread's handle is still open.
                    if let Ok((ty, reason, cat)) = self.job.last_exit() {
                        let step = self.self_at;
                        self.selfs[step].exit_type = ty;
                        self.selfs[step].exit_reason = reason;
                        self.selfs[step].exit_cat = cat;
                    }
                    symbian::log!(
                        "[domprobe] selftest {} killed the worker: {} {}",
                        step_name(self.self_at),
                        self.selfs[self.self_at].exit_cat,
                        self.selfs[self.self_at].exit_reason
                    );
                    let _ = self.job.abandon();
                    self.phase = Phase::Reporting;
                    let _ = symbian::timer_after(1);
                }
            }
            Phase::Worker => {
                // A worker that never answers is the failure being hunted, so it has a budget and
                // is recorded rather than waited on.
                self.waited = self.waited.saturating_add(1);
                if self.waited > 100 {
                    let i = self.at;
                    self.rows[i].worker_silent = true;
                    symbian::log!("[domprobe] worker case {} never answered", CASES[i].0);
                    // The Job is still marked busy and its buffers are still held by a thread that
                    // may be alive. Abandoning is refused in that case, and then nothing further can
                    // be submitted — so the phase ends here rather than pretending to continue.
                    if self.job.abandon().is_err() {
                        symbian::log!("[domprobe] worker still running; ending the phase");
                        self.phase = Phase::Reporting;
                        let _ = symbian::timer_after(1);
                        return;
                    }
                    self.next_worker();
                }
            }
            Phase::Reporting => {
                self.write_report();
                self.done = true;
            }
        }
    }

    fn should_exit(&self) -> bool {
        self.done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every case is named and non-empty, because a row with no name is a row nobody can act on.
    #[test]
    fn every_case_is_named_and_has_markup() {
        for (name, html) in CASES {
            assert!(!name.is_empty());
            assert!(!html.is_empty(), "{name} has no document");
        }
    }

    /// The order is the bisect: the first case must be the smallest thing that can fail.
    #[test]
    fn the_first_case_is_the_smallest() {
        let (_, first) = CASES[0];
        for (_, html) in &CASES[1..] {
            assert!(first.len() <= html.len(), "the bisect must start small");
        }
    }

    /// Every error maps to a code a report can name.
    #[test]
    fn every_error_has_a_code() {
        use symbian_dom::Error as E;
        for e in [E::Argument, E::NoMemory, E::Parse, E::Css, E::TooLarge, E::Malformed] {
            assert!(code_of(e) < 0);
        }
        assert_eq!(code_of(E::Internal(-42)), -42);
    }

    /// On the host there is no bridge, and the probe must record that rather than appearing to pass.
    #[test]
    fn the_host_records_the_missing_bridge() {
        let mut p = Probe::new();
        p.run_one();
        assert_eq!(p.rows.len(), 1);
        assert_ne!(p.rows[0].err, 0, "the host has no bridge; a zero here would be a lie");
    }
}
