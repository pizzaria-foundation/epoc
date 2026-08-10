//! Raw FFI bindings to the Symbian C++ shim.
//!
//! A hand-written mirror of `shim/inc/symbian_shim.h`. Hand-written rather than
//! bindgen-generated because the header is small, stable, and the *comments* in it
//! are the contract — a generator would drop them and the next person would have
//! to rediscover why `shim_fb_lock` must be paired with `shim_fb_unlock` before
//! anything else is called.
//!
//! Nothing here is safe. Safe wrappers belong a layer up; this crate exists so the
//! unsafe surface is countable and in one file.
//!
//! # Building for the host
//!
//! The externs only exist when linking against the shim, so on the host they are
//! behind `cfg(target_vendor = "symbian")` — the vendor field the target JSON
//! sets. That keeps `cargo test --workspace` working, and means a mistake in this
//! file is a compile error on the host rather than a link error on the device,
//! where the feedback loop is a Bluetooth transfer away.

#![cfg_attr(not(test), no_std)]

use core::ffi::c_void;

// ------------------------------------------------------------------- errors --
// A subset of e32err.h. KErrPermissionDenied is the one to expect when a
// capability is missing from the SIS.

pub const SHIM_OK: i32 = 0;
pub const SHIM_ERR_NOT_FOUND: i32 = -1;
pub const SHIM_ERR_GENERAL: i32 = -2;
pub const SHIM_ERR_CANCEL: i32 = -3;
pub const SHIM_ERR_NO_MEMORY: i32 = -4;
pub const SHIM_ERR_NOT_SUPPORTED: i32 = -5;
pub const SHIM_ERR_ARGUMENT: i32 = -6;
pub const SHIM_ERR_BAD_HANDLE: i32 = -8;
pub const SHIM_ERR_OVERFLOW: i32 = -9;
pub const SHIM_ERR_ALREADY_EXISTS: i32 = -11;
pub const SHIM_ERR_IN_USE: i32 = -14;
pub const SHIM_ERR_NOT_READY: i32 = -18;
pub const SHIM_ERR_ACCESS_DENIED: i32 = -21;
pub const SHIM_ERR_EOF: i32 = -25;
pub const SHIM_ERR_TIMED_OUT: i32 = -33;
pub const SHIM_ERR_DISCONNECTED: i32 = -36;
pub const SHIM_ERR_PERMISSION: i32 = -46;

// ------------------------------------------------------------------- events --

pub const SHIM_EV_NONE: i32 = 0;
/// Translated character in `a` — the window server has already applied Shift,
/// Caps Lock and the Fn layer, so this stream *is* text input.
pub const SHIM_EV_KEY_CHAR: i32 = 1;
/// A key with no character: `a` carries one of the `key::*` ids below.
pub const SHIM_EV_KEY_DOWN: i32 = 2;
pub const SHIM_EV_KEY_UP: i32 = 3;
pub const SHIM_EV_REDRAW: i32 = 4;
pub const SHIM_EV_RESIZE: i32 = 5;
pub const SHIM_EV_FOCUS: i32 = 6;
pub const SHIM_EV_TIMER: i32 = 10;
pub const SHIM_EV_CONNECTED: i32 = 20;
pub const SHIM_EV_RECV: i32 = 21;
pub const SHIM_EV_SENT: i32 = 22;
pub const SHIM_EV_CLOSED: i32 = 23;
pub const SHIM_EV_RESOLVED: i32 = 24;
/// `RConnection` is up. `a` is the IAP the OS chose — persist it and pass it back to
/// [`shim_net_start`] next time to connect without prompting.
pub const SHIM_EV_NET_READY: i32 = 25;
/// A worker-thread job finished; `status` is what `rust_work` returned.
pub const SHIM_EV_WORK_DONE: i32 = 30;
/// An image decode finished. `a` and `b` are the decoded width and height — what the
/// codec could deliver, which is bounded by the request but rarely equal to it, since
/// the ICL only reduces by powers of two.
pub const SHIM_EV_IMAGE_DONE: i32 = 40;
/// A clip finished opening. `a` is its duration in milliseconds.
pub const SHIM_EV_AUDIO_OPENED: i32 = 41;
/// Playback ended. `status` is `SHIM_OK` at the natural end, `SHIM_ERR_CANCEL` when
/// stopped, and a real error otherwise; `d` carries the platform's raw code.
pub const SHIM_EV_AUDIO_DONE: i32 = 42;
/// A subscribed Publish & Subscribe property changed. `a` is the key within the app's
/// category, `c` is the freshly read integer value. Emitted by `shim_prop`'s subscriber.
/// A headless daemon uses this as its stop signal — whoever launched it sets the property,
/// this arrives.
pub const SHIM_EV_PROP: i32 = 53;
pub const SHIM_EV_QUIT: i32 = 90;

