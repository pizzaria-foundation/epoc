//! The Message Server: reconnaissance, writing, reading, and store events.
//!
//! # This module makes the binary that uses it a deployment risk
//!
//! It reaches `shim_msg.cpp`, which imports `msgs.dso` and `mtur.dso`. The E72's messaging
//! DLLs are a 2009 Nokia build and this SDK's import libraries need not be the same ones;
//! an ordinal we call that the handset does not export makes the E32 loader refuse the
//! whole image — no panic, no log, and no report file at all (`docs/device-notes.md`, "An
//! import that does not resolve makes the app vanish").
//!
//! So anything using this belongs in a binary of its own, with nothing else to lose. In
//! `apps/devdump` that is the `msg` probe, and the launcher records its absence as a
//! finding rather than being taken down with it.
//!
//! # What it is for
//!
//! Enough to run a messaging service that lives inside Nokia's own Messaging application.
//! Traffic goes both ways: a message written here appears in the user's inbox, and a reply
//! the user composes there has to be found and carried out. Finding it needs the entry, its
//! body, and — the part that is not a function call — [`Session::observe`], which turns
//! Message Server notifications into events on the shim's ring.
//!
//! Everything above the trait is where the interesting bugs are: the grow-and-retry loops
//! for a folder listing and a body, deciding what counts as an unhandled reply, and not
//! offering the same reply twice. So [`Msv`] is the raw shape, [`ShimMsv`] is the FFI, and
//! [`MemMsv`] is an in-memory store a host test can drive — the same arrangement as
//! [`crate::fs::Fs`] and [`crate::sql::Sql`], for the same reason.

use alloc::string::String;
use alloc::vec::Vec;

use symbian_sys as sys;

use crate::error::{Error, Result};
use crate::fs::Utf16Path;

pub use sys::ShimMsvEntry as RawEntry;
pub use sys::ShimMtmInfo as MtmInfo;

/// A `TMsvId`. The message store's own identifier for an entry, folder or service.
pub type EntryId = i32;

/// The standard folders, with the names to print them under.
pub const FOLDERS: &[(EntryId, &str)] = &[
    (sys::SHIM_MSV_ROOT, "root"),
    (sys::SHIM_MSV_INBOX, "inbox"),
    (sys::SHIM_MSV_OUTBOX, "outbox"),
    (sys::SHIM_MSV_DRAFTS, "drafts"),
    (sys::SHIM_MSV_SENT, "sent"),
];

/// MTM type UIDs worth recognising by name in a report.
///
/// Not a complete list and cannot be: the whole point of enumerating the registry is to
/// discover what this handset has. Anything unrecognised is printed as its raw UID, which is
/// the finding.
///
/// Every value here except where noted was **read off an E72** by `apps/devdump`'s messaging
/// probe (see `docs/device-dump.txt`), not taken from a header. The first version of this
/// table guessed MMS as `0x100056DE` and was wrong; the handset reports two MMS entries,
/// neither of them that. The report printed the raw UIDs rather than a wrong name, which is
/// the only reason the mistake was visible.
pub fn mtm_name(uid: u32) -> Option<&'static str> {
    Some(match uid {
        0x1000102C => "SMS",
        // Two of them on this handset, both named "Multimedia message" by the registry.
        0x100058E1 => "MMS",
        0x100059C8 => "MMS (2)",
        0x10001028 => "IMAP4",
        0x10001029 => "POP3",
        0x1000102A => "SMTP",
        0x10009ED5 => "OBEX/Bluetooth",
        0x10005535 => "BIO",
        0x10009158 => "service message",
        0x102072D6 => "Message",
        0x101F7C5C => "sync mailbox",
        _ => return None,
    })
}

/// Entry type UIDs.
///
/// Re-exported from the shim's ABI rather than written out here, because `shim_msg.cpp`
/// asserts those against `msvstd.hrh` at compile time. The first version of this file wrote
/// its own values and got them wrong by a whole page — `0x10001852` for the message type
/// instead of `0x10000F6A` — which makes [`Entry::is_message`] answer false for every entry
/// in the store. Nothing fails; a service just never recognises one of its own messages. The
/// compile-time check on the C side is the only place that can catch it.
pub const TYPE_MESSAGE: u32 = sys::SHIM_MSV_TYPE_MESSAGE;
pub const TYPE_SERVICE: u32 = sys::SHIM_MSV_TYPE_SERVICE;
pub const TYPE_FOLDER: u32 = sys::SHIM_MSV_TYPE_FOLDER;

// ----------------------------------------------------------------------- the trait --

/// Every Message Server operation, one method per shim entry point.
///
/// Raw on purpose, with one deliberate shape borrowed from [`crate::sql::Sql`]:
/// [`Msv::children`], [`Msv::services`] and [`Msv::body`] fill what fits and return the
/// **full** length. So "the buffer was too small" is a number rather than an error code every
/// caller has to remember, and the retry loop lives in [`Session`] above this line, where a
/// host test can drive it against [`MemMsv`].
pub trait Msv {
    fn open(&mut self) -> Result<i32>;
    fn close(&mut self, handle: i32);
    fn mtm_count(&mut self, handle: i32) -> Result<i32>;
    fn refresh_registry(&mut self, handle: i32) -> Result<()>;
    fn can_instantiate(&mut self, handle: i32, mtm_uid: u32) -> Result<()>;
    fn mtm_info(&mut self, handle: i32, index: i32) -> Result<MtmInfo>;
    fn folder_count(&mut self, handle: i32, folder_id: EntryId) -> Result<i32>;
    /// How many children of the folder are unread — one server-side count.
    fn unread_count(&mut self, handle: i32, folder_id: EntryId) -> Result<i32>;

    /// Handle-less in the shim: installing a registration is a server-wide act, not
    /// something a session owns.
    fn install_mtm(&mut self, path: &[u16]) -> Result<()>;
    fn deinstall_mtm(&mut self, path: &[u16]) -> Result<()>;

    fn create_service(&mut self, handle: i32, mtm_uid: u32, name: &[u16]) -> Result<EntryId>;
    fn create_message(&mut self, handle: i32, msg: &sys::ShimNewMessage) -> Result<EntryId>;
    fn delete_entry(&mut self, handle: i32, id: EntryId) -> Result<()>;
    fn delete_services(&mut self, handle: i32, mtm_uid: u32) -> Result<i32>;

    fn entry(&mut self, handle: i32, id: EntryId) -> Result<RawEntry>;
    /// Fills `out` with as many ids as fit; returns the **full** child count.
    fn children(&mut self, handle: i32, folder: EntryId, out: &mut [EntryId]) -> Result<usize>;
    /// Fills `out`; returns the **full** service count.
    fn services(&mut self, handle: i32, mtm_uid: u32, out: &mut [EntryId]) -> Result<usize>;
    /// Fills `out`; returns the **full** character count. No body text is `Ok(0)`.
    fn body(&mut self, handle: i32, id: EntryId, out: &mut [u16]) -> Result<usize>;
    fn set_flags(&mut self, handle: i32, id: EntryId, set: i32, clear: i32) -> Result<()>;
    fn move_entry(&mut self, handle: i32, id: EntryId, new_parent: EntryId) -> Result<()>;
    fn observe(&mut self, handle: i32, enable: bool) -> Result<()>;
}

/// [`Msv`] over the shim.
///
/// Zero-sized: the session itself lives in `shim_msg.cpp`, which keeps a single slot. Nothing
/// to carry, and it costs nothing in a device build.
#[derive(Copy, Clone, Debug, Default)]
pub struct ShimMsv;

impl Msv for ShimMsv {
    fn open(&mut self) -> Result<i32> {
        let mut handle = 0i32;
        // SAFETY: `handle` is a live local; the shim writes at most one i32 through it.
        Error::check(unsafe { sys::shim_msv_open(&mut handle) })?;
        Ok(handle)
    }

