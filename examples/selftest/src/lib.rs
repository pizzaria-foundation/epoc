//! Everything the SDK can do, run once, written to a text file you can carry off the
//! phone.
//!
//! # Why this exists
//!
//! Every previous device question cost a build, a transfer, an install, a photo and a
//! reading. That loop is fine for one unknown and absurd for forty. This runs the whole
//! battery unattended and leaves a report.
//!
//! # Where the report lands
//!
//! Tried in order, first one that works: `E:\` (memory card, visible over USB mass
//! storage), `C:\Data\`, `C:\`, then the app's private directory as a last resort. The
//! private directory needs no capability but is invisible to the file manager and to USB,
//! which makes it useless for a file whose entire purpose is to be carried away — so it
//! is the fallback, not the default. The path that worked is printed on screen.
//!
//! # The report is written after every phase
//!
//! Not once at the end. If a phase panics or the app dies, the file still holds
//! everything up to that point — and on a platform where a fault shows as the app simply
//! closing, the last line written is the diagnosis.
//!
//! # What it measures
//!
//! The timings are the part that cannot be got any other way. Every performance number in
//! this SDK's documentation so far has been an estimate scaled from a host measurement,
//! and at least one of them was wrong by a factor of two. These are the real ones.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use symbian::fs::{self, OpenMode, ShimFs, Utf16Path};
use symbian::net::{Iap, Ipv4, Net, ShimNet};
use symbian_crypto as crypto;
use symbian_sys as sys;
use symbian_ui::{chrome, App, Canvas, Handled, Key, KeyEvent, RawEvent, Rect, Softkey, Theme};

/// Where `tools/echo.py` listens. Change and rebuild.
const ECHO_ADDR: Ipv4 = Ipv4::new(192, 168, 15, 74);
const ECHO_PORT: u16 = 7654;
const HTTP_HOST: &str = "example.com";
/* A literal address for the same host, used when the lookup fails.
 *
 * Without it the phases chain: no DNS means no HTTP, and a run that cannot resolve tells
 * you nothing about whether TCP to the internet works. Separating them turns one failure
 * into two independent answers -- "DNS is broken but sockets are fine" is a diagnosis,
 * "the network phase failed" is not. */
const HTTP_FALLBACK: Ipv4 = Ipv4::new(104, 20, 23, 154);

pub const OP_MODPOW: i32 = 1;

/// Runs on the worker thread. Input is three length-prefixed fields: modulus, base,
/// exponent. It allocates nothing, which is the contract for anything on that thread.
pub fn modpow_job(opcode: i32, input: &[u8], out: &mut [u8]) -> i32 {
    if opcode != OP_MODPOW {
        return sys::SHIM_ERR_NOT_SUPPORTED;
    }
    let mut fields: [&[u8]; 3] = [&[], &[], &[]];
    let mut rest = input;
    for f in fields.iter_mut() {
        if rest.len() < 2 {
            return sys::SHIM_ERR_ARGUMENT;
        }
        let n = u16::from_be_bytes([rest[0], rest[1]]) as usize;
        if rest.len() < 2 + n {
            return sys::SHIM_ERR_ARGUMENT;
        }
        *f = &rest[2..2 + n];
        rest = &rest[2 + n..];
    }
    let Ok(m) = crypto::bignum::Modulus::new(fields[0]) else {
        return sys::SHIM_ERR_ARGUMENT;
    };
    match crypto::bignum::modpow(fields[1], fields[2], &m, out) {
        Ok(()) => 0,
        Err(_) => sys::SHIM_ERR_ARGUMENT,
    }
}

// --------------------------------------------------------------------------- report --

/// The report, built as text and flushed after every phase.
struct Report {
    text: String,
    pass: u32,
    fail: u32,
    /// Where it is being written, once a writable location is found.
    path: Option<Utf16Path>,
    /// Human-readable version of the same, for the screen.
    path_label: String,
}

impl Report {
    fn new() -> Self {
        Report { text: String::new(), pass: 0, fail: 0, path: None, path_label: String::new() }
    }

    fn line(&mut self, s: &str) {
        self.text.push_str(s);
        self.text.push('\n');
    }

    fn head(&mut self, s: &str) {
        self.text.push('\n');
        self.text.push_str("== ");
        self.text.push_str(s);
        self.text.push('\n');
    }

    /// A check with a verdict. The prefix is fixed-width so the file can be grepped for
    /// `FAIL` and skimmed by eye.
    fn check(&mut self, name: &str, ok: bool) {
        if ok {
            self.pass += 1;
            self.text.push_str("  ok   ");
        } else {
            self.fail += 1;
            self.text.push_str("  FAIL ");
        }
        self.text.push_str(name);
        self.text.push('\n');
    }

    fn check_note(&mut self, name: &str, ok: bool, note: &str) {
        if ok {
            self.pass += 1;
            self.text.push_str("  ok   ");
        } else {
            self.fail += 1;
            self.text.push_str("  FAIL ");
        }
        self.text.push_str(name);
        self.text.push_str("  ");
        self.text.push_str(note);
        self.text.push('\n');
    }

    fn info(&mut self, key: &str, value: &str) {
        self.text.push_str("  .    ");
        self.text.push_str(key);
        self.text.push_str(": ");
        self.text.push_str(value);
        self.text.push('\n');
    }

    fn num(&mut self, key: &str, v: i64) {
        let mut s = String::new();
        push_i64(&mut s, v);
        self.info(key, &s);
    }

    /// Find somewhere writable, most useful first.
    fn open_output(&mut self, fs: &mut ShimFs) {
        // C:\Data\ first, because it is where the earlier probe wrote rustsdk.txt and is
        // therefore the one location on this handset already known to be both writable
        // and reachable. E: would be nicer — it appears over USB mass storage — but a
        // phone with no memory card makes it a dead end, and a report nobody can find is
        // the same as no report.
        let candidates = ["C:\\Data\\rustsdk-selftest.txt",
            "E:\\rustsdk-selftest.txt",
            "C:\\rustsdk-selftest.txt"];
        for c in candidates {
            let Ok(p) = Utf16Path::new(c) else { continue };
            if fs::write_atomic(fs, &p, b"").is_ok() {
                self.path = Some(p);
                self.path_label = String::from(c);
                return;
            }
        }
        // The data cage always works and nobody can get at it. Better than nothing, and
        // labelled so the screen can say so.
        if let Ok(dir) = fs::private_path(fs) {
            if let Ok(p) = Utf16Path::join(dir.as_units(), "rustsdk-selftest.txt") {
                if fs::write_atomic(fs, &p, b"").is_ok() {
                    self.path = Some(p);
                    self.path_label = String::from("(private dir - not reachable over USB)");
                }
            }
        }
    }

    /// Rewrite the whole file. Called before and after every phase, so a crash leaves
    /// the report intact and naming the phase it died in.
    fn flush(&mut self, fs: &mut ShimFs) {
        if let Some(p) = &self.path {
            let _ = fs::write_atomic(fs, p, self.text.as_bytes());
        }
    }
}

fn push_i64(s: &mut String, mut v: i64) {
    if v < 0 {
        s.push('-');
        v = -v;
    }
    let mut d = [0u8; 20];
    let mut n = 0;
    loop {
        d[n] = b'0' + (v % 10) as u8;
        n += 1;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    for i in (0..n).rev() {
        s.push(d[i] as char);
    }
}

fn hex(data: &[u8]) -> String {
    let mut s = String::new();
    for b in data {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap_or('?'));
        s.push(char::from_digit((b & 15) as u32, 16).unwrap_or('?'));
    }
    s
}

fn unhex(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    (0..b.len() / 2)
        .map(|i| {
            let hi = (b[i * 2] as char).to_digit(16).unwrap_or(0) as u8;
            let lo = (b[i * 2 + 1] as char).to_digit(16).unwrap_or(0) as u8;
            (hi << 4) | lo
        })
        .collect()
}

fn now_us() -> u64 {
    unsafe { sys::shim_now_us() }
}

// ---------------------------------------------------------------------------- phases --

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Phase {
    Platform,
    Libraries,
    Storage,
    Hashes,
    Ciphers,
    Random,
    Bignum,
    Inflate,
    Timings,
    Graphics,
    WorkerStart,
    WorkerWait,
    /// Waits for the bearer, which is negotiating concurrently and has been since the
    /// first tick. There is no BearerWait beside it any more: the sweep stopped being a
    /// step in the sequence when the access-point dialog stopped being the last thing to
    /// happen.
    BearerSweep,
    Dns,
    DnsWait,
    Tcp,
    TcpWait,
    Http,
    HttpWait,
    Done,
}

