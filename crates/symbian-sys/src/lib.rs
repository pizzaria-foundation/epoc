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
//! # Safety
//!
//! Every function in this crate shares one contract, which is why each of them carries a
//! one-line `# Safety` section pointing back here rather than a paragraph of its own: they
//! are all the same door into the same C++ shim, and the invariants are properties of that
//! door, not of any one function.
//!
//! * **Buffers are (pointer, length) pairs, and the length has to be the truth.** Strings
//!   cross as `(*const u16, i32)` — UTF-16 code units, which `TPtrC16` wraps with no copy —
//!   and output buffers as `(*mut T, i32 capacity, *mut i32 written)`. The shim wraps the
//!   pointer in a descriptor whose *maximum length* is the number you passed, so a capacity
//!   larger than the allocation is a write past the end of it, on the C++ side, where Rust's
//!   bounds checks are not. A capacity smaller than the allocation is merely a short read.
//!
//! * **Null is usually an error rather than undefined behaviour, but do not lean on it.**
//!   Most shim entry points open with a null check and return `SHIM_ERR_ARGUMENT` — see
//!   `shim_file_read` or `shim_prop_get`. Not all of them do: `shim_cell_get` hands its five
//!   out-pointers straight to the C++ object behind them. Nobody has swept the file for the
//!   exceptions, so treat a null pointer as undefined and pass real ones.
//!
//! * **Handles are opaque `i32`, and a stale one is caught.** They are slots in a table, never
//!   pointers, so a handle from a closed file, socket or statement returns
//!   `SHIM_ERR_BAD_HANDLE` rather than reaching a freed C++ object. The file, image, audio and
//!   SQL tables also tag the slot with a generation counter, so a handle whose slot has been
//!   reused by something else is rejected too rather than silently addressing the new
//!   occupant. Handles are the one thing here that a caller cannot misuse into UB.
//!
//! * **A leave never crosses back.** Every `shim_*` function is a TRAP barrier that returns
//!   a Symbian error code. A Leave is a longjmp-style unwind that runs no destructors, and
//!   letting one cross a Rust frame compiled `panic=abort` — which has no landing pads —
//!   skips every `Drop`. The shim keeps the leaving work in a private `DoSomethingL()`.
//!
//! * **The GUI thread owns the screen.** `RWsSession` is not thread safe and window
//!   operations must run on the thread that owns the window group, so everything touching
//!   the framebuffer, the window server or Avkon has to be called from the thread the shim
//!   pumps `rust_step` on. The worker thread ([`shim_work_submit`]) has its own heap, so
//!   memory allocated there and freed on the GUI thread is a cross-heap free — silent
//!   corruption rather than a clean failure.
//!
//! * **Nothing returned outlives the next call.** The pixel pointer from [`shim_fb_lock`] is
//!   valid only until [`shim_fb_unlock`] — `CFbsBitmap::DataAddress()` has to be preceded by
//!   `BeginDataAccess()` and the font-and-bitmap server's heap may compact in between — and
//!   the cached tables some probes return are replaced on their next refresh. Copy what you
//!   need; do not hold a pointer across another shim call.
//!
//! Whether a *particular* function must be on the GUI thread, and what it does with the
//! buffers it is given, is documented on that function's declaration in the `extern` block
//! below, which mirrors the comments in `shim/inc/symbian_shim.h` — that header is the
//! contract, and this file is its Rust half.
//!
//! The per-function `# Safety` lines name parameters the way the header declares them, which is
//! not always the way the host stub below spells them: the stub ignores most of its arguments and
//! abbreviates them, and renaming 216 stubs to match would be a large diff for no reader.
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
/// A raw hardware key scan code, delivered only in resident mode; `a` carries the scan code.
/// For a launcher to see the Menu and End keys the translated-character path never produces.
pub const SHIM_EV_RAWKEY: i32 = 7;
pub const SHIM_EV_TIMER: i32 = 10;
pub const SHIM_EV_CONNECTED: i32 = 20;
pub const SHIM_EV_RECV: i32 = 21;
pub const SHIM_EV_SENT: i32 = 22;
pub const SHIM_EV_CLOSED: i32 = 23;
pub const SHIM_EV_RESOLVED: i32 = 24;
/// `RConnection` is up. `a` is the IAP the OS chose — persist it and pass it back to
/// [`shim_net_start`] next time to connect without prompting.
pub const SHIM_EV_NET_READY: i32 = 25;
/// An RFCOMM listener accepted a client. `handle` is the new accepted-socket handle (`>= 0`)
/// when `status` is `SHIM_OK`; on failure `status` is the error and no socket was opened.
/// Distinct from the TCP events so a daemon that runs both can branch without ambiguity.
pub const SHIM_EV_BT_ACCEPTED: i32 = 26;
/// Bytes arrived on an RFCOMM socket. `handle` is the socket, `a` the count (`0` with a
/// `SHIM_OK` status is a clean peer close on some stacks — treat per protocol).
pub const SHIM_EV_BT_RECV: i32 = 27;
/// An RFCOMM send completed. `handle` is the socket, `a` the count written when `status` is
/// `SHIM_OK` (RFCOMM `Write` is all-or-nothing, like TCP).
pub const SHIM_EV_BT_SENT: i32 = 28;
/// An RFCOMM socket closed or its link dropped. `handle` is the socket, `status` the reason.
pub const SHIM_EV_BT_CLOSED: i32 = 29;
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
/// A position update completed. `status` is `SHIM_OK` for a fix and the platform's own code
/// otherwise — `SHIM_ERR_TIMED_OUT` for a module that ran out of sky, `SHIM_ERR_ACCESS_DENIED`
/// when the requestor was never declared. `a` is satellites used (-1 when not asked for or no
/// fix), `b` the horizontal accuracy in whole metres (-1 when unknown), `c` 1 when satellite
/// info was requested. The fix itself comes from `shim_gps_read` — latitude is a double.
pub const SHIM_EV_GPS_FIX: i32 = 43;
/// The serving cell tower was read. `status` is `SHIM_OK` or the platform's error; `a` is 1 when
/// the modem said the location area is known. The identifiers come from `shim_cell_get` — an event
/// carries four integers and a caller wants five with a parse behind two of them.
pub const SHIM_EV_CELL: i32 = 44;
/// A subscribed Publish & Subscribe property changed. `a` is the key within the app's
/// category, `c` is the freshly read integer value. Emitted by `shim_prop`'s subscriber.
/// A headless daemon uses this as its stop signal — whoever launched it sets the property,
/// this arrives.
pub const SHIM_EV_PROP: i32 = 53;
/// Something changed in the message store. `a` is one of `SHIM_MSV_EV_*`, `b` the entry id,
/// `c` its parent folder, `d` how many entries the platform's original selection carried.
///
/// **A hint, never data.** By the time this is read the id may be gone and the flags may have
/// changed again, and the shim delivers at most a handful per notification. A reader re-reads
/// the entry from the store, which is what makes a dropped ring slot, a restarted process and
/// an event nobody was listening for all the same recoverable case. Off until
/// [`shim_msv_observe`].
pub const SHIM_EV_MSV: i32 = 60;

/// Response headers arrived for the HTTP transaction in flight. `a` is the HTTP status code.
///
/// Redirects are already followed by the time this fires — the platform stack follows them for
/// GET without reporting it — so this is the status of the page that will actually load, not of
/// the URL that was asked for.
pub const SHIM_EV_HTTP_HEAD: i32 = 70;
/// Body bytes arrived. `a` is the running total the stack has handed over.
///
/// Not the same as what is readable: [`shim_httpc_read`] drains a capped buffer, and the
/// difference shows up as [`SHIM_HTTP_TRUNCATED`] rather than as two numbers quietly disagreeing.
pub const SHIM_EV_HTTP_BODY: i32 = 71;
/// The transaction ended. `status` is [`SHIM_OK`] or the platform error.
///
/// `a` is the HTTP status, `b` the total body bytes, `c` the [`SHIM_HTTP_*`](SHIM_HTTP_GZIP)
/// flags, `d` how many body callbacks it took. The error is worth keeping rather than collapsing
/// to a boolean: an untrusted certificate is only distinguishable from a dead server by its code.
pub const SHIM_EV_HTTP_DONE: i32 = 72;

pub const SHIM_EV_QUIT: i32 = 90;

/// Another application asked this one to open a document — a URL, in practice.
///
/// `a` carries its length in UTF-16 units; the text is collected with
/// [`shim_app_open_request`]. Arrives both on a cold start and on a warm one, and the receiver does
/// not have to know which.
pub const SHIM_EV_OPEN_URL: i32 = 91;

/// The response said `Content-Encoding: gzip`.
pub const SHIM_HTTP_GZIP: i32 = 1 << 0;
/// The response said `Transfer-Encoding: chunked`.
pub const SHIM_HTTP_CHUNKED: i32 = 1 << 1;
/// The body starts `1f 8b` — so the stack handed over the *compressed* bytes and inflating them
/// is ours to do. Together with [`SHIM_HTTP_GZIP`] this is the whole question F2 asks: the header
/// alone says the server compressed, and only this flag says whether we get it decoded for free.
pub const SHIM_HTTP_GZIP_MAGIC: i32 = 1 << 2;
/// The caller fell so far behind draining that the shim dropped body bytes.
///
/// Not "the page was too big": the shim's buffer holds the *backlog* between the stack handing bytes
/// over and [`shim_httpc_read`] taking them, and it releases what has been read. A caller that
/// drains on every [`SHIM_EV_HTTP_BODY`] never sees this. The total in `b` is still the real size.
pub const SHIM_HTTP_TRUNCATED: i32 = 1 << 3;

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
    ///
    /// Set for every way the layer can be engaged: held, tapped-to-arm, or locked. This is the bit
    /// a *text field* wants — what matters there is which character the key produces, not how the
    /// layer was turned on.
    pub const FUNC: i32 = 4;

    /// The Fn key is physically down **right now**.
    ///
    /// The bit a *shortcut* wants, and a separate one because the two questions are different. Fn
    /// has three states on this hardware — held, armed by a tap, and locked by two taps — and only
    /// the first is a deliberate gesture happening at this instant. The other two are stored state
    /// from some earlier press, which is fine for typing a digit and wrong for "and now close the
    /// application".
    ///
    /// Split out after writing, in a browser, that a Fn-modified softkey "cannot be reached by
    /// accident because it must be held" — which was not true of anything the app could actually
    /// observe. A comment claiming a safety property the API could not express.
    pub const FUNC_HELD: i32 = 8;
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

// --------------------------------------------------------------------- sql --
// TSqlColumnType, flattened: the platform's ESqlInt and ESqlInt64 both arrive here as
// SHIM_SQL_INT, because the difference is a storage detail of the row buffer.

pub const SHIM_SQL_NULL: i32 = 0;
pub const SHIM_SQL_INT: i32 = 1;
pub const SHIM_SQL_REAL: i32 = 2;
pub const SHIM_SQL_TEXT: i32 = 3;
pub const SHIM_SQL_BLOB: i32 = 4;

/// [`shim_sql_step`]: the statement has no more rows.
pub const SHIM_SQL_DONE: i32 = 0;
/// [`shim_sql_step`]: a row is ready to be read.
pub const SHIM_SQL_ROW: i32 = 1;

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

/// `RFs::Drive` — what kind of thing a drive letter is.
///
/// Separate from [`ShimVolumeInfo`] because the platform keeps them separate, and because
/// a drive can be present with no volume mounted. An empty memory-card slot answers this
/// call and refuses the other one.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
pub struct ShimDriveInfo {
    /// `TDriveInfo::iType`: `EMediaNotPresent`, `EMediaHardDisk`, `EMediaFlash`, …
    pub media_type: i32,
    pub battery: i32,
    /// `KDriveAttLocal`, `KDriveAttRemovable`, `KDriveAttInternal`, …
    pub drive_att: u32,
    /// `KMediaAttWriteProtected`, `KMediaAttLocked`, …
    pub media_att: u32,
}

