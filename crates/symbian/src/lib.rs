//! Safe wrappers over the C++ shim.
//!
//! [`symbian_sys`] is the raw ABI: `unsafe`, raw pointers, error codes. This crate is
//! the layer that turns it into something an app can use without `unsafe` — owned
//! handles that close themselves, `Result` instead of negative integers, and the
//! retry loops that a partial read or a partial write needs.
//!
//! # Testable without a phone
//!
//! Every module here is written against a trait rather than against the shim
//! directly, with the shim as one implementation and an in-memory fake as another.
//! That is not architecture for its own sake: the interesting bugs in file I/O are in
//! the *loops* — reading until a zero-length read, writing until the whole buffer is
//! gone, replacing a file atomically — and those are pure logic that a host test can
//! exercise properly. The FFI call itself is the boring part.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_op_in_unsafe_fn)]

/// `pub` rather than plain, so [`log!`] can name `$crate::__alloc::format!` and expand in
/// a caller that never declared `extern crate alloc` itself — a requirement the caller
/// would otherwise discover only by the expansion failing.
pub extern crate alloc;

#[doc(hidden)]
pub use alloc as __alloc;

pub mod agenda;
pub mod apps;
pub mod backoff;
pub mod cenrep;
pub mod cache;
pub mod caps;
pub mod clipboard;
pub mod cpu;
pub mod device;
pub mod error;
pub mod fs;
pub mod hal;
pub mod handlers;
pub mod msg;
pub mod image;
pub mod intent;
pub mod log;
pub mod mem;
pub mod net;
pub mod process;
pub mod prop;
pub mod random;
pub mod tele;
pub mod tls;
pub mod url;
pub mod vol;
pub mod sql;

/// Seconds since the Unix epoch, from the handset's clock.
///
/// Drifts, and the user can change it from the clock application — MTProto rejects a
/// `msg_id` more than 30 s ahead or 300 s behind the server, which is why the handshake
/// reports the server's time and the client keeps the difference.
///
/// Here rather than in each application: reaching for the shim directly means an `unsafe`
/// block, and the crates above this one are `#![forbid(unsafe_code)]` on purpose.
pub fn unix_time() -> i64 {
    unsafe { symbian_sys::shim_unix_time() }
}

/// Seconds east of UTC, from the handset's locale setting. Negative for the Americas
/// (Brazil is -10800), positive for Europe. Added to a UTC timestamp to get the local
/// wall-clock time the user sees on the device's own clock.
pub fn utc_offset() -> i32 {
    unsafe { symbian_sys::shim_utc_offset() }
}

/// Block the current thread for `ms` milliseconds. For a headless helper backing off on a busy
/// resource; never call it on a GUI thread (it freezes the window server).
pub fn sleep_ms(ms: i32) {
    // SAFETY: a plain thread sleep with no pointers.
    unsafe { symbian_sys::shim_sleep_ms(ms) }
}

/// Microseconds since the handset booted, from the nanokernel tick.
///
/// Monotonic, unlike [`unix_time`], which the user can change from the clock application.
/// For measuring an interval that distinction is the whole point.
///
/// Here rather than in each application, for the same reason as [`unix_time`]: the crates
/// above this one are `#![forbid(unsafe_code)]`.
pub fn monotonic_us() -> u64 {
    unsafe { symbian_sys::shim_now_us() }
}

/// This process's own UID3 — the value the build passed as `SHIM_APP_UID3`. Used as the
/// Publish & Subscribe category an app publishes its own telemetry in (present/step stats),
/// so a reader (the app's own dev bridge) knows where to look. Zero if the build
/// did not set it. Here rather than in each app, for the usual `#![forbid(unsafe_code)]`
/// reason.
pub fn own_uid3() -> u32 {
    unsafe { symbian_sys::shim_own_uid3() }
}

/// A one-shot timer. The completion arrives as `SHIM_EV_TIMER` carrying this handle.
///
/// What it is for here: getting off the start-up path. Anything an application does before
/// its first frame is time the window is not on screen, and anything that can fail there
/// fails invisibly — there is no window to put the message in. Arming a timer costs
/// microseconds and moves the work to after the window server has drawn something.
pub fn timer_cancel(handle: i32) {
    unsafe { symbian_sys::shim_timer_cancel(handle) }
}

/// Arm a repeating timer, delivering a [`symbian_sys::SHIM_EV_TIMER`] every `ms`.
///
/// The handle it returns is what tells that event apart from any other timer's, and it is
/// how an app gets a periodic tick at all: the [`crate`]'s `App` trait has no tick method,
/// because on the device Avkon owns the loop and nothing calls into an app except through
/// an event. Anything that has to advance on its own — a state machine, an animation, a
/// poll — arms one of these and steps from the event.
///
/// Cancel with [`timer_cancel`].
pub fn timer_every(ms: i32) -> Result<i32> {
    let mut handle = 0i32;
    // SAFETY: `handle` is a live local; the shim writes at most one i32 through it.
    Error::check(unsafe { symbian_sys::shim_timer_every(ms, &mut handle) })?;
    Ok(handle)
}