pub struct SelfTest {
    phase: Phase,
    report: Report,
    fs: ShimFs,
    net: ShimNet,
    exit: bool,
    started: bool,

    /// Drives the state machine. One phase per tick, so the screen updates and the app
    /// stays responsive instead of freezing for the length of the whole battery.
    driver: Option<i32>,
    deadline: Option<i32>,
    /// The bearer's own deadline, separate from the phase sequence's.
    ///
    /// It has to be separate because the two now run at the same time: bringing a
    /// connection up is asynchronous and there was never a reason to serialise it behind
    /// ten seconds of arithmetic. One timer shared between them would have the graphics
    /// phase cancel the access-point dialog's.
    bearer_deadline: Option<i32>,

    // worker
    work_in: Vec<u8>,
    work_out: alloc::boxed::Box<[u8]>,
    work_started_us: u64,
    /// Ticks the GUI thread served while the job ran. The number is the proof: if the
    /// computation were on this thread it would be zero.
    work_ticks: u32,

    // network
    sweep_at: usize,
    sweep_handle: i32,
    bearer_handle: i32,
    bearer_iap: i32,
    /// The sweep is exhausted and said so once. Only so the line is not repeated.
    bearer_none: bool,
    attempt_started_us: u64,
    /// What to try, in order: the two id-less strategies then every access point the
    /// handset actually has.
    sweep: Vec<i32>,
    /// Names for the report, parallel to the tail of `sweep`.
    iap_names: Vec<String>,
    dns_handle: i32,
    tcp_handle: i32,
    resolved: Option<Ipv4>,
    rx_total: usize,
    /// The read target. A field, not a local, because the shim holds a pointer to it
    /// until the read completes and a local would be gone by then.
    rx_buf: alloc::boxed::Box<[u8]>,
    /// Likewise for the send.
    tx: Vec<u8>,
    rx_seen: Vec<u8>,
    phase_started_us: u64,

    /// What the screen shows.
    status: String,
}

/* Numbered access points first, then the two that ask the system to choose.
 *
 * The order is backwards from the obvious one, and the reason is in an earlier report:
 *
 *     IAP 1: err -1
 *     FAIL timed out  bearer      <- this was IAP 2
 *
 * An id that does not exist answers KErrNotFound immediately. An id that *times out* is
 * one the stack accepted and was still trying to bring up when the 12 s deadline fired.
 * So IAP 2 exists and was working, and the sweep killed it. Reading that timeout as
 * "another bad guess" is what sent the previous round chasing the comms database, which
 * added six commdb ordinals this handset does not export and stopped the image loading
 * altogether.
 *
 * Which makes the numbered sweep cheap rather than wasteful: a missing id costs one round
 * trip, so twenty of them cost almost nothing, and the ones that cost real time are
 * exactly the ones worth waiting for. The strategies that ask the system to choose go
 * last, because on this handset neither has ever completed and both are slow to say so. */

/* The prompt first, deliberately.
 *
 * It is the entry that shows the access-point dialog, and the dialog is the one part of
 * this that waits on a person -- so it has to be asked for in the first tick, while
 * somebody is still looking at the phone. Everything else in the sweep is the machine
 * talking to itself and can happen behind ten seconds of arithmetic. */
const SWEEP_HEAD: [i32; 23] = [
    // Attach first: if anything else on the handset is online this joins it immediately,
    // with no dialog and nothing to wait for. It is also the strategy that should have been
    // here all along -- what preceded it opened a socket with no RConnection, which uses
    // the *configured default connection* rather than one that is up, and reported success
    // on a handset that had neither.
    sys::SHIM_IAP_ATTACH,
    sys::SHIM_IAP_PROMPT,
    sys::SHIM_IAP_DEFAULT,
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20,
];

/* One deadline for every strategy, and a long one.
 *
 * Three rounds of tuning this number were all wrong for the same reason: I sized it for
 * network negotiation. It is not waiting for a network. On this handset a connection
 * attempt raises a dialog and waits for a person -- not only the strategy named "prompt",
 * which is what I had assumed, but any of them. One access point timed out at 35013 ms in
 * two separate runs, to the millisecond, which is not what a radio failing to associate
 * looks like. It is what a countdown looks like.
 *
 * So the deadline is no longer a measurement of anything. It is only there to stop an
 * unattended run hanging forever, and it is set well past human reaction time. */
const BEARER_DEADLINE_S: i32 = 150;

impl Default for SelfTest {
    fn default() -> Self {
        Self::new()
    }
}

impl SelfTest {
    pub fn new() -> Self {
        SelfTest {
            phase: Phase::Platform,
            report: Report::new(),
            fs: ShimFs,
            net: ShimNet,
            exit: false,
            started: false,
            driver: None,
            deadline: None,
            bearer_deadline: None,
            work_in: Vec::new(),
            work_out: vec![0u8; 256].into_boxed_slice(),
            work_started_us: 0,
            work_ticks: 0,
            sweep_at: 0,
            sweep_handle: -1,
            bearer_handle: -1,
            bearer_iap: -1,
            bearer_none: false,
            attempt_started_us: 0,
            sweep: SWEEP_HEAD.to_vec(),
            iap_names: Vec::new(),
            dns_handle: -1,
            tcp_handle: -1,
            resolved: None,
            rx_total: 0,
            rx_buf: vec![0u8; 512].into_boxed_slice(),
            tx: Vec::new(),
            rx_seen: Vec::new(),
            phase_started_us: 0,
            status: String::from("press Select to run everything"),
        }
    }

    fn begin(&mut self) {
        if self.started {
            return;
        }
        self.started = true;
        self.report.open_output(&mut self.fs);
        self.report.line("Symbian Rust SDK - device self test");
        self.report.line("");
        let label = self.report.path_label.clone();
        self.report.info("report", &label);

        let mut h = 0i32;
        if unsafe { sys::shim_timer_every(60, &mut h) } == sys::SHIM_OK {
            self.driver = Some(h);
        }
        self.status = String::from("running...");
    }

    fn expect(&mut self, secs: i32) {
        self.cancel_deadline();
        let mut h = 0i32;
        if unsafe { sys::shim_timer_after(secs * 1000, &mut h) } == sys::SHIM_OK {
            self.deadline = Some(h);
        }
    }

    /// A deadline for the bearer, separate from whatever phase is running.
    fn expect_bearer(&mut self, secs: i32) {
        self.cancel_bearer_deadline();
        let mut h = 0i32;
        if unsafe { sys::shim_timer_after(secs * 1000, &mut h) } == sys::SHIM_OK {
            self.bearer_deadline = Some(h);
        }
    }

    fn cancel_bearer_deadline(&mut self) {
        if let Some(h) = self.bearer_deadline.take() {
            unsafe { sys::shim_timer_cancel(h) };
        }
    }

    fn cancel_deadline(&mut self) {
        if let Some(h) = self.deadline.take() {
            unsafe { sys::shim_timer_cancel(h) };
        }
    }

    /// Advance one step. Synchronous phases finish here; asynchronous ones set a deadline
    /// and return, and their completion moves things on.
    fn step(&mut self) {
        match self.phase {
            Phase::Platform => {
                // The connection is asked for first, and then everything else runs while it
                // negotiates.
                //
                // It used to be the last thing the test did, which meant the access-point
                // dialog appeared ten seconds after launch -- by which time nobody is
                // looking at the phone, and the whole run was those ten seconds plus however
                // long a person took to notice. None of that was necessary:
                // RConnection::Start completes through an event, so it overlaps with the
                // arithmetic for free.
                self.start_bearer();
                self.do_platform();
                self.report.flush(&mut self.fs);
                self.next(Phase::Libraries);
            }
            Phase::Libraries => {
                self.do_libraries();
                self.report.flush(&mut self.fs);
                self.next(Phase::Storage);
            }
            Phase::Storage => {
                self.do_storage();
                self.report.flush(&mut self.fs);
                self.next(Phase::Hashes);
            }
            Phase::Hashes => {
                self.do_hashes();
                self.report.flush(&mut self.fs);
                self.next(Phase::Ciphers);
            }
            Phase::Ciphers => {
                self.do_ciphers();
                self.report.flush(&mut self.fs);
                self.next(Phase::Random);
            }
            Phase::Random => {
                self.do_random();
                self.report.flush(&mut self.fs);
                self.next(Phase::Bignum);
            }
            Phase::Bignum => {
                self.do_bignum();
                self.report.flush(&mut self.fs);
                self.next(Phase::Inflate);
            }
            Phase::Inflate => {
                self.do_inflate();
                self.report.flush(&mut self.fs);
                self.next(Phase::Timings);
            }
            Phase::Timings => {
                self.do_timings();
                self.report.flush(&mut self.fs);
                self.next(Phase::Graphics);
            }
            Phase::Graphics => {
                self.do_graphics();
                self.report.flush(&mut self.fs);
                self.next(Phase::WorkerStart);
            }
            Phase::WorkerStart => {
                self.do_worker_start();
            }
            Phase::BearerSweep => {
                // The sweep runs on its own from the first tick; this only waits for it.
                // Driving it from here is what made the dialog the last thing to happen.
                match self.bearer_ready() {
                    Some(_) => self.next(Phase::Dns),
                    None => self.status = String::from("waiting for a connection"),
                }
            }
            Phase::Dns => {
                self.do_dns();
            }
            Phase::Tcp => {
                self.do_tcp();
            }
            Phase::Http => {
                self.do_http();
            }
            // The *Wait phases do nothing on a tick; their events drive them, and the
            // deadline rescues them if none arrives.
            _ => {}
        }
    }