/// `RFs::Volume` — size, free space and the label of a mounted volume.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
pub struct ShimVolumeInfo {
    pub size: i64,
    pub free: i64,
    /// Changes when the medium is swapped, so it identifies the card rather than the slot.
    pub unique_id: u32,
    pub name_len: i32,
    pub name: [u16; 32],
}

/// One entry of the Message Server's MTM registry.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct ShimMtmInfo {
    /// The MTM type UID — `KUidMsgTypeSMS` and friends.
    pub type_uid: u32,
    pub technology_uid: u32,
    pub name_len: i32,
    pub name: [u16; 64],
}

impl Default for ShimMtmInfo {
    fn default() -> Self {
        Self { type_uid: 0, technology_uid: 0, name_len: 0, name: [0; 64] }
    }
}

/// The result of loading a polymorphic DLL and calling through its ordinal 1.
///
/// Every step is a separate field because they fail for different reasons. `lookup_ok`
/// false with `load_err` zero means the image loaded and exports nothing — which is what a
/// DLL built without `EXPORT_C` produces, and what no amount of collapsing into one
/// pass/fail would ever tell you.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
pub struct ShimDllProbe {
    pub load_err: i32,
    /// Should be `0x10000079`, `KDynamicLibraryUid`.
    pub uid1: u32,
    pub uid2: u32,
    pub uid3: u32,
    /// 1 if `RLibrary::Lookup(1)` returned non-NULL.
    pub lookup_ok: i32,
    pub call_err: i32,
    /// The sentinel the callee wrote. A non-null `Lookup` proves an export table exists;
    /// only this proves our code ran with our arguments.
    pub magic: u32,
    pub echo: u32,
    pub ticks: u32,
}

/// Flags for [`ShimNewMessage::flags`]. `NEW | UNREAD` is what makes the native Messaging
/// application bold an entry and what the notification list counts.
pub const SHIM_MSV_NEW: i32 = 0x01;
pub const SHIM_MSV_UNREAD: i32 = 0x02;
pub const SHIM_MSV_COMPLETE: i32 = 0x04;
pub const SHIM_MSV_VISIBLE: i32 = 0x08;

/// Everything a message needs to land in a folder.
///
/// The pointers are borrowed for the duration of the call only — the shim copies into
/// descriptors and into the entry's store before returning.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct ShimNewMessage {
    pub service_id: i32,
    pub mtm_uid: u32,
    pub parent_id: i32,
    /// 0 means now.
    pub unix_time: i64,
    /// 0 means "the body's length".
    pub size: i32,
    pub flags: i32,
    /// `iDetails` — who it is from.
    pub details: *const u16,
    pub details_len: i32,
    /// `iDescription` — subject, or a preview line.
    pub description: *const u16,
    pub description_len: i32,
    /// Stored as rich text in the entry's `CMsvStore`.
    pub body: *const u16,
    pub body_len: i32,
}

impl Default for ShimNewMessage {
    fn default() -> Self {
        Self {
            service_id: 0,
            mtm_uid: 0,
            parent_id: SHIM_MSV_INBOX,
            unix_time: 0,
            size: 0,
            flags: SHIM_MSV_NEW | SHIM_MSV_UNREAD,
            details: core::ptr::null(),
            details_len: 0,
            description: core::ptr::null(),
            description_len: 0,
            body: core::ptr::null(),
            body_len: 0,
        }
    }
}

/// Indication bits for [`shim_ncn_notify`]. `NORMAL` is icon + tone + note: what SMS does.
pub const SHIM_NCN_ICON: i32 = 0x01;
pub const SHIM_NCN_TONE: i32 = 0x02;
pub const SHIM_NCN_SOFT_NOTE: i32 = 0x04;
pub const SHIM_NCN_NORMAL: i32 = 0x07;

/// Entry type UIDs, mirrored from `msvstd.hrh`.
///
/// `shim_msg.cpp` asserts each against the platform's own constant at compile time. That guard
/// exists because the first version of these was guessed wrong, and a wrong type UID is
/// invisible: every `is_message()` answers false and a service silently never recognises one
/// of its own messages.
pub const SHIM_MSV_TYPE_ROOT: u32 = 0x1000_0F67;
pub const SHIM_MSV_TYPE_SERVICE: u32 = 0x1000_0F68;
pub const SHIM_MSV_TYPE_FOLDER: u32 = 0x1000_0F69;
pub const SHIM_MSV_TYPE_MESSAGE: u32 = 0x1000_0F6A;
pub const SHIM_MSV_TYPE_ATTACHMENT: u32 = 0x1000_0F6B;

/// Standard message folders, mirrored from `msvids.h` so Rust need not carry that header.
pub const SHIM_MSV_ROOT: i32 = 0x1000;
pub const SHIM_MSV_INBOX: i32 = 0x1002;
pub const SHIM_MSV_OUTBOX: i32 = 0x1003;
pub const SHIM_MSV_DRAFTS: i32 = 0x1004;
pub const SHIM_MSV_SENT: i32 = 0x1005;

/// Entry flags that only the read side reports, continuing the bit space
/// [`SHIM_MSV_NEW`]..[`SHIM_MSV_VISIBLE`] uses on the write side.
///
/// One vocabulary for reading an entry's state and for writing it, deliberately: a caller
/// cannot pass a read flag to [`shim_msv_set_flags`] and have it mean something else.
pub const SHIM_MSV_IN_PREPARATION: i32 = 0x10;
pub const SHIM_MSV_FAILED: i32 = 0x20;

/// One entry of the message store, flattened.
///
/// `details_len` and `description_len` are the **full** platform lengths; the arrays hold
/// the first `min(len, capacity)` units. There is no documented cap on either field, so a
/// caller that cares must compare — see [`crate::ShimMsvEntry`] users in `symbian::msg`,
/// which turn the comparison into a `truncated` flag rather than losing it.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct ShimMsvEntry {
    pub id: i32,
    pub parent: i32,
    pub service_id: i32,
    pub mtm_uid: u32,
    /// `KUidMsvMessageEntry` / `...ServiceEntry` / `...FolderEntry` / `...AttachmentEntry`.
    pub type_uid: u32,
    /// Seconds since the Unix epoch. The shim converts out of Symbian's year-0 count with
    /// the same helper the write side converts into, so the two cannot disagree.
    pub unix_time: i64,
    pub size: i32,
    pub flags: i32,
    pub details_len: i32,
    pub description_len: i32,
    pub details: [u16; 64],
    pub description: [u16; 128],
}

impl Default for ShimMsvEntry {
    fn default() -> Self {
        Self {
            id: 0,
            parent: 0,
            service_id: 0,
            mtm_uid: 0,
            type_uid: 0,
            unix_time: 0,
            size: 0,
            flags: 0,
            details_len: 0,
            description_len: 0,
            details: [0; 64],
            description: [0; 128],
        }
    }
}

/// Carried in `a` of [`SHIM_EV_MSV`]. The four entry kinds put a real id in `b`; the session
/// and registry ones put 0, because the platform's notification is not about an entry.
pub const SHIM_MSV_EV_CREATED: i32 = 1;
pub const SHIM_MSV_EV_CHANGED: i32 = 2;
pub const SHIM_MSV_EV_DELETED: i32 = 3;
pub const SHIM_MSV_EV_MOVED: i32 = 4;
pub const SHIM_MSV_EV_MTM_INSTALLED: i32 = 5;
pub const SHIM_MSV_EV_MTM_REMOVED: i32 = 6;
pub const SHIM_MSV_EV_SERVER_READY: i32 = 7;
pub const SHIM_MSV_EV_SERVER_GONE: i32 = 8;

// ------------------------------------------------------------------ Bluetooth --

/// Flags in [`ShimBtDevice::flags`].
///
/// Symbian has no "trusted" bit. S60's trusted means "connects without asking the user to
/// authorise it", which is `TBTDeviceSecurity::NoAuthorise` — so that is what
/// [`SHIM_BT_TRUSTED`] reads and what [`shim_bt_set_trusted`] writes.
pub const SHIM_BT_PAIRED: i32 = 0x01;
pub const SHIM_BT_TRUSTED: i32 = 0x02;
pub const SHIM_BT_BLOCKED: i32 = 0x04;
pub const SHIM_BT_ENCRYPT: i32 = 0x08;
/// The name came from the user-chosen friendly name rather than the device's own.
pub const SHIM_BT_FRIENDLY: i32 = 0x10;

/// Which route [`shim_bt_power_set`] got its answer from.
pub const SHIM_BT_VIA_NOTIFIER: i32 = 1;
pub const SHIM_BT_VIA_CENREP: i32 = 2;

/// One remote Bluetooth device, from the registry or from an inquiry.
///
/// `name_len` is the **full** length; `name` holds the first `min(len, 32)` units. A caller
/// that cares compares them, the way `symbian::msg` turns the same comparison into a
/// `truncated` flag rather than losing it.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
pub struct ShimBtDevice {
    pub addr: [u8; 6],
    pub pad: [u8; 2],
    pub device_class: u32,
    pub flags: i32,
    pub name_len: i32,
    pub name: [u16; 32],
}

/// This handset's own Bluetooth record.
///
/// Every `i32` is `-1` when the registry says the field was never set, which is not the same
/// as zero: an unset scan-enable is "the record does not say", not "invisible".
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct ShimBtLocal {
    pub addr: [u8; 6],
    pub pad: [u8; 2],
    pub device_class: u32,
    /// `THCIScanEnable`: 0 none, 1 inquiry, 2 page, 3 both.
    pub scan_enable: i32,
    pub limited: i32,
    pub power_setting: i32,
    pub paired_only: i32,
    pub name_len: i32,
    pub name: [u16; 32],
}

impl Default for ShimBtLocal {
    fn default() -> Self {
        Self {
            addr: [0; 6],
            pad: [0; 2],
            device_class: 0,
            scan_enable: -1,
            limited: -1,
            power_setting: -1,
            paired_only: -1,
            name_len: 0,
            name: [0; 32],
        }
    }
}

/// Sentinel for a [`ShimBtRfcommProbe`] step that was never reached, so "failed" and "not
/// attempted" cannot be confused. Matches `SHIM_BT_PROBE_SKIPPED` in the header.
pub const SHIM_BT_PROBE_SKIPPED: i32 = -0x7fff_ffff;

/// One Symbian error code per step of bringing an RFCOMM server socket up, filled by
/// [`shim_bt_rfcomm_probe`]. `0` (`KErrNone`) is success for each step.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct ShimBtRfcommProbe {
    /// `RSocketServ::Connect`.
    pub serv_err: i32,
    /// `RSocket::Open` over `KRFCOMM`.
    pub open_err: i32,
    /// `GetOpt(KRFCOMMGetAvailableServerChannel)`.
    pub channel_err: i32,
    /// The server channel it handed back, `-1` if unknown.
    pub channel: i32,
    /// `Bind(TBTSockAddr)` on that channel.
    pub bind_err: i32,
    /// `RSdp::Connect` + `RSdpDatabase::Open`.
    pub sdp_open_err: i32,
    /// `CreateServiceRecord` + protocol-descriptor/name attributes.
    pub sdp_reg_err: i32,
    /// `Listen()`.
    pub listen_err: i32,
}