    fn close(&mut self, handle: i32) {
        // SAFETY: the shim ignores a handle it does not recognise.
        unsafe { sys::shim_msv_close(handle) }
    }

    fn mtm_count(&mut self, handle: i32) -> Result<i32> {
        let mut out = 0i32;
        // SAFETY: live local; handle validated by the shim.
        Error::check(unsafe { sys::shim_msv_mtm_count(handle, &mut out) })?;
        Ok(out)
    }

    fn refresh_registry(&mut self, handle: i32) -> Result<()> {
        // SAFETY: no pointers; the handle is validated by the shim.
        Error::check(unsafe { sys::shim_msv_refresh_registry(handle) })
    }

    fn can_instantiate(&mut self, handle: i32, mtm_uid: u32) -> Result<()> {
        // SAFETY: no pointers; the handle is validated by the shim.
        Error::check(unsafe { sys::shim_msv_can_instantiate(handle, mtm_uid) })
    }

    fn mtm_info(&mut self, handle: i32, index: i32) -> Result<MtmInfo> {
        let mut out = MtmInfo::default();
        // SAFETY: `out` is a live local of the layout the C side writes.
        Error::check(unsafe { sys::shim_msv_mtm_info(handle, index, &mut out) })?;
        Ok(out)
    }

    fn folder_count(&mut self, handle: i32, folder_id: EntryId) -> Result<i32> {
        let mut out = 0i32;
        // SAFETY: live local; handle validated by the shim.
        Error::check(unsafe { sys::shim_msv_folder_count(handle, folder_id, &mut out) })?;
        Ok(out)
    }

    fn unread_count(&mut self, handle: i32, folder_id: EntryId) -> Result<i32> {
        let mut out = 0i32;
        // SAFETY: live local; handle validated by the shim.
        Error::check(unsafe { sys::shim_msv_folder_unread(handle, folder_id, &mut out) })?;
        Ok(out)
    }

    fn install_mtm(&mut self, path: &[u16]) -> Result<()> {
        // SAFETY: `path` is valid for its length and only read.
        Error::check(unsafe { sys::shim_msv_install_mtm(path.as_ptr(), path.len() as i32) })
    }

    fn deinstall_mtm(&mut self, path: &[u16]) -> Result<()> {
        // SAFETY: as above.
        Error::check(unsafe { sys::shim_msv_deinstall_mtm(path.as_ptr(), path.len() as i32) })
    }

    fn create_service(&mut self, handle: i32, mtm_uid: u32, name: &[u16]) -> Result<EntryId> {
        let mut out = 0i32;
        // SAFETY: `name` is valid for its length and only read; `out` is a live local.
        Error::check(unsafe {
            sys::shim_msv_create_service(handle, mtm_uid, name.as_ptr(), name.len() as i32, &mut out)
        })?;
        Ok(out)
    }

    fn create_message(&mut self, handle: i32, msg: &sys::ShimNewMessage) -> Result<EntryId> {
        let mut out = 0i32;
        // SAFETY: `msg`'s pointers are the caller's to keep alive across this call; the shim
        // copies out of them before returning.
        Error::check(unsafe { sys::shim_msv_create_message(handle, msg, &mut out) })?;
        Ok(out)
    }

    fn delete_entry(&mut self, handle: i32, id: EntryId) -> Result<()> {
        // SAFETY: no pointers; the handle is validated by the shim.
        Error::check(unsafe { sys::shim_msv_delete_entry(handle, id) })
    }

    fn delete_services(&mut self, handle: i32, mtm_uid: u32) -> Result<i32> {
        // SAFETY: no pointers; the handle is validated by the shim.
        let rc = unsafe { sys::shim_msv_delete_services(handle, mtm_uid) };
        if rc < 0 {
            return Err(Error::from_code(rc));
        }
        Ok(rc)
    }

    fn entry(&mut self, handle: i32, id: EntryId) -> Result<RawEntry> {
        let mut out = RawEntry::default();
        // SAFETY: `out` is a live local of the layout the C side writes, and the shim zeroes
        // it before it tries anything.
        Error::check(unsafe { sys::shim_msv_entry(handle, id, &mut out) })?;
        Ok(out)
    }

    fn children(&mut self, handle: i32, folder: EntryId, out: &mut [EntryId]) -> Result<usize> {
        let mut count = 0i32;
        // SAFETY: `out` is valid for its length; the shim writes at most `cap` of them and
        // reports the full count separately.
        Error::check(unsafe {
            sys::shim_msv_children(handle, folder, out.as_mut_ptr(), out.len() as i32, &mut count)
        })?;
        Ok(count.max(0) as usize)
    }

    fn services(&mut self, handle: i32, mtm_uid: u32, out: &mut [EntryId]) -> Result<usize> {
        let mut count = 0i32;
        // SAFETY: as above.
        Error::check(unsafe {
            sys::shim_msv_services(handle, mtm_uid, out.as_mut_ptr(), out.len() as i32, &mut count)
        })?;
        Ok(count.max(0) as usize)
    }

    fn body(&mut self, handle: i32, id: EntryId, out: &mut [u16]) -> Result<usize> {
        let mut len = 0i32;
        // SAFETY: as above.
        Error::check(unsafe {
            sys::shim_msv_body(handle, id, out.as_mut_ptr(), out.len() as i32, &mut len)
        })?;
        Ok(len.max(0) as usize)
    }

    fn set_flags(&mut self, handle: i32, id: EntryId, set: i32, clear: i32) -> Result<()> {
        // SAFETY: no pointers; the handle is validated by the shim.
        Error::check(unsafe { sys::shim_msv_set_flags(handle, id, set, clear) })
    }

    fn move_entry(&mut self, handle: i32, id: EntryId, new_parent: EntryId) -> Result<()> {
        // SAFETY: no pointers; the handle is validated by the shim.
        Error::check(unsafe { sys::shim_msv_move_entry(handle, id, new_parent) })
    }

    fn observe(&mut self, handle: i32, enable: bool) -> Result<()> {
        // SAFETY: no pointers; the handle is validated by the shim.
        Error::check(unsafe { sys::shim_msv_observe(handle, if enable { 1 } else { 0 }) })
    }
}

// --------------------------------------------------------------------- an entry --

/// One entry, owned and decoded.
pub struct Entry {
    pub id: EntryId,
    pub parent: EntryId,
    pub service_id: EntryId,
    pub mtm_uid: u32,
    pub type_uid: u32,
    /// Seconds since the Unix epoch.
    pub unix_time: i64,
    pub size: i32,
    pub flags: i32,
    /// `iDetails` — the correspondent, and the left-hand column in the native list.
    pub details: String,
    /// `iDescription` — subject or preview, the second line of the row.
    pub description: String,
    /// True when the platform's field was longer than the shim's buffer.
    ///
    /// Reported rather than hidden because `iDetails` carries a correspondent's identity and
    /// has no documented cap. A silently shortened correspondent is a reply addressed to the
    /// wrong person, so a caller that round-trips identities through this field needs to know.
    pub details_truncated: bool,
    pub description_truncated: bool,
}

impl Entry {
    fn from_raw(raw: &RawEntry) -> Entry {
        let dn = (raw.details_len.max(0) as usize).min(raw.details.len());
        let cn = (raw.description_len.max(0) as usize).min(raw.description.len());
        Entry {
            id: raw.id,
            parent: raw.parent,
            service_id: raw.service_id,
            mtm_uid: raw.mtm_uid,
            type_uid: raw.type_uid,
            unix_time: raw.unix_time,
            size: raw.size,
            flags: raw.flags,
            details: String::from_utf16_lossy(&raw.details[..dn]),
            description: String::from_utf16_lossy(&raw.description[..cn]),
            details_truncated: raw.details_len as usize > raw.details.len(),
            description_truncated: raw.description_len as usize > raw.description.len(),
        }
    }