    fn next(&mut self, p: Phase) {
        self.phase = p;
        self.status = String::from(phase_name(p));
        self.phase_started_us = now_us();
        if p == Phase::Done {
            self.finish();
            return;
        }
        // A breadcrumb *before* the phase runs, flushed immediately.
        //
        // Writing only after a phase completes means a phase that kills the process
        // leaves no trace of itself — the file ends at the last thing that worked, and
        // the one that did not is invisible. On a platform whose failure mode is the app
        // silently closing, naming the phase you are about to enter is most of the
        // diagnosis.
        self.report.text.push_str("\n-- entering ");
        self.report.text.push_str(phase_name(p));
        self.report.text.push('\n');
        self.report.flush(&mut self.fs);
    }

    fn finish(&mut self) {
        if let Some(h) = self.driver.take() {
            unsafe { sys::shim_timer_cancel(h) };
        }
        self.cancel_deadline();
        let dropped = unsafe { sys::shim_events_dropped() };
        self.report.head("summary");
        self.report.num("events dropped", dropped as i64);
        self.report
            .check("no events dropped (a non-zero count means rust_step fell behind)", dropped == 0);
        let (p, f) = (self.report.pass, self.report.fail);
        self.report.num("passed", p as i64);
        self.report.num("failed", f as i64);
        self.report.line("");
        self.report.line("end of report");
        self.report.flush(&mut self.fs);

        let mut s = String::new();
        push_i64(&mut s, p as i64);
        s.push_str(" ok, ");
        push_i64(&mut s, f as i64);
        s.push_str(" failed");
        self.status = s;
    }

    // ---- synchronous phases ----

    fn do_platform(&mut self) {
        self.report.head("platform");

        let (mut w, mut h) = (0i32, 0i32);
        let rc = unsafe { sys::shim_screen_size(&mut w, &mut h) };
        self.report.check("screen size readable", rc == sys::SHIM_OK);
        self.report.num("width", w as i64);
        self.report.num("height", h as i64);

        let mut fmt = 0i32;
        let rc = unsafe { sys::shim_screen_format(&mut fmt) };
        self.report.check("screen format readable", rc == sys::SHIM_OK);
        // The raw TDisplayMode, not a shim enum: 7 is EColor64K (RGB565) and 11 is
        // EColor16MU (32bpp 0x00RRGGBB), which is what this handset reports.
        self.report.num("display mode (7=EColor64K 11=EColor16MU)", fmt as i64);

        let mut word = 0u32;
        if unsafe { sys::shim_probe_pixel_layout(&mut word) } == sys::SHIM_OK {
            // Pure red through the documented TRgb API, read back raw. Turns "which byte
            // is red" from a guess into a fact on whatever device this is.
            self.report.info("pure red as stored", &hex(&word.to_be_bytes()));
        }

        let unix = unsafe { sys::shim_unix_time() };
        self.report.num("unix time", unix);
        self.report
            .check("clock is set to something plausible (after 2020)", unix > 1_577_836_800);

        // Monotonic clock resolution and monotonicity, which every timing below rests on.
        let a = now_us();
        let mut moved = false;
        for _ in 0..200_000u32 {
            if now_us() > a {
                moved = true;
                break;
            }
        }
        let b = now_us();
        self.report.check("monotonic clock advances", moved && b >= a);
        self.report.num("clock delta over the probe loop (us)", (b - a) as i64);

        // How much the heap will give us, by doubling until it refuses. Worth knowing
        // before anything tries to buffer a message.
        let mut biggest = 0usize;
        let mut size = 4096usize;
        while size <= 8 * 1024 * 1024 {
            let p = unsafe { sys::shim_alloc(size as u32) };
            if p.is_null() {
                break;
            }
            unsafe { sys::shim_free(p) };
            biggest = size;
            size *= 2;
        }
        self.report.num("largest single allocation (bytes)", biggest as i64);
        self.report.check("at least 1 MB allocatable in one block", biggest >= 1024 * 1024);
    }

    fn do_libraries(&mut self) {
        self.report.head("optional libraries (Open C)");
        // Whether a handset has these is a property of the phone, not the SDK. libcrypto
        // would supply AES, RSA and bignum; libz an inflate; libc BSD sockets. The SDK
        // implements all three itself precisely because this answer is not knowable at
        // build time — this records what is actually here.
        let libs = [
            ("libc.dll", "BSD sockets, stdio"),
            ("libcrypto.dll", "AES, RSA, bignum"),
            ("libssl.dll", "TLS"),
            ("libz.dll", "inflate"),
            ("libm.dll", "libm"),
            ("libpthread.dll", "threads"),
            ("euser.dll", "control: must be present"),
            ("avkon.dll", "control: must be present"),
            ("esock.dll", "sockets"),
            ("insock.dll", "IPv4"),
            /* Asked because the shim deliberately does not link it. CSystemRandom in here
             * is the platform's real CSPRNG and would be a better entropy source than
             * Math::Random -- but a new DLL dependency can stop the image loading, and that
             * failure produces no report at all. This turns "can we upgrade the RNG" from a
             * guess into a value. */
            ("random.dll", "CSystemRandom, a real CSPRNG"),
            /* Asked because the keyboard's Fn layer needs it and this binary deliberately
             * does NOT link it. MCoeFepAwareTextEditor's own virtuals are IMPORT_C and live
             * here, not in cone -- so a class deriving from it imports fepbase, and a
             * handset without fepbase would not load the image at all. Probing from a build
             * that does not import it is the only way to get an answer instead of silence. */
            ("fepbase.dll", "the front-end processor base, for the Fn layer"),
            ("cryptography.dll", "platform crypto"),
        ];
        for (name, what) in libs {
            let mut buf = [0u16; 32];
            let mut n = 0;
            for b in name.bytes() {
                buf[n] = b as u16;
                n += 1;
            }
            let rc = unsafe { sys::shim_dll_present(buf.as_ptr(), n as i32) };
            let mut note = String::from(what);
            note.push_str(" [");
            push_i64(&mut note, rc as i64);
            note.push(']');
            // Only the controls are failures; the rest are findings.
            if name == "euser.dll" || name == "avkon.dll" {
                self.report.check_note(name, rc == 0, &note);
            } else {
                self.report.info(name, &note);
            }
        }
    }

