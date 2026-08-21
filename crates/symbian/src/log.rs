//! The device log: one switch, one file, one call.
//!
//! ```ignore
//! symbian::log!("[net] connect rc={rc}");
//! ```
//!
//! The point of this module is that the call site above is the *only* thing an app author
//! has to know. There is no logger to construct, nothing to thread through the app struct,
//! no `#[cfg]` at the call site, and nothing to remember to remove before a release build.
//! Write the line while the code is fresh; decide later whether the build carries it.
//!
//! # Why this exists at all
//!
//! Symbian gives an application no log, no console and no debugger. A failure on this device
//! is a sentence on a 320x240 screen that someone has to photograph and type back, and a
//! failure that closes the application leaves nothing at all. The Telegram client's own
//! version of this file found a 1000x clock error, a truncating append and four distinct
//! bearer failures that had been indistinguishable — which is why it is now in the SDK
//! rather than in one app.
//!
//! # Two switches
//!
//! `DEBUG=` decides what is *in the binary*; the flag file at `C:\Data\_logs\<app>.on` decides
//! whether a build that carries logging is writing it right now. See [`flag_path`] and
//! [`set_enabled`] — the second one is what a Settings screen calls, and it takes effect without
//! a restart.
//!
//! # The build switch
//!
//! `DEBUG=1` in the app's `app.conf`. `tools/symbuild` turns that into `SYMBIAN_DEBUG` in
//! the environment of the cargo invocation, and [`ENABLED`] is a `const bool` read from it.
//! So [`log!`](crate::log) expands to `if false { … }` in a build with `DEBUG=0`, which the
//! optimiser deletes along with the format string. Off means absent, not quiet.
//!
//! Deliberately an environment variable rather than a cargo feature: a feature has to be
//! declared in every app's `Cargo.toml` and forwarded down to this crate, which is the
//! per-app boilerplate this module exists to remove. `symbuild` owns the whole cargo
//! invocation, so it sets this for every app with no cooperation from any of them. Cargo
//! tracks env vars read by `option_env!`, so flipping `DEBUG` rebuilds what it should.
//!
//! # Where the lines go
//!
//! `C:\Data\_logs\<app>.txt` — under the directory this SDK's apps already write everything to:
//! no capability, always present (unlike flogger's `C:\logs\`), and reachable over USB and
//! Bluetooth, so the log comes off the phone with no host and no network in the picture.
//! [`path_label`] reports which candidate actually won, because a hardcoded label here once
//! claimed a path the log was not going to.
//!
//! Appended and capped rather than rewritten: a flash append was measured at 23 us for 64 KB
//! on the E72 (`docs/device-notes.md`), so a line costs about nothing, and the file surviving
//! across launches is what makes the last line before a crash readable on the next one. Past
//! [`crate::LOG_MAX`] the file starts over — see [`crate::fs::append_capped`].
//!
//! It is still file I/O on the GUI thread, which is why this is a diagnostic aid and not a
//! data path, and why logging per frame is a bad idea even when each line is cheap.
//!
//! # What must never be in it
//!
//! The auth key, any password, an API secret, or a whole phone number. This file gets pasted
//! into a chat window; a log line is a file, and a file with an auth key in it is the
//! account. Sizes and error names are what a diagnosis is made of. [`redact_phone`] is here
//! so that the safe version of "which number was it" costs one call:
//!
//! ```ignore
//! symbian::log!("[act] send code to {}", symbian::log::redact_phone(&number));
//! ```

use alloc::string::String;

use crate::fs::{self, ShimFs, Utf16Path};

/// Whether this build carries logging, decided by `DEBUG` in `app.conf`.
///
/// A `const`, so the branch in [`log!`](crate::log) is resolved at compile time and a
/// `DEBUG=0` build contains neither the call nor the string it would have formatted.
pub const ENABLED: bool = option_env!("SYMBIAN_DEBUG").is_some();

/// The log file's basename, from `SYMBIAN_APP_NAME`.
///
/// The fallback matters for a `cargo test` on the host, where nothing sets the variable and
/// the sink does nothing anyway.
pub const APP: &str = match option_env!("SYMBIAN_APP_NAME") {
    Some(name) => name,
    None => "app",
};

