//! Which WAV formats this handset will actually play, and how fast it opens them.
//!
//! # Why this exists
//!
//! A Telegram voice message is Ogg/Opus, which nothing on this device can open — the
//! platform's format list (`mmf/common/mmffourcc.h`) ends at AMR, AAC and MP3. So the
//! plan is to decode Opus in Rust and hand the platform a WAV file. That plan rests on
//! assumptions the documentation does not settle, and each one changes the work:
//!
//! - **Does 48 kHz play?** Opus always decodes at 48 kHz regardless of what was encoded.
//!   If the handset plays 48 kHz mono, the decoded samples go straight to disk. If it
//!   does not, a resampler has to be written and every voice message pays for it. Both
//!   shipped SDK examples only ever use 8 kHz, so the answer is not in the examples.
//! - **Will the media framework read from our private data cage?** The controller runs
//!   in a subthread MMF creates. Same process, so it should — but "should" is what the
//!   image decoder taught us to stop trusting. If it will not, the WAV has to be written
//!   somewhere `WriteUserData` reaches and left visible to the user.
//! - **What does opening cost?** A voice message plays after a decode and a file write.
//!   If open alone is a second, the interface needs to say so.
//!
//! # Why this probe has no C++ of its own
//!
//! `imgprobe` needed some, because its matrix varied things the shim ABI deliberately
//! does not expose. This matrix varies sample rate, channel count and directory — bytes
//! in a file and a path. So it drives the real `shim_audio.cpp` rather than a copy, and
//! what it measures is the code that will ship.
//!
//! # Reading the report
//!
//! Row A is the control: 8 kHz mono, the only configuration any shipped SDK example
//! uses. Every other row changes one thing. Each row plays about a second of a tone at a
//! different pitch, so a row that reports success but makes no sound is distinguishable
//! from one that played — **the report cannot hear**, and that is the one measurement
//! this instrument cannot take for itself.
//!
//! One row per launch, for the reason `imgprobe` learned the hard way: a row that takes
//! the application down must cost one relaunch, not the rest of the matrix.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use symbian::fs::{self, ShimFs, Utf16Path};
use symbian_ui::{chrome, App, Canvas, Handled, KeyEvent, Rect, Theme};

/// Where the report lands. Same reasoning as `imgprobe`: `C:\Data\` is writable with
/// `WriteUserData` and visible in File Manager, which is what makes it carryable off the
/// phone over Bluetooth.
const REPORT_PATHS: &[&str] = &["C:\\Data\\audioprobe.txt", "E:\\audioprobe.txt"];

/// Which row runs next, remembered across launches. See `imgprobe`'s equivalent — the
/// index is advanced *before* the row is attempted, so a row that wedges the handset
/// costs one relaunch rather than trapping the probe on it forever.
const STATE_PATH: &str = "C:\\Data\\audioprobe-next.txt";

/// How long a row may take to open and finish before it is called stuck.
///
/// A one-second clip that has to open, play and report has an order of magnitude of
/// headroom here. As in `imgprobe` this is best effort: the timeout is an active object,
/// so it only fires while the scheduler still services this application.
const ROW_TIMEOUT_MS: i64 = 8_000;

const POLL_MS: i32 = 250;

/// How much audio each row plays. Long enough to hear and recognise, short enough that
/// sitting through the whole matrix is not a chore.
const CLIP_MS: u32 = 900;

/// One row of the matrix.
struct Row {
    letter: char,
    rate: u32,
    channels: u16,
    /// Hz of the test tone. Distinct per row so the ear can tell which one is playing —
    /// and, more usefully, so a row playing at the *wrong* pitch reveals a sample-rate
    /// mismatch that the platform would otherwise report as success.
    tone_hz: u32,
    /// Write the clip into the app's private directory instead of `C:\Data\`.
    private: bool,
    what: &'static str,
}