    fn do_storage(&mut self) {
        self.report.head("storage");

        let Ok(dir) = fs::private_path(&mut self.fs) else {
            self.report.check("private path", false);
            return;
        };
        self.report.check("private path", true);

        let Ok(path) = Utf16Path::join(dir.as_units(), "t.bin") else {
            self.report.check("path join", false);
            return;
        };

        // Round trip.
        let payload: Vec<u8> = (0..1000u32).map(|i| (i * 7 % 251) as u8).collect();
        let wrote = fs::write_atomic(&mut self.fs, &path, &payload).is_ok();
        self.report.check("write_atomic 1000 bytes", wrote);
        let back = fs::read(&mut self.fs, &path).ok().flatten();
        self.report
            .check("read back identical", back.as_deref() == Some(&payload[..]));

        // A large file, which exercises the chunked-read loop rather than one call.
        let big: Vec<u8> = (0..64 * 1024u32).map(|i| (i % 256) as u8).collect();
        let t = now_us();
        let ok = fs::write_atomic(&mut self.fs, &path, &big).is_ok();
        let write_us = now_us() - t;
        self.report.check("write 64 KB", ok);
        let t = now_us();
        let back = fs::read(&mut self.fs, &path).ok().flatten();
        let read_us = now_us() - t;
        self.report.check("read 64 KB identical", back.as_deref() == Some(&big[..]));
        self.report.num("64 KB write (us)", write_us as i64);
        self.report.num("64 KB read (us)", read_us as i64);

        // Atomic replace really replaces, including shrinking.
        let _ = fs::write_atomic(&mut self.fs, &path, b"a much longer original value");
        let _ = fs::write_atomic(&mut self.fs, &path, b"short");
        let back = fs::read(&mut self.fs, &path).ok().flatten();
        self.report
            .check("replace truncates rather than leaving a tail", back.as_deref() == Some(&b"short"[..]));

        // And leaves no temp file behind.
        if let Ok(tmp) = path.with_suffix(".tmp") {
            let leftover = fs::read(&mut self.fs, &tmp).ok().flatten();
            self.report.check("no .tmp left behind", leftover.is_none());
        }

        // Append.
        if let Ok(mut f) = fs::File::open(&mut self.fs, &path, OpenMode::Append) {
            let _ = f.write_all(b"MORE");
        }
        let back = fs::read(&mut self.fs, &path).ok().flatten();
        self.report
            .check("append adds rather than truncating", back.as_deref() == Some(&b"shortMORE"[..]));

        // A missing file reads as absent, not as an error.
        if let Ok(missing) = Utf16Path::join(dir.as_units(), "nope.bin") {
            let r = fs::read(&mut self.fs, &missing);
            self.report.check("missing file reads as None", matches!(r, Ok(None)));
        }

        // Handle exhaustion: the shim has eight slots and the ninth must be refused
        // cleanly rather than corrupt anything.
        //
        // Through the raw ABI rather than symbian::fs, and that is a finding in itself:
        // `File<'a, F>` holds `&'a mut F`, so the borrow checker permits exactly one open
        // file at a time. For a zero-sized ShimFs that restriction buys nothing and costs
        // the ability to have two files open — worth fixing, and noted here rather than
        // quietly worked around.
        {
            let mut handles = [0i32; 12];
            let mut got = 0usize;
            for i in 0..handles.len() {
                let mut name = String::from("h");
                push_i64(&mut name, i as i64);
                let Ok(p) = Utf16Path::join(dir.as_units(), &name) else { break };
                let mut h = 0i32;
                let rc = unsafe {
                    sys::shim_file_open(
                        p.as_units().as_ptr(),
                        p.len() as i32,
                        sys::SHIM_FILE_WRITE | sys::SHIM_FILE_CREATE,
                        &mut h,
                    )
                };
                if rc != sys::SHIM_OK {
                    break;
                }
                handles[got] = h;
                got += 1;
            }
            self.report.num("file handles opened before refusal", got as i64);
            self.report.check("at least 8 handles available", got >= 8);
            for h in &handles[..got] {
                unsafe { sys::shim_file_close(*h) };
            }
            let again = fs::read(&mut self.fs, &path).is_ok();
            self.report.check("handles released on close", again);
            self.report.info(
                "API note",
                "symbian::fs::File borrows &mut Fs, so only one file can be open at a time",
            );
        }

        // A non-ASCII filename, since paths are UTF-16 and the conversion is easy to get
        // wrong by casting bytes.
        if let Ok(p) = Utf16Path::join(dir.as_units(), "acentuação.txt") {
            let ok = fs::write_atomic(&mut self.fs, &p, b"ok").is_ok();
            let back = fs::read(&mut self.fs, &p).ok().flatten();
            self.report
                .check("non-ASCII filename round trips", ok && back.as_deref() == Some(&b"ok"[..]));
        }
    }

