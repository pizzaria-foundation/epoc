//! Which `CImageDecoder` configuration actually decodes on this handset.
//!
//! # Why this exists
//!
//! A photo in the Telegram client opens correctly — one frame, 240x320, a whole JPEG with
//! both markers, measured on the device — and then `Convert` never completes. Five rounds
//! of build → Bluetooth → install → test went into varying one property at a time on a
//! hypothesis the handset then refuted. Each round cost somebody an afternoon and produced
//! one bit of information.
//!
//! `docs/device-notes.md` already says what to do instead:
//!
//! > on a platform with no debugger, no console and no log, **build the instrument instead
//! > of guessing**.
//!
//! This is the instrument. Seven configurations, one install, one report.
//!
//! # Reading the report
//!
//! Row A is the control: exactly what the two shipped Nokia examples do
//! (`sdk/s60cppexamples/OcrExample/src/ImageHandler.cpp` and
//! `sdk/s60cppexamples/OpenGLEx/Utils/Textureutils.cpp`). Every other row changes exactly
//! one thing from A, so a row that behaves differently names its own cause.
//!
//! Every row prints its elapsed milliseconds, because *"a timeout is a measurement of your
//! deadline, not of the system"* — a row that answered in 40 ms and one that spent the
//! whole budget must not print the same line.
//!
//! `impl` is the ECom implementation UID of the plugin that answered. `0x101F45D7` is
//! Symbian's reference JPEG decoder; anything else is a vendor plugin, and row G forces the
//! reference one so the two can be compared directly.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use symbian::fs::{self, ShimFs, Utf16Path};
use symbian_ui::{chrome, App, Canvas, Handled, KeyEvent, Rect, Theme};

/// A baseline JPEG, 240x320, generated on the host so the probe never depends on a photo
/// having been downloaded.
///
/// Deliberately the same shape as the image that hangs, and deliberately *baseline* (SOF0)
/// rather than progressive: a progressive JPEG is a different scan structure that an ICL
/// plugin need not implement, and mixing that question into this one would make every row
/// ambiguous. It is also busy rather than flat — a solid colour compresses to almost
/// nothing and would not exercise the codec the way a photograph does.
static SAMPLE_JPEG: &[u8] = include_bytes!("sample.jpg");

/// Where the sample is written so `FileNewL` has something to open, and where a real photo
/// can be dropped to test the actual failing image.
///
/// The Telegram client writes its last downloaded photo here for exactly that reason. It
/// cannot be read out of that app's private directory instead: the data cage is per-UID and
/// reading another application's needs `AllFiles`, which is not a capability an unsigned
/// package can have.
const INPUT_PATHS: &[&str] = &["C:\\Data\\imgprobe-input.jpg", "C:\\Data\\imgprobe-sample.jpg"];

/// How long a single row may take before it is called stuck rather than slow.
///
/// The whole image is on disk and the E72's own gallery opens one of these in a fraction of
/// a second. Six seconds is an order of magnitude of headroom.
///
/// It is a best effort and not a guarantee, which is the whole reason for
/// [`STATE_PATH`]: a timeout is an active object like any other, so it only fires if the
/// scheduler still services this application. A decoder plugin that live-locks the GUI
/// thread takes the timer down with it.
const ROW_TIMEOUT_MS: i64 = 6_000;

/// How often to look at a running row.
const POLL_MS: i32 = 200;

/// Which row runs next, remembered across launches.
///
/// **One row per launch, and that is the entire design.** The first version ran all seven
/// in one go and got two: row A answered in 252 ms, row B wedged, and everything after it
/// was lost because a decode that hangs the GUI thread hangs the timer that was supposed
/// to give up on it. A probe whose rows can kill each other is not an instrument.
///
/// So the row index lives on disk and is advanced *before* the row is attempted. A row
/// that takes the phone down costs one relaunch and is recorded as `STUCK` by its own
/// absence of a result line — which is the same reasoning `docs/device-notes.md` gives for
/// the report file: "a crash in application code leaves a partial file; a loader failure
/// leaves nothing".
const STATE_PATH: &str = "C:\\Data\\imgprobe-next.txt";

