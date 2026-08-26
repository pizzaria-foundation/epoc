//! httpprobe — F2 of the browser plan: does the platform HTTP stack work, on the GUI thread?
//!
//! # The one question, and why it needs a screen
//!
//! `apps/tlsprobe` already proved this handset can complete a real HTTPS request. It proved it
//! headless, blocking, on a private worker thread — which is exactly what a browser cannot do. So
//! this probe asks a different question with the same URLs: does a fetch through `RHTTPSession`
//! run while a window is on screen and stay off the thread that draws it?
//!
//! Which is why the answer is a **counter on screen**, not a line in a file. The probe arms a
//! periodic timer and increments a tick on every one. If the number keeps moving while a page is
//! loading, the pump was never blocked, and that is the whole finding — a log line saying "fetch
//! succeeded" would look identical whether the UI froze for six seconds or not.
//!
//! The same ticks are the stopwatch. One mechanism, two measurements: a fetch's cost in ticks is
//! its cost in frames, which is the unit that actually matters here.
//!
//! # What else it reports, and why each one is a decision
//!
//! - **gzip.** Asked for on every request. `gzip` in the header with `1f 8b` in the body means the
//!   stack handed over compressed bytes and F3 needs an inflate stage; the header without the
//!   magic means we get decompression for free. This single bit decides whether F3 has that stage
//!   at all.
//! - **chunked and parts.** Whether the stack decoded chunked transfer encoding without being
//!   asked, and in how many callbacks a body arrives — which is what streaming will look like.
//! - **redirects.** `http://google.com/` is in the list to answer whether a 301 is followed for us
//!   silently, as `thttpevent.h` claims for GET.
//! - **the failure codes.** Kept raw. R7 is that this phone's 2009 certificate store may not trust
//!   a modern root, and an untrusted certificate is only distinguishable from a dead server by its
//!   code. `letsencrypt.org` is in the list precisely because its root postdates the handset.
//!
//! # Headless, and why it stopped having a screen
//!
//! It had one, and the screen was the point: F2's finding was a tick counter that kept moving while
//! a page loaded, which no log can show. That question is answered, and everything since — inflate,
//! cache, redirects — is measured through the report and the log rather than seen.
//!
//! Keeping the window cost a run. A GUI application is one instance per UID3: launching one that is
//! already running brings the existing window group to the front instead of starting a process. A
//! run that died mid-list could leave that group behind, and the next `exec` then spawned a process
//! that exited immediately — no log, no report, no panic, indistinguishable from a binary that will
//! not load. Headless has no window group and cannot collide with its own corpse, which is what a
//! build/push/run/read loop with nobody in the room needs.
//!
//! The [`App`] implementation stays for `cargo run --example sim`, where a screen is free.
//!
//! # Isolated on purpose
//!
//! This binary adds `http.dso` and `inetprotutil.dso`, neither of which anything in this repo has
//! linked before. The project rule is one risky import set per binary, because an import that does
//! not resolve makes the application vanish with no panic, no log and no report — so a failure here
//! means "the HTTP stack is not importable", and cannot mean anything else.
//!
//! # Testable without a phone
//!
//! The state machine is generic over [`Net`] and [`Http`], so the tests below replay whole
//! sequences — a bearer that fails, a body in four parts, a target that errors mid-list — none of
//! which needs the device. What needs the device is the tick counter, and that is the point.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use symbian::http::{cache, Body, Fetch, Flags, Http, Progress, Response, ShimHttp};
use symbian::net::{Bearer, Net, RawEvent, ShimNet};
use symbian_report::{push_i64, Report};
use symbian_ui::{chrome, App, Canvas, Handled, Key, KeyEvent, Rect, Softkey, Theme};

/// How often the pump is nudged. Also the resolution of every duration in the report, which is
/// the honest way round: the browser will care about whole frames, not milliseconds.
const TICK_MS: i32 = 200;

/// Which bearer strategy this build uses.
///
/// A knob rather than a decision, because it is the experiment. The first device run used
/// [`Strategy::Attach`] — join whatever is already online — and every one of ten targets failed
/// with the same network error while `tlsprobe`, on its own bearer, resolved a name and opened TCP
/// on the same handset minutes later. That points at the connection we hand the HTTP stack rather
/// than at the stack, and `shim_net.cpp` already warns that an RConnection which was never
/// *started* is not the same animal as one that was: an attached connection may not be something
/// the stack can open sockets on.
///
/// [`Strategy::Default`] is the discriminator, and it is the one that keeps this probe autonomous:
/// it takes the phone's configured access point with no dialog to answer, so a run needs nobody
/// watching. Prompt is deliberately absent for that reason.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Strategy {
    /// Join a connection that is already up.
    Attach,
    /// Bring up the configured default, silently.
    Default,
}

/// The strategy this build uses. One line to flip, which is the point.
pub const BEARER: Strategy = Strategy::Default;

/// The compressed body this holds per target, and the decoded size it will accept.
///
/// The largest page measured in F2 was 294 KB compressed; a megabyte of headroom covers it without
/// making the biggest target in the list decide the heap. `MAX_DECODED` is the bound on the
/// *inflated* size, which DEFLATE's unbounded ratio makes a required argument rather than a nicety.
const BODY_CAP: usize = 1024 * 1024;
const MAX_DECODED: usize = 4 * 1024 * 1024;

/// The opcode the worker drill uses. Above [`symbian::work::OP_APP_BASE`], as an app-defined job
/// must be — the low numbers decode as crypto payloads and would answer nonsense rather than refuse.
pub const OP_ECHO_SUM: i32 = symbian::work::OP_APP_BASE;

/// A job that does nothing at all: writes a constant and returns.
///
/// The bisect. The first device run submitted the real job successfully and then never saw a
/// completion — so the two candidate causes were the *job* (it allocates on the worker's heap and
/// walks 64 KB) and the *path* (thread creation, `rust_work` dispatch, the completion event reaching
/// a headless pump). This one has no allocation, no loop and a one-byte payload, so if it completes
/// the path is fine and the job is at fault; if it does not, the job never was.
pub const OP_NOP: i32 = symbian::work::OP_APP_BASE + 1;

/// How big the opaque payload is. Chosen to be past the old fixed buffers by a wide margin: the
/// crypto `Job` held 775 bytes in and 256 out, and the point of F4 is that a job's size is the
/// caller's business.
pub const WORK_PAYLOAD: usize = 64 * 1024;

/// The heap ceiling the drill asks the worker for.
///
/// Past the old fixed 256 KB, because that ceiling is the thing F4 removed. A layout job will want
/// more still; this proves the parameter reaches the thread.
pub const WORK_HEAP: usize = 4 * 1024 * 1024;

/// The job itself: sum every input byte into a little-endian u64, and report the length.
///
/// Deliberately trivial and deliberately *not* a memcpy. It has to read every byte of a payload
/// larger than any buffer this facility used to carry, and produce something a caller can check
/// against an answer computed independently on the GUI thread — so a truncated payload, a wrong
/// pointer, or a byte lost at a buffer boundary all show up as a mismatch rather than as a job that
/// appeared to work.
///
/// It also allocates, once, to prove the worker's heap is real and usable at the requested ceiling.
/// What it allocates does not escape, which is the contract the facility rests on.
pub fn worker_dispatch(opcode: i32, input: &[u8], out: &mut [u8]) -> i32 {
    if opcode == OP_NOP {
        // Nothing but a store. No allocation, no iteration, no borrow of `input`.
        if out.len() < 16 {
            return -2;
        }
        out[..8].copy_from_slice(&0xA5A5_A5A5_A5A5_A5A5u64.to_le_bytes());
        out[8..16].copy_from_slice(&(input.len() as u64).to_le_bytes());
        return 0;
    }
    if opcode != OP_ECHO_SUM {
        return -1;
    }
    if out.len() < 16 {
        return -2;
    }
    // A scratch allocation on the worker's own heap, freed here. If the ceiling had not been
    // raised this is where a large job would fail.
    let scratch: Vec<u8> = Vec::with_capacity(64 * 1024);
    let _ = scratch.capacity();

    let mut sum = 0u64;
    for &b in input {
        sum = sum.wrapping_add(b as u64);
    }
    out[..8].copy_from_slice(&sum.to_le_bytes());
    out[8..16].copy_from_slice(&(input.len() as u64).to_le_bytes());
    0
}

/// What the worker drill produced.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkRow {
    /// Whether the do-nothing job completed. The bisect: this failing means the worker path is
    /// broken, not the job.
    pub nop_ok: bool,
    /// The error the do-nothing job reported, if any.
    pub nop_err: i32,
    /// Ticks the do-nothing job took.
    pub nop_ticks: u32,
    /// The tick at which the platform stopped reporting a live worker, or 0 if it never did.
    ///
    /// This splits the two halves of "no completion arrived". `shim_work_busy` answers the active
    /// object's own `IsActive()`, which `SetActive` sets and the scheduler clears when it dispatches
    /// `RunL`. So: it going false means RunL *did* run and the event was lost between the ring and
    /// the pump; it staying true means RunL was never dispatched at all. The breadcrumb file already
    /// says the worker thread posted its completion, so one of those two is happening and they need
    /// different fixes.
    pub busy_cleared_at: u32,
    /// Whether the job was accepted for submission.
    pub submitted: bool,
    /// Whether a completion arrived.
    pub completed: bool,
    /// The sum the worker computed against the one computed here. Equal is the finding.
    pub sum_matches: bool,
    pub len_matches: bool,
    /// A platform error, if the job failed.
    pub err: i32,
    /// Pump ticks the job took — which is also the proof it did not run inline.
    pub ticks: u32,
}

/// Where a cancel drill pulls the plug.
///
/// Cancelling is Back pressed during a load, so in a browser it is not optional and it is not rare.
/// The question worth answering is not whether the call returns — it is whether the session is still
/// usable afterwards, because a stack left holding a half-finished transaction would turn one
/// impatient user action into a browser that stops loading anything. So every drill cancels and then
/// **fetches something else and requires it to work**.
///
/// Three points, because they are three different states inside the stack: nothing sent yet, headers
/// received and body streaming, and mid-body with data already delivered.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Drill {
    /// Immediately after submitting, before any event.
    BeforeAnything,
    /// On the response headers.
    OnHeaders,
    /// On the nth body part.
    OnBodyPart(u32),
}

impl Drill {
    fn label(self) -> &'static str {
        match self {
            Drill::BeforeAnything => "before any event",
            Drill::OnHeaders => "on response headers",
            Drill::OnBodyPart(_) => "mid-body",
        }
    }
}

pub const DRILLS: &[Drill] =
    &[Drill::BeforeAnything, Drill::OnHeaders, Drill::OnBodyPart(3)];

/// The drill loads this: big enough that there is reliably a body still arriving to interrupt.
/// Measured at 293 KB in 94 parts, so a cancel on part 3 is a cancel with 91 parts left.
const DRILL_TARGET: &str = "https://www.cloudflare.com/";

/// And then fetches this, which must succeed. Small, so a failure is about the session and not
/// about the page.
const DRILL_RECOVERY: &str = "http://example.com/";

/// What one drill produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrillRow {
    pub at: &'static str,
    /// Whether the cancel call itself was accepted.
    pub cancelled: bool,
    /// Events that arrived for the cancelled transaction after the cancel.
    ///
    /// Not required to be zero — the stack may already have queued one — but they must not be
    /// mistaken for the next fetch's, which is what [`Fetch`] guards by refusing to report twice.
    pub strays: u32,
    /// The status the follow-up fetch got. This is the finding.
    pub recovery_status: u16,
    /// A platform error on the follow-up fetch, if any.
    pub recovery_err: i32,
}