/// The Publish & Subscribe key an app publishes its present-efficiency telemetry under, in
/// its **own** UID3 category ([`shim_own_uid3`]). Writing your own category needs no
/// capability, and the dev bridge reads it back in the same process — so this is the cheap,
/// no-disk-I/O channel for the `[gfx]` metric, cheap enough to publish from the frame it
/// measures. The value is the percentage of blit cost the dirty-rect present *saved*
/// (0–100). Key 0 is the daemon stop flag, so telemetry starts at 1.
pub const PS_KEY_GFX: u32 = 1;

/// The Publish & Subscribe key an app publishes its `rust_step` watchdog under, in its own
/// UID3 category (same free-write/free-read reasoning as [`PS_KEY_GFX`]). The value packs the
/// worst *handle-phase* time in the high 16 bits and the worst *draw-phase* time in the low
/// 16 (both milliseconds) — a long step freezes the whole phone, and the split says whether
/// the cause is event/input work or rendering.
pub const PS_KEY_STEP: u32 = 2;

/// Publish & Subscribe key for the receive-path breakdown (a fine cut of the step's handle
/// phase): worst decrypt time in the high 16 bits, worst unwrap (inflate + TL parse) in the
/// low 16 (both milliseconds). Says whether a network-processing stall is the offloadable
/// crypto or the allocation-heavy parse.
pub const PS_KEY_RECV: u32 = 3;

/// Publish & Subscribe key for the receive **handle** split at the protocol-driver level:
/// worst `feed` time (decode + parse of an incoming batch) in the high 16 bits, worst
/// `process` time (draining steps / building the model) in the low 16 (both ms). The safe,
/// driver-level cut — no per-packet work, no change to the portable protocol crate.
pub const PS_KEY_NET: u32 = 4;


/// Abstract key ids, mirroring `TShimKey` in `shim_app.cpp`.
///
/// Deliberately above `0x110000` so they cannot collide with a Unicode scalar:
/// `SHIM_EV_KEY_CHAR` puts a real codepoint in the same field, and a numeric
/// overlap would turn a keypress into a stray character.
pub mod key {
    pub const UP: i32 = 0x110000;
    pub const DOWN: i32 = 0x110001;
    pub const LEFT: i32 = 0x110002;
    pub const RIGHT: i32 = 0x110003;
    pub const SELECT: i32 = 0x110004;
    pub const SOFT_LEFT: i32 = 0x110005;
    pub const SOFT_MIDDLE: i32 = 0x110006;
    pub const SOFT_RIGHT: i32 = 0x110007;
    pub const BACKSPACE: i32 = 0x110008;
    pub const DELETE: i32 = 0x110009;
    pub const ENTER: i32 = 0x11000A;
    pub const CALL: i32 = 0x11000B;
    pub const END: i32 = 0x11000C;
}

// ---------------------------------------------------------------- keyboard --
// Which mechanism turns a physical key into a character.

/// The shim's own scan-code table; tested on hardware. Does not produce the Fn
/// symbol layer.
pub const SHIM_KEYBOARD_SCAN: i32 = 0;
/// Symbian's front-end processor. Produces dead-key composition (á, ã, ê) and the
/// full Fn/Chr symbol layer.
pub const SHIM_KEYBOARD_FEP: i32 = 1;

/// Modifier bits in `ShimEvent::b`.
pub mod modifier {
    pub const SHIFT: i32 = 1;
    pub const CTRL: i32 = 2;
    /// The E72's Fn/Chr key, which is how digits and symbols are reached.
    pub const FUNC: i32 = 4;
}