    fn do_hashes(&mut self) {
        self.report.head("hashes (same vectors the host tests use)");

        let s = crypto::sha256::sha256(b"abc");
        self.report.check(
            "SHA-256 abc",
            hex(&s) == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        );
        let s = crypto::sha256::sha256(b"");
        self.report.check(
            "SHA-256 empty",
            hex(&s) == "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        );
        let s = crypto::sha1::sha1(b"abc");
        self.report
            .check("SHA-1 abc", hex(&s) == "a9993e364706816aba3e25717850c26c9cd0d89d");
        let s = crypto::sha512::sha512(b"abc");
        self.report.check(
            "SHA-512 abc",
            hex(&s)
                == "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
                    2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect::<String>(),
        );

        // The long-message vector, which is the one that catches a bit-counter carry bug
        // and only shows up past 2^32 bits... but also exercises the block loop hard.
        let mut h = crypto::sha256::Sha256::new();
        for _ in 0..1000 {
            h.update(&[b'a'; 1000]);
        }
        self.report.check(
            "SHA-256 one million a",
            hex(&h.finish()) == "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0",
        );

        let t = crypto::hmac::hmac_sha256(&[0x0b; 20], b"Hi There");
        self.report.check(
            "HMAC-SHA-256 RFC 4231 case 1",
            hex(&t) == "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
        );
        let t = crypto::hmac::hmac_sha512(b"Jefe", b"what do ya want for nothing?");
        self.report.check(
            "HMAC-SHA-512 RFC 4231 case 2",
            hex(&t)
                == "164b7a7bfcf819e2e395fbe73b56e0a387bd64222e831fd610270cd7ea250554\
                    9758bf75c05a994a6d034f65f8f0e6fdcaeab1a34d4a6b4b636e070a38bce737"
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect::<String>(),
        );

        self.report.check("ct_eq agrees on equal", crypto::ct_eq(b"abc", b"abc"));
        self.report.check("ct_eq rejects unequal", !crypto::ct_eq(b"abc", b"abd"));
    }

    fn do_ciphers(&mut self) {
        self.report.head("ciphers");

        // FIPS 197 appendix C, all three key lengths.
        let pt = unhex("00112233445566778899aabbccddeeff");
        for (key, want) in [
            ("000102030405060708090a0b0c0d0e0f", "69c4e0d86a7b0430d8cdb78070b4c55a"),
            (
                "000102030405060708090a0b0c0d0e0f1011121314151617",
                "dda97ca4864cdfe06eaf70a0ec0d7191",
            ),
            (
                "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
                "8ea2b7ca516745bfeafc49904b496089",
            ),
        ] {
            let aes = crypto::Aes::new(&unhex(key));
            let ok = match &aes {
                Some(a) => {
                    let mut b = [0u8; 16];
                    b.copy_from_slice(&pt);
                    a.encrypt_block(&mut b);
                    let enc_ok = hex(&b) == want;
                    a.decrypt_block(&mut b);
                    enc_ok && b[..] == pt[..]
                }
                None => false,
            };
            let mut name = String::from("AES-");
            push_i64(&mut name, (key.len() * 4) as i64);
            name.push_str(" FIPS 197 encrypt and invert");
            self.report.check(&name, ok);
        }

        // AES-IGE, the mode no OpenSSL still has. Vector generated from an independent
        // AES on the host.
        let aes =
            crypto::Aes::new(&unhex("603deb1015ca71be2b73aef0857d77811f352c073b6108d72d9810a30914dff4"));
        if let Some(aes) = aes {
            let mut iv = unhex("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f");
            let mut data = unhex("6bc1bee22e409f96e93d7e117393172a");
            let ok = crypto::ige::encrypt(&aes, &mut iv, &mut data).is_ok()
                && hex(&data) == "e59d5e17c2f0e7ad6f87b1e04366e5c9";
            self.report.check("AES-IGE one block", ok);

            let mut iv = unhex("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f");
            let round = crypto::ige::decrypt(&aes, &mut iv, &mut data).is_ok()
                && hex(&data) == "6bc1bee22e409f96e93d7e117393172a";
            self.report.check("AES-IGE inverts", round);
        } else {
            self.report.check("AES-IGE", false);
        }
    }

    /// Does the entropy source actually vary, and does the DRBG turn it into bytes?
    ///
    /// The host cannot answer the first question: `shim_entropy` is a counter there. This
    /// is the only place the real pool is ever seen, and a pool that returns the same bytes
    /// every call would produce a DRBG that is deterministic across launches -- which for
    /// a Diffie-Hellman secret means every session on every handset shares a key. That
    /// failure is silent, total, and invisible to every test that can run without a phone.
    fn do_random(&mut self) {
        self.report.head("randomness (the only place the real pool is visible)");

        let mut a = [0u8; 64];
        let mut b = [0u8; 64];
        let ra = unsafe { sys::shim_entropy(a.as_mut_ptr(), 64) };
        // Deliberately back to back, with nothing between: the pool leans on jitter, and
        // two calls a microsecond apart is the hardest case for it, not the easiest.
        let rb = unsafe { sys::shim_entropy(b.as_mut_ptr(), 64) };
        self.report.check("entropy call succeeds", ra == sys::SHIM_OK && rb == sys::SHIM_OK);
        self.report.check("two back-to-back pools differ", a != b);
        self.report.check("the pool is not all zero", a != [0u8; 64]);

        // How many bytes actually differ between the two pools. A pool driven only by a
        // millisecond clock would move in one or two bytes; the number is reported rather
        // than asserted on because what counts as enough is a judgement, and a judgement
        // belongs to whoever reads the report.
        let differing = a.iter().zip(b.iter()).filter(|(x, y)| x != y).count();
        self.report.num("bytes differing between two pools (of 64)", differing as i64);

        let mut rng = match symbian::random::Random::new() {
            Ok(r) => {
                self.report.check("Random::new", true);
                r
            }
            Err(e) => {
                self.report.check_note("Random::new", false, err_name(e));
                return;
            }
        };

        let mut buf = [0u8; 1024];
        rng.fill(&mut buf);
        let ones: u32 = buf.iter().map(|x| x.count_ones()).sum();
        self.report.num("set bits in 1024 DRBG bytes (expect near 4096)", ones as i64);
        self.report.check("bit balance is plausible", ones > 3800 && ones < 4400);

        let mut seen = [false; 256];
        for &x in buf.iter() {
            seen[x as usize] = true;
        }
        let missing = seen.iter().filter(|s| !**s).count();
        self.report.num("byte values never seen in 1024 (expect a handful)", missing as i64);

        let t = now_us();
        let mut big = [0u8; 8192];
        rng.fill(&mut big);
        self.report.num("DRBG 8 KB (us)", (now_us() - t) as i64);
    }

    fn do_bignum(&mut self) {
        self.report.head("bignum");
        // Small cases first: if these fail the 2048-bit timing below means nothing.
        for (b, e, m, want) in [(2u32, 10u32, 1001u32, 23u32), (5, 3, 13, 8), (7, 7, 11, 6)] {
            let ok = match crypto::bignum::Modulus::new(&m.to_be_bytes()) {
                Ok(md) => {
                    let mut out = [0u8; 4];
                    crypto::bignum::modpow(&b.to_be_bytes(), &e.to_be_bytes(), &md, &mut out).is_ok()
                        && u32::from_be_bytes(out) == want
                }
                Err(_) => false,
            };
            let mut name = String::new();
            push_i64(&mut name, b as i64);
            name.push('^');
            push_i64(&mut name, e as i64);
            name.push_str(" mod ");
            push_i64(&mut name, m as i64);
            self.report.check(&name, ok);
        }
        self.report.check(
            "an even modulus is refused",
            crypto::bignum::Modulus::new(&1000u32.to_be_bytes()).is_err(),
        );
    }

    fn do_inflate(&mut self) {
        self.report.head("inflate");
        // "hello" compressed by real zlib.
        let blob = unhex("789ccb48cdc9c90700062c0215");
        let out = crypto::inflate::inflate_any(&blob, 1024);
        self.report
            .check("zlib stream decompresses", out.as_deref() == Ok(&b"hello"[..]));
        self.report.check(
            "max_out is enforced",
            crypto::inflate::inflate_any(&blob, 4).is_err(),
        );
    }

    fn do_timings(&mut self) {
        self.report.head("timings (the numbers the docs have been estimating)");

        // SHA-256 over 64 KB.
        let data = vec![0xA5u8; 64 * 1024];
        let t = now_us();
        let _ = crypto::sha256::sha256(&data);
        let us = now_us() - t;
        self.report.num("SHA-256 over 64 KB (us)", us as i64);
        // checked_div rather than a `us > 0` guard: the clock can report zero elapsed on a fast
        // path, and there is no rate to print when it does.
        if let Some(rate) = (64u64 * 1_000_000).checked_div(us) {
            self.report.num("SHA-256 KB/s", rate as i64);
        }

        // AES-256 over 64 KB, block at a time, which is how IGE drives it.
        if let Some(aes) = crypto::Aes::new(&[0x42u8; 32]) {
            let mut block = [0u8; 16];
            let t = now_us();
            for _ in 0..(64 * 1024 / 16) {
                aes.encrypt_block(&mut block);
            }
            let us = now_us() - t;
            self.report.num("AES-256 over 64 KB (us)", us as i64);
            if let Some(rate) = (64u64 * 1_000_000).checked_div(us) {
                self.report.num("AES-256 KB/s", rate as i64);
            }
        }

        // The number that matters most: a real 2048-bit exponentiation, on the GUI thread
        // so the measurement is of the arithmetic and not of thread scheduling. The docs
        // say 0.4-0.6 s, scaled from a host measurement of 37 ms. This is the truth.
        let mut n = vec![0u8; 256];
        let mut s = 0xC0DEu32 | 1;
        for b in n.iter_mut() {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            *b = s as u8;
        }
        n[0] |= 0x80;
        n[255] |= 1;
        if let Ok(m) = crypto::bignum::Modulus::new(&n) {
            let exp = vec![0xA5u8; 256];
            let mut out = vec![0u8; 256];
            let t = now_us();
            let ok = crypto::bignum::modpow(&[3], &exp, &m, &mut out).is_ok();
            let us = now_us() - t;
            self.report.check("2048-bit modpow completes", ok);
            self.report.num("2048-bit modpow (ms)", (us / 1000) as i64);
            self.report.info(
                "docs estimate was 400-600 ms, scaled from 37 ms on the host",
                "compare above",
            );
        }

        // SHA-512 and the two-factor key derivation.
        //
        // The host says SHA-512 is 1.7x *faster* than SHA-256, because it does 128-byte
        // blocks in 64-bit words and the host has 64-bit registers. This core does not, so
        // the comparison should invert here -- and that inversion is why the PBKDF2 estimate
        // was scaled by the wrong hash and came out at 2.5 s for something guessed at
        // twelve. These lines settle it.
        let t = now_us();
        let mut h = crypto::Sha512::new();
        h.update(&data);
        let _ = h.finish();
        let us = (now_us() - t).max(1);
        self.report.num("SHA-512 over 64 KB (us)", us as i64);
        self.report.num("SHA-512 KB/s", (64 * 1_000_000 / us) as i64);

        // A thousand iterations, extrapolated to Telegram's hundred thousand. Measuring the
        // real count would be most of a minute inside one tick, which is the freeze the
        // worker thread exists to avoid.
        let mut dk = [0u8; 64];
        let t = now_us();
        crypto::pbkdf2_hmac_sha512(b"password", b"salt", 1000, &mut dk);
        let us = (now_us() - t).max(1);
        self.report.num("PBKDF2-SHA512 1000 iterations (us)", us as i64);
        self.report.num("=> 100000 iterations, projected (ms)", (us / 10) as i64);
        self.report.check("a 2FA password check would take under 30 s", us / 10 < 30_000);
    }

    fn do_graphics(&mut self) {
        self.report.head("graphics (the frame budget)");
        // Times the framebuffer path the same way the app does it, so the number is the
        // real cost of a frame rather than of a benchmark.
        let mut fb = sys::ShimFb::default();
        if unsafe { sys::shim_fb_lock(&mut fb) } != sys::SHIM_OK || fb.pixels.is_null() {
            self.report.check("framebuffer lockable", false);
            return;
        }
        self.report.check("framebuffer lockable", true);
        self.report.num("stride (bytes)", fb.stride as i64);
        self.report.num("format (1=RGB565)", fb.format as i64);

        let stride_px = (fb.stride / 2) as usize;
        let len = stride_px * fb.height as usize;
        // SAFETY: the shim guarantees this is valid until unlock.
        let px: &mut [u16] = unsafe { core::slice::from_raw_parts_mut(fb.pixels as *mut u16, len) };

        let t = now_us();
        for _ in 0..10 {
            px.fill(0x1234);
        }
        let fill_us = (now_us() - t) / 10;

        unsafe { sys::shim_fb_unlock() };

        let t = now_us();
        for _ in 0..10 {
            unsafe { sys::shim_present(0, 0, fb.width, fb.height) };
        }
        let present_us = (now_us() - t) / 10;

        self.report.num("full-screen fill (us)", fill_us as i64);
        self.report.num("present incl. expansion and BitBlt (us)", present_us as i64);
        let frame = fill_us + present_us;
        self.report.num("fill + present (us)", frame as i64);
        if let Some(fps) = 1_000_000u64.checked_div(frame) {
            self.report.num("implied max fps", fps as i64);
        }
        self.report
            .check("a full repaint fits in 100 ms", frame < 100_000);
    }

    fn do_worker_start(&mut self) {
        self.report.head("worker thread");
        let mut n = vec![0u8; 256];
        let mut s = 0xBEEFu32 | 1;
        for b in n.iter_mut() {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            *b = s as u8;
        }
        n[0] |= 0x80;
        n[255] |= 1;
        self.work_in.clear();
        for f in [&n[..], &[3u8][..], &[0xA5u8; 256][..]] {
            self.work_in.extend_from_slice(&(f.len() as u16).to_be_bytes());
            self.work_in.extend_from_slice(f);
        }
        self.work_ticks = 0;
        self.work_started_us = now_us();
        let rc = unsafe {
            sys::shim_work_submit(
                OP_MODPOW,
                self.work_in.as_ptr(),
                self.work_in.len() as i32,
                self.work_out.as_mut_ptr(),
                self.work_out.len() as i32,
            )
        };
        self.report.check_note("job submitted", rc == sys::SHIM_OK, {
            let mut s = String::from("rc=");
            push_i64(&mut s, rc as i64);
            &s.clone()
        });
        if rc == sys::SHIM_OK {
            self.phase = Phase::WorkerWait;
            self.expect(30);
        } else {
            self.next(Phase::BearerSweep);
        }
    }

    // ---- network phases ----

    /// How long the attempt in flight has taken, as a report note. An id that does not
    /// exist answers in milliseconds; one that is genuinely negotiating takes seconds, and
    /// telling those apart is the whole difference between a bad guess and a short
    /// deadline.
    fn attempt_note(&self, prefix: &str) -> String {
        let mut n = String::from(prefix);
        n.push_str("  after ");
        push_i64(&mut n, ((now_us() - self.attempt_started_us) / 1000) as i64);
        n.push_str(" ms");
        n
    }

    /// What to call the strategy at `sweep_at` in the report. The id-less ones have fixed
    /// names; a real access point reports the name and service type the database gave it,
    /// because "IAP 6 worked" is not an answer anyone can act on.
    fn sweep_name(&self) -> String {
        let iap = self.sweep[self.sweep_at];
        if iap == sys::SHIM_IAP_ATTACH {
            return String::from("attach to an existing connection");
        }
        if iap == sys::SHIM_IAP_DEFAULT || iap == sys::SHIM_IAP_PROMPT {
            return String::from(sweep_label(iap));
        }
        let head = SWEEP_HEAD.len();
        match self.iap_names.get(self.sweep_at - head) {
            Some(n) => n.clone(),
            None => String::from("access point"),
        }
    }

    /// Kick the bearer off, concurrently with everything else.
    fn start_bearer(&mut self) {
        self.report.head("bearer (running in the background from here)");

        // What is already up, asked before anything is attempted.
        //
        // This is the line that separates "nothing is online" from "we cannot join what
        // is". Both look identical from a socket that never connects, and three device runs
        // went into not being able to tell them apart.
        match symbian::net::connections_up() {
            Ok(n) => {
                self.report.num("connections already up", n as i64);
                // One-based by Symbian convention, which the headers do not state. Index 0
                // is tried too, and the report says which answered -- one run settles it
                // rather than a guess surviving in a comment.
                for idx in [1u32, 0] {
                    if let Ok(iap) = symbian::net::connection_iap(idx) {
                        let mut note = String::from("index ");
                        push_i64(&mut note, idx as i64);
                        note.push_str(" -> IAP ");
                        push_i64(&mut note, iap as i64);
                        self.report.info("existing connection", &note);
                        break;
                    }
                }
            }
            Err(e) => self.report.check_note("connections readable", false, err_name(e)),
        }
        self.report.flush(&mut self.fs);
        self.sweep_at = 0;
        self.do_sweep_step();
    }

    /// Whether the network phases can proceed, are still waiting, or have run out.
    ///
    /// `Some(true)` with no handle means the routeless fallback: no connection was
    /// negotiated, so the socket will be opened on whatever route happens to exist. That
    /// may be none, and then the phases below time out — which is a worse answer than a
    /// bearer but a better one than not trying.
    fn bearer_ready(&mut self) -> Option<bool> {
        if self.bearer_handle >= 0 {
            return Some(true);
        }
        if self.sweep_at >= self.sweep.len() {
            // Every strategy refused, including attaching to something already up. There is
            // no route, and the phases below will say so by timing out -- which is the
            // honest outcome now that nothing claims success on the way here.
            if !self.bearer_none {
                self.bearer_none = true;
                self.report.check("some bearer strategy worked", false);
                self.report.flush(&mut self.fs);
            }
            return Some(false);
        }
        None
    }

    fn do_sweep_step(&mut self) {
        if self.sweep_handle >= 0 {
            self.net.net_stop(self.sweep_handle);
            self.sweep_handle = -1;
        }
        if self.sweep_at >= self.sweep.len() {
            self.report.check("some bearer strategy worked", false);
            self.next(Phase::Done);
            return;
        }
        let iap = self.sweep[self.sweep_at];


        let strategy = match iap {
            sys::SHIM_IAP_ATTACH => Iap::Attach,
            sys::SHIM_IAP_PROMPT => Iap::Prompt,
            sys::SHIM_IAP_DEFAULT => Iap::Default,
            id => Iap::Id(id as u32),
        };
        /* Announced and flushed before the attempt, not after it.
         *
         * The sweep is up to ten strategies, one of which waits 40 s on a human, so a
         * run can spend two and a half minutes here writing nothing. The first report
         * off this handset ended at "-- entering bearer" and read like a freeze; it was
         * a sweep in progress. A phase that can be slow has to narrate itself, or the
         * only observable difference between working and hung is patience. */
        self.status = String::from("connecting - ANSWER ANY DIALOG on screen");
        let label = self.sweep_name();
        self.report.info("trying", &label);
        self.report.flush(&mut self.fs);
        self.attempt_started_us = now_us();

        match self.net.net_start(strategy) {
            Ok(h) => {
                self.sweep_handle = h;
                // The prompt waits on a person and gets longer, but not so long that an
                // unattended run stalls for minutes on a dialog nobody is there to answer.
                self.expect_bearer(BEARER_DEADLINE_S);
            }
            Err(e) => {
                self.report.info(&label, err_name(e));
                self.report.flush(&mut self.fs);
                self.sweep_at += 1;
            }
        }
    }

    fn do_dns(&mut self) {
        self.report.head("dns");
        match self.net.resolve(self.bearer_handle, HTTP_HOST) {
            Ok(h) => {
                self.dns_handle = h;
                self.phase = Phase::DnsWait;
                self.expect(20);
            }
            Err(e) => {
                self.report.check_note("resolve issued", false, err_name(e));
                self.next(Phase::Tcp);
            }
        }
    }

    fn do_tcp(&mut self) {
        self.report.head("tcp echo");
        let o = ECHO_ADDR.octets();
        let mut target = String::new();
        for (i, b) in o.iter().enumerate() {
            if i > 0 {
                target.push('.');
            }
            push_i64(&mut target, *b as i64);
        }
        target.push(':');
        push_i64(&mut target, ECHO_PORT as i64);
        self.report.info("target", &target);
        self.report.info("note", "needs the phone on the same LAN as the host; a failure here may be routing, not sockets");

        match self.net.tcp_open(self.bearer_handle) {
            Ok(h) => {
                self.tcp_handle = h;
                let r = self.net.tcp_connect(h, ECHO_ADDR, ECHO_PORT);
                self.report.check("connect issued", r.is_ok());
                self.report.flush(&mut self.fs);
                if r.is_ok() {
                    self.rx_seen.clear();
                    self.phase = Phase::TcpWait;
                    self.expect(20);
                } else {
                    self.close_tcp();
                    self.next(Phase::Http);
                }
            }
            Err(e) => {
                self.report.check_note("socket open", false, err_name(e));
                self.next(Phase::Http);
            }
        }
    }

    fn do_http(&mut self) {
        self.report.head("http get");
        let addr = match self.resolved {
            Some(a) => a,
            None => {
                self.report.info("dns failed, using a literal address", "104.20.23.154");
                HTTP_FALLBACK
            }
        };
        match self.net.tcp_open(self.bearer_handle) {
            Ok(h) => {
                self.tcp_handle = h;
                let r = self.net.tcp_connect(h, addr, 80);
                self.report.check("connect issued", r.is_ok());
                self.report.flush(&mut self.fs);
                if r.is_ok() {
                    self.rx_seen.clear();
                    self.phase = Phase::HttpWait;
                    self.expect(25);
                } else {
                    self.close_tcp();
                    self.next(Phase::Done);
                }
            }
            Err(e) => {
                self.report.check_note("socket open", false, err_name(e));
                self.next(Phase::Done);
            }
        }
    }

    fn close_tcp(&mut self) {
        if self.tcp_handle >= 0 {
            self.net.tcp_close(self.tcp_handle);
            self.tcp_handle = -1;
        }
    }

    /// Ask for more bytes into the scratch buffer.
    ///
    /// The buffer is a field rather than a local because the shim holds a pointer to it
    /// until the read completes — a local would be gone by then.
    fn issue_read(&mut self) {
        let h = self.tcp_handle;
        if h < 0 {
            return;
        }
        let _ = self.net.tcp_recv(h, &mut self.rx_buf);
    }

    /// Everything the network phases do with an event.
    fn on_net_event(&mut self, ev: &RawEvent) -> bool {
        // The bearer first, and outside the phase match: it runs concurrently with whatever
        // the test sequence is doing, so its completion can arrive during the graphics
        // phase or the worker thread's.
        if ev.kind == sys::SHIM_EV_NET_READY
            && self.sweep_handle >= 0
            && ev.handle == self.sweep_handle
        {
            self.cancel_bearer_deadline();
            if ev.status == 0 {
                let mut got = String::from("IAP ");
                push_i64(&mut got, ev.a as i64);
                let note = self.attempt_note(&got);
                let label = self.sweep_name();
                self.report.check_note(&label, true, &note);
                self.report.flush(&mut self.fs);
                self.bearer_handle = self.sweep_handle;
                self.bearer_iap = ev.a;
                // Kept: everything after this uses it, so it must not be closed by the
                // next sweep step.
                self.sweep_handle = -1;
                self.sweep_at = self.sweep.len();
            } else {
                let mut err = String::from("err ");
                push_i64(&mut err, ev.status as i64);
                let note = self.attempt_note(&err);
                let label = self.sweep_name();
                self.report.info(&label, &note);
                self.report.flush(&mut self.fs);
                self.sweep_at += 1;
                self.do_sweep_step();
            }
            return true;
        }

        match self.phase {
            Phase::DnsWait => {
                if ev.kind != sys::SHIM_EV_RESOLVED || ev.handle != self.dns_handle {
                    return false;
                }
                self.cancel_deadline();
                if ev.status == 0 && ev.a != 0 {
                    let a = Ipv4(ev.a as u32);
                    let o = a.octets();
                    let mut s = String::new();
                    for (i, b) in o.iter().enumerate() {
                        if i > 0 {
                            s.push('.');
                        }
                        push_i64(&mut s, *b as i64);
                    }
                    self.report.check_note("resolved example.com", true, &s);
                    self.resolved = Some(a);
                } else {
                    let mut note = String::from("status ");
                    push_i64(&mut note, ev.status as i64);
                    self.report.check_note("resolved example.com", false, &note);
                    if self.bearer_handle < 0 && self.sweep_at < self.sweep.len() {
                        // The routeless attempt found no route. That is an answer about
                        // this strategy, not about DNS, so go back and bring a bearer up
                        // rather than running TCP and HTTP over nothing.
                        self.report.info("no bearer failed", "falling back to the bearer sweep");
                        self.report.flush(&mut self.fs);
                        self.phase = Phase::BearerSweep;
                        return true;
                    }
                }
                self.next(Phase::Tcp);
                true
            }

            Phase::TcpWait | Phase::HttpWait => {
                if ev.handle != self.tcp_handle {
                    return false;
                }
                let http = self.phase == Phase::HttpWait;
                match ev.kind {
                    sys::SHIM_EV_CONNECTED => {
                        self.cancel_deadline();
                        if ev.status != 0 {
                            let mut note = String::from("err ");
                            push_i64(&mut note, ev.status as i64);
                            self.report.check_note("connected", false, &note);
                            self.close_tcp();
                            self.next(if http { Phase::Done } else { Phase::Http });
                            return true;
                        }
                        self.report.check("connected", true);
                        self.report.flush(&mut self.fs);
                        self.issue_read();
                        let payload: &[u8] = if http {
                            b"GET / HTTP/1.0\r\nHost: example.com\r\n\r\n"
                        } else {
                            b"hello from E72"
                        };
                        self.tx.clear();
                        self.tx.extend_from_slice(payload);
                        let h = self.tcp_handle;
                        let r = self.net.tcp_send(h, &self.tx);
                        self.report.check("send issued", r.is_ok());
                        self.expect(20);
                        true
                    }
                    sys::SHIM_EV_SENT => {
                        self.report.check_note("send completed", ev.status == 0, {
                            let mut s = String::from("status ");
                            push_i64(&mut s, ev.status as i64);
                            &s.clone()
                        });
                        true
                    }
                    sys::SHIM_EV_RECV => {
                        // KErrEof and KErrDisconnected are the peer closing, which for
                        // HTTP/1.0 is how the response ends.
                        if ev.status == -25 || ev.status == -36 || (ev.status == 0 && ev.a == 0) {
                            self.finish_socket_phase(http);
                            return true;
                        }
                        if ev.status != 0 {
                            let mut note = String::from("err ");
                            push_i64(&mut note, ev.status as i64);
                            self.report.check_note("recv", false, &note);
                            self.finish_socket_phase(http);
                            return true;
                        }
                        let n = (ev.a.max(0) as usize).min(self.rx_buf.len());
                        self.rx_total += n;
                        for &b in &self.rx_buf[..n] {
                            if self.rx_seen.len() < 512 {
                                self.rx_seen.push(b);
                            }
                        }
                        self.cancel_deadline();
                        self.expect(15);
                        self.issue_read();
                        true
                    }
                    sys::SHIM_EV_CLOSED => {
                        self.finish_socket_phase(http);
                        true
                    }
                    _ => false,
                }
            }

            _ => false,
        }
    }

    /// Report what came back and move on.
    fn finish_socket_phase(&mut self, http: bool) {
        self.cancel_deadline();
        self.close_tcp();

        let seen = core::mem::take(&mut self.rx_seen);
        self.report.num("bytes received", seen.len() as i64);
        // The first line, printable characters only. A server's reply is not ours to
        // trust the contents of.
        let mut first = String::new();
        for &b in seen.iter().take(60) {
            if b == b'\n' {
                break;
            }
            first.push(if (0x20..0x7F).contains(&b) { b as char } else { '.' });
        }
        self.report.info("first line", &first);

        if http {
            self.report
                .check("HTTP response begins with a status line", first.starts_with("HTTP/1."));
            self.next(Phase::Done);
        } else {
            // The echo server greets, then echoes. Both halves have to be there.
            let text = String::from_utf8_lossy(&seen);
            self.report
                .check("echo server greeted", text.contains("symbian-echo ready"));
            self.report
                .check("payload came back", text.contains("hello from E72"));
            self.next(Phase::Http);
        }
    }
}

fn sweep_label(iap: i32) -> &'static str {
    match iap {
        sys::SHIM_IAP_DEFAULT => "system default",
        sys::SHIM_IAP_PROMPT => "prompt",
        _ => "access point",
    }
}