impl Default for ShimBtRfcommProbe {
    fn default() -> Self {
        Self {
            serv_err: SHIM_BT_PROBE_SKIPPED,
            open_err: SHIM_BT_PROBE_SKIPPED,
            channel_err: SHIM_BT_PROBE_SKIPPED,
            channel: -1,
            bind_err: SHIM_BT_PROBE_SKIPPED,
            sdp_open_err: SHIM_BT_PROBE_SKIPPED,
            sdp_reg_err: SHIM_BT_PROBE_SKIPPED,
            listen_err: SHIM_BT_PROBE_SKIPPED,
        }
    }
}

/// One filesystem entry's metadata, from [`shim_file_stat`]. Size is split because the ABI is
/// 32-bit; the date is fields because Symbian's epoch is year 0. `month`/`day` are 1-based.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
pub struct ShimFileStat {
    pub size_lo: u32,
    pub size_hi: u32,
    pub year: i32,
    pub month: i32,
    pub day: i32,
    pub hour: i32,
    pub minute: i32,
    pub second: i32,
    /// `KEntryAtt*` bits, as the file server reports them.
    pub attributes: i32,
    pub is_dir: i32,
}

impl ShimFileStat {
    /// The entry's size, reassembled from the two halves.
    pub fn size(&self) -> u64 {
        ((self.size_hi as u64) << 32) | self.size_lo as u64
    }
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
    // tls — one-shot blocking HTTPS GET (headless helpers only; see shim_tls.cpp)
    pub fn shim_https_get(
        host: *const u16,
        host_len: i32,
        port: i32,
        path: *const u16,
        path_len: i32,
        out: *mut u8,
        out_cap: i32,
    ) -> i32;
    // Fetch straight to a file, optionally asking for gzip — for a body too large to hold. Returns
    // the body byte count; the status and whether it is gzip come back through the out params.
    pub fn shim_http_fetch_file(
        host: *const u16,
        host_len: i32,
        port: i32,
        path: *const u16,
        path_len: i32,
        tls: i32,
        gzip: i32,
        file: *const u16,
        file_len: i32,
        status: *mut i32,
        gzipped: *mut i32,
    ) -> i32;
    // zlib — read a gzip file in pieces (shim_gzip.cpp). Synchronous; safe from a pump callback.
    pub fn shim_gunzip_open(path: *const u16, len: i32, handle: *mut i32) -> i32;
    pub fn shim_gunzip_read(handle: i32, out: *mut u8, cap: i32) -> i32;
    pub fn shim_gunzip_close(handle: i32);
    // The same GET without TLS, for a service on a network the user controls whose certificate
    // this handset cannot be made to trust. Cleartext; the caller opts in per URL.
    pub fn shim_http_get(
        host: *const u16,
        host_len: i32,
        port: i32,
        path: *const u16,
        path_len: i32,
        out: *mut u8,
        out_cap: i32,
    ) -> i32;
    // http — the platform's own stack, asynchronous and safe from a GUI pump (shim_http.cpp).
    // One transaction at a time; completion arrives as SHIM_EV_HTTP_DONE.
    pub fn shim_httpc_open(net: i32) -> i32;
    pub fn shim_httpc_get(url: *const u16, len: i32, want_gzip: i32) -> i32;
    /// Arm the **next** GET to resume from a byte offset, as `Range: bytes=N-`.
    ///
    /// A one-shot rather than a parameter on every entry point: a resume is a property of one
    /// request, and widening four existing signatures for what one caller wants is churn. Cleared by
    /// the GET that consumes it — including a GET that fails — so a refused resume cannot leak into
    /// an unrelated fetch and silently start it in the middle.
    ///
    /// The answer to a Range request is 206 with the remainder. A server that does not support it
    /// answers 200 with the whole thing, which is also correct: the caller compares what it asked
    /// for with what it got.
    pub fn shim_httpc_range_from(offset: i64) -> i32;
    // The conditional form. Either validator may be null/empty; given one, a server that agrees
    // the copy is current answers 304 with no body.
    pub fn shim_httpc_get_cond(
        url: *const u16,
        len: i32,
        want_gzip: i32,
        if_none_match: *const u16,
        inm_len: i32,
        if_modified_since: *const u16,
        ims_len: i32,
    ) -> i32;
    // The response's ETag (want_etag != 0) or Last-Modified. Zero means the server sent none.
    /// One POST with the body already in memory.
    ///
    /// `body` is bytes rather than UTF-16 on purpose: a JSON document is encoded by whoever built
    /// it, and narrowing here would be the shim guessing at somebody else's charset. Both `body`
    /// and `content_type` are copied inside the call, so neither has to outlive it — unlike the
    /// buffers of `shim_net_send`.
    pub fn shim_httpc_post(
        url: *const u16,
        len: i32,
        content_type: *const u8,
        ct_len: i32,
        body: *const u8,
        body_len: i32,
    ) -> i32;
    pub fn shim_httpc_validator(want_etag: i32, out: *mut u16, cap: i32) -> i32;
    pub fn shim_httpc_read(out: *mut u8, cap: i32) -> i32;
    pub fn shim_httpc_info(
        status: *mut i32,
        total: *mut i32,
        held: *mut i32,
        flags: *mut i32,
        err: *mut i32,
    ) -> i32;
    // Where the bytes came from, after any redirect the stack followed. Read after
    // SHIM_EV_HTTP_DONE; returns the count of UTF-16 units written.
    pub fn shim_httpc_url(out: *mut u16, cap: i32) -> i32;
    pub fn shim_httpc_cancel() -> i32;
    pub fn shim_httpc_close();
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
    // The same, with the worker's heap ceiling chosen by the caller. A crypto job wants 256 KB
    // and a page layout wants megabytes; on Symbian a thread heap is reserved to its maximum and
    // committed to its minimum, so a large ceiling costs address space and no memory.
    pub fn shim_work_submit_ex(
        opcode: i32,
        input: *const u8,
        in_len: i32,
        out: *mut u8,
        out_len: i32,
        heap_max: i32,
        stack: i32,
    ) -> i32;
    pub fn shim_work_submit(
        opcode: i32,
        input: *const u8,
        in_len: i32,
        out: *mut u8,
        out_len: i32,
    ) -> i32;
    pub fn shim_work_busy() -> i32;
    pub fn shim_work_exit_info(
        ty: *mut i32,
        reason: *mut i32,
        cat: *mut u8,
        cat_cap: i32,
    ) -> i32;
    pub fn shim_cleanup_probe() -> i32;
    pub fn shim_cleanup_probe_bare() -> i32;

    // timers
    pub fn shim_sleep_ms(ms: i32);
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

    // position
    pub fn shim_gps_start(
        interval_ms: i32,
        timeout_ms: i32,
        want_satellites: i32,
        module_uid: i32,
    ) -> i32;
    pub fn shim_gps_stop();
    pub fn shim_gps_read(
        lat: *mut f64,
        lon: *mut f64,
        alt: *mut f64,
        h_acc: *mut f64,
        v_acc: *mut f64,
        sats: *mut i32,
        in_view: *mut i32,
    ) -> i32;
    pub fn shim_gps_module_count(out: *mut i32) -> i32;
    pub fn shim_gps_module_info(
        index: i32,
        name: *mut u16,
        name_cap: i32,
        name_len: *mut i32,
        out: *mut i32,
        out_cap: i32,
    ) -> i32;

    // cell
    pub fn shim_cell_read() -> i32;
    pub fn shim_cell_get(
        mcc: *mut i32,
        mnc: *mut i32,
        lac: *mut i32,
        cid: *mut i32,
        area_known: *mut i32,
    ) -> i32;
    pub fn shim_cell_stop();

    // audio
    pub fn shim_audio_open_file(path: *const u16, path_len: i32) -> i32;
    pub fn shim_audio_play() -> i32;
    pub fn shim_audio_pause() -> i32;
    pub fn shim_audio_stop() -> i32;
    pub fn shim_audio_position_ms() -> i32;
    pub fn shim_audio_duration_ms() -> i32;
    pub fn shim_audio_set_volume(percent: i32) -> i32;
    pub fn shim_audio_close() -> i32;

    // sql (USE_SQL) — Symbian SQL, which is SQLite behind sqldb.dll.
    //
    // Statement text is UTF-8 (`*const u8`) because the platform has 8-bit overloads of
    // Exec and Prepare; bound and returned *values* are UTF-16 like every other string in
    // this ABI, because Bind/Column have no 8-bit form. Parameter and column indexes are
    // zero-based, unlike sqlite3's own C API where parameters start at 1.
    pub fn shim_sql_open(path: *const u16, path_len: i32, create: i32, handle: *mut i32) -> i32;
    pub fn shim_sql_close(db: i32);
    pub fn shim_sql_delete(path: *const u16, path_len: i32) -> i32;
    pub fn shim_sql_exec(db: i32, sql: *const u8, len: i32, changed: *mut i32) -> i32;
    pub fn shim_sql_size(db: i32, out: *mut i32) -> i32;
    pub fn shim_sql_last_error(db: i32, buf: *mut u16, cap: i32, len: *mut i32) -> i32;
    pub fn shim_sql_prepare(db: i32, sql: *const u8, len: i32, stmt: *mut i32) -> i32;
    pub fn shim_sql_finalize(stmt: i32);
    pub fn shim_sql_reset(stmt: i32) -> i32;
    /// [`SHIM_SQL_ROW`], [`SHIM_SQL_DONE`], or a negative error.
    ///
    /// **SELECT only.** Stepping a statement with no row set — an INSERT, an UPDATE, a
    /// CREATE — panics inside the SQL client and closes the process. Use
    /// [`shim_sql_exec_stmt`] for those.
    pub fn shim_sql_step(stmt: i32) -> i32;
    /// Run a prepared non-SELECT statement to completion; `changed` receives the rows
    /// affected. The only safe way to run a bound INSERT, UPDATE or DELETE.
    pub fn shim_sql_exec_stmt(stmt: i32, changed: *mut i32) -> i32;
    pub fn shim_sql_bind_null(stmt: i32, index: i32) -> i32;
    pub fn shim_sql_bind_int(stmt: i32, index: i32, value: i64) -> i32;
    pub fn shim_sql_bind_real(stmt: i32, index: i32, value: f64) -> i32;
    pub fn shim_sql_bind_text(stmt: i32, index: i32, text: *const u16, len: i32) -> i32;
    pub fn shim_sql_bind_blob(stmt: i32, index: i32, data: *const u8, len: i32) -> i32;
    pub fn shim_sql_column_type(stmt: i32, col: i32, out: *mut i32) -> i32;
    pub fn shim_sql_column_int(stmt: i32, col: i32, out: *mut i64) -> i32;
    pub fn shim_sql_column_real(stmt: i32, col: i32, out: *mut f64) -> i32;
    /// `*len` receives the column's full length whether or not it fitted; the return is
    /// [`SHIM_ERR_OVERFLOW`] when it did not.
    pub fn shim_sql_column_text(stmt: i32, col: i32, buf: *mut u16, cap: i32, len: *mut i32) -> i32;
    pub fn shim_sql_column_blob(stmt: i32, col: i32, buf: *mut u8, cap: i32, len: *mut i32) -> i32;
    pub fn shim_sql_column_index(stmt: i32, name: *const u16, len: i32, out: *mut i32) -> i32;

    // diagnostics
    pub fn shim_panic(file: *const u8, file_len: u32, line: u32) -> !;
    pub fn shim_debug(text: *const u16, len: i32);

    // keyboard
    pub fn shim_keyboard_mode(mode: i32) -> i32;
    pub fn shim_keyboard_mode_get() -> i32;

    // directory create + listing
    pub fn shim_mkdir(path: *const u16, path_len: i32) -> i32;
    pub fn shim_dir_list(path: *const u16, path_len: i32, buf: *mut u16, cap: i32, count: *mut i32) -> i32;
    pub fn shim_dir_list_all(path: *const u16, path_len: i32, buf: *mut u16, cap: i32, count: *mut i32) -> i32;
    /// Size, modification time and attributes of one entry (`RFs::Entry`).
    pub fn shim_file_stat(path: *const u16, path_len: i32, out: *mut ShimFileStat) -> i32;