/// Longest a single line may be, before the stamp. A runaway `{:?}` on a large structure is
/// a real way to fill a phone's disk with one call.
const MAX_LINE: usize = 1024;

/// Where the log is going, once resolved. `None` until the first line, and still `None` if
/// nowhere turned out to be writable.
static mut PATH: Option<Utf16Path> = None;
/// Which candidate won, for [`path_label`].
static mut LABEL: &str = "(not opened yet)";
/// Uptime at the first line, so every stamp is relative to launch rather than to 1970.
static mut START_US: u64 = 0;
static mut RESOLVED: bool = false;
/// The run-time switch, read once from the flag file. `None` until then.
static mut RUNTIME: Option<bool> = None;
/// The run-time switch: `C:\Data\_logs\<name>.on`, one byte, `1` or `0`.
///
/// # Why a second switch at all
///
/// `DEBUG=` in `app.conf` is a *build* decision and has to stay one: off means the call sites
/// and their format strings are not in the binary. But "is this build allowed to log" and "do I
/// want it logging right now" are different questions, and only the second one can be answered
/// by the person holding the phone. A shipped build with `DEBUG=1` that always writes is a
/// build that spends flash on every user; one that never writes cannot be diagnosed.
///
/// So the file. Absent means **on**, which keeps a `DEBUG=1` build behaving exactly as it did
/// before this existed — the switch can only ever turn something off that was already compiled
/// in.
///
/// It is a file in a public directory rather than a private-cage setting on purpose: a headless
/// daemon has no screen to put a toggle on, and this way its log can be turned on from the host
/// without a rebuild —
///
/// ```text
/// epoc sh --push /dev/null 'C:\Data\_logs\connd.on'    # (a 0-byte file reads as on)
/// ```
///
/// — or from another application on the phone, which is how one Settings screen turns the log
/// on for a whole family of processes.
pub fn flag_path(name: &str) -> crate::Result<Utf16Path> {
    let mut path = String::from(crate::DATA_LOG_DIR);
    path.push_str(name);
    path.push_str(".on");
    Utf16Path::new(&path)
}

/// How a flag file's contents read. `None` for "no opinion", so a truncated or corrupt file
/// leaves the default rather than silently turning logging off.
///
/// Separate from the I/O so the decision is host-testable: `0`, `n` and `f` (any case) are off,
/// anything else — including an empty file — is on.
pub fn decode_flag(bytes: &[u8]) -> Option<bool> {
    match bytes.first() {
        None => Some(true),
        Some(b'0') | Some(b'n') | Some(b'N') | Some(b'f') | Some(b'F') => Some(false),
        Some(_) => Some(true),
    }
}

/// Whether a line written now would be kept: this build carries logging *and* the run-time
/// switch is on.
///
/// The file is read once per process, on the first call. A toggle made through
/// [`set_enabled`] takes effect immediately regardless, because it sets the same flag it
/// persists — so a Settings screen does not have to restart anything to be believed.
pub fn enabled() -> bool {
    ENABLED && runtime_on()
}

fn runtime_on() -> bool {
    // SAFETY: single-threaded, as everything else in this module.
    if let Some(on) = unsafe { core::ptr::read(core::ptr::addr_of!(RUNTIME)) } {
        return on;
    }
    let mut fs = ShimFs;
    let on = flag_path(APP)
        .ok()
        .and_then(|p| fs::read(&mut fs, &p).ok().flatten())
        .and_then(|b| decode_flag(&b))
        .unwrap_or(true);
    // SAFETY: as above.
    unsafe { RUNTIME = Some(on) };
    on
}

/// Turn the log on or off, now and for every later launch.
///
/// Immediate: the next `log!` obeys it without a restart. Persisted best-effort — if the flag
/// file cannot be written the setting still holds for this run, which is the half the user is
/// looking at.
///
/// Writes nothing else and reads nothing else, so it is safe to call from a settings screen on
/// the GUI thread.
pub fn set_enabled(on: bool) {
    set_enabled_for(APP, on);
    // SAFETY: single-threaded.
    unsafe { RUNTIME = Some(on) };
}

