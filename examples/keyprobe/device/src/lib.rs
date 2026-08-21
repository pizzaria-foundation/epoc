//! Shows the raw key data the window server hands us, one line per event.
//!
//! # Why this exists
//!
//! The E72's QWERTY prints digits on some letter keys, and pressing one of those
//! was reported as producing the digit. Three layers could be responsible:
//!
//! 1. the window server's own key translation, which produces `iCode` from the
//!    physical key plus the modifier state;
//! 2. a modifier we are misreading — `EModifierFunc` reported as held when it is
//!    not would make the whole keyboard type its Fn layer;
//! 3. the shim's own mapping table, which could be matching a digit's key code.
//!
//! Each of those needs a different fix and they are indistinguishable from the
//! outside. So this prints all three inputs — `iCode`, `iScanCode`, `iModifiers` —
//! and lets the device answer the question instead of being guessed at.
//!
//! Read it like this: press `Q`. If `code` is 0x0071 the window server produced the
//! letter and the fault is downstream of it. If `code` is 0x0031 the window server
//! produced the digit, and `mods` then says whether it did so because it believes a
//! modifier is held.

#![no_std]
#![no_main]

extern crate alloc;

use symbian_app::symbian_sys as sys;
use symbian_ui::{App, Canvas, Handled, KeyEvent, Rect, RawEvent, Theme};

symbian_app::entry!(KeyProbe::new());

/// One captured event, kept as the raw numbers with no interpretation applied.
#[derive(Copy, Clone, Default)]
struct Row {
    kind: i32,
    a: i32,
    mods: i32,
    repeats: i32,
    scan: i32,
    /// The raw platform `iModifiers` word. The whole point of the second revision of this
    /// tool: the three-bit summary in `mods` read 00 for every key on the E72, which only
    /// ever meant "not shift, ctrl or func".
    native: i32,
}

/// Ten rows fits the screen at 11px with room for the header. The oldest is dropped, not
/// the newest: here, unlike the shim's input queue, the interesting event is the one that
/// just happened.
const CAP: usize = 10;

const LOG_FILE: &str = "keylog.txt";

pub struct KeyProbe {
    rows: [Row; CAP],
    len: usize,
    total: u32,
    file_handle: i32,
}

impl KeyProbe {
    pub fn new() -> Self {
        // Preload the system clipboard with a known string, so the paste test in cal has something
        // to paste (Opções ▸ Colar / Ctrl+V should produce exactly this).
        let test = "https://cole.isto/teste/basic.ics";
        let utf16: alloc::vec::Vec<u16> = test.encode_utf16().collect();
        unsafe { sys::shim_clip_set_text(utf16.as_ptr(), utf16.len() as i32); }
        KeyProbe { rows: [Row::default(); CAP], len: 0, total: 0, file_handle: 0 }
    }

    fn log_open(&mut self) -> i32 {
        let mut path = [0u16; 256];
        let mut plen: i32 = 0;
        let rc = unsafe { sys::shim_private_path(path.as_mut_ptr(), 256, &mut plen) };
        if rc != sys::SHIM_OK {
            return 0;
        }
        for c in LOG_FILE.encode_utf16() {
            let i = plen as usize;
            if i < 256 {
                path[i] = c;
                plen += 1;
            }
        }
        let mode = sys::SHIM_FILE_WRITE | sys::SHIM_FILE_CREATE | sys::SHIM_FILE_APPEND;
        let mut handle: i32 = 0;
        let rc = unsafe { sys::shim_file_open(path.as_ptr(), plen, mode, &mut handle) };
        if rc != sys::SHIM_OK {
            return 0;
        }
        // Write header line so the file is self-describing.
        let header = b"kind  code   scan  mod  native    repeat\n";
        unsafe { sys::shim_file_write(handle, header.as_ptr(), header.len() as i32); }
        handle
    }

    fn log_row(&mut self, row: &Row) {
        if self.file_handle == 0 {
            self.file_handle = self.log_open();
        }
        if self.file_handle == 0 {
            return;
        }
        let mut buf = [0u8; 80];
        let n = format_row_file(row, &mut buf);
        unsafe { sys::shim_file_write(self.file_handle, buf.as_ptr(), n as i32); }
    }
}

impl Default for KeyProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl App for KeyProbe {
    fn title(&self) -> &str {
        "Key probe"
    }

    /// Every key event, raw. Consuming them is the point: a translated `KeyEvent` is
    /// exactly the view that hid the bug this tool was built to find.
    fn handle_raw(&mut self, ev: &RawEvent) -> Handled {
        const KEY_KINDS: [i32; 3] =
            [sys::SHIM_EV_KEY_CHAR, sys::SHIM_EV_KEY_DOWN, sys::SHIM_EV_KEY_UP];
        if !KEY_KINDS.contains(&ev.kind) {
            return Handled::Ignored;
        }
        let row = Row {
            kind: ev.kind,
            a: ev.a,
            mods: ev.b,
            repeats: ev.c,
            scan: ev.d,
            native: ev.native,
        };
        self.log_row(&row);
        if self.len == CAP {
            self.rows.rotate_left(1);
            self.rows[CAP - 1] = row;
        } else {
            self.rows[self.len] = row;
            self.len += 1;
        }
        self.total = self.total.wrapping_add(1);
        Handled::Consumed
    }

    /// Never reached: handle_raw consumes every key. Present because the trait requires
    /// it, and because leaving it unreachable documents that this app deliberately works
    /// below the translation layer.
    fn handle_key(&mut self, _ev: KeyEvent, _t: &Theme<'_>, _s: Rect) -> Handled {
        Handled::Ignored
    }