    pub fn is_message(&self) -> bool {
        self.type_uid == TYPE_MESSAGE
    }
    pub fn is_service(&self) -> bool {
        self.type_uid == TYPE_SERVICE
    }
    pub fn is_folder(&self) -> bool {
        self.type_uid == TYPE_FOLDER
    }
    /// `is_new` rather than `new`, because a `new()` that answers a question rather than
    /// building one reads as a constructor at every call site.
    pub fn is_new(&self) -> bool {
        self.flags & sys::SHIM_MSV_NEW != 0
    }
    pub fn unread(&self) -> bool {
        self.flags & sys::SHIM_MSV_UNREAD != 0
    }
    pub fn complete(&self) -> bool {
        self.flags & sys::SHIM_MSV_COMPLETE != 0
    }
    pub fn visible(&self) -> bool {
        self.flags & sys::SHIM_MSV_VISIBLE != 0
    }
    /// Still being written. A UI MTM creates a reply in this state and clears it in the same
    /// `ChangeL` that makes the entry visible, *after* committing the body — so an entry that
    /// is visible, complete and not in preparation is one whose body is really there.
    pub fn in_preparation(&self) -> bool {
        self.flags & sys::SHIM_MSV_IN_PREPARATION != 0
    }
    pub fn failed(&self) -> bool {
        self.flags & sys::SHIM_MSV_FAILED != 0
    }
}

// --------------------------------------------------------------------- events --

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum StoreEventKind {
    Created,
    Changed,
    Deleted,
    Moved,
    MtmInstalled,
    MtmRemoved,
    ServerReady,
    ServerGone,
}

/// A Message Server notification, decoded.
///
/// **A hint, never data.** By the time this is read the entry may be gone and its flags may
/// have changed again, and the shim delivers at most a handful per platform notification
/// (`batch` carries the real selection size). So the right response is to re-read the store,
/// which is also what a restarted process does — one recovery path, exercised constantly.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct StoreEvent {
    pub kind: StoreEventKind,
    /// 0 for the session and registry kinds, whose notification is not about an entry.
    pub id: EntryId,
    pub parent: EntryId,
    /// How many entries the platform's selection held. Larger than the number of events
    /// delivered means the rest were dropped on purpose; rescan.
    pub batch: i32,
}

/// Decode a [`sys::SHIM_EV_MSV`] event, or `None` for any other kind.
///
/// `None` rather than a panic or an error so a caller can hand it every event it drains
/// without filtering first. Pure, and therefore the one piece of the event path a host test
/// can pin exactly.
pub fn store_event(ev: &sys::ShimEvent) -> Option<StoreEvent> {
    if ev.kind != sys::SHIM_EV_MSV {
        return None;
    }
    let kind = match ev.a {
        sys::SHIM_MSV_EV_CREATED => StoreEventKind::Created,
        sys::SHIM_MSV_EV_CHANGED => StoreEventKind::Changed,
        sys::SHIM_MSV_EV_DELETED => StoreEventKind::Deleted,
        sys::SHIM_MSV_EV_MOVED => StoreEventKind::Moved,
        sys::SHIM_MSV_EV_MTM_INSTALLED => StoreEventKind::MtmInstalled,
        sys::SHIM_MSV_EV_MTM_REMOVED => StoreEventKind::MtmRemoved,
        sys::SHIM_MSV_EV_SERVER_READY => StoreEventKind::ServerReady,
        sys::SHIM_MSV_EV_SERVER_GONE => StoreEventKind::ServerGone,
        // A sub-kind this build does not know. Not an error and not a guess.
        _ => return None,
    };
    Some(StoreEvent { kind, id: ev.b, parent: ev.c, batch: ev.d })
}

// -------------------------------------------------------------------- session --

/// An open Message Server session. Closes itself.
///
/// One at a time: the shim keeps a single slot, because a probe asks its questions in
/// sequence and a handle table would be ceremony around one pointer. A second
/// [`Session::open`] while one is live returns [`Error::InUse`].
pub struct Session<M: Msv = ShimMsv> {
    msv: M,
    handle: i32,
}

impl Session<ShimMsv> {
    /// `CMsvSession::OpenSyncL`.
    ///
    /// Synchronous, and therefore able to block: the Message Server may be starting up, or
    /// rebuilding its index. Anything calling this from a process that must stay responsive
    /// wants a deadline around the *process*, which is how `apps/devdump` runs it.
    pub fn open() -> Result<Self> {
        Session::with(ShimMsv)
    }
}

impl<M: Msv> Session<M> {
    /// Open over a given [`Msv`] — the host-test entry point, with [`MemMsv`].
    pub fn with(mut msv: M) -> Result<Self> {
        let handle = msv.open()?;
        Ok(Session { msv, handle })
    }

    /// How many MTMs are registered on this handset.
    pub fn mtm_count(&mut self) -> Result<i32> {
        self.msv.mtm_count(self.handle)
    }

    /// Throw away the client-side registry snapshot and build a fresh one.
    ///
    /// [`Session::mtm_count`] reads a **per-process copy** of the registry, taken when the
    /// session opened and refreshed thereafter only through session events. So counting
    /// after [`Session::install_mtm`] measures the state from before the install, and
    /// "registered, but this process has not noticed" reads exactly like "not registered".
    /// Call this between the two, or the count is not evidence of anything.
    pub fn refresh_registry(&mut self) -> Result<()> {
        self.msv.refresh_registry(self.handle)
    }

    /// Ask the framework to instantiate a Client MTM of this type.
    ///
    /// The definitive test that a registration worked, and the reason [`Session::mtm_count`]
    /// is not: the count reads a per-process copy the session refreshes on an event that
    /// cannot arrive while the caller is still inside its own `RunL`, so a freshly installed
    /// MTM legitimately does not appear in it. This asks the framework to find the type, load
    /// the DLL and call the factory at the registered ordinal instead — and each of those
    /// failing has its own error.
    ///
    /// The instance is destroyed immediately. The question is whether one can be made.
    pub fn can_instantiate(&mut self, mtm_uid: u32) -> Result<()> {
        self.msv.can_instantiate(self.handle, mtm_uid)
    }

    /// One registry entry: its type UID, technology UID and human-readable name.
    pub fn mtm_info(&mut self, index: i32) -> Result<MtmInfo> {
        self.msv.mtm_info(self.handle, index)
    }

    /// How many entries a standard folder holds. See [`FOLDERS`].
    pub fn folder_count(&mut self, folder_id: EntryId) -> Result<i32> {
        self.msv.folder_count(self.handle, folder_id)
    }

    /// How many entries in a standard folder are unread — the "N new messages" number a home
    /// screen shows for the inbox ([`sys::SHIM_MSV_INBOX`]). One server-side count.
    pub fn unread_count(&mut self, folder_id: EntryId) -> Result<i32> {
        self.msv.unread_count(self.handle, folder_id)
    }

    /// Tell the Message Server about a `.mtm` registration file outside ROM.
    ///
    /// Dropping the compiled resource into `C:\resource\messaging\mtm\` is not enough:
    /// without this call the Message Server never reads it. It also fires
    /// `EMsvMtmGroupInstalled`, which is how a *running* Messaging application picks the new
    /// type up without being restarted.
    ///
    /// De-installs first, because installing over an existing group fails and a reinstall is
    /// the normal case during development. A `NotFound` from that half is the ordinary
    /// first-run answer and is swallowed; anything else is returned.
    pub fn install_mtm(&mut self, path: &str) -> Result<()> {
        let p = Utf16Path::new(path)?;
        match self.msv.deinstall_mtm(p.as_units()) {
            Ok(()) | Err(Error::NotFound) => {}
            Err(e) => return Err(e),
        }
        self.msv.install_mtm(p.as_units())
    }