/// [`set_enabled`], but for *another* process's log — how an app with a screen turns logging on
/// for the headless daemons it starts.
///
/// Only the file is written: the other process reads it on its next line (or its next launch,
/// if it has already read it). That is the honest limit of a flag in a file, and it is why this
/// is a separate call rather than something [`set_enabled`] does silently for a list of names.
pub fn set_enabled_for(name: &str, on: bool) {
    crate::ensure_log_dir();
    let mut fs = ShimFs;
    if let Ok(p) = flag_path(name) {
        let _ = fs::write_atomic(&mut fs, &p, if on { b"1" } else { b"0" });
    }
}

/// The conventional path for an app's log: `C:\Data\_logs\<name>.txt`.
///
/// Exposed because [`crate::applog`] is not the only writer — an app keeping its own
/// formatted log (its own layout, its own cap) still wants it where the tooling looks. The
/// caller is responsible for the directory existing; [`crate::ensure_log_dir`] is the call,
/// and both [`crate::applog`] and [`line`] make it before they open anything.
pub fn data_path(name: &str) -> crate::Result<Utf16Path> {
    let mut path = String::from(crate::DATA_LOG_DIR);
    path.push_str(name);
    path.push_str(".txt");
    Utf16Path::new(&path)
}

/// Pick the log file, once, and remember which one it was.
///
/// The ladder, in order, and each rung is there because of a way the one above it fails:
///
/// 1. `C:\Data\_logs\<app>.txt` — where this SDK's apps write, proven writable with no
///    capability and readable over USB and Bluetooth. Created by
///    [`crate::ensure_log_dir`], since a directory that is not there fails the open.
/// 2. `C:\logs\<app>.log` — flogger's directory, for a handset where `C:\Data\` is not
///    writable.
/// 3. `C:\logs_<app>.txt` — the drive root, when neither directory can be opened. Still the
///    flat name: the point of this rung is that no directory had to exist.
/// 4. the app's private cage — always works and cannot be read from outside, which makes it
///    useless for the one job this has, but better than losing the log.
///
/// Probed by opening for append rather than by writing an empty file: a write-probe would
/// truncate the log left by the previous launch, which is exactly the log worth having.
fn resolve() {
    // SAFETY: single-threaded. Every caller is on the GUI thread (or the daemon's single
    // active scheduler), the same assumption `symbian_app`'s telemetry statics make.
    unsafe {
        if RESOLVED {
            return;
        }
        RESOLVED = true;
        START_US = crate::monotonic_us();
    }

    crate::ensure_log_dir();

    let mut fs = ShimFs;
    let data = match data_path(APP) {
        Ok(p) => Some(p),
        Err(_) => None,
    };

    let mut candidates: [(Option<Utf16Path>, &'static str); 3] = [
        (data, "C:\\Data\\_logs\\<app>.txt"),
        (build(crate::LOG_DIR, APP, ".log"), "C:\\logs\\<app>.log"),
        (build("C:\\", "logs_", APP), "C:\\logs_<app>.txt"),
    ];
    for (candidate, label) in candidates.iter_mut() {
        let Some(p) = candidate.take() else { continue };
        if fs::File::open(&mut fs, &p, fs::OpenMode::Append).is_ok() {
            // SAFETY: as above.
            unsafe {
                PATH = Some(p);
                LABEL = label;
            }
            return;
        }
    }

    // Last resort: the private cage. Unreadable from outside, so it is the rung that keeps
    // the log rather than the rung that delivers it.
    if let Ok(dir) = fs::private_path(&mut fs) {
        if let Ok(p) = Utf16Path::join(dir.as_units(), "log.txt") {
            // SAFETY: as above.
            unsafe {
                PATH = Some(p);
                LABEL = "(private)\\log.txt";
            }
        }
    }
}

/// `dir + mid + tail` as a path, or `None` if it does not fit. Used only by [`resolve`],
/// where a path too long for the buffer means "try the next candidate".
fn build(dir: &str, mid: &str, tail: &str) -> Option<Utf16Path> {
    let mut s = String::from(dir);
    s.push_str(mid);
    s.push_str(tail);
    Utf16Path::new(&s).ok()
}

/// Where the log is being written — the path that actually won, not the one this was expected
/// to take. Empty-ish text until the first line, since the ladder is walked lazily.
pub fn path_label() -> &'static str {
    // SAFETY: single-threaded; a `&'static str` read.
    unsafe { LABEL }
}

