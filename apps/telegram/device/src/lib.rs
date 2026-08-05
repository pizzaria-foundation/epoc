//! Device entry points: the three functions the C++ shim calls, plus the runtime
//! items Rust needs and `no_std` does not supply.
//!
//! This is the only crate in the tree that carries a `#[global_allocator]` or a
//! `#[panic_handler]`. Keeping them here rather than in `tg` is what lets the app
//! logic stay a plain rlib that `cargo test` can run on the host — the whole
//! reason the chat is testable without a phone.
//!
//! Nothing here makes decisions. It translates: `ShimEvent` into `KeyEvent`, a
//! raw framebuffer pointer into a `Canvas`, and back. Anything that looks like a
//! policy choice belongs in `tg`.

#![no_std]
#![no_main]

extern crate alloc;

use core::alloc::{GlobalAlloc, Layout};
use core::panic::PanicInfo;
use core::ptr;

use symbian_gfx::{Canvas, Size};
use symbian_sys as sys;
use symbian_ui::{BitmapFont, Fonts, Handled, Key, KeyEvent, Modifiers, Rect, Softkey, Theme};

// ------------------------------------------------------------------ allocator --

/// `RHeap` aligns to 8 bytes. Anything stricter has to be arranged by hand.
const NATIVE_ALIGN: usize = 8;

struct SymbianHeap;

unsafe impl GlobalAlloc for SymbianHeap {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if l.align() <= NATIVE_ALIGN {
            return sys::shim_alloc(l.size() as u32) as *mut u8;
        }
        // Over-allocate and record the shift in the word below the aligned
        // pointer, so dealloc can find the original cell. `RHeap`'s 8-byte
        // guarantee is documented but not something to bet the heap on, and this
        // path costs nothing for the 99% of allocations that never need it.
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

    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        if l.align() <= NATIVE_ALIGN {
            // Let RHeap grow the cell in place when it can; that is the whole
            // reason to forward realloc rather than alloc-copy-free.
            return sys::shim_realloc(p as *mut _, new as u32) as *mut u8;
        }
        let np = self.alloc(Layout::from_size_align_unchecked(new, l.align()));
        if !np.is_null() {
            ptr::copy_nonoverlapping(p, np, core::cmp::min(l.size(), new));
            self.dealloc(p, l);
        }
        np
    }
}

/// Zero-sized, so it lands in no section at all and contributes nothing to the
/// writable static data that a Symbian DLL may not have. (We ship an EXE, where
/// it would be allowed anyway, but keeping the property makes the crate reusable.)
#[global_allocator]
static HEAP: SymbianHeap = SymbianHeap;

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // A Rust panic cannot be allowed to unwind into C++: we are built
    // panic=abort, so there are no landing pads on either side of the boundary.
    // shim_panic calls User::Panic and never returns.
    match info.location() {
        Some(l) => unsafe { sys::shim_panic(l.file().as_ptr(), l.file().len() as u32, l.line()) },
        None => unsafe { sys::shim_panic(ptr::null(), 0, 0) },
    }
}

// ---------------------------------------------------------------------- fonts --

// Embedded rather than taken from Symbian's CFont. The Rust rasterizer is already
// tested and the atlases guarantee glyph coverage regardless of which font a given
// handset variant shipped — worth ~55 KB on a device with 250 MB of storage. The
// `Font` trait stays the seam if we ever want the system font instead.
static UI_BODY: &[u8] = include_bytes!("../../../../crates/symbian-ui/assets/ui11.sbf");
static UI_STRONG: &[u8] = include_bytes!("../../../../crates/symbian-ui/assets/ui11b.sbf");
static UI_SMALL: &[u8] = include_bytes!("../../../../crates/symbian-ui/assets/ui9.sbf");

// ----------------------------------------------------------------- app state --

/// Everything the entry points share.
///
/// A single mutable static, which is exactly the thing Symbian forbids in a DLL —
/// but this is an EXE, where writable static data is unrestricted (elf2e32's check
/// is inside `if (isDllp)`). The alternative would be threading a context pointer
/// through the C ABI, which buys nothing here: the app is single-threaded by
/// construction because `RWsSession` is not thread-safe and all drawing must
/// happen on the GUI thread.
static mut APP: Option<tg::App> = None;

/// Set once the first frame has been drawn, so `rust_step` can tell a genuine
/// no-op from "we have never painted".
static mut PAINTED: bool = false;

fn app() -> Option<&'static mut tg::App> {
    // SAFETY: single-threaded; every caller is the GUI thread via the shim.
    unsafe { (&raw mut APP).as_mut().and_then(|o| o.as_mut()) }
}