/// Row A is the control — 8 kHz mono is what both shipped SDK examples use, and the only
/// configuration with any on-disk precedent. Everything after it changes one thing.
const ROWS: &[Row] = &[
    Row {
        letter: 'A',
        rate: 8_000,
        channels: 1,
        tone_hz: 440,
        private: false,
        what: "control: 8kHz mono, what both SDK examples use",
    },
    Row {
        letter: 'B',
        rate: 48_000,
        channels: 1,
        tone_hz: 660,
        private: false,
        what: "48kHz mono - Opus decodes at 48k, so this decides the resampler",
    },
    Row {
        letter: 'C',
        rate: 16_000,
        channels: 1,
        tone_hz: 880,
        private: false,
        what: "16kHz mono - the fallback rate if B fails",
    },
    Row {
        letter: 'D',
        rate: 48_000,
        channels: 1,
        tone_hz: 660,
        private: true,
        what: "same as B, from the private data cage instead of C:\\Data",
    },
    Row {
        letter: 'E',
        rate: 44_100,
        channels: 2,
        tone_hz: 550,
        private: false,
        what: "44.1kHz stereo - the one rate every device is expected to have",
    },
];

/// What the current row is waiting for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Opening,
    Playing,
    Settled,
}

pub struct AudioProbe {
    report: String,
    path: Option<Utf16Path>,
    row: usize,
    phase: Phase,
    /// When the row started, and when the open completed, in monotonic microseconds.
    started_us: u64,
    opened_us: u64,
    /// What the platform reported, filled in as the events arrive.
    open_status: i32,
    reported_ms: i32,
    play_status: i32,
    play_raw: i32,
    /// The furthest playback position seen. A clip that reports success without the
    /// position ever moving did not play, and that is a different finding from a failure.
    max_pos_ms: i32,
    clip_label: String,
    timer: Option<i32>,
    screen: Vec<String>,
    done: bool,
}

impl AudioProbe {
    pub fn new() -> Self {
        let mut p = AudioProbe {
            report: String::new(),
            path: None,
            row: 0,
            phase: Phase::Settled,
            started_us: 0,
            opened_us: 0,
            open_status: 0,
            reported_ms: 0,
            play_status: 0,
            play_raw: 0,
            max_pos_ms: 0,
            clip_label: String::new(),
            timer: None,
            screen: Vec::new(),
            done: false,
        };
        p.begin();
        p
    }

    fn begin(&mut self) {
        let mut fs = ShimFs;
        self.open_output(&mut fs);

        self.row = read_next_row(&mut fs);
        write_next_row(&mut fs, self.row + 1);

        self.report = read_report(&mut fs, self.path.as_ref());
        if self.report.is_empty() {
            self.line("audioprobe: which WAV formats this handset plays");
            self.line("");
            self.line("One row per launch. Each row plays about a second of a tone at its");
            self.line("own pitch - LISTEN, because a row can report success and be silent,");
            self.line("and that is the one thing this report cannot measure for itself.");
            self.line("");
        }

        if self.row >= ROWS.len() {
            self.line("");
            self.line("every row attempted. delete audioprobe-next.txt to run them again.");
            self.line("status -18 not ready, -14 in use, -5 not supported, -1 not found.");
            self.done = true;
            self.flush(&mut fs);
            return;
        }

        self.flush(&mut fs);
        self.start_row();
    }

    fn open_output(&mut self, fs: &mut ShimFs) {
        for c in REPORT_PATHS {
            let Ok(p) = Utf16Path::new(c) else { continue };
            // Read first, write only when absent: the report spans launches, so probing
            // writability with an empty write would discard every row already recorded.
            if fs::read(fs, &p).is_ok() || fs::write_atomic(fs, &p, b"").is_ok() {
                self.path = Some(p);
                return;
            }
        }
        if let Ok(dir) = fs::private_path(fs) {
            if let Ok(p) = Utf16Path::join(dir.as_units(), "audioprobe.txt") {
                if fs::read(fs, &p).is_ok() || fs::write_atomic(fs, &p, b"").is_ok() {
                    self.path = Some(p);
                }
            }
        }
    }

