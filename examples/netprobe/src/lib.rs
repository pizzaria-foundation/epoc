//! Four tests, each isolating one unknown, in the order that makes a failure mean
//! something.
//!
//! | | proves | adds |
//! |---|---|---|
//! | echo | `RConnection`, `RSocket`, connect, write, read, close | everything at once, on the LAN |
//! | dns | `RHostResolver` | name resolution |
//! | http | the whole path | the internet |
//! | work | the worker thread | a second thread |
//!
//! **Echo is the gate.** It talks to `tools/echo.py` on a hardcoded LAN address, so
//! there is no DNS and no internet in the way: if it fails, there is exactly one place
//! for the fault to be. Nothing after it is worth reading until it passes, because
//! everything downstream assumes a working socket.
//!
//! The `work` test is the only one whose result is not the point. A modular
//! exponentiation returns digits either way; what it proves is the **spinner**, animated
//! from `rust_step` by a repeating timer while the job runs. A spinner that keeps moving
//! means the computation is genuinely on another thread. A frozen one means it is not,
//! whatever digits come back — and that is the only way to tell the two apart from
//! outside.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

use symbian::net::{Bearer, Ipv4, Lookup, Progress, ShimNet, TcpStream};
use symbian_sys as sys;
use symbian_ui::{
    chrome, App, Canvas, Handled, Key, KeyEvent, RawEvent, Rect, Softkey, Theme,
};

/// Where `tools/echo.py` is listening. Change this and rebuild.
///
/// Hardcoded rather than typed in on the phone: an address entry screen is more code
/// than the test it serves, and getting it wrong is diagnosable because the probe prints
/// what it tried.
const ECHO_ADDR: Ipv4 = Ipv4::new(192, 168, 15, 74);
const ECHO_PORT: u16 = 7654;

/// Plain HTTP on purpose. There is no TLS here — `libssl` exists only on handsets with
/// Open C, and this test is about reaching the internet, not about cryptography.
const HTTP_HOST: &str = "example.com";
const HTTP_PORT: u16 = 80;

const DNS_HOST: &str = "example.com";

/// The worker's job table.
pub const OP_MODPOW: i32 = 1;

/// Runs on the worker thread, not the GUI thread.
///
/// `modpow` is the right first job for this facility rather than a synthetic one: it
/// takes 0.4-0.6 s on this hardware, which is exactly the case the thread exists for,
/// and it allocates nothing — fixed-size arrays over the caller's slices — so it
/// satisfies the "nothing the job allocates may outlive it" contract by construction.
///
/// Input is three length-prefixed byte strings: modulus, base, exponent. Crude, and
/// appropriate: a job interface crossing a thread boundary with no allocator in common
/// is not the place for a serialisation format.
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
    let Ok(m) = symbian_crypto::bignum::Modulus::new(fields[0]) else {
        return sys::SHIM_ERR_ARGUMENT;
    };
    match symbian_crypto::bignum::modpow(fields[1], fields[2], &m, out) {
        Ok(()) => 0,
        Err(_) => sys::SHIM_ERR_ARGUMENT,
    }
}

// ----------------------------------------------------------------------------- log --

const LINE_CAP: usize = 46;
const LOG_CAP: usize = 11;

/// A fixed ring of fixed lines.
///
/// No allocation and no `format!`. `core::fmt` on this target pulls in more code than
/// the rest of this app, and a log that allocates while reporting an out-of-memory
/// failure is a log that does not report it.
struct Log {
    lines: [[u8; LINE_CAP]; LOG_CAP],
    lens: [usize; LOG_CAP],
    len: usize,
}

impl Log {
    fn new() -> Self {
        Log { lines: [[0; LINE_CAP]; LOG_CAP], lens: [0; LOG_CAP], len: 0 }
    }

    fn clear(&mut self) {
        self.len = 0;
    }

