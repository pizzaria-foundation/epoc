//! Dumps the handset's own ABNT2 QWERTY keymap to a file, then says what it found.
//!
//! # Why this exists
//!
//! The SDK treats the E72's keyboard as a US QWERTY and the target handset's is ABNT2.
//! The consequences are user-visible: `~` then `a` types `a`, so "não" cannot be written,
//! and `+` cannot be typed at all because the Chr/Fn symbol layer does not exist beyond
//! twelve overlaid digits.
//!
//! Fixing that needs a keymap, and `docs/device-notes.md` records three rounds of
//! on-device debugging wasted on keyboard behaviour that had been reasoned about instead
//! of measured. So this asks the phone. `src/keydump.cpp` reads the answer out of
//! `ptiengine.dll` — the platform's keymap database, the layer *underneath* the FEP, not
//! the FEP itself — and `tools/mkkeymap.py` turns the file into the static table in
//! `crates/symbian-keys`.
//!
//! # Why it is its own binary
//!
//! It links `ptiengine`, and an import the handset cannot satisfy stops the image loading
//! with no error and no log. `docs/device-notes.md` states the rule: "if a facility might
//! not resolve, it belongs in its own binary, where failing to load costs a probe rather
//! than the report." Nothing that ships imports this DLL. Run `examples/libprobe` first —
//! it asks whether `ptiengine.dll` is there.
//!
//! # Reading the screen
//!
//! `err 0` and a four-figure `keys` count means the dump is good; fetch the file. `dead`
//! is the number of dead-key markers found, and it is the number that matters most: if it
//! is zero, either this handset has no ABNT2 keymap installed or the engine did not switch
//! language, and the `en` baseline count is what tells those apart. Two identical counts
//! mean the engine ignored the language and the dump is worthless.

#![no_std]
#![no_main]

extern crate alloc;

use symbian_ui::{Align, App, Canvas, Handled, KeyEvent, Point, Rect, Theme};

symbian_app::entry!(KeyDump::new());

/// `C:\Data\` and not the app's private directory.
///
/// The private directory needs no capability, which makes it the right place for almost
/// everything — and the wrong place for this, because File Manager and USB cannot see into
/// it. A dump nobody can carry off the phone is not a dump. `examples/imgprobe` reached the
/// same conclusion for its report and its `app.conf` says so; this is that lesson applied
/// rather than rediscovered.
///
/// So: `WriteUserData`, and the file shows up in File Manager under Data, from where it can
/// be sent over Bluetooth to `tools/btrecv.py`.
const DUMP_PATH: &str = "C:\\Data\\keymap.txt";

/// Slot indices, mirroring `TSlot` in `src/keydump.cpp`.
mod slot {
    pub const ERR: usize = 0;
    pub const KEYS_BR: usize = 1;
    pub const DEAD_BR: usize = 2;
    pub const KEYS_EN: usize = 3;
    pub const NUMERIC: usize = 4;
    pub const BYTES: usize = 5;
    pub const COUNT: usize = 6;
}

extern "C" {
    fn keydump_run(path: *const u16, path_len: i32, out: *mut i32, cap: i32) -> i32;
}

pub struct KeyDump {
    slots: [i32; slot::COUNT],
    done: bool,
}

impl KeyDump {
    pub fn new() -> Self {
        KeyDump { slots: [-1; slot::COUNT], done: false }
    }

    /// Run the dump once.
    ///
    /// Not in `new`: the shim's file session and the Avkon framework are up by the time
    /// the first key or the first draw arrives, and doing platform work inside a
    /// constructor is how a probe fails before it can draw the reason.
    fn run(&mut self) {
        if self.done {
            return;
        }
        self.done = true;

        // A fixed absolute path, so it needs no query and cannot half-resolve. UTF-16 on
        // the stack: the shim wants code units and this path is ASCII, so one unit per byte
        // holds.
        let mut path = [0u16; 64];
        let mut plen = 0usize;
        for c in DUMP_PATH.encode_utf16() {
            if plen < path.len() {
                path[plen] = c;
                plen += 1;
            }
        }
        unsafe {
            keydump_run(
                path.as_ptr(),
                plen as i32,
                self.slots.as_mut_ptr(),
                slot::COUNT as i32,
            );
        }
    }
}