/// Fixed-size and POD so the C++ side can push one from inside a `CActive::RunL`
/// without allocating — which it must not do, because RunL runs with the cleanup
/// stack in an unknown state.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ShimEvent {
    pub kind: i32,
    /// Which socket, timer or window; 0 when global.
    pub handle: i32,
    /// Symbian error code, `SHIM_OK` on success.
    pub status: i32,
    pub a: i32,
    pub b: i32,
    pub c: i32,
    pub d: i32,
    /// Platform-native extra; for key events, the raw Symbian `iModifiers` word.
    ///
    /// `b` is the portable three-bit summary and is what apps should use. This is
    /// here because a summary is what hid the E72 keyboard bug: `b` was 00 for every
    /// key, which only meant "not shift, ctrl or func" — it could not distinguish a
    /// clean event from one carrying `EModifierNumLock`.
    pub native: i32,
}

// ------------------------------------------------------------------ network --

/// Let the OS ask the user which access point to use.
pub const SHIM_IAP_PROMPT: i32 = -1;
/// Take the configured default without asking.
pub const SHIM_IAP_DEFAULT: i32 = -2;
/// Join a connection that is already up. See the shim header.
pub const SHIM_IAP_ATTACH: i32 = -3;

// --------------------------------------------------------------------- files --

pub const SHIM_FILE_READ: i32 = 0x01;
pub const SHIM_FILE_WRITE: i32 = 0x02;
pub const SHIM_FILE_CREATE: i32 = 0x04;
pub const SHIM_FILE_APPEND: i32 = 0x08;

// -------------------------------------------------------------- framebuffer --

pub const SHIM_PF_RGB565: i32 = 1;
pub const SHIM_PF_XRGB8888: i32 = 2;

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct ShimFb {
    pub pixels: *mut u8,
    /// BYTES per scanline. Read it; never compute it from the width.
    pub stride: i32,
    pub width: i32,
    pub height: i32,
    /// Always `SHIM_PF_RGB565`: the shim converts on present if the screen is
    /// 32bpp, so Rust only ever sees one format.
    pub format: i32,
}

