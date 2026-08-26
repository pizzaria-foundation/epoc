//! tileprobe — Marco 0 of the map plan: does a map tile survive the trip from the web to pixels?
//!
//! # The one measurement, and why it needs both halves
//!
//! `apps/httpprobe` proved this handset fetches real pages over the platform HTTP stack. Nothing
//! has ever proved it can turn the bytes into an image: `crates/symbian/src/image.rs` and
//! `shim/src/shim_image.cpp` are written, reviewed and **have never executed on a device** — no
//! `app.conf` in the repo sets `USE_IMAGE` and nothing calls `Decoder`. A map is a grid of PNGs,
//! so that untested path is its critical path, and this probe is the first execution of it.
//!
//! Fetching and decoding are measured together because that is the shape the map has: bytes arrive
//! in memory and go straight into `Decoder::memory`, never touching a file. A probe that decoded a
//! PNG already on the card would test a different thing and would not notice, for instance, a
//! truncated body that still decodes into a half tile.
//!
//! # What it reports, and why each number is a decision waiting
//!
//! - **ms of network, per tile.** The stack does one transaction at a time (see the header of
//!   `shim/src/shim_http.cpp`). Four tiles in sequence is the honest cost of the smallest useful
//!   pan, and it is what decides whether the map needs a connection pool or a placeholder grid.
//! - **ms of decode, per tile.** From `symbian::monotonic_us`, not from pump ticks: a decode may
//!   well be faster than one tick, and a stopwatch coarser than the thing it times reports zero.
//! - **the decoder's whole `Progress`.** `native_w/h`, `out_w/h`, the reduction factor, the display
//!   mode it settled on, and how many `ContinueConvert` rounds it took. This is the first reading
//!   of this handset's ICL in the whole project, and every field of it is new information.
//! - **out_w == 256.** The pass/fail. A tile that comes back at another size means the ICL reduced
//!   it, and the map's blit would have to resample every tile — a different application.
//!
//! # Nothing here waits
//!
//! Both halves are asynchronous and both complete as events: `SHIM_EV_HTTP_*` from the HTTP stack
//! and `SHIM_EV_IMAGE_DONE` from the ICL. `shim_image.cpp`'s header explains at length why waiting
//! on a decode takes the whole device with it — the codec's own active object runs in the calling
//! thread, so blocking for it deadlocks the thread that would have driven it.
//!
//! # The tiles, and the etiquette
//!
//! Four adjacent tiles at zoom 15 over Recife — a 2x2 block, which is what one pan step asks for.
//! The OSM tile usage policy requires an identifiable User-Agent and forbids heavy use; the shim
//! already sends `SymbianRustSdk/0.1`, which identifies rather than impersonates, and four tiles
//! per run is not bulk. If this probe is ever put in a loop, that stops being true.
//!
//! # Testable without a phone
//!
//! The state machine is generic over [`Net`], [`Http`] and [`Images`], so the tests below replay a
//! body arriving in parts, a decode that fails and a tile that comes back at the wrong size — none
//! of which needs a device. What needs the device is every millisecond in the report.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use symbian::http::{Body, Fetch, Http, Progress, ShimHttp};
use symbian::image::{Decoder, Images, ShimImages};
use symbian::net::{Bearer, Net, RawEvent, ShimNet};
use symbian_report::{push_i64, Report};

/// The pump tick. 200 ms: fine enough to notice a stalled fetch, and not the stopwatch — every
/// duration in the report comes from [`symbian::monotonic_us`] instead.
const TICK_MS: i32 = 200;

/// A fetch that produces nothing for this long is abandoned and the next tile is tried. Twelve
/// seconds is well past any tile on a working connection and short enough that four dead ones do
/// not make a run outlast the person watching it.
const FETCH_TIMEOUT_TICKS: u32 = 60;

/// The same, for a decode. A decode that never completes emits no event at all, which is precisely
/// the failure `Decoder::progress` exists to describe — so this ceiling is what gets that snapshot
/// into the report instead of hanging.
const DECODE_TIMEOUT_TICKS: u32 = 25;

/// Ticks to keep running after the report is written, so the log flush lands.
const LINGER_TICKS: u32 = 3;

/// Room for one tile's body. A 256x256 PNG from OSM is 2–40 KB; 256 KB is far past anything
/// legitimate and bounds what a misrouted response can cost.
const BODY_CAP: usize = 256 * 1024;