pub fn timer_after(ms: i32) -> Result<i32> {
    let mut handle = 0i32;
    let rc = unsafe { symbian_sys::shim_timer_after(ms, &mut handle) };
    Error::check(rc)?;
    Ok(handle)
}

/// flogger's log directory. Not where this SDK's logs go — see [`DATA_DIR`] — but the second
/// rung of `log`'s ladder, for a handset where `C:\Data\` is not writable.
pub const LOG_DIR: &str = "C:\\logs\\";

/// Create `C:\logs\` if it is not there, so a file can be opened inside it.
///
/// Symbian has no "create parents on open": `RFile::Replace` on a path whose directory is
/// missing fails with `KErrPathNotFound`, and on a fresh handset that directory only exists
/// if flogger has been enabled. So anything writing a log there has to ask first.
///
/// Best-effort and silent — on the host it does nothing, and an existing directory is
/// success. Exposed because [`applog`] is not the only writer: an app keeping its own
/// formatted log file (a size cap, redaction, its own layout) still wants it in the one
/// place the monitor looks.
pub fn ensure_log_dir() {
    if let Ok(dir) = fs::Utf16Path::new(LOG_DIR) {
        let units = dir.as_units();
        // SAFETY: `units` is valid for `units.len()` u16 and only read. An already-existing
        // directory returns success, so the result carries nothing worth branching on.
        let _ = unsafe { symbian_sys::shim_mkdir(units.as_ptr(), units.len() as i32) };
    }
}

/// Where app logs go: `C:\Data\`, with the `logs_` prefix that marks a file there as one.
///
/// Not `C:\logs\`, which is flogger's and need not exist on a handset where flogger has
/// never run. `C:\Data\` always exists, is writable with no capability, is reachable over
/// USB and Bluetooth, and is where this SDK's apps already write everything else — one
/// directory to look in beats two. The prefix is required because that directory also holds
/// the user's own files.
pub const DATA_DIR: &str = "C:\\Data\\";
/// The prefix on a log file in [`DATA_DIR`]. See [`applog`].
pub const DATA_LOG_PREFIX: &str = "logs_";

/// Size at which a log file starts over. See [`fs::append_capped`].
pub const LOG_MAX: u64 = 64 * 1024;

/// Append a line to `C:\Data\logs_<name>.txt`, the SDK's device-log convention.
///
/// This is what `symbian::log!` writes through, and it is deliberately the same path an app
/// keeping its own richer log would choose: one location, one naming rule, so whatever
/// reads logs off a handset finds all of them.
///
/// Best-effort and silent — on the host, or if the file cannot be opened, it does nothing.
/// A diagnostic aid, never a data path. A trailing newline is added if absent, and the file
/// starts over once it passes 64 KB.
pub fn applog(name: &str, line: &str) {
    let _ = applog_to(&mut ShimFs, name, line);
}

/// [`applog`] over any [`Fs`], so the path, the append and the size cap are testable on the
/// host rather than only observable on a phone.
pub fn applog_to<F: Fs>(fs: &mut F, name: &str, line: &str) -> Result<()> {
    let p = log::data_path(name)?;
    let mut buf = alloc::string::String::from(line);
    if !buf.ends_with('\n') {
        buf.push('\n');
    }
    fs::append_capped(fs, &p, buf.as_bytes(), LOG_MAX)
}

#[cfg(test)]
mod applog_tests {
    use super::*;

    #[test]
    fn writes_to_the_data_convention_and_appends() {
        let mut fs = MemFs::new();
        applog_to(&mut fs, "myapp", "first").unwrap();
        applog_to(&mut fs, "myapp", "second\n").unwrap();
        let got = fs.contents("C:\\Data\\logs_myapp.txt").expect("the path is the contract");
        // The newline is added when absent and not doubled when present.
        assert_eq!(core::str::from_utf8(got).unwrap(), "first\nsecond\n");
    }

    #[test]
    fn starts_over_once_past_the_cap() {
        let mut fs = MemFs::new();
        let long = "x".repeat(1024);
        for _ in 0..70 {
            applog_to(&mut fs, "myapp", &long).unwrap();
        }
        let got = fs.contents("C:\\Data\\logs_myapp.txt").unwrap();
        // Past 64 KB it restarted, so what is left is the newest lines, not the oldest.
        assert!((got.len() as u64) < LOG_MAX, "still {} bytes", got.len());
        assert!(!got.is_empty(), "the line that tripped the cap must still be there");
    }
}
pub mod work;

pub use error::{Error, Result};
pub use fs::{File, Fs, MemFs, OpenMode, ShimFs};
pub use image::{Decoder, Image, Images, MemImages, ShimImages};
pub use net::{Bearer, Iap, Ipv4, Lookup, Net, Progress, RawEvent, ShimNet, TcpStream, UdpSocket};