/// Translate one shim event into a toolkit key event. Returns `None` for events
/// the app does not act on, which is most of them.
fn to_key_event(e: &sys::ShimEvent) -> Option<KeyEvent> {
    let mods = Modifiers {
        shift: e.b & sys::modifier::SHIFT != 0,
        ctrl: e.b & sys::modifier::CTRL != 0,
        func: e.b & sys::modifier::FUNC != 0,
    };
    let key = match e.kind {
        sys::SHIM_EV_KEY_CHAR => Key::Char(char::from_u32(e.a as u32)?),
        sys::SHIM_EV_KEY_DOWN => match e.a {
            sys::key::UP => Key::Up,
            sys::key::DOWN => Key::Down,
            sys::key::LEFT => Key::Left,
            sys::key::RIGHT => Key::Right,
            sys::key::SELECT => Key::Select,
            sys::key::SOFT_LEFT => Key::Softkey(Softkey::Left),
            sys::key::SOFT_MIDDLE => Key::Softkey(Softkey::Middle),
            sys::key::SOFT_RIGHT => Key::Softkey(Softkey::Right),
            sys::key::BACKSPACE => Key::Backspace,
            sys::key::DELETE => Key::Delete,
            sys::key::ENTER => Key::Enter,
            sys::key::CALL => Key::Call,
            sys::key::END => Key::End,
            other => Key::Raw(other as u16),
        },
        _ => return None,
    };
    Some(KeyEvent { key, mods, repeat: e.c > 0 })
}

// --------------------------------------------------------------- entry points --

/// Called once, after the control exists and before the pump starts.
#[no_mangle]
pub extern "C" fn rust_app_start() {
    // SAFETY: called exactly once, from CShimAppUi::ConstructL, on the GUI thread.
    unsafe {
        APP = Some(tg::App::mock());
        PAINTED = false;
    }
}

/// Called once, during teardown, before the surface Rust may hold a pointer to
/// goes away.
#[no_mangle]
pub extern "C" fn rust_app_stop() {
    unsafe {
        APP = None;
    }
}

/// Drain events, update, redraw if anything changed.
///
/// Runs from a `CIdle` on the GUI thread and must return in a few milliseconds: a
/// long one starves the window server, which freezes the whole phone rather than
/// just this app.
#[no_mangle]
pub extern "C" fn rust_step() {
    let Some(app) = app() else { return };
    let (w, h) = screen_size();

    // One `with_theme` around the whole step. The fonts are parsed once per frame
    // rather than once per key, and — the reason it has to be this shape — a
    // `Theme` borrows the atlases, so it cannot escape the closure that owns them.
    let dirty = with_theme(|theme| {
        let mut dirty = unsafe { !PAINTED };
        let screen = Rect::from_xywh(0, 0, w, h);

        // Drain the whole queue before drawing. Coalescing several key presses
        // into one repaint is the difference between keeping up and falling
        // behind when someone holds a key down.
        let mut ev = sys::ShimEvent::default();
        while unsafe { sys::shim_poll_event(&mut ev) } == 1 {
            match ev.kind {
                sys::SHIM_EV_RESIZE | sys::SHIM_EV_REDRAW => dirty = true,
                sys::SHIM_EV_QUIT => {
                    unsafe { sys::shim_request_exit() };
                    return false;
                }
                _ => {
                    if let Some(k) = to_key_event(&ev) {
                        if app.handle_key(k, theme, screen) == Handled::Consumed {
                            dirty = true;
                        }
                    }
                }
            }
        }
        dirty
    });

    if app.should_exit {
        unsafe { sys::shim_request_exit() };
        return;
    }
    if !dirty {
        return;
    }

    draw(app);
    unsafe { PAINTED = true };
}

fn screen_size() -> (i32, i32) {
    let (mut w, mut h) = (0i32, 0i32);
    if unsafe { sys::shim_screen_size(&mut w, &mut h) } != sys::SHIM_OK || w <= 0 || h <= 0 {
        // The E72's panel. Only reached if the shim is not ready, in which case
        // nothing will be drawn anyway; a sane default beats a zero-sized canvas.
        return (320, 240);
    }
    (w, h)
}

/// Build the theme and hand it to `f`. The fonts are parsed on every call rather
/// than cached in a static: `BitmapFont` borrows the atlas bytes, so caching one
/// would need a self-referential static, and parsing is a header read plus a
/// bounds check per glyph record — cheap next to a full repaint.
fn with_theme<R>(f: impl FnOnce(&Theme<'_>) -> R) -> R {
    let body = BitmapFont::new(UI_BODY).expect("ui11 atlas is malformed");
    let strong = BitmapFont::new(UI_STRONG).expect("ui11b atlas is malformed");
    let small = BitmapFont::new(UI_SMALL).expect("ui9 atlas is malformed");
    let fonts = Fonts { body: &body, strong: &strong, small: &small, title: &strong };
    f(&Theme::dark(fonts))
}

fn draw(app: &mut tg::App) {
    let mut fb = sys::ShimFb::default();
    if unsafe { sys::shim_fb_lock(&mut fb) } != sys::SHIM_OK || fb.pixels.is_null() {
        return;
    }

    let (w, h) = (fb.width, fb.height);
    // stride is in bytes and the buffer is RGB565, so two bytes a pixel.
    let stride_px = (fb.stride / 2) as usize;
    let len = stride_px * h as usize;

    // SAFETY: the shim guarantees the pointer is valid for `stride * height`
    // bytes until shim_fb_unlock, and that nothing else touches it meanwhile.
    // The buffer is ordinary memory the shim allocated, not the FBS chunk, so it
    // does not move under us.
    let pixels: &mut [u16] = unsafe { core::slice::from_raw_parts_mut(fb.pixels as *mut u16, len) };

    with_theme(|theme| {
        let mut canvas = Canvas::new(pixels, Size::new(w, h), stride_px);
        app.draw(&mut canvas, theme);
    });

    unsafe {
        sys::shim_fb_unlock();
        sys::shim_present(0, 0, w, h);
    }
}