/// The box handed to the decoder.
///
/// Exactly the tile size, and that is the measurement: the ICL only reduces by powers of two, so
/// asking for 256 and being given 256 says it did not reduce. Asking for something larger would
/// have made "no reduction" and "reduced to fit" indistinguishable in the result.
const TILE_PX: i32 = 256;

/// A 2x2 block at zoom 15 over Recife — one pan step's worth.
pub const TILES: &[&str] = &[
    "https://tile.openstreetmap.org/15/13209/17118.png",
    "https://tile.openstreetmap.org/15/13210/17118.png",
    "https://tile.openstreetmap.org/15/13209/17119.png",
    "https://tile.openstreetmap.org/15/13210/17119.png",
];

/// One tile's trip, end to end.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Row {
    pub url: &'static str,
    pub status: u16,
    /// The platform error from the fetch, 0 when it completed.
    pub err: i32,
    pub bytes: usize,
    /// How many body callbacks the body arrived in — what streaming looks like here.
    pub parts: u32,
    pub net_ms: i64,
    /// 0 when the decode was never started, which is a different fact from a decode that took no
    /// measurable time.
    pub decode_ms: i64,
    /// The platform error from the decode, 0 when it produced pixels.
    pub decode_err: i32,
    pub out_w: i32,
    pub out_h: i32,
    pub native_w: i32,
    pub native_h: i32,
    pub factor: i32,
    /// `TDisplayMode` the codec settled on.
    pub mode: i32,
    /// `TFrameInfo::iFlags`: bit 2 is `EFullyScaleable`, bit 4 is `ECanDither`.
    pub frame_flags: i32,
    pub continues: i32,
    /// The first pixel of the decoded tile. Not decoration: a decode that reports success and
    /// hands back a buffer of zeroes is a failure the size fields cannot show.
    pub first_pixel: u16,
    /// True when a ceiling here ended it rather than a completion.
    pub abandoned: bool,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Before the first tick. Nothing has been asked of the platform yet.
    Waking,
    /// A bearer was requested and has not come up.
    Connecting,
    /// A tile is being fetched.
    Fetching,
    /// A tile's bytes are in the decoder.
    Decoding,
    /// The list is done.
    Finished,
    /// Bring-up failed; the report says why.
    Failed,
}

pub struct TileProbe<N: Net, H: Http, I: Images> {
    net: N,
    http: H,
    images: I,
    phase: Phase,
    bearer: Option<Bearer>,
    session: bool,
    /// Index into [`TILES`].
    at: usize,
    fetch: Option<Fetch>,
    decoder: Option<Decoder<I>>,
    body: Body,
    parts: u32,
    ticks: u32,
    /// The tick the current fetch or decode started on, for the ceilings.
    stage_started: u32,
    /// The monotonic microsecond the current fetch or decode started at, for the report.
    stage_us: u64,
    row: Row,
    rows: Vec<Row>,
    note: String,
    report_path: String,
    reported: bool,
    finished_at: u32,
    exit: bool,
}

impl TileProbe<ShimNet, ShimHttp, ShimImages> {
    pub fn new() -> Self {
        // Arming the timer is what makes the probe run at all, and it happens here rather than in
        // `with` because `with` is what the host tests use: a test drives ticks itself, and a
        // constructor that reached for the platform clock would make every one of them need a
        // phone.
        let _ = symbian::timer_every(TICK_MS);
        Self::with(ShimNet, ShimHttp, ShimImages)
    }
}

impl Default for TileProbe<ShimNet, ShimHttp, ShimImages> {
    fn default() -> Self {
        Self::new()
    }
}