impl Default for ShimFb {
    fn default() -> Self {
        Self { pixels: core::ptr::null_mut(), stride: 0, width: 0, height: 0, format: 0 }
    }
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
pub struct ShimFontMetrics {
    pub height: i32,
    pub ascent: i32,
    pub descent: i32,
    pub max_width: i32,
}

// ---------------------------------------------------------------- the shim --

#[cfg(target_vendor = "symbian")]
extern "C" {
    // alloc — none of these can leave, which is why the shim calls User::Alloc
    // and never User::AllocL: OOM must be a null pointer, not a C++ throw
    // unwinding through Rust frames that have no landing pads.
    pub fn shim_alloc(size: u32) -> *mut c_void;
    pub fn shim_realloc(p: *mut c_void, size: u32) -> *mut c_void;
    pub fn shim_free(p: *mut c_void);
    pub fn shim_alloc_len(p: *const c_void) -> u32;

    // events
    pub fn shim_poll_event(out: *mut ShimEvent) -> i32;
    pub fn shim_events_dropped() -> i32;

    // lifecycle
    pub fn shim_request_exit();

    // framebuffer
    pub fn shim_fb_lock(out: *mut ShimFb) -> i32;
    pub fn shim_fb_unlock();
    pub fn shim_present(x: i32, y: i32, w: i32, h: i32) -> i32;
    pub fn shim_screen_size(w: *mut i32, h: *mut i32) -> i32;
    pub fn shim_screen_format(format: *mut i32) -> i32;
    pub fn shim_probe_pixel_layout(out_word: *mut u32) -> i32;
    /// `SHIM_OK` if the named DLL loads on this device, else the Symbian error
    /// (`KErrNotFound` is -1). `name` is UTF-16, e.g. `libcrypto.dll`.
    pub fn shim_dll_present(name: *const u16, len: i32) -> i32;
    /// This process's own UID3, used as its Publish & Subscribe telemetry category.
    pub fn shim_own_uid3() -> u32;
    pub fn shim_entropy(out: *mut u8, len: i32) -> i32;

    // files
    pub fn shim_private_path(buf: *mut u16, cap: i32, len: *mut i32) -> i32;
    pub fn shim_file_open(path: *const u16, len: i32, mode: i32, handle: *mut i32) -> i32;
    pub fn shim_file_read(handle: i32, buf: *mut u8, cap: i32, got: *mut i32) -> i32;
    pub fn shim_file_write(handle: i32, buf: *const u8, len: i32) -> i32;
    pub fn shim_file_size(handle: i32, out: *mut i64) -> i32;
    pub fn shim_file_seek(handle: i32, pos: i64) -> i32;
    pub fn shim_file_delete(path: *const u16, len: i32) -> i32;
    pub fn shim_file_rename(from: *const u16, from_len: i32, to: *const u16, to_len: i32) -> i32;
    pub fn shim_file_close(handle: i32);

    // network
    pub fn shim_net_connections() -> i32;
    pub fn shim_net_connection_iap(index: i32, iap: *mut i32) -> i32;
    pub fn shim_net_start(iap: i32, handle: *mut i32) -> i32;
    pub fn shim_net_stop(handle: i32);
    pub fn shim_dns_resolve(conn: i32, host: *const u16, len: i32, handle: *mut i32) -> i32;
    pub fn shim_dns_close(handle: i32);
    pub fn shim_tcp_open(conn: i32, handle: *mut i32) -> i32;
    pub fn shim_tcp_connect(handle: i32, ipv4: u32, port: u16) -> i32;
    pub fn shim_tcp_send(handle: i32, buf: *const u8, len: i32) -> i32;
    pub fn shim_tcp_recv(handle: i32, buf: *mut u8, cap: i32) -> i32;
    pub fn shim_tcp_close(handle: i32);
    pub fn shim_udp_open(conn: i32, handle: *mut i32) -> i32;
    pub fn shim_udp_send_to(handle: i32, buf: *const u8, len: i32, ipv4: u32, port: u16) -> i32;
    pub fn shim_udp_recv_from(handle: i32, buf: *mut u8, cap: i32) -> i32;

    // worker thread
    pub fn shim_work_submit(
        opcode: i32,
        input: *const u8,
        in_len: i32,
        out: *mut u8,
        out_len: i32,
    ) -> i32;
    pub fn shim_work_busy() -> i32;

    // timers
    pub fn shim_timer_after(ms: i32, handle: *mut i32) -> i32;
    pub fn shim_timer_every(ms: i32, handle: *mut i32) -> i32;
    pub fn shim_timer_cancel(handle: i32);
    pub fn shim_now_us() -> u64;
    pub fn shim_unix_time() -> i64;
    pub fn shim_utc_offset() -> i32;

    // image
    pub fn shim_image_probe(path: *const u16, path_len: i32, w: *mut i32, h: *mut i32) -> i32;
    pub fn shim_image_decode_start(
        path: *const u16,
        path_len: i32,
        max_w: i32,
        max_h: i32,
        handle: *mut i32,
    ) -> i32;
    pub fn shim_image_decode_start_mem(
        data: *const u8,
        len: i32,
        max_w: i32,
        max_h: i32,
        handle: *mut i32,
    ) -> i32;
    pub fn shim_image_result(
        handle: i32,
        out: *mut u16,
        out_cap: i32,
        w: *mut i32,
        h: *mut i32,
    ) -> i32;
    pub fn shim_image_describe(handle: i32, out: *mut i32, cap: i32) -> i32;
    pub fn shim_image_close(handle: i32);

    // audio
    pub fn shim_audio_open_file(path: *const u16, path_len: i32) -> i32;
    pub fn shim_audio_play() -> i32;
    pub fn shim_audio_pause() -> i32;
    pub fn shim_audio_stop() -> i32;
    pub fn shim_audio_position_ms() -> i32;
    pub fn shim_audio_duration_ms() -> i32;
    pub fn shim_audio_set_volume(percent: i32) -> i32;
    pub fn shim_audio_close() -> i32;

    // diagnostics
    pub fn shim_panic(file: *const u8, file_len: u32, line: u32) -> !;
    pub fn shim_debug(text: *const u16, len: i32);

    // keyboard
    pub fn shim_keyboard_mode(mode: i32) -> i32;
    pub fn shim_keyboard_mode_get() -> i32;

    // directory create + listing
    pub fn shim_mkdir(path: *const u16, path_len: i32) -> i32;
    pub fn shim_dir_list(path: *const u16, path_len: i32, buf: *mut u16, cap: i32, count: *mut i32) -> i32;

    // app-lifecycle monitor (USE_APPMON) — window-group + focus changes
    pub fn shim_process_start(path: *const u16, path_len: i32) -> i32;
    /// Whether a process built from the given UID3 is currently running. Returns 1 for
    /// running, 0 for not, negative on error.
    pub fn shim_process_running(uid3: u32) -> i32;

    // memory readings (USE_MEM) — device-wide RAM and this process's own heap, in KiB.
    /// Free device RAM in KiB, or a negative Symbian error. The figure to watch for pressure.
    pub fn shim_mem_free_kb() -> i32;
    /// Total device RAM in KiB, or a negative error.
    pub fn shim_mem_total_kb() -> i32;
    /// Bytes this process has allocated, in KiB. There is no way to ask this of another
    /// process, so no caller can attribute RAM per app: the figures are device-wide, plus
    /// this process's own heap.
    pub fn shim_heap_used_kb() -> i32;

    // task control (USE_TASK) — close/kill/foreground/enumerate apps by UID3.
    /// Ask the app with this UID3 to close cooperatively (`TApaTask::EndTask`). No
    /// capability. `SHIM_ERR_NOT_FOUND` if no running task has the UID.
    pub fn shim_prop_define(category: u32, key: u32) -> i32;
    /// Set the integer value of a property.
    pub fn shim_prop_set(category: u32, key: u32, value: i32) -> i32;
    /// Read the integer value of a property into `*out`.
    pub fn shim_prop_get(category: u32, key: u32, out: *mut i32) -> i32;
    /// Subscribe to a property; every change posts a [`SHIM_EV_PROP`] carrying the key and
    /// the freshly read value. One outstanding subscription per key.
    pub fn shim_prop_subscribe(category: u32, key: u32) -> i32;
    /// Cancel the subscription started by [`shim_prop_subscribe`].
    pub fn shim_prop_unsubscribe(category: u32, key: u32);
}

// Host stubs, so this crate compiles and the workspace tests run. They abort
// rather than return, because reaching one means a host binary tried to talk to a
// phone and every subsequent value would be a lie.
#[cfg(not(target_vendor = "symbian"))]
mod host_stubs {
    use super::*;

