//! Enumerate and launch installed applications, for a launcher.
//!
//! Where [`crate::process`] launches a known executable by its path, this reaches the
//! application registry — `RApaLsSession` on the far side — so it can discover the apps it did
//! not ship and start them the way the native shell would. As with the rest of the crate every
//! `unsafe` block stays on this side of the wall so the caller never touches the raw ABI, and on
//! the host the shim functions are stubs returning [`Error::NotReady`], so these fail cleanly
//! under `cargo test` and the launcher UI is tested against the [`Apps`] fake instead.

use alloc::string::String;
use alloc::vec::Vec;

use symbian_sys as sys;

use crate::error::{Error, Result};

/// The longest caption the shim copies out. Matches `KCaptionMax` in `shim_apparc.cpp`; captions
/// are longer on the platform but no menu entry needs more, and a fixed bound keeps the crossing
/// a plain copy rather than a negotiation.
const CAPTION_MAX: usize = 64;

/// One installed application, as the registry sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppInfo {
    /// The application's UID3 — its identity, and what [`launch`] takes.
    pub uid3: u32,
    /// The menu caption, already decoded from UTF-16.
    pub caption: String,
    /// Whether the app asked to be kept out of the menu. Advisory for now; a later increment
    /// acts on it.
    pub hidden: bool,
    /// A control-panel item (a Settings/options sub-panel), registered as an app but not a
    /// standalone program. The launcher filters these out of the grid by default.
    pub system: bool,
}

/// Re-scan the installed applications and return them.
///
/// One server-side scan (`shim_apps_refresh`) then a copy of each entry. The list is whatever the
/// phone itself would show — including hidden apps, flagged rather than dropped, because a
/// launcher that wants to *un*hide one has to be able to see it first.
pub fn installed() -> Result<Vec<AppInfo>> {
    // SAFETY: no pointers; the shim scans and returns a count or a negative error.
    let count = unsafe { sys::shim_apps_refresh() };
    Error::check(count)?;
    let count = count.max(0);

    let mut out = Vec::with_capacity(count as usize);
    for index in 0..count {
        let mut uid3: u32 = 0;
        let mut hidden: u8 = 0;
        let mut buf = [0u16; CAPTION_MAX];
        let mut caption_len: i32 = 0;
        // SAFETY: `buf` is valid for CAPTION_MAX u16; the out-params are all live locals, and the
        // shim writes at most `cap` units and the length into `caption_len`.
        let rc = unsafe {
            sys::shim_app_at(
                index,
                &mut uid3,
                &mut hidden,
                buf.as_mut_ptr(),
                CAPTION_MAX as i32,
                &mut caption_len,
            )
        };
        // A racing uninstall could shrink the list between refresh and read; skip a now-missing
        // entry rather than fail the whole enumeration.
        if rc == sys::SHIM_ERR_NOT_FOUND {
            continue;
        }
        Error::check(rc)?;

        let n = (caption_len.max(0) as usize).min(CAPTION_MAX);
        let mut caption = String::from_utf16_lossy(&buf[..n]);
        // Some registered apps carry no caption at all (a few system entries on the E72). A blank
        // row reads as a bug, so fall back to the UID in hex — enough to tell them apart and pick one.
        if caption.trim().is_empty() {
            caption = alloc::format!("[{uid3:08X}]");
        }
        out.push(AppInfo {
            uid3,
            caption,
            // The shim packs flags into one byte: bit 0 = hidden, bit 1 = system (control panel).
            hidden: hidden & 1 != 0,
            system: hidden & 2 != 0,
        });
    }
    // Sort at the source so every consumer — the menu grid and the shortcut picker alike — sees one
    // stable alphabetical order. AppArc returns entries in registration/scan order, which reads as
    // random on screen; case-insensitive by caption is what a user expects a program list to be.
    out.sort_by_key(|a| a.caption.to_lowercase());
    Ok(out)
}

