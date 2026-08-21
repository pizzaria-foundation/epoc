//! Bluetooth: the state the platform's BT server keeps, as safe Rust.
//!
//! The server itself is in ROM and nothing here replaces it — the native OBEX push, the
//! headset profiles and the host's own `tools/btpush.py` all depend on it. What this module
//! does is read and write the state it keeps, so a change made here is one the native
//! Bluetooth screen sees, and one made there is one we see.
//!
//! # Three places, not one
//!
//! Bluetooth settings on S60 are scattered, and this module does not pretend otherwise:
//!
//! ```text
//!   power on/off      Central Repository 0x10204DA9 key 1     [POWER_REPO]/[POWER_KEY]
//!                     (read) and RNotifier 0x100059E2 (set)   set_power
//!   the registry      btmanclient — paired list, trust,       paired, set_trusted,
//!                     unpair, rename, the local record        unpair, rename, local
//!   the rest          Publish & Subscribe, category           the PS_* constants below,
//!                     0x101f75b6, keys from 0x10203637        read with crate::prop
//! ```
//!
//! The third row has no functions here on purpose. Those settings are ordinary P&S keys and
//! [`crate::prop`] already reads, writes and subscribes to any category — so this module names
//! the constants and gets out of the way rather than wrapping a wrapper.
//!
//! The one that pays for itself immediately is [`PS_REGISTRY_CHANGED`]: subscribe to it and a
//! paired-device list refreshes itself when the *native* app forgets a device, with no polling
//! and no code beyond the subscription.
//!
//! # Device-only, and gated
//!
//! Everything routes through `shim_bt_*`, which exists only in a binary whose `app.conf` set
//! `USE_BT=1` — six imports at once (`btmanclient btdevice bluetooth btextnotifiers esock
//! centralrepository`). Off-device, and in a build that did not opt in, every call reports
//! [`Error::NotReady`] or [`Error::NotSupported`], which is why the screens above this are
//! generic over [`Bt`] and tested against [`FakeBt`].

use alloc::string::String;
use alloc::vec::Vec;

use crate::error::{Error, Result};
use symbian_sys as sys;

/// `KCRUidBluetoothPowerState`. The repository `apps/netd` already reads to publish the status
/// bar's Bluetooth dot — named here so the two cannot drift apart.
pub const POWER_REPO: u32 = 0x1020_4DA9;
/// `KBTPowerState`. `EBTPowerOn` is 1.
pub const POWER_KEY: u32 = 1;

/// `KUidSystemCategory` — the P&S category every Bluetooth key below lives in. Not this app's
/// UID: these are the platform's keys, published by the BT server for anyone to read.
pub const PS_CATEGORY: u32 = 0x101f_75b6;

/// `KUidBluetoothPubSubKeyBase`, the base every key below offsets from.
const PS_BASE: u32 = 0x1020_3637;

/// Visibility, as the stack reports it (`KPropertyKeyBluetoothGetScanningStatus`).
pub const PS_SCANNING_GET: u32 = PS_BASE + 3;
/// Visibility, as a client sets it. Writable, unlike the `GET` key.
pub const PS_SCANNING_SET: u32 = PS_BASE + 4;
/// Limited-discoverable mode, read and write.
pub const PS_LIMITED_GET: u32 = PS_BASE + 5;
pub const PS_LIMITED_SET: u32 = PS_BASE + 6;
/// Class of device, read and write.
pub const PS_DEVICE_CLASS_GET: u32 = PS_BASE + 7;
pub const PS_DEVICE_CLASS_SET: u32 = PS_BASE + 8;
/// Bumped by the BT server whenever a registry table changes — the free refresh signal. The
/// value says *which* table ([`REGISTRY_REMOTE_TABLE`] and friends), and like `SHIM_EV_MSV` it
/// is a hint: a reader re-reads the registry rather than trusting the number.
pub const PS_REGISTRY_CHANGED: u32 = PS_BASE + 11;
pub const REGISTRY_REMOTE_TABLE: i32 = (PS_BASE + 12) as i32;
pub const REGISTRY_LOCAL_TABLE: i32 = (PS_BASE + 13) as i32;
pub const REGISTRY_CSY_TABLE: i32 = (PS_BASE + 14) as i32;
/// The local Bluetooth name, read and write. Both carry text, not an integer, so
/// [`crate::prop`]'s integer API does not reach them — a name change goes through
/// [`local`]/the registry instead.
pub const PS_NAME_GET: u32 = PS_BASE + 15;
pub const PS_NAME_SET: u32 = PS_BASE + 16;
/// Whether the stack accepts connections from unpaired devices, read and write.
pub const PS_PAIRED_ONLY_GET: u32 = PS_BASE + 18;
pub const PS_PAIRED_ONLY_SET: u32 = PS_BASE + 19;
/// Non-zero while the stack is running an inquiry. Worth respecting before starting another:
/// an inquiry disturbs an active audio link.
pub const PS_INQUIRY_ACTIVE: u32 = PS_BASE + 20;