impl<N: Net, H: Http, I: Images + Clone> TileProbe<N, H, I> {
    pub fn with(net: N, http: H, images: I) -> Self {
        Self {
            net,
            http,
            images,
            phase: Phase::Waking,
            bearer: None,
            session: false,
            at: 0,
            fetch: None,
            decoder: None,
            body: Body::with_cap(BODY_CAP),
            parts: 0,
            ticks: 0,
            stage_started: 0,
            stage_us: 0,
            row: Row::default(),
            rows: Vec::new(),
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

    fn fail(&mut self, what: &str, code: i32) {
        self.phase = Phase::Failed;
        let mut s = String::from(what);
        s.push_str(" (");
        push_i64(&mut s, code as i64);
        s.push(')');
        self.note = s;
        symbian::log!("[tileprobe] FAILED {}", code);
    }

    /// The configured access point, with no dialog to answer — the same choice httpprobe settled
    /// on, and for the same reason: an unattended run needs nobody watching.
    fn connect(&mut self) {
        self.phase = Phase::Connecting;
        match Bearer::start_default(&mut self.net) {
            Ok(b) => {
                symbian::log!("[tileprobe] bearer requested, handle {}", b.handle());
                self.bearer = Some(b);
                self.note = String::from("bringing up a bearer");
            }
            Err(e) => self.fail("no bearer", e.code()),
        }
    }

    fn begin(&mut self) {
        let handle = match self.bearer.as_ref() {
            Some(b) => b.handle(),
            None => return,
        };
        if let Err(e) = self.http.open(handle) {
            self.fail("session", e.code());
            return;
        }
        symbian::log!("[tileprobe] session open on bearer handle {}", handle);
        self.session = true;
        self.next_tile();
    }

    fn next_tile(&mut self) {
        self.body = Body::with_cap(BODY_CAP);
        self.parts = 0;
        self.decoder = None;

        if self.at >= TILES.len() {
            self.fetch = None;
            self.phase = Phase::Finished;
            return;
        }

        let url = TILES[self.at];
        self.row = Row { url, ..Row::default() };
        self.stage_started = self.ticks;
        self.stage_us = symbian::monotonic_us();
        // gzip deliberately NOT asked for. A PNG is already compressed, so the header would buy
        // nothing and would put an inflate stage between the socket and the codec — one more place
        // for a tile to be wrong, measuring something this probe is not about.
        symbian::log!("[tileprobe] GET {}", url);
        match Fetch::start(&mut self.http, url, false) {
            Ok(f) => {
                self.fetch = Some(f);
                self.phase = Phase::Fetching;
                self.note = String::from(url);
            }
            Err(e) => {
                // A URL the shim would not take is a result, not a crash: record it and move on,
                // because stopping here would lose the tiles after it.
                self.row.err = e.code();
                self.commit_row();
            }
        }
    }

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
            self.body.push(&buf[..n]);
        }
    }

    /// The fetch ended. Either hand the bytes to the codec or record the failure.
    fn fetch_done(&mut self, status: u16, err: i32) {
        self.drain();
        self.fetch = None;
        self.row.status = status;
        self.row.err = err;
        self.row.parts = self.parts;
        self.row.bytes = self.body.len();
        self.row.net_ms = elapsed_ms(self.stage_us);
        symbian::log!(
            "[tileprobe] fetch done: status={} err={} bytes={} ms={}",
            status as i32,
            err,
            self.body.len() as i32,
            self.row.net_ms as i32
        );

        if err != 0 || status != 200 || self.body.is_empty() {
            self.commit_row();
            return;
        }
        self.start_decode();
    }

    fn start_decode(&mut self) {
        // The decoder reads from these bytes until it completes, so it takes ownership — the
        // buffer being alive is not a nicety, it is the contract in `image.rs`.
        let bytes = self.body.raw().to_vec();
        self.stage_started = self.ticks;
        self.stage_us = symbian::monotonic_us();
        match Decoder::memory(self.images.clone(), bytes, TILE_PX, TILE_PX) {
            Ok(d) => {
                self.decoder = Some(d);
                self.phase = Phase::Decoding;
            }
            Err(e) => {
                self.row.decode_err = e.code();
                self.commit_row();
            }
        }
    }

    /// A decode ended. `abandoned` when a ceiling here ended it rather than the codec.
    fn decode_done(&mut self, abandoned: bool) {
        let Some(mut d) = self.decoder.take() else {
            return;
        };
        self.row.decode_ms = elapsed_ms(self.stage_us);
        self.row.abandoned = abandoned;

        // Asked before the pixels: on a decode that never completed this snapshot is the only
        // evidence there is, and taking the result first would consume the handle.
        if let Some(p) = d.progress() {
            self.row.native_w = p.native_w;
            self.row.native_h = p.native_h;
            self.row.factor = p.factor;
            self.row.mode = p.mode;
            self.row.frame_flags = p.frame_flags;
            self.row.continues = p.continues;
            if p.error != 0 {
                self.row.decode_err = p.error;
            }
        }

        match d.take() {
            Ok(img) => {
                self.row.out_w = img.width;
                self.row.out_h = img.height;
                self.row.first_pixel = img.pixels.first().copied().unwrap_or(0);
            }
            Err(e) => {
                if self.row.decode_err == 0 {
                    self.row.decode_err = e.code();
                }
            }
        }
        symbian::log!(
            "[tileprobe] decode done: err={} {}x{} ms={}",
            self.row.decode_err,
            self.row.out_w,
            self.row.out_h,
            self.row.decode_ms as i32
        );
        self.commit_row();
    }