/// One app's icon: an RGB565 colour plane and an 8-bit coverage mask, both row-major `w`*`h` and
/// tightly packed (stride == `w`). This is what [`icon`] hands back and what the launcher's icon
/// cache holds; it is the exact shape `symbian_gfx`'s masked blit consumes, kept free of any gfx
/// dependency here so the services crate stays about the device, not the drawing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Icon {
    /// Width in pixels.
    pub w: i32,
    /// Height in pixels.
    pub h: i32,
    /// RGB565 pixels, `w`*`h`, row-major.
    pub pixels: Vec<u16>,
    /// 8-bit coverage per pixel (0 transparent, 255 opaque), `w`*`h`, row-major.
    pub mask: Vec<u8>,
}

/// Fetch app `uid3`'s icon at roughly `size` pixels through AppArc's masked bitmap.
///
/// **Prefer [`icon_c`].** Measured on the E72, this route has almost no useful domain: the handset's
/// app icons are MIF (scalable), which it cannot read, and for an app with no icon file at all it
/// reports success and hands back something that is not that app's icon. It also has no mask, so
/// what it does read draws as an opaque square. See `docs/device-notes.md`.
///
/// The buffers are sized to `size`*`size` up front, which is a ceiling rather than a request — the
/// real dimensions are read back from the bitmap and reported, and are frequently neither `size`
/// nor square. An app with no icon (or any platform refusal) is an ordinary [`Error`], which the
/// launcher reads as "draw the caption instead" — an icon is decoration, never a reason to fail a
/// screen. Off-device this is [`Error::NotReady`] like the rest of the family.
pub fn icon(uid3: u32, size: u16) -> Result<Icon> {
    icon_impl(uid3, size, Variant::A)
}

/// Diagnostic sibling of [`icon`] using the shim's variant-B fetch (the `TInt` GetAppIcon overload,
/// colour filled green). Only the isolated `iconprobe` app calls it, to tell whether that overload
/// panics on MIF-icon apps the way the default one does.
pub fn icon_b(uid3: u32, size: u16) -> Result<Icon> {
    icon_impl(uid3, size, Variant::B)
}

/// Fetch `uid3`'s icon through Avkon's icon utilities, reading the app's registered icon *file*
/// instead of asking AppArc for a masked bitmap.
///
/// This is the route that works for apps whose icon is a MIF (scalable, SVG-T) rather than an MBM —
/// the ones where [`icon`] cannot even try, because the platform panics the caller instead of
/// returning an error — and it is the only route that yields a real mask, so icons draw as cut-outs
/// rather than opaque squares.
///
/// This is the route to use. Measured on the E72: the right size, a real mask, and the app's own
/// icon, including for the MIF-icon apps that make [`icon`] panic the process.
///
/// `bitmap_id` is the colour plane's index inside the icon file; the mask is taken to be the next
/// index. It stays a parameter rather than a constant because it is a property of the handset, not
/// of the API: **16384** is what the E72 wants (the mifconv convention, confirmed on the device),
/// while MBM files index from 0. [`ICON_ID_MIF`] and [`ICON_ID_MBM`] name the two.
///
/// Available only in a binary built with `USE_AKNICON=1`; elsewhere this is
/// [`Error::NotSupported`], the same as any other facility that was not compiled in.
pub fn icon_c(uid3: u32, size: u16, bitmap_id: i32) -> Result<Icon> {
    icon_impl(uid3, size, Variant::C { bitmap_id })
}

/// The bitmap index [`icon_c`] wants for a MIF (scalable) icon file — what the E72's own apps use.
/// mifconv-generated headers number their entries from this offset.
pub const ICON_ID_MIF: i32 = 16384;
/// The bitmap index [`icon_c`] wants for a plain MBM icon file, which numbers from zero.
pub const ICON_ID_MBM: i32 = 0;