impl DrillRow {
    /// Whether the session survived being interrupted here.
    pub fn recovered(&self) -> bool {
        self.recovery_status >= 200 && self.recovery_status < 400 && self.recovery_err == 0
    }
}

/// Which half of the worker drill is running.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum WorkStage {
    /// The do-nothing job.
    Nop,
    /// The real one.
    Real,
}

/// Which half of a drill is in flight.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum DrillStage {
    /// The big fetch, not yet cancelled.
    Loading,
    /// Cancelled; the follow-up fetch is in flight.
    Recovering,
}

/// How long the finished screen stays up before the probe closes itself.
///
/// It has to close, and that is not tidiness. A running process holds its own binary open, so a
/// probe that finishes and sits there cannot be replaced: pushing the next build over it fails with
/// `InUse`, and the only ways out are a keypress on the handset or a reboot. That turned a
/// self-driving build/push/run/read loop back into one that needs somebody in the room — which is
/// the whole thing this probe was supposed to stop needing.
///
/// The linger exists so a person watching still gets to read the last screen. Fifteen seconds is
/// long enough to see ten rows and short enough not to be a wait.
const LINGER_TICKS: u32 = 75;

/// A tick budget per target. A fetch the stack never finishes must not stall the list — one of the
/// things this probe is measuring is what the platform's own timeouts are, and "it hung" is a
/// finding that needs a number next to it.
const TICKS_PER_TARGET: u32 = 150; /* 30 s */

/// One URL and the reason it is in the list. The reason is not decoration: a probe whose targets
/// have no stated purpose grows a list nobody can prune.
pub struct Target {
    pub url: &'static str,
    pub why: &'static str,
}

/// The R4/R7 list. Kept short enough to run in one sitting and mixed on purpose.
pub const TARGETS: &[Target] = &[
    Target { url: "http://example.com/", why: "cleartext baseline: separates HTTP from TLS" },
    Target { url: "https://example.com/", why: "TLS baseline, tiny page, modern cert" },
    Target { url: "https://www.google.com/", why: "the tlsprobe target, so the two compare" },
    Target { url: "http://google.com/", why: "301 to https — is it followed for us?" },
    Target { url: "https://letsencrypt.org/", why: "ISRG root postdates the handset. R7" },
    Target { url: "https://www.cloudflare.com/", why: "modern-only TLS profile" },
    Target { url: "https://github.com/", why: "HSTS, and a real HTML page" },
    Target { url: "https://en.m.wikipedia.org/wiki/Symbian", why: "a real page, gzip, sizeable" },
    Target { url: "https://news.ycombinator.com/", why: "small real HTML, no JS needed to read" },
    Target { url: "http://neverssl.com/", why: "cleartext that stays cleartext" },

    // --- Does this handset do ECDSA? Open since August, and never actually asked. ---
    //
    // `docs/plan-browser.md` records pizzaria.foundation answering KErrNotSupported on
    // ECDHE-ECDSA-CHACHA20-POLY1305 and concludes the cipher list refuses it. That conflates two
    // variables: the signature algorithm and the bulk cipher. ChaCha20 is from 2013 and AES-GCM is
    // from 2008, and mbedTLS 3.4.1 — which the ssl.dll patch is built on — implements both.
    //
    // These three separate them. All are ECDSA P-256 with AES-GCM available; what differs is
    // whose root signed them.
    Target {
        url: "https://opencellid.org/",
        why: "ECDSA + AES-GCM on a Google Trust Services root, cross-signed by GlobalSign R1 (1998). If ECDSA works at all, this reaches.",
    },
    Target {
        url: "https://api.beacondb.net/",
        why: "ECDSA + AES-GCM on Let's Encrypt, whose DST Root CA X3 cross-signature expired in 2021. Fails on the root even if ECDSA is fine.",
    },
    Target {
        url: "https://www.cloudflare.com/cdn-cgi/trace",
        why: "ECDSA by default on an old DigiCert root. A second opinion on the same question.",
    },
];

/// What one target produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub url: &'static str,
    /// The HTTP status, or 0 when it never got one.
    pub status: u16,
    /// The platform error, 0 for success. Raw: see the module note on certificates.
    pub err: i32,
    pub bytes: usize,
    pub parts: u32,
    pub flags: Flags,
    /// The body's size after decoding, and what it cost to get there.
    ///
    /// This is the F3 evidence the host tests cannot produce: the fixtures are runs of one byte
    /// built by hand, and a real page is a real DEFLATE stream from a real server. A decoded size
    /// that is plausibly larger than the compressed one, on ten different sites, is the inflate path
    /// working on input nobody chose.
    pub decoded: usize,
    /// Set when decoding failed, with the reason's code.
    pub decode_err: i32,
    /// Bytes decoded again after a cache round trip, or 0 when nothing was cached.
    ///
    /// The check that matters is this equalling [`Row::decoded`]: same page, stored compressed,
    /// read back, inflated a second time to the same size. That exercises the whole F3 spine end to
    /// end — the flags surviving the format, the body arriving intact, the decoder agreeing with
    /// itself — on input from a real server.
    pub cached_decoded: usize,
    /// Set when storing or re-reading failed, with the reason's code.
    pub cache_err: i32,
    /// The `ETag` the response carried, if any. Empty means this page cannot be revalidated.
    pub etag: String,
    /// The `Last-Modified` the response carried, if any.
    pub last_modified: String,
    /// What a conditional refetch answered: 304 is the win, 200 means the copy was already stale,
    /// 0 means not attempted (no validator to send).
    pub revalidated: u16,
    /// A platform error on the conditional refetch.
    pub revalidate_err: i32,
    /// Where the bytes came from, when that differs from the URL asked for. Empty otherwise.
    ///
    /// F2 showed `http://google.com/` answering 200 with 28 KB, which means the stack followed the
    /// 301 without telling anyone. This is that redirect made visible — and it is what every
    /// relative link on the page has to resolve against.
    pub redirected_to: String,
    /// How many pump ticks the fetch took. The stopwatch and the liveness proof, same number.
    pub ticks: u32,
    /// True when the tick budget ran out rather than the transaction ending.
    pub timed_out: bool,
}