    fn commit_row(&mut self) {
        let row = core::mem::take(&mut self.row);
        self.rows.push(row);
        self.at += 1;
        self.next_tile();
    }

    fn on_tick(&mut self) {
        self.ticks = self.ticks.saturating_add(1);

        match self.phase {
            Phase::Waking => self.connect(),
            Phase::Connecting => {}
            Phase::Fetching => {
                if self.ticks.saturating_sub(self.stage_started) >= FETCH_TIMEOUT_TICKS {
                    if let Some(f) = self.fetch.as_mut() {
                        f.cancel(&mut self.http);
                    }
                    self.row.abandoned = true;
                    self.fetch_done(0, symbian_sys::SHIM_ERR_TIMED_OUT);
                }
            }
            Phase::Decoding => {
                if self.ticks.saturating_sub(self.stage_started) >= DECODE_TIMEOUT_TICKS {
                    self.decode_done(true);
                }
            }
            Phase::Finished => {
                if self.reported && self.ticks.saturating_sub(self.finished_at) >= LINGER_TICKS {
                    self.exit = true;
                }
            }
            Phase::Failed => {
                if self.reported && self.ticks.saturating_sub(self.finished_at) >= LINGER_TICKS {
                    self.exit = true;
                }
            }
        }
    }

    fn on_raw(&mut self, ev: &RawEvent) {
        if ev.kind == symbian_sys::SHIM_EV_TIMER {
            self.on_tick();
            self.report_if_finished();
            return;
        }

        // The bearer's own event. Its state machine owns the retry, so this only reacts to the
        // transition.
        if ev.kind == symbian_sys::SHIM_EV_NET_READY {
            let mut up = false;
            if let Some(b) = self.bearer.as_mut() {
                match b.on_event(&mut self.net, ev) {
                    Ok(true) => up = true,
                    Ok(false) => {}
                    Err(e) => self.fail("bearer", e.code()),
                }
            }
            if up && !self.session {
                self.begin();
            }
            self.report_if_finished();
            return;
        }

        if ev.kind == symbian_sys::SHIM_EV_IMAGE_DONE {
            // The handle check is what keeps a completion from a decode this probe already
            // abandoned from being credited to the one now running.
            let ours = self.decoder.as_ref().is_some_and(|d| d.owns(ev.handle));
            if ours {
                self.decode_done(false);
            }
            self.report_if_finished();
            return;
        }

        if let Some(f) = self.fetch.as_mut() {
            match f.on_event_with(&mut self.http, ev) {
                Progress::Idle | Progress::Head(_) => {}
                Progress::Body(_) => {
                    self.parts = self.parts.saturating_add(1);
                    self.drain();
                }
                Progress::Done(r) => self.fetch_done(r.status, 0),
                Progress::NotModified => self.fetch_done(304, 0),
                Progress::Failed(e) => self.fetch_done(0, e.code()),
            }
        }
        self.report_if_finished();
    }

    fn report_if_finished(&mut self) {
        if !matches!(self.phase, Phase::Finished | Phase::Failed) || self.reported {
            return;
        }
        self.reported = true;
        self.finished_at = self.ticks;
        let mut fs = symbian::fs::ShimFs;
        self.write_report(&mut fs);
        symbian::log!("[tileprobe] report written, closing in {} ticks", LINGER_TICKS);
    }