/// The full path of the file `uid3`'s icon comes from — `\resource\apps\something.mbm` or `.mif`.
///
/// Diagnostic, and the question pixels cannot answer: when a fetch succeeds but draws the wrong
/// picture, this says whether the platform read the right file at all. The extension is the other
/// half — `.mbm` is a plain bitmap, `.mif` is scalable, and that decides which fetch can read it.
///
/// Needs `USE_AKNICON=1`; elsewhere it is [`Error::NotSupported`].
pub fn icon_file(uid3: u32) -> Result<alloc::string::String> {
    // A Symbian path maxes out at 256 units; this is that plus room, and the shim clamps anyway.
    let mut buf = [0u16; 300];
    let mut len: i32 = 0;
    // SAFETY: `buf` and `len` are live locals; the shim writes at most `buf.len()` units and sets
    // `len` to how many.
    let rc = unsafe { sys::shim_app_icon_file(uid3, buf.as_mut_ptr(), buf.len() as i32, &mut len) };
    Error::check(rc)?;
    let n = (len.max(0) as usize).min(buf.len());
    Ok(alloc::string::String::from_utf16_lossy(&buf[..n]))
}

/// Which of the three icon fetches to run. They differ only in how the platform is asked; the
/// buffer handling, the size sanity check and the returned [`Icon`] are shared.
#[derive(Clone, Copy)]
enum Variant {
    /// `GetAppIcon(TSize)` into a `CApaMaskedBitmap`.
    A,
    /// `GetAppIcon(TInt)`, colour filled green — the diagnostic.
    B,
    /// The app's icon file through `AknIconUtils`.
    C { bitmap_id: i32 },
}

/// The largest icon edge this will allocate for, in pixels. Matches the shim's own row ceiling: it
/// refuses anything wider, so asking for more could never be satisfied. An icon that big would be
/// 128 KB of pixels, which is far past anything S60 registers.
const MAX_ICON_EDGE: i32 = 256;

/// Why one attempt failed. Private, because the size an overflow carries is only useful to the
/// retry immediately below — the public API keeps the crate's ordinary [`Error`].
enum IconErr {
    /// The buffers were too small; the platform wants this many pixels.
    TooSmall(i32, i32),
    /// Anything else, passed through unchanged.
    Other(Error),
}

fn icon_impl(uid3: u32, size: u16, variant: Variant) -> Result<Icon> {
    match icon_try(uid3, size as i32, variant) {
        Ok(icon) => Ok(icon),
        // The buffers were too small — but the shim reports the size it wants *before* refusing, so
        // ask again at exactly that. One retry, never a loop: the second attempt is sized from the
        // platform's own answer, so a second overflow would mean the answer itself was not usable.
        // This is what makes an icon of any size work rather than only one that fits the guess.
        Err(IconErr::TooSmall(w, h)) => {
            let edge = w.max(h);
            if edge <= 0 || edge > MAX_ICON_EDGE {
                return Err(Error::NotFound);
            }
            icon_try(uid3, edge, variant).map_err(|e| match e {
                IconErr::Other(e) => e,
                IconErr::TooSmall(..) => Error::NotFound,
            })
        }
        Err(IconErr::Other(e)) => Err(e),
    }
}