/// How many devices either cache holds, mirroring the shim's own limit. A refresh that finds
/// more reports the full count and keeps the first `CACHE_LIMIT`.
pub const CACHE_LIMIT: usize = 32;

/// `THCIScanEnable`: which scans the radio answers, which is what "visible" means here.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Visibility {
    /// Neither found nor connectable. The radio is on and nothing can reach it.
    Hidden,
    /// Found by a scan, but not connectable.
    InquiryOnly,
    /// Connectable by a device that already knows the address, but not discoverable — which is
    /// what the native UI calls "hidden" and is the setting a paired headset still works with.
    PageOnly,
    /// Discoverable and connectable.
    Visible,
}

impl Visibility {
    /// The raw `THCIScanEnable`, or `None` for a value the registry never set.
    pub fn from_raw(raw: i32) -> Option<Self> {
        match raw {
            0 => Some(Visibility::Hidden),
            1 => Some(Visibility::InquiryOnly),
            2 => Some(Visibility::PageOnly),
            3 => Some(Visibility::Visible),
            _ => None,
        }
    }

    pub fn raw(self) -> i32 {
        match self {
            Visibility::Hidden => 0,
            Visibility::InquiryOnly => 1,
            Visibility::PageOnly => 2,
            Visibility::Visible => 3,
        }
    }

    /// Is this device findable by somebody scanning right now?
    pub fn discoverable(self) -> bool {
        matches!(self, Visibility::InquiryOnly | Visibility::Visible)
    }
}

/// A remote device, from the registry or from an inquiry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Device {
    pub addr: [u8; 6],
    /// The 24-bit class of device. [`major_class`](Device::major_class) is the part that says
    /// whether this is a headset or a phone.
    pub device_class: u32,
    pub name: String,
    /// The platform's name was longer than the shim carries. The name above is still the start
    /// of it, which is what a list row shows anyway.
    pub name_truncated: bool,
    pub paired: bool,
    /// S60's "trusted": connects without asking the user to authorise it.
    pub trusted: bool,
    pub blocked: bool,
    pub encrypted: bool,
    /// The name is the user-chosen friendly name, not the one the device reports.
    pub friendly_name: bool,
}

impl Device {
    /// The major device class — 4 is audio/video, 2 a phone, 1 a computer.
    pub fn major_class(&self) -> u8 {
        ((self.device_class >> 8) & 0x1f) as u8
    }

    /// The address as the phone writes it: six bytes, high first, colon separated.
    pub fn addr_string(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        let mut s = String::new();
        for (i, b) in self.addr.iter().enumerate() {
            if i > 0 {
                s.push(':');
            }
            s.push(HEX[(b >> 4) as usize] as char);
            s.push(HEX[(b & 0x0f) as usize] as char);
        }
        s
    }

    fn from_raw(raw: &sys::ShimBtDevice) -> Self {
        let kept = (raw.name_len as usize).min(raw.name.len());
        Device {
            addr: raw.addr,
            device_class: raw.device_class,
            name: String::from_utf16_lossy(&raw.name[..kept]),
            name_truncated: raw.name_len as usize > raw.name.len(),
            paired: raw.flags & sys::SHIM_BT_PAIRED != 0,
            trusted: raw.flags & sys::SHIM_BT_TRUSTED != 0,
            blocked: raw.flags & sys::SHIM_BT_BLOCKED != 0,
            encrypted: raw.flags & sys::SHIM_BT_ENCRYPT != 0,
            friendly_name: raw.flags & sys::SHIM_BT_FRIENDLY != 0,
        }
    }
}

/// This handset's own Bluetooth record. The `Option`s are fields the registry never set, which
/// is not the same as a default: an unset visibility is "the record does not say".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Local {
    pub addr: [u8; 6],
    pub device_class: u32,
    pub name: String,
    pub visibility: Option<Visibility>,
    pub limited_discoverable: Option<bool>,
    pub power_setting: Option<i32>,
    pub accept_paired_only: Option<bool>,
}