    fn push(&mut self, f: impl FnOnce(&mut Writer<'_>)) {
        // Drop the oldest, not the newest: here the interesting line is the one that just
        // happened, which is the opposite of the shim's input queue.
        if self.len == LOG_CAP {
            self.lines.rotate_left(1);
            self.lens.rotate_left(1);
            self.len -= 1;
        }
        let mut w = Writer { buf: &mut self.lines[self.len], at: 0 };
        f(&mut w);
        self.lens[self.len] = w.at;
        self.len += 1;
    }

    fn line(&self, i: usize) -> &str {
        core::str::from_utf8(&self.lines[i][..self.lens[i]]).unwrap_or("?")
    }
}

/// Writes into a fixed buffer, silently stopping at the end.
///
/// Truncation rather than failure: a log line that is one character too long should lose
/// the character, not the line.
struct Writer<'a> {
    buf: &'a mut [u8; LINE_CAP],
    at: usize,
}

impl Writer<'_> {
    fn s(&mut self, text: &str) -> &mut Self {
        for &b in text.as_bytes() {
            if self.at < LINE_CAP {
                self.buf[self.at] = b;
                self.at += 1;
            }
        }
        self
    }

    fn n(&mut self, mut v: i64) -> &mut Self {
        if v < 0 {
            self.s("-");
            v = -v;
        }
        let mut digits = [0u8; 20];
        let mut k = 0;
        loop {
            digits[k] = b'0' + (v % 10) as u8;
            k += 1;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        for i in (0..k).rev() {
            if self.at < LINE_CAP {
                self.buf[self.at] = digits[i];
                self.at += 1;
            }
        }
        self
    }

    fn ip(&mut self, a: Ipv4) -> &mut Self {
        let o = a.octets();
        self.n(o[0] as i64).s(".").n(o[1] as i64).s(".").n(o[2] as i64).s(".").n(o[3] as i64)
    }

    /// The printable part of a byte string, non-printables as dots. Bounded, because a
    /// server's reply is not ours to trust the length of.
    fn bytes(&mut self, data: &[u8]) -> &mut Self {
        for &b in data.iter().take(24) {
            if self.at < LINE_CAP {
                self.buf[self.at] = if (0x20..0x7F).contains(&b) { b } else { b'.' };
                self.at += 1;
            }
        }
        self
    }
}

// ---------------------------------------------------------------------------- tests --

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Test {
    Echo,
    Dns,
    Http,
    Work,
}

impl Test {
    const ALL: [Test; 4] = [Test::Echo, Test::Dns, Test::Http, Test::Work];

    fn name(self) -> &'static str {
        match self {
            Test::Echo => "1 echo (LAN)",
            Test::Dns => "2 dns",
            Test::Http => "3 http get",
            Test::Work => "4 worker thread",
        }
    }

    fn next(self) -> Test {
        let i = Test::ALL.iter().position(|&t| t == self).unwrap_or(0);
        Test::ALL[(i + 1) % Test::ALL.len()]
    }

    fn prev(self) -> Test {
        let i = Test::ALL.iter().position(|&t| t == self).unwrap_or(0);
        Test::ALL[(i + Test::ALL.len() - 1) % Test::ALL.len()]
    }
}

pub struct NetProbe {
    test: Test,
    log: Log,
    net: ShimNet,

    bearer: Option<Bearer>,
    /// The test to run once the bearer comes up. A bearer takes a user prompt on the
    /// first run, so every test has to be able to wait for one.
    pending: Option<Test>,

    stream: Option<TcpStream>,
    lookup: Option<Lookup>,
    /// For the HTTP test: the address to connect to once DNS answers.
    http_pending: bool,

    /// Worker job buffers. Owned and kept alive for the whole job, which the ABI
    /// requires — the worker holds raw pointers into them.
    work_in: Vec<u8>,
    work_out: Box<[u8]>,
    work_running: bool,
    spin: u32,
    /// Handle of the repeating timer that animates the spinner.
    timer: Option<i32>,

    exit: bool,
}