    /// Write this row's clip and ask the platform to open it.
    fn start_row(&mut self) {
        let r = &ROWS[self.row];

        // The breadcrumb goes in before anything is attempted and is flushed immediately,
        // so a row that takes the application down names itself by having no result under
        // it. Same rule as imgprobe.
        let mut head = String::from("-- entering row ");
        head.push(r.letter);
        head.push_str("  ");
        head.push_str(r.what);
        self.line(&head);

        let mut fs = ShimFs;
        let Some(clip) = self.write_clip(&mut fs, r) else {
            self.line("  FAIL  could not write the clip");
            self.finish();
            return;
        };
        let label = self.clip_label.clone();
        self.kv("clip", &label);
        self.flush(&mut fs);

        self.started_us = symbian::monotonic_us();
        self.phase = Phase::Opening;
        let units = clip.as_units();
        self.open_status =
            unsafe { symbian_sys::shim_audio_open_file(units.as_ptr(), units.len() as i32) };
        if self.open_status != 0 {
            // A synchronous failure means no event is coming, so settle now rather than
            // spending the timeout waiting for one.
            self.phase = Phase::Settled;
            self.write_row(0, false);
            self.finish();
            return;
        }
        self.timer = symbian::timer_after(POLL_MS).ok();
    }

    /// Generate this row's WAV and put it where the row says.
    fn write_clip(&mut self, fs: &mut ShimFs, r: &Row) -> Option<Utf16Path> {
        let frames = r.rate * CLIP_MS / 1000;
        let mut samples: Vec<i16> = Vec::with_capacity((frames * r.channels as u32) as usize);
        for i in 0..frames {
            let v = square(i, r.rate, r.tone_hz);
            for _ in 0..r.channels {
                samples.push(v);
            }
        }
        let wav = wav_file(r.rate, r.channels, &samples);

        let p = if r.private {
            let dir = fs::private_path(fs).ok()?;
            Utf16Path::join(dir.as_units(), "audioprobe-clip.wav").ok()?
        } else {
            Utf16Path::new("C:\\Data\\audioprobe-clip.wav").ok()?
        };
        fs::write_atomic(fs, &p, &wav).ok()?;

        let mut s = String::new();
        push_i64(&mut s, r.rate as i64);
        s.push_str("Hz ");
        push_i64(&mut s, r.channels as i64);
        s.push_str("ch ");
        push_i64(&mut s, wav.len() as i64);
        s.push_str(" bytes, tone ");
        push_i64(&mut s, r.tone_hz as i64);
        s.push_str("Hz, ");
        s.push_str(if r.private { "private dir" } else { "C:\\Data" });
        self.clip_label = s;
        Some(p)
    }

    fn on_opened(&mut self, status: i32, duration_ms: i32) {
        if self.phase != Phase::Opening {
            return;
        }
        self.opened_us = symbian::monotonic_us();
        self.open_status = status;
        self.reported_ms = duration_ms;
        if status != 0 {
            self.phase = Phase::Settled;
            self.write_row(self.elapsed_ms(), false);
            self.finish();
            return;
        }
        self.phase = Phase::Playing;
        // Half volume: loud enough to hear across a room, quiet enough that running the
        // matrix in an office is not an event.
        let _ = unsafe { symbian_sys::shim_audio_set_volume(50) };
        let _ = unsafe { symbian_sys::shim_audio_play() };
    }

    fn on_done(&mut self, status: i32, raw: i32) {
        if self.phase != Phase::Playing {
            return;
        }
        self.phase = Phase::Settled;
        self.play_status = status;
        self.play_raw = raw;
        self.write_row(self.elapsed_ms(), false);
        self.finish();
    }

    fn poll(&mut self) {
        // The position is sampled rather than waited on, because a clip that reports a
        // clean finish without the position ever having moved is a distinct outcome:
        // the platform accepted the file and produced no sound.
        if self.phase == Phase::Playing {
            let pos = unsafe { symbian_sys::shim_audio_position_ms() };
            if pos > self.max_pos_ms {
                self.max_pos_ms = pos;
            }
        }
        if self.elapsed_ms() >= ROW_TIMEOUT_MS {
            let _ = unsafe { symbian_sys::shim_audio_stop() };
            self.phase = Phase::Settled;
            self.write_row(self.elapsed_ms(), true);
            self.finish();
            return;
        }
        self.timer = symbian::timer_after(POLL_MS).ok();
    }

    fn elapsed_ms(&self) -> i64 {
        ((symbian::monotonic_us() - self.started_us) / 1000) as i64
    }