    // app-lifecycle monitor (USE_APPMON) — window-group + focus changes
    pub fn shim_process_start(path: *const u16, path_len: i32) -> i32;
    /// Start a process without waiting for its rendezvous. The only one of the three that a
    /// GUI thread may call: the waiting variants block in `User::WaitForRequest`, which on a
    /// thread with a running active scheduler steals another request's completion and kills
    /// the process with a stray-signal panic.
    pub fn shim_process_spawn(path: *const u16, path_len: i32) -> i32;
    /// As [`shim_process_start`], but abandons the wait after `timeout_ms` and kills the
    /// child, returning [`SHIM_ERR_TIMED_OUT`].
    ///
    /// The plain call waits on the child's rendezvous with no escape, which is right for a
    /// controller that cannot proceed without its daemon and wrong for anything running an
    /// untrusted probe: a child that neither signals nor dies hangs the caller for good.
    pub fn shim_process_start_timeout(path: *const u16, path_len: i32, timeout_ms: i32) -> i32;
    /// Whether a process built from the given UID3 is currently running. Returns 1 for
    /// running, 0 for not, negative on error.
    ///
    /// Reports *liveness*, which is not the same as "finished its work" — a process that
    /// panicked mid-write stops being alive exactly like one that completed. Anything that
    /// needs to tell those apart has to read what the child left behind.
    pub fn shim_process_running(uid3: u32) -> i32;
    /// Kill every live process with this UID3 — the escape hatch for a resident launcher.
    /// [`SHIM_OK`] if one was killed, [`SHIM_ERR_NOT_FOUND`] if none matched.
    pub fn shim_process_kill(uid3: u32) -> i32;

    // resident (launcher) behaviour
    /// Turn resident behaviour on/off: capture the Menu key to bring this app forward, and make
    /// End send to background instead of closing. [`SHIM_ERR_NOT_READY`] before the window group
    /// exists. Needs SwEvent, granted at load on a ROM-patched handset.
    pub fn shim_set_resident(on: i32) -> i32;
    pub fn shim_app_open_request(out: *mut u16, cap: i32) -> i32;
    pub fn shim_cheap_stats(size: *mut i32, allocated: *mut i32);
    pub fn shim_cheap_compress() -> i32;
    /// Drop this app behind the others without closing it.
    pub fn shim_app_to_background() -> i32;
    /// Bring this app back to the front, focus included.
    pub fn shim_app_to_foreground() -> i32;
    /// Sum the CPU microseconds of every thread matching `pattern` (UTF-16).
    pub fn shim_cpu_time(
        pattern: *const u16,
        pattern_len: i32,
        total_us: *mut i64,
        threads: *mut i32,
    ) -> i32;
    /// The full name of the nth running process.
    pub fn shim_process_at(index: i32, out: *mut u16, cap: i32, len: *mut i32) -> i32;

    // installed-app enumeration and launch (USE_APPARC)
    /// Re-scan installed applications into the shim's cache. Returns the count (>= 0) or a
    /// negative error. Goes through `RApaLsSession` — the registry the native menu reads.
    pub fn shim_apps_refresh() -> i32;
    /// How many apps the last [`shim_apps_refresh`] found; 0 before the first refresh.
    pub fn shim_apps_count() -> i32;
    /// Copy cache entry `index` out. `uid3` and `hidden` (1/0) are written when non-null; the
    /// caption is copied up to `cap` u16 with its length in `*caption_len`.
    /// [`SHIM_ERR_NOT_FOUND`] for a bad index.
    pub fn shim_app_at(
        index: i32,
        uid3: *mut u32,
        hidden: *mut u8,
        caption: *mut u16,
        cap: i32,
        caption_len: *mut i32,
    ) -> i32;
    /// Start the installed app with this UID3, the way the shell would. [`SHIM_OK`] on
    /// acceptance; the launched app runs with its own capabilities, not the caller's.
    pub fn shim_app_launch(uid3: u32) -> i32;
    /// Launch app `uid3` pointed at `doc` (a URL, UTF-16, `doc_len` units) by `route`.
    ///
    /// Only linked when the app is built with `USE_LAUNCH_DOC=1`; every other binary must not
    /// reference this or it imports a symbol it does not need. There is no `OpenUrl` on S60 — a
    /// browser is asked by convention, and `route` selects which convention: 0 document name, 1 the
    /// browser's `4 <url>` tail end, 2 `StartDocument` at an explicit app, 3 `StartDocument` letting
    /// the platform resolve. [`SHIM_OK`] means the platform accepted the launch, **not** that the
    /// URL opened; nothing in AppArc reports that.
    pub fn shim_app_launch_doc(uid3: u32, doc: *const u16, doc_len: i32, route: i32) -> i32;
    /// Deliver a message to a running application, bringing it forward. The way the shell hands a
    /// URL to a browser that is already open. [`SHIM_ERR_NOT_FOUND`] when it is not running.
    pub fn shim_app_task_message(uid3: u32, msg: *const u8, msg_len: i32) -> i32;
    /// Put UTF-16 `text` on the system clipboard as plain text, in the format Avkon's Paste reads.
    /// [`SHIM_ERR_NOT_SUPPORTED`] unless the app was built with `USE_CLIPBOARD=1`.
    pub fn shim_clip_set_text(text: *const u16, len: i32) -> i32;
    /// Read the clipboard's plain text into `out` (at most `cap` UTF-16 units); `len` gets the
    /// count. [`SHIM_ERR_NOT_FOUND`] when there is nothing to paste, [`SHIM_ERR_NOT_SUPPORTED`]
    /// unless the app was built with `USE_CLIPBOARD=1`.
    pub fn shim_clip_get_text(out: *mut u16, cap: i32, len: *mut i32) -> i32;
    /// Kill the installed app with this UID3 through the window server — the way to stop an app
    /// that will not close itself, like a resident launcher. [`SHIM_OK`] if killed,
    /// [`SHIM_ERR_NOT_FOUND`] if it has no running task.
    pub fn shim_app_kill(uid3: u32) -> i32;
    /// Ask the app with this UID3 to close (`TApaTask::EndTask`), through the window server.
    ///
    /// The one to use. [`shim_app_kill`] is `RThread::Kill` underneath and **faults the caller**
    /// without `PowerMgmt` — measured on the E72, and it took the launcher down every time an app
    /// was closed from its task switcher. This posts a close event instead: no capability, and an
    /// application that ignores it simply stays.
    pub fn shim_app_end(uid3: u32) -> i32;
    /// `1` when the keypad is locked (or the phone is in autolock), `0` when it is not, negative on
    /// error — [`SHIM_ERR_NOT_READY`] in a process with no control environment.
    ///
    /// `RAknKeyLock::IsKeyLockEnabled`, out of avkon, which every GUI build already links. The
    /// Publish&Subscribe route every write-up names is not available: those keys are not defined on
    /// this handset (read over the remote shell, they answer `KErrNotFound`).
    pub fn shim_keylock() -> i32;
    /// List running apps' UID3s (window-server task list, front-to-back), up to `cap`. Returns the
    /// count written, or a negative error / [`SHIM_ERR_NOT_READY`].
    pub fn shim_apps_running(out: *mut u32, cap: i32) -> i32;
    /// Fetch app `uid3`'s icon at `size` pixels into caller buffers: `rgb_out` gets RGB565
    /// pixels, `mask_out` 8-bit coverage (0 transparent, 255 opaque), both row-major `w`*`h`.
    /// `cap` is each buffer's pixel capacity. `w`/`h` are written when the size is known.
    /// [`SHIM_OK`], [`SHIM_ERR_OVERFLOW`] if too small, or the platform error (e.g. no icon).
    pub fn shim_app_icon(
        uid3: u32,
        size: i32,
        rgb_out: *mut u16,
        mask_out: *mut u8,
        cap: i32,
        w: *mut i32,
        h: *mut i32,
    ) -> i32;
    /// Signal strength via CTelephony (telephony daemon only). `bars` 0..7 (-1 unknown), `dbm` raw.
    pub fn shim_tele_signal(bars: *mut i32, dbm: *mut i32) -> i32;
    /// Read an integer Central Repository key. `SHIM_OK` and `*out` set, or the platform error.
    pub fn shim_cenrep_get(repo: u32, key: u32, out: *mut i32) -> i32;
    pub fn shim_cenrep_get_string(repo: u32, key: u32, buf: *mut u16, cap: i32, len: *mut i32) -> i32;
    pub fn shim_cenrep_set(repo: u32, key: u32, value: i32) -> i32;
    pub fn shim_cenrep_set_string(repo: u32, key: u32, text: *const u16, len: i32) -> i32;

    /// Is the Bluetooth radio on? Reads the same CenRep key `apps/netd` publishes.
    pub fn shim_bt_power_get(out_on: *mut i32) -> i32;
    /// Turn the radio on or off. `*out_via` gets [`SHIM_BT_VIA_NOTIFIER`] or
    /// [`SHIM_BT_VIA_CENREP`] to say which route answered, or 0 if neither did.
    pub fn shim_bt_power_set(on: i32, out_via: *mut i32) -> i32;
    /// This handset's own Bluetooth record — name, address, scan-enable, class.
    pub fn shim_bt_local_get(out: *mut ShimBtLocal) -> i32;
    /// Set the scan-enable (0..3) through the registry's local-device record.
    pub fn shim_bt_visibility_set(scan_enable: i32) -> i32;
    /// Re-read the paired-device view; `*out_count` is the full count, of which at most 32 are
    /// readable with [`shim_bt_paired_get`].
    pub fn shim_bt_paired_refresh(out_count: *mut i32) -> i32;
    /// One device from the last refresh. `SHIM_ERR_NOT_FOUND` past the end.
    pub fn shim_bt_paired_get(index: i32, out: *mut ShimBtDevice) -> i32;
    /// Trust or untrust: read the record, flip `NoAuthorise`, write it back.
    pub fn shim_bt_set_trusted(addr6: *const u8, trusted: i32) -> i32;
    /// Forget a device — the link key goes.
    pub fn shim_bt_unpair(addr6: *const u8) -> i32;
    /// Set the user-chosen friendly name. An empty name clears it.
    pub fn shim_bt_rename(addr6: *const u8, name: *const u16, len: i32) -> i32;
    /// Close the registry session and drop both caches.
    pub fn shim_bt_close() -> i32;
    /// One inquiry, **run to completion before returning**. Daemon only: ten seconds on the GUI
    /// thread freezes the whole phone. `SHIM_ERR_TIMED_OUT` when `budget_ms` ended it.
    pub fn shim_bt_inquiry_sync(budget_ms: i32, max_devices: i32, out_found: *mut i32) -> i32;
    /// One device from the last inquiry. `SHIM_ERR_NOT_FOUND` past the end.
    pub fn shim_bt_found_get(index: i32, out: *mut ShimBtDevice) -> i32;
    /// Bring an RFCOMM server socket up once, synchronously, tear it all down, and report each
    /// step into `*out`. Daemon only. `SHIM_OK` when the sequence ran (read the struct for
    /// per-step results); an error if it could not run at all.
    pub fn shim_bt_rfcomm_probe(out: *mut ShimBtRfcommProbe) -> i32;
    /// Open the RFCOMM listener: claim a channel, bind, register a persistent SPP SDP record
    /// named by `name`/`name_len` (ASCII), and `Listen(backlog)`. Sets `*out_channel`.
    pub fn shim_btrf_listen_start(
        backlog: i32,
        name: *const u16,
        name_len: i32,
        out_channel: *mut i32,
    ) -> i32;
    /// Start one async Accept. Completion is `SHIM_EV_BT_ACCEPTED` (`handle` = new socket).
    pub fn shim_btrf_accept() -> i32;
    /// Start an async receive. `buf` must stay valid until `SHIM_EV_BT_RECV` for this handle.
    pub fn shim_btrf_recv(handle: i32, buf: *mut u8, cap: i32) -> i32;
    /// Start an async send. `buf` must stay valid until `SHIM_EV_BT_SENT` for this handle.
    pub fn shim_btrf_send(handle: i32, buf: *const u8, len: i32) -> i32;
    /// Close one accepted socket, cancelling any outstanding recv/send.
    pub fn shim_btrf_close(handle: i32) -> i32;
    /// Deregister the SDP record and close the listener.
    pub fn shim_btrf_listen_stop() -> i32;
    /// Diagnostic variant of [`shim_app_icon`] using the `TInt` GetAppIcon overload, colour green.
    pub fn shim_app_icon_b(
        uid3: u32,
        size: i32,
        rgb_out: *mut u16,
        mask_out: *mut u8,
        cap: i32,
        w: *mut i32,
        h: *mut i32,
    ) -> i32;
    /// Variant C of [`shim_app_icon`] (USE_AKNICON): reads the app's registered icon *file* through
    /// Avkon's `AknIconUtils`, so MIF (scalable) icons work as well as MBM ones and the mask plane
    /// is real. `bitmap_id` indexes the colour plane within that file; the mask is the next index.
    pub fn shim_app_icon_c(
        uid3: u32,
        size: i32,
        bitmap_id: i32,
        rgb_out: *mut u16,
        mask_out: *mut u8,
        cap: i32,
        w: *mut i32,
        h: *mut i32,
    ) -> i32;
    /// The path of the file an app's icon comes from (USE_AKNICON), as UTF-16 units.
    pub fn shim_app_icon_file(uid3: u32, out: *mut u16, cap: i32, len: *mut i32) -> i32;