/// One attempt at `size`*`size`.
fn icon_try(uid3: u32, size: i32, variant: Variant) -> core::result::Result<Icon, IconErr> {
    let cap = (size as usize) * (size as usize);
    let mut pixels = alloc::vec![0u16; cap];
    let mut mask = alloc::vec![0u8; cap];
    let mut w: i32 = 0;
    let mut h: i32 = 0;
    // SAFETY: both buffers hold `cap` elements; the shim writes at most `cap` of each and sets
    // `w`/`h`. Pointers are to live locals for the duration of the call.
    let rc = unsafe {
        let (p, m, c) = (pixels.as_mut_ptr(), mask.as_mut_ptr(), cap as i32);
        match variant {
            Variant::A => sys::shim_app_icon(uid3, size, p, m, c, &mut w, &mut h),
            Variant::B => sys::shim_app_icon_b(uid3, size, p, m, c, &mut w, &mut h),
            Variant::C { bitmap_id } => {
                sys::shim_app_icon_c(uid3, size, bitmap_id, p, m, c, &mut w, &mut h)
            }
        }
    };
    // An overflow is the one error that carries information: the size the icon actually is. It goes
    // back to the caller as a value rather than being flattened into "no icon", which is what let
    // an icon larger than the guess be lost silently.
    if rc == sys::SHIM_ERR_OVERFLOW {
        return Err(IconErr::TooSmall(w, h));
    }
    Error::check(rc).map_err(IconErr::Other)?;

    // Trust the reported size only within what we allocated; a zero or over-cap size is a
    // malformed answer, treated as "no usable icon" rather than a slice out of bounds.
    let n = (w.max(0) as usize).saturating_mul(h.max(0) as usize);
    if n == 0 || n > cap {
        return Err(IconErr::Other(Error::NotFound));
    }
    pixels.truncate(n);
    mask.truncate(n);
    Ok(Icon { w, h, pixels, mask })
}

/// Start the installed app with this UID3, the way the shell would.
///
/// Returns once the platform has accepted the launch. Launching needs no capability, and the
/// started app runs with its own, not the launcher's.
pub fn launch(uid3: u32) -> Result<()> {
    // SAFETY: no pointers; the shim resolves the UID and hands it to AppArc.
    Error::check(unsafe { sys::shim_app_launch(uid3) })
}

/// Which convention to use when asking an app to open a URL.
///
/// There is no `OpenUrl` on S60. A native browser is asked to open an address by a *convention*,
/// and which convention a given firmware honours is a question only that firmware answers — so this
/// is a dial, not a choice made in the SDK. `apps/urlprobe` turns the dial on a real handset and the
/// answer becomes a note in `docs/device-notes.md`.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[repr(i32)]
pub enum LaunchDoc {
    /// `CApaCommandLine::SetDocumentNameL` + `EApaCommandOpen`, at an explicit app. The documented
    /// way to say "open this app on this thing".
    DocumentName = 0,
    /// `SetTailEndL("4 <url>")` — the S60 browser's own command set, where 4 means "open URL".
    /// Undocumented, and the one most likely to work on this handset.
    BrowserTail = 1,
    /// `RApaLsSession::StartDocument` at an explicit app; the platform builds the command line.
    StartDocument = 2,
    /// `StartDocument` with no app named, letting the platform resolve the handler. Expected to
    /// fail for a URL — it is not a file and has no recognizer — but it is the one route that would
    /// make a scheme registry unnecessary, which is worth a single call to rule out.
    Resolve = 3,
}

/// Ask app `uid3` to open `doc` (a URL), by `route`.
///
/// **Only for a binary built with `USE_LAUNCH_DOC=1`.** Every other build links a stub that returns
/// [`Error::NotSupported`], so calling this elsewhere is not a link failure — it is a quiet "no",
/// which is the honest answer for a binary that did not opt into the path.
///
/// `Ok(())` means the platform *accepted the launch*. It does not mean the URL opened: AppArc has
/// no way to report that, and an app that starts and ignores its command line looks identical to
/// one that honoured it. The only instrument is the handset.
pub fn launch_doc(uid3: u32, doc: &str, route: LaunchDoc) -> Result<()> {
    let units: Vec<u16> = doc.encode_utf16().collect();
    // SAFETY: the pointer and length describe `units`, which outlives the call; the shim copies
    // into a descriptor before doing anything that can leave.
    Error::check(unsafe {
        sys::shim_app_launch_doc(uid3, units.as_ptr(), units.len() as i32, route as i32)
    })
}