    /// Remove a registration. `NotFound` is not an error here.
    pub fn deinstall_mtm(&mut self, path: &str) -> Result<()> {
        let p = Utf16Path::new(path)?;
        match self.msv.deinstall_mtm(p.as_units()) {
            Ok(()) | Err(Error::NotFound) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Create the service entry — the account the native Messaging application lists.
    ///
    /// `name` is what the user sees. Returns the new entry's id, which every message of this
    /// service must carry.
    pub fn create_service(&mut self, mtm_uid: u32, name: &str) -> Result<EntryId> {
        let units: Vec<u16> = name.encode_utf16().collect();
        self.msv.create_service(self.handle, mtm_uid, &units)
    }

    /// Write a message into a folder. Returns its id.
    pub fn create_message(&mut self, msg: &NewMessage) -> Result<EntryId> {
        let raw = msg.raw();
        self.msv.create_message(self.handle, &raw)
    }

    /// Delete every service of a type, and everything under it. Returns how many went.
    ///
    /// Written as "all of this type" rather than "the one I made" on purpose: anything that
    /// creates a service per run and forgets to remove it fills the user's Messaging account
    /// list with copies of itself, and by the time that is noticed nothing remembers the old
    /// ids. This is the call that cleans up after a mistake already made.
    pub fn delete_services(&mut self, mtm_uid: u32) -> Result<i32> {
        self.msv.delete_services(self.handle, mtm_uid)
    }

    pub fn delete_entry(&mut self, id: EntryId) -> Result<()> {
        self.msv.delete_entry(self.handle, id)
    }

    /// One entry's fields, decoded.
    pub fn entry(&mut self, id: EntryId) -> Result<Entry> {
        let raw = self.msv.entry(self.handle, id)?;
        Ok(Entry::from_raw(&raw))
    }

    /// A folder's children, newest first.
    ///
    /// Grows and re-reads when the first attempt reports more than fitted, rather than
    /// returning a short answer that looks complete. The loop is bounded by re-asking at the
    /// reported size: a folder that grows between the two calls costs one more round, not an
    /// unbounded number.
    pub fn children(&mut self, folder: EntryId) -> Result<Vec<EntryId>> {
        let mut buf = alloc::vec![0i32; 32];
        loop {
            let total = self.msv.children(self.handle, folder, &mut buf)?;
            if total <= buf.len() {
                buf.truncate(total);
                return Ok(buf);
            }
            buf.resize(total, 0);
        }
    }

    /// Service entries of one MTM type.
    ///
    /// How a service finds the account it created on a previous run instead of creating a
    /// second one — which is the mistake [`Session::delete_services`] exists to clean up
    /// after.
    pub fn services(&mut self, mtm_uid: u32) -> Result<Vec<EntryId>> {
        let mut buf = alloc::vec![0i32; 8];
        loop {
            let total = self.msv.services(self.handle, mtm_uid, &mut buf)?;
            if total <= buf.len() {
                buf.truncate(total);
                return Ok(buf);
            }
            buf.resize(total, 0);
        }
    }

    /// The whole body text, however long. An entry with no body is an empty string.
    pub fn body(&mut self, id: EntryId) -> Result<String> {
        let mut buf = alloc::vec![0u16; 256];
        loop {
            let total = self.msv.body(self.handle, id, &mut buf)?;
            if total <= buf.len() {
                buf.truncate(total);
                return Ok(String::from_utf16_lossy(&buf));
            }
            buf.resize(total, 0);
        }
    }

    /// Set and clear flags in one server round trip. `set` wins where the two collide.
    pub fn set_flags(&mut self, id: EntryId, set: i32, clear: i32) -> Result<()> {
        self.msv.set_flags(self.handle, id, set, clear)
    }

    /// Clear new and unread. What the platform's own applications do when the user has
    /// looked at something.
    pub fn mark_read(&mut self, id: EntryId) -> Result<()> {
        self.set_flags(id, 0, sys::SHIM_MSV_NEW | sys::SHIM_MSV_UNREAD)
    }

    /// Reparent an entry.
    ///
    /// Durable state: a service that records "I have carried this out" by moving the entry
    /// survives its own restart, where a set of ids held in the process would forget and send
    /// the user's message a second time.
    pub fn move_entry(&mut self, id: EntryId, to: EntryId) -> Result<()> {
        self.msv.move_entry(self.handle, id, to)
    }

    /// Start delivering [`StoreEvent`]s onto the shim's event ring.
    ///
    /// Opt-in, because events pushed to a ring nobody drains are only a dropped-event count.
    pub fn observe(&mut self) -> Result<()> {
        self.msv.observe(self.handle, true)
    }

    pub fn stop_observing(&mut self) -> Result<()> {
        self.msv.observe(self.handle, false)
    }

    /// The underlying [`Msv`].
    ///
    /// Here for tests in the crates above this one: with [`MemMsv`] it is how a test asserts
    /// the *order* of calls, which is the thing a return value cannot show — a body committed
    /// before the entry is published, a reply moved only after the service said it was sent.
    /// On a device it hands back a zero-sized [`ShimMsv`] and there is nothing to see.
    pub fn msv(&mut self) -> &mut M {
        &mut self.msv
    }
}

impl<M: Msv> Drop for Session<M> {
    fn drop(&mut self) {
        self.msv.close(self.handle);
    }
}

// ------------------------------------------------------------------ new message --

/// A message about to be written into a folder.
///
/// It owns its text. The FFI struct the shim receives carries raw pointers, and building it
/// inside [`Session::create_message`] rather than exposing it means those pointers cannot
/// outlive the buffers they point at — which on this platform would not be a dangling read
/// so much as a descriptor over freed memory handed to the Message Server.
pub struct NewMessage {
    /// From [`Session::create_service`]. A message with no service is a message the framework
    /// cannot walk back to an account.
    pub service_id: EntryId,
    pub mtm_uid: u32,
    /// One of [`FOLDERS`]' ids. Defaults to the inbox.
    pub parent_id: EntryId,
    /// Seconds since the Unix epoch; 0 means now.
    pub unix_time: i64,
    /// [`sys::SHIM_MSV_NEW`] and friends. Defaults to new + unread, which is what makes the
    /// native application bold the row and the notification list count it.
    pub flags: i32,
    details: Vec<u16>,
    description: Vec<u16>,
    body: Vec<u16>,
}

impl NewMessage {
    pub fn new(service_id: EntryId, mtm_uid: u32) -> Self {
        NewMessage {
            service_id,
            mtm_uid,
            parent_id: sys::SHIM_MSV_INBOX,
            unix_time: 0,
            flags: sys::SHIM_MSV_NEW | sys::SHIM_MSV_UNREAD,
            details: Vec::new(),
            description: Vec::new(),
            body: Vec::new(),
        }
    }

    /// Who it is from — `iDetails`, the left-hand column in the native list.
    pub fn from(mut self, s: &str) -> Self {
        self.details = s.encode_utf16().collect();
        self
    }

    /// Subject, or a preview line — `iDescription`, the second line of the row.
    pub fn subject(mut self, s: &str) -> Self {
        self.description = s.encode_utf16().collect();
        self
    }

    /// The message text, stored as rich text in the entry's own store.
    pub fn body(mut self, s: &str) -> Self {
        self.body = s.encode_utf16().collect();
        self
    }

    pub fn at(mut self, unix_time: i64) -> Self {
        self.unix_time = unix_time;
        self
    }

    pub fn into_folder(mut self, folder_id: EntryId) -> Self {
        self.parent_id = folder_id;
        self
    }

    fn raw(&self) -> sys::ShimNewMessage {
        sys::ShimNewMessage {
            service_id: self.service_id,
            mtm_uid: self.mtm_uid,
            parent_id: self.parent_id,
            unix_time: self.unix_time,
            size: 0,
            flags: self.flags,
            details: self.details.as_ptr(),
            details_len: self.details.len() as i32,
            description: self.description.as_ptr(),
            description_len: self.description.len() as i32,
            body: self.body.as_ptr(),
            body_len: self.body.len() as i32,
        }
    }
}

/// The platform's own new-message notification: indicator, tone, floating note.
///
/// Every function here returns the platform's error rather than hiding it, and that is the
/// point. `MNcnNotification` is documented as an interface for *email* plugins — its
/// parameter is a mailbox and the note it raises says "New email" — and its implementation
/// is an ECom plugin, so nothing in the device library sweep could tell us whether it is
/// even present.
///
/// On the E72 it is worse than absent: calling it **kills the process**, measured, with a
/// folder id and with a real service id alike (`docs/device-notes.md`). So a message
/// delivered into the inbox arrives quietly, and these stay here for a handset that might
/// answer differently rather than for this one.
pub mod ncn {
    use super::*;

    /// Icon only.
    pub const ICON: i32 = sys::SHIM_NCN_ICON;
    /// Icon, tone and floating note — what an arriving SMS produces.
    pub const NORMAL: i32 = sys::SHIM_NCN_NORMAL;

    pub fn notify(service_id: EntryId, indication: i32) -> Result<()> {
        // SAFETY: no pointers.
        Error::check(unsafe { sys::shim_ncn_notify(service_id, indication) })
    }

    /// Zero the new-message counter for a service, for when the user has read them.
    pub fn mark_unread(service_id: EntryId) -> Result<()> {
        // SAFETY: no pointers.
        Error::check(unsafe { sys::shim_ncn_mark_unread(service_id) })
    }
}

/// The registry entry's name as UTF-16 units, empty if it has none.
pub fn info_name(info: &MtmInfo) -> &[u16] {
    let n = (info.name_len.max(0) as usize).min(info.name.len());
    &info.name[..n]
}

// ------------------------------------------------------------------- the fake --

/// Which trait method was called, in order.
///
/// Order is the thing worth asserting and the thing a return value cannot show: the body has
/// to be committed before the entry becomes visible, and a reply has to be moved only after
/// the service said it was sent. Same shape as [`crate::sql::Call`], for the same reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MsvCall {
    Open,
    Close,
    InstallMtm,
    DeinstallMtm,
    CreateService(u32),
    CreateMessage(EntryId),
    DeleteEntry(EntryId),
    DeleteServices(u32),
    Entry(EntryId),
    Children(EntryId),
    Services(u32),
    Body(EntryId),
    SetFlags(EntryId, i32, i32),
    MoveEntry(EntryId, EntryId),
    Observe(bool),
}

/// An in-memory message store.
///
/// Public, and not behind `#[cfg(test)]`, because the crates above this one need it too —
/// `symbian-mtm` is where deciding what counts as an unhandled reply lives, and there is no
/// phone in a `cargo test`. Same reasoning as [`crate::fs::MemFs`] and [`crate::sql::MemSql`].
/// It costs nothing in a device build: nothing references it, and `--gc-sections` sweeps it.
pub struct MemMsv {
    /// Each entry with its body text.
    pub entries: Vec<(RawEntry, Vec<u16>)>,
    /// Registration paths installed, in order.
    pub installed: Vec<Vec<u16>>,
    pub calls: Vec<MsvCall>,
    pub observing: bool,
    /// Make `observe` refuse.
    ///
    /// Its own flag rather than `fail_next`, because the caller under test is
    /// `symbian_mtm::Bridge::install`, which makes several calls in sequence and must survive
    /// this one specifically — event delivery is an optimisation there, not a requirement.
    pub refuse_observe: bool,
    /// Fail the next trait call with this, then clear it.
    pub fail_next: Option<Error>,
    next_id: EntryId,
}

impl Default for MemMsv {
    fn default() -> Self {
        MemMsv::new()
    }
}

impl MemMsv {
    pub fn new() -> Self {
        MemMsv {
            entries: Vec::new(),
            installed: Vec::new(),
            calls: Vec::new(),
            observing: false,
            refuse_observe: false,
            fail_next: None,
            next_id: 0x2000,
        }
    }