extern "C" {
    fn imgprobe_start(
        config: i32,
        path: *const u16,
        path_len: i32,
        data: *const u8,
        len: i32,
    ) -> i32;
    fn imgprobe_poll(out: *mut i32, cap: i32) -> i32;
    fn imgprobe_stop();
}

/// Slot indices, mirroring `TSlot` in `imgprobe.cpp`.
mod slot {
    pub const OPEN_ERR: usize = 0;
    pub const IMPL_UID: usize = 1;
    pub const FRAMES: usize = 2;
    pub const HEADER_DONE: usize = 3;
    pub const NATIVE_W: usize = 4;
    pub const NATIVE_H: usize = 5;
    pub const FRAME_W: usize = 6;
    pub const FRAME_H: usize = 7;
    pub const FLAGS: usize = 8;
    pub const FRAME_MODE: usize = 9;
    pub const DEST_W: usize = 10;
    pub const DEST_H: usize = 11;
    pub const DEST_MODE: usize = 12;
    pub const PENDING: usize = 13;
    pub const STATUS: usize = 14;
    pub const CONTINUES: usize = 15;
    pub const FRAME_STATE: usize = 16;
    pub const COUNT: usize = 17;
}

/// One row of the matrix. `A` is the control; the `what` of every other row is the single
/// thing it changes from A.
struct Row {
    letter: char,
    what: &'static str,
}

const ROWS: &[Row] = &[
    Row { letter: 'A', what: "control: Nokia examples, frame rect + EColor16M" },
    Row { letter: 'B', what: "dest = iOverallSizeInPixels" },
    Row { letter: 'C', what: "dest mode = EColor64K" },
    Row { letter: 'D', what: "dest = ReducedSize toward 320x240" },
    Row { letter: 'E', what: "EOptionAlwaysThread" },
    Row { letter: 'F', what: "DataNewL instead of FileNewL" },
    Row { letter: 'G', what: "forced reference decoder 0x101F45D7" },
];

pub struct ImgProbe {
    report: String,
    /// Where the report is being written, and a label for the screen.
    path: Option<Utf16Path>,
    path_label: String,
    /// The image every row decodes.
    input: Option<Utf16Path>,
    input_label: String,
    /// Which row is running, or `ROWS.len()` once every row is done.
    row: usize,
    /// When the running row started, in monotonic microseconds.
    started_us: u64,
    /// The repeating poll timer.
    timer: Option<i32>,
    /// The last few report lines, for the screen.
    screen: Vec<String>,
    done: bool,
}

impl ImgProbe {
    pub fn new() -> Self {
        let mut p = ImgProbe {
            report: String::new(),
            path: None,
            path_label: String::new(),
            input: None,
            input_label: String::new(),
            row: 0,
            started_us: 0,
            timer: None,
            screen: Vec::new(),
            done: false,
        };
        p.begin();
        p
    }