    /// The report. Written once, when the list is done.
    pub fn write_report<F: symbian::fs::Fs>(&mut self, fs: &mut F) {
        let mut r = Report::new("tileprobe");
        r.head("A map tile, from the web to pixels");
        r.line("");
        r.line("The first execution of crates/symbian/src/image.rs and shim_image.cpp on a");
        r.line("handset. Milliseconds come from the monotonic clock, not from pump ticks.");
        r.line("");

        if self.phase == Phase::Failed {
            r.check_note("bring-up", false, &self.note);
        } else {
            r.check("bearer and HTTP session", self.session);
        }

        let mut fetched = 0u32;
        let mut decoded = 0u32;
        let mut full_size = 0u32;
        let mut net_total = 0i64;
        let mut decode_total = 0i64;

        for row in &self.rows {
            if row.err == 0 && row.status == 200 {
                fetched += 1;
                net_total += row.net_ms;
            }
            if row.decode_err == 0 && row.out_w > 0 {
                decoded += 1;
                decode_total += row.decode_ms;
                if row.out_w == TILE_PX && row.out_h == TILE_PX {
                    full_size += 1;
                }
            }

            let mut line = String::new();
            // Only the z/x/y tail: the host is the same on every row and the prefix would push
            // everything that differs off a narrow report.
            line.push_str(tail(row.url));
            line.push_str("  ");
            if row.err != 0 {
                line.push_str("ERR ");
                push_i64(&mut line, row.err as i64);
            } else {
                push_i64(&mut line, row.status as i64);
            }
            line.push_str("  ");
            push_i64(&mut line, row.bytes as i64);
            line.push_str(" B in ");
            push_i64(&mut line, row.net_ms);
            line.push_str(" ms / ");
            push_i64(&mut line, row.parts as i64);
            line.push_str(" parts");
            r.line(&line);

            let mut line = String::from("    decode ");
            if row.decode_err != 0 {
                line.push_str("ERR ");
                push_i64(&mut line, row.decode_err as i64);
                line.push(' ');
            } else if row.out_w == 0 {
                line.push_str("not attempted ");
            } else {
                push_i64(&mut line, row.out_w as i64);
                line.push('x');
                push_i64(&mut line, row.out_h as i64);
                line.push_str(" in ");
                push_i64(&mut line, row.decode_ms);
                line.push_str(" ms  px0=0x");
                symbian_report::push_hex(&mut line, row.first_pixel as u32, 4);
                line.push(' ');
            }
            line.push_str(" native=");
            push_i64(&mut line, row.native_w as i64);
            line.push('x');
            push_i64(&mut line, row.native_h as i64);
            line.push_str(" factor=");
            push_i64(&mut line, row.factor as i64);
            line.push_str(" mode=");
            push_i64(&mut line, row.mode as i64);
            line.push_str(" flags=");
            push_i64(&mut line, row.frame_flags as i64);
            line.push_str(" continues=");
            push_i64(&mut line, row.continues as i64);
            if row.abandoned {
                line.push_str("  ABANDONED");
            }
            r.line(&line);
        }

        r.line("");
        r.check("every tile fetched", fetched as usize == TILES.len());
        r.check("every tile decoded", decoded as usize == TILES.len());
        // The pass that matters. A tile the ICL reduced is not a failure of this probe and IS a
        // different map: every blit would have to resample.
        r.check("every tile came back at 256x256", full_size as usize == TILES.len());
        r.num("total network ms, four tiles in sequence", net_total);
        r.num("total decode ms", decode_total);
        r.line("");
        r.line("The network total is the cost of the smallest useful pan on a stack that runs");
        r.line("one transaction at a time. It is the number that decides whether the map needs");
        r.line("a connection pool (F3 of the browser plan) or a placeholder grid.");
        r.line("");
        r.line("px0 of 0x0000 on every tile with a successful decode means the codec reported");
        r.line("success and wrote nothing — a failure no size field can show.");

        r.open_output(fs, "", "tileprobe.txt");
        r.finish(fs);
        self.report_path = String::from(r.path_label());
    }
}

/// Whole milliseconds since `started`, from the monotonic clock. Saturates rather than wrapping:
/// a clock that went backwards is a bad reading, not a negative duration.
fn elapsed_ms(started: u64) -> i64 {
    let now = symbian::monotonic_us();
    (now.saturating_sub(started) / 1000) as i64
}

/// The `z/x/y.png` tail of a tile URL, or the whole thing when it has no recognisable shape.
fn tail(url: &str) -> &str {
    match url.rfind("/15/") {
        Some(i) => &url[i + 1..],
        None => url,
    }
}

impl<N: Net, H: Http, I: Images + Clone> symbian_app::DaemonApp for TileProbe<N, H, I> {
    fn handle_raw(&mut self, ev: &RawEvent) {
        self.on_raw(ev);
    }