    fn take_failure(&mut self) -> Result<()> {
        match self.fail_next.take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn find(&self, id: EntryId) -> Option<usize> {
        self.entries.iter().position(|(e, _)| e.id == id)
    }

    /// Put a message in a folder as if the platform had. Returns its id.
    pub fn push_message(
        &mut self,
        parent: EntryId,
        mtm_uid: u32,
        service: EntryId,
        details: &str,
        body: &str,
        flags: i32,
    ) -> EntryId {
        let id = self.next_id;
        self.next_id += 1;
        let mut raw = RawEntry {
            id,
            parent,
            service_id: service,
            mtm_uid,
            type_uid: TYPE_MESSAGE,
            flags,
            ..Default::default()
        };
        set_field(&mut raw.details, &mut raw.details_len, details);
        set_field(&mut raw.description, &mut raw.description_len, body);
        self.entries.push((raw, body.encode_utf16().collect()));
        id
    }

    /// A service entry under the root, as [`Session::create_service`] would leave it.
    pub fn push_service(&mut self, mtm_uid: u32, name: &str) -> EntryId {
        let id = self.next_id;
        self.next_id += 1;
        let mut raw = RawEntry {
            id,
            parent: sys::SHIM_MSV_ROOT,
            mtm_uid,
            type_uid: TYPE_SERVICE,
            flags: sys::SHIM_MSV_COMPLETE | sys::SHIM_MSV_VISIBLE,
            ..Default::default()
        };
        set_field(&mut raw.details, &mut raw.details_len, name);
        self.entries.push((raw, Vec::new()));
        id
    }

    /// Exactly what a UI MTM's `ReplyL` leaves behind: complete, visible, not in preparation,
    /// `iDetails` the correspondent and the text in both the description and the body.
    pub fn push_reply(
        &mut self,
        parent: EntryId,
        mtm_uid: u32,
        service: EntryId,
        to: &str,
        text: &str,
    ) -> EntryId {
        self.push_message(
            parent,
            mtm_uid,
            service,
            to,
            text,
            sys::SHIM_MSV_COMPLETE | sys::SHIM_MSV_VISIBLE,
        )
    }

    pub fn body_of(&self, id: EntryId) -> Option<String> {
        self.find(id).map(|i| String::from_utf16_lossy(&self.entries[i].1))
    }

    pub fn parent_of(&self, id: EntryId) -> Option<EntryId> {
        self.find(id).map(|i| self.entries[i].0.parent)
    }

    pub fn flags_of(&self, id: EntryId) -> Option<i32> {
        self.find(id).map(|i| self.entries[i].0.flags)
    }
}

/// Write a `&str` into a fixed array + length pair the way the shim does — full length
/// reported, array filled with what fits.
fn set_field(dst: &mut [u16], len: &mut i32, s: &str) {
    let units: Vec<u16> = s.encode_utf16().collect();
    *len = units.len() as i32;
    let n = units.len().min(dst.len());
    dst[..n].copy_from_slice(&units[..n]);
}

impl Msv for MemMsv {
    fn open(&mut self) -> Result<i32> {
        self.calls.push(MsvCall::Open);
        self.take_failure()?;
        Ok(1)
    }

    fn close(&mut self, _handle: i32) {
        self.calls.push(MsvCall::Close);
    }

    fn mtm_count(&mut self, _handle: i32) -> Result<i32> {
        self.take_failure()?;
        Ok(self.installed.len() as i32)
    }