/// Hand a message to `uid3` if it is already running, bringing it to the foreground.
///
/// The half of "open a URL" that starting an application cannot do. `launch` and [`launch_doc`]
/// both *start* something; neither does anything useful to an application that is already up, and
/// on this handset the browser usually is — it accepts the launch, nothing comes forward, and the
/// user sees whatever page was already open.
///
/// The payload is 8-bit and application-specific. The S60 browser reads `"<command> <argument>"`,
/// where 4 means "open this URL"; the caller owns that convention, because it is the browser's and
/// not the platform's.
///
/// [`Error::NotFound`] when the application is not running — not a failure, but the caller's cue to
/// start it instead.
pub fn task_message(uid3: u32, msg: &[u8]) -> Result<()> {
    // SAFETY: the pointer and length describe `msg`, which outlives the call; the shim copies into
    // a descriptor before handing it to the window server.
    Error::check(unsafe { sys::shim_app_task_message(uid3, msg.as_ptr(), msg.len() as i32) })
}

/// Ask `uid3` to open `url`, by whichever convention this firmware honours.
///
/// The whole recipe in one call, because it is four calls and getting the *order* right is the part
/// that took a handset to learn:
///
/// 1. **Already running** → [`task_message`] with the browser's `"4 <url>"`. Tried first, and it is
///    the case the others cannot serve — every starting call accepts the launch, brings nothing
///    forward, and leaves the page that was already open.
/// 2. **Not running** → [`launch_doc`], each [`LaunchDoc`] route in turn.
/// 3. **Nothing took it** → [`launch`], so at least the application opens.
///
/// `Ok(())` means something accepted it. It does **not** mean the address was opened: an
/// application that starts and ignores its command line reports exactly what one that honoured it
/// reports, and no API here can tell them apart. A caller that cares should put the URL somewhere
/// the user can recover it — see [`crate::clipboard`] — *before* calling, not after, because there
/// is no failure to react to.
///
/// Only useful from a binary built with `USE_LAUNCH_DOC=1`; elsewhere the routes answer
/// [`Error::NotSupported`] and this degrades to a plain launch.
///
/// # It also steps aside
///
/// On success this calls [`to_background`], because a hand-off that keeps the screen is not a
/// hand-off. The application being asked to open the address will put something in front of the
/// user — a page, a connection prompt, a "which access point" query — and on S60 3rd Edition that
/// something arrives *behind* whatever is already in front. `shim_net.cpp` learned this on the
/// handset for the CommsDat dialog and fixes it the same way; there is no reason for every caller
/// to learn it again.
///
/// The step aside happens only when something accepted the URL. A failed hand-off leaves the
/// caller in front, which is where it has to be to tell the user that nothing opened.
pub fn open_at(uid3: u32, url: &str) -> Result<()> {
    // `4` is the S60 browser's own command for "open this URL". It is the browser's convention and
    // not the platform's, which is why it is built here at the call rather than inside the shim.
    let cmd = alloc::format!("4 {url}");

    let accepted = accept_url(uid3, url, &cmd);
    if accepted.is_ok() {
        // Whether we manage to leave says nothing about whether the URL opened, so it is not the
        // caller's error to handle: off-device it is always NotReady, and on a handset the worst
        // case is the symptom this exists to remove rather than a new one.
        let _ = to_background();
    }
    accepted
}

/// The routes, in the order the handset taught. Split out of [`open_at`] so the step aside has one
/// answer to act on rather than four returns to intercept.
fn accept_url(uid3: u32, url: &str, cmd: &str) -> Result<()> {
    // NotFound is the ordinary case here: the app simply is not running yet, so a failure is not
    // worth reporting — it only means the routes below still have work to do.
    if task_message(uid3, cmd.as_bytes()).is_ok() {
        return Ok(());
    }
    for route in [
        LaunchDoc::BrowserTail,
        LaunchDoc::DocumentName,
        LaunchDoc::StartDocument,
        LaunchDoc::Resolve,
    ] {
        // The prefixed form for the routes that reach the browser's own command parsing, the bare
        // URL for the ones that do not.
        let doc = match route {
            LaunchDoc::DocumentName | LaunchDoc::StartDocument => cmd,
            _ => url,
        };
        if launch_doc(uid3, doc, route).is_ok() {
            return Ok(());
        }
    }
    launch(uid3)
}