    /// Everything before the first row: choose a sink, write the sample, start row A.
    ///
    /// The report is flushed here, before any decode is attempted, so that an absent file
    /// is a finding rather than an ambiguity — `device-notes.md` again: a crash in
    /// application code leaves a partial file, a loader failure leaves nothing at all.
    fn begin(&mut self) {
        let mut fs = ShimFs;
        self.open_output(&mut fs);

        // Which row this launch runs, and the *next* one recorded before anything is
        // attempted. Advancing first is what makes a row that takes the phone down cost one
        // relaunch instead of trapping the probe on it forever.
        self.row = read_next_row(&mut fs);
        write_next_row(&mut fs, self.row + 1);

        // Everything already written stays: the report accumulates across launches, because
        // each launch only contributes one row.
        self.report = read_report(&mut fs, self.path.as_ref());
        if self.report.is_empty() {
            self.line("imgprobe: which CImageDecoder configuration decodes on this handset");
            self.line("");
            self.line("One row per launch. Reopen the app for the next row; a row with no");
            self.line("result line under it is one that took the application down.");
            self.line("");
        }

        if self.row >= ROWS.len() {
            self.line("");
            self.line("every row attempted. delete imgprobe-next.txt to run them again.");
            self.line("TFrameInfoState: 0 uninit, 1 header, 2 frame, 3 complete.");
            self.line("TDisplayMode: 7 EColor64K, 8 EColor16M, 9 EColor16MU, 10 EColor16MA.");
            self.done = true;
            self.flush(&mut fs);
            return;
        }

        // The sample first, so there is always something to decode; then a real photo if
        // one has been left for us, because "the shim is wrong" and "this image is the
        // problem" are different findings.
        self.write_sample(&mut fs);
        self.flush(&mut fs);

        self.start_row();
    }

    fn open_output(&mut self, fs: &mut ShimFs) {
        // C:\Data\ first: it is writable with WriteUserData and visible in File Manager,
        // which is what makes the report carryable off the phone over Bluetooth. E: would
        // also appear over USB but a handset with no card makes it a dead end.
        for c in ["C:\\Data\\imgprobe.txt", "E:\\imgprobe.txt", "C:\\imgprobe.txt"] {
            let Ok(p) = Utf16Path::new(c) else { continue };
            // Reading first, and only writing when there is nothing there.
            //
            // The obvious writability test is an empty write, and here it would have been a
            // bug with no symptom: the report now spans launches, so truncating it to probe
            // the path would throw away every row already recorded — and the file would
            // still look plausible, holding exactly the last row run.
            if fs::read(fs, &p).is_ok() || fs::write_atomic(fs, &p, b"").is_ok() {
                self.path = Some(p);
                self.path_label = String::from(c);
                return;
            }
        }
        if let Ok(dir) = fs::private_path(fs) {
            if let Ok(p) = Utf16Path::join(dir.as_units(), "imgprobe.txt") {
                if fs::read(fs, &p).is_ok() || fs::write_atomic(fs, &p, b"").is_ok() {
                    self.path = Some(p);
                    self.path_label = String::from("(private dir - not reachable over USB)");
                }
            }
        }
    }

    /// Put an image on disk for `FileNewL` to open, preferring a real one.
    fn write_sample(&mut self, fs: &mut ShimFs) {
        // A photo left by the Telegram client wins: it is the image that actually fails.
        if let Ok(p) = Utf16Path::new(INPUT_PATHS[0]) {
            if let Ok(Some(bytes)) = fs::read(fs, &p) {
                if bytes.len() > 4 {
                    self.input = Some(p);
                    let mut s = String::from(INPUT_PATHS[0]);
                    s.push_str(" (");
                    push_i64(&mut s, bytes.len() as i64);
                    s.push_str(" bytes, from the client)");
                    self.input_label = s;
                    return;
                }
            }
        }
        // Otherwise the built-in one, which needs writing out first.
        if let Ok(p) = Utf16Path::new(INPUT_PATHS[1]) {
            if fs::write_atomic(fs, &p, SAMPLE_JPEG).is_ok() {
                self.input = Some(p);
                let mut s = String::from(INPUT_PATHS[1]);
                s.push_str(" (");
                push_i64(&mut s, SAMPLE_JPEG.len() as i64);
                s.push_str(" bytes, built in)");
                self.input_label = s;
                return;
            }
        }
        self.input_label = String::from("NONE - could not write an image to decode");
    }

