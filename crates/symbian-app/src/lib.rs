//! The device side of an application, as one macro.
//!
//! ```ignore
//! #![no_std]
//! #![no_main]
//! extern crate alloc;
//!
//! symbian_app::entry!(MyApp::new());
//! ```
//!
//! That is the whole of a device crate. The macro supplies the allocator, the panic
//! handler, the three `extern "C"` entry points the shim calls, the event translation and
//! the theme.
//!
//! # Why a macro and not a function
//!
//! `#[global_allocator]` and `#[panic_handler]` are lang items, and a lang item can be
//! defined exactly once in a linked program. A library cannot provide them — the moment
//! two crates in a dependency graph both did, nothing would link. So they have to be
//! defined *in the final staticlib*, which is what a macro does: it expands in the
//! caller's crate, so the items land there while the code behind them lives here.
//!
//! Before this existed, every app carried its own copy of the allocator, the panic
//! handler, the key translation and the framebuffer setup — around 120 lines of `unsafe`
//! per app, duplicated three times across the reference app and two diagnostics. Three
//! copies is where the copies start drifting.
//!
//! # What it does not do
//!
//! Own the loop. Avkon calls `CActiveScheduler::Start()` and never returns until the app
//! exits, so `rust_step` is a callee that must return promptly: it runs on the GUI thread
//! from a `CIdle`, and a long one starves the window server, which freezes the whole
//! phone rather than just this app.

#![no_std]

extern crate alloc;

use core::alloc::{GlobalAlloc, Layout};
use core::ptr;

use symbian_gfx::{BitmapFont, Canvas, Size};
use symbian_sys as sys;
use symbian_ui::{Fonts, Key, KeyEvent, Modifiers, Palette, Softkey, Theme};

// Re-exported so the macro's expansion can name everything through `$crate` and the
// caller needs no imports of its own.
pub use symbian_gfx;
pub use symbian_sys;
pub use symbian_ui;

/// Re-exported so the macro can name `Box` without the caller having imported it.
pub use alloc::boxed::Box as __Box;

/// `RHeap` aligns to 8 bytes. Anything stricter has to be arranged by hand.
const NATIVE_ALIGN: usize = 8;

/// The allocator, over the shim's non-leaving `User::Alloc`.
///
/// Zero-sized, so it occupies no writable static data — which matters because elf2e32
/// rejects a DLL that has any. We ship EXEs, where it would be allowed, but keeping the
/// property costs nothing and makes the crate reusable if that ever changes.
pub struct Heap;

unsafe impl GlobalAlloc for Heap {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if l.align() <= NATIVE_ALIGN {
            return unsafe { sys::shim_alloc(l.size() as u32) } as *mut u8;
        }
        // Over-allocate and record the shift in the word below the aligned pointer, so
        // dealloc can find the original cell. `RHeap`'s 8-byte guarantee is documented
        // but not something to bet a heap on, and this path costs nothing for the 99% of
        // allocations that never need it.
        let total = l.size() + l.align() + core::mem::size_of::<usize>();
        let raw = unsafe { sys::shim_alloc(total as u32) } as usize;
        if raw == 0 {
            return ptr::null_mut();
        }
        let base = raw + core::mem::size_of::<usize>();
        let aligned = (base + l.align() - 1) & !(l.align() - 1);
        unsafe { *((aligned - core::mem::size_of::<usize>()) as *mut usize) = raw };
        aligned as *mut u8
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        if l.align() <= NATIVE_ALIGN {
            unsafe { sys::shim_free(p as *mut _) };
        } else {
            let raw = unsafe { *((p as usize - core::mem::size_of::<usize>()) as *const usize) };
            unsafe { sys::shim_free(raw as *mut _) };
        }
    }

    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        if l.align() <= NATIVE_ALIGN {
            // Let RHeap grow the cell in place when it can; that is the whole reason to
            // forward realloc rather than alloc-copy-free.
            return unsafe { sys::shim_realloc(p as *mut _, new as u32) } as *mut u8;
        }
        let np = unsafe { self.alloc(Layout::from_size_align_unchecked(new, l.align())) };
        if !np.is_null() {
            unsafe {
                ptr::copy_nonoverlapping(p, np, core::cmp::min(l.size(), new));
                self.dealloc(p, l);
            }
        }
        np
    }
}