/// Ask the installed app with this UID3 to close, through the window server.
///
/// `TApaTask::EndTask`: it posts the application's window group a close event and the application
/// exits on its own. **No capability**, which is the whole point — see [`kill`], which needs
/// `PowerMgmt` and faults the caller without it.
///
/// The cost is honest and small: an application that ignores the event stays running. A task
/// switcher can live with that; what it cannot live with is dying.
///
/// [`Error::NotFound`] if the app has no running task.
pub fn end(uid3: u32) -> Result<()> {
    // SAFETY: no pointers; the shim resolves the task and posts the event.
    Error::check(unsafe { sys::shim_app_end(uid3) })
}

/// Kill the installed app with this UID3 through the window server — **needs `PowerMgmt`**.
///
/// `TApaTask::KillTask` is `RThread::Kill` on a thread in another process. Without the capability
/// the kernel does not answer with an error: a capability violation on an executive call panics the
/// *calling* thread, so this takes the caller down with no panic file and nothing in any log. That
/// is measured, on the E72, and it is what the launcher's task switcher did to the launcher every
/// time somebody closed an app.
///
/// So: use [`end`] unless this process declares `PowerMgmt` and means it. Kept because for an app
/// that does — a supervisor, an escape hatch — killing is the point.
///
/// [`Error::NotFound`] if the app has no running task.
pub fn kill(uid3: u32) -> Result<()> {
    // SAFETY: no pointers; the shim finds the task by UID and asks the window server to kill it.
    Error::check(unsafe { sys::shim_app_kill(uid3) })
}

/// The UID3s of applications running right now — the window-server task list, front-to-back
/// (most recent first), deduplicated and excluding non-application window groups. For a "recent
/// apps" / task-switch action. Off-device this is [`Error::NotReady`].
pub fn running() -> Result<Vec<u32>> {
    const CAP: usize = 64;
    let mut buf = [0u32; CAP];
    // SAFETY: `buf` holds CAP u32; the shim writes at most `cap` and returns the count.
    let n = unsafe { sys::shim_apps_running(buf.as_mut_ptr(), CAP as i32) };
    Error::check(n)?;
    let n = (n.max(0) as usize).min(CAP);
    Ok(buf[..n].to_vec())
}

/// Drop this application behind whatever else is on screen, without closing it.
///
/// For a helper the user never asked to see. A GUI app is brought to the foreground when it is
/// started, which is right for something opened from a menu and wrong for a background job another
/// app kicked off — a task that needs the Avkon environment (so it cannot be a headless daemon) but
/// has no business taking the screen from whatever started it.
pub fn to_background() -> Result<()> {
    // SAFETY: no arguments; the shim reads the current UI environment or reports not-ready.
    Error::check(unsafe { sys::shim_app_to_background() })
}

/// Bring this application back to the front, focus included.
///
/// The mirror of [`to_background`], and it is [`kill`] that needs it: ending another app's task can
/// leave *that* app in front — some are restarted by the platform, and a dying window group
/// reshuffles the z-order — so a task manager that closes an app can find itself behind the app it
/// just closed. Re-asserting the foreground is how it stays the thing the user is looking at.
///
/// A no-op that reports [`Error::NotReady`] off-device, and harmless when already in front.
pub fn to_foreground() -> Result<()> {
    // SAFETY: no arguments; the shim reads the current UI environment or reports not-ready.
    Error::check(unsafe { sys::shim_app_to_foreground() })
}

/// Turn resident (launcher) behaviour on or off.
///
/// On: the app no longer closes on the End key — it drops to the background, the way a home screen
/// does — and the Menu key is captured so pressing it brings the app forward. This is what turns a
/// fullscreen app into a launcher you return to rather than one you open once. It needs `SwEvent`
/// (to capture a key system-wide), which a ROM-patched handset grants at load. Off-device it is a
/// no-op that reports [`Error::NotReady`], so the reference launcher can call it unconditionally.
pub fn set_resident(on: bool) -> Result<()> {
    // SAFETY: no pointers; the shim captures/cancels a key on this app's window group.
    Error::check(unsafe { sys::shim_set_resident(on as i32) })
}