    fn should_exit(&self) -> bool {
        self.exit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use symbian::image::{Image, MemImages};

    /// The real `ShimNet` and `ShimHttp`, which on the host are stubs that answer
    /// `NotSupported` to everything. Inert rather than faked, because nothing below drives the
    /// fetch through them — `apps/httpprobe` already covers the fetch, and what is under test
    /// here is what happens to the bytes afterwards.
    fn probe(images: MemImages) -> TileProbe<ShimNet, ShimHttp, MemImages> {
        TileProbe::with(ShimNet, ShimHttp, images)
    }

    #[test]
    fn a_tile_that_decodes_at_full_size_passes() {
        let tile = MemImages::solid(TILE_PX, TILE_PX, 0x1234);
        let mut p = probe(MemImages::new(vec![tile]));
        p.row = Row { url: TILES[0], ..Row::default() };
        p.body.push(&[0x89, b'P', b'N', b'G']);
        p.start_decode();
        p.decode_done(false);

        let row = &p.rows()[0];
        assert_eq!(row.decode_err, 0);
        assert_eq!((row.out_w, row.out_h), (TILE_PX, TILE_PX));
        assert_eq!(row.first_pixel, 0x1234);
    }

    #[test]
    fn a_reduced_tile_is_recorded_rather_than_rounded_up() {
        // The ICL reducing by two is not an error and must not read as one — it is a different
        // map, and the report has to be able to say so.
        let tile = MemImages::solid(128, 128, 0x0f0f);
        let mut p = probe(MemImages::new(vec![tile]));
        p.row = Row { url: TILES[0], ..Row::default() };
        p.body.push(&[0x89, b'P', b'N', b'G']);
        p.start_decode();
        p.decode_done(false);

        let row = &p.rows()[0];
        assert_eq!(row.decode_err, 0);
        assert_eq!((row.out_w, row.out_h), (128, 128));
    }

    #[test]
    fn an_empty_body_is_never_handed_to_the_codec() {
        let mut p = probe(MemImages::new(vec![]));
        p.row = Row { url: TILES[0], ..Row::default() };
        p.fetch_done(200, 0);
        let row = &p.rows()[0];
        assert_eq!(row.decode_ms, 0);
        assert_eq!(row.out_w, 0);
    }

    #[test]
    fn a_failed_fetch_moves_to_the_next_tile_instead_of_stopping() {
        let mut p = probe(MemImages::new(vec![]));
        p.row = Row { url: TILES[0], ..Row::default() };
        p.fetch_done(0, symbian_sys::SHIM_ERR_TIMED_OUT);

        // The first row carries the failure that was fed in. The rest are the host's own: with no
        // shim underneath, every `Fetch::start` is refused, and the probe records each refusal and
        // moves on rather than stopping the list — which is the property under test, and the
        // reason the list ends up complete rather than one row long.
        assert_eq!(p.rows()[0].err, symbian_sys::SHIM_ERR_TIMED_OUT);
        assert_eq!(p.rows().len(), TILES.len());
        assert_eq!(p.phase(), Phase::Finished);
    }

    #[test]
    fn the_url_tail_is_what_the_report_shows() {
        assert_eq!(tail("https://tile.openstreetmap.org/15/13209/17118.png"), "15/13209/17118.png");
        assert_eq!(tail("nonsense"), "nonsense");
    }

    #[test]
    fn a_decode_that_never_completes_is_abandoned_with_its_snapshot() {
        // A queued image, so the decode actually starts — with an empty queue `MemImages` refuses
        // and the row is committed before any ceiling could apply.
        let tile = MemImages::solid(TILE_PX, TILE_PX, 0x1234);
        let mut p = probe(MemImages::new(vec![tile]));
        p.row = Row { url: TILES[0], ..Row::default() };
        p.body.push(&[0x89, b'P', b'N', b'G']);
        p.start_decode();
        assert_eq!(p.phase(), Phase::Decoding);

        // No SHIM_EV_IMAGE_DONE ever arrives. The ceiling is what gets the snapshot into the
        // report instead of leaving the probe in Decoding forever.
        for _ in 0..DECODE_TIMEOUT_TICKS {
            p.on_tick();
        }
        assert!(p.rows()[0].abandoned);
    }

    #[test]
    fn an_image_event_for_a_stale_handle_is_ignored() {
        let tile: Image = MemImages::solid(TILE_PX, TILE_PX, 0x1234);
        let mut p = probe(MemImages::new(vec![tile]));
        p.row = Row { url: TILES[0], ..Row::default() };
        p.body.push(&[0x89, b'P', b'N', b'G']);
        p.start_decode();
        let stale = RawEvent {
            kind: symbian_sys::SHIM_EV_IMAGE_DONE,
            handle: 0x7fff,
            ..Default::default()
        };
        p.on_raw(&stale);
        assert!(p.rows().is_empty());
    }
}