    macro_rules! nope {
        ($name:literal) => {
            panic!(concat!($name, " is only available on the device"))
        };
    }

    pub unsafe fn shim_alloc(_size: u32) -> *mut c_void {
        nope!("shim_alloc")
    }
    pub unsafe fn shim_realloc(_p: *mut c_void, _size: u32) -> *mut c_void {
        nope!("shim_realloc")
    }
    pub unsafe fn shim_free(_p: *mut c_void) {
        nope!("shim_free")
    }
    pub unsafe fn shim_alloc_len(_p: *const c_void) -> u32 {
        nope!("shim_alloc_len")
    }
    pub unsafe fn shim_poll_event(_out: *mut ShimEvent) -> i32 {
        0
    }
    pub unsafe fn shim_events_dropped() -> i32 {
        0
    }
    pub unsafe fn shim_request_exit() {}
    pub unsafe fn shim_fb_lock(_out: *mut ShimFb) -> i32 {
        SHIM_ERR_NOT_READY
    }
    pub unsafe fn shim_fb_unlock() {}
    pub unsafe fn shim_present(_x: i32, _y: i32, _w: i32, _h: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    pub unsafe fn shim_screen_size(_w: *mut i32, _h: *mut i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    pub unsafe fn shim_screen_format(_f: *mut i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    pub unsafe fn shim_probe_pixel_layout(_w: *mut u32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    pub unsafe fn shim_dll_present(_name: *const u16, _len: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    pub unsafe fn shim_own_uid3() -> u32 {
        0
    }
    pub unsafe fn shim_entropy(out: *mut u8, len: i32) -> i32 {
        /* The host stub is the one place a fake entropy source is defensible, and it is
         * still worth being loud about: this is a counter, not entropy. Host tests must
         * exercise the DRBG's *shape* -- that it advances, that it never repeats a block --
         * and never its unpredictability, which cannot be tested here anyway. */
        if out.is_null() || len <= 0 {
            return SHIM_ERR_ARGUMENT;
        }
        for i in 0..len {
            core::ptr::write(out.add(i as usize), (i as u8).wrapping_mul(31).wrapping_add(7));
        }
        SHIM_OK
    }
    pub unsafe fn shim_private_path(_b: *mut u16, _c: i32, _l: *mut i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    pub unsafe fn shim_file_open(_p: *const u16, _l: i32, _m: i32, _h: *mut i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    pub unsafe fn shim_file_read(_h: i32, _b: *mut u8, _c: i32, _g: *mut i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    pub unsafe fn shim_file_write(_h: i32, _b: *const u8, _l: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    pub unsafe fn shim_file_size(_h: i32, _o: *mut i64) -> i32 {
        SHIM_ERR_NOT_READY
    }
    pub unsafe fn shim_file_seek(_h: i32, _p: i64) -> i32 {
        SHIM_ERR_NOT_READY
    }
    pub unsafe fn shim_file_delete(_p: *const u16, _l: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    pub unsafe fn shim_file_rename(
        _f: *const u16,
        _fl: i32,
        _t: *const u16,
        _tl: i32,
    ) -> i32 {
        SHIM_ERR_NOT_READY
    }
    pub unsafe fn shim_file_close(_h: i32) {}
    pub unsafe fn shim_net_connections() -> i32 {
        0
    }
    pub unsafe fn shim_net_connection_iap(_i: i32, iap: *mut i32) -> i32 {
        if !iap.is_null() {
            *iap = -1;
        }
        SHIM_ERR_NOT_FOUND
    }
    pub unsafe fn shim_net_start(_iap: i32, _h: *mut i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    pub unsafe fn shim_net_stop(_h: i32) {}
    pub unsafe fn shim_dns_resolve(
        _c: i32,
        _host: *const u16,
        _l: i32,
        _h: *mut i32,
    ) -> i32 {
        SHIM_ERR_NOT_READY
    }
    pub unsafe fn shim_dns_close(_h: i32) {}
    pub unsafe fn shim_tcp_open(_c: i32, _h: *mut i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    pub unsafe fn shim_tcp_connect(_h: i32, _ip: u32, _p: u16) -> i32 {
        SHIM_ERR_NOT_READY
    }
    pub unsafe fn shim_tcp_send(_h: i32, _b: *const u8, _l: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    pub unsafe fn shim_tcp_recv(_h: i32, _b: *mut u8, _c: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    pub unsafe fn shim_tcp_close(_h: i32) {}
    pub unsafe fn shim_udp_open(_c: i32, _h: *mut i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    pub unsafe fn shim_udp_send_to(
        _h: i32,
        _b: *const u8,
        _l: i32,
        _ip: u32,
        _p: u16,
    ) -> i32 {
        SHIM_ERR_NOT_READY
    }
    pub unsafe fn shim_udp_recv_from(_h: i32, _b: *mut u8, _c: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    pub unsafe fn shim_work_submit(
        _op: i32,
        _in: *const u8,
        _il: i32,
        _out: *mut u8,
        _ol: i32,
    ) -> i32 {
        SHIM_ERR_NOT_READY
    }
    pub unsafe fn shim_work_busy() -> i32 {
        0
    }
    pub unsafe fn shim_timer_after(_ms: i32, _h: *mut i32) -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    pub unsafe fn shim_timer_every(_ms: i32, _h: *mut i32) -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    pub unsafe fn shim_timer_cancel(_h: i32) {}
    pub unsafe fn shim_now_us() -> u64 {
        0
    }
    pub unsafe fn shim_unix_time() -> i64 {
        0
    }
    pub unsafe fn shim_utc_offset() -> i32 {
        0
    }
    pub unsafe fn shim_image_probe(_p: *const u16, _l: i32, _w: *mut i32, _h: *mut i32) -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    pub unsafe fn shim_image_decode_start(
        _p: *const u16,
        _l: i32,
        _mw: i32,
        _mh: i32,
        _h: *mut i32,
    ) -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    pub unsafe fn shim_image_decode_start_mem(
        _d: *const u8,
        _l: i32,
        _mw: i32,
        _mh: i32,
        _h: *mut i32,
    ) -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    pub unsafe fn shim_image_result(
        _h: i32,
        _o: *mut u16,
        _cap: i32,
        _w: *mut i32,
        _ht: *mut i32,
    ) -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    pub unsafe fn shim_image_describe(_h: i32, _o: *mut i32, _c: i32) -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    pub unsafe fn shim_image_close(_h: i32) {}
    pub unsafe fn shim_audio_open_file(_p: *const u16, _l: i32) -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    pub unsafe fn shim_audio_play() -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    pub unsafe fn shim_audio_pause() -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    pub unsafe fn shim_audio_stop() -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    pub unsafe fn shim_audio_position_ms() -> i32 {
        0
    }
    pub unsafe fn shim_audio_duration_ms() -> i32 {
        0
    }
    pub unsafe fn shim_audio_set_volume(_p: i32) -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    pub unsafe fn shim_audio_close() -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    pub unsafe fn shim_panic(_f: *const u8, _l: u32, _line: u32) -> ! {
        nope!("shim_panic")
    }
    pub unsafe fn shim_debug(_t: *const u16, _l: i32) {}
    pub unsafe fn shim_keyboard_mode(_mode: i32) -> i32 {
        0
    }
    pub unsafe fn shim_keyboard_mode_get() -> i32 {
        SHIM_KEYBOARD_SCAN
    }
    pub unsafe fn shim_mkdir(_p: *const u16, _pl: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    pub unsafe fn shim_dir_list(_p: *const u16, _pl: i32, _b: *mut u16, _c: i32, count: *mut i32) -> i32 {
        if !count.is_null() {
            core::ptr::write(count, 0);
        }
        SHIM_ERR_NOT_READY
    }
    pub unsafe fn shim_process_start(_p: *const u16, _pl: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    pub unsafe fn shim_process_running(_uid3: u32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    pub unsafe fn shim_mem_free_kb() -> i32 {
        SHIM_ERR_NOT_READY
    }
    pub unsafe fn shim_mem_total_kb() -> i32 {
        SHIM_ERR_NOT_READY
    }
    pub unsafe fn shim_heap_used_kb() -> i32 {
        SHIM_ERR_NOT_READY
    }
    pub unsafe fn shim_prop_define(_c: u32, _k: u32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    pub unsafe fn shim_prop_set(_c: u32, _k: u32, _v: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    pub unsafe fn shim_prop_get(_c: u32, _k: u32, out: *mut i32) -> i32 {
        if !out.is_null() {
            core::ptr::write(out, 0);
        }
        SHIM_ERR_NOT_READY
    }
    pub unsafe fn shim_prop_subscribe(_c: u32, _k: u32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    pub unsafe fn shim_prop_unsubscribe(_c: u32, _k: u32) {}
}

#[cfg(not(target_vendor = "symbian"))]
pub use host_stubs::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_ids_cannot_be_mistaken_for_characters() {
        // SHIM_EV_KEY_CHAR puts a Unicode scalar in the same field these ids use.
        // Unicode ends at 0x10FFFF, so every id must sit above it or a keypress
        // could be decoded as text.
        let ids = [
            key::UP, key::DOWN, key::LEFT, key::RIGHT, key::SELECT,
            key::SOFT_LEFT, key::SOFT_MIDDLE, key::SOFT_RIGHT,
            key::BACKSPACE, key::DELETE, key::ENTER, key::CALL, key::END,
        ];
        for id in ids {
            assert!(id > 0x10FFFF, "{id:#x} overlaps the Unicode range");
            assert!(char::from_u32(id as u32).is_none());
        }
    }

    #[test]
    fn key_ids_are_distinct() {
        let ids = [
            key::UP, key::DOWN, key::LEFT, key::RIGHT, key::SELECT,
            key::SOFT_LEFT, key::SOFT_MIDDLE, key::SOFT_RIGHT,
            key::BACKSPACE, key::DELETE, key::ENTER, key::CALL, key::END,
        ];
        for (i, a) in ids.iter().enumerate() {
            for b in &ids[i + 1..] {
                assert_ne!(a, b, "duplicate key id {a:#x}");
            }
        }
    }

    #[test]
    fn event_struct_is_the_size_the_c_side_writes() {
        // 8 int32 fields, no padding. If this ever fails, the C++ struct and this
        // one have drifted and every event would be misread.
        assert_eq!(core::mem::size_of::<ShimEvent>(), 32);
        assert_eq!(core::mem::align_of::<ShimEvent>(), 4);
    }
}