impl Default for NetProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl NetProbe {
    pub fn new() -> Self {
        let mut p = NetProbe {
            test: Test::Echo,
            log: Log::new(),
            net: ShimNet,
            bearer: None,
            pending: None,
            stream: None,
            lookup: None,
            http_pending: false,
            work_in: Vec::new(),
            work_out: vec![0u8; 256].into_boxed_slice(),
            work_running: false,
            spin: 0,
            timer: None,
            exit: false,
        };
        p.log.push(|w| {
            w.s("Select runs, arrows switch test");
        });
        p
    }

    /// Bring the bearer up if it is not, and remember what to do afterwards.
    ///
    /// Returns true when the caller can proceed immediately.
    fn need_bearer(&mut self, then: Test) -> bool {
        if let Some(b) = &self.bearer {
            if b.is_up() {
                return true;
            }
            self.log.push(|w| {
                w.s("bearer still coming up");
            });
            return false;
        }
        // No saved IAP: this is a probe, and the first run prompting is part of what it
        // is testing. A real app persists the id with symbian::fs and passes it here.
        match Bearer::start(&mut self.net, None) {
            Ok(b) => {
                self.bearer = Some(b);
                self.pending = Some(then);
                self.log.push(|w| {
                    w.s("bearer: asking for access point");
                });
            }
            Err(e) => self.log.push(|w| {
                w.s("bearer failed: ").s(err_name(e));
            }),
        }
        false
    }

    fn run(&mut self, test: Test) {
        self.log.clear();
        self.log.push(|w| {
            w.s(test.name());
        });
        // Drop anything the previous test left open, or its completions arrive against a
        // socket this run knows nothing about.
        self.stream = None;
        self.lookup = None;
        self.http_pending = false;

        match test {
            Test::Echo => self.start_tcp(ECHO_ADDR, ECHO_PORT),
            Test::Dns => self.start_dns(DNS_HOST),
            Test::Http => {
                self.http_pending = true;
                self.start_dns(HTTP_HOST);
            }
            Test::Work => self.start_work(),
        }
    }

    fn start_tcp(&mut self, addr: Ipv4, port: u16) {
        if !self.need_bearer(self.test) {
            return;
        }
        let Some(bearer) = &self.bearer else { return };
        // 512 in, 256 out: enough for a greeting plus a short echo, and for an HTTP
        // status line and the first headers.
        let mut s = match TcpStream::open(&mut self.net, bearer, 512, 256) {
            Ok(s) => s,
            Err(e) => {
                self.log.push(|w| {
                    w.s("open failed: ").s(err_name(e));
                });
                return;
            }
        };
        self.log.push(|w| {
            w.s("connect ").ip(addr).s(":").n(port as i64);
        });
        if let Err(e) = s.connect(&mut self.net, addr, port) {
            self.log.push(|w| {
                w.s("connect failed: ").s(err_name(e));
            });
            return;
        }
        self.stream = Some(s);
    }

    fn start_dns(&mut self, host: &str) {
        if !self.need_bearer(self.test) {
            return;
        }
        let Some(bearer) = &self.bearer else { return };
        self.log.push(|w| {
            w.s("resolving ").s(host);
        });
        match Lookup::start(&mut self.net, bearer, host) {
            Ok(l) => self.lookup = Some(l),
            Err(e) => self.log.push(|w| {
                w.s("resolve failed: ").s(err_name(e));
            }),
        }
    }