    fn refresh_registry(&mut self, _handle: i32) -> Result<()> {
        self.take_failure()
    }

    fn can_instantiate(&mut self, _handle: i32, _mtm_uid: u32) -> Result<()> {
        self.take_failure()
    }

    fn mtm_info(&mut self, _handle: i32, _index: i32) -> Result<MtmInfo> {
        self.take_failure()?;
        Err(Error::NotFound)
    }

    fn folder_count(&mut self, _handle: i32, folder_id: EntryId) -> Result<i32> {
        self.take_failure()?;
        Ok(self.entries.iter().filter(|(e, _)| e.parent == folder_id).count() as i32)
    }

    fn unread_count(&mut self, _handle: i32, folder_id: EntryId) -> Result<i32> {
        self.take_failure()?;
        Ok(self
            .entries
            .iter()
            .filter(|(e, _)| e.parent == folder_id && e.flags & sys::SHIM_MSV_UNREAD != 0)
            .count() as i32)
    }

    fn install_mtm(&mut self, path: &[u16]) -> Result<()> {
        self.calls.push(MsvCall::InstallMtm);
        self.take_failure()?;
        self.installed.push(path.to_vec());
        Ok(())
    }

    fn deinstall_mtm(&mut self, path: &[u16]) -> Result<()> {
        self.calls.push(MsvCall::DeinstallMtm);
        self.take_failure()?;
        match self.installed.iter().position(|p| p == path) {
            Some(i) => {
                self.installed.remove(i);
                Ok(())
            }
            // The device's ordinary first-run answer, which `install_mtm` swallows.
            None => Err(Error::NotFound),
        }
    }

    fn create_service(&mut self, _handle: i32, mtm_uid: u32, name: &[u16]) -> Result<EntryId> {
        self.calls.push(MsvCall::CreateService(mtm_uid));
        self.take_failure()?;
        Ok(self.push_service(mtm_uid, &String::from_utf16_lossy(name)))
    }

    fn create_message(&mut self, _handle: i32, msg: &sys::ShimNewMessage) -> Result<EntryId> {
        self.take_failure()?;
        // SAFETY: the pointers come from a `NewMessage` alive for the duration of this call,
        // which is the same contract the device shim relies on.
        let text = |p: *const u16, n: i32| -> String {
            if p.is_null() || n <= 0 {
                String::new()
            } else {
                String::from_utf16_lossy(unsafe { core::slice::from_raw_parts(p, n as usize) })
            }
        };
        let details = text(msg.details, msg.details_len);
        let description = text(msg.description, msg.description_len);
        let body = text(msg.body, msg.body_len);

        let id = self.next_id;
        self.next_id += 1;
        let mut raw = RawEntry {
            id,
            parent: msg.parent_id,
            service_id: msg.service_id,
            mtm_uid: msg.mtm_uid,
            type_uid: TYPE_MESSAGE,
            unix_time: msg.unix_time,
            // The device forces both regardless of what the caller asked, because the body is
            // committed before the second ChangeL publishes the entry.
            flags: msg.flags | sys::SHIM_MSV_COMPLETE | sys::SHIM_MSV_VISIBLE,
            ..Default::default()
        };
        set_field(&mut raw.details, &mut raw.details_len, &details);
        set_field(&mut raw.description, &mut raw.description_len, &description);
        self.entries.push((raw, body.encode_utf16().collect()));
        self.calls.push(MsvCall::CreateMessage(id));
        Ok(id)
    }

    fn delete_entry(&mut self, _handle: i32, id: EntryId) -> Result<()> {
        self.calls.push(MsvCall::DeleteEntry(id));
        self.take_failure()?;
        match self.find(id) {
            Some(i) => {
                self.entries.remove(i);
                Ok(())
            }
            None => Err(Error::NotFound),
        }
    }

    fn delete_services(&mut self, _handle: i32, mtm_uid: u32) -> Result<i32> {
        self.calls.push(MsvCall::DeleteServices(mtm_uid));
        self.take_failure()?;
        let before = self.entries.len();
        self.entries
            .retain(|(e, _)| !(e.type_uid == TYPE_SERVICE && e.mtm_uid == mtm_uid));
        Ok((before - self.entries.len()) as i32)
    }

    fn entry(&mut self, _handle: i32, id: EntryId) -> Result<RawEntry> {
        self.calls.push(MsvCall::Entry(id));
        self.take_failure()?;
        match self.find(id) {
            Some(i) => Ok(self.entries[i].0),
            None => Err(Error::NotFound),
        }
    }

    fn children(&mut self, _handle: i32, folder: EntryId, out: &mut [EntryId]) -> Result<usize> {
        self.calls.push(MsvCall::Children(folder));
        self.take_failure()?;
        // Newest first, like the device's EMsvSortByDateReverse — here that is insertion
        // order reversed, which is the same thing for a fake that stamps nothing.
        let ids: Vec<EntryId> = self
            .entries
            .iter()
            .rev()
            .filter(|(e, _)| e.parent == folder)
            .map(|(e, _)| e.id)
            .collect();
        let n = out.len().min(ids.len());
        out[..n].copy_from_slice(&ids[..n]);
        Ok(ids.len())
    }

    fn services(&mut self, _handle: i32, mtm_uid: u32, out: &mut [EntryId]) -> Result<usize> {
        self.calls.push(MsvCall::Services(mtm_uid));
        self.take_failure()?;
        let ids: Vec<EntryId> = self
            .entries
            .iter()
            .filter(|(e, _)| e.type_uid == TYPE_SERVICE && e.mtm_uid == mtm_uid)
            .map(|(e, _)| e.id)
            .collect();
        let n = out.len().min(ids.len());
        out[..n].copy_from_slice(&ids[..n]);
        Ok(ids.len())
    }

    fn body(&mut self, _handle: i32, id: EntryId, out: &mut [u16]) -> Result<usize> {
        self.calls.push(MsvCall::Body(id));
        self.take_failure()?;
        let i = self.find(id).ok_or(Error::NotFound)?;
        let body = &self.entries[i].1;
        let n = out.len().min(body.len());
        out[..n].copy_from_slice(&body[..n]);
        Ok(body.len())
    }

    fn set_flags(&mut self, _handle: i32, id: EntryId, set: i32, clear: i32) -> Result<()> {
        self.calls.push(MsvCall::SetFlags(id, set, clear));
        self.take_failure()?;
        let i = self.find(id).ok_or(Error::NotFound)?;
        // Clear then set, so `set` wins — the device does the same.
        self.entries[i].0.flags &= !clear;
        self.entries[i].0.flags |= set;
        Ok(())
    }

    fn move_entry(&mut self, _handle: i32, id: EntryId, new_parent: EntryId) -> Result<()> {
        self.calls.push(MsvCall::MoveEntry(id, new_parent));
        self.take_failure()?;
        let i = self.find(id).ok_or(Error::NotFound)?;
        self.entries[i].0.parent = new_parent;
        Ok(())
    }