    // device inventory (USE_HAL)
    /// `HAL::Get`. `attr` is a `HALData::TAttribute`; see `symbian::hal` for the table.
    /// `KErrNotSupported` means the handset does not implement that attribute, which is
    /// itself an answer about the hardware.
    pub fn shim_hal_get(attr: i32, out: *mut i32) -> i32;
    /// One entry of the **phone's own theme** colour table, as `0x00RRGGBB` in `out`.
    ///
    /// `major`/`minor` are the two halves of a `TAknsItemID` and `index` is the entry within that
    /// table — `AknsUtils::GetCachedColor`, out of `aknskins`, which is **not** in the base library
    /// set: a build wanting this needs `USE_SKIN=1`.
    ///
    /// One generic accessor rather than a function per colour, for the reason [`shim_hal_get`] gives:
    /// the ID table is *data* and belongs in Rust, where a host test can cover it, not as sixty
    /// exported functions each able to be wrong in its own way. The names live in
    /// `symbian::skin`.
    ///
    /// [`SHIM_ERR_NOT_READY`] from a headless process: the skin instance is the *application's*,
    /// created by Avkon during app-UI construction, so a daemon has none.
    pub fn shim_skin_color(major: i32, minor: i32, index: i32, out: *mut u32) -> i32;
    /// Up to `cap` pixels of a themed **background bitmap**, on an even grid, as `0x00RRGGBB`.
    ///
    /// Returns the count written, and fills `width`/`height` with the bitmap's real size — which is
    /// how a caller tells "no such bitmap" from "a bitmap of nothing".
    ///
    /// Not needed for a palette: the colour table does carry hue after all — see
    /// `docs/reference/skinprobe.txt`, which also records the first reading of that data getting it
    /// backwards. This is kept because "what does the theme's background look like" is still a
    /// question worth asking, and because the answer on the E72 is itself a finding: all four
    /// background IDs return NULL from `GetCachedBitmap`, which reads a cache nothing had filled.
    ///
    /// Samples rather than one average, because "what is the page colour" is a decision — mean,
    /// median, corner — and decisions belong in Rust where a host test can pin them.
    pub fn shim_skin_samples(
        major: i32,
        minor: i32,
        out: *mut u32,
        cap: i32,
        width: *mut i32,
        height: *mut i32,
    ) -> i32;

    // drives and volumes (USE_FS_INFO)
    /// Bit N set means drive letter `'A' + N` exists.
    pub fn shim_drive_list(out_mask: *mut u32) -> i32;
    /// `drive` is 0 for A:, 1 for B:, … — `TDriveNumber`'s own numbering.
    pub fn shim_drive_info(drive: i32, out: *mut ShimDriveInfo) -> i32;
    /// `SHIM_ERR_NOT_READY` for a drive that exists with nothing mounted — an empty card
    /// slot. A finding, not a failure.
    pub fn shim_volume_info(drive: i32, out: *mut ShimVolumeInfo) -> i32;

    // platform security (USE_CAPS)
    /// 1 if this process holds the `TCapability`, 0 if not, negative on a bad argument.
    /// Reports what the loader *granted*; whether the capability opens the door is a
    /// separate question, answered by attempting the operation.
    pub fn shim_has_capability(cap: i32) -> i32;
    /// `RFs::Att`. Used as a capability probe against a path outside the data cage, where
    /// the error code is the result and nothing is created or destroyed.
    pub fn shim_fs_att(path: *const u16, len: i32, out: *mut u32) -> i32;

    // messaging, read-only (USE_MSG) — imports msgs.dso; see shim_msg.cpp on isolation
    pub fn shim_msv_open(out_handle: *mut i32) -> i32;
    pub fn shim_msv_mtm_count(handle: i32, out: *mut i32) -> i32;
    /// Rebuild the client-side registry snapshot. Required after installing an MTM: the
    /// registry is a per-process copy and does not notice an install on its own.
    pub fn shim_msv_refresh_registry(handle: i32) -> i32;
    /// Ask the framework to find the type, load its DLL and call its factory. The definitive
    /// test that a registration worked — counting the registry is not.
    pub fn shim_msv_can_instantiate(handle: i32, mtm_uid: u32) -> i32;
    pub fn shim_msv_mtm_info(handle: i32, index: i32, out: *mut ShimMtmInfo) -> i32;
    pub fn shim_msv_folder_count(handle: i32, folder_id: i32, out: *mut i32) -> i32;
    /// How many children of the folder are unread — one server-side count, for a home-screen
    /// "N new messages" indicator.
    pub fn shim_msv_folder_unread(handle: i32, folder_id: i32, out: *mut i32) -> i32;
    pub fn shim_msv_close(handle: i32);
    /// Tell the Message Server about a `.mtm` outside ROM. Dropping the file in
    /// `C:\resource\messaging\mtm\` is not enough on its own.
    pub fn shim_msv_install_mtm(path: *const u16, len: i32) -> i32;
    /// De-install first: installing over an existing group fails, so a reinstall needs the
    /// pair. `SHIM_ERR_NOT_FOUND` is the ordinary first-run answer.
    pub fn shim_msv_deinstall_mtm(path: *const u16, len: i32) -> i32;
    /// Create the service entry — the "account" the native Messaging application lists.
    pub fn shim_msv_create_service(handle: i32, mtm_uid: u32, name: *const u16, name_len: i32, out_id: *mut i32) -> i32;
    /// Create an entry, write its body, commit, then make it visible — in that order.
    pub fn shim_msv_create_message(handle: i32, msg: *const ShimNewMessage, out_id: *mut i32) -> i32;
    pub fn shim_msv_delete_entry(handle: i32, id: i32) -> i32;
    /// Delete every service of a type and everything under it. Returns the count removed.
    pub fn shim_msv_delete_services(handle: i32, mtm_uid: u32) -> i32;

    // messaging, the read side (USE_MSG). Adds no import: every call is in msgs.dso already.
    /// One entry's fields. Zeroes `out` before it tries, so a caller that ignores the error
    /// reads an empty entry rather than its own stack.
    pub fn shim_msv_entry(handle: i32, id: i32, out: *mut ShimMsvEntry) -> i32;
    /// A folder's children, newest first. Writes `min(count, cap)` and reports the full
    /// count — truncation is a number, not an error, so the retry loop stays in Rust.
    pub fn shim_msv_children(handle: i32, folder_id: i32, out_ids: *mut i32, cap: i32, out_count: *mut i32) -> i32;
    /// Service entries of one MTM type. How a service finds the account it made last run
    /// instead of creating a second one.
    pub fn shim_msv_services(handle: i32, mtm_uid: u32, out_ids: *mut i32, cap: i32, out_count: *mut i32) -> i32;
    /// The body as UTF-16. No body text is length 0 and success, not `NOT_FOUND`.
    pub fn shim_msv_body(handle: i32, id: i32, out: *mut u16, cap: i32, out_len: *mut i32) -> i32;
    /// Set and clear flags in one `ChangeL`, read-modify-write inside the shim. `set` wins.
    pub fn shim_msv_set_flags(handle: i32, id: i32, set: i32, clear: i32) -> i32;
    /// Reparent. Durable state that survives a restart, where a set of ids in a process is not.
    pub fn shim_msv_move_entry(handle: i32, id: i32, new_parent: i32) -> i32;
    /// Start or stop delivering session events as [`SHIM_EV_MSV`]. Off by default.
    pub fn shim_msv_observe(handle: i32, enable: i32) -> i32;

    // the new-message notification (USE_NCN) — imports ecom.dso
    /// Raise the platform's own indicator/tone/note for a service. Failure is a value: the
    /// interface is documented as an *email* plugin API and its implementation is an ECom
    /// plugin nothing has swept for.
    pub fn shim_ncn_notify(service_id: i32, indication: i32) -> i32;
    pub fn shim_ncn_mark_unread(service_id: i32) -> i32;

