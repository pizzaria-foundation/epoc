//! Read-only reconnaissance over the Message Server.
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
//! # Read-only, on purpose
//!
//! Opening a session, enumerating the registered MTMs and counting folder entries is the
//! whole surface. That is enough to learn what the platform's messaging stack contains
//! before deciding whether to build on it, and it puts none of the user's actual messages
//! at risk for a reconnaissance run.

use alloc::vec::Vec;

use symbian_sys as sys;

use crate::error::{Error, Result};
use crate::fs::Utf16Path;

pub use sys::ShimMtmInfo as MtmInfo;

/// The standard folders, with the names to print them under.
pub const FOLDERS: &[(i32, &str)] = &[
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

/// An open Message Server session. Closes itself.
///
/// One at a time: the shim keeps a single slot, because a probe asks its questions in
/// sequence and a handle table would be ceremony around one pointer. A second [`Session::open`]
/// while one is live returns [`Error::InUse`].
pub struct Session {
    handle: i32,
}

impl Session {
    /// `CMsvSession::OpenSyncL`.
    ///
    /// Synchronous, and therefore able to block: the Message Server may be starting up, or
    /// rebuilding its index. Anything calling this from a process that must stay responsive
    /// wants a deadline around the *process*, which is how `apps/devdump` runs it.
    pub fn open() -> Result<Self> {
        let mut handle = 0i32;
        // SAFETY: `handle` is a live local; the shim writes at most one i32 through it.
        Error::check(unsafe { sys::shim_msv_open(&mut handle) })?;
        Ok(Session { handle })
    }

    /// How many MTMs are registered on this handset.
    pub fn mtm_count(&mut self) -> Result<i32> {
        let mut out = 0i32;
        // SAFETY: live local; handle validated by the shim.
        Error::check(unsafe { sys::shim_msv_mtm_count(self.handle, &mut out) })?;
        Ok(out)
    }

    /// Throw away the client-side registry snapshot and build a fresh one.
    ///
    /// [`Session::mtm_count`] reads a **per-process copy** of the registry, taken when the
    /// session opened and refreshed thereafter only through session events. So counting
    /// after [`Session::install_mtm`] measures the state from before the install, and
    /// "registered, but this process has not noticed" reads exactly like "not registered".
    /// Call this between the two, or the count is not evidence of anything.
    pub fn refresh_registry(&mut self) -> Result<()> {
        // SAFETY: no pointers; the handle is validated by the shim.
        Error::check(unsafe { sys::shim_msv_refresh_registry(self.handle) })
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
        // SAFETY: no pointers; the handle is validated by the shim.
        Error::check(unsafe { sys::shim_msv_can_instantiate(self.handle, mtm_uid) })
    }

    /// One registry entry: its type UID, technology UID and human-readable name.
    pub fn mtm_info(&mut self, index: i32) -> Result<MtmInfo> {
        let mut out = MtmInfo::default();
        // SAFETY: `out` is a live local of the layout the C side writes.
        Error::check(unsafe { sys::shim_msv_mtm_info(self.handle, index, &mut out) })?;
        Ok(out)
    }

    /// How many entries a standard folder holds. See [`FOLDERS`].
    pub fn folder_count(&mut self, folder_id: i32) -> Result<i32> {
        let mut out = 0i32;
        // SAFETY: live local; handle validated by the shim.
        Error::check(unsafe { sys::shim_msv_folder_count(self.handle, folder_id, &mut out) })?;
        Ok(out)
    }
}

/// A message about to be written into a folder.
///
/// It owns its text. The FFI struct the shim receives carries raw pointers, and building it
/// inside [`Session::create_message`] rather than exposing it means those pointers cannot
/// outlive the buffers they point at — which on this platform would not be a dangling read
/// so much as a descriptor over freed memory handed to the Message Server.
pub struct NewMessage {
    /// From [`Session::create_service`]. A message with no service is a message the framework
    /// cannot walk back to an account.
    pub service_id: i32,
    pub mtm_uid: u32,
    /// One of [`FOLDERS`]' ids. Defaults to the inbox.
    pub parent_id: i32,
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
    pub fn new(service_id: i32, mtm_uid: u32) -> Self {
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

    pub fn into_folder(mut self, folder_id: i32) -> Self {
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

impl Session {
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
        let units = p.as_units();
        // SAFETY: `units` is valid for its length and only read.
        let rc = unsafe { sys::shim_msv_deinstall_mtm(units.as_ptr(), units.len() as i32) };
        if rc < 0 && rc != sys::SHIM_ERR_NOT_FOUND {
            return Err(Error::from_code(rc));
        }
        // SAFETY: as above.
        Error::check(unsafe { sys::shim_msv_install_mtm(units.as_ptr(), units.len() as i32) })
    }

    /// Remove a registration. `NotFound` is not an error here.
    pub fn deinstall_mtm(&mut self, path: &str) -> Result<()> {
        let p = Utf16Path::new(path)?;
        let units = p.as_units();
        // SAFETY: `units` is valid for its length and only read.
        let rc = unsafe { sys::shim_msv_deinstall_mtm(units.as_ptr(), units.len() as i32) };
        if rc < 0 && rc != sys::SHIM_ERR_NOT_FOUND {
            return Err(Error::from_code(rc));
        }
        Ok(())
    }

    /// Create the service entry — the account the native Messaging application lists.
    ///
    /// `name` is what the user sees. Returns the new entry's id, which every message of this
    /// service must carry.
    pub fn create_service(&mut self, mtm_uid: u32, name: &str) -> Result<i32> {
        let units: Vec<u16> = name.encode_utf16().collect();
        let mut out = 0i32;
        // SAFETY: `units` is valid for its length and only read; `out` is a live local.
        Error::check(unsafe {
            sys::shim_msv_create_service(
                self.handle,
                mtm_uid,
                units.as_ptr(),
                units.len() as i32,
                &mut out,
            )
        })?;
        Ok(out)
    }

    /// Write a message into a folder. Returns its id.
    pub fn create_message(&mut self, msg: &NewMessage) -> Result<i32> {
        let raw = msg.raw();
        let mut out = 0i32;
        // SAFETY: `raw` borrows `msg`'s buffers, which outlive this call; the shim copies
        // out of them before returning.
        Error::check(unsafe { sys::shim_msv_create_message(self.handle, &raw, &mut out) })?;
        Ok(out)
    }

    /// Delete every service of a type, and everything under it. Returns how many went.
    ///
    /// Written as "all of this type" rather than "the one I made" on purpose: anything that
    /// creates a service per run and forgets to remove it fills the user's Messaging account
    /// list with copies of itself, and by the time that is noticed nothing remembers the old
    /// ids. This is the call that cleans up after a mistake already made.
    pub fn delete_services(&mut self, mtm_uid: u32) -> Result<i32> {
        // SAFETY: no pointers; the handle is validated by the shim.
        let rc = unsafe { sys::shim_msv_delete_services(self.handle, mtm_uid) };
        if rc < 0 {
            return Err(Error::from_code(rc));
        }
        Ok(rc)
    }

    pub fn delete_entry(&mut self, id: i32) -> Result<()> {
        // SAFETY: no pointers; the handle is validated by the shim.
        Error::check(unsafe { sys::shim_msv_delete_entry(self.handle, id) })
    }
}

/// The platform's own new-message notification: indicator, tone, floating note.
///
/// Every function here returns the platform's error rather than hiding it, and that is the
/// point. `MNcnNotification` is documented as an interface for *email* plugins — its
/// parameter is a mailbox and the note it raises says "New email" — and its implementation
/// is an ECom plugin, so nothing in the device library sweep could tell us whether it is
/// even present. Until a probe says otherwise, a caller should treat a failure here as a
/// finding about the handset and carry on: the message is already in the inbox either way.
pub mod ncn {
    use super::*;

    /// Icon only.
    pub const ICON: i32 = sys::SHIM_NCN_ICON;
    /// Icon, tone and floating note — what an arriving SMS produces.
    pub const NORMAL: i32 = sys::SHIM_NCN_NORMAL;

    pub fn notify(service_id: i32, indication: i32) -> Result<()> {
        // SAFETY: no pointers.
        Error::check(unsafe { sys::shim_ncn_notify(service_id, indication) })
    }

    /// Zero the new-message counter for a service, for when the user has read them.
    pub fn mark_unread(service_id: i32) -> Result<()> {
        // SAFETY: no pointers.
        Error::check(unsafe { sys::shim_ncn_mark_unread(service_id) })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: the shim ignores a handle it does not recognise.
        unsafe { sys::shim_msv_close(self.handle) }
    }
}

/// The registry entry's name as UTF-16 units, empty if it has none.
pub fn info_name(info: &MtmInfo) -> &[u16] {
    let n = (info.name_len.max(0) as usize).min(info.name.len());
    &info.name[..n]
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
