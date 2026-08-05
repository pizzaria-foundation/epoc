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

use core::alloc::{GlobalAlloc, Layout};
use core::panic::PanicInfo;
use core::ptr;

use symbian_gfx::{Align, BitmapFont, Canvas, Color, Font, Point, Rect, Size};
use symbian_sys as sys;

const NATIVE_ALIGN: usize = 8;

struct SymbianHeap;

unsafe impl GlobalAlloc for SymbianHeap {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if l.align() <= NATIVE_ALIGN {
            return sys::shim_alloc(l.size() as u32) as *mut u8;
        }
        let total = l.size() + l.align() + core::mem::size_of::<usize>();
        let raw = sys::shim_alloc(total as u32) as usize;
        if raw == 0 {
            return ptr::null_mut();
        }
        let base = raw + core::mem::size_of::<usize>();
        let aligned = (base + l.align() - 1) & !(l.align() - 1);
        *((aligned - core::mem::size_of::<usize>()) as *mut usize) = raw;
        aligned as *mut u8
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        if l.align() <= NATIVE_ALIGN {
            sys::shim_free(p as *mut _);
        } else {
            let raw = *((p as usize - core::mem::size_of::<usize>()) as *const usize);
            sys::shim_free(raw as *mut _);
        }
    }
}

#[global_allocator]
static HEAP: SymbianHeap = SymbianHeap;

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    match info.location() {
        Some(l) => unsafe { sys::shim_panic(l.file().as_ptr(), l.file().len() as u32, l.line()) },
        None => unsafe { sys::shim_panic(ptr::null(), 0, 0) },
    }
}

static UI_BODY: &[u8] = include_bytes!("../../../../crates/symbian-ui/assets/ui11.sbf");
static UI_SMALL: &[u8] = include_bytes!("../../../../crates/symbian-ui/assets/ui9.sbf");

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

struct State {
    results: [i32; CAP],
    len: usize,
}

static mut STATE: Option<State> = None;
static mut DIRTY: bool = true;

fn state() -> Option<&'static mut State> {
    // SAFETY: single-threaded; every caller is the GUI thread via the shim.
    unsafe { (&raw mut STATE).as_mut().and_then(|o| o.as_mut()) }
}

/// Ask about one DLL. The name is converted to UTF-16 on the stack: the shim wants
/// code units, and these names are ASCII so one unit per byte holds.
fn query(name: &str) -> i32 {
    let mut buf = [0u16; 32];
    let mut n = 0;
    for b in name.bytes() {
        if n < buf.len() {
            buf[n] = b as u16;
            n += 1;
        }
    }
    unsafe { sys::shim_dll_present(buf.as_ptr(), n as i32) }
}

#[no_mangle]
pub extern "C" fn rust_app_start() {
    // Every probe runs once, at startup: RLibrary::Load actually maps the DLL, so
    // repeating it on every repaint would churn the loader for no new information.
    let mut st = State { results: [0; CAP], len: 0 };
    for p in PROBES.iter().take(CAP) {
        st.results[st.len] = query(p.dll);
        st.len += 1;
    }
    unsafe {
        STATE = Some(st);
        DIRTY = true;
    }
}

#[no_mangle]
pub extern "C" fn rust_app_stop() {
    unsafe {
        STATE = None;
    }
}

#[no_mangle]
pub extern "C" fn rust_step() {
    let Some(st) = state() else { return };
    let mut ev = sys::ShimEvent::default();
    while unsafe { sys::shim_poll_event(&mut ev) } == 1 {
        match ev.kind {
            sys::SHIM_EV_QUIT => {
                unsafe { sys::shim_request_exit() };
                return;
            }
            sys::SHIM_EV_RESIZE | sys::SHIM_EV_REDRAW => unsafe { DIRTY = true },
            _ => {}
        }
    }
    if !unsafe { DIRTY } {
        return;
    }
    draw(st);
    unsafe { DIRTY = false };
}

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

fn draw(st: &State) {
    let mut fb = sys::ShimFb::default();
    if unsafe { sys::shim_fb_lock(&mut fb) } != sys::SHIM_OK || fb.pixels.is_null() {
        return;
    }
    let stride_px = (fb.stride / 2) as usize;
    let len = stride_px * fb.height as usize;
    // SAFETY: the shim guarantees the pointer is valid for stride*height bytes until
    // shim_fb_unlock, and that nothing else touches it meanwhile.
    let pixels: &mut [u16] = unsafe { core::slice::from_raw_parts_mut(fb.pixels as *mut u16, len) };

    let body = BitmapFont::new(UI_BODY).expect("ui11 atlas is malformed");
    let small = BitmapFont::new(UI_SMALL).expect("ui9 atlas is malformed");

    {
        let mut c = Canvas::new(pixels, Size::new(fb.width, fb.height), stride_px);
        let w = fb.width;
        c.clear(Color::hex(0x0A0C10));
        c.fill_rect(Rect::from_xywh(0, 0, w, 16), Color::hex(0x1A2634));
        c.hline(15, 0, w, Color::hex(0x2C3C4E));
        c.draw_text(Point::new(4, 12), "Device libraries", &small, Color::hex(0xE0E6EC));

        let ok = Color::hex(0x6FD08A);
        let bad = Color::hex(0xE06F6F);
        let dim = Color::hex(0x7C8894);

        let mut y = 19;
        for (i, p) in PROBES.iter().take(st.len).enumerate() {
            let e = st.results[i];
            let color = if e == 0 { ok } else { bad };
            c.draw_text(Point::new(4, y + body.ascent()), p.dll, &body, color);

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
                    &body,
                    color,
                    Align::End,
                );
            }
            c.draw_text_in(
                Rect::from_xywh(w - 50, y, 48, body.line_height()),
                p.gives,
                &small,
                dim,
                Align::End,
            );
            y += body.line_height() + 2;
        }

        c.draw_text_in(
            Rect::from_xywh(0, fb.height - 14, w, 12),
            "red End key exits",
            &small,
            dim,
            Align::Center,
        );
    }

    unsafe {
        sys::shim_fb_unlock();
        sys::shim_present(0, 0, fb.width, fb.height);
    }
}
