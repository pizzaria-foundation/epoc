//! Reports which optional DLLs this handset actually has.
//!
//! # Why this exists
//!
//! The SDK's import libraries describe what Symbian *could* provide. What a given
//! handset provides is a different question, and the SDK cannot answer it: Open C —
//! `libc`, `libcrypto`, `libssl`, `libz` — shipped as a separate package on S60 3rd
//! Edition, in ROM on some devices and installable on others.
//!
//! The answer decides how storage, crypto and TCP get built:
//!
//! - **`libcrypto` present** means AES, RSA, `BN_mod_exp` and `RAND_bytes` come from
//!   the device instead of being written. That is the whole bignum layer, which is
//!   the largest and most delicate part of an MTProto handshake. (SHA-256 still has
//!   to be written: this OpenSSL is 0.9.8a, from 2005, and predates it.)
//! - **`libz` present** means `inflate` for free, which `gzip_packed` needs.
//! - **`libc` present** means BSD sockets and `fopen`, which are *synchronous* and so
//!   far simpler than `RSocket` on a `CActive` — at the cost of needing a thread,
//!   since a blocking call on the GUI thread freezes the whole phone.
//!
//! # Why it asks instead of just linking
//!
//! Importing a DLL that is not there does not fail gracefully. The E32 loader refuses
//! to start the process, which on a phone presents as the icon doing nothing at all —
//! no error, no panic, no log. That failure mode has already cost this project a day.
//! `RLibrary::Load` turns it into a number.
//!
//! `RLibrary::Load` is also a stronger test than checking the filesystem: a DLL can
//! be present and still fail to load, through a wrong UID, its own unsatisfied
//! imports, or a capability we do not hold — and each of those breaks an import
//! exactly as thoroughly as the file being missing.

#![no_std]
#![no_main]

extern crate alloc;

use symbian_ui::{Align, App, Canvas, Handled, KeyEvent, Point, Rect, Theme};

symbian_app::entry!(LibProbe::new());

/// What each DLL would buy us, so the screen explains itself and the reading does
/// not have to be brought back here to be interpreted.
struct Probe {
    dll: &'static str,
    gives: &'static str,
}

const PROBES: &[Probe] = &[
    Probe { dll: "libc.dll", gives: "BSD sockets, stdio" },
    Probe { dll: "libcrypto.dll", gives: "AES, RSA, bignum" },
    Probe { dll: "libssl.dll", gives: "TLS" },
    Probe { dll: "libz.dll", gives: "inflate" },
    Probe { dll: "libm.dll", gives: "libm" },
    Probe { dll: "libpthread.dll", gives: "threads" },
    // The two the SDK's own GUI apps link, as a control: these must come back
    // present, and if they do not then the query itself is broken rather than the
    // device being bare.
    Probe { dll: "euser.dll", gives: "(control: must be OK)" },
    Probe { dll: "avkon.dll", gives: "(control: must be OK)" },
];

const CAP: usize = 8;

pub struct LibProbe {
    results: [i32; CAP],
    len: usize,
}

impl LibProbe {
    /// Every probe runs once, here: `RLibrary::Load` actually maps the DLL, so repeating
    /// it on every repaint would churn the loader for no new information.
    pub fn new() -> Self {
        let mut p = LibProbe { results: [0; CAP], len: 0 };
        for probe in PROBES.iter().take(CAP) {
            p.results[p.len] = query(probe.dll);
            p.len += 1;
        }
        p
    }
}

impl Default for LibProbe {
    fn default() -> Self {
        Self::new()
    }
}

/// Ask about one DLL. The name is converted to UTF-16 on the stack: the shim wants code
/// units, and these names are ASCII so one unit per byte holds.
fn query(name: &str) -> i32 {
    let mut buf = [0u16; 32];
    let mut n = 0;
    for b in name.bytes() {
        if n < buf.len() {
            buf[n] = b as u16;
            n += 1;
        }
    }
    unsafe { symbian_app::symbian_sys::shim_dll_present(buf.as_ptr(), n as i32) }
}

impl App for LibProbe {
    fn title(&self) -> &str {
        "Library probe"
    }

    fn handle_key(&mut self, _ev: KeyEvent, _t: &Theme<'_>, _s: Rect) -> Handled {
        // Nothing to interact with: the answer is fixed at startup. Ignoring keys leaves
        // the red End key to the platform, which is how you get out.
        Handled::Ignored
    }

    fn draw(&mut self, c: &mut Canvas<'_>, theme: &Theme<'_>) {
        let (w, h) = (c.size().w, c.size().h);
        let body = theme.fonts.body;
        let small = theme.fonts.small;

        symbian_ui::chrome::clear(c, theme);
        symbian_ui::chrome::title_bar(
            c,
            Rect::from_xywh(0, 0, w, 16),
            theme,
            "Device libraries",
            None,
        );

        // Green for present, red for absent — and the error number for anything that is
        // neither, because "not found" and "no permission" mean very different things
        // about how to proceed.
        let ok = symbian_ui::Color::hex(0x6FD08A);
        let bad = symbian_ui::Color::hex(0xE06F6F);
        let dim = theme.palette.dim;

        let mut y = 19;
        for (i, p) in PROBES.iter().take(self.len).enumerate() {
            let e = self.results[i];
            let color = if e == 0 { ok } else { bad };
            c.draw_text(Point::new(4, y + body.ascent()), p.dll, body, color);

            let mut buf = [0u8; 40];
            let mut n = 0;
            push_str(&mut buf, &mut n, err_name(e));
            if e != 0 {
                push_str(&mut buf, &mut n, " (");
                push_int(&mut buf, &mut n, e);
                push_str(&mut buf, &mut n, ")");
            }
            if let Ok(s) = core::str::from_utf8(&buf[..n]) {
                c.draw_text_in(
                    Rect::from_xywh(w - 150, y, 96, body.line_height()),
                    s,
                    body,
                    color,
                    Align::End,
                );
            }
            c.draw_text_in(
                Rect::from_xywh(w - 52, y, 50, body.line_height()),
                p.gives,
                small,
                dim,
                Align::End,
            );
            y += body.line_height() + 2;
        }

        c.draw_text_in(
            Rect::from_xywh(0, h - 14, w, 12),
            "red End key exits",
            small,
            dim,
            Align::Center,
        );
    }
}

// ------------------------------------------------------------------- formatting --

fn push_str(buf: &mut [u8], at: &mut usize, s: &str) {
    for &b in s.as_bytes() {
        if *at < buf.len() {
            buf[*at] = b;
            *at += 1;
        }
    }
}

fn push_int(buf: &mut [u8], at: &mut usize, v: i32) {
    if v < 0 {
        push_str(buf, at, "-");
    }
    let mut m = v.unsigned_abs();
    let mut digits = [0u8; 12];
    let mut n = 0;
    loop {
        digits[n] = b'0' + (m % 10) as u8;
        n += 1;
        m /= 10;
        if m == 0 {
            break;
        }
    }
    for i in (0..n).rev() {
        if *at < buf.len() {
            buf[*at] = digits[i];
            *at += 1;
        }
    }
}

/// The Symbian errors worth naming. -1 is the one that matters — it is the "this
/// handset does not have it" answer, as opposed to a permission or format problem,
/// which would mean something quite different about how to proceed.
fn err_name(e: i32) -> &'static str {
    match e {
        0 => "OK",
        -1 => "not found",
        -18 => "in use",
        -21 => "not supported",
        -46 => "no permission",
        _ => "error",
    }
}