    fn start_row(&mut self) {
        let Some(path) = self.input.clone() else {
            self.line("  FAIL  no image to decode");
            self.finish();
            return;
        };

        // The breadcrumb goes in *before* the row runs and is flushed, so a row that takes
        // the whole application down names itself.
        let r = &ROWS[self.row];
        let mut head = String::from("-- entering row ");
        head.push(r.letter);
        head.push_str("  ");
        head.push_str(r.what);
        self.line(&head);
        // Which image, on every row: a row that ran against the real photo and one that ran
        // against the built-in sample are different measurements, and a report that does
        // not say which is a report that cannot be compared with the next one.
        let img = self.input_label.clone();
        self.kv("image", &img);
        let mut fs = ShimFs;
        self.flush(&mut fs);

        self.started_us = symbian::monotonic_us();
        let units = path.as_units();
        let rc = unsafe {
            imgprobe_start(
                self.row as i32,
                units.as_ptr(),
                units.len() as i32,
                SAMPLE_JPEG.as_ptr(),
                SAMPLE_JPEG.len() as i32,
            )
        };
        // A row that will not even open still gets polled once, so its numbers are written
        // the same way every other row's are.
        let _ = rc;
        if self.timer.is_none() {
            self.timer = symbian::timer_after(POLL_MS).ok();
        }
    }

    /// Look at the running row; write it down when it settles or when it runs out of time.
    fn poll_row(&mut self) {
        let mut v = [0i32; slot::COUNT];
        let n = unsafe { imgprobe_poll(v.as_mut_ptr(), v.len() as i32) };
        if n <= 0 {
            self.timer = symbian::timer_after(POLL_MS).ok();
            return;
        }

        let elapsed_ms = ((symbian::monotonic_us() - self.started_us) / 1000) as i64;
        let pending = v[slot::PENDING] == 1;
        let opened = v[slot::OPEN_ERR] == 0;
        let timed_out = elapsed_ms >= ROW_TIMEOUT_MS;

        if pending && !timed_out {
            self.timer = symbian::timer_after(POLL_MS).ok();
            return;
        }

        self.write_row(&v, elapsed_ms, pending, opened, timed_out);

        unsafe { imgprobe_stop() };
        self.finish();
    }

    fn write_row(&mut self, v: &[i32], elapsed_ms: i64, pending: bool, opened: bool, to: bool) {
        let r = &ROWS[self.row];

        // The verdict first and fixed-width, so the file can be grepped and skimmed.
        let verdict = if !opened {
            "FAIL "
        } else if to || pending {
            "STUCK"
        } else if v[slot::STATUS] == 0 {
            "  ok "
        } else {
            "FAIL "
        };

        let mut s = String::new();
        s.push_str(verdict);
        s.push(' ');
        s.push(r.letter);
        s.push_str("  ");
        push_i64(&mut s, elapsed_ms);
        s.push_str("ms  ");
        if !opened {
            s.push_str("open=");
            push_i64(&mut s, v[slot::OPEN_ERR] as i64);
        } else if to || pending {
            s.push_str("never completed");
        } else {
            s.push_str("status=");
            push_i64(&mut s, v[slot::STATUS] as i64);
            if v[slot::CONTINUES] > 0 {
                s.push_str(" cont=");
                push_i64(&mut s, v[slot::CONTINUES] as i64);
            }
        }
        self.line(&s);

        if opened {
            let mut d = String::from("       impl=0x");
            push_hex(&mut d, v[slot::IMPL_UID] as u32);
            d.push_str(if v[slot::IMPL_UID] == 0x101F_45D7 { " (reference)" } else { " (vendor)" });
            d.push_str("  frames=");
            push_i64(&mut d, v[slot::FRAMES] as i64);
            d.push_str(" hdr=");
            push_i64(&mut d, v[slot::HEADER_DONE] as i64);
            self.line(&d);

            let mut g = String::from("       native=");
            push_i64(&mut g, v[slot::NATIVE_W] as i64);
            g.push('x');
            push_i64(&mut g, v[slot::NATIVE_H] as i64);
            g.push_str(" frame=");
            push_i64(&mut g, v[slot::FRAME_W] as i64);
            g.push('x');
            push_i64(&mut g, v[slot::FRAME_H] as i64);
            g.push_str(" dest=");
            push_i64(&mut g, v[slot::DEST_W] as i64);
            g.push('x');
            push_i64(&mut g, v[slot::DEST_H] as i64);
            self.line(&g);

            let mut m = String::from("       flags=0x");
            push_hex(&mut m, v[slot::FLAGS] as u32);
            m.push_str(" framemode=");
            push_i64(&mut m, v[slot::FRAME_MODE] as i64);
            m.push_str(" destmode=");
            push_i64(&mut m, v[slot::DEST_MODE] as i64);
            m.push_str(" state=");
            push_i64(&mut m, v[slot::FRAME_STATE] as i64);
            self.line(&m);
        }
    }

