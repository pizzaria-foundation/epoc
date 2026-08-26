//! Can this handset talk to `api.github.com`?
//!
//! One question, one binary, and it is asked before anything is built on the answer. The package
//! manager's repository feature is designed around GitHub Releases, which means a JSON payload from
//! `api.github.com` over TLS 1.2 — and every part of that sentence is a claim about a phone from
//! 2009 rather than about our code.
//!
//! What is already known, from `docs/plan-browser.md`, measured on this handset on 2026-08-24:
//!
//! ```text
//! GET https://en.wikipedia.org/wiki/Symbian   status=200  bytes=108128
//! GET https://github.com/                     status=200  bytes=117450
//! ```
//!
//! So the transport works: the patched `ssl.dll` routes `CSecureSocket` into mbedtls 3.4.1 and gives
//! the whole device TLS 1.2, `RHTTPSession` included. What is *not* known is this host in
//! particular:
//!
//! - `api.github.com` is a different name than `github.com`, with its own certificate and its own
//!   TLS configuration.
//! - The API **refuses a request with no `User-Agent`** with a 403. `shim_http.cpp:250` sets one, so
//!   the expectation is good — but "sets a UA" and "sets a UA this API accepts" are two statements.
//! - The size of a `/releases/latest` payload decides whether the JSON parser can hold the document
//!   or has to stream it. Guessing that wrong is a rewrite.
//!
//! Three facts, and none of them is reachable by reasoning. So: one fetch, and the status, the byte
//! count and the first 512 bytes of the body go in the log.
//!
//! Headless and one-shot. `daemon_entry!` rather than `entry!` for the reason `apps/httpprobe`
//! records: a GUI application is one instance per UID3, so a run that died leaving its window group
//! behind made the next launch exit on the spot, with no log to say why.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use symbian::http::{Body, Fetch, Flags, Http, Progress, ShimHttp};
use symbian::net::{Bearer, Net, RawEvent, ShimNet};

/// Where to read the repository to ask about: one line, `owner/repo`.
///
/// A file rather than a constant, because the first run answered the transport question and left a
/// content one — a 404 for a repository that is not public yet — and finding out how big a real
/// release payload is should not cost a cross-compile, a sideload and an install per attempt. Push a
/// new target with `epoc sh "put …"` and run again.
const TARGET_FILE: &str = "C:\\Data\\ghprobe.txt";
/// Used when there is no file, or it is empty.
const DEFAULT_TARGET: &str = "pizzaria-foundation/home";

/// Where the decoded body is saved, so it can be pulled off the phone and become the JSON parser's
/// fixture.
///
/// A real payload, and that is the whole reason this write exists: a fixture written by the same
/// person who writes the parser proves only that the two agree with each other. This one has
/// `author` blocks, `node_id`s, escaped text and a hundred fields nobody asked for — exactly the
/// shape that finds a parser's assumptions.
const SAVE_PATH: &str = "C:\\Data\\ghprobe.json";

/// `/releases/latest` rather than `/releases`: one release instead of every release the repository
/// ever had, which is the difference between a payload of a few KB and one of a few hundred. The
/// real feature will want the full list, and knowing the size of one is how we find out whether that
/// is affordable.
/// The `owner/repo` to ask about, from [`TARGET_FILE`] or the default.
///
/// Trimmed of whitespace and of a leading `https://github.com/`, because the thing a person has in
/// their clipboard is a browser URL and retyping it as `owner/repo` is a step that exists only to be
/// got wrong. The real feature will want the same courtesy.
fn target() -> String {
    let mut fs = symbian::fs::ShimFs;
    let raw = symbian::fs::Utf16Path::new(TARGET_FILE)
        .ok()
        .and_then(|p| symbian::fs::read(&mut fs, &p).ok().flatten())
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let t = raw
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("github.com/")
        .trim_end_matches('/')
        .trim();
    if t.is_empty() {
        String::from(DEFAULT_TARGET)
    } else {
        String::from(t)
    }
}

fn url() -> String {
    alloc::format!("https://api.github.com/repos/{}/releases/latest", target())
}

/// How much of the *decoded* body to print. Enough to see the shape of the JSON — `tag_name`, the
/// first asset, `browser_download_url`.
const KEEP: usize = 512;

/// Ceiling on the decoded body. DEFLATE's ratio is unbounded, so this is attacker-controlled input
/// like any other and the caller has to say what it will hold — `Body::decode_to`'s own words. 256 KB
/// is far above the 3.4 KB compressed that this API actually sent, and far below anything that would
/// trouble a 4 MB heap.
const MAX_BODY: usize = 256 * 1024;