    fn start_work(&mut self) {
        if self.work_running {
            self.log.push(|w| {
                w.s("already running");
            });
            return;
        }
        // A real 2048-bit modulus, so the timing is the real timing. Built here rather
        // than as a constant because the job's input format is three length-prefixed
        // fields and assembling it is clearer than a hex blob.
        let modulus: Vec<u8> = {
            let mut v = vec![0u8; 256];
            let mut s = 0xC0DEu32 | 1;
            for b in v.iter_mut() {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                *b = s as u8;
            }
            v[0] |= 0x80; // full width
            v[255] |= 1; // odd, which Montgomery reduction requires
            v
        };
        self.work_in.clear();
        for field in [&modulus[..], &[3u8][..], &[0xA5u8; 256][..]] {
            self.work_in.extend_from_slice(&(field.len() as u16).to_be_bytes());
            self.work_in.extend_from_slice(field);
        }

        // The spinner is the actual test. A repeating timer drives it from rust_step,
        // which only runs if the GUI thread is free — so the spinner moving *is* the
        // proof that the job is somewhere else.
        if self.timer.is_none() {
            let mut h = 0i32;
            if unsafe { sys::shim_timer_every(120, &mut h) } == sys::SHIM_OK {
                self.timer = Some(h);
            }
        }

        let rc = unsafe {
            sys::shim_work_submit(
                OP_MODPOW,
                self.work_in.as_ptr(),
                self.work_in.len() as i32,
                self.work_out.as_mut_ptr(),
                self.work_out.len() as i32,
            )
        };
        if rc == sys::SHIM_OK {
            self.work_running = true;
            self.log.push(|w| {
                w.s("2048-bit modpow submitted");
            });
            self.log.push(|w| {
                w.s("spinner must keep moving");
            });
        } else {
            self.log.push(|w| {
                w.s("submit failed: ").n(rc as i64);
            });
        }
    }

    /// Read whatever arrived and log it, then send the echo probe's payload once.
    fn drain(&mut self, first: bool) {
        let mut buf = [0u8; 64];
        let Some(s) = &mut self.stream else { return };
        loop {
            let n = match s.read(&mut self.net, &mut buf) {
                Ok(n) => n,
                Err(_) => break,
            };
            if n == 0 {
                break;
            }
            self.log.push(|w| {
                w.s("recv ").n(n as i64).s(" \"").bytes(&buf[..n]).s("\"");
            });
        }
        if first && self.test == Test::Echo {
            let payload = b"hello from E72";
            match s.write(&mut self.net, payload) {
                Ok(n) => self.log.push(|w| {
                    w.s("sent ").n(n as i64).s(" bytes");
                }),
                Err(e) => self.log.push(|w| {
                    w.s("send failed: ").s(err_name(e));
                }),
            }
        }
    }
}

/// Short names for the errors this probe can actually produce. `Display` would pull in
/// `core::fmt`'s machinery for the sake of a dozen strings.
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

impl App for NetProbe {
    fn title(&self) -> &str {
        "Net probe"
    }