    fn finish(&mut self) {
        if let Some(h) = self.timer.take() {
            symbian::timer_cancel(h);
        }
        // How many rows are left, not "done".
        //
        // It said "done" after a single row, which is exactly the kind of instrument error
        // `device-notes.md` warns about: "the probe collapsed iModifiers into three bits"
        // — a report that claims more than it measured. One row finishing is not the matrix
        // finishing, and the difference decides whether anyone reopens the app.
        let remaining = ROWS.len().saturating_sub(self.row + 1);
        let mut s = String::from("       reopen imgprobe for the next row (");
        push_i64(&mut s, remaining as i64);
        s.push_str(" left)");
        self.line(&s);
        self.done = true;
        let mut fs = ShimFs;
        self.flush(&mut fs);
    }

    fn line(&mut self, s: &str) {
        self.report.push_str(s);
        self.report.push('\n');
        // The screen keeps only the tail; the file has everything.
        self.screen.push(String::from(s));
        if self.screen.len() > 12 {
            self.screen.remove(0);
        }
    }

    fn kv(&mut self, k: &str, v: &str) {
        let mut s = String::from("  .    ");
        s.push_str(k);
        s.push_str(": ");
        s.push_str(v);
        self.line(&s);
    }

    /// Rewrite the whole file. Called after every row, so a run that dies leaves a report
    /// naming the row it died in.
    fn flush(&mut self, fs: &mut ShimFs) {
        if let Some(p) = &self.path {
            let _ = fs::write_atomic(fs, p, self.report.as_bytes());
        }
    }
}

impl Default for ImgProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl App for ImgProbe {
    fn title(&self) -> &str {
        "imgprobe"
    }

    fn handle_key(&mut self, _ev: KeyEvent, _t: &Theme<'_>, _s: Rect) -> Handled {
        Handled::Ignored
    }

    fn handle_raw(&mut self, ev: &symbian_ui::RawEvent) -> Handled {
        if ev.kind == symbian_sys::SHIM_EV_TIMER && Some(ev.handle) == self.timer {
            self.timer = None;
            if !self.done {
                self.poll_row();
            }
            return Handled::Consumed;
        }
        Handled::Ignored
    }

    fn draw(&mut self, c: &mut Canvas<'_>, theme: &Theme<'_>) {
        let screen = Rect::from_size(c.size());
        let frame = symbian_ui::Frame::split(screen, theme, true, true);
        chrome::clear(c, theme);
        chrome::title_bar(c, frame.title, theme, "imgprobe", None);
        chrome::softkey_bar(c, frame.softkeys, theme, [None, None, Some("Sair")]);

        let small = theme.fonts.small;
        let mut y = frame.content.y0 + 2;
        for l in &self.screen {
            c.draw_text(
                symbian_ui::Point::new(frame.content.x0 + 2, y + small.ascent()),
                l,
                small,
                theme.palette.text,
            );
            y += small.line_height();
        }
    }
}