    fn draw(&mut self, c: &mut Canvas<'_>, theme: &Theme<'_>) {
        use symbian_ui::{Align, Point};

        let w = c.size().w;
        let h = c.size().h;
        let body = theme.fonts.body;
        let small = theme.fonts.small;
        let ink = theme.palette.text;
        let dim = theme.palette.dim;
        let hot = theme.palette.accent;

        symbian_ui::chrome::clear(c, theme);
        symbian_ui::chrome::title_bar(c, Rect::from_xywh(0, 0, w, 16), theme, "Key probe", Some("Fn = digit"));

        // Newest first: while pressing keys the line you want is the one that just
        // appeared, and putting it at the top means it is always in the same place.
        let mut y = 18;
        for i in (0..self.len).rev() {
            let mut line = [0u8; 64];
            let n = format_row(&self.rows[i], &mut line);
            if let Ok(s) = core::str::from_utf8(&line[..n]) {
                let color = if i + 1 == self.len { hot } else { ink };
                c.draw_text(Point::new(4, y + body.ascent()), s, body, color);
            }
            y += body.line_height() + 1;
        }

        if self.len == 0 {
            c.draw_text_in(Rect::from_xywh(0, 100, w, 20), "press any key", body, dim, Align::Center);
        }
        let mut tally = [0u8; 32];
        let mut n = 0;
        push_dec(&mut tally, &mut n, self.total);
        push_str(&mut tally, &mut n, " events");
        if let Ok(s) = core::str::from_utf8(&tally[..n]) {
            c.draw_text_in(Rect::from_xywh(0, h - 14, w, 12), s, small, dim, Align::Center);
        }
    }
}

// ------------------------------------------------------------------- formatting --

/// Append `v` as `width` uppercase hex digits. No `format!`: this runs on every
/// repaint and `core::fmt` on a soft-float target drags in more code than the whole
/// rest of this app.
fn push_hex(buf: &mut [u8], at: &mut usize, v: u32, width: usize) {
    const D: &[u8; 16] = b"0123456789ABCDEF";
    for i in (0..width).rev() {
        if *at < buf.len() {
            buf[*at] = D[((v >> (i * 4)) & 0xF) as usize];
            *at += 1;
        }
    }
}
fn push_str(buf: &mut [u8], at: &mut usize, s: &str) {
    for &b in s.as_bytes() {
        if *at < buf.len() {
            buf[*at] = b;
            *at += 1;
        }
    }
}
fn push_dec(buf: &mut [u8], at: &mut usize, v: u32) {
    if v >= 10 {
        push_dec(buf, at, v / 10);
    }
    if *at < buf.len() {
        buf[*at] = b'0' + (v % 10) as u8;
        *at += 1;
    }
}
/// Compact hex for the log file: kind code scan mod native repeat, one line per event.
fn format_row_file(r: &Row, buf: &mut [u8; 80]) -> usize {
    let mut n = 0;
    push_str(buf, &mut n, match r.kind {
        sys::SHIM_EV_KEY_CHAR => "CHR ",
        sys::SHIM_EV_KEY_DOWN => "DWN ",
        _ => "UP  ",
    });
    push_hex(buf, &mut n, r.a as u32, 4);
    push_str(buf, &mut n, "  ");
    push_hex(buf, &mut n, r.scan as u32, 4);
    push_str(buf, &mut n, "  ");
    push_hex(buf, &mut n, r.mods as u32, 2);
    push_str(buf, &mut n, "  ");
    push_hex(buf, &mut n, r.native as u32, 6);
    push_str(buf, &mut n, "  ");
    if r.repeats > 0 {
        push_dec(buf, &mut n, r.repeats as u32);
    } else {
        push_str(buf, &mut n, "0");
    }
    push_str(buf, &mut n, "\n");
    n
}

/// The line for one row, into a caller-owned buffer.
///
/// `a` is printed as hex *and*, when it is a printable ASCII scalar, as the literal
/// character in quotes. Both, because the hex is what identifies the bug and the
/// character is what makes the line readable while pressing keys.
fn format_row(r: &Row, buf: &mut [u8; 64]) -> usize {
    let mut n = 0;
    push_str(
        buf,
        &mut n,
        match r.kind {
            symbian_app::symbian_sys::SHIM_EV_KEY_CHAR => "chr ",
            symbian_app::symbian_sys::SHIM_EV_KEY_DOWN => "dwn ",
            _ => "up  ",
        },
    );
    push_hex(buf, &mut n, r.a as u32, 4);
    if (0x20..0x7F).contains(&r.a) {
        push_str(buf, &mut n, " '");
        if n < buf.len() {
            buf[n] = r.a as u8;
            n += 1;
        }
        push_str(buf, &mut n, "'");
    } else {
        push_str(buf, &mut n, "    ");
    }
    push_str(buf, &mut n, "  scan ");
    push_hex(buf, &mut n, r.scan as u32, 4);
    // Both the summary and the truth, side by side. That contrast is the lesson this
    // tool taught: `mod` read 00 for every key on the E72 and it meant nothing.
    push_str(buf, &mut n, " mod ");
    push_hex(buf, &mut n, r.mods as u32, 2);
    push_str(buf, &mut n, " raw ");
    // Six digits: iModifiers goes up to 0x08000000, but the top byte is 3D-pointer
    // and rotation bits that no keyboard sets, so six covers everything a key can
    // carry and leaves room for the rest of the line.
    push_hex(buf, &mut n, r.native as u32, 6);
    if r.repeats > 0 {
        push_str(buf, &mut n, " r");
        push_dec(buf, &mut n, r.repeats as u32);
    }
    n
}