    fn handle_raw(&mut self, ev: &RawEvent) -> Handled {
        // The spinner tick. Consumed here rather than becoming a key, and the only reason
        // this app implements handle_raw at all besides the network completions.
        if ev.kind == sys::SHIM_EV_TIMER {
            self.spin = self.spin.wrapping_add(1);
            return Handled::Consumed;
        }

        if ev.kind == sys::SHIM_EV_WORK_DONE {
            self.work_running = false;
            if let Some(h) = self.timer.take() {
                unsafe { sys::shim_timer_cancel(h) };
            }
            let status = ev.status;
            let head = [self.work_out[0], self.work_out[1], self.work_out[2]];
            self.log.push(|w| {
                if status == 0 {
                    w.s("modpow ok, result starts ")
                        .n(head[0] as i64)
                        .s(" ")
                        .n(head[1] as i64)
                        .s(" ")
                        .n(head[2] as i64);
                } else {
                    w.s("modpow failed: ").n(status as i64);
                }
            });
            return Handled::Consumed;
        }

        // The bearer, which every network test waits for.
        if let Some(b) = &mut self.bearer {
            match b.on_event(&mut self.net, ev) {
                Ok(true) => {
                    let iap = b.iap().unwrap_or(0);
                    self.log.push(|w| {
                        w.s("bearer up, IAP ").n(iap as i64);
                    });
                    if let Some(t) = self.pending.take() {
                        self.run(t);
                    }
                    return Handled::Consumed;
                }
                Ok(false) => {}
                Err(e) => {
                    self.pending = None;
                    self.log.push(|w| {
                        w.s("bearer failed: ").s(err_name(e));
                    });
                    return Handled::Consumed;
                }
            }
        }

        // DNS.
        if let Some(l) = &mut self.lookup {
            match l.on_event(ev) {
                Ok(Some(addr)) => {
                    self.log.push(|w| {
                        w.s("resolved ").ip(addr);
                    });
                    self.lookup = None;
                    if self.http_pending {
                        self.http_pending = false;
                        self.start_tcp(addr, HTTP_PORT);
                    }
                    return Handled::Consumed;
                }
                Ok(None) => {}
                Err(e) => {
                    self.lookup = None;
                    self.http_pending = false;
                    self.log.push(|w| {
                        w.s("resolve failed: ").s(err_name(e));
                    });
                    return Handled::Consumed;
                }
            }
        }

        // The socket. `progress` is taken before any borrow of self, because the handlers
        // below log and logging borrows self mutably too.
        let progress = match &mut self.stream {
            Some(s) => s.on_event(&mut self.net, ev),
            None => return Handled::Ignored,
        };
        match progress {
            Progress::None => Handled::Ignored,
            Progress::Connected => {
                self.log.push(|w| {
                    w.s("connected");
                });
                if self.test == Test::Http {
                    // HTTP/1.0 with no keep-alive, so the server closes when it is done
                    // and the close itself is part of the result.
                    let req = b"GET / HTTP/1.0\r\nHost: example.com\r\n\r\n";
                    if let Some(s) = &mut self.stream {
                        let _ = s.write(&mut self.net, req);
                    }
                    self.log.push(|w| {
                        w.s("sent GET /");
                    });
                } else {
                    self.drain(true);
                }
                Handled::Consumed
            }
            Progress::Received(_) => {
                self.drain(false);
                Handled::Consumed
            }
            Progress::Sent(n) => {
                self.log.push(|w| {
                    w.s("send complete, ").n(n as i64).s(" bytes");
                });
                Handled::Consumed
            }
            Progress::Closed => {
                // Drain first: bytes that arrived with the close are still readable, and
                // an HTTP/1.0 response ends *with* the close.
                self.drain(false);
                self.log.push(|w| {
                    w.s("closed");
                });
                self.stream = None;
                Handled::Consumed
            }
            Progress::Failed(e) => {
                self.log.push(|w| {
                    w.s("failed: ").s(err_name(e));
                });
                self.stream = None;
                Handled::Consumed
            }
        }
    }