impl Row {
    /// Whether this counts as the stack working, which is narrower than "no error": a 500 proves
    /// the round trip as well as a 200 does, and a 0 with no error proves nothing at all.
    pub fn reached_server(&self) -> bool {
        self.status > 0
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Waiting for the first tick before touching the network, like every probe here — bring-up
    /// happens before the first frame otherwise, and a failure then has no screen to appear on.
    Waking,
    /// Asking each cached page whether it is still current, after the target list.
    Revalidating,
    /// The cancel drills, after the revalidation pass.
    Drilling,
    /// The worker-thread drill: an opaque job off the GUI thread. F4.
    Working,
    /// Bearer coming up.
    Connecting,
    /// A fetch in flight, or about to start.
    Fetching,
    /// Every target done, report written.
    Finished,
    /// Nothing more will happen; the reason is on screen.
    Failed,
}

pub struct HttpProbe<N: Net = ShimNet, H: Http = ShimHttp> {
    net: N,
    http: H,
    phase: Phase,
    bearer: Option<Bearer>,
    /// Whether the HTTP session has been opened over the bearer.
    session: bool,
    at: usize,
    fetch: Option<Fetch>,
    rows: Vec<Row>,
    /// Ticks since the process started. Never reset — it is the proof, and a proof that resets is
    /// a proof a frozen UI could fake.
    ticks: u32,
    /// The tick the current fetch started on.
    fetch_started: u32,
    /// Bytes drained from the current fetch, so the report says what a caller could actually read
    /// rather than what the stack claims to have delivered.
    drained: usize,
    /// The current target's body, accumulated compressed and decoded when the transaction ends.
    body: Body,
    /// The cache's encode buffer, owned here so storing ten responses allocates once rather than
    /// ten times — which on this heap is the difference between fragmentation and not.
    scratch: Vec<u8>,
    /// Which drill is running, its half, and what they have produced.
    /// Which row the revalidation pass is on.
    reval: usize,
    drill: usize,
    drill_stage: DrillStage,
    drill_parts: u32,
    drill_strays: u32,
    drill_cancelled: bool,
    drill_rows: Vec<DrillRow>,
    /// The worker drill: the job, what we expect back, and what came back.
    job: symbian::work::Job,
    work_expect_sum: u64,
    work_started: u32,
    work_stage: WorkStage,
    /// How many ticks the worker drill has polled `platform_busy` for.
    work_polls: u32,
    work_row: WorkRow,
    note: String,
    report_path: String,
    /// Written once. The list can finish from either of two places — the last completion, or the
    /// last timeout — and a report written from both would truncate itself.
    reported: bool,
    /// The tick the report was written on, which starts the linger before closing.
    finished_at: u32,
    exit: bool,
}

impl HttpProbe<ShimNet, ShimHttp> {
    pub fn new() -> Self {
        // Arming the timer is what makes the probe run at all, and it happens here rather than in
        // `with` because `with` is what the host tests use: a test drives ticks itself, and a
        // constructor that reached for the platform clock would make every one of them need a
        // phone. The first tick is also the bring-up trigger, so this is not just the counter.
        let _ = symbian::timer_every(TICK_MS);
        Self::with(ShimNet, ShimHttp)
    }
}

impl Default for HttpProbe<ShimNet, ShimHttp> {
    fn default() -> Self {
        Self::new()
    }
}

impl<N: Net, H: Http> HttpProbe<N, H> {
    pub fn with(net: N, http: H) -> Self {
        HttpProbe {
            net,
            http,
            phase: Phase::Waking,
            bearer: None,
            session: false,
            at: 0,
            fetch: None,
            rows: Vec::new(),
            ticks: 0,
            fetch_started: 0,
            drained: 0,
            body: Body::with_cap(BODY_CAP),
            scratch: Vec::new(),
            reval: 0,
            drill: 0,
            drill_stage: DrillStage::Loading,
            drill_parts: 0,
            drill_strays: 0,
            drill_cancelled: false,
            drill_rows: Vec::new(),
            job: symbian::work::Job::with_capacity(WORK_PAYLOAD, 16),
            work_expect_sum: 0,
            work_started: 0,
            work_stage: WorkStage::Nop,
            work_polls: 0,
            work_row: WorkRow::default(),
            note: String::from("waiting for the first tick"),
            report_path: String::new(),
            reported: false,
            finished_at: 0,
            exit: false,
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn drill_rows(&self) -> &[DrillRow] {
        &self.drill_rows
    }

    pub fn work_row(&self) -> WorkRow {
        self.work_row
    }

    pub fn ticks(&self) -> u32 {
        self.ticks
    }

    /// Bring up the bearer. Attach first: if anything on the handset is already online this joins
    /// it with no dialog, which is both faster and the only path that works with the screen
    /// already drawn.
    fn connect(&mut self) {
        self.phase = Phase::Connecting;
        let started = match BEARER {
            Strategy::Attach => {
                symbian::log!("[httpprobe] bearer: attach");
                Bearer::attach(&mut self.net)
            }
            Strategy::Default => {
                symbian::log!("[httpprobe] bearer: default");
                Bearer::start_default(&mut self.net)
            }
        };
        match started {
            Ok(b) => {
                symbian::log!("[httpprobe] bearer requested, handle {}", b.handle());
                self.bearer = Some(b);
                self.note = String::from("bringing up a bearer");
            }
            Err(e) => {
                symbian::log!("[httpprobe] bearer request FAILED {}", e.code());
                self.fail("no bearer", e.code());
            }
        }
    }

    fn fail(&mut self, what: &str, code: i32) {
        self.phase = Phase::Failed;
        let mut s = String::from(what);
        s.push_str(" (");
        push_i64(&mut s, code as i64);
        s.push(')');
        self.note = s;
    }

    /// Open the session over the bearer that just came up, then start the first target.
    fn begin(&mut self) {
        let handle = match self.bearer.as_ref() {
            Some(b) => b.handle(),
            None => return,
        };
        if let Err(e) = self.http.open(handle) {
            symbian::log!("[httpprobe] session open FAILED {}", e.code());
            self.fail("session", e.code());
            return;
        }
        symbian::log!("[httpprobe] session open OK on bearer handle {}", handle);
        self.session = true;
        self.phase = Phase::Fetching;
        self.next();
    }

    /// Start the target at `self.at`, or finish.
    fn next(&mut self) {
        self.drained = 0;
        self.body = Body::with_cap(BODY_CAP);
        if self.at >= TARGETS.len() {
            self.fetch = None;
            self.reval = 0;
            self.start_revalidation();
            return;
        }
        let url = TARGETS[self.at].url;
        self.fetch_started = self.ticks;
        // gzip asked for on every request, including cleartext ones: the question is what the
        // stack does with the header, and that does not depend on the scheme.
        symbian::log!("[httpprobe] GET {}", url);
        match Fetch::start(&mut self.http, url, true) {
            Ok(f) => {
                self.fetch = Some(f);
                self.note = String::from(url);
            }
            Err(e) => {
                // A URL the shim would not take is a result, not a crash: record it and move on,
                // because stopping the list here would lose the nine targets after it.
                self.record(Row {
                    url,
                    status: 0,
                    err: e.code(),
                    bytes: 0,
                    parts: 0,
                    flags: Flags(0),
                    decoded: 0,
                    decode_err: 0,
                    cached_decoded: 0,
                    cache_err: 0,
                    etag: String::new(),
                    last_modified: String::new(),
                    revalidated: 0,
                    revalidate_err: 0,
                    redirected_to: String::new(),
                    ticks: 0,
                    timed_out: false,
                });
                self.at += 1;
                self.next();
            }
        }
    }

    fn record(&mut self, row: Row) {
        self.rows.push(row);
    }

    /// Drain whatever body bytes are held. Called on every body event and once at the end.
    ///
    /// The bytes are counted and dropped: this probe is measuring the transport, and holding a
    /// page would make the biggest target in the list the thing that decides the heap size.
    fn drain(&mut self) {
        let mut buf = [0u8; 1024];
        loop {
            let n = match self.http.read(&mut buf) {
                Ok(n) => n,
                Err(_) => return,
            };
            if n == 0 {
                return;
            }
            self.drained += n;
            self.body.push(&buf[..n]);
        }
    }

    fn finish_target(&mut self, resp: Option<Response>, err: i32, timed_out: bool) {
        self.drain();
        let url = TARGETS[self.at].url;
        let elapsed = self.ticks.saturating_sub(self.fetch_started);

        // Decode into a counting sink. The probe wants the size and whether it worked, not the
        // page — and a sink that only counts is the cheapest way to exercise the whole inflate path
        // without the decoded megabyte ever existing, which is the property being tested.
        let redirected_to = match self.fetch.as_ref() {
            Some(f) if f.was_redirected() => String::from(f.effective_url()),
            _ => String::new(),
        };
        let (etag, last_modified) = match self.fetch.as_ref() {
            Some(f) => (String::from(&f.validators().etag), String::from(&f.validators().last_modified)),
            None => (String::new(), String::new()),
        };
        // A response that did not finish has no decodable body and must not be treated as one.
        //
        // Both halves of that were wrong. Decoding a partial body with `Flags(0)` passed the raw
        // bytes through and reported the count as `decoded`, so a timed-out 294 KB fetch read as a
        // 294 KB page. And caching it would store a truncated body that a later hit would serve as
        // a complete page — a cache actively worse than none. Measured: Cloudflare stalled at 83 KB
        // of 294 KB and hit the tick budget, and this path reported success at both.
        let complete = resp.is_some() && !timed_out;
        let flags = resp.map(|r| r.flags).unwrap_or(Flags(0));
        let mut counter = Counter::default();
        let (decoded, decode_err) = if !complete || self.body.is_empty() {
            (0, 0)
        } else {
            match self.body.decode_to(flags, MAX_DECODED, &mut counter) {
                Ok(n) => (n, 0),
                Err(e) => (counter.n, e.code()),
            }
        };

        // Cache the response as it came off the wire, then read it back and decode it again. The
        // stored URL is the effective one, because that is what a restored page's links resolve
        // against — storing the requested URL would make a cache hit answer for the wrong address.
        let cache_url = if redirected_to.is_empty() { String::from(url) } else { redirected_to.clone() };
        let (cached_decoded, cache_err) = if !complete || self.body.is_empty() || decode_err != 0 {
            (0, 0)
        } else {
            self.cache_round_trip(&cache_url, flags, resp.map(|r| r.status).unwrap_or(0))
        };

        let row = match resp {
            Some(r) => Row {
                url,
                status: r.status,
                err: 0,
                bytes: r.total,
                parts: r.parts,
                flags: r.flags,
                decoded,
                decode_err,
                cached_decoded,
                cache_err,
                etag: etag.clone(),
                last_modified: last_modified.clone(),
                revalidated: 0,
                revalidate_err: 0,
                redirected_to: redirected_to.clone(),
                ticks: elapsed,
                timed_out: false,
            },
            None => Row {
                url,
                status: 0,
                err,
                bytes: self.drained,
                parts: 0,
                flags: Flags(0),
                decoded,
                decode_err,
                cached_decoded,
                cache_err,
                etag,
                last_modified,
                revalidated: 0,
                revalidate_err: 0,
                redirected_to,
                ticks: elapsed,
                timed_out,
            },
        };
        symbian::log!(
            "[httpprobe] done {} status={} err={} bytes={} parts={} flags={} decoded={} derr={} ticks={}",
            row.url,
            row.status,
            row.err,
            row.bytes,
            row.parts,
            row.flags.0,
            row.decoded,
            row.decode_err,
            row.ticks
        );
        self.record(row);
        self.fetch = None;
        self.at += 1;
        self.next();
    }

    /// Ask the next cached page whether it is still current, or move on to the drills.
    ///
    /// This is the pass that makes the cache more than a snapshot: a conditional GET still costs a
    /// round trip, so the answer is current, but a server that agrees sends 304 with no body. On a
    /// link metered by the kilobyte that is the whole difference between a cache worth having and
    /// one that only helps offline.
    ///
    /// Only rows with a validator are asked. A page that sent neither `ETag` nor `Last-Modified`
    /// cannot be revalidated at all, and how many of those there are is itself the finding.
    fn start_revalidation(&mut self) {
        while self.reval < self.rows.len() {
            let i = self.reval;
            let has = !self.rows[i].etag.is_empty() || !self.rows[i].last_modified.is_empty();
            if !has || self.rows[i].status == 0 {
                self.reval += 1;
                continue;
            }
            self.phase = Phase::Revalidating;
            self.fetch_started = self.ticks;
            self.body = Body::with_cap(BODY_CAP);
            let url = self.reval_url(i);
            let etag = String::from(&self.rows[i].etag);
            let lm = String::from(&self.rows[i].last_modified);
            symbian::log!("[httpprobe] revalidating {}", url.as_str());
            match Fetch::start_conditional(&mut self.http, &url, true, &etag, &lm) {
                Ok(f) => {
                    self.fetch = Some(f);
                    self.note = String::from("revalidating");
                    return;
                }
                Err(e) => {
                    self.rows[i].revalidate_err = e.code();
                    self.reval += 1;
                }
            }
        }
        self.fetch = None;
        self.drill = 0;
        self.start_drill();
    }

    /// The URL to revalidate row `i` against: where the bytes came from, not what was asked for.
    fn reval_url(&self, i: usize) -> String {
        if self.rows[i].redirected_to.is_empty() {
            String::from(self.rows[i].url)
        } else {
            String::from(&self.rows[i].redirected_to)
        }
    }

    /// Events during the revalidation pass.
    fn revalidation_event(&mut self, ev: &RawEvent) {
        let progress = match self.fetch.as_mut() {
            Some(f) => f.on_event_with(&mut self.http, ev),
            None => return,
        };
        let i = self.reval;
        match progress {
            Progress::NotModified => {
                self.rows[i].revalidated = 304;
                symbian::log!("[httpprobe] {} -> 304 not modified", self.reval_url(i).as_str());
                self.reval += 1;
                self.start_revalidation();
            }
            Progress::Done(r) => {
                // 200 to a conditional request is a legitimate answer — the page changed between
                // the two fetches, or the server does not honour the validator. Recorded either
                // way; guessing which would be inventing a finding.
                self.rows[i].revalidated = r.status;
                self.reval += 1;
                self.start_revalidation();
            }
            Progress::Failed(e) => {
                self.rows[i].revalidate_err = e.code();
                self.reval += 1;
                self.start_revalidation();
            }
            // The body of a 200 is drained so the next request starts from a clean buffer.
            Progress::Body(_) => self.drain(),
            _ => {}
        }
    }

    /// Begin the next drill, or finish.
    fn start_drill(&mut self) {
        if self.drill >= DRILLS.len() {
            self.start_work_drill();
            return;
        }
        self.phase = Phase::Drilling;
        self.drill_stage = DrillStage::Loading;
        self.drill_parts = 0;
        self.drill_strays = 0;
        self.drill_cancelled = false;
        self.body = Body::with_cap(BODY_CAP);
        self.fetch_started = self.ticks;

        let d = DRILLS[self.drill];
        symbian::log!("[httpprobe] drill {}: cancel {}", self.drill, d.label());
        match Fetch::start(&mut self.http, DRILL_TARGET, true) {
            Ok(f) => {
                self.fetch = Some(f);
                self.note = String::from(d.label());
                // BeforeAnything cancels without waiting for a single event, which is the state a
                // real Back press is most likely to catch: the user has already changed their mind
                // by the time the first byte comes back.
                if d == Drill::BeforeAnything {
                    self.do_cancel();
                }
            }
            Err(e) => {
                symbian::log!("[httpprobe] drill {} could not start: {}", self.drill, e.code());
                self.finish_drill(0, e.code());
            }
        }
    }

    /// Send an opaque payload to the worker thread and check what comes back. F4's exit criterion.
    ///
    /// The payload is larger than any buffer this facility used to carry and the heap ceiling asked
    /// for is larger than the one it used to have, so this fails on the old code in two independent
    /// ways — which is what makes it a test of the change rather than of the shim.
    fn start_work_drill(&mut self) {
        self.phase = Phase::Working;
        self.work_stage = WorkStage::Nop;
        self.note = String::from("worker: nop");
        self.work_started = self.ticks;

        // The do-nothing job first, with the DEFAULT heap ceiling and a one-byte payload: this half
        // asks only whether a job completes at all under a headless pump.
        self.job.set_worker_heap(symbian::work::DEFAULT_WORKER_HEAP);
        match self.job.submit_bytes(OP_NOP, &[0u8], 16) {
            Ok(()) => symbian::log!("[httpprobe] worker nop submitted"),
            Err(e) => {
                self.work_row.nop_err = e.code();
                symbian::log!("[httpprobe] worker nop refused: {}", e.code());
                self.start_real_work();
            }
        }
    }

    /// The real job: a payload and a heap ceiling both past what the old fixed buffers allowed.
    fn start_real_work(&mut self) {
        self.work_stage = WorkStage::Real;
        self.note = String::from("worker: real");
        self.work_started = self.ticks;

        // A payload with no repeating structure, so a lost or duplicated chunk changes the sum.
        let mut payload = Vec::with_capacity(WORK_PAYLOAD);
        let mut x: u32 = 0x1234_5678;
        for _ in 0..WORK_PAYLOAD {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            payload.push((x >> 24) as u8);
        }
        self.work_expect_sum = payload.iter().map(|&b| b as u64).sum();

        self.job.set_worker_heap(WORK_HEAP);
        match self.job.submit_bytes(OP_ECHO_SUM, &payload, 16) {
            Ok(()) => {
                self.work_row.submitted = true;
                symbian::log!(
                    "[httpprobe] worker real submitted: {} bytes, heap {}",
                    WORK_PAYLOAD,
                    WORK_HEAP
                );
            }
            Err(e) => {
                self.work_row.err = e.code();
                symbian::log!("[httpprobe] worker real refused: {}", e.code());
                self.finish_work_drill();
            }
        }
    }

    fn finish_work_drill(&mut self) {
        self.work_row.ticks = self.ticks.saturating_sub(self.work_started);
        symbian::log!(
            "[httpprobe] worker done: nop={} nop_err={} submitted={} completed={} sum={} len={} err={}",
            self.work_row.nop_ok,
            self.work_row.nop_err,
            self.work_row.submitted,
            self.work_row.completed,
            self.work_row.sum_matches,
            self.work_row.len_matches,
            self.work_row.err
        );
        self.phase = Phase::Finished;
        self.note = String::from("done");
    }

    /// The worker's completion, for whichever half is running.
    fn work_event(&mut self, ev: &RawEvent) {
        let Some(result) = self.job.on_event(ev) else { return };
        match self.work_stage {
            WorkStage::Nop => {
                self.work_row.nop_ticks = self.ticks.saturating_sub(self.work_started);
                match result {
                    Ok(bytes) if bytes.len() >= 16 && bytes[0] == 0xA5 => self.work_row.nop_ok = true,
                    Ok(_) => self.work_row.nop_err = -3,
                    Err(e) => self.work_row.nop_err = e.code(),
                }
                symbian::log!("[httpprobe] worker nop -> ok={}", self.work_row.nop_ok);
                self.start_real_work();
            }
            WorkStage::Real => {
                match result {
                    Ok(bytes) if bytes.len() >= 16 => {
                        self.work_row.completed = true;
                        let mut s = [0u8; 8];
                        let mut l = [0u8; 8];
                        s.copy_from_slice(&bytes[..8]);
                        l.copy_from_slice(&bytes[8..16]);
                        self.work_row.sum_matches = u64::from_le_bytes(s) == self.work_expect_sum;
                        self.work_row.len_matches = u64::from_le_bytes(l) == WORK_PAYLOAD as u64;
                    }
                    Ok(_) => {
                        self.work_row.completed = true;
                        self.work_row.err = -3;
                    }
                    Err(e) => self.work_row.err = e.code(),
                }
                self.finish_work_drill();
            }
        }
    }

    /// Pull the plug, then ask the session to do something else.
    fn do_cancel(&mut self) {
        if let Some(f) = self.fetch.as_mut() {
            f.cancel(&mut self.http);
        }
        self.drill_cancelled = true;
        self.drill_stage = DrillStage::Recovering;
        self.fetch_started = self.ticks;
        symbian::log!("[httpprobe] drill {} cancelled, recovering", self.drill);

        // The whole point: a cancelled transaction must leave a session that still works.
        match Fetch::start(&mut self.http, DRILL_RECOVERY, false) {
            Ok(f) => self.fetch = Some(f),
            Err(e) => {
                symbian::log!("[httpprobe] drill {} recovery refused: {}", self.drill, e.code());
                self.finish_drill(0, e.code());
            }
        }
    }

    fn finish_drill(&mut self, status: u16, err: i32) {
        let row = DrillRow {
            at: DRILLS[self.drill].label(),
            cancelled: self.drill_cancelled,
            strays: self.drill_strays,
            recovery_status: status,
            recovery_err: err,
        };
        symbian::log!(
            "[httpprobe] drill {} at={} cancelled={} strays={} recovery={} err={}",
            self.drill,
            row.at,
            row.cancelled,
            row.strays,
            row.recovery_status,
            row.recovery_err
        );
        self.drill_rows.push(row);
        self.fetch = None;
        self.drill += 1;
        self.start_drill();
    }

    /// Events during a drill. Separate from the target list so neither can confuse the other.
    fn drill_event(&mut self, ev: &RawEvent) {
        let progress = match self.fetch.as_mut() {
            Some(f) => f.on_event(ev),
            None => return,
        };

        match self.drill_stage {
            DrillStage::Loading => {
                let trigger = match DRILLS[self.drill] {
                    Drill::BeforeAnything => false, // already cancelled at start
                    Drill::OnHeaders => matches!(progress, Progress::Head(_)),
                    Drill::OnBodyPart(n) => {
                        if matches!(progress, Progress::Body(_)) {
                            self.drill_parts += 1;
                        }
                        self.drill_parts >= n
                    }
                };
                if trigger {
                    self.do_cancel();
                } else if let Progress::Done(_) | Progress::Failed(_) = progress {
                    // It finished before we could interrupt it — which is a legitimate outcome for a
                    // small page on a fast link, and must be reported as "not cancelled" rather than
                    // counted as a pass.
                    symbian::log!("[httpprobe] drill {} completed before the cancel point", self.drill);
                    self.finish_drill(0, 0);
                }
            }
            DrillStage::Recovering => match progress {
                Progress::Done(r) => {
                    let s = r.status;
                    self.finish_drill(s, 0);
                }
                Progress::Failed(e) => {
                    let c = e.code();
                    self.finish_drill(0, c);
                }
                _ => {}
            },
        }
    }

    /// Store the body, read it back, decode it again, and report the size.
    ///
    /// Returns `(decoded, err)`. A failure anywhere is reported rather than ignored: the point of
    /// the round trip is that it is allowed to disagree, and a probe that swallowed the difference
    /// would be asserting nothing.
    fn cache_round_trip(&mut self, url: &str, flags: Flags, status: u16) -> (usize, i32) {
        let mut fs = symbian::fs::ShimFs;
        let entry = cache::Ref {
            url,
            status,
            flags,
            // No validators yet: the shim does not read ETag or Last-Modified. Left empty rather
            // than invented, so `has_validator` tells the truth about what can be revalidated.
            etag: "",
            last_modified: "",
            // Borrowed, not copied. The first version of this called `.to_vec()` here and the
            // handset killed the run on the page after the largest one — see the module note in
            // `http::cache`.
            body: self.body.raw(),
        };
        if let Err(e) = cache::put(&mut fs, &entry, &mut self.scratch) {
            return (0, e.code());
        }

        let mut buf = Vec::new();
        if !cache::load(&mut fs, url, &mut buf) {
            return (0, -1);
        }
        let back = match cache::decode_ref(&buf) {
            Some(b) => b,
            None => return (0, -2),
        };
        // Inflated straight out of the file buffer: no second Body, no third copy of the page.
        let mut counter = Counter::default();
        match symbian_crypto::inflate::inflate_any_to(back.body, MAX_DECODED, &mut counter) {
            Ok(n) if back.flags.needs_inflate() => (n, 0),
            Ok(_) => (back.body.len(), 0),
            Err(_) if !back.flags.needs_inflate() => (back.body.len(), 0),
            Err(e) => (counter.n, inflate_code(e)),
        }
    }

    /// One pump tick. Drives the timeout, the linger, and is the number on screen.
    fn on_tick(&mut self) {
        self.ticks = self.ticks.saturating_add(1);

        if self.reported && self.ticks.saturating_sub(self.finished_at) >= LINGER_TICKS {
            self.exit = true;
            return;
        }

        if self.phase == Phase::Waking {
            self.connect();
            return;
        }

        if self.phase == Phase::Fetching
            && self.fetch.is_some()
            && self.ticks.saturating_sub(self.fetch_started) >= TICKS_PER_TARGET
        {
            if let Some(f) = self.fetch.as_mut() {
                f.cancel(&mut self.http);
            }
            self.finish_target(None, 0, true);
        }

        if self.phase == Phase::Revalidating
            && self.ticks.saturating_sub(self.fetch_started) >= TICKS_PER_TARGET
        {
            if let Some(f) = self.fetch.as_mut() {
                f.cancel(&mut self.http);
            }
            let i = self.reval;
            self.rows[i].revalidate_err = -999;
            self.reval += 1;
            self.start_revalidation();
        }

        // While a job is out, watch the platform's own view of it. See WorkRow::busy_cleared_at.
        if self.phase == Phase::Working && self.work_row.busy_cleared_at == 0 {
            self.work_polls = self.work_polls.saturating_add(1);
            if !self.job.platform_busy() {
                self.work_row.busy_cleared_at = self.work_polls;
                symbian::log!(
                    "[httpprobe] platform_busy went false at poll {} — RunL ran, event lost",
                    self.work_polls
                );
            }
        }

        // A worker job that never completes would hang the probe, and "the GUI thread kept
        // ticking while it ran" is itself the thing being demonstrated — so the ticks are both the
        // proof and the timeout.
        if self.phase == Phase::Working
            && self.ticks.saturating_sub(self.work_started) >= TICKS_PER_TARGET
        {
            // Each half gets its own budget, so a nop that hangs still lets the real job be tried —
            // and the pair of answers is the bisect.
            match self.work_stage {
                WorkStage::Nop => {
                    self.work_row.nop_err = -999;
                    symbian::log!("[httpprobe] worker nop never completed");
                    // Without this the Job stays busy forever and the real job is refused with
                    // InUse — an error about the wrong thing, which is exactly how the first
                    // device run hid its own finding behind a second one.
                    if let Err(e) = self.job.abandon() {
                        self.work_row.err = e.code();
                        symbian::log!("[httpprobe] cannot abandon: worker still running");
                        self.finish_work_drill();
                        return;
                    }
                    self.start_real_work();
                }
                WorkStage::Real => {
                    self.work_row.err = -999;
                    self.finish_work_drill();
                }
            }
        }

        // A drill that hangs is the failure it exists to look for — a session wedged by the cancel —
        // so it must be recorded, not waited on forever.
        if self.phase == Phase::Drilling
            && self.ticks.saturating_sub(self.fetch_started) >= TICKS_PER_TARGET
        {
            symbian::log!("[httpprobe] drill {} timed out in {:?}", self.drill, self.drill_stage);
            if let Some(f) = self.fetch.as_mut() {
                f.cancel(&mut self.http);
            }
            self.finish_drill(0, -999);
        }
    }

    /// Write the report the moment the list is done, and only then.
    ///
    /// Called from both event paths because either can be the one that finishes the list. The
    /// filesystem comes from the shim rather than from a type parameter: the host tests reach the
    /// stubs, which decline, and a report nobody can write on a desktop is the correct outcome.
    fn report_if_finished(&mut self) {
        if self.phase != Phase::Finished || self.reported {
            return;
        }
        self.reported = true;
        self.finished_at = self.ticks;
        let mut fs = symbian::fs::ShimFs;
        self.write_report(&mut fs);
        symbian::log!("[httpprobe] report written, closing in {} ticks", LINGER_TICKS);
    }

    /// The report. Written once, when the list is done.
    pub fn write_report<F: symbian::fs::Fs>(&mut self, fs: &mut F) {
        let mut r = Report::new("httpprobe");
        r.head("HTTP through the platform stack");
        r.line("");
        r.line("Ticks are pump ticks of 200 ms — the stopwatch. They were also the liveness");
        r.line("proof while this probe had a screen; it is headless now, and that question was");
        r.line("answered in F2. See the crate docs for why the window had to go.");
        r.info(
            "bearer strategy",
            match BEARER {
                Strategy::Attach => "attach (join a live connection)",
                Strategy::Default => "default (configured access point, no dialog)",
            },
        );
        r.num("total ticks", self.ticks as i64);
        r.line("");

        let mut reached = 0u32;
        let mut gzip_free = 0u32;
        let mut gzip_raw = 0u32;
        let mut chunked = 0u32;

        for row in &self.rows {
            let mut line = String::from(row.url);
            line.push_str("  ");
            if row.reached_server() {
                reached += 1;
                line.push_str("HTTP ");
                push_i64(&mut line, row.status as i64);
            } else {
                line.push_str("ERR ");
                push_i64(&mut line, row.err as i64);
                if row.timed_out {
                    line.push_str(" (timeout)");
                }
            }
            line.push_str("  ");
            push_i64(&mut line, row.bytes as i64);
            line.push_str("B in ");
            push_i64(&mut line, row.parts as i64);
            line.push_str(" parts, ");
            push_i64(&mut line, row.ticks as i64);
            line.push_str(" ticks");

            if row.decode_err != 0 {
                line.push_str(" [DECODE FAILED ");
                push_i64(&mut line, row.decode_err as i64);
                line.push(']');
            } else if row.decoded > 0 {
                line.push_str(" -> ");
                push_i64(&mut line, row.decoded as i64);
                line.push_str("B decoded");
            }

            if row.cache_err != 0 {
                line.push_str(" [CACHE FAILED ");
                push_i64(&mut line, row.cache_err as i64);
                line.push(']');
            } else if row.cached_decoded == row.decoded && row.decoded > 0 {
                line.push_str(" [cached ok]");
            } else if row.cached_decoded > 0 {
                line.push_str(" [CACHE MISMATCH ");
                push_i64(&mut line, row.cached_decoded as i64);
                line.push(']');
            }
            if !row.redirected_to.is_empty() {
                line.push_str("\n      -> followed to ");
                line.push_str(&row.redirected_to);
            }
            if row.flags.chunked() {
                chunked += 1;
                line.push_str(" [chunked]");
            }
            if row.flags.needs_inflate() {
                gzip_raw += 1;
                line.push_str(" [gzip RAW - we inflate]");
            } else if row.flags.gzip() {
                gzip_free += 1;
                line.push_str(" [gzip decoded for us]");
            }
            if row.flags.truncated() {
                line.push_str(" [body over cap]");
            }
            r.line(&line);
        }

        r.line("");
        r.head("what this decides");
        r.check_note(
            "the stack reached a server",
            reached > 0,
            "if zero, F3 goes back to HTTP over a raw socket",
        );
        // Reported, not judged — and it used to be a verdict, which was wrong once F2 settled the
        // answer. The stack never inflates; that is a fact about this platform, so printing FAIL for
        // it on every run is noise that trains a reader to skip the verdict block.
        r.info(
            "who inflates",
            if gzip_raw > 0 && gzip_free == 0 {
                "we do, always — the stack never does"
            } else if gzip_free > 0 && gzip_raw > 0 {
                "BOTH SEEN — the stack is inconsistent, decide per response"
            } else {
                "no compressed body in this run"
            },
        );
        r.num("bodies inflated by the stack", gzip_free as i64);
        r.num("bodies we must inflate", gzip_raw as i64);

        let decoded_ok = self.rows.iter().filter(|r| r.decode_err == 0 && r.decoded > 0).count();
        let decode_failed = self.rows.iter().filter(|r| r.decode_err != 0).count();
        r.check_note(
            "every body decoded",
            decode_failed == 0 && decoded_ok > 0,
            "F3's inflate path, on real pages from real servers",
        );
        r.num("bodies decoded", decoded_ok as i64);
        r.num("bodies that failed to decode", decode_failed as i64);

        let cached_ok = self
            .rows
            .iter()
            .filter(|r| r.cache_err == 0 && r.decoded > 0 && r.cached_decoded == r.decoded)
            .count();
        let cache_bad = self
            .rows
            .iter()
            .filter(|r| r.cache_err != 0 || (r.decoded > 0 && r.cached_decoded != r.decoded))
            .count();
        r.check_note(
            "cached, re-read and decoded identically",
            cache_bad == 0 && cached_ok > 0,
            "downloaded, inflated and cached — F3's exit criterion",
        );
        r.num("responses cached and verified", cached_ok as i64);
        r.num("cache round trips that disagreed", cache_bad as i64);

        r.line("");
        r.head("revalidation — asking whether a cached page is still current");
        let with_validator = self.rows.iter().filter(|x| !x.etag.is_empty() || !x.last_modified.is_empty()).count();
        let not_modified = self.rows.iter().filter(|x| x.revalidated == 304).count();
        let changed = self.rows.iter().filter(|x| x.revalidated >= 200 && x.revalidated < 300).count();
        let reval_failed = self.rows.iter().filter(|x| x.revalidate_err != 0).count();
        for row in &self.rows {
            if row.etag.is_empty() && row.last_modified.is_empty() {
                continue;
            }
            let mut line = String::from(short_host(row.url));
            line.push_str(": ");
            if row.revalidate_err != 0 {
                line.push_str("FAILED ");
                push_i64(&mut line, row.revalidate_err as i64);
            } else if row.revalidated == 304 {
                line.push_str("304 not modified — body not resent");
            } else if row.revalidated > 0 {
                line.push_str("HTTP ");
                push_i64(&mut line, row.revalidated as i64);
                line.push_str(" — the copy was stale, or the validator was ignored");
            } else {
                line.push_str("not attempted");
            }
            if !row.etag.is_empty() {
                line.push_str(" [etag]");
            }
            if !row.last_modified.is_empty() {
                line.push_str(" [last-modified]");
            }
            r.line(&line);
        }
        r.check_note(
            "a cached page can be revalidated",
            not_modified > 0 && reval_failed == 0,
            "304 means the round trip happened and the body did not",
        );
        r.num("pages carrying a validator", with_validator as i64);
        r.num("answered 304", not_modified as i64);
        r.num("answered with a fresh body", changed as i64);
        r.num("revalidations that failed", reval_failed as i64);
        r.line("");
        r.line("A page with no validator cannot be revalidated at all. For those the stored copy");
        r.line("is only a snapshot: good for Back and for offline, never for ordinary navigation.");

        r.line("");
        r.head("worker thread — an opaque job off the GUI thread (F4)");
        {
            let w = self.work_row;
            let mut nop = String::from("nop (1 byte, default heap): ");
            if w.nop_ok {
                nop.push_str("completed in ");
                push_i64(&mut nop, w.nop_ticks as i64);
                nop.push_str(" ticks");
            } else if w.nop_err == -999 {
                nop.push_str("NEVER COMPLETED — the worker path is broken, not the job");
            } else {
                nop.push_str("FAILED ");
                push_i64(&mut nop, w.nop_err as i64);
            }
            r.line(&nop);

            // Printed only when something failed. A job that completes inside one pump tick never
            // gives the poll loop a chance to observe the flip, so `busy_cleared_at` stays zero on
            // the happy path — and reading that as "RunL was never dispatched" put an accusation
            // next to a pass. A diagnostic that fires on success is worse than none: it is the same
            // mistake as the chunked check that printed FAIL for a settled fact.
            let failed = !w.nop_ok || !w.completed || !w.sum_matches || !w.len_matches;
            if failed {
                let mut diag = String::from("platform view: ");
                if w.busy_cleared_at > 0 {
                    diag.push_str("IsActive cleared at poll ");
                    push_i64(&mut diag, w.busy_cleared_at as i64);
                    diag.push_str(" — RunL RAN and the event was lost after it");
                } else {
                    diag.push_str("IsActive still set — RunL may never have been dispatched");
                }
                r.line(&diag);
            }

            let mut line = String::new();
            push_i64(&mut line, WORK_PAYLOAD as i64);
            line.push_str(" bytes, heap ceiling ");
            push_i64(&mut line, (WORK_HEAP / 1024) as i64);
            line.push_str(" KB: ");
            if !w.submitted {
                line.push_str("REFUSED ");
                push_i64(&mut line, w.err as i64);
            } else if w.err == -999 {
                line.push_str("NEVER COMPLETED");
            } else if w.err != 0 {
                line.push_str("FAILED ");
                push_i64(&mut line, w.err as i64);
            } else if w.sum_matches && w.len_matches {
                line.push_str("round trip correct in ");
                push_i64(&mut line, w.ticks as i64);
                line.push_str(" ticks");
            } else {
                line.push_str("WRONG ANSWER (sum ok: ");
                line.push_str(if w.sum_matches { "yes" } else { "no" });
                line.push_str(", len ok: ");
                line.push_str(if w.len_matches { "yes" } else { "no" });
                line.push(')');
            }
            r.line(&line);
            r.check_note(
                "an opaque job runs on the worker and answers correctly",
                w.submitted && w.completed && w.sum_matches && w.len_matches,
                "payload and heap ceiling both past what the old fixed buffers allowed",
            );
        }

        r.line("");
        r.head("cancel drills — Back pressed during a load");
        for row in &self.drill_rows {
            let mut line = String::from(row.at);
            line.push_str(": ");
            if !row.cancelled {
                line.push_str("NOT CANCELLED (finished first)");
            } else if row.recovered() {
                line.push_str("cancelled, next fetch HTTP ");
                push_i64(&mut line, row.recovery_status as i64);
            } else if row.recovery_err == -999 {
                line.push_str("cancelled, then the SESSION HUNG");
            } else {
                line.push_str("cancelled, next fetch FAILED ");
                push_i64(&mut line, row.recovery_err as i64);
            }
            if row.strays > 0 {
                line.push_str(" (");
                push_i64(&mut line, row.strays as i64);
                line.push_str(" late events)");
            }
            r.line(&line);
        }
        let drilled = self.drill_rows.iter().filter(|d| d.cancelled).count();
        let survived = self.drill_rows.iter().filter(|d| d.cancelled && d.recovered()).count();
        r.check_note(
            "the session survives being cancelled",
            drilled > 0 && survived == drilled,
            "a wedged session turns one impatient Back into a browser that loads nothing",
        );
        r.num("drills that cancelled", drilled as i64);
        r.num("drills that recovered", survived as i64);
        r.line("");

        let redirects = self.rows.iter().filter(|r| !r.redirected_to.is_empty()).count();
        r.check_note(
            "silent redirects are visible",
            redirects > 0,
            "a page's relative links resolve against this, not against what was typed",
        );
        r.num("redirects observed", redirects as i64);
        // Reported, never judged. The first run's version asserted `chunked > 0` as a pass, which
        // was wrong twice over: a stack that decodes chunked before we ever see the header shows
        // ZERO here, and so does a run where no server happened to use it. Absence proves nothing,
        // so it must not be able to print FAIL.
        r.num("responses declaring chunked", chunked as i64);
        r.num("targets reached", reached as i64);
        r.num("targets tried", self.rows.len() as i64);

        r.line("");
        r.line("A row with ERR and a negative code near -7500 is a certificate the handset");
        r.line("would not trust. That is R7, and it is not a bug in this probe.");

        r.open_output(fs, "", "httpprobe.txt");
        r.finish(fs);
        self.report_path = String::from(r.path_label());
    }
}

/// The headless entry: the same state machine, no window.
///
/// `handle_raw` is shared with the [`App`] implementation rather than duplicated — every event this
/// probe cares about is a platform event, so there was never any UI logic in the path that matters.
impl<N: Net, H: Http> symbian_app::DaemonApp for HttpProbe<N, H> {
    fn handle_raw(&mut self, ev: &RawEvent) {
        let _ = App::handle_raw(self, ev);
    }

    fn should_exit(&self) -> bool {
        App::should_exit(self)
    }
}

impl<N: Net, H: Http> App for HttpProbe<N, H> {
    fn title(&self) -> &str {
        "httpprobe"
    }

    fn handle_raw(&mut self, ev: &RawEvent) -> Handled {
        if ev.kind == symbian_sys::SHIM_EV_TIMER {
            self.on_tick();
            self.report_if_finished();
            return Handled::Consumed;
        }

        // The bearer's own event. Its state machine owns the retry, so this only reacts to the
        // transition.
        if ev.kind == symbian_sys::SHIM_EV_NET_READY {
            let mut up = false;
            if let Some(b) = self.bearer.as_mut() {
                match b.on_event(&mut self.net, ev) {
                    Ok(true) => up = true,
                    Ok(false) => {}
                    Err(e) => {
                        self.fail("bearer", e.code());
                        return Handled::Consumed;
                    }
                }
            }
            if up {
                let iap = self.bearer.as_ref().and_then(|b| b.iap()).unwrap_or(0);
                symbian::log!("[httpprobe] bearer UP, iap={}", iap);
            } else if ev.status != 0 {
                symbian::log!("[httpprobe] bearer event status={}", ev.status);
            }
            if up && !self.session {
                self.begin();
            }
            return Handled::Consumed;
        }

        // The worker's completion, routed by KIND and before the HTTP guard below.
        //
        // This is where three device runs were lost. The guard was written when every event this
        // probe cared about was an HTTP one, and it discards anything else — so when the worker
        // drill arrived it was thrown away here, several checks before the phase routing that would
        // have handled it. The breadcrumbs said the thread ran, computed, posted its completion and
        // had its RunL dispatched; every one of those was true, and the event died in this function.
        //
        // Kind first, phase second. A phase check cannot rescue an event that a kind filter already
        // dropped, and putting the filter first made the bug look like a platform failure.
        if ev.kind == symbian_sys::SHIM_EV_WORK_DONE {
            self.work_event(ev);
            self.report_if_finished();
            return Handled::Consumed;
        }

        let is_http = ev.kind == symbian_sys::SHIM_EV_HTTP_HEAD
            || ev.kind == symbian_sys::SHIM_EV_HTTP_BODY
            || ev.kind == symbian_sys::SHIM_EV_HTTP_DONE;
        if !is_http {
            return Handled::Ignored;
        }

        // Every field, raw, before anything interprets it. The first run reported one error code
        // per target and nothing about where it came from — which stage, which event kind — and
        // that gap is why a second device round trip was needed at all.
        symbian::log!(
            "[httpprobe] ev kind={} status={} a={} b={} c={} d={}",
            ev.kind,
            ev.status,
            ev.a,
            ev.b,
            ev.c,
            ev.d
        );

        if self.phase == Phase::Revalidating {
            self.revalidation_event(ev);
            self.report_if_finished();
            return Handled::Consumed;
        }

        if self.phase == Phase::Drilling {
            // An event arriving for a transaction already cancelled. Counted rather than dropped
            // silently: the number is the answer to "does the stack stop talking when told to", and
            // zero is not the only acceptable value — one already queued is fine, being mistaken for
            // the next fetch's is not.
            if self.drill_cancelled && self.drill_stage == DrillStage::Recovering {
                if let Some(f) = self.fetch.as_ref() {
                    if f.url() == DRILL_TARGET {
                        self.drill_strays += 1;
                    }
                }
            }
            self.drill_event(ev);
            self.report_if_finished();
            return Handled::Consumed;
        }

        // `on_event_with`, not `on_event`: the effective URL can only be read while the transaction
        // is still open, so the completion is the one moment to ask.
        let progress = match self.fetch.as_mut() {
            Some(f) => f.on_event_with(&mut self.http, ev),
            None => return Handled::Consumed,
        };
        match progress {
            Progress::Idle => {}
            Progress::Head(_) => {}
            // Drain as bytes arrive rather than at the end. It is what a browser will do, and it
            // is the only way the shim's buffer cap does not decide the maximum page size.
            Progress::Body(_) => self.drain(),
            Progress::Done(r) => self.finish_target(Some(r), 0, false),
            Progress::Failed(e) => self.finish_target(None, e.code(), false),
            // The list sends unconditional requests, so a 304 here would mean the platform added a
            // validator of its own. Recorded as the odd thing it would be rather than ignored.
            Progress::NotModified => {
                symbian::log!("[httpprobe] unexpected 304 on an unconditional GET");
                self.finish_target(None, 304, false);
            }
        }
        self.report_if_finished();
        Handled::Consumed
    }

    fn handle_key(&mut self, ev: KeyEvent, _theme: &Theme<'_>, _screen: Rect) -> Handled {
        match ev.key {
            Key::Softkey(Softkey::Right) | Key::End => self.exit = true,
            _ => return Handled::Ignored,
        }
        Handled::Consumed
    }

    fn should_exit(&self) -> bool {
        self.exit
    }

    fn draw(&mut self, c: &mut Canvas<'_>, theme: &Theme<'_>) {
        use symbian_ui::Align;

        let screen = Rect::from_size(c.size());
        let frame = chrome::Frame::split(screen, theme, true, true);
        chrome::clear(c, theme);
        chrome::title_bar(c, frame.title, theme, "httpprobe", None);
        chrome::softkey_bar(c, frame.softkeys, theme, [None, None, Some("Exit")]);

        let line_h = theme.fonts.small.line_height();
        let mut y = frame.content.y0 + 2;

        // The tick counter first and biggest. It is the finding.
        let mut head = String::from("tick ");
        push_i64(&mut head, self.ticks as i64);
        head.push_str("   ");
        push_i64(&mut head, self.rows.len() as i64);
        head.push('/');
        push_i64(&mut head, TARGETS.len() as i64);
        let head_rect = Rect { y0: y, y1: y + theme.fonts.title.line_height(), ..frame.content };
        c.draw_text_in(head_rect, &head, theme.fonts.title, theme.palette.text, Align::Center);
        y = head_rect.y1 + 2;

        let note_rect = Rect { y0: y, y1: y + line_h, ..frame.content };
        c.draw_text_in(note_rect, &self.note, theme.fonts.small, theme.palette.dim, Align::Center);
        y = note_rect.y1 + 2;

        // The tail of the results, newest last — the interesting row is the one that just landed.
        let room = ((frame.content.y1 - y) / line_h.max(1)).max(0) as usize;
        let skip = self.rows.len().saturating_sub(room);
        for row in &self.rows[skip..] {
            let mut line = String::new();
            if row.reached_server() {
                push_i64(&mut line, row.status as i64);
            } else {
                push_i64(&mut line, row.err as i64);
            }
            line.push(' ');
            push_i64(&mut line, row.bytes as i64);
            line.push_str("B ");
            if row.flags.needs_inflate() {
                line.push_str("gz! ");
            } else if row.flags.gzip() {
                line.push_str("gz ");
            }
            line.push_str(short_host(row.url));

            let colour =
                if row.reached_server() { theme.palette.text } else { theme.palette.dim };
            let r = Rect { y0: y, y1: y + line_h, ..frame.content };
            c.draw_text_in(r, &line, theme.fonts.small, colour, Align::Start);
            y = r.y1;
        }

        if !self.report_path.is_empty() {
            let r = Rect { y0: frame.content.y1 - line_h, ..frame.content };
            c.draw_text_in(
                r,
                &self.report_path,
                theme.fonts.small,
                theme.palette.dim,
                Align::Center,
            );
        }
    }
}

/// A stable code for an inflate failure, for the report.
fn inflate_code(e: symbian_crypto::inflate::Error) -> i32 {
    use symbian_crypto::inflate::Error as E;
    match e {
        E::Truncated => -101,
        E::Corrupt => -102,
        E::BadDistance => -103,
        E::TooLarge => -104,
        E::ChecksumMismatch => -105,
        E::Sink => -106,
    }
}

/// A sink that only counts.
///
/// The point of the inflate design is that the decoded body never has to exist all at once, and a
/// probe that collected it into a `Vec` to measure it would defeat exactly that — and would be the
/// one thing on the list able to exhaust the heap. Counting exercises every byte of the decode path
/// and holds none of it.
#[derive(Default)]
struct Counter {
    n: usize,
}

impl symbian_crypto::inflate::Sink for Counter {
    fn write(&mut self, bytes: &[u8]) -> core::result::Result<(), symbian_crypto::inflate::Error> {
        self.n += bytes.len();
        Ok(())
    }
}

/// The host of a URL, for a 320-pixel line. Not a URL parser — [`symbian::url`] is that, and this
/// wants less: everything between the scheme and the next slash, so a row is recognisable.
fn short_host(url: &str) -> &str {
    let rest = match url.find("//") {
        Some(i) => &url[i + 2..],
        None => url,
    };
    match rest.find('/') {
        Some(i) => &rest[..i],
        None => rest,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use symbian::error::{Error, Result};
    use symbian::net::{Ipv4, Iap};

    /// A network that comes up when told to.
    struct FakeNet {
        attach_ok: bool,
        next_handle: i32,
    }

    impl Net for FakeNet {
        fn net_start(&mut self, iap: Iap) -> Result<i32> {
            if iap == Iap::Attach && !self.attach_ok {
                return Err(Error::NotFound);
            }
            self.next_handle += 1;
            Ok(self.next_handle)
        }
        fn net_stop(&mut self, _h: i32) {}
        fn resolve(&mut self, _c: i32, _h: &str) -> Result<i32> {
            Err(Error::NotFound)
        }
        fn dns_close(&mut self, _h: i32) {}
        fn tcp_open(&mut self, _c: i32) -> Result<i32> {
            Err(Error::NotFound)
        }
        fn tcp_connect(&mut self, _h: i32, _a: Ipv4, _p: u16) -> Result<()> {
            Err(Error::NotFound)
        }
        fn tcp_send(&mut self, _h: i32, _b: &[u8]) -> Result<()> {
            Err(Error::NotFound)
        }
        fn tcp_recv(&mut self, _h: i32, _b: &mut [u8]) -> Result<()> {
            Err(Error::NotFound)
        }
        fn tcp_close(&mut self, _h: i32) {}
        fn udp_open(&mut self, _c: i32) -> Result<i32> {
            Err(Error::NotFound)
        }
        fn udp_send_to(&mut self, _h: i32, _a: Ipv4, _p: u16, _b: &[u8]) -> Result<()> {
            Err(Error::NotFound)
        }
    }

    #[derive(Default)]
    struct FakeHttp {
        opened: Option<i32>,
        urls: Vec<String>,
        body: Vec<u8>,
        cancels: u32,
        /// Where the platform will claim the bytes came from.
        effective: String,
        /// What the platform will claim the response carried.
        validators: symbian::http::Validators,
        /// The conditional headers each request carried.
        conditions: Vec<(String, String)>,
    }

    impl Http for FakeHttp {
        fn open(&mut self, bearer: i32) -> Result<()> {
            self.opened = Some(bearer);
            Ok(())
        }
        fn get(&mut self, url: &str, gzip: bool) -> Result<()> {
            self.get_conditional(url, gzip, "", "")
        }
        fn get_conditional(
            &mut self,
            url: &str,
            _gzip: bool,
            etag: &str,
            last_modified: &str,
        ) -> Result<()> {
            self.urls.push(String::from(url));
            self.conditions.push((String::from(etag), String::from(last_modified)));
            Ok(())
        }
        fn validators(&mut self) -> Result<symbian::http::Validators> {
            Ok(self.validators.clone())
        }
        fn reset(&mut self, bearer: i32) -> Result<()> {
            self.opened = Some(bearer);
            Ok(())
        }
        fn read(&mut self, out: &mut [u8]) -> Result<usize> {
            let n = core::cmp::min(out.len(), self.body.len());
            out[..n].copy_from_slice(&self.body[..n]);
            self.body.drain(..n);
            Ok(n)
        }
        fn effective_url(&mut self) -> Result<String> {
            Ok(self.effective.clone())
        }
        fn cancel(&mut self) {
            self.cancels += 1;
        }
    }

    fn probe(attach_ok: bool) -> HttpProbe<FakeNet, FakeHttp> {
        HttpProbe::with(FakeNet { attach_ok, next_handle: 0 }, FakeHttp::default())
    }

    fn ev(kind: i32, status: i32, a: i32, b: i32, c: i32, d: i32) -> RawEvent {
        RawEvent { kind, status, a, b, c, d, ..Default::default() }
    }

    /// Complete the whole target list, leaving the probe wherever it goes next.
    ///
    /// With no validators in the fake, revalidation has nothing to ask and the probe lands straight
    /// in the drills — which is what the drill tests want.
    fn run_list(p: &mut HttpProbe<FakeNet, FakeHttp>) {
        for _ in 0..TARGETS.len() {
            p.handle_raw(&ev(symbian_sys::SHIM_EV_HTTP_DONE, 0, 200, 10, 0, 1));
        }
    }

    /// Carry every drill to its end, whichever point it cancels at.
    fn run_drills(p: &mut HttpProbe<FakeNet, FakeHttp>) {
        // Bounded rather than `while`: a drill that fails to advance is the bug this whole section
        // is about, and a test that hung on it would report nothing.
        for _ in 0..(DRILLS.len() * 12) {
            if p.phase() != Phase::Drilling {
                return;
            }
            match DRILLS[p.drill] {
                Drill::BeforeAnything => {}
                Drill::OnHeaders if p.drill_stage == DrillStage::Loading => {
                    p.handle_raw(&ev(symbian_sys::SHIM_EV_HTTP_HEAD, 0, 200, 0, 0, 0));
                    continue;
                }
                Drill::OnBodyPart(_) if p.drill_stage == DrillStage::Loading => {
                    p.handle_raw(&ev(symbian_sys::SHIM_EV_HTTP_BODY, 0, 8, 0, 0, 0));
                    continue;
                }
                _ => {}
            }
            p.handle_raw(&ev(symbian_sys::SHIM_EV_HTTP_DONE, 0, 200, 10, 0, 1));
        }
    }

    fn tick(p: &mut HttpProbe<FakeNet, FakeHttp>) {
        p.handle_raw(&ev(symbian_sys::SHIM_EV_TIMER, 0, 0, 0, 0, 0));
    }

    /// The bearer's ready event carries a handle, and the probe must not open a session before it.
    fn bring_up(p: &mut HttpProbe<FakeNet, FakeHttp>) {
        tick(p); // Waking -> Connecting
        let handle = p.bearer.as_ref().expect("a bearer was requested").handle();
        let mut e = ev(symbian_sys::SHIM_EV_NET_READY, 0, 1, 0, 0, 0);
        e.handle = handle;
        p.handle_raw(&e);
    }

    #[test]
    fn nothing_touches_the_network_before_the_first_tick() {
        let p = probe(true);
        assert_eq!(p.phase(), Phase::Waking);
        assert!(p.bearer.is_none(), "bring-up must wait for a frame to exist");
    }

    #[test]
    fn the_first_tick_asks_for_exactly_one_bearer() {
        let mut p = probe(true);
        tick(&mut p);
        assert_eq!(p.phase(), Phase::Connecting);
        // One request, whichever strategy is compiled in. Two would mean two bearers, and on a
        // phone with Wi-Fi and packet data both available that is two routes and one of them paid
        // for.
        assert_eq!(p.net.next_handle, 1);
        assert!(p.bearer.is_some());
    }

    /// The strategy is a knob, and the probe must not be quietly using the other one.
    ///
    /// Asserted rather than assumed because it is the whole experiment: the first device run used
    /// Attach and every target failed, so a build that claims Default and attaches anyway would
    /// reproduce the failure and look like a refutation.
    #[test]
    fn the_default_strategy_never_joins_an_existing_connection() {
        // FakeNet refuses Attach when built this way, so an attaching probe cannot get a bearer.
        let mut p = probe(false);
        tick(&mut p);
        match BEARER {
            Strategy::Default => {
                assert!(p.bearer.is_some(), "Default must not depend on a live connection");
            }
            Strategy::Attach => {
                // Attach with nothing to join: no fallback, so this is a dead end by design.
                assert_eq!(p.phase(), Phase::Failed);
            }
        }
    }

    #[test]
    fn the_session_opens_on_the_bearer_that_came_up() {
        let mut p = probe(true);
        bring_up(&mut p);
        assert_eq!(p.phase(), Phase::Fetching);
        assert_eq!(p.http.opened, Some(1));
        assert_eq!(p.http.urls, vec![String::from(TARGETS[0].url)]);
    }

    #[test]
    fn ticks_keep_rising_while_a_fetch_is_in_flight() {
        // The whole point of the probe, asserted in the only place a test can assert it: the tick
        // handler must not be reachable only when nothing is loading.
        let mut p = probe(true);
        bring_up(&mut p);
        let before = p.ticks();
        tick(&mut p);
        tick(&mut p);
        assert_eq!(p.ticks(), before + 2);
        assert_eq!(p.phase(), Phase::Fetching, "ticking must not disturb the fetch");
    }

    #[test]
    fn a_completed_fetch_records_and_advances() {
        let mut p = probe(true);
        bring_up(&mut p);
        p.http.body = vec![b'x'; 40];

        p.handle_raw(&ev(symbian_sys::SHIM_EV_HTTP_HEAD, 0, 200, 0, 0, 0));
        p.handle_raw(&ev(symbian_sys::SHIM_EV_HTTP_BODY, 0, 40, 0, 0, 0));
        let flags = symbian_sys::SHIM_HTTP_GZIP | symbian_sys::SHIM_HTTP_GZIP_MAGIC;
        p.handle_raw(&ev(symbian_sys::SHIM_EV_HTTP_DONE, 0, 200, 40, flags, 1));

        assert_eq!(p.rows().len(), 1);
        let row = &p.rows()[0];
        assert_eq!(row.url, TARGETS[0].url);
        assert_eq!(row.status, 200);
        assert_eq!(row.bytes, 40);
        assert!(row.flags.needs_inflate());
        assert!(row.reached_server());
        // And it moved on, rather than sitting on a finished transaction.
        assert_eq!(p.http.urls.len(), 2);
    }

    #[test]
    fn a_failure_is_a_row_and_not_the_end_of_the_list() {
        let mut p = probe(true);
        bring_up(&mut p);
        // -7548 stands in for a certificate the handset will not trust: the case R7 is about, and
        // the one that must not stop the other nine targets.
        p.handle_raw(&ev(symbian_sys::SHIM_EV_HTTP_DONE, -7548, 0, 0, 0, 0));

        assert_eq!(p.rows().len(), 1);
        assert_eq!(p.rows()[0].err, -7548);
        assert!(!p.rows()[0].reached_server());
        assert_eq!(p.phase(), Phase::Fetching);
        assert_eq!(p.http.urls.len(), 2, "the list must continue");
    }

    #[test]
    fn a_fetch_that_never_answers_times_out_and_the_list_moves_on() {
        let mut p = probe(true);
        bring_up(&mut p);
        for _ in 0..(TICKS_PER_TARGET + 1) {
            tick(&mut p);
        }
        assert_eq!(p.rows().len(), 1);
        assert!(p.rows()[0].timed_out);
        assert_eq!(p.http.cancels, 1, "a timeout must cancel, not abandon");
        assert_eq!(p.http.urls.len(), 2);
    }

    /// It must close itself, or the next build cannot be pushed over it.
    #[test]
    fn a_finished_probe_lingers_then_closes_itself() {
        let mut p = probe(true);
        bring_up(&mut p);
        run_list(&mut p);
        run_drills(&mut p);
        assert_eq!(p.phase(), Phase::Finished);
        assert!(!p.should_exit(), "the last screen has to be readable");

        for _ in 0..(LINGER_TICKS - 1) {
            tick(&mut p);
        }
        assert!(!p.should_exit(), "closing early loses the screen");
        tick(&mut p);
        assert!(p.should_exit(), "a probe that never closes holds its own binary open");
    }

    #[test]
    fn the_whole_list_runs_and_then_the_drills_do() {
        let mut p = probe(true);
        bring_up(&mut p);
        run_list(&mut p);
        assert_eq!(p.rows().len(), TARGETS.len());
        assert_eq!(p.phase(), Phase::Drilling, "the list flows into the cancel drills");
        run_drills(&mut p);
        assert_eq!(p.phase(), Phase::Finished);
        assert_eq!(p.drill_rows().len(), DRILLS.len());
    }

    /// A page that sent a validator gets asked whether it is still current, before the drills.
    #[test]
    fn pages_with_a_validator_are_revalidated() {
        let mut p = probe(true);
        p.http.validators = symbian::http::Validators {
            etag: String::from("\"v1\""),
            last_modified: String::new(),
        };
        bring_up(&mut p);
        // Each target needs its headers seen for the validator to be captured.
        for _ in 0..TARGETS.len() {
            p.handle_raw(&ev(symbian_sys::SHIM_EV_HTTP_HEAD, 0, 200, 0, 0, 0));
            p.handle_raw(&ev(symbian_sys::SHIM_EV_HTTP_DONE, 0, 200, 10, 0, 1));
        }
        assert!(p.rows().iter().all(|r| r.etag == "\"v1\""), "the validator reached the rows");
        assert_eq!(p.phase(), Phase::Revalidating, "the list flows into revalidation");

        // The conditional request must carry the stored ETag, not nothing.
        let last = p.http.conditions.last().unwrap();
        assert_eq!(last.0, "\"v1\"", "the refetch is conditional on what was stored");

        // Every one answers 304.
        for _ in 0..TARGETS.len() {
            if p.phase() != Phase::Revalidating {
                break;
            }
            p.handle_raw(&ev(symbian_sys::SHIM_EV_HTTP_DONE, 0, 304, 0, 0, 0));
        }
        assert!(p.rows().iter().all(|r| r.revalidated == 304), "all should be unchanged");
        assert_eq!(p.phase(), Phase::Drilling, "and then the drills run");
    }

    /// With no validator there is nothing to ask, so the pass is skipped rather than sending a
    /// conditional request with empty headers — which is just a second full download.
    #[test]
    fn pages_without_a_validator_are_not_revalidated() {
        let mut p = probe(true);
        bring_up(&mut p);
        run_list(&mut p);
        assert!(p.rows().iter().all(|r| r.revalidated == 0));
        assert_eq!(p.phase(), Phase::Drilling, "straight to the drills");
    }

    /// A 200 answering a conditional request is recorded as what it is, not as a failure.
    #[test]
    fn a_stale_copy_is_recorded_rather_than_treated_as_an_error() {
        let mut p = probe(true);
        p.http.validators =
            symbian::http::Validators { etag: String::from("\"old\""), last_modified: String::new() };
        bring_up(&mut p);
        for _ in 0..TARGETS.len() {
            p.handle_raw(&ev(symbian_sys::SHIM_EV_HTTP_HEAD, 0, 200, 0, 0, 0));
            p.handle_raw(&ev(symbian_sys::SHIM_EV_HTTP_DONE, 0, 200, 10, 0, 1));
        }
        assert_eq!(p.phase(), Phase::Revalidating);
        p.handle_raw(&ev(symbian_sys::SHIM_EV_HTTP_DONE, 0, 200, 99, 0, 1));
        assert_eq!(p.rows()[0].revalidated, 200);
        assert_eq!(p.rows()[0].revalidate_err, 0, "a changed page is not an error");
    }

    /// Every drill must cancel and then get its follow-up fetch answered. That second half is the
    /// finding: a cancel that wedges the session would show up here and nowhere else.
    #[test]
    fn every_drill_cancels_and_the_session_survives() {
        let mut p = probe(true);
        bring_up(&mut p);
        run_list(&mut p);
        run_drills(&mut p);

        assert_eq!(p.drill_rows().len(), DRILLS.len());
        for (i, row) in p.drill_rows().iter().enumerate() {
            assert!(row.cancelled, "drill {i} ({}) never cancelled", row.at);
            assert!(row.recovered(), "drill {i} ({}) left the session unusable", row.at);
        }
    }

    /// The drill cancels the big target and recovers on the small one — not the other way round, and
    /// not by quietly skipping the cancel.
    #[test]
    fn a_drill_cancels_the_big_target_and_then_fetches_another() {
        let mut p = probe(true);
        bring_up(&mut p);
        run_list(&mut p);
        // The first drill cancels before any event, so finishing the list has already issued both
        // of its fetches: the big target and, after the cancel, the recovery.
        let n = p.http.urls.len();
        assert_eq!(p.http.urls[n - 2], DRILL_TARGET, "the drill loads the big page");
        assert!(p.http.cancels >= 1, "and cancels it");
        assert_eq!(p.http.urls[n - 1], DRILL_RECOVERY, "then asks the session for something else");
    }

    /// A drill that never advances is recorded, not waited on. The timeout is the shape a wedged
    /// session would actually take.
    #[test]
    fn a_wedged_drill_times_out_and_is_reported() {
        let mut p = probe(true);
        bring_up(&mut p);
        run_list(&mut p);
        assert_eq!(p.phase(), Phase::Drilling);

        // Feed nothing but ticks: no completion ever arrives.
        for _ in 0..(TICKS_PER_TARGET * (DRILLS.len() as u32) + DRILLS.len() as u32 + 4) {
            tick(&mut p);
        }
        assert_eq!(p.drill_rows().len(), DRILLS.len(), "each drill gave up rather than hanging");
        assert!(
            p.drill_rows().iter().all(|r| !r.recovered()),
            "a timed-out drill must not read as a pass"
        );
    }

    /// A followed redirect lands in the row, because that is what a page's links resolve against.
    #[test]
    fn a_followed_redirect_is_recorded() {
        let mut p = probe(true);
        p.http.effective = String::from("https://www.google.com/");
        bring_up(&mut p);
        // TARGETS[0] is http://example.com/, so the fake's answer differs from what was asked.
        p.handle_raw(&ev(symbian_sys::SHIM_EV_HTTP_DONE, 0, 200, 10, 0, 1));
        assert_eq!(p.rows()[0].redirected_to, "https://www.google.com/");
    }

    /// No redirect leaves the field empty rather than echoing the request back.
    #[test]
    fn no_redirect_leaves_the_field_empty() {
        let mut p = probe(true);
        p.http.effective = String::from(TARGETS[0].url);
        bring_up(&mut p);
        p.handle_raw(&ev(symbian_sys::SHIM_EV_HTTP_DONE, 0, 200, 10, 0, 1));
        assert!(p.rows()[0].redirected_to.is_empty());
    }

    // ------------------------------------------------------------------- the worker --

    /// The job itself is pure, so it is checked here rather than only on the phone.
    #[test]
    fn the_worker_job_sums_every_byte() {
        let payload: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
        let expect: u64 = payload.iter().map(|&b| b as u64).sum();

        let mut out = [0u8; 16];
        assert_eq!(worker_dispatch(OP_ECHO_SUM, &payload, &mut out), 0);

        let mut s = [0u8; 8];
        let mut l = [0u8; 8];
        s.copy_from_slice(&out[..8]);
        l.copy_from_slice(&out[8..16]);
        assert_eq!(u64::from_le_bytes(s), expect);
        assert_eq!(u64::from_le_bytes(l), payload.len() as u64);
    }

    /// A byte lost anywhere changes the answer, which is why the job sums rather than copies.
    #[test]
    fn a_truncated_payload_gives_a_different_sum() {
        let payload: Vec<u8> = (1..=200u8).collect();
        let mut full = [0u8; 16];
        let mut short = [0u8; 16];
        worker_dispatch(OP_ECHO_SUM, &payload, &mut full);
        worker_dispatch(OP_ECHO_SUM, &payload[..199], &mut short);
        assert_ne!(full, short, "the check has to be sensitive to a single lost byte");
    }

    /// The dispatcher refuses what it does not know, rather than answering it.
    #[test]
    fn the_dispatcher_refuses_an_unknown_opcode() {
        let mut out = [0u8; 16];
        // Deliberately not `OP_ECHO_SUM + 1`, which is OP_NOP and is handled — an "unknown" opcode
        // computed by arithmetic stops being unknown the moment a neighbour is added.
        assert_eq!(worker_dispatch(99, &[1, 2, 3], &mut out), -1);
    }

    /// The do-nothing job is the bisect, so it has to actually answer.
    #[test]
    fn the_nop_job_writes_its_marker() {
        let mut out = [0u8; 16];
        assert_eq!(worker_dispatch(OP_NOP, &[0u8; 1], &mut out), 0);
        assert_eq!(out[0], 0xA5, "the marker is what the probe checks for");
        let mut l = [0u8; 8];
        l.copy_from_slice(&out[8..16]);
        assert_eq!(u64::from_le_bytes(l), 1);
    }

    /// Too small an output buffer is refused rather than written past.
    #[test]
    fn the_dispatcher_refuses_a_short_output_buffer() {
        let mut out = [0u8; 15];
        assert_eq!(worker_dispatch(OP_ECHO_SUM, &[1], &mut out), -2);
    }

    /// A worker completion must reach the drill, whatever the phase.
    ///
    /// The regression this pins: the routing filtered events by kind before it looked at the phase,
    /// and the filter listed only HTTP kinds — so `SHIM_EV_WORK_DONE` was dropped in `handle_raw`
    /// and three device runs were spent proving that the platform, which was working, was working.
    #[test]
    fn a_worker_completion_is_routed_and_not_filtered_out() {
        let mut p = probe(true);
        bring_up(&mut p);
        // Phase is Fetching — deliberately not Working. The event must still be routed by kind.
        assert_eq!(p.phase(), Phase::Fetching);
        assert_eq!(
            p.handle_raw(&ev(symbian_sys::SHIM_EV_WORK_DONE, 0, 0, 0, 0, 0)),
            Handled::Consumed,
            "a worker completion must never be filtered out as 'not an HTTP event'"
        );
    }

    /// The drill runs after the cancel drills, and a shim that refuses it is recorded rather than
    /// silently skipped. On the host every extern is a stub, so this is the refusal path.
    #[test]
    fn the_worker_drill_runs_last_and_records_its_refusal() {
        let mut p = probe(true);
        bring_up(&mut p);
        run_list(&mut p);
        run_drills(&mut p);
        assert_eq!(p.phase(), Phase::Finished);

        let w = p.work_row();
        assert!(!w.submitted, "the host has no worker, so submission must fail");
        assert_ne!(w.err, 0, "and the reason must be recorded, not swallowed");
    }

    /// A timed-out fetch is not a page: nothing decoded, nothing cached.
    ///
    /// Both were happening. The partial body went through `decode_to` with no flags, which passed
    /// the raw bytes to the counter and reported them as decoded; and it was then stored, so a
    /// later hit would have served a truncated page as a complete one.
    #[test]
    fn a_timed_out_fetch_is_neither_decoded_nor_cached() {
        let mut p = probe(true);
        bring_up(&mut p);
        p.http.body = vec![b'x'; 4096];
        // Bytes arrive, then the tick budget runs out before the transaction ends.
        p.handle_raw(&ev(symbian_sys::SHIM_EV_HTTP_BODY, 0, 4096, 0, 0, 0));
        for _ in 0..(TICKS_PER_TARGET + 1) {
            tick(&mut p);
        }

        let row = &p.rows()[0];
        assert!(row.timed_out);
        assert_eq!(row.bytes, 4096, "what arrived is still reported");
        assert_eq!(row.decoded, 0, "a partial body is not a decoded page");
        assert_eq!(row.cached_decoded, 0, "and must not be stored as one");
        assert_eq!(row.cache_err, 0, "not storing it is not an error");
    }

    /// A compressed body reaches the row decoded, which is the whole F3 addition.
    #[test]
    fn a_gzip_body_is_decoded_into_the_row() {
        // A tiny gzip member: one stored block holding "hi", with a correct trailer. Small enough
        // to write out, which the streaming tests in symbian-crypto deliberately are not.
        let plain = b"hi";
        let mut member: Vec<u8> = vec![0x1F, 0x8B, 8, 0, 0, 0, 0, 0, 0, 0];
        // BFINAL=1, BTYPE=00 (stored), then align, LEN=2, NLEN=!2.
        member.push(0x01);
        member.extend_from_slice(&2u16.to_le_bytes());
        member.extend_from_slice(&(!2u16).to_le_bytes());
        member.extend_from_slice(plain);
        let mut crc = 0xFFFF_FFFFu32;
        for &b in plain {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 { 0xEDB8_8320 ^ (crc >> 1) } else { crc >> 1 };
            }
        }
        member.extend_from_slice(&(!crc).to_le_bytes());
        member.extend_from_slice(&(plain.len() as u32).to_le_bytes());

        let mut p = probe(true);
        bring_up(&mut p);
        p.http.body = member.clone();
        let flags = symbian_sys::SHIM_HTTP_GZIP | symbian_sys::SHIM_HTTP_GZIP_MAGIC;
        p.handle_raw(&ev(symbian_sys::SHIM_EV_HTTP_BODY, 0, member.len() as i32, 0, 0, 0));
        p.handle_raw(&ev(
            symbian_sys::SHIM_EV_HTTP_DONE,
            0,
            200,
            member.len() as i32,
            flags,
            1,
        ));

        let row = &p.rows()[0];
        assert_eq!(row.decode_err, 0, "a valid gzip body must decode");
        assert_eq!(row.decoded, plain.len(), "decoded size is the plain size");
    }

    #[test]
    fn body_bytes_are_drained_as_they_arrive() {
        let mut p = probe(true);
        bring_up(&mut p);
        p.http.body = vec![b'y'; 100];
        p.handle_raw(&ev(symbian_sys::SHIM_EV_HTTP_BODY, 0, 100, 0, 0, 0));
        assert!(p.http.body.is_empty(), "streaming, not buffering to the end");
        assert_eq!(p.drained, 100);
    }

    #[test]
    fn every_target_states_why_it_is_in_the_list() {
        for t in TARGETS {
            assert!(!t.why.is_empty(), "{} has no stated purpose", t.url);
        }
    }

    #[test]
    fn hosts_shorten_for_a_narrow_line() {
        assert_eq!(short_host("https://en.m.wikipedia.org/wiki/Symbian"), "en.m.wikipedia.org");
        assert_eq!(short_host("http://example.com/"), "example.com");
        assert_eq!(short_host("https://example.com"), "example.com");
    }

    #[test]
    fn draw_fills_the_screen_in_every_phase() {
        for setup in [0usize, 1, 2] {
            let mut p = probe(true);
            if setup >= 1 {
                bring_up(&mut p);
            }
            if setup >= 2 {
                p.handle_raw(&ev(symbian_sys::SHIM_EV_HTTP_DONE, 0, 200, 10, 0, 1));
            }
            let (_, px) = symbian_ui::testing::with_canvas(
                symbian_gfx::Size::new(320, 240),
                |c| {
                    symbian_ui::testing::with_theme(symbian_ui::Palette::DARK, |theme| {
                        p.draw(c, theme)
                    });
                },
            );
            assert!(px.iter().any(|&v| v != 0), "empty frame in setup {}", setup);
        }
    }
}