fn err_name(e: symbian::Error) -> &'static str {
    use symbian::Error as E;
    match e {
        E::NotFound => "not found",
        E::PathNotFound => "path not found",
        E::AlreadyExists => "exists",
        E::NoMemory => "no memory",
        E::AccessDenied => "denied",
        E::InUse => "in use",
        E::Argument => "bad argument",
        E::Overflow => "overflow",
        E::NotReady => "not ready",
        E::UnexpectedEof => "eof",
        E::Platform(_) => "platform error",
    }
}

fn phase_name(p: Phase) -> &'static str {
    match p {
        Phase::Platform => "platform",
        Phase::Libraries => "libraries",
        Phase::Storage => "storage",
        Phase::Hashes => "hashes",
        Phase::Ciphers => "ciphers",
        Phase::Random => "randomness",
        Phase::Bignum => "bignum",
        Phase::Inflate => "inflate",
        Phase::Timings => "timings",
        Phase::Graphics => "graphics",
        Phase::WorkerStart | Phase::WorkerWait => "worker thread",
        Phase::BearerSweep => "bearer",
        Phase::Dns | Phase::DnsWait => "dns",
        Phase::Tcp | Phase::TcpWait => "tcp echo",
        Phase::Http | Phase::HttpWait => "http",
        Phase::Done => "done",
    }
}