/// The panic handler's body. `shim_panic` calls `User::Panic` and never returns.
///
/// A Rust panic must not unwind into C++: we build `panic=abort`, so there are no
/// landing pads on either side of the boundary, and a throw crossing a Rust frame skips
/// every `Drop`.
pub fn panic_to_shim(info: &core::panic::PanicInfo<'_>) -> ! {
    match info.location() {
        Some(l) => unsafe { sys::shim_panic(l.file().as_ptr(), l.file().len() as u32, l.line()) },
        None => unsafe { sys::shim_panic(ptr::null(), 0, 0) },
    }
}

// The three atlases every app links. Held here rather than in each app so a new project
// gets working text without deciding anything, and so all apps agree on what "body" and
// "small" mean. ~58 KB, against 250 MB of device storage.
static UI_BODY: &[u8] = include_bytes!("../../symbian-ui/assets/ui11.sbf");
static UI_STRONG: &[u8] = include_bytes!("../../symbian-ui/assets/ui11b.sbf");
static UI_SMALL: &[u8] = include_bytes!("../../symbian-ui/assets/ui9.sbf");

/// Build a theme and hand it to `f`.
///
/// A closure rather than a return value because `Theme` borrows its font atlases, so it
/// cannot outlive the `BitmapFont`s it points at. The atlases are parsed per call rather
/// than cached in a static — caching one would need a self-referential static, and
/// parsing is a header read plus a bounds check, which is nothing against a full repaint.
pub fn with_theme<R>(palette: Palette, f: impl FnOnce(&Theme<'_>) -> R) -> R {
    let body = BitmapFont::new(UI_BODY).expect("ui11 atlas is malformed");
    let strong = BitmapFont::new(UI_STRONG).expect("ui11b atlas is malformed");
    let small = BitmapFont::new(UI_SMALL).expect("ui9 atlas is malformed");
    let fonts = Fonts { body: &body, strong: &strong, small: &small, title: &strong };
    f(&Theme::new(palette, fonts))
}

/// The screen size the shim reports, or the E72's panel if it is not ready yet.
pub fn screen_size() -> Size {
    let (mut w, mut h) = (0i32, 0i32);
    if unsafe { sys::shim_screen_size(&mut w, &mut h) } != sys::SHIM_OK || w <= 0 || h <= 0 {
        // Only reached before the surface exists, in which case nothing will be drawn
        // anyway — but a sane default beats a zero-sized canvas.
        return Size::new(320, 240);
    }
    Size::new(w, h)
}

/// Translate one shim event into a toolkit key event. `None` for events that are not
/// keys, which is most of them.
pub fn to_key_event(e: &sys::ShimEvent) -> Option<KeyEvent> {
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

/// Copy a shim event into the toolkit's platform-independent view of one.
pub fn to_raw_event(e: &sys::ShimEvent) -> symbian_ui::RawEvent {
    symbian_ui::RawEvent {
        kind: e.kind,
        handle: e.handle,
        status: e.status,
        a: e.a,
        b: e.b,
        c: e.c,
        d: e.d,
        native: e.native,
    }
}

/// Lock the framebuffer, hand a `Canvas` over it to `f`, unlock and present.
///
/// Returns false if the surface was not available, in which case nothing was drawn.
pub fn present(f: impl FnOnce(&mut Canvas<'_>)) -> bool {
    let mut fb = sys::ShimFb::default();
    if unsafe { sys::shim_fb_lock(&mut fb) } != sys::SHIM_OK || fb.pixels.is_null() {
        return false;
    }
    // stride is in bytes and the buffer is RGB565, so two bytes a pixel.
    let stride_px = (fb.stride / 2) as usize;
    let len = stride_px * fb.height as usize;

    // SAFETY: the shim guarantees the pointer is valid for stride*height bytes until
    // shim_fb_unlock, and that nothing else touches it meanwhile. The buffer is ordinary
    // memory the shim allocated, not the FBS chunk, so it does not move under us.
    let pixels: &mut [u16] =
        unsafe { core::slice::from_raw_parts_mut(fb.pixels as *mut u16, len) };
    {
        let mut c = Canvas::new(pixels, Size::new(fb.width, fb.height), stride_px);
        f(&mut c);
    }
    unsafe {
        sys::shim_fb_unlock();
        sys::shim_present(0, 0, fb.width, fb.height);
    }
    true
}

/// Define a device application.
///
/// ```ignore
/// symbian_app::entry!(MyApp::new());
/// symbian_app::entry!(MyApp::new(), palette = symbian_ui::Palette::S60);
/// ```
///
/// Expands to the allocator, the panic handler and `rust_app_start` / `rust_step` /
/// `rust_app_stop`. The app must implement [`symbian_ui::App`].
#[macro_export]
macro_rules! entry {
    ($ctor:expr) => {
        $crate::entry!($ctor, palette = $crate::symbian_ui::Palette::DARK);
    };
    ($ctor:expr, palette = $palette:expr) => {
        #[global_allocator]
        static __SYMBIAN_HEAP: $crate::Heap = $crate::Heap;

        #[panic_handler]
        fn __symbian_panic(info: &core::panic::PanicInfo) -> ! {
            $crate::panic_to_shim(info)
        }

        // A single mutable static, which is exactly the thing Symbian forbids in a DLL —
        // but this is an EXE, where writable static data is unrestricted (elf2e32's check
        // is inside `if (isDllp)`). Threading a context pointer through the C ABI instead
        // would buy nothing: the app is single-threaded by construction, because
        // RWsSession is not thread-safe and all drawing happens on the GUI thread.
        // Boxed as a trait object rather than stored by value, which is what lets
        // `entry!` take one expression instead of a type *and* an expression: a `static`
        // needs a concrete type written out, and `Option<impl App>` is not one.
        //
        // The cost is a vtable call on handle_key, draw and should_exit — three per
        // frame, against ~76,800 pixel writes in the same frame. It is not measurable.
        static mut __SYMBIAN_APP:
            Option<$crate::__Box<dyn $crate::symbian_ui::App>> = None;

        /// Set once the first frame has been drawn, so `rust_step` can tell a genuine
        /// no-op from "we have never painted".
        static mut __SYMBIAN_PAINTED: bool = false;

        #[no_mangle]
        pub extern "C" fn rust_app_start() {
            // SAFETY: called exactly once, from CShimAppUi::ConstructL, on the GUI thread.
            unsafe {
                __SYMBIAN_APP = Some($crate::__Box::new($ctor));
                __SYMBIAN_PAINTED = false;
            }
        }

        #[no_mangle]
        pub extern "C" fn rust_app_stop() {
            // Before the surface goes away, so nothing holds a pointer into it.
            unsafe {
                __SYMBIAN_APP = None;
            }
        }

        #[no_mangle]
        pub extern "C" fn rust_step() {
            use $crate::symbian_ui::App as _;

            // SAFETY: single-threaded; every caller is the GUI thread via the shim.
            let app: &mut dyn $crate::symbian_ui::App =
                match unsafe { (&raw mut __SYMBIAN_APP).as_mut() } {
                    Some(slot) => match slot.as_mut() {
                        Some(b) => &mut **b,
                        None => return,
                    },
                    None => return,
                };

            let size = $crate::screen_size();
            let screen = $crate::symbian_gfx::Rect::from_size(size);

            // One theme for the whole step: the fonts are parsed once per frame rather
            // than once per key, and — the reason it has to be this shape — a Theme
            // borrows the atlases, so it cannot escape the closure that owns them.
            let dirty = $crate::with_theme($palette, |theme| {
                let mut dirty = unsafe { !__SYMBIAN_PAINTED };
                let mut ev = $crate::symbian_sys::ShimEvent::default();

                // Drain the whole queue before drawing. Coalescing several key presses
                // into one repaint is the difference between keeping up and falling
                // behind when someone holds a key down.
                while unsafe { $crate::symbian_sys::shim_poll_event(&mut ev) } == 1 {
                    match ev.kind {
                        $crate::symbian_sys::SHIM_EV_RESIZE
                        | $crate::symbian_sys::SHIM_EV_REDRAW => dirty = true,
                        $crate::symbian_sys::SHIM_EV_QUIT => {
                            unsafe { $crate::symbian_sys::shim_request_exit() };
                            return false;
                        }
                        _ => {
                            // Raw first. An app that consumes it does not also get a
                            // translated key, which is what lets a diagnostic see the
                            // numbers the platform sent rather than our reading of them.
                            let raw = $crate::to_raw_event(&ev);
                            if app.handle_raw(&raw) == $crate::symbian_ui::Handled::Consumed {
                                dirty = true;
                            } else if let Some(k) = $crate::to_key_event(&ev) {
                                if app.handle_key(k, theme, screen)
                                    == $crate::symbian_ui::Handled::Consumed
                                {
                                    dirty = true;
                                }
                            }
                        }
                    }
                }
                dirty
            });

            if app.should_exit() {
                unsafe { $crate::symbian_sys::shim_request_exit() };
                return;
            }
            if !dirty {
                return;
            }

            $crate::with_theme($palette, |theme| {
                $crate::present(|c| app.draw(c, theme));
            });
            unsafe { __SYMBIAN_PAINTED = true };
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_atlases_the_macro_links_are_parseable() {
        // A malformed atlas would panic inside with_theme on the device, at the first
        // repaint, with no log — so it is checked here, where the failure is a test name.
        for (name, data) in [("ui11", UI_BODY), ("ui11b", UI_STRONG), ("ui9", UI_SMALL)] {
            let f = BitmapFont::new(data);
            assert!(f.is_ok(), "{name}.sbf is malformed: {:?}", f.err());
        }
    }

    #[test]
    fn a_char_event_becomes_a_char_key() {
        let mut ev = sys::ShimEvent::default();
        ev.kind = sys::SHIM_EV_KEY_CHAR;
        ev.a = 'q' as i32;
        let k = to_key_event(&ev).expect("a printable char must translate");
        assert_eq!(k.key, Key::Char('q'));
        assert!(!k.mods.shift);
    }

    #[test]
    fn navigation_keys_translate_by_id() {
        for (id, want) in [
            (sys::key::UP, Key::Up),
            (sys::key::DOWN, Key::Down),
            (sys::key::SELECT, Key::Select),
            (sys::key::SOFT_LEFT, Key::Softkey(Softkey::Left)),
            (sys::key::SOFT_RIGHT, Key::Softkey(Softkey::Right)),
        ] {
            let mut ev = sys::ShimEvent::default();
            ev.kind = sys::SHIM_EV_KEY_DOWN;
            ev.a = id;
            assert_eq!(to_key_event(&ev).unwrap().key, want);
        }
    }

    #[test]
    fn an_unknown_key_id_survives_as_raw() {
        // Rather than being dropped. A silently discarded key is how the E72's Fn key
        // stayed invisible through two rounds of on-device debugging.
        let mut ev = sys::ShimEvent::default();
        ev.kind = sys::SHIM_EV_KEY_DOWN;
        ev.a = 0x4242;
        assert_eq!(to_key_event(&ev).unwrap().key, Key::Raw(0x4242));
    }

    #[test]
    fn modifiers_come_from_the_portable_summary() {
        let mut ev = sys::ShimEvent::default();
        ev.kind = sys::SHIM_EV_KEY_CHAR;
        ev.a = 'a' as i32;
        ev.b = sys::modifier::SHIFT | sys::modifier::FUNC;
        let k = to_key_event(&ev).unwrap();
        assert!(k.mods.shift && k.mods.func && !k.mods.ctrl);
    }

    #[test]
    fn non_key_events_do_not_translate() {
        for kind in [sys::SHIM_EV_REDRAW, sys::SHIM_EV_RESIZE, sys::SHIM_EV_TIMER] {
            let mut ev = sys::ShimEvent::default();
            ev.kind = kind;
            assert!(to_key_event(&ev).is_none(), "kind {kind} should not be a key");
        }
    }

    #[test]
    fn a_lone_surrogate_is_rejected_rather_than_becoming_a_replacement_char() {
        // The shim carries a UCS-2 code unit, and a surrogate half is not a scalar.
        // Turning it into U+FFFD would put a visible box in someone's message; dropping
        // it loses one keystroke of a character that cannot be typed on this keyboard
        // anyway.
        let mut ev = sys::ShimEvent::default();
        ev.kind = sys::SHIM_EV_KEY_CHAR;
        ev.a = 0xD800;
        assert!(to_key_event(&ev).is_none());
    }

    #[test]
    fn repeat_is_reported() {
        let mut ev = sys::ShimEvent::default();
        ev.kind = sys::SHIM_EV_KEY_CHAR;
        ev.a = 'x' as i32;
        ev.c = 3;
        assert!(to_key_event(&ev).unwrap().repeat);
    }

    #[test]
    fn screen_size_falls_back_to_the_e72_panel() {
        // On the host every shim extern is a stub returning NOT_READY, so this exercises
        // exactly the path a device takes before its surface exists.
        assert_eq!(screen_size(), Size::new(320, 240));
    }
}
