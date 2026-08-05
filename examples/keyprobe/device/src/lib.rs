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

use core::alloc::{GlobalAlloc, Layout};
use core::panic::PanicInfo;
use core::ptr;

use symbian_gfx::{Align, BitmapFont, Canvas, Color, Font, Point, Rect, Size};
use symbian_sys as sys;

// The allocator and panic handler are duplicated from apps/telegram/device rather
// than shared, and deliberately: a crate that exports these lang items cannot be a
// dependency of another crate that also exports them, so "the runtime items crate"
// can only ever be the final staticlib. Twenty lines is the price of that rule.

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

/// One captured event, kept as the raw numbers with no interpretation applied.
#[derive(Copy, Clone, Default)]
struct Row {
    kind: i32,
    a: i32,
    mods: i32,
    repeats: i32,
    scan: i32,
    /// The raw platform `iModifiers` word. The whole point of the second revision of
    /// this tool: the three-bit summary in `mods` read 00 for every key on the E72,
    /// which only ever meant "not shift, ctrl or func".
    native: i32,
}

/// Ten rows fits the screen at 11px with room for the header. The oldest is dropped,
/// not the newest: here, unlike the shim's input queue, the interesting event is the
/// one that just happened.
const CAP: usize = 10;

struct State {
    rows: [Row; CAP],
    len: usize,
    total: u32,
}

static mut STATE: Option<State> = None;
static mut DIRTY: bool = true;

fn state() -> Option<&'static mut State> {
    // SAFETY: single-threaded; every caller is the GUI thread via the shim.
    unsafe { (&raw mut STATE).as_mut().and_then(|o| o.as_mut()) }
}

#[no_mangle]
pub extern "C" fn rust_app_start() {
    unsafe {
        STATE = Some(State { rows: [Row::default(); CAP], len: 0, total: 0 });
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
            sys::SHIM_EV_KEY_CHAR | sys::SHIM_EV_KEY_DOWN | sys::SHIM_EV_KEY_UP => {
                let row = Row {
                    kind: ev.kind,
                    a: ev.a,
                    mods: ev.b,
                    repeats: ev.c,
                    scan: ev.d,
                    native: ev.native,
                };
                if st.len == CAP {
                    st.rows.rotate_left(1);
                    st.rows[CAP - 1] = row;
                } else {
                    st.rows[st.len] = row;
                    st.len += 1;
                }
                st.total = st.total.wrapping_add(1);
                unsafe { DIRTY = true };
            }
            _ => {}
        }
    }

    if !unsafe { DIRTY } {
        return;
    }
    draw(st);
    unsafe { DIRTY = false };
}

// --------------------------------------------------------------- formatting --

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
            sys::SHIM_EV_KEY_CHAR => "chr ",
            sys::SHIM_EV_KEY_DOWN => "dwn ",
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

// -------------------------------------------------------------------- paint --

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
        let bg = Color::hex(0x0A0C10);
        let ink = Color::hex(0xE0E6EC);
        let dim = Color::hex(0x7C8894);
        let hot = Color::hex(0x6FD08A);
        c.clear(bg);

        let w = fb.width;
        let head = Rect::from_xywh(0, 0, w, 16);
        c.fill_rect(head, Color::hex(0x1A2634));
        c.hline(15, 0, w, Color::hex(0x2C3C4E));
        c.draw_text(Point::new(4, 12), "Key probe  (Ctrl = digit)", &small, ink);
        let mut buf = [0u8; 64];
        let mut n = 0;
        push_dec(&mut buf, &mut n, st.total);
        push_str(&mut buf, &mut n, " events");
        if let Ok(s) = core::str::from_utf8(&buf[..n]) {
            c.draw_text_in(Rect::from_xywh(w - 90, 0, 86, 16), s, &small, dim, Align::End);
        }

        // Newest first: while pressing keys, the line you want is the one that just
        // appeared, and putting it at the top means it is always in the same place.
        let mut y = 18;
        for i in (0..st.len).rev() {
            let r = &st.rows[i];
            let mut line = [0u8; 64];
            let n = format_row(r, &mut line);
            if let Ok(s) = core::str::from_utf8(&line[..n]) {
                let color = if i + 1 == st.len { hot } else { ink };
                c.draw_text(Point::new(4, y + body.ascent()), s, &body, color);
            }
            y += body.line_height() + 1;
        }

        if st.len == 0 {
            c.draw_text_in(
                Rect::from_xywh(0, 100, w, 20),
                "press any key",
                &body,
                dim,
                Align::Center,
            );
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