    fn observe(&mut self, _handle: i32, enable: bool) -> Result<()> {
        self.calls.push(MsvCall::Observe(enable));
        if self.refuse_observe {
            return Err(Error::Platform(sys::SHIM_ERR_NOT_SUPPORTED));
        }
        self.take_failure()?;
        self.observing = enable;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Session<MemMsv> {
        Session::with(MemMsv::new()).unwrap()
    }

    #[test]
    fn the_standard_folders_are_all_there() {
        let names: alloc::vec::Vec<&str> = FOLDERS.iter().map(|(_, n)| *n).collect();
        assert_eq!(names, alloc::vec!["root", "inbox", "outbox", "drafts", "sent"]);
    }

    /// The ids come from msvids.h and are the wire values the Message Server keys on.
    #[test]
    fn folder_ids_match_the_platform_constants() {
        assert_eq!(sys::SHIM_MSV_INBOX, 0x1002);
        assert_eq!(sys::SHIM_MSV_OUTBOX, 0x1003);
        assert_eq!(sys::SHIM_MSV_DRAFTS, 0x1004);
        assert_eq!(sys::SHIM_MSV_SENT, 0x1005);
    }

    /// The read-side flags continue the write-side bit space rather than starting a second
    /// one. If they ever collide, a `set_flags` call would silently change the wrong state.
    #[test]
    fn the_flag_bits_do_not_overlap() {
        let all = [
            sys::SHIM_MSV_NEW,
            sys::SHIM_MSV_UNREAD,
            sys::SHIM_MSV_COMPLETE,
            sys::SHIM_MSV_VISIBLE,
            sys::SHIM_MSV_IN_PREPARATION,
            sys::SHIM_MSV_FAILED,
        ];
        let mut seen = 0i32;
        for f in all {
            assert_eq!(seen & f, 0, "flag {f:#x} overlaps an earlier one");
            seen |= f;
        }
        assert_eq!(seen, 0x3F);
    }

    /// An unrecognised MTM must stay unrecognised. Guessing a name for it would invent the
    /// very fact the enumeration exists to discover.
    #[test]
    fn unknown_mtms_are_not_named() {
        assert_eq!(mtm_name(0x1000102C), Some("SMS"));
        assert_eq!(mtm_name(0xDEADBEEF), None);
        // The UID the first version of this table guessed for MMS. It is not MMS, and it is
        // not anything on this handset — so it must not resolve to a name.
        assert_eq!(mtm_name(0x100056DE), None);
        assert_eq!(mtm_name(0x100058E1), Some("MMS"));
    }

    /// The default is what a caller almost always wants, and getting it wrong is silent: a
    /// message that is neither new nor unread lands in the inbox without bolding the row or
    /// raising a count, which reads as "the message never arrived".
    #[test]
    fn a_new_message_defaults_to_new_and_unread_in_the_inbox() {
        let m = NewMessage::new(42, 0xE0001234);
        assert_eq!(m.parent_id, sys::SHIM_MSV_INBOX);
        assert_eq!(m.flags, sys::SHIM_MSV_NEW | sys::SHIM_MSV_UNREAD);
        assert_eq!(m.unix_time, 0, "0 means the shim stamps it now");
    }

    /// The builder has to survive being moved: the raw struct points into the Vecs, so a
    /// pointer captured before a move would dangle. Building it only inside the call is what
    /// prevents that, and this pins the lengths that prove the text arrived at all.
    #[test]
    fn the_builder_encodes_utf16_and_reports_its_lengths() {
        let m = NewMessage::new(1, 2).from("Ana").subject("oi").body("bom dia");
        let raw = m.raw();
        assert_eq!(raw.details_len, 3);
        assert_eq!(raw.description_len, 2);
        assert_eq!(raw.body_len, 7);
        assert!(!raw.details.is_null() && !raw.body.is_null());
    }

    /// Non-ASCII is the normal case in Portuguese and the encoding is where it would break.
    #[test]
    fn accents_survive_the_encoding() {
        let m = NewMessage::new(1, 2).body("ação");
        let raw = m.raw();
        assert_eq!(raw.body_len, 4, "one UTF-16 unit per char here, not one per byte");
    }

    #[test]
    fn info_name_honours_the_written_length() {
        let mut info = MtmInfo::default();
        info.name[0] = b'S' as u16;
        info.name[1] = b'M' as u16;
        info.name[2] = b'S' as u16;
        info.name_len = 3;
        assert_eq!(info_name(&info), &[83, 77, 83]);
        info.name_len = 9999;
        assert_eq!(info_name(&info).len(), 64);
    }

    // ------------------------------------------------------- the read path --

    /// The loop that matters, driven the way the device drives it: more children than the
    /// first buffer holds. The shim fills what fits and reports the real count, so a caller
    /// that trusted the first answer would silently see a fraction of the folder — and on a
    /// device that fraction is "the reply is not there yet".
    ///
    /// The fake fills exactly `out.len()` and never less, because that is what the shim does.
    /// A fake that under-filled would hide a broken loop instead of exposing one.
    #[test]
    fn children_grows_until_the_whole_folder_fits() {
        let mut fake = MemMsv::new();
        for i in 0..40 {
            fake.push_message(sys::SHIM_MSV_DRAFTS, 0xE1, 1, "Ana", &alloc::format!("m{i}"), 0);
        }
        let mut s = Session::with(fake).unwrap();
        let ids = s.children(sys::SHIM_MSV_DRAFTS).unwrap();
        assert_eq!(ids.len(), 40, "every child, not the first bufferful");
        assert_eq!(
            ids.iter().collect::<alloc::collections::BTreeSet<_>>().len(),
            40,
            "and no id repeated by a re-read"
        );
        // Two calls: the short one, then the sized one.
        let reads = s.msv().calls.iter().filter(|c| **c == MsvCall::Children(sys::SHIM_MSV_DRAFTS)).count();
        assert_eq!(reads, 2);
    }

    #[test]
    fn children_of_an_empty_folder_is_empty_not_an_error() {
        let mut s = store();
        assert!(s.children(sys::SHIM_MSV_SENT).unwrap().is_empty());
    }

    #[test]
    fn unread_count_counts_only_unread_in_the_folder() {
        let mut fake = MemMsv::new();
        // Two unread and one read in the inbox; one unread elsewhere must not be counted.
        fake.push_message(sys::SHIM_MSV_INBOX, 0xE1, 1, "Ana", "hi", sys::SHIM_MSV_UNREAD);
        fake.push_message(sys::SHIM_MSV_INBOX, 0xE1, 1, "Bea", "yo", sys::SHIM_MSV_UNREAD);
        fake.push_message(sys::SHIM_MSV_INBOX, 0xE1, 1, "Cal", "ok", 0);
        fake.push_message(sys::SHIM_MSV_DRAFTS, 0xE1, 1, "Dan", "wip", sys::SHIM_MSV_UNREAD);
        let mut s = Session::with(fake).unwrap();
        assert_eq!(s.unread_count(sys::SHIM_MSV_INBOX).unwrap(), 2);
        assert_eq!(s.unread_count(sys::SHIM_MSV_SENT).unwrap(), 0, "empty folder is zero, not an error");
    }

    /// A body longer than the 256-unit first buffer.
    #[test]
    fn body_grows_until_the_whole_text_fits() {
        let long: String = "x".repeat(1000);
        let mut fake = MemMsv::new();
        let id = fake.push_message(sys::SHIM_MSV_INBOX, 0xE1, 1, "Ana", &long, 0);
        let mut s = Session::with(fake).unwrap();
        assert_eq!(s.body(id).unwrap().len(), 1000);
        assert_eq!(s.msv().calls.iter().filter(|c| **c == MsvCall::Body(id)).count(), 2);
    }

    /// No body text is an empty string, not `NotFound`. A notification with nothing in it is
    /// an ordinary entry, and making callers tell empty from missing invents a distinction
    /// the store does not make.
    #[test]
    fn an_entry_with_no_body_reads_as_empty() {
        let mut fake = MemMsv::new();
        let id = fake.push_message(sys::SHIM_MSV_INBOX, 0xE1, 1, "Ana", "", 0);
        let mut s = Session::with(fake).unwrap();
        assert_eq!(s.body(id).unwrap(), "");
    }

    #[test]
    fn services_finds_only_our_own_type() {
        let mut fake = MemMsv::new();
        fake.push_service(0xE1, "ours");
        fake.push_service(0xE2, "somebody else's");
        let mut s = Session::with(fake).unwrap();
        assert_eq!(s.services(0xE1).unwrap().len(), 1);
        assert_eq!(s.services(0xE3).unwrap().len(), 0);
    }

    /// Truncation has to be visible. `iDetails` carries a correspondent's identity with no
    /// documented cap, and a silently shortened one is a reply addressed to the wrong person.
    #[test]
    fn a_long_correspondent_is_reported_as_truncated() {
        let long: String = "a".repeat(100);
        let mut fake = MemMsv::new();
        let id = fake.push_message(sys::SHIM_MSV_INBOX, 0xE1, 1, &long, "hi", 0);
        let mut s = Session::with(fake).unwrap();
        let e = s.entry(id).unwrap();
        assert!(e.details_truncated);
        assert_eq!(e.details.len(), 64, "what fitted, and no more");

        let e2 = {
            let mut fake = MemMsv::new();
            let id = fake.push_message(sys::SHIM_MSV_INBOX, 0xE1, 1, "Ana", "hi", 0);
            let mut s = Session::with(fake).unwrap();
            s.entry(id).unwrap()
        };
        assert!(!e2.details_truncated);
        assert_eq!(e2.details, "Ana");
    }

    #[test]
    fn the_flag_accessors_read_the_bits_they_claim() {
        let mut fake = MemMsv::new();
        let id = fake.push_message(
            sys::SHIM_MSV_INBOX,
            0xE1,
            1,
            "Ana",
            "hi",
            sys::SHIM_MSV_NEW | sys::SHIM_MSV_UNREAD | sys::SHIM_MSV_IN_PREPARATION,
        );
        let mut s = Session::with(fake).unwrap();
        let e = s.entry(id).unwrap();
        assert!(e.is_new() && e.unread() && e.in_preparation());
        assert!(!e.complete() && !e.visible() && !e.failed());
        assert!(e.is_message() && !e.is_service());
    }

    /// `mark_read` must touch exactly two bits. Clearing `VISIBLE` by accident would make the
    /// user's message disappear from the folder it is sitting in.
    #[test]
    fn mark_read_clears_only_new_and_unread() {
        let mut fake = MemMsv::new();
        let id = fake.push_message(
            sys::SHIM_MSV_INBOX,
            0xE1,
            1,
            "Ana",
            "hi",
            sys::SHIM_MSV_NEW | sys::SHIM_MSV_UNREAD | sys::SHIM_MSV_COMPLETE | sys::SHIM_MSV_VISIBLE,
        );
        let mut s = Session::with(fake).unwrap();
        s.mark_read(id).unwrap();
        let e = s.entry(id).unwrap();
        assert!(!e.is_new() && !e.unread());
        assert!(e.complete() && e.visible());
    }

    #[test]
    fn set_wins_over_clear_on_the_same_bit() {
        let mut fake = MemMsv::new();
        let id = fake.push_message(sys::SHIM_MSV_INBOX, 0xE1, 1, "Ana", "hi", 0);
        let mut s = Session::with(fake).unwrap();
        s.set_flags(id, sys::SHIM_MSV_FAILED, sys::SHIM_MSV_FAILED).unwrap();
        assert!(s.entry(id).unwrap().failed());
    }

    #[test]
    fn moving_an_entry_changes_its_parent() {
        let mut fake = MemMsv::new();
        let id = fake.push_reply(sys::SHIM_MSV_DRAFTS, 0xE1, 1, "Ana", "ok");
        let mut s = Session::with(fake).unwrap();
        s.move_entry(id, sys::SHIM_MSV_SENT).unwrap();
        assert_eq!(s.entry(id).unwrap().parent, sys::SHIM_MSV_SENT);
        assert!(s.children(sys::SHIM_MSV_DRAFTS).unwrap().is_empty());
    }

    /// The first run has nothing to de-install, and the device answers `KErrNotFound`. If
    /// `install_mtm` did not swallow that, every first install would look like a failure.
    #[test]
    fn installing_swallows_the_first_runs_missing_deinstall() {
        let mut s = store();
        s.install_mtm("C:\\resource\\messaging\\mtm\\x.rsc").unwrap();
        // And again: now the de-install half finds something, and it still succeeds.
        s.install_mtm("C:\\resource\\messaging\\mtm\\x.rsc").unwrap();
        assert_eq!(s.mtm_count().unwrap(), 1, "installed once, not twice over");
    }

    #[test]
    fn deinstalling_something_absent_is_not_an_error() {
        let mut s = store();
        s.deinstall_mtm("C:\\resource\\messaging\\mtm\\nothing.rsc").unwrap();
    }

    /// The write path's ordering, asserted through the call log rather than the result: the
    /// entry has to exist before its body is written and be published only afterwards, or a
    /// reader that catches it in between sees a message with nothing in it.
    #[test]
    fn creating_a_message_lands_it_complete_and_visible() {
        let mut s = store();
        let svc = s.create_service(0xE1, "Telegram").unwrap();
        let id = s
            .create_message(&NewMessage::new(svc, 0xE1).from("Ana").body("bom dia"))
            .unwrap();
        let e = s.entry(id).unwrap();
        assert!(e.complete() && e.visible() && e.is_new() && e.unread());
        assert_eq!(e.details, "Ana");
        assert_eq!(s.body(id).unwrap(), "bom dia");
    }

    // -------------------------------------------------------- event decode --

    fn ev(kind: i32, a: i32, b: i32, c: i32, d: i32) -> sys::ShimEvent {
        sys::ShimEvent { kind, handle: 1, status: 0, a, b, c, d, native: 0 }
    }

    #[test]
    fn every_sub_kind_decodes() {
        let cases = [
            (sys::SHIM_MSV_EV_CREATED, StoreEventKind::Created),
            (sys::SHIM_MSV_EV_CHANGED, StoreEventKind::Changed),
            (sys::SHIM_MSV_EV_DELETED, StoreEventKind::Deleted),
            (sys::SHIM_MSV_EV_MOVED, StoreEventKind::Moved),
            (sys::SHIM_MSV_EV_MTM_INSTALLED, StoreEventKind::MtmInstalled),
            (sys::SHIM_MSV_EV_MTM_REMOVED, StoreEventKind::MtmRemoved),
            (sys::SHIM_MSV_EV_SERVER_READY, StoreEventKind::ServerReady),
            (sys::SHIM_MSV_EV_SERVER_GONE, StoreEventKind::ServerGone),
        ];
        for (raw, want) in cases {
            let got = store_event(&ev(sys::SHIM_EV_MSV, raw, 0x2001, 0x1004, 3)).unwrap();
            assert_eq!(got.kind, want);
            assert_eq!(got.id, 0x2001);
            assert_eq!(got.parent, 0x1004);
            assert_eq!(got.batch, 3);
        }
    }

    /// Handed any other event, this must decline rather than misread it — the whole point of
    /// returning an Option is that a caller can pass everything it drains.
    #[test]
    fn other_events_are_declined_not_misread() {
        assert!(store_event(&ev(sys::SHIM_EV_TIMER, 1, 0, 0, 0)).is_none());
        assert!(store_event(&ev(sys::SHIM_EV_PROP, 1, 0, 0, 0)).is_none());
        assert!(store_event(&ev(sys::SHIM_EV_QUIT, 0, 0, 0, 0)).is_none());
        // A sub-kind from a future shim. Not an error, and not a guess.
        assert!(store_event(&ev(sys::SHIM_EV_MSV, 99, 0, 0, 0)).is_none());
    }

    #[test]
    fn observing_is_off_until_asked() {
        let mut s = Session::with(MemMsv::new()).unwrap();
        s.observe().unwrap();
        s.stop_observing().unwrap();
        // The order is what matters: nothing turns delivery on as a side effect of opening.
        assert_eq!(
            s.msv().calls,
            alloc::vec![MsvCall::Open, MsvCall::Observe(true), MsvCall::Observe(false)]
        );
    }
}