    fn write_row(&mut self, elapsed_ms: i64, timed_out: bool) {
        let r = &ROWS[self.row];
        let open_ms = if self.opened_us > self.started_us {
            ((self.opened_us - self.started_us) / 1000) as i64
        } else {
            0
        };

        let verdict = if timed_out {
            "STUCK"
        } else if self.open_status != 0 || self.play_status != 0 {
            "FAIL "
        } else if self.max_pos_ms == 0 {
            // Accepted and silent. Worth its own word, because "ok" here would be a
            // report claiming more than it measured.
            "MUTE?"
        } else {
            "  ok "
        };

        let mut s = String::from(verdict);
        s.push(' ');
        s.push(r.letter);
        s.push_str("  ");
        push_i64(&mut s, elapsed_ms);
        s.push_str("ms  open=");
        push_i64(&mut s, self.open_status as i64);
        s.push_str(" in ");
        push_i64(&mut s, open_ms);
        s.push_str("ms");
        if !timed_out && self.open_status == 0 {
            s.push_str("  play=");
            push_i64(&mut s, self.play_status as i64);
            if self.play_raw != self.play_status {
                s.push_str(" (raw ");
                push_i64(&mut s, self.play_raw as i64);
                s.push(')');
            }
        }
        self.line(&s);

        let mut d = String::from("       duration: platform said ");
        push_i64(&mut d, self.reported_ms as i64);
        d.push_str("ms, clip is ");
        push_i64(&mut d, CLIP_MS as i64);
        d.push_str("ms, position reached ");
        push_i64(&mut d, self.max_pos_ms as i64);
        d.push_str("ms");
        self.line(&d);

        // A duration far from the clip's own is the signature of a sample rate the
        // platform silently substituted, which is exactly the failure that plays at the
        // wrong pitch instead of failing.
        if self.open_status == 0 && self.reported_ms > 0 {
            let ratio = (self.reported_ms as i64 * 100) / CLIP_MS as i64;
            if !(80..=125).contains(&ratio) {
                let mut w = String::from("       ! duration is ");
                push_i64(&mut w, ratio);
                w.push_str("% of the clip - the rate was probably substituted");
                self.line(&w);
            }
        }
    }

    fn finish(&mut self) {
        if let Some(h) = self.timer.take() {
            symbian::timer_cancel(h);
        }
        let _ = unsafe { symbian_sys::shim_audio_close() };

        let remaining = ROWS.len().saturating_sub(self.row + 1);
        let mut s = String::from("       reopen audioprobe for the next row (");
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

    fn flush(&mut self, fs: &mut ShimFs) {
        if let Some(p) = &self.path {
            let _ = fs::write_atomic(fs, p, self.report.as_bytes());
        }
    }
}

impl Default for AudioProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl App for AudioProbe {
    fn title(&self) -> &str {
        "audioprobe"
    }

    fn handle_key(&mut self, _ev: KeyEvent, _t: &Theme<'_>, _s: Rect) -> Handled {
        Handled::Ignored
    }

    fn handle_raw(&mut self, ev: &symbian_ui::RawEvent) -> Handled {
        if self.done {
            return Handled::Ignored;
        }
        match ev.kind {
            symbian_sys::SHIM_EV_TIMER if Some(ev.handle) == self.timer => {
                self.timer = None;
                self.poll();
                Handled::Consumed
            }
            symbian_sys::SHIM_EV_AUDIO_OPENED => {
                self.on_opened(ev.status, ev.a);
                Handled::Consumed
            }
            symbian_sys::SHIM_EV_AUDIO_DONE => {
                self.on_done(ev.status, ev.d);
                Handled::Consumed
            }
            _ => Handled::Ignored,
        }
    }