impl Default for KeyDump {
    fn default() -> Self {
        Self::new()
    }
}

impl App for KeyDump {
    fn title(&self) -> &str {
        "Key dump"
    }

    fn handle_key(&mut self, _ev: KeyEvent, _t: &Theme<'_>, _s: Rect) -> Handled {
        // Nothing to interact with. Ignoring keys leaves the red End key to the platform,
        // which is how you get out.
        Handled::Ignored
    }

    fn draw(&mut self, c: &mut Canvas<'_>, theme: &Theme<'_>) {
        self.run();

        let (w, h) = (c.size().w, c.size().h);
        let body = theme.fonts.body;
        let small = theme.fonts.small;
        let dim = theme.palette.dim;
        let ok = symbian_ui::Color::hex(0x6FD08A);
        let bad = symbian_ui::Color::hex(0xE06F6F);

        symbian_ui::chrome::clear(c, theme);
        symbian_ui::chrome::title_bar(
            c,
            Rect::from_xywh(0, 0, w, 16),
            theme,
            "Key dump",
            Some("ABNT2"),
        );

        // `dead` is deliberately the row a reader's eye lands on: it is the one number
        // that says whether this dump answers the question, and a green zero would be a
        // lie. See the module comment.
        let rows: [(&str, i32); 6] = [
            ("err", self.slots[slot::ERR]),
            ("keys br", self.slots[slot::KEYS_BR]),
            ("dead br", self.slots[slot::DEAD_BR]),
            ("numeric", self.slots[slot::NUMERIC]),
            ("keys en", self.slots[slot::KEYS_EN]),
            ("bytes", self.slots[slot::BYTES]),
        ];

        let mut y = 22;
        for (label, value) in rows {
            let good = match label {
                "err" => value == 0,
                // A zero count for any of the rest means the measurement failed, whatever
                // the error code said.
                _ => value > 0,
            };
            let color = if good { ok } else { bad };
            c.draw_text(Point::new(6, y + body.ascent()), label, body, dim);

            let mut buf = [0u8; 16];
            let mut n = 0;
            push_int(&mut buf, &mut n, value);
            if let Ok(s) = core::str::from_utf8(&buf[..n]) {
                c.draw_text_in(
                    Rect::from_xywh(w - 90, y, 84, body.line_height()),
                    s,
                    body,
                    color,
                    Align::End,
                );
            }
            y += body.line_height() + 3;
        }

        // The check that catches the failure an error code cannot: if pt-BR and English
        // report the same number of mapped keys, the engine did not switch layouts and
        // both dumps describe the same keyboard.
        let suspect = self.slots[slot::KEYS_BR] > 0
            && self.slots[slot::KEYS_BR] == self.slots[slot::KEYS_EN];
        let note = if suspect {
            "br == en: language did not switch"
        } else {
            "send C:\\Data\\keymap.txt off the phone"
        };
        c.draw_text_in(
            Rect::from_xywh(0, h - 26, w, 12),
            note,
            small,
            if suspect { bad } else { dim },
            Align::Center,
        );
        c.draw_text_in(
            Rect::from_xywh(0, h - 14, w, 12),
            "red End key exits",
            small,
            dim,
            Align::Center,
        );
    }
}

/// No `format!`: `core::fmt` on a soft-float target drags in more code than the rest of
/// this app, and this runs on every repaint.
fn push_int(buf: &mut [u8], at: &mut usize, v: i32) {
    if v < 0 {
        if *at < buf.len() {
            buf[*at] = b'-';
            *at += 1;
        }
        push_dec(buf, at, v.unsigned_abs());
    } else {
        push_dec(buf, at, v as u32);
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