    fn handle_key(&mut self, ev: KeyEvent, _t: &Theme<'_>, _s: Rect) -> Handled {
        match ev.key {
            Key::Left | Key::Up => self.test = self.test.prev(),
            Key::Right | Key::Down => self.test = self.test.next(),
            Key::Select | Key::Enter => {
                let t = self.test;
                self.run(t);
            }
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

        // The spinner lives in the title bar's detail slot, which is where the eye
        // already is and costs no layout.
        let spinner = ["|", "/", "-", "\\"][(self.spin % 4) as usize];
        let detail = if self.work_running { spinner } else { "" };
        chrome::title_bar(c, frame.title, theme, self.test.name(), Some(detail));
        chrome::softkey_bar(c, frame.softkeys, theme, [Some("Run"), None, Some("Exit")]);

        let small = theme.fonts.small;
        let mut y = frame.content.y0 + 1;
        for i in 0..self.log.len {
            let text = self.log.line(i);
            // The newest line in the accent colour: with eleven lines of similar text,
            // "what just happened" is otherwise a search.
            let color = if i + 1 == self.log.len {
                theme.palette.accent
            } else {
                theme.palette.text
            };
            c.draw_text(Point::new(4, y + small.ascent()), text, small, color);
            y += small.line_height() + 1;
        }

        let hint = Rect { y0: frame.content.y1 - 12, ..frame.content };
        c.draw_text_in(hint, "arrows: test   Select: run", small, theme.palette.dim, Align::Center);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_truncates_rather_than_losing_a_line() {
        let mut log = Log::new();
        log.push(|w| {
            w.s("x".repeat(200).as_str());
        });
        assert_eq!(log.line(0).len(), LINE_CAP);
    }

    #[test]
    fn the_log_drops_the_oldest_line() {
        // The opposite of the shim's input queue, and deliberately: here the interesting
        // line is the one that just happened.
        let mut log = Log::new();
        for i in 0..LOG_CAP + 3 {
            log.push(|w| {
                w.n(i as i64);
            });
        }
        assert_eq!(log.len, LOG_CAP);
        assert_eq!(log.line(0), "3");
        assert_eq!(log.line(LOG_CAP - 1), "13");
    }

    #[test]
    fn numbers_format_including_the_awkward_ones() {
        let mut log = Log::new();
        log.push(|w| {
            w.n(0).s(" ").n(-42).s(" ").n(i32::MAX as i64);
        });
        assert_eq!(log.line(0), "0 -42 2147483647");
    }

    #[test]
    fn addresses_format_as_dotted_quad() {
        let mut log = Log::new();
        log.push(|w| {
            w.ip(Ipv4::new(192, 168, 15, 74));
        });
        assert_eq!(log.line(0), "192.168.15.74");
    }

    #[test]
    fn non_printable_bytes_become_dots() {
        // A server's reply is not ours to trust: a stray control byte must not break the
        // line it is being shown on.
        let mut log = Log::new();
        log.push(|w| {
            w.bytes(&[b'o', b'k', 0x00, 0x1B, 0xFF, b'!']);
        });
        assert_eq!(log.line(0), "ok...!");
    }

    #[test]
    fn the_test_selector_wraps_both_ways() {
        assert_eq!(Test::Echo.prev(), Test::Work);
        assert_eq!(Test::Work.next(), Test::Echo);
        let mut t = Test::Echo;
        for _ in 0..Test::ALL.len() {
            t = t.next();
        }
        assert_eq!(t, Test::Echo);
    }

    #[test]
    fn the_worker_job_rejects_a_malformed_input() {
        // The job runs on another thread with no way to report a panic, so every field is
        // bounds-checked and a bad input is an error code rather than a fault.
        let mut out = [0u8; 32];
        assert_ne!(modpow_job(OP_MODPOW, &[], &mut out), 0);
        assert_ne!(modpow_job(OP_MODPOW, &[0, 9, 1], &mut out), 0, "length past the end");
        assert_ne!(modpow_job(99, &[], &mut out), 0, "unknown opcode");
    }

    #[test]
    fn every_test_screen_draws_without_panicking() {
        // The layout arithmetic runs against a real canvas here rather than first on the
        // phone, where a panic is a User::Panic dialog with a number in it.
        use symbian_ui::testing;
        for t in Test::ALL {
            let mut app = NetProbe::new();
            app.test = t;
            // A full log, so the drawing loop runs at its widest rather than with one line.
            for i in 0..LOG_CAP + 2 {
                app.log.push(|w| {
                    w.s("line ").n(i as i64).s(" 192.168.15.74:7654 recv 64");
                });
            }
            app.work_running = true;
            app.spin = 3;
            let (_, px) = testing::with_canvas(symbian_gfx::Size::new(320, 240), |c| {
                testing::with_theme(symbian_ui::Palette::DARK, |theme| app.draw(c, theme));
            });
            assert!(px.iter().any(|&p| p != 0), "{t:?} drew an empty frame");
        }
    }

    #[test]
    fn switching_tests_does_not_leave_a_stale_socket() {
        // A completion for the previous test's socket arriving after a new run started
        // would be routed against a socket this run knows nothing about. run() drops them
        // for that reason, and this pins it.
        let mut app = NetProbe::new();
        app.http_pending = true;
        app.run(Test::Work);
        assert!(app.stream.is_none());
        assert!(app.lookup.is_none());
        assert!(!app.http_pending);
    }

    #[test]
    fn the_worker_job_computes_a_modpow() {
        // Small, so the test is fast; the same code path the 2048-bit job takes.
        let mut input = Vec::new();
        for f in [&[11u8][..], &[5u8][..], &[3u8][..]] {
            input.extend_from_slice(&(f.len() as u16).to_be_bytes());
            input.extend_from_slice(f);
        }
        let mut out = [0u8; 1];
        assert_eq!(modpow_job(OP_MODPOW, &input, &mut out), 0);
        // 5^3 mod 11 = 125 mod 11 = 4
        assert_eq!(out[0], 4);
    }
}