/// Listing and launching installed apps, as an interface rather than a set of calls.
///
/// The reason this is a trait mirrors [`crate::process::Procs`]: a launcher's screen — navigation,
/// selection, an empty list, a launch that is refused — is pure logic that belongs under a host
/// test, and the device supplies only [`ShimApps`] while the test supplies a fake with a fixed
/// roster. Same shape and same reasoning as [`crate::fs::Fs`] / [`crate::fs::MemFs`].
pub trait Apps {
    /// Re-scan and return the installed applications.
    fn installed(&mut self) -> Result<Vec<AppInfo>>;
    /// Start the app with this UID3.
    fn launch(&mut self, uid3: u32) -> Result<()>;
}

/// [`Apps`] over the shim. Zero-sized; there is nothing to hold — the cache lives in the shim.
#[derive(Copy, Clone, Debug, Default)]
pub struct ShimApps;

impl Apps for ShimApps {
    fn installed(&mut self) -> Result<Vec<AppInfo>> {
        installed()
    }

    fn launch(&mut self, uid3: u32) -> Result<()> {
        launch(uid3)
    }
}

/// The document another application asked this one to open, if any.
///
/// A URL, in practice: the launcher routes a link to whichever application the user set for that
/// scheme, and this is the receiving end of every route it tries — the document name of a cold
/// start and the task message sent to an application already running. The caller does not have to
/// know which happened.
///
/// Reading it consumes it. A request left behind is a link from a previous run opening by itself on
/// the next one, which is the kind of thing that looks like the phone is haunted.
pub fn open_request() -> Option<alloc::string::String> {
    let mut buf = [0u16; 1024];
    let n = unsafe { sys::shim_app_open_request(buf.as_mut_ptr(), buf.len() as i32) };
    if n <= 0 {
        return None;
    }
    alloc::string::String::from_utf16(&buf[..n as usize]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake roster: whatever apps you hand it, and a record of what got launched. The launcher
    /// screen is tested against this so navigation and launch have somewhere to run without a
    /// phone. Lives here beside `Apps` so any consumer's tests can reach it.
    #[derive(Clone, Debug, Default)]
    pub struct FakeApps {
        pub roster: Vec<AppInfo>,
        pub launched: Vec<u32>,
        pub fail_launch: bool,
    }

    impl Apps for FakeApps {
        fn installed(&mut self) -> Result<Vec<AppInfo>> {
            Ok(self.roster.clone())
        }
        fn launch(&mut self, uid3: u32) -> Result<()> {
            if self.fail_launch {
                return Err(Error::Platform(sys::SHIM_ERR_NOT_FOUND));
            }
            self.launched.push(uid3);
            Ok(())
        }
    }

    fn app(uid3: u32, caption: &str) -> AppInfo {
        AppInfo { uid3, caption: String::from(caption), hidden: false, system: false }
    }

    #[test]
    fn fake_lists_and_records_launches() {
        let mut apps = FakeApps {
            roster: alloc::vec![app(0xE1, "One"), app(0xE2, "Two")],
            ..Default::default()
        };
        let listed = apps.installed().unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[1].caption, "Two");

        apps.launch(0xE2).unwrap();
        assert_eq!(apps.launched, alloc::vec![0xE2]);
    }

    #[test]
    fn shim_apps_are_not_ready_on_host() {
        // The device path degrades to NotReady off-device rather than pretending to enumerate.
        assert!(matches!(installed(), Err(Error::NotReady)));
        assert!(matches!(launch(0xE0000001), Err(Error::NotReady)));
        assert!(matches!(icon(0xE0000001, 44), Err(Error::NotReady)));
    }
}