/// Give up if nothing has happened for this long. A probe that hangs teaches nothing and has to be
/// killed by hand, which on a headless binary means a reboot.
const TIMEOUT_MS: i32 = 60_000;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Phase {
    Connecting,
    Fetching,
    Done,
}

pub struct GhProbe<N: Net = ShimNet, H: Http = ShimHttp> {
    net: N,
    http: H,
    bearer: Option<Bearer>,
    fetch: Option<Fetch>,
    phase: Phase,
    /// Body bytes **on the wire** — compressed, if the server compressed them.
    got: usize,
    /// The bytes themselves, held so they can be inflated once the response says whether to.
    body: Body,
    timer: Option<i32>,
    exit: bool,
}

impl GhProbe<ShimNet, ShimHttp> {
    pub fn new() -> Self {
        Self::with(ShimNet, ShimHttp)
    }
}

impl<N: Net, H: Http> GhProbe<N, H> {
    pub fn with(net: N, http: H) -> Self {
        let mut me = Self {
            net,
            http,
            bearer: None,
            fetch: None,
            phase: Phase::Connecting,
            got: 0,
            body: Body::with_cap(MAX_BODY),
            timer: None,
            exit: false,
        };
        symbian::log!("[ghprobe] asking {}", url());
        me.timer = symbian::timer_after(TIMEOUT_MS).ok();
        me.connect();
        me
    }

    /// Attach to a bearer that is already up before asking for one.
    ///
    /// Attaching joins whatever the phone already has online, with no dialog. That matters more than
    /// speed here: a probe that stops on an access-point prompt nobody is watching reports nothing.
    fn connect(&mut self) {
        match Bearer::attach(&mut self.net) {
            Ok(b) => {
                symbian::log!("[ghprobe] bearer attached, handle {}", b.handle());
                self.bearer = Some(b);
            }
            Err(e) => {
                symbian::log!("[ghprobe] no bearer to attach ({}); asking for the default", e.code());
                match Bearer::start_default(&mut self.net) {
                    Ok(b) => self.bearer = Some(b),
                    Err(e) => self.give_up("no bearer", e.code()),
                }
            }
        }
    }

    /// Open the HTTP session over the bearer that just came up, and then ask.
    ///
    /// The session is a separate step from the bearer and easy to leave out — `Fetch::start` only
    /// issues the GET, so without this the answer is `KErrNotReady` (-18) from the shim and it looks
    /// like a transport failure rather than a missing call. It looked exactly like that here on the
    /// first run.
    fn start_fetch(&mut self) {
        let handle = match self.bearer.as_ref() {
            Some(b) => b.handle(),
            None => return,
        };
        if let Err(e) = self.http.open(handle) {
            return self.give_up("session open", e.code());
        }
        symbian::log!("[ghprobe] session open on bearer handle {handle}");
        let u = url();
        // gzip on, because the answer to "how big is this payload" is more useful with the encoding
        // the real feature would use. `Fetch` inflates transparently.
        match Fetch::start(&mut self.http, &u, true) {
            Ok(f) => {
                self.phase = Phase::Fetching;
                self.fetch = Some(f);
                symbian::log!("[ghprobe] fetch started");
            }
            Err(e) => self.give_up("fetch refused", e.code()),
        }
    }

    /// Copy out whatever the stack has buffered.
    ///
    /// Held rather than decoded here, because whether these bytes need inflating is a property of
    /// the *response* and is only settled when the transaction ends. Reading them raw and printing
    /// them is what the first run did, and it printed 512 bytes of gzip as dots.
    fn drain(&mut self) {
        let mut buf = [0u8; 1024];
        loop {
            let n = match self.http.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            self.got += n;
            self.body.push(&buf[..n]);
        }
    }

    fn give_up(&mut self, what: &str, code: i32) {
        // The code stays a number unless it is one this project has measured and can explain. An
        // explanation that is wrong sends whoever debugs this to the wrong place; the number sends
        // them to the log. `apps/browser` adopted this rule after `-5` cost an afternoon.
        let words = match code {
            -5 => " (KErrNotSupported: cipher or protocol refused, not a certificate)",
            -18 => " (KErrNotReady: the HTTP session was never opened over the bearer)",
            -7548 => " (certificate not trusted)",
            _ => "",
        };
        symbian::log!("[ghprobe] FAILED: {what} rc={code}{words}");
        self.finish();
    }