impl Local {
    fn from_raw(raw: &sys::ShimBtLocal) -> Self {
        let kept = (raw.name_len as usize).min(raw.name.len());
        Local {
            addr: raw.addr,
            device_class: raw.device_class,
            name: String::from_utf16_lossy(&raw.name[..kept]),
            visibility: Visibility::from_raw(raw.scan_enable),
            limited_discoverable: flag(raw.limited),
            power_setting: if raw.power_setting < 0 { None } else { Some(raw.power_setting) },
            accept_paired_only: flag(raw.paired_only),
        }
    }
}

fn flag(raw: i32) -> Option<bool> {
    match raw {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

/// Which route [`set_power`] actually took. Worth surfacing rather than swallowing: the
/// notifier asks the user and the CenRep write does not, so a UI that reported "done" for both
/// would be lying about one of them.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PowerRoute {
    /// The platform's own "Activate Bluetooth?" query answered, and the radio came up.
    Notifier,
    /// A direct write to the power key.
    CenRep,
}

/// Is the radio on?
pub fn power() -> Result<bool> {
    let mut on = 0i32;
    // SAFETY: `on` is a live local the shim writes exactly once.
    let rc = unsafe { sys::shim_bt_power_get(&mut on) };
    Error::check(rc)?;
    Ok(on == 1)
}

/// Turn the radio on or off.
///
/// **Turning it on can raise the platform's own query and wait for the user.** The shim bounds
/// that wait, but a caller on the GUI thread is still blocked for as long as it lasts, so this
/// belongs behind an explicit user action and never inside a redraw.
pub fn set_power(on: bool) -> Result<PowerRoute> {
    let mut via = 0i32;
    // SAFETY: `via` is a live local the shim writes exactly once.
    let rc = unsafe { sys::shim_bt_power_set(on as i32, &mut via) };
    Error::check(rc)?;
    match via {
        sys::SHIM_BT_VIA_NOTIFIER => Ok(PowerRoute::Notifier),
        sys::SHIM_BT_VIA_CENREP => Ok(PowerRoute::CenRep),
        // SHIM_OK with no route is the shim contradicting itself; treat it as the platform
        // failure it must have been rather than inventing a route.
        _ => Err(Error::Platform(sys::SHIM_ERR_GENERAL)),
    }
}

/// This handset's own record.
pub fn local() -> Result<Local> {
    let mut raw = sys::ShimBtLocal::default();
    // SAFETY: `raw` is a live local the shim fills or leaves at its defaults.
    let rc = unsafe { sys::shim_bt_local_get(&mut raw) };
    Error::check(rc)?;
    Ok(Local::from_raw(&raw))
}

/// Set visibility through the registry's local-device record.
pub fn set_visibility(v: Visibility) -> Result<()> {
    // SAFETY: no pointers.
    Error::check(unsafe { sys::shim_bt_visibility_set(v.raw()) })
}

/// Re-read the paired devices.
///
/// Returns what fits ([`CACHE_LIMIT`]) and the full count, in that order, so a caller can say
/// "12 of 40" rather than quietly showing twelve.
pub fn paired() -> Result<(Vec<Device>, usize)> {
    let mut total = 0i32;
    // SAFETY: `total` is a live local the shim writes exactly once.
    let rc = unsafe { sys::shim_bt_paired_refresh(&mut total) };
    Error::check(rc)?;
    Ok((read_cache(Cache::Paired), total.max(0) as usize))
}

/// The devices from the last [`inquiry`], read back out of the shim's cache.
pub fn found() -> Vec<Device> {
    read_cache(Cache::Found)
}

/// Run one inquiry, **blocking until it finishes**.
///
/// Daemon only. An inquiry takes on the order of ten seconds, and ten seconds inside
/// `rust_step` starves the window server, which freezes the whole phone rather than just this
/// app. A GUI app wants the asynchronous route instead.
///
/// Returns how many devices the inquiry reported; read them with [`found`]. A budget that
/// expires first is [`Error::TimedOut`] with whatever was collected still readable.
pub fn inquiry(budget_ms: i32, max_devices: i32) -> Result<usize> {
    let mut n = 0i32;
    // SAFETY: `n` is a live local the shim writes exactly once.
    let rc = unsafe { sys::shim_bt_inquiry_sync(budget_ms, max_devices, &mut n) };
    Error::check(rc)?;
    Ok(n.max(0) as usize)
}

/// Trust or untrust a device.
pub fn set_trusted(addr: &[u8; 6], trusted: bool) -> Result<()> {
    // SAFETY: `addr` is six live bytes the shim reads and does not keep.
    Error::check(unsafe { sys::shim_bt_set_trusted(addr.as_ptr(), trusted as i32) })
}

/// Forget a device.
pub fn unpair(addr: &[u8; 6]) -> Result<()> {
    // SAFETY: as above.
    Error::check(unsafe { sys::shim_bt_unpair(addr.as_ptr()) })
}

/// Give a device a friendly name. An empty name clears it, putting the device's own back.
pub fn rename(addr: &[u8; 6], name: &str) -> Result<()> {
    let units: Vec<u16> = name.encode_utf16().collect();
    // SAFETY: both pointers are live for the call and the shim copies what it needs.
    Error::check(unsafe {
        sys::shim_bt_rename(addr.as_ptr(), units.as_ptr(), units.len() as i32)
    })
}

/// Close the registry session and drop both caches.
pub fn close() -> Result<()> {
    // SAFETY: no pointers.
    Error::check(unsafe { sys::shim_bt_close() })
}

/// One Symbian error code per step of bringing an RFCOMM server socket up, as reported by
/// [`rfcomm_probe`]. A step's value is [`RFCOMM_STEP_OK`] (`KErrNone`) on success, a negative
/// Symbian error on failure, or [`RFCOMM_STEP_SKIPPED`] if the sequence never reached it.
pub type RfcommProbe = sys::ShimBtRfcommProbe;

/// `KErrNone` — the value each step of an [`RfcommProbe`] carries when it succeeded.
pub const RFCOMM_STEP_OK: i32 = 0;

/// The value of an [`RfcommProbe`] step that was never attempted, so a caller can tell
/// "failed" from "not reached".
pub const RFCOMM_STEP_SKIPPED: i32 = sys::SHIM_BT_PROBE_SKIPPED;

/// Bring an RFCOMM server socket up once — connect the socket server, open RFCOMM, claim a
/// server channel, bind, register and delete an SPP SDP record, and listen — then tear it all
/// down, reporting each step. This is the question that decides whether the remote-shell agent
/// can exist on this handset at all: an unsigned app on a stock ROM may be refused `Listen` or
/// the SDP write for want of `LocalServices`, and no `libsweep` DLL-open can tell us so.
///
/// Daemon only, like [`inquiry`]: the calls are fast but this belongs with the headless probe,
/// not on a GUI thread. Returns the filled [`RfcommProbe`]; [`Err`] only when the sequence
/// could not run at all (e.g. the build has no `USE_BTSOCK`).
pub fn rfcomm_probe() -> Result<RfcommProbe> {
    let mut p = RfcommProbe::default();
    // SAFETY: `p` is a live local the shim writes exactly once and does not keep.
    Error::check(unsafe { sys::shim_bt_rfcomm_probe(&mut p) })?;
    Ok(p)
}

/// The asynchronous RFCOMM *server* the remote-shell agent runs on. The phone listens and the
/// laptop dials in; there is no connect side here. Every call is non-blocking — completions
/// arrive as `SHIM_EV_BT_*` events driven from the daemon pump — so this whole module is for a
/// daemon, never a GUI thread.
///
/// One listener per process; accepted sockets are small integer handles. The receive and send
/// buffers are caller-owned and **must stay put and untouched** until the matching
/// `SHIM_EV_BT_RECV` / `SHIM_EV_BT_SENT` arrives, exactly as for [`crate::net::TcpStream`].
pub mod rfcomm {
    use super::{sys, Error, Result};
    use alloc::vec::Vec;

    /// Open the listener: claim a server channel, bind, register a persistent SPP SDP record
    /// named `name` (ASCII), and listen with `backlog`. Returns the advertised channel.
    pub fn listen(name: &str, backlog: i32) -> Result<u8> {
        let units: Vec<u16> = name.encode_utf16().collect();
        let mut chan = 0i32;
        // SAFETY: `units` outlives the call; the shim copies what it needs. `chan` is live.
        Error::check(unsafe {
            sys::shim_btrf_listen_start(backlog, units.as_ptr(), units.len() as i32, &mut chan)
        })?;
        Ok(chan.clamp(0, 255) as u8)
    }

    /// Start one asynchronous accept. Completion is `SHIM_EV_BT_ACCEPTED`, whose `handle` is
    /// the new socket (or `-1` with an error status). At most one accept may be outstanding.
    pub fn accept() -> Result<()> {
        // SAFETY: no pointers.
        Error::check(unsafe { sys::shim_btrf_accept() })
    }

    /// Start an asynchronous receive of up to `buf.len()` bytes. `buf` must stay valid and
    /// untouched until `SHIM_EV_BT_RECV` arrives for `handle`; that event's `a` is the count.
    pub fn recv(handle: i32, buf: &mut [u8]) -> Result<()> {
        // SAFETY: caller keeps `buf` alive until the completion event, per the contract above.
        Error::check(unsafe { sys::shim_btrf_recv(handle, buf.as_mut_ptr(), buf.len() as i32) })
    }

    /// Start an asynchronous send. `buf` must stay valid and untouched until `SHIM_EV_BT_SENT`.
    /// RFCOMM `Write` is all-or-nothing, so success means the whole buffer went.
    pub fn send(handle: i32, buf: &[u8]) -> Result<()> {
        // SAFETY: caller keeps `buf` alive until the completion event, per the contract above.
        Error::check(unsafe { sys::shim_btrf_send(handle, buf.as_ptr(), buf.len() as i32) })
    }

    /// Close one accepted socket, cancelling any outstanding recv/send first.
    pub fn close(handle: i32) -> Result<()> {
        // SAFETY: no pointers.
        Error::check(unsafe { sys::shim_btrf_close(handle) })
    }

    /// Deregister the SDP record and close the listener. Accepted sockets are left alone.
    pub fn listen_stop() -> Result<()> {
        // SAFETY: no pointers.
        Error::check(unsafe { sys::shim_btrf_listen_stop() })
    }
}

/// Which of the shim's two caches [`read_cache`] is walking.
///
/// A discriminator rather than the obvious function pointer, because the two entry points are
/// `extern "C"` on the device and plain Rust functions in the host stubs — so a signature that
/// named either calling convention would compile on exactly one target.
#[derive(Copy, Clone)]
enum Cache {
    Paired,
    Found,
}

/// Walk one of the shim's caches until it says there is no more.
///
/// The count is not asked for separately: `SHIM_ERR_NOT_FOUND` past the end *is* the end, and a
/// second source of truth about how many there are is a second thing that can be wrong.
fn read_cache(which: Cache) -> Vec<Device> {
    let mut out = Vec::new();
    for i in 0..CACHE_LIMIT {
        let mut raw = sys::ShimBtDevice::default();
        // SAFETY: `raw` is a live local; the shim fills it or reports not-found.
        let rc = unsafe {
            match which {
                Cache::Paired => sys::shim_bt_paired_get(i as i32, &mut raw),
                Cache::Found => sys::shim_bt_found_get(i as i32, &mut raw),
            }
        };
        if rc != sys::SHIM_OK {
            break;
        }
        out.push(Device::from_raw(&raw));
    }
    out
}

/// Managing Bluetooth, as an interface rather than a set of calls.
///
/// Same reasoning as [`crate::apps::Apps`] and [`crate::fs::Fs`]: a screen's logic — a list, a
/// selection, a confirm, a call that is refused — is pure and belongs under a host test, so the
/// device supplies [`ShimBt`] and the test supplies [`FakeBt`].
pub trait Bt {
    fn power(&mut self) -> Result<bool>;
    fn set_power(&mut self, on: bool) -> Result<PowerRoute>;
    fn local(&mut self) -> Result<Local>;
    fn set_visibility(&mut self, v: Visibility) -> Result<()>;
    /// The paired devices that fit, and the full count.
    fn paired(&mut self) -> Result<(Vec<Device>, usize)>;
    fn set_trusted(&mut self, addr: &[u8; 6], trusted: bool) -> Result<()>;
    fn unpair(&mut self, addr: &[u8; 6]) -> Result<()>;
    fn rename(&mut self, addr: &[u8; 6], name: &str) -> Result<()>;
}

/// [`Bt`] over the shim. Zero-sized: the caches live in the shim, not here.
#[derive(Copy, Clone, Debug, Default)]
pub struct ShimBt;

impl Bt for ShimBt {
    fn power(&mut self) -> Result<bool> {
        power()
    }
    fn set_power(&mut self, on: bool) -> Result<PowerRoute> {
        set_power(on)
    }
    fn local(&mut self) -> Result<Local> {
        local()
    }
    fn set_visibility(&mut self, v: Visibility) -> Result<()> {
        set_visibility(v)
    }
    fn paired(&mut self) -> Result<(Vec<Device>, usize)> {
        paired()
    }
    fn set_trusted(&mut self, addr: &[u8; 6], trusted: bool) -> Result<()> {
        set_trusted(addr, trusted)
    }
    fn unpair(&mut self, addr: &[u8; 6]) -> Result<()> {
        unpair(addr)
    }
    fn rename(&mut self, addr: &[u8; 6], name: &str) -> Result<()> {
        rename(addr, name)
    }
}

/// A Bluetooth stack made of a `Vec`, for the screens above this to be tested against.
///
/// Public, and not behind `#[cfg(test)]`, for the same reason [`crate::fs::MemFs`] is: the
/// crates above this one need it too, and a fake that only this crate's own tests can reach is
/// a fake the screens cannot use.
#[derive(Clone, Debug, Default)]
pub struct FakeBt {
    pub on: bool,
    pub local: Local,
    pub devices: Vec<Device>,
    /// What the full count should report, when the point of the test is that it exceeds what
    /// fits. Zero means "as many as `devices` holds".
    pub total_override: usize,
    /// Every call that changes something, in order — so a test asserts what was asked for and
    /// not merely what the state ended up as.
    pub log: Vec<String>,
    /// When set, every mutating call fails with it and nothing changes.
    pub fail: Option<Error>,
}

impl FakeBt {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a device by name and address, paired and untrusted.
    pub fn with_device(mut self, name: &str, addr: [u8; 6]) -> Self {
        self.devices.push(Device {
            addr,
            name: String::from(name),
            paired: true,
            ..Device::default()
        });
        self
    }

    fn index(&self, addr: &[u8; 6]) -> Result<usize> {
        self.devices.iter().position(|d| &d.addr == addr).ok_or(Error::NotFound)
    }

    fn guard(&self) -> Result<()> {
        match self.fail {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn note(&mut self, what: &str) {
        self.log.push(String::from(what));
    }
}

impl Bt for FakeBt {
    fn power(&mut self) -> Result<bool> {
        Ok(self.on)
    }

    fn set_power(&mut self, on: bool) -> Result<PowerRoute> {
        self.guard()?;
        self.on = on;
        self.note(if on { "power on" } else { "power off" });
        // The fake takes the silent route, because a fake cannot ask a user anything.
        Ok(PowerRoute::CenRep)
    }

    fn local(&mut self) -> Result<Local> {
        Ok(self.local.clone())
    }

    fn set_visibility(&mut self, v: Visibility) -> Result<()> {
        self.guard()?;
        self.local.visibility = Some(v);
        self.note("visibility");
        Ok(())
    }

    fn paired(&mut self) -> Result<(Vec<Device>, usize)> {
        self.guard()?;
        let kept: Vec<Device> =
            self.devices.iter().filter(|d| d.paired).take(CACHE_LIMIT).cloned().collect();
        let total = if self.total_override > 0 {
            self.total_override
        } else {
            self.devices.iter().filter(|d| d.paired).count()
        };
        Ok((kept, total))
    }

    fn set_trusted(&mut self, addr: &[u8; 6], trusted: bool) -> Result<()> {
        self.guard()?;
        let i = self.index(addr)?;
        self.devices[i].trusted = trusted;
        self.note(if trusted { "trust" } else { "untrust" });
        Ok(())
    }

    fn unpair(&mut self, addr: &[u8; 6]) -> Result<()> {
        self.guard()?;
        let i = self.index(addr)?;
        self.devices.remove(i);
        self.note("unpair");
        Ok(())
    }

    fn rename(&mut self, addr: &[u8; 6], name: &str) -> Result<()> {
        self.guard()?;
        let i = self.index(addr)?;
        self.devices[i].name = String::from(name);
        self.devices[i].friendly_name = !name.is_empty();
        self.note("rename");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ps_keys_are_the_offsets_bt_subscribe_documents() {
        // Transcribed constants, asserted rather than trusted: the base plus an offset is the
        // one kind of constant a typo hides in, and a wrong key reads as "the platform does not
        // support this" rather than as a mistake of ours.
        assert_eq!(PS_CATEGORY, 0x101f_75b6, "KUidSystemCategory");
        assert_eq!(PS_SCANNING_GET, 0x1020_363a);
        assert_eq!(PS_SCANNING_SET, 0x1020_363b);
        assert_eq!(PS_REGISTRY_CHANGED, 0x1020_3642);
        assert_eq!(PS_NAME_GET, 0x1020_3646);
        assert_eq!(PS_PAIRED_ONLY_SET, 0x1020_364a);
        assert_eq!(PS_INQUIRY_ACTIVE, 0x1020_364b);
    }

    #[test]
    fn hidden_is_not_the_same_as_unset() {
        // The distinction the Option exists for. A record that never set the field is not a
        // record that set it to zero, and a UI that conflated them would report a fresh phone
        // as deliberately invisible.
        assert_eq!(Visibility::from_raw(0), Some(Visibility::Hidden));
        assert_eq!(Visibility::from_raw(-1), None);
        assert_eq!(Visibility::from_raw(4), None);
    }

    #[test]
    fn page_only_is_connectable_but_not_findable() {
        // Which is exactly the native "hidden" setting, and the reason `discoverable` is a
        // method rather than an equality check against Visible.
        assert!(!Visibility::PageOnly.discoverable());
        assert!(Visibility::Visible.discoverable());
        assert!(Visibility::InquiryOnly.discoverable());
        assert!(!Visibility::Hidden.discoverable());
    }

    #[test]
    fn a_raw_device_becomes_one_with_an_owned_name() {
        let mut raw = sys::ShimBtDevice::default();
        raw.addr = [0x00, 0x1B, 0xAF, 0x12, 0x34, 0x56];
        raw.device_class = 0x24_04_18; // major class 4: audio/video
        raw.flags = sys::SHIM_BT_PAIRED | sys::SHIM_BT_TRUSTED | sys::SHIM_BT_FRIENDLY;
        let name: Vec<u16> = "HS-16".encode_utf16().collect();
        raw.name[..name.len()].copy_from_slice(&name);
        raw.name_len = name.len() as i32;

        let d = Device::from_raw(&raw);
        assert_eq!(d.name, "HS-16");
        assert_eq!(d.addr_string(), "00:1B:AF:12:34:56");
        assert_eq!(d.major_class(), 4);
        assert!(d.paired && d.trusted && d.friendly_name);
        assert!(!d.blocked && !d.encrypted);
        assert!(!d.name_truncated);
    }

    #[test]
    fn a_name_longer_than_the_shim_carries_says_so() {
        // The array is full and the length says there was more. Losing that would turn "we
        // showed the first 32 characters" into "the name is 32 characters long".
        let mut raw = sys::ShimBtDevice::default();
        raw.name_len = 200;
        let d = Device::from_raw(&raw);
        assert!(d.name_truncated);
        assert_eq!(d.name.len(), 32);
    }

    #[test]
    fn the_fake_records_what_was_asked_for_not_just_the_result() {
        let mut bt = FakeBt::new().with_device("HS-16", [1, 2, 3, 4, 5, 6]);
        bt.set_power(true).unwrap();
        bt.set_trusted(&[1, 2, 3, 4, 5, 6], true).unwrap();
        bt.rename(&[1, 2, 3, 4, 5, 6], "Fone").unwrap();

        assert_eq!(bt.log, ["power on", "trust", "rename"]);
        let (devices, total) = bt.paired().unwrap();
        assert_eq!(total, 1);
        assert_eq!(devices[0].name, "Fone");
        assert!(devices[0].trusted);
    }

    #[test]
    fn forgetting_an_unknown_device_is_not_found_and_changes_nothing() {
        let mut bt = FakeBt::new().with_device("HS-16", [1, 2, 3, 4, 5, 6]);
        assert_eq!(bt.unpair(&[9, 9, 9, 9, 9, 9]), Err(Error::NotFound));
        assert_eq!(bt.paired().unwrap().1, 1);
        assert!(bt.log.is_empty());
    }

    #[test]
    fn a_count_larger_than_what_fits_is_reported_as_both_numbers() {
        // So a screen can say "32 of 40" rather than quietly showing 32.
        let mut bt = FakeBt::new().with_device("one", [1; 6]);
        bt.total_override = 40;
        let (devices, total) = bt.paired().unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(total, 40);
    }
}