impl App for SelfTest {
    fn title(&self) -> &str {
        "SDK self test"
    }

    fn handle_raw(&mut self, ev: &RawEvent) -> Handled {
        if ev.kind == sys::SHIM_EV_TIMER {
            // The driver tick advances the state machine one phase at a time, so the
            // screen updates between phases instead of freezing for the whole battery.
            if Some(ev.handle) == self.driver {
                // Count ticks served while a job is on the worker. If the computation
                // were on this thread the count would be zero, which is the measurement
                // rather than an impression of a moving spinner.
                if self.phase == Phase::WorkerWait {
                    self.work_ticks += 1;
                    return Handled::Consumed;
                }
                self.step();
                return Handled::Consumed;
            }
            // The bearer's own deadline, checked before the phase sequence's. It fires while
            // some unrelated phase is running, which is the point of it being separate.
            if Some(ev.handle) == self.bearer_deadline {
                self.cancel_bearer_deadline();
                let what = self.attempt_note(&self.sweep_name());
                self.report.check_note("timed out", false, &what);
                self.report.flush(&mut self.fs);
                self.sweep_at += 1;
                self.do_sweep_step();
                return Handled::Consumed;
            }
            if Some(ev.handle) == self.deadline {
                self.cancel_deadline();
                let what = String::from(phase_name(self.phase));
                self.report.check_note("timed out", false, &what);
                self.report.flush(&mut self.fs);
                // A timeout is an answer about this phase, not the end of the run.
                match self.phase {
                    Phase::WorkerWait => self.next(Phase::BearerSweep),

                    Phase::DnsWait => {
                        // Close the lookup nobody is going to answer. Leaving it open holds
                        // the connection it was made against, and the bearer sweep that
                        // follows then answers KErrLocked on a prompt that waited two
                        // minutes -- which is what the last device report showed.
                        if self.dns_handle >= 0 {
                            self.net.dns_close(self.dns_handle);
                            self.dns_handle = -1;
                        }
                        // A timeout and an error mean the same thing here and only one of
                        // them used to fall back. The routeless attempt succeeds trivially
                        // -- it just declines to open an RConnection -- so its failure shows
                        // up as DNS never completing, which took this path and skipped
                        // straight to TCP over a route that does not exist.
                        if self.bearer_handle < 0 && self.sweep_at < self.sweep.len() {
                            self.report.info("no route", "falling back to the bearer sweep");
                            self.report.flush(&mut self.fs);
                            self.phase = Phase::BearerSweep;
                        } else {
                            self.next(Phase::Tcp);
                        }
                    }
                    Phase::TcpWait => {
                        self.close_tcp();
                        self.next(Phase::Http);
                    }
                    Phase::HttpWait => {
                        self.close_tcp();
                        self.next(Phase::Done);
                    }
                    _ => {}
                }
                return Handled::Consumed;
            }
            return Handled::Ignored;
        }

        if ev.kind == sys::SHIM_EV_WORK_DONE && self.phase == Phase::WorkerWait {
            self.cancel_deadline();
            let us = now_us() - self.work_started_us;
            self.report.check_note("job completed", ev.status == 0, {
                let mut s = String::from("status ");
                push_i64(&mut s, ev.status as i64);
                &s.clone()
            });
            self.report.num("wall time (ms)", (us / 1000) as i64);
            self.report.num("GUI ticks served while it ran", self.work_ticks as i64);
            // The whole point of the thread, as a number: the GUI thread kept running.
            self.report.check(
                "the GUI thread kept running during the job (ticks > 0)",
                self.work_ticks > 0,
            );
            self.next(Phase::BearerSweep);
            return Handled::Consumed;
        }

        if self.on_net_event(ev) {
            return Handled::Consumed;
        }
        Handled::Ignored
    }

