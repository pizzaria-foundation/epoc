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
    out.sort_by(|a, b| a.caption.to_lowercase().cmp(&b.caption.to_lowercase()));
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

/// Fetch app `uid3`'s icon at roughly `size` pixels — the same icon the native menu draws.
///
/// The buffers are sized to `size`*`size` up front; the shim scales the registered icon to fit and
/// reports the real dimensions back. An app with no icon (or any platform refusal) is an ordinary
/// [`Error`], which the launcher reads as "draw the caption instead" — an icon is decoration, never
/// a reason to fail a screen. Off-device this is [`Error::NotReady`] like the rest of the family.
pub fn icon(uid3: u32, size: u16) -> Result<Icon> {
    icon_impl(uid3, size, false)
}

/// Diagnostic sibling of [`icon`] using the shim's variant-B fetch (the `TInt` GetAppIcon overload,
/// colour filled green). Only the isolated `iconprobe` app calls it, to tell whether that overload
/// panics on MIF-icon apps the way the default one does.
pub fn icon_b(uid3: u32, size: u16) -> Result<Icon> {
    icon_impl(uid3, size, true)
}

fn icon_impl(uid3: u32, size: u16, variant_b: bool) -> Result<Icon> {
    let size = size as i32;
    let cap = (size as usize) * (size as usize);
    let mut pixels = alloc::vec![0u16; cap];
    let mut mask = alloc::vec![0u8; cap];
    let mut w: i32 = 0;
    let mut h: i32 = 0;
    // SAFETY: both buffers hold `cap` elements; the shim writes at most `cap` of each and sets
    // `w`/`h`. Pointers are to live locals for the duration of the call.
    let rc = unsafe {
        let (p, m, c) = (pixels.as_mut_ptr(), mask.as_mut_ptr(), cap as i32);
        if variant_b {
            sys::shim_app_icon_b(uid3, size, p, m, c, &mut w, &mut h)
        } else {
            sys::shim_app_icon(uid3, size, p, m, c, &mut w, &mut h)
        }
    };
    Error::check(rc)?;

    // Trust the reported size only within what we allocated; a zero or over-cap size is a
    // malformed answer, treated as "no usable icon" rather than a slice out of bounds.
    let n = (w.max(0) as usize).saturating_mul(h.max(0) as usize);
    if n == 0 || n > cap {
        return Err(Error::NotFound);
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

/// Kill the installed app with this UID3 through the window server.
///
/// Unlike [`crate::process::kill`], which uses `RProcess::Kill` and needs `PowerMgmt` to end a
/// process it did not create, this goes through the window server (`TApaTask::KillTask`) — the way
/// one app stops another it does not own, and the way to end a resident launcher that will not
/// close itself. [`Error::NotFound`] if the app has no running task.
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