    fn finish(&mut self) {
        self.phase = Phase::Done;
        if let Some(t) = self.timer.take() {
            symbian::timer_cancel(t);
        }
        if let Some(b) = self.bearer.as_mut() {
            b.stop(&mut self.net);
        }
        self.exit = true;
    }

    /// The report. Deliberately the whole answer in one place, because the next person to read this
    /// is reading `epoc logs ghprobe` and nothing else.
    ///
    /// **Both sizes**, and that distinction is the point of the second run: the wire size is what the
    /// connection cost and the decoded size is what the JSON parser will actually hold. The first run
    /// reported only the first and printed 512 bytes of gzip as dots.
    fn report(&mut self, status: u16, flags: Flags) {
        let mut out: Vec<u8> = Vec::new();
        let decoded = match self.body.decode_to(flags, MAX_BODY, &mut out) {
            Ok(n) => n,
            Err(e) => {
                symbian::log!("[ghprobe] body would not decode rc={}", e.code());
                0
            }
        };
        symbian::log!(
            "[ghprobe] status={status} wire={} decoded={} gzip={} for-the-parser={}",
            self.got,
            decoded,
            flags.gzip(),
            decoded
        );
        // JSON is ASCII by definition, so anything else is a byte worth seeing as a dot rather than
        // as a mangled character.
        let show = out.len().min(KEEP);
        let mut line = String::with_capacity(show);
        for &b in &out[..show] {
            line.push(if (0x20..0x7F).contains(&b) { b as char } else { '.' });
        }
        symbian::log!("[ghprobe] body[0..{show}]: {line}");

        if decoded > 0 {
            let mut fs = symbian::fs::ShimFs;
            match symbian::fs::Utf16Path::new(SAVE_PATH)
                .and_then(|p| symbian::fs::write_atomic(&mut fs, &p, &out))
            {
                Ok(()) => symbian::log!("[ghprobe] saved {decoded} bytes to {SAVE_PATH}"),
                Err(e) => symbian::log!("[ghprobe] save failed rc={}", e.code()),
            }
        }
        if status == 404 {
            symbian::log!(
                "[ghprobe] 404 is a content answer, not a transport one: the handshake closed and the \
                 User-Agent was accepted (a refused UA answers 403). The repository is private, \
                 renamed, or has no releases. Put another target in {TARGET_FILE}."
            );
        }
        if status == 403 {
            symbian::log!(
                "[ghprobe] 403 from this API usually means the User-Agent was refused; \
                 shim_http.cpp:250 is where it is set"
            );
        }
        self.finish();
    }

    fn on_event(&mut self, ev: &RawEvent) {
        if self.exit {
            return;
        }
        if ev.kind == symbian_sys::SHIM_EV_TIMER && Some(ev.handle) == self.timer {
            symbian::log!("[ghprobe] nothing happened for {}s; giving up", TIMEOUT_MS / 1_000);
            self.timer = None;
            self.finish();
            return;
        }

        if self.phase == Phase::Connecting {
            let up = match self.bearer.as_mut() {
                Some(b) => b.on_event(&mut self.net, ev),
                None => return,
            };
            match up {
                Ok(true) => {
                    symbian::log!("[ghprobe] bearer up");
                    self.start_fetch();
                }
                Ok(false) => {}
                Err(e) => self.give_up("bearer failed", e.code()),
            }
            return;
        }

        let Some(mut f) = self.fetch.take() else { return };
        let progress = f.on_event_with(&mut self.http, ev);
        self.fetch = Some(f);
        match progress {
            Progress::Idle => {}
            Progress::Head(status) => symbian::log!("[ghprobe] headers: status={status}"),
            Progress::Body(_) => self.drain(),
            Progress::Done(r) => {
                self.drain();
                self.report(r.status, r.flags);
            }
            Progress::NotModified => {
                symbian::log!("[ghprobe] 304, which we never asked for");
                self.finish();
            }
            Progress::Failed(e) => self.give_up("transfer failed", e.code()),
        }
    }
}

impl Default for GhProbe<ShimNet, ShimHttp> {
    fn default() -> Self {
        Self::new()
    }
}

impl<N: Net, H: Http> symbian_app::DaemonApp for GhProbe<N, H> {
    fn handle_raw(&mut self, ev: &RawEvent) {
        self.on_event(ev);
    }

    fn should_exit(&self) -> bool {
        self.exit
    }
}