    fn handle_key(&mut self, ev: KeyEvent, _t: &Theme<'_>, _s: Rect) -> Handled {
        match ev.key {
            Key::Select | Key::Enter => self.begin(),
            Key::Softkey(Softkey::Right) | Key::End => self.exit = true,
            _ => return Handled::Ignored,
        }
        Handled::Consumed
    }

    fn should_exit(&self) -> bool {
        self.exit
    }

    fn draw(&mut self, c: &mut Canvas<'_>, theme: &Theme<'_>) {
        use symbian_ui::{Align, Point};

        let screen = Rect::from_size(c.size());
        let frame = chrome::Frame::split(screen, theme, true, true);
        chrome::clear(c, theme);
        chrome::title_bar(c, frame.title, theme, "SDK self test", None);
        chrome::softkey_bar(
            c,
            frame.softkeys,
            theme,
            [Some(if self.started { "" } else { "Run" }), None, Some("Exit")],
        );

        let body = theme.fonts.body;
        let small = theme.fonts.small;
        let mut y = frame.content.y0 + 4;

        c.draw_text(Point::new(6, y + body.ascent()), &self.status, body, theme.palette.accent);
        y += body.line_height() + 6;

        // Running totals, so a run that dies mid-way still says how far it got.
        let mut line = String::new();
        push_i64(&mut line, self.report.pass as i64);
        line.push_str(" ok   ");
        push_i64(&mut line, self.report.fail as i64);
        line.push_str(" failed");
        let color = if self.report.fail > 0 { theme.palette.unread } else { theme.palette.text };
        c.draw_text(Point::new(6, y + body.ascent()), &line, body, color);
        y += body.line_height() + 8;

        if self.started {
            c.draw_text(Point::new(6, y + small.ascent()), "writing to:", small, theme.palette.dim);
            y += small.line_height() + 1;
            c.draw_text(
                Point::new(6, y + small.ascent()),
                &self.report.path_label,
                small,
                theme.palette.text,
            );
        }

        let hint = Rect { y0: frame.content.y1 - 14, ..frame.content };
        let msg = if self.phase == Phase::Done {
            "done - copy the txt off the phone"
        } else if self.started {
            "running, leave it alone"
        } else {
            "Select runs every test"
        };
        c.draw_text_in(hint, msg, small, theme.palette.dim, Align::Center);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_report_formats_verdicts_greppably() {
        // The file is read by grepping for FAIL, so the prefix is fixed width and the
        // word appears nowhere else in a passing line.
        let mut r = Report::new();
        r.check("a thing", true);
        r.check("another", false);
        assert!(r.text.contains("  ok   a thing"));
        assert!(r.text.contains("  FAIL another"));
        assert_eq!((r.pass, r.fail), (1, 1));
    }

    #[test]
    fn integers_format_including_the_awkward_ones() {
        let mut s = String::new();
        push_i64(&mut s, 0);
        s.push(' ');
        push_i64(&mut s, -42);
        s.push(' ');
        push_i64(&mut s, i32::MAX as i64);
        assert_eq!(s, "0 -42 2147483647");
    }

    #[test]
    fn hex_and_unhex_round_trip() {
        let data = [0x00u8, 0x0f, 0xa5, 0xff];
        assert_eq!(hex(&data), "000fa5ff");
        assert_eq!(unhex("000fa5ff"), data);
    }

    #[test]
    fn the_worker_job_computes_a_modpow() {
        let mut input = Vec::new();
        for f in [&[11u8][..], &[5u8][..], &[3u8][..]] {
            input.extend_from_slice(&(f.len() as u16).to_be_bytes());
            input.extend_from_slice(f);
        }
        let mut out = [0u8; 1];
        assert_eq!(modpow_job(OP_MODPOW, &input, &mut out), 0);
        assert_eq!(out[0], 4); // 5^3 mod 11
    }

    #[test]
    fn every_phase_has_a_name() {
        // The name is what a timeout reports, so a phase without one would time out
        // saying nothing.
        //
        // The list is hand-maintained and therefore cannot enforce completeness — adding
        // Phase::Random did not make this fail. What actually guarantees coverage is that
        // `phase_name` matches without a `_` arm, so a new variant is a compile error until
        // it is named. This test is the weaker half of that pair and the count below is
        // what keeps it honest: it fails when the enum grows, which is the reminder to add
        // the variant here too.
        let all = [
            Phase::Platform, Phase::Libraries, Phase::Storage, Phase::Hashes,
            Phase::Ciphers, Phase::Random, Phase::Bignum, Phase::Inflate, Phase::Timings,
            Phase::Graphics, Phase::WorkerStart, Phase::WorkerWait, Phase::BearerSweep,
            Phase::Dns, Phase::DnsWait, Phase::Tcp, Phase::TcpWait,
            Phase::Http, Phase::HttpWait, Phase::Done,
        ];
        for p in all {
            assert!(!phase_name(p).is_empty(), "{p:?}");
        }
        assert_eq!(all.len(), Phase::Done as usize + 1, "a phase is missing from this list");
    }

    #[test]
    fn draws_before_and_after_starting() {
        use symbian_ui::testing;
        for started in [false, true] {
            let mut app = SelfTest::new();
            app.started = started;
            app.report.path_label = String::from("E:\\symbian-selftest.txt");
            let (_, px) = testing::with_canvas(symbian_gfx::Size::new(320, 240), |c| {
                testing::with_theme(symbian_ui::Palette::DARK, |t| app.draw(c, t));
            });
            assert!(px.iter().any(|&p| p != 0));
        }
    }
}