/// Append one line, stamped with milliseconds since the first line.
///
/// Called by [`log!`](crate::log). The [`ENABLED`] guard is repeated here so a direct caller
/// cannot write to a `DEBUG=0` build's log either.
pub fn line(text: &str) {
    if !enabled() {
        return;
    }
    resolve();

    // SAFETY: single-threaded. Reached through `addr_of!` rather than by naming the static,
    // because a `&` on a `static mut` is a warning today and an error in edition 2024 — the
    // aliasing rule it protects is satisfied here by there being one thread.
    let start = unsafe { core::ptr::read(core::ptr::addr_of!(START_US)) };
    let path = unsafe { &*core::ptr::addr_of!(PATH) };
    let Some(path) = path.as_ref() else { return };

    let ms = crate::monotonic_us().saturating_sub(start) / 1000;
    let mut out = String::new();
    let mut stamp = String::new();
    push_i64(&mut stamp, ms as i64);
    while stamp.len() < 6 {
        stamp.insert(0, ' ');
    }
    out.push_str(&stamp);
    out.push_str("  ");
    if text.len() > MAX_LINE {
        // On a char boundary: a `String` sliced anywhere else panics, and a panic on this
        // device is a silent vanish.
        let mut end = MAX_LINE;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        out.push_str(&text[..end]);
        out.push_str(" …");
    } else {
        out.push_str(text);
    }
    out.push('\n');

    let mut fs = ShimFs;
    let _ = fs::append_capped(&mut fs, path, out.as_bytes(), crate::LOG_MAX);
}

/// A phone number with everything but the first three and last two characters replaced.
///
/// A log file gets pasted into a chat window. The country code and the last two digits are
/// enough to tell one attempt from another, and the rest is nobody's business.
pub fn redact_phone(number: &str) -> String {
    let mut s = String::new();
    let n = number.chars().count();
    for (i, c) in number.chars().enumerate() {
        if i < 3 || i + 2 >= n {
            s.push(c);
        } else {
            s.push('*');
        }
    }
    s
}

/// Format an integer without `core::fmt`, for the one place that cannot use it: the stamp on
/// every line. `core::fmt` on this target pulls in far more code than a two-line loop, and a
/// `DEBUG=0` build should not link any of it on account of a log.
fn push_i64(s: &mut String, mut v: i64) {
    if v < 0 {
        s.push('-');
        v = -v;
    }
    let mut d = [0u8; 20];
    let mut n = 0;
    loop {
        d[n] = b'0' + (v % 10) as u8;
        n += 1;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    while n > 0 {
        n -= 1;
        s.push(d[n] as char);
    }
}

/// Append a formatted line to this app's log, when the build has `DEBUG=1`.
///
/// Takes the same arguments as `format!`. Formatting happens inside the guard, so a build
/// with logging off does not allocate a string to hand to a function that discards it.
///
/// A leading category in brackets is the convention the tooling reads, and costs nothing to
/// follow:
///
/// ```ignore
/// symbian::log!("[net] connect rc={rc}");
/// symbian::log!("[ui] screen={:?} rows={}", screen, rows);
/// ```
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {{
        // `ENABLED` first and by name, so a DEBUG=0 build resolves this to `if false` and the
        // format string never reaches the binary. The run-time switch is checked before the
        // format runs, so a log turned off costs a static read rather than an allocation.
        if $crate::log::ENABLED && $crate::log::enabled() {
            $crate::log::line(&$crate::__alloc::format!($($arg)*));
        }
    }};
}

#[cfg(test)]
mod tests {
    /// The macro has to expand and typecheck in a crate that never declared
    /// `extern crate alloc` itself — that is what the `__alloc` re-export is for, and a
    /// compile failure here is the only way that regression would show up.
    #[test]
    fn the_macro_expands_without_alloc_in_scope() {
        crate::log!("value={}", 1 + 1);
        crate::log!("no arguments");
    }