    // loading our own DLL (USE_DLL_PROBE)
    /// Load `name`, look up ordinal 1, call it with `arg`, and report every step.
    /// The return value is about *this call*; the DLL's own failures arrive in `out`.
    pub fn shim_dll_call_ordinal1(name: *const u16, len: i32, arg: u32, out: *mut ShimDllProbe) -> i32;
    /// Load and look up an ordinal without calling it. 1 = present, 0 = loaded but absent,
    /// negative = would not load. Cannot fault, which is the point.
    pub fn shim_dll_has_ordinal(name: *const u16, len: i32, ordinal: i32) -> i32;

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
    /// As [`shim_prop_define`], but with an open read policy so a different-SID process can read it.
    pub fn shim_prop_define_public(category: u32, key: u32) -> i32;
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

    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. Nothing to uphold on the way in; the cell that comes
    /// back is uninitialised and is only valid until it is passed to `shim_free`.
    pub unsafe fn shim_alloc(_size: u32) -> *mut c_void {
        nope!("shim_alloc")
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `p` must be a live cell from `shim_alloc` or
    /// `shim_realloc`, and is invalid afterwards whether the call moved it or not.
    pub unsafe fn shim_realloc(_p: *mut c_void, _size: u32) -> *mut c_void {
        nope!("shim_realloc")
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `p` must be a live cell from `shim_alloc` or
    /// `shim_realloc`, and nothing may touch it afterwards.
    pub unsafe fn shim_free(_p: *mut c_void) {
        nope!("shim_free")
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `p` must be a live cell from `shim_alloc` or
    /// `shim_realloc`.
    pub unsafe fn shim_alloc_len(_p: *const c_void) -> u32 {
        nope!("shim_alloc_len")
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must point to a writable `ShimEvent`.
    pub unsafe fn shim_poll_event(_out: *mut ShimEvent) -> i32 {
        0
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_events_dropped() -> i32 {
        0
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_request_exit() {}
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must point to a writable `ShimFb`. The pixel
    /// pointer it comes back with is valid only until `shim_fb_unlock`, and no other shim function may
    /// be called while the lock is held.
    pub unsafe fn shim_fb_lock(_out: *mut ShimFb) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_fb_unlock() {}
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_present(_x: i32, _y: i32, _w: i32, _h: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `w` must point to a writable `i32` and `h` must point
    /// to a writable `i32`.
    pub unsafe fn shim_screen_size(_w: *mut i32, _h: *mut i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `format` must point to a writable `i32`.
    pub unsafe fn shim_screen_format(_f: *mut i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out_word` must point to a writable `u32`.
    pub unsafe fn shim_probe_pixel_layout(_w: *mut u32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `name` must be readable for `len` UTF-16 code units.
    pub unsafe fn shim_dll_present(_name: *const u16, _len: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_own_uid3() -> u32 {
        0
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must be writable for `len` bytes.
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
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `buf` must be writable for `cap` UTF-16 code units and
    /// `len` must point to a writable `i32`.
    pub unsafe fn shim_private_path(_b: *mut u16, _c: i32, _l: *mut i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `path` must be readable for `len` UTF-16 code units and
    /// `handle` must point to a writable `i32`.
    pub unsafe fn shim_file_open(_p: *const u16, _l: i32, _m: i32, _h: *mut i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `buf` must be writable for `cap` bytes and `got` must
    /// point to a writable `i32`.
    pub unsafe fn shim_file_read(_h: i32, _b: *mut u8, _c: i32, _g: *mut i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `buf` must be readable for `len` bytes.
    pub unsafe fn shim_file_write(_h: i32, _b: *const u8, _l: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must point to a writable `i64`.
    pub unsafe fn shim_file_size(_h: i32, _o: *mut i64) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_file_seek(_h: i32, _p: i64) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `path` must be readable for `len` UTF-16 code units.
    pub unsafe fn shim_file_delete(_p: *const u16, _l: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `from` must be readable for `from_len` UTF-16 code
    /// units and `to` must be readable for `to_len` UTF-16 code units.
    pub unsafe fn shim_file_rename(
        _f: *const u16,
        _fl: i32,
        _t: *const u16,
        _tl: i32,
    ) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_file_close(_h: i32) {}
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_net_connections() -> i32 {
        0
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `iap` must point to a writable `i32`.
    pub unsafe fn shim_net_connection_iap(_i: i32, iap: *mut i32) -> i32 {
        if !iap.is_null() {
            *iap = -1;
        }
        SHIM_ERR_NOT_FOUND
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `handle` must point to a writable `i32`.
    pub unsafe fn shim_net_start(_iap: i32, _h: *mut i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_net_stop(_h: i32) {}
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `host` must be readable for `host_len` UTF-16 code
    /// units, `path` must be readable for `path_len` UTF-16 code units and `out` must be writable for
    /// `out_cap` bytes.
    pub unsafe fn shim_https_get(
        _host: *const u16,
        _host_len: i32,
        _port: i32,
        _path: *const u16,
        _path_len: i32,
        _out: *mut u8,
        _out_cap: i32,
    ) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `host` must be readable for `host_len` UTF-16 code
    /// units, `path` must be readable for `path_len` UTF-16 code units, `file` must be readable for
    /// `file_len` UTF-16 code units, `status` must point to a writable `i32` and `gzipped` must point
    /// to a writable `i32`.
    // The arity is the C++ shim's, not ours: this mirrors a declaration in
    // shim/inc/symbian_shim.h, and splitting the Rust side into a struct would leave the two
    // halves of the ABI describing different functions.
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn shim_http_fetch_file(
        _host: *const u16,
        _host_len: i32,
        _port: i32,
        _path: *const u16,
        _path_len: i32,
        _tls: i32,
        _gzip: i32,
        _file: *const u16,
        _file_len: i32,
        _status: *mut i32,
        _gzipped: *mut i32,
    ) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `path` must be readable for `len` UTF-16 code units and
    /// `handle` must point to a writable `i32`.
    pub unsafe fn shim_gunzip_open(_p: *const u16, _l: i32, _h: *mut i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must be writable for `cap` bytes.
    pub unsafe fn shim_gunzip_read(_h: i32, _out: *mut u8, _cap: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_gunzip_close(_h: i32) {}
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `host` must be readable for `host_len` UTF-16 code
    /// units, `path` must be readable for `path_len` UTF-16 code units and `out` must be writable for
    /// `out_cap` bytes.
    pub unsafe fn shim_http_get(
        _host: *const u16,
        _host_len: i32,
        _port: i32,
        _path: *const u16,
        _path_len: i32,
        _out: *mut u8,
        _out_cap: i32,
    ) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_httpc_open(_net: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_httpc_range_from(_offset: i64) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `url` must be readable for `len` UTF-16 code units.
    pub unsafe fn shim_httpc_get(_url: *const u16, _len: i32, _gzip: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    #[allow(clippy::too_many_arguments)]
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `url` must be readable for `len` UTF-16 code units,
    /// `if_none_match` must be readable for `inm_len` UTF-16 code units and `if_modified_since` must be
    /// readable for `ims_len` UTF-16 code units.
    pub unsafe fn shim_httpc_get_cond(
        _url: *const u16,
        _len: i32,
        _gzip: i32,
        _inm: *const u16,
        _inm_len: i32,
        _ims: *const u16,
        _ims_len: i32,
    ) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `url` must be readable for `len` UTF-16 code units,
    /// `content_type` must be readable for `ct_len` bytes and `body` must be readable for `body_len`
    /// bytes.
    pub unsafe fn shim_httpc_post(
        _url: *const u16,
        _len: i32,
        _ct: *const u8,
        _ct_len: i32,
        _body: *const u8,
        _body_len: i32,
    ) -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must be writable for `cap` UTF-16 code units.
    pub unsafe fn shim_httpc_validator(_want_etag: i32, _out: *mut u16, _cap: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must be writable for `cap` bytes.
    pub unsafe fn shim_httpc_read(_out: *mut u8, _cap: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `status` must point to a writable `i32`, `total` must
    /// point to a writable `i32`, `held` must point to a writable `i32`, `flags` must point to a
    /// writable `i32` and `err` must point to a writable `i32`.
    pub unsafe fn shim_httpc_info(
        _status: *mut i32,
        _total: *mut i32,
        _held: *mut i32,
        _flags: *mut i32,
        _err: *mut i32,
    ) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must be writable for `cap` UTF-16 code units.
    pub unsafe fn shim_httpc_url(_out: *mut u16, _cap: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_httpc_cancel() -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_httpc_close() {}
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `host` must be readable for `len` UTF-16 code units and
    /// `handle` must point to a writable `i32`.
    pub unsafe fn shim_dns_resolve(
        _c: i32,
        _host: *const u16,
        _l: i32,
        _h: *mut i32,
    ) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_dns_close(_h: i32) {}
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `handle` must point to a writable `i32`.
    pub unsafe fn shim_tcp_open(_c: i32, _h: *mut i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_tcp_connect(_h: i32, _ip: u32, _p: u16) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `buf` must be readable for `len` bytes.
    pub unsafe fn shim_tcp_send(_h: i32, _b: *const u8, _l: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `buf` must be writable for `cap` bytes.
    pub unsafe fn shim_tcp_recv(_h: i32, _b: *mut u8, _c: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_tcp_close(_h: i32) {}
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `handle` must point to a writable `i32`.
    pub unsafe fn shim_udp_open(_c: i32, _h: *mut i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `buf` must be readable for `len` bytes.
    pub unsafe fn shim_udp_send_to(
        _h: i32,
        _b: *const u8,
        _l: i32,
        _ip: u32,
        _p: u16,
    ) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `buf` must be writable for `cap` bytes.
    pub unsafe fn shim_udp_recv_from(_h: i32, _b: *mut u8, _c: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `input` must be readable for `in_len` bytes and `out`
    /// writable for `out_len`, and both must stay alive and untouched until `SHIM_EV_WORK_DONE` arrives
    /// — the worker thread is still holding them.
    pub unsafe fn shim_work_submit(
        _op: i32,
        _in: *const u8,
        _il: i32,
        _out: *mut u8,
        _ol: i32,
    ) -> i32 {
        SHIM_ERR_NOT_READY
    }
    #[allow(clippy::too_many_arguments)]
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `input` must be readable for `in_len` bytes and `out`
    /// writable for `out_len`, and both must stay alive and untouched until `SHIM_EV_WORK_DONE` arrives
    /// — the worker thread is still holding them.
    pub unsafe fn shim_work_submit_ex(
        _op: i32,
        _in: *const u8,
        _il: i32,
        _out: *mut u8,
        _ol: i32,
        _heap: i32,
        _stack: i32,
    ) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_work_busy() -> i32 {
        0
    }

    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `ty` must point to a writable `i32`, `reason` must
    /// point to a writable `i32` and `cat` must be writable for `cat_cap` bytes.
    pub unsafe fn shim_work_exit_info(
        _ty: *mut i32,
        _reason: *mut i32,
        _cat: *mut u8,
        _cat_cap: i32,
    ) -> i32 {
        SHIM_ERR_NOT_READY
    }

    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_cleanup_probe() -> i32 {
        0
    }

    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must be writable for `cap` UTF-16 code units.
    pub unsafe fn shim_app_open_request(_out: *mut u16, _cap: i32) -> i32 {
        0
    }

    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `size` must point to a writable `i32` and `allocated`
    /// must point to a writable `i32`.
    pub unsafe fn shim_cheap_stats(size: *mut i32, allocated: *mut i32) {
        if !size.is_null() {
            *size = 0;
        }
        if !allocated.is_null() {
            *allocated = 0;
        }
    }

    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_cheap_compress() -> i32 {
        0
    }

    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_cleanup_probe_bare() -> i32 {
        0
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_sleep_ms(_ms: i32) {}
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `handle` must point to a writable `i32`.
    pub unsafe fn shim_timer_after(_ms: i32, _h: *mut i32) -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `handle` must point to a writable `i32`.
    pub unsafe fn shim_timer_every(_ms: i32, _h: *mut i32) -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_timer_cancel(_h: i32) {}
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_now_us() -> u64 {
        0
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_unix_time() -> i64 {
        0
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_utc_offset() -> i32 {
        0
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `path` must be readable for `path_len` UTF-16 code
    /// units, `w` must point to a writable `i32` and `h` must point to a writable `i32`.
    pub unsafe fn shim_image_probe(_p: *const u16, _l: i32, _w: *mut i32, _h: *mut i32) -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `path` must be readable for `path_len` UTF-16 code
    /// units and `handle` must point to a writable `i32`.
    pub unsafe fn shim_image_decode_start(
        _p: *const u16,
        _l: i32,
        _mw: i32,
        _mh: i32,
        _h: *mut i32,
    ) -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `data` must be readable for `len` bytes and `handle`
    /// must point to a writable `i32`.
    pub unsafe fn shim_image_decode_start_mem(
        _d: *const u8,
        _l: i32,
        _mw: i32,
        _mh: i32,
        _h: *mut i32,
    ) -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must be writable for `out_cap` UTF-16 code units,
    /// `w` must point to a writable `i32` and `h` must point to a writable `i32`.
    pub unsafe fn shim_image_result(
        _h: i32,
        _o: *mut u16,
        _cap: i32,
        _w: *mut i32,
        _ht: *mut i32,
    ) -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must be writable for `cap` i32s.
    pub unsafe fn shim_image_describe(_h: i32, _o: *mut i32, _c: i32) -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_image_close(_h: i32) {}
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_gps_start(_i: i32, _t: i32, _s: i32, _m: i32) -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_gps_stop() {}
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `lat` must point to a writable `f64`, `lon` must point
    /// to a writable `f64`, `alt` must point to a writable `f64`, `h_acc` must point to a writable
    /// `f64`, `v_acc` must point to a writable `f64`, `sats` must point to a writable `i32` and
    /// `in_view` must point to a writable `i32`.
    pub unsafe fn shim_gps_read(
        _lat: *mut f64,
        _lon: *mut f64,
        _alt: *mut f64,
        _ha: *mut f64,
        _va: *mut f64,
        _s: *mut i32,
        _iv: *mut i32,
    ) -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must point to a writable `i32`.
    pub unsafe fn shim_gps_module_count(_o: *mut i32) -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `name` must be writable for `name_cap` UTF-16 code
    /// units, `name_len` must point to a writable `i32` and `out` must be writable for `out_cap` i32s.
    pub unsafe fn shim_gps_module_info(
        _i: i32,
        _n: *mut u16,
        _nc: i32,
        _nl: *mut i32,
        _o: *mut i32,
        _oc: i32,
    ) -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_cell_read() -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `mcc` must point to a writable `i32`, `mnc` must point
    /// to a writable `i32`, `lac` must point to a writable `i32`, `cid` must point to a writable `i32`
    /// and `area_known` must point to a writable `i32`.
    pub unsafe fn shim_cell_get(
        _mcc: *mut i32,
        _mnc: *mut i32,
        _lac: *mut i32,
        _cid: *mut i32,
        _ak: *mut i32,
    ) -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_cell_stop() {}
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `path` must be readable for `path_len` UTF-16 code
    /// units.
    pub unsafe fn shim_audio_open_file(_p: *const u16, _l: i32) -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_audio_play() -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_audio_pause() -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_audio_stop() -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_audio_position_ms() -> i32 {
        0
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_audio_duration_ms() -> i32 {
        0
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_audio_set_volume(_p: i32) -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_audio_close() -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `file` must be readable for `file_len` bytes.
    pub unsafe fn shim_panic(_f: *const u8, _l: u32, _line: u32) -> ! {
        nope!("shim_panic")
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `text` must be readable for `len` UTF-16 code units.
    pub unsafe fn shim_debug(_t: *const u16, _l: i32) {}
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_keyboard_mode(_mode: i32) -> i32 {
        0
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_keyboard_mode_get() -> i32 {
        SHIM_KEYBOARD_SCAN
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `path` must be readable for `path_len` UTF-16 code
    /// units.
    pub unsafe fn shim_mkdir(_p: *const u16, _pl: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `path` must be readable for `path_len` UTF-16 code
    /// units, `buf` must be writable for `cap` UTF-16 code units and `count` must point to a writable
    /// `i32`.
    pub unsafe fn shim_dir_list(_p: *const u16, _pl: i32, _b: *mut u16, _c: i32, count: *mut i32) -> i32 {
        if !count.is_null() {
            core::ptr::write(count, 0);
        }
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `path` must be readable for `path_len` UTF-16 code
    /// units, `buf` must be writable for `cap` UTF-16 code units and `count` must point to a writable
    /// `i32`.
    pub unsafe fn shim_dir_list_all(_p: *const u16, _pl: i32, _b: *mut u16, _c: i32, count: *mut i32) -> i32 {
        if !count.is_null() {
            core::ptr::write(count, 0);
        }
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `path` must be readable for `path_len` UTF-16 code
    /// units and `out` must point to a writable `ShimFileStat`.
    pub unsafe fn shim_file_stat(_p: *const u16, _pl: i32, out: *mut ShimFileStat) -> i32 {
        if !out.is_null() {
            core::ptr::write(out, ShimFileStat::default());
        }
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `path` must be readable for `path_len` UTF-16 code
    /// units.
    pub unsafe fn shim_process_spawn(_p: *const u16, _pl: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `path` must be readable for `path_len` UTF-16 code
    /// units.
    pub unsafe fn shim_process_start(_p: *const u16, _pl: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `path` must be readable for `path_len` UTF-16 code
    /// units.
    pub unsafe fn shim_process_start_timeout(_p: *const u16, _pl: i32, _ms: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_process_running(_uid3: u32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_process_kill(_uid3: u32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_set_resident(_on: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `pattern` must be readable for `pattern_len` UTF-16
    /// code units, `total_us` must point to a writable `i64` and `threads` must point to a writable
    /// `i32`.
    pub unsafe fn shim_cpu_time(
        _pattern: *const u16,
        _pattern_len: i32,
        total_us: *mut i64,
        threads: *mut i32,
    ) -> i32 {
        if !total_us.is_null() {
            core::ptr::write(total_us, 0);
        }
        if !threads.is_null() {
            core::ptr::write(threads, 0);
        }
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must be writable for `cap` UTF-16 code units and
    /// `len` must point to a writable `i32`.
    pub unsafe fn shim_process_at(_index: i32, _out: *mut u16, _cap: i32, len: *mut i32) -> i32 {
        if !len.is_null() {
            core::ptr::write(len, 0);
        }
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_apps_refresh() -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_apps_count() -> i32 {
        0
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `uid3` must be writable for `cap` u32s, `hidden` must
    /// be writable for `cap` bytes, `caption` must be writable for `cap` UTF-16 code units and
    /// `caption_len` must point to a writable `i32`.
    pub unsafe fn shim_app_at(
        _index: i32,
        _uid3: *mut u32,
        _hidden: *mut u8,
        _caption: *mut u16,
        _cap: i32,
        caption_len: *mut i32,
    ) -> i32 {
        if !caption_len.is_null() {
            core::ptr::write(caption_len, 0);
        }
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_app_launch(_uid3: u32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `msg` must be readable for `msg_len` bytes.
    pub unsafe fn shim_app_task_message(_uid3: u32, _msg: *const u8, _len: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_app_to_background() -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_app_to_foreground() -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `text` must be readable for `len` UTF-16 code units.
    pub unsafe fn shim_clip_set_text(_text: *const u16, _len: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must be writable for `cap` UTF-16 code units and
    /// `len` must point to a writable `i32`.
    pub unsafe fn shim_clip_get_text(_out: *mut u16, _cap: i32, _len: *mut i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `doc` must be readable for `doc_len` UTF-16 code units.
    pub unsafe fn shim_app_launch_doc(
        _uid3: u32,
        _doc: *const u16,
        _doc_len: i32,
        _route: i32,
    ) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_app_kill(_uid3: u32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_app_end(_uid3: u32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_keylock() -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must be writable for `cap` u32s.
    pub unsafe fn shim_apps_running(_out: *mut u32, _cap: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `rgb_out` must be writable for `cap` UTF-16 code units,
    /// `mask_out` must be writable for `cap` bytes, `w` must point to a writable `i32` and `h` must
    /// point to a writable `i32`.
    pub unsafe fn shim_app_icon(
        _uid3: u32,
        _size: i32,
        _rgb_out: *mut u16,
        _mask_out: *mut u8,
        _cap: i32,
        w: *mut i32,
        h: *mut i32,
    ) -> i32 {
        if !w.is_null() {
            core::ptr::write(w, 0);
        }
        if !h.is_null() {
            core::ptr::write(h, 0);
        }
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `bars` must point to a writable `i32` and `dbm` must
    /// point to a writable `i32`.
    pub unsafe fn shim_tele_signal(bars: *mut i32, dbm: *mut i32) -> i32 {
        if !bars.is_null() {
            core::ptr::write(bars, -1);
        }
        if !dbm.is_null() {
            core::ptr::write(dbm, 0);
        }
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `buf` must be writable for `cap` UTF-16 code units and
    /// `len` must point to a writable `i32`.
    pub unsafe fn shim_cenrep_get_string(
        _repo: u32,
        _key: u32,
        _buf: *mut u16,
        _cap: i32,
        len: *mut i32,
    ) -> i32 {
        if !len.is_null() {
            *len = 0;
        }
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_cenrep_set(_repo: u32, _key: u32, _value: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `text` must be readable for `len` UTF-16 code units.
    pub unsafe fn shim_cenrep_set_string(_repo: u32, _key: u32, _t: *const u16, _l: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must point to a writable `i32`.
    pub unsafe fn shim_cenrep_get(_repo: u32, _key: u32, out: *mut i32) -> i32 {
        if !out.is_null() {
            core::ptr::write(out, 0);
        }
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out_on` must point to a writable `i32`.
    pub unsafe fn shim_bt_power_get(out_on: *mut i32) -> i32 {
        if !out_on.is_null() {
            core::ptr::write(out_on, 0);
        }
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out_via` must point to a writable `i32`.
    pub unsafe fn shim_bt_power_set(_on: i32, out_via: *mut i32) -> i32 {
        if !out_via.is_null() {
            core::ptr::write(out_via, 0);
        }
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must point to a writable `ShimBtLocal`.
    pub unsafe fn shim_bt_local_get(out: *mut ShimBtLocal) -> i32 {
        if !out.is_null() {
            core::ptr::write(out, ShimBtLocal::default());
        }
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_bt_visibility_set(_scan_enable: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out_count` must point to a writable `i32`.
    pub unsafe fn shim_bt_paired_refresh(out_count: *mut i32) -> i32 {
        if !out_count.is_null() {
            core::ptr::write(out_count, 0);
        }
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must point to a writable `ShimBtDevice`.
    pub unsafe fn shim_bt_paired_get(_index: i32, out: *mut ShimBtDevice) -> i32 {
        if !out.is_null() {
            core::ptr::write(out, ShimBtDevice::default());
        }
        SHIM_ERR_NOT_FOUND
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `addr6` must point to the six readable bytes of a
    /// Bluetooth address.
    pub unsafe fn shim_bt_set_trusted(_addr6: *const u8, _trusted: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `addr6` must point to the six readable bytes of a
    /// Bluetooth address.
    pub unsafe fn shim_bt_unpair(_addr6: *const u8) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `addr6` must point to the six readable bytes of a
    /// Bluetooth address and `name` must be readable for `len` UTF-16 code units.
    pub unsafe fn shim_bt_rename(_addr6: *const u8, _name: *const u16, _len: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_bt_close() -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out_found` must point to a writable `i32`.
    pub unsafe fn shim_bt_inquiry_sync(
        _budget_ms: i32,
        _max_devices: i32,
        out_found: *mut i32,
    ) -> i32 {
        if !out_found.is_null() {
            core::ptr::write(out_found, 0);
        }
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must point to a writable `ShimBtDevice`.
    pub unsafe fn shim_bt_found_get(_index: i32, out: *mut ShimBtDevice) -> i32 {
        if !out.is_null() {
            core::ptr::write(out, ShimBtDevice::default());
        }
        SHIM_ERR_NOT_FOUND
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must point to a writable `ShimBtRfcommProbe`.
    pub unsafe fn shim_bt_rfcomm_probe(out: *mut ShimBtRfcommProbe) -> i32 {
        if !out.is_null() {
            core::ptr::write(out, ShimBtRfcommProbe::default());
        }
        SHIM_ERR_NOT_SUPPORTED
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `name` must be readable for `name_len` UTF-16 code
    /// units and `out_channel` must point to a writable `i32`.
    pub unsafe fn shim_btrf_listen_start(
        _backlog: i32,
        _name: *const u16,
        _name_len: i32,
        out_channel: *mut i32,
    ) -> i32 {
        if !out_channel.is_null() {
            core::ptr::write(out_channel, 0);
        }
        SHIM_ERR_NOT_SUPPORTED
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_btrf_accept() -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `buf` must be writable for `cap` bytes.
    pub unsafe fn shim_btrf_recv(_handle: i32, _buf: *mut u8, _cap: i32) -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `buf` must be readable for `len` bytes.
    pub unsafe fn shim_btrf_send(_handle: i32, _buf: *const u8, _len: i32) -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_btrf_close(_handle: i32) -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_btrf_listen_stop() -> i32 {
        SHIM_ERR_NOT_SUPPORTED
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `rgb_out` must be writable for `cap` UTF-16 code units,
    /// `mask_out` must be writable for `cap` bytes, `w` must point to a writable `i32` and `h` must
    /// point to a writable `i32`.
    pub unsafe fn shim_app_icon_b(
        _uid3: u32,
        _size: i32,
        _rgb_out: *mut u16,
        _mask_out: *mut u8,
        _cap: i32,
        w: *mut i32,
        h: *mut i32,
    ) -> i32 {
        if !w.is_null() {
            core::ptr::write(w, 0);
        }
        if !h.is_null() {
            core::ptr::write(h, 0);
        }
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `rgb_out` must be writable for `cap` UTF-16 code units,
    /// `mask_out` must be writable for `cap` bytes, `w` must point to a writable `i32` and `h` must
    /// point to a writable `i32`.
    // The arity is the C++ shim's, not ours: this mirrors a declaration in
    // shim/inc/symbian_shim.h, and splitting the Rust side into a struct would leave the two
    // halves of the ABI describing different functions.
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn shim_app_icon_c(
        _uid3: u32,
        _size: i32,
        _bitmap_id: i32,
        _rgb_out: *mut u16,
        _mask_out: *mut u8,
        _cap: i32,
        w: *mut i32,
        h: *mut i32,
    ) -> i32 {
        if !w.is_null() {
            core::ptr::write(w, 0);
        }
        if !h.is_null() {
            core::ptr::write(h, 0);
        }
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must be writable for `cap` UTF-16 code units and
    /// `len` must point to a writable `i32`.
    pub unsafe fn shim_app_icon_file(
        _uid3: u32,
        _out: *mut u16,
        _cap: i32,
        len: *mut i32,
    ) -> i32 {
        if !len.is_null() {
            core::ptr::write(len, 0);
        }
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must point to a writable `i32`.
    pub unsafe fn shim_hal_get(_attr: i32, _out: *mut i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must point to a writable `u32`.
    pub unsafe fn shim_skin_color(_major: i32, _minor: i32, _index: i32, _out: *mut u32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must be writable for `cap` u32s, `width` must
    /// point to a writable `i32` and `height` must point to a writable `i32`.
    pub unsafe fn shim_skin_samples(
        _major: i32,
        _minor: i32,
        _out: *mut u32,
        _cap: i32,
        _width: *mut i32,
        _height: *mut i32,
    ) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out_mask` must point to a writable `u32`.
    pub unsafe fn shim_drive_list(_out: *mut u32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must point to a writable `ShimDriveInfo`.
    pub unsafe fn shim_drive_info(_d: i32, _out: *mut ShimDriveInfo) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must point to a writable `ShimVolumeInfo`.
    pub unsafe fn shim_volume_info(_d: i32, _out: *mut ShimVolumeInfo) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_has_capability(_cap: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `path` must be readable for `len` UTF-16 code units and
    /// `out` must point to a writable `u32`.
    pub unsafe fn shim_fs_att(_p: *const u16, _l: i32, _out: *mut u32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out_handle` must point to a writable `i32`.
    pub unsafe fn shim_msv_open(_out: *mut i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must point to a writable `i32`.
    pub unsafe fn shim_msv_mtm_count(_h: i32, _out: *mut i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_msv_refresh_registry(_h: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_msv_can_instantiate(_h: i32, _u: u32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must point to a writable `ShimMtmInfo`.
    pub unsafe fn shim_msv_mtm_info(_h: i32, _i: i32, _out: *mut ShimMtmInfo) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must point to a writable `i32`.
    pub unsafe fn shim_msv_folder_count(_h: i32, _f: i32, _out: *mut i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must point to a writable `i32`.
    pub unsafe fn shim_msv_folder_unread(_h: i32, _f: i32, _out: *mut i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_msv_close(_h: i32) {}
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `path` must be readable for `len` UTF-16 code units.
    pub unsafe fn shim_msv_install_mtm(_p: *const u16, _l: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `path` must be readable for `len` UTF-16 code units.
    pub unsafe fn shim_msv_deinstall_mtm(_p: *const u16, _l: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `name` must be readable for `name_len` UTF-16 code
    /// units and `out_id` must point to a writable `i32`.
    pub unsafe fn shim_msv_create_service(_h: i32, _u: u32, _n: *const u16, _nl: i32, _o: *mut i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `msg` must point to a readable `ShimNewMessage` and
    /// `out_id` must point to a writable `i32`.
    pub unsafe fn shim_msv_create_message(_h: i32, _m: *const ShimNewMessage, _o: *mut i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_msv_delete_entry(_h: i32, _id: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_msv_delete_services(_h: i32, _u: u32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must point to a writable `ShimMsvEntry`.
    pub unsafe fn shim_msv_entry(_h: i32, _id: i32, _out: *mut ShimMsvEntry) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out_ids` must be writable for `cap` i32s and
    /// `out_count` must point to a writable `i32`.
    pub unsafe fn shim_msv_children(_h: i32, _f: i32, _o: *mut i32, _c: i32, _n: *mut i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out_ids` must be writable for `cap` i32s and
    /// `out_count` must point to a writable `i32`.
    pub unsafe fn shim_msv_services(_h: i32, _u: u32, _o: *mut i32, _c: i32, _n: *mut i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must be writable for `cap` UTF-16 code units and
    /// `out_len` must point to a writable `i32`.
    pub unsafe fn shim_msv_body(_h: i32, _id: i32, _o: *mut u16, _c: i32, _l: *mut i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_msv_set_flags(_h: i32, _id: i32, _s: i32, _c: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_msv_move_entry(_h: i32, _id: i32, _p: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_msv_observe(_h: i32, _e: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_ncn_notify(_s: i32, _i: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_ncn_mark_unread(_s: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `name` must be readable for `len` UTF-16 code units and
    /// `out` must point to a writable `ShimDllProbe`.
    pub unsafe fn shim_dll_call_ordinal1(
        _n: *const u16,
        _l: i32,
        _a: u32,
        _o: *mut ShimDllProbe,
    ) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `name` must be readable for `len` UTF-16 code units.
    pub unsafe fn shim_dll_has_ordinal(_n: *const u16, _l: i32, _o: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_mem_free_kb() -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_mem_total_kb() -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_heap_used_kb() -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_prop_define_public(_c: u32, _k: u32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_prop_define(_c: u32, _k: u32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_prop_set(_c: u32, _k: u32, _v: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must point to a writable `i32`.
    pub unsafe fn shim_prop_get(_c: u32, _k: u32, out: *mut i32) -> i32 {
        if !out.is_null() {
            core::ptr::write(out, 0);
        }
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_prop_subscribe(_c: u32, _k: u32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_prop_unsubscribe(_c: u32, _k: u32) {}

    // The SQL stubs return NOT_READY rather than panicking, unlike the allocator's.
    // Reaching one means a host binary reached for the phone's database, which is a
    // mistake — but `symbian::sql` is written against a trait with an in-memory
    // implementation for exactly that case, so the useful failure is the error the
    // wrapper propagates, not an abort inside a stub nobody meant to call.
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `path` must be readable for `path_len` UTF-16 code
    /// units and `handle` must point to a writable `i32`.
    pub unsafe fn shim_sql_open(_p: *const u16, _pl: i32, _c: i32, handle: *mut i32) -> i32 {
        if !handle.is_null() {
            core::ptr::write(handle, 0);
        }
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_sql_close(_db: i32) {}
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `path` must be readable for `path_len` UTF-16 code
    /// units.
    pub unsafe fn shim_sql_delete(_p: *const u16, _pl: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `sql` must be readable for `len` bytes and `changed`
    /// must point to a writable `i32`.
    pub unsafe fn shim_sql_exec(_db: i32, _s: *const u8, _l: i32, changed: *mut i32) -> i32 {
        if !changed.is_null() {
            core::ptr::write(changed, 0);
        }
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must point to a writable `i32`.
    pub unsafe fn shim_sql_size(_db: i32, out: *mut i32) -> i32 {
        if !out.is_null() {
            core::ptr::write(out, 0);
        }
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `buf` must be writable for `cap` UTF-16 code units and
    /// `len` must point to a writable `i32`.
    pub unsafe fn shim_sql_last_error(_db: i32, _b: *mut u16, _c: i32, len: *mut i32) -> i32 {
        if !len.is_null() {
            core::ptr::write(len, 0);
        }
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `sql` must be readable for `len` bytes and `stmt` must
    /// point to a writable `i32`.
    pub unsafe fn shim_sql_prepare(_db: i32, _s: *const u8, _l: i32, stmt: *mut i32) -> i32 {
        if !stmt.is_null() {
            core::ptr::write(stmt, 0);
        }
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_sql_finalize(_stmt: i32) {}
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_sql_reset(_stmt: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_sql_step(_stmt: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `changed` must point to a writable `i32`.
    pub unsafe fn shim_sql_exec_stmt(_stmt: i32, changed: *mut i32) -> i32 {
        if !changed.is_null() {
            core::ptr::write(changed, 0);
        }
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_sql_bind_null(_stmt: i32, _i: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_sql_bind_int(_stmt: i32, _i: i32, _v: i64) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. No pointers cross here, so the shared contract is the
    /// whole of it.
    pub unsafe fn shim_sql_bind_real(_stmt: i32, _i: i32, _v: f64) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `text` must be readable for `len` UTF-16 code units.
    pub unsafe fn shim_sql_bind_text(_stmt: i32, _i: i32, _t: *const u16, _l: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `data` must be readable for `len` bytes.
    pub unsafe fn shim_sql_bind_blob(_stmt: i32, _i: i32, _d: *const u8, _l: i32) -> i32 {
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must point to a writable `i32`.
    pub unsafe fn shim_sql_column_type(_stmt: i32, _c: i32, out: *mut i32) -> i32 {
        if !out.is_null() {
            core::ptr::write(out, SHIM_SQL_NULL);
        }
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must point to a writable `i64`.
    pub unsafe fn shim_sql_column_int(_stmt: i32, _c: i32, out: *mut i64) -> i32 {
        if !out.is_null() {
            core::ptr::write(out, 0);
        }
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `out` must point to a writable `f64`.
    pub unsafe fn shim_sql_column_real(_stmt: i32, _c: i32, out: *mut f64) -> i32 {
        if !out.is_null() {
            core::ptr::write(out, 0.0);
        }
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `buf` must be writable for `cap` UTF-16 code units and
    /// `len` must point to a writable `i32`.
    pub unsafe fn shim_sql_column_text(_stmt: i32, _c: i32, _b: *mut u16, _cap: i32, len: *mut i32) -> i32 {
        if !len.is_null() {
            core::ptr::write(len, 0);
        }
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `buf` must be writable for `cap` bytes and `len` must
    /// point to a writable `i32`.
    pub unsafe fn shim_sql_column_blob(_stmt: i32, _c: i32, _b: *mut u8, _cap: i32, len: *mut i32) -> i32 {
        if !len.is_null() {
            core::ptr::write(len, 0);
        }
        SHIM_ERR_NOT_READY
    }
    /// # Safety
    ///
    /// The shim ABI contract in the crate docs. `name` must be readable for `len` UTF-16 code units and
    /// `out` must point to a writable `i32`.
    pub unsafe fn shim_sql_column_index(_stmt: i32, _n: *const u16, _l: i32, out: *mut i32) -> i32 {
        if !out.is_null() {
            core::ptr::write(out, -1);
        }
        SHIM_ERR_NOT_READY
    }
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