    fn draw(&mut self, c: &mut Canvas<'_>, theme: &Theme<'_>) {
        let screen = Rect::from_size(c.size());
        let frame = symbian_ui::Frame::split(screen, theme, true, true);
        chrome::clear(c, theme);
        chrome::title_bar(c, frame.title, theme, "audioprobe", None);
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

/// A square wave, as integers.
///
/// Square rather than sine because there is no sine here to call: this target is
/// soft-float and `libm` is not linked, so a sine would mean a table. A square wave is
/// two branches, is unmistakable through a phone speaker, and carries its pitch just as
/// clearly — and pitch is the measurement, since a wrong pitch is how a substituted
/// sample rate shows itself.
fn square(i: u32, rate: u32, hz: u32) -> i16 {
    if hz == 0 || rate == 0 {
        return 0;
    }
    let period = rate / hz.max(1);
    if period == 0 {
        return 0;
    }
    // A quarter of full scale. Full scale through a small speaker is distortion, and
    // distortion is exactly what would make a wrong-pitch row hard to identify.
    if (i % period) * 2 < period {
        8_000
    } else {
        -8_000
    }
}

/// A RIFF/WAVE file of signed little-endian 16-bit PCM.
///
/// Deliberately a local copy of `symbian_audio::wav` rather than a dependency on it: this
/// probe exists to find out whether the platform accepts a file of this shape, and importing
/// the SDK's writer would make a passing probe evidence about the SDK rather than about the
/// platform. The two are checked against each other in the tests below.
fn wav_file(rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
    let block = channels * 16 / 8;
    let data_len = (samples.len() * 2) as u32;
    let mut out: Vec<u8> = Vec::with_capacity(44 + samples.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * block as u32).to_le_bytes());
    out.extend_from_slice(&block.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

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
    let mut n = 0;
    loop {
        buf[n] = b'0' + (v % 10) as u8;
        n += 1;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    while n > 0 {
        n -= 1;
        s.push(buf[n] as char);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_a_is_the_configuration_the_shipped_examples_use() {
        // If the control drifts from what has on-disk precedent, every other row loses
        // its reference and the matrix stops being able to name a cause.
        assert_eq!(ROWS[0].letter, 'A');
        assert_eq!(ROWS[0].rate, 8_000);
        assert_eq!(ROWS[0].channels, 1);
    }

    #[test]
    fn every_row_has_a_distinct_pitch_or_a_distinct_source() {
        // Two rows that sound identical cannot be told apart by the one measurement the
        // report cannot take: whether sound came out. D deliberately repeats B's tone
        // because it varies the directory instead, so the pair is allowed.
        for (i, a) in ROWS.iter().enumerate() {
            for b in &ROWS[i + 1..] {
                assert!(
                    a.tone_hz != b.tone_hz || a.private != b.private,
                    "rows {} and {} are indistinguishable by ear",
                    a.letter,
                    b.letter
                );
            }
        }
    }

    #[test]
    fn the_matrix_asks_the_question_that_decides_the_resampler() {
        // 48 kHz mono is what Opus decodes to. Without a row for it the probe would come
        // back without the one answer that changes the amount of work left.
        assert!(ROWS.iter().any(|r| r.rate == 48_000 && r.channels == 1 && !r.private));
    }

    #[test]
    fn the_probes_wav_writer_agrees_with_the_clients() {
        // The copy is deliberate — see wav_file — but a copy that has drifted would make
        // a passing probe evidence about a file the client never writes.
        let samples = [1i16, -2, 3, -4];
        assert_eq!(wav_file(48_000, 1, &samples), symbian_audio::wav::file(48_000, 1, &samples));
        assert_eq!(wav_file(44_100, 2, &samples), symbian_audio::wav::file(44_100, 2, &samples));
    }

    #[test]
    fn the_tone_is_a_square_wave_at_the_pitch_asked_for() {
        // Counting sign changes recovers the frequency, which is what makes a row played
        // at the wrong rate audible as the wrong note.
        let rate = 48_000;
        let hz = 600;
        let n = rate;
        let mut crossings: u32 = 0;
        let mut prev = square(0, rate, hz);
        for i in 1..n {
            let v = square(i, rate, hz);
            if (v > 0) != (prev > 0) {
                crossings += 1;
            }
            prev = v;
        }
        // Two crossings per cycle, over exactly one second.
        assert!((crossings / 2).abs_diff(hz) <= 2, "got {} Hz", crossings / 2);
    }

    #[test]
    fn a_clip_is_the_length_it_claims() {
        let r = &ROWS[1];
        let frames = r.rate * CLIP_MS / 1000;
        assert_eq!(frames, 43_200);
        let wav = wav_file(r.rate, r.channels, &alloc::vec![0i16; frames as usize]);
        assert_eq!(wav.len(), 44 + frames as usize * 2);
    }
}