/// The row index left by the previous launch, or 0.
///
/// Stored as decimal text rather than a byte: a report directory a human is going to read
/// should not contain a file whose contents are invisible.
fn read_next_row(fs: &mut ShimFs) -> usize {
    let Ok(p) = Utf16Path::new(STATE_PATH) else { return 0 };
    let Ok(Some(bytes)) = fs::read(fs, &p) else { return 0 };
    let mut n = 0usize;
    let mut any = false;
    for b in bytes {
        if b.is_ascii_digit() {
            n = n.saturating_mul(10).saturating_add((b - b'0') as usize);
            any = true;
        } else if any {
            break;
        }
    }
    n
}

fn write_next_row(fs: &mut ShimFs, row: usize) {
    let Ok(p) = Utf16Path::new(STATE_PATH) else { return };
    let mut s = String::new();
    push_i64(&mut s, row as i64);
    s.push('\n');
    let _ = fs::write_atomic(fs, &p, s.as_bytes());
}

/// Whatever previous launches wrote, so this one can append its single row.
fn read_report(fs: &mut ShimFs, path: Option<&Utf16Path>) -> String {
    let Some(p) = path else { return String::new() };
    match fs::read(fs, p) {
        Ok(Some(bytes)) => match core::str::from_utf8(&bytes) {
            Ok(s) => String::from(s),
            Err(_) => String::new(),
        },
        _ => String::new(),
    }
}

fn push_i64(s: &mut String, mut v: i64) {
    if v < 0 {
        s.push('-');
        v = -v;
    }
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    for b in &buf[i..] {
        s.push(*b as char);
    }
}

fn push_hex(s: &mut String, v: u32) {
    const D: &[u8; 16] = b"0123456789abcdef";
    let mut started = false;
    for shift in (0..8).rev() {
        let nib = ((v >> (shift * 4)) & 0xF) as usize;
        if nib != 0 || started || shift == 0 {
            started = true;
            s.push(D[nib] as char);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sample_is_a_whole_baseline_jpeg() {
        // The probe's control image has to be above suspicion, or a row that fails on it
        // proves nothing. SOI and EOI mean whole; SOF0 rather than SOF2 means baseline,
        // and mixing progressive JPEG into this question would make every row ambiguous.
        assert_eq!(&SAMPLE_JPEG[..2], &[0xFF, 0xD8], "no SOI");
        assert_eq!(&SAMPLE_JPEG[SAMPLE_JPEG.len() - 2..], &[0xFF, 0xD9], "no EOI");

        let mut i = 2;
        let mut sof = None;
        while i + 3 < SAMPLE_JPEG.len() {
            if SAMPLE_JPEG[i] != 0xFF {
                i += 1;
                continue;
            }
            let m = SAMPLE_JPEG[i + 1];
            if matches!(m, 0xC0 | 0xC1 | 0xC2) {
                sof = Some(m);
                break;
            }
            if m == 0xDA {
                break;
            }
            let seg = u16::from_be_bytes([SAMPLE_JPEG[i + 2], SAMPLE_JPEG[i + 3]]) as usize;
            i += 2 + seg;
        }
        assert_eq!(sof, Some(0xC0), "must be baseline (SOF0), not progressive (SOF2)");
    }

    #[test]
    fn every_row_changes_one_thing_and_says_which() {
        // The matrix is only readable if each row names its single difference from the
        // control — otherwise a row that behaves differently does not identify a cause.
        assert_eq!(ROWS[0].letter, 'A');
        assert!(ROWS[0].what.contains("control"));
        for r in &ROWS[1..] {
            assert!(!r.what.is_empty(), "row {} has no description", r.letter);
            assert!(!r.what.contains("control"));
        }
    }

    #[test]
    fn numbers_format_without_core_fmt() {
        let mut s = String::new();
        push_i64(&mut s, 0);
        push_i64(&mut s, -42);
        assert_eq!(s, "0-42");

        let mut h = String::new();
        push_hex(&mut h, 0x101F_45D7);
        assert_eq!(h, "101f45d7");

        let mut z = String::new();
        push_hex(&mut z, 0);
        assert_eq!(z, "0");
    }
}