    /// The flag file decides, and an unreadable or truncated one decides nothing.
    ///
    /// The empty case is the one worth pinning: `epoc sh --push /dev/null …` is the documented
    /// way to turn a daemon's log on from the host, and it writes zero bytes.
    #[test]
    fn a_flag_file_reads_as_a_switch() {
        assert_eq!(crate::log::decode_flag(b""), Some(true), "an empty file is on");
        assert_eq!(crate::log::decode_flag(b"1"), Some(true));
        assert_eq!(crate::log::decode_flag(b"1\n"), Some(true));
        assert_eq!(crate::log::decode_flag(b"0"), Some(false));
        assert_eq!(crate::log::decode_flag(b"0\r\n"), Some(false));
        assert_eq!(crate::log::decode_flag(b"no"), Some(false));
        assert_eq!(crate::log::decode_flag(b"false"), Some(false));
        // Anything else is on: the switch may only turn off what was compiled in, so an
        // unrecognised byte must not be the thing that silences a build.
        assert_eq!(crate::log::decode_flag(b"yes"), Some(true));
        assert_eq!(crate::log::decode_flag(&[0xff]), Some(true));
    }

    /// The flag file sits beside the log it switches, so one directory holds both and
    /// `ls C:\Data\_logs` says which apps log and which of them are on.
    #[test]
    fn the_flag_sits_next_to_the_log() {
        let text = |p: crate::fs::Utf16Path| -> alloc::string::String {
            char::decode_utf16(p.as_units().iter().copied()).map(|c| c.unwrap()).collect()
        };
        assert_eq!(text(crate::log::data_path("connd").unwrap()), "C:\\Data\\_logs\\connd.txt");
        assert_eq!(text(crate::log::flag_path("connd").unwrap()), "C:\\Data\\_logs\\connd.on");
    }

    /// The switch is the environment and nothing else — asserted against the variable rather
    /// than against `false`, because a developer with `DEBUG=1` exported in their shell
    /// should not see a test failure for it. What must hold is that the constants track the
    /// variables, and that every entry point is a silent no-op on the host either way.
    #[test]
    fn the_constants_track_the_environment() {
        assert_eq!(crate::log::ENABLED, option_env!("SYMBIAN_DEBUG").is_some());
        assert_eq!(crate::log::APP, option_env!("SYMBIAN_APP_NAME").unwrap_or("app"));
        crate::log::line("this goes nowhere on the host");
        crate::log!("[net] formatted rc={}", -4180);
    }

    /// The path this SDK promises. A change here silently orphans every log on every
    /// handset, so it is pinned by a test rather than by a comment.
    #[test]
    fn the_data_path_is_the_convention() {
        let p = crate::log::data_path("myapp").unwrap();
        let got: alloc::string::String =
            char::decode_utf16(p.as_units().iter().copied()).map(|c| c.unwrap()).collect();
        assert_eq!(got, "C:\\Data\\_logs\\myapp.txt");
    }

    #[test]
    fn a_phone_number_keeps_only_its_ends() {
        // The file is pasted into a chat window. Enough to tell two attempts apart, and no
        // more than that.
        let got = crate::log::redact_phone("5511987654321");
        assert_eq!(got, "551********21");
        assert!(!got.contains("98765"), "the middle survived");
    }

    #[test]
    fn short_numbers_are_not_widened_or_panicked_on() {
        // i + 2 >= n covers every character of a short string, which is the safe direction:
        // redaction that reveals nothing beats an index that panics.
        assert_eq!(crate::log::redact_phone(""), "");
        assert_eq!(crate::log::redact_phone("12"), "12");
        assert_eq!(crate::log::redact_phone("1234"), "1234");
    }

    #[test]
    fn numbers_render_including_negatives() {
        // Every Symbian error is negative and the log is mostly error codes.
        let mut s = alloc::string::String::new();
        super::push_i64(&mut s, -4180);
        assert_eq!(s, "-4180");
        let mut s = alloc::string::String::new();
        super::push_i64(&mut s, 0);
        assert_eq!(s, "0");
    }
}
