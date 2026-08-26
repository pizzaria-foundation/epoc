//! A messaging service inside Nokia's own Messaging application.
//!
//! The user's inbox lists your messages with your icon, opens them with your viewer, and
//! offers Reply. The reply they compose lands in the message store, and this crate is what
//! notices and hands it to you.
//!
//! # The two halves, and why only one of them is Rust
//!
//! Nokia's Messaging application loads a **message type module** — four C++ components
//! deriving from platform base classes — into its own process. That half cannot be Rust: the
//! classes are C++ inheritance and the DLL may hold no writable static data at all. It lives
//! in `shim/mtm/` as a library of base classes, and a service subclasses it in about a
//! hundred lines.
//!
//! This half is the other side: a process of your own that puts arriving messages into the
//! store and carries outgoing replies away. Nothing here runs inside Nokia's process, so a
//! bug here costs your daemon rather than the user's inbox.
//!
//! # What you implement
//!
//! Two methods. [`MessagingService::send`] carries one reply out; [`MessagingService::deleted`]
//! notes that the user threw something away. Everything else — registering, finding or
//! creating the account, noticing replies, not offering the same one twice — is [`Bridge`].
//!
//! ```ignore
//! const DESC: Descriptor = Descriptor::new(
//!     0xE0DD_0B01,                                  // == MTM_TYPE_UID in app.conf
//!     "C:\\resource\\messaging\\mtm\\tgmtmreg.rsc", // where symbuild installed it
//!     "Telegram",
//! );
//!
//! struct Tg { /* ... */ }
//! impl MessagingService for Tg {
//!     fn send(&mut self, out: &Outgoing<'_>) -> Sent {
//!         match self.mtproto.send_message(out.to, out.text) {
//!             Ok(()) => Sent::Done,
//!             Err(_) => Sent::Failed,
//!         }
//!     }
//! }
//!
//! // Once, at startup:
//! let ticker = symbian::timer_every(DESC.poll_interval_ms)?;
//!
//! // In the daemon's DaemonApp::handle_raw, for every event:
//! self.bridge.handle_raw(ev);              // a store event, if they work here
//! if self.bridge.rescan_owed() || ev.kind == symbian_sys::SHIM_EV_TIMER {
//!     let _ = self.bridge.poll();          // the timer is what makes it certain
//! }
//! ```
//!
//! # A timer is the mechanism; an event is an optimisation
//!
//! [`Bridge::poll`] re-reads the store and offers whatever it finds. Called on a timer, that
//! alone is a complete implementation — and it is the one this crate asks a service to write,
//! because it depends on nothing unmeasured.
//!
//! The Message Server also tells every open session when something changes, and
//! [`Bridge::handle_raw`] turns that into "a rescan is owed" so a reply is picked up in
//! milliseconds instead of at the next tick. That path is *unproven on this handset*: whether a
//! session event crosses a process boundary at all is what `apps/devdump/probes/msvev` exists to
//! measure, and it has not answered yet. Nothing depends on the answer — a service that never
//! receives an event behaves identically, one poll interval later.
//!
//! Which is also the reliability argument, and why the event carries no data. A dropped ring
//! slot, a daemon that was not running when the user replied, a notification the shim capped
//! because a hundred entries changed at once, and an event mechanism that turns out not to work
//! at all — the same recovery in every case, exercised on every single reply rather than only
//! in a disaster.
//!
//! # What "already handled" means, and why it is not a set of ids
//!
//! A carried reply is **moved** to the sent folder. That is durable state in the store, so a
//! daemon that restarts knows what it has already done. A set of ids in memory would forget,
//! and the failure mode of forgetting is sending the user's message again every time the
//! process starts.

#![cfg_attr(target_vendor = "symbian", no_std)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use symbian::error::Result;
use symbian::msg::{self, Entry, EntryId, MemMsv, Msv, NewMessage, Session, ShimMsv};
use symbian_sys as sys;

/// Build-time identity, carried as a runtime value.
///
/// **Not** on [`MessagingService`], and that is worth defending. Every field here already
/// exists in `app.conf` (`MTM_TYPE_UID`, `MTM_NAME`) and in the generated registration
/// resource. A trait method returning it would be a *fourth* copy of a fact that has to agree
/// in three places, and Rust could not enforce agreement with any of them — it would only add
/// somewhere else to be wrong. As a `const` written next to the `app.conf` value it stays one
/// grep.
#[derive(Copy, Clone, Debug)]
pub struct Descriptor<'a> {
    /// Must equal `MTM_TYPE_UID` in `app.conf` and `mtm_type_uid` in the registration.
    pub mtm_uid: u32,
    /// Where `symbuild` installed the registration resource.
    pub registration: &'a str,
    /// The account name the user sees in the Messaging application.
    pub service_name: &'a str,
    /// Folders an outgoing reply may appear in.
    ///
    /// Both drafts and outbox by default, because **which one the Messaging application picks
    /// is not measured**: it passes the destination to the UI MTM's `ReplyL` and nothing
    /// documents its choice for a third party. Watching both costs one extra folder listing
    /// per rescan and removes a whole class of "the reply is never seen". Narrow it once a
    /// device run says which.
    pub outgoing: &'a [EntryId],
    /// Where a carried reply is moved to, which is also how the store remembers it is done.
    pub sent: EntryId,
    /// How often a service should call [`Bridge::poll`], in milliseconds.
    ///
    /// This is the *mechanism*, not a fallback. Store events may make a reply arrive sooner, but
    /// whether they cross a process boundary on this handset is unmeasured, so nothing may
    /// depend on them. Five seconds is a compromise: a reply the user is waiting on feels
    /// prompt, and a folder listing every five seconds is cheap next to what a chat service is
    /// doing anyway.
    pub poll_interval_ms: i32,
}

impl Descriptor<'static> {
    pub const fn new(
        mtm_uid: u32,
        registration: &'static str,
        service_name: &'static str,
    ) -> Self {
        Descriptor {
            mtm_uid,
            registration,
            service_name,
            outgoing: &[sys::SHIM_MSV_DRAFTS, sys::SHIM_MSV_OUTBOX],
            sent: sys::SHIM_MSV_SENT,
            poll_interval_ms: 5_000,
        }
    }
}

/// A message arriving from the service, on its way into the user's inbox.
pub struct Incoming<'a> {
    /// `iDetails` — the correspondent, and the left-hand column in the native list.
    pub from: &'a str,
    /// `iDescription` — the preview line. `None` uses the first line of `text`.
    pub preview: Option<&'a str>,
    pub text: &'a str,
    /// Seconds since the Unix epoch; 0 means now.
    pub unix_time: i64,
    pub unread: bool,
}

impl<'a> Incoming<'a> {
    pub fn new(from: &'a str, text: &'a str) -> Self {
        Incoming { from, preview: None, text, unix_time: 0, unread: true }
    }

    pub fn at(mut self, unix_time: i64) -> Self {
        self.unix_time = unix_time;
        self
    }

    pub fn read(mut self) -> Self {
        self.unread = false;
        self
    }
}

/// A reply the user composed inside Nokia's Messaging application.
pub struct Outgoing<'a> {
    pub id: EntryId,
    /// The correspondent, out of `iDetails`.
    ///
    /// That is where a chat identity lives for this family of MTMs, because there is no
    /// addressee list — `AddAddresseeL` leaves with `KErrNotSupported`. The field has no
    /// documented length cap, so see [`Outgoing::to_truncated`] before using it as a key.
    pub to: &'a str,
    pub text: &'a str,
    pub unix_time: i64,
    /// True when the platform's `iDetails` was longer than the shim could carry.
    ///
    /// A service whose identities can exceed 64 UTF-16 units must not use `to` as a lookup
    /// key when this is set — it would address the reply to whoever else shares that prefix.
    /// Reported rather than hidden for exactly that reason.
    pub to_truncated: bool,
}

/// What the service did with a reply.
pub enum Sent {
    /// Gone. The bridge moves the entry to [`Descriptor::sent`] and it is never offered again.
    Done,
    /// Accepted, not yet acknowledged — a network round trip in flight. The entry stays where
    /// it is and is not offered again; call [`Bridge::confirm`] or [`Bridge::fail`] with this
    /// token when the answer arrives.
    Queued(u32),
    /// Not sent, and not worth retrying. The entry is marked failed and **left where the user
    /// can see it**: deleting somebody's unsent message is not ours to do.
    Failed,
}

/// The two runtime decisions that belong to a service and to nothing else.
pub trait MessagingService {
    /// Carry one reply out. Called at most once per entry until the answer says otherwise.
    fn send(&mut self, out: &Outgoing<'_>) -> Sent;

    /// The user deleted one of our entries. Default: nothing.
    ///
    /// A hint about an id, not a guarantee — the entry is already gone, so there is nothing to
    /// read. Useful for a service that mirrors deletions upstream.
    fn deleted(&mut self, _id: EntryId) {}
}

/// Is this entry a reply waiting to be carried out?
///
/// A free function, and public, so a test can enumerate every near miss rather than trusting a
/// method it cannot reach. Each clause earns its place:
///
/// - **in one of the watched folders** — anything else is somebody else's business, and an
///   entry in the *inbox* is a message we delivered, which would otherwise be echoed straight
///   back to the service.
/// - **a message**, not a service or folder entry, which have no body and never will.
/// - **our MTM and our service** — a handset can have more than one account of one type, and
///   another service's reply is not ours to send.
/// - **complete, visible, not in preparation** — this is the exact state a UI MTM's `ReplyL`
///   publishes in its final `ChangeL`, *after* committing the body. So the predicate cannot
///   see a half-written reply; it is the platform's own ordering doing the work.
/// - **not failed** — already offered and refused. Trying again on every rescan would be a
///   loop the user cannot stop.
pub fn is_pending(e: &Entry, desc: &Descriptor<'_>, service_id: EntryId) -> bool {
    desc.outgoing.contains(&e.parent)
        && e.is_message()
        && e.mtm_uid == desc.mtm_uid
        && e.service_id == service_id
        && e.complete()
        && e.visible()
        && !e.in_preparation()
        && !e.failed()
}

/// What [`Bridge::install`] did.
#[derive(Copy, Clone, Debug)]
pub struct Installed {
    pub service_id: EntryId,
    /// False when the account already existed from a previous run.
    pub service_created: bool,
    /// The registry count after the install, read from a refreshed snapshot.
    ///
    /// Not evidence on its own: the count comes from a per-process copy the session refreshes
    /// on an event that cannot arrive inside the caller's own call. It is here as a number for
    /// a report, not as a test.
    pub registry_count: i32,
}

/// A reply the service accepted but has not acknowledged.
struct InFlight {
    token: u32,
    id: EntryId,
}

/// Owns the session, the account, and the loop that finds replies.
pub struct Bridge<S: MessagingService, M: Msv = ShimMsv> {
    session: Session<M>,
    desc: Descriptor<'static>,
    service: S,
    service_id: EntryId,
    in_flight: Vec<InFlight>,
    next_token: u32,
    rescan_owed: bool,
}

impl<S: MessagingService> Bridge<S, ShimMsv> {
    /// Open a session and take ownership of it.
    ///
    /// The shim keeps one session slot, so a daemon that also wants its own
    /// [`symbian::msg::Session`] will get [`symbian::error::Error::InUse`]. Use
    /// [`Bridge::session`] instead.
    pub fn new(desc: Descriptor<'static>, service: S) -> Result<Self> {
        Bridge::with(ShimMsv, desc, service)
    }
}

impl<S: MessagingService, M: Msv> Bridge<S, M> {
    pub fn with(msv: M, desc: Descriptor<'static>, service: S) -> Result<Self> {
        Ok(Bridge {
            session: Session::with(msv)?,
            desc,
            service,
            service_id: 0,
            in_flight: Vec::new(),
            next_token: 1,
            rescan_owed: false,
        })
    }

    /// Register the MTM, find or create the account, start observing, and do a first scan.
    ///
    /// Idempotent, on purpose and in both halves. The registration de-installs before it
    /// installs, because installing over an existing group fails. And the account is
    /// **found** before it is created — a service that creates one per run fills the user's
    /// Messaging account list with copies of itself, and by the time that is noticed nothing
    /// remembers the old ids.
    ///
    /// The first scan is not a nicety either: it is the path that catches a reply written
    /// while the daemon was not running, which no event will ever mention.
    pub fn install(&mut self) -> Result<Installed> {
        self.session.install_mtm(self.desc.registration)?;
        self.session.refresh_registry()?;
        let registry_count = self.session.mtm_count().unwrap_or(-1);

        let existing = self.session.services(self.desc.mtm_uid)?;
        let (service_id, service_created) = match existing.first() {
            Some(&id) => (id, false),
            None => (
                self.session
                    .create_service(self.desc.mtm_uid, self.desc.service_name)?,
                true,
            ),
        };
        self.service_id = service_id;

        /* Event delivery is switched on if it can be, and its failure is not fatal: the timer
         * is what a service actually relies on. An install that refused to complete because an
         * optimisation was unavailable would make the unmeasured path load-bearing after all. */
        let _ = self.session.observe();
        self.rescan_owed = true;

        Ok(Installed { service_id, service_created, registry_count })
    }

    pub fn service_id(&self) -> EntryId {
        self.service_id
    }

    pub fn service(&mut self) -> &mut S {
        &mut self.service
    }

    /// The session, for a service that needs the store directly. Shared, not lent twice.
    pub fn session(&mut self) -> &mut Session<M> {
        &mut self.session
    }

    /// Put a message in the user's inbox. Returns its store id.
    ///
    /// The id is returned rather than remembered: a service that needs to map its own message
    /// ids to store ids already has somewhere to keep that, and a table in here would be a
    /// second database next to the one every service has.
    ///
    /// Does **not** raise the platform's new-message notification — see [`Bridge::notify`].
    pub fn deliver(&mut self, msg: &Incoming<'_>) -> Result<EntryId> {
        let preview = msg.preview.unwrap_or_else(|| first_line(msg.text));
        let mut m = NewMessage::new(self.service_id, self.desc.mtm_uid)
            .from(msg.from)
            .subject(preview)
            .body(msg.text)
            .at(msg.unix_time);
        m.flags = if msg.unread {
            sys::SHIM_MSV_NEW | sys::SHIM_MSV_UNREAD
        } else {
            0
        };
        self.session.create_message(&m)
    }

    /// Hand one platform event in. Cheap, and deliberately so.
    ///
    /// It decodes the event and records that a rescan is owed. It reads nothing: this runs
    /// inside the daemon's event drain, and a store read there can block on the Message
    /// Server — which on a handset is the difference between a daemon and a hung process.
    ///
    /// Events for other subsystems are ignored, so a caller can pass everything it drains.
    pub fn handle_raw(&mut self, ev: &sys::ShimEvent) {
        let Some(store) = msg::store_event(ev) else {
            return;
        };
        match store.kind {
            msg::StoreEventKind::Deleted => {
                // Reported straight through: there is nothing to re-read, the entry is gone.
                // Still owes a rescan, because a delete arrives in the same batch as the
                // changes around it and the batch may have been capped.
                self.service.deleted(store.id);
                self.rescan_owed = true;
            }
            // Everything else, including the server coming back and the registry changing,
            // means "look again". Distinguishing them here would be deciding on data this
            // event is not allowed to carry.
            _ => self.rescan_owed = true,
        }
    }

    /// True when something has changed since the last [`Bridge::poll`].
    ///
    /// A daemon checks this once after draining its events rather than polling per event, so a
    /// burst of twenty notifications costs one scan.
    pub fn rescan_owed(&self) -> bool {
        self.rescan_owed
    }

    /// Scan the watched folders, offer each unhandled reply to the service, act on the answer.
    ///
    /// Returns how many were offered. Safe to call at any time — it derives everything from
    /// the store — which is what makes it both the event path and the recovery path. A service
    /// with no event delivery can call it on a timer and lose nothing but latency.
    pub fn poll(&mut self) -> Result<usize> {
        self.rescan_owed = false;
        let mut offered = 0usize;

        for &folder in self.desc.outgoing {
            for id in self.session.children(folder)? {
                if self.in_flight.iter().any(|f| f.id == id) {
                    continue;
                }
                // Re-read rather than trusting the listing: between the listing and here, the
                // Messaging application may have finished writing, or the user may have
                // deleted it. A NotFound is the second case and is not an error.
                let entry = match self.session.entry(id) {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                if !is_pending(&entry, &self.desc, self.service_id) {
                    continue;
                }

                let text = self.session.body(id)?;
                let out = Outgoing {
                    id,
                    to: &entry.details,
                    text: &text,
                    unix_time: entry.unix_time,
                    to_truncated: entry.details_truncated,
                };
                let verdict = self.service.send(&out);
                offered += 1;

                match verdict {
                    Sent::Done => self.finish(id)?,
                    Sent::Failed => self.mark_failed(id)?,
                    Sent::Queued(token) => self.in_flight.push(InFlight { token, id }),
                }
            }
        }
        Ok(offered)
    }

    /// A token for a [`Sent::Queued`] answer, unique within this process.
    pub fn next_token(&mut self) -> u32 {
        let t = self.next_token;
        self.next_token = self.next_token.wrapping_add(1).max(1);
        t
    }

    /// The queued reply went out.
    pub fn confirm(&mut self, token: u32) -> Result<()> {
        match self.take_in_flight(token) {
            Some(id) => self.finish(id),
            None => Ok(()),
        }
    }

    /// The queued reply did not go out, and will not.
    pub fn fail(&mut self, token: u32) -> Result<()> {
        match self.take_in_flight(token) {
            Some(id) => self.mark_failed(id),
            None => Ok(()),
        }
    }

    /// How many replies are awaiting [`Bridge::confirm`] or [`Bridge::fail`].
    pub fn in_flight(&self) -> usize {
        self.in_flight.len()
    }

    /// The platform's new-message notification: indicator, tone, floating note.
    ///
    /// **Measured fatal on the E72.** `MNcnNotification::NewMessages` kills the calling
    /// process, with a folder id and with a real service id alike (`docs/device-notes.md`).
    /// So it is here, it is never called by [`Bridge::deliver`], and it returns the platform's
    /// error rather than hiding it — for a handset that might answer differently, not for this
    /// one. On this one, delivered messages arrive quietly.
    pub fn notify(&mut self) -> Result<()> {
        msg::ncn::notify(self.service_id, msg::ncn::NORMAL)
    }

    fn take_in_flight(&mut self, token: u32) -> Option<EntryId> {
        let i = self.in_flight.iter().position(|f| f.token == token)?;
        Some(self.in_flight.remove(i).id)
    }

    /// Carried out: move it out of the watched folder, and stop it looking unread.
    ///
    /// The move is what makes this durable. Recording the id in memory instead would forget
    /// across a restart, and the failure mode of forgetting is sending the user's message a
    /// second time.
    fn finish(&mut self, id: EntryId) -> Result<()> {
        self.session
            .set_flags(id, 0, sys::SHIM_MSV_NEW | sys::SHIM_MSV_UNREAD)?;
        self.session.move_entry(id, self.desc.sent)
    }

    /// Refused: flag it and leave it where it is.
    ///
    /// Not deleted, and not moved. It is the user's unsent message; they should be able to
    /// find it, and the platform's own applications show a failed entry in place.
    fn mark_failed(&mut self, id: EntryId) -> Result<()> {
        self.session.set_flags(id, sys::SHIM_MSV_FAILED, 0)
    }
}

/// Everything up to the first line break, for a preview line.
fn first_line(s: &str) -> &str {
    match s.find(['\n', '\r']) {
        Some(i) => &s[..i],
        None => s,
    }
}

/// A [`Bridge`] over an in-memory store, for host tests in a service's own crate.
pub type MemBridge<S> = Bridge<S, MemMsv>;

/// Build a bridge over a given [`MemMsv`] — the host-test entry point.
pub fn with_fake<S: MessagingService>(
    fake: MemMsv,
    desc: Descriptor<'static>,
    service: S,
) -> Result<MemBridge<S>> {
    Bridge::with(fake, desc, service)
}

/// Turn a body into a preview line, exposed because a service composing its own
/// [`Incoming::preview`] wants the same rule.
pub fn preview_of(text: &str) -> String {
    String::from(first_line(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    const MTM: u32 = 0xE0DD_0B01;
    const REG: &str = "C:\\resource\\messaging\\mtm\\testreg.rsc";
    const DESC: Descriptor<'static> = Descriptor::new(MTM, REG, "Test service");

    /// Records what it was offered and answers however the test says.
    ///
    /// A test double for a trait the *test* implements, so it stays `#[cfg(test)]` — unlike
    /// `MemMsv`, which has callers above it. Same distinction that keeps `FakeNet` private
    /// while `MemFs` is public.
    struct Recorder {
        offered: Vec<(EntryId, String, String)>,
        deleted: Vec<EntryId>,
        answer: fn(usize) -> Sent,
    }

    impl Recorder {
        fn new() -> Self {
            Recorder { offered: Vec::new(), deleted: Vec::new(), answer: |_| Sent::Done }
        }
        fn answering(answer: fn(usize) -> Sent) -> Self {
            Recorder { offered: Vec::new(), deleted: Vec::new(), answer }
        }
    }

    impl MessagingService for Recorder {
        fn send(&mut self, out: &Outgoing<'_>) -> Sent {
            let n = self.offered.len();
            self.offered.push((out.id, String::from(out.to), String::from(out.text)));
            (self.answer)(n)
        }
        fn deleted(&mut self, id: EntryId) {
            self.deleted.push(id);
        }
    }

    fn bridge(fake: MemMsv, svc: Recorder) -> MemBridge<Recorder> {
        let mut b = with_fake(fake, DESC, svc).unwrap();
        b.install().unwrap();
        b
    }

    /// Event delivery being unavailable must not stop a service starting. The timer is the
    /// mechanism; observation is an optimisation whose viability on this handset is unmeasured,
    /// and an install that failed without it would make the unproven path load-bearing.
    #[test]
    fn install_survives_event_delivery_being_refused() {
        let mut fake = MemMsv::new();
        fake.refuse_observe = true;
        let mut b = with_fake(fake, DESC, Recorder::new()).unwrap();
        let out = b.install().expect("install must not depend on observe");
        assert!(out.service_created);
        assert!(!b.session().msv().observing);
        /* And the loop still works, because it never needed an event. */
        let svc = out.service_id;
        b.session().msv().push_reply(sys::SHIM_MSV_DRAFTS, MTM, svc, "Ana", "ok");
        assert_eq!(b.poll().unwrap(), 1);
    }

    /// The default interval has to be short enough that a reply feels prompt. The number is a
    /// judgement, but a *missing* one would be a service that never polls at all.
    #[test]
    fn the_default_poll_interval_is_set_and_sane() {
        const { assert!(DESC.poll_interval_ms >= 1_000, "would hammer the Message Server") };
        const { assert!(DESC.poll_interval_ms <= 15_000, "a reply would feel lost") };
    }

    #[test]
    fn install_registers_and_creates_the_account() {
        let mut b = with_fake(MemMsv::new(), DESC, Recorder::new()).unwrap();
        let out = b.install().unwrap();
        assert!(out.service_created);
        assert_ne!(out.service_id, 0);
        assert_eq!(b.service_id(), out.service_id);
    }

    /// The mistake this prevents already happened once, and the cleanup for it is
    /// `delete_services`. A second run must reuse the account, not add another.
    #[test]
    fn installing_twice_reuses_the_account() {
        let mut fake = MemMsv::new();
        let first = fake.push_service(MTM, "Test service");
        let mut b = with_fake(fake, DESC, Recorder::new()).unwrap();
        let out = b.install().unwrap();
        assert!(!out.service_created);
        assert_eq!(out.service_id, first);
        assert_eq!(b.session().services(MTM).unwrap().len(), 1);
    }

    /// Another MTM's account is not ours to adopt.
    #[test]
    fn a_foreign_account_is_not_reused() {
        let mut fake = MemMsv::new();
        fake.push_service(0xE0DD_9999, "somebody else");
        let mut b = with_fake(fake, DESC, Recorder::new()).unwrap();
        assert!(b.install().unwrap().service_created);
    }

    /// Delivery has to be switched on by the install, or the daemon sits waiting for events
    /// the shim was never told to send.
    #[test]
    fn observing_starts_with_the_install() {
        let mut b = with_fake(MemMsv::new(), DESC, Recorder::new()).unwrap();
        assert!(!b.session().msv().observing);
        b.install().unwrap();
        assert!(b.session().msv().observing);
    }

    // ---------------------------------------------------------------- the loop --

    #[test]
    fn a_reply_in_drafts_is_offered_once_and_then_moved() {
        let mut fake = MemMsv::new();
        let svc = fake.push_service(MTM, "Test service");
        let id = fake.push_reply(sys::SHIM_MSV_DRAFTS, MTM, svc, "Ana", "bom dia");
        let mut b = bridge(fake, Recorder::new());

        assert_eq!(b.poll().unwrap(), 1);
        assert_eq!(b.service().offered.len(), 1);
        assert_eq!(b.service().offered[0].1, "Ana");
        assert_eq!(b.service().offered[0].2, "bom dia");

        // Moved to sent, which is what stops it being offered again — including after a
        // restart, which an in-memory set of ids would not survive.
        assert_eq!(b.session().entry(id).unwrap().parent, sys::SHIM_MSV_SENT);
        assert_eq!(b.poll().unwrap(), 0);
        assert_eq!(b.service().offered.len(), 1);
    }

    /// The outbox as well as drafts, because which one the Messaging application uses is not
    /// measured and watching one would be a guess.
    #[test]
    fn a_reply_in_the_outbox_is_found_too() {
        let mut fake = MemMsv::new();
        let svc = fake.push_service(MTM, "Test service");
        fake.push_reply(sys::SHIM_MSV_OUTBOX, MTM, svc, "Ana", "ok");
        let mut b = bridge(fake, Recorder::new());
        assert_eq!(b.poll().unwrap(), 1);
    }

    /// Every near miss, one test, because each of these silently either loses a reply or
    /// echoes something that was never one.
    #[test]
    fn nothing_else_is_mistaken_for_a_reply() {
        let mut fake = MemMsv::new();
        let svc = fake.push_service(MTM, "Test service");
        let published = sys::SHIM_MSV_COMPLETE | sys::SHIM_MSV_VISIBLE;

        // Still being written by the Messaging application: body not committed yet.
        fake.push_message(
            sys::SHIM_MSV_DRAFTS,
            MTM,
            svc,
            "Ana",
            "half",
            published | sys::SHIM_MSV_IN_PREPARATION,
        );
        // Created but not yet published.
        fake.push_message(sys::SHIM_MSV_DRAFTS, MTM, svc, "Ana", "invisible", sys::SHIM_MSV_COMPLETE);
        // Already offered and refused.
        fake.push_message(sys::SHIM_MSV_DRAFTS, MTM, svc, "Ana", "no", published | sys::SHIM_MSV_FAILED);
        // Another MTM's reply.
        fake.push_message(sys::SHIM_MSV_DRAFTS, 0xE0DD_9999, svc, "Ana", "theirs", published);
        // Our MTM, another account.
        fake.push_message(sys::SHIM_MSV_DRAFTS, MTM, svc + 500, "Ana", "other account", published);
        // A message we delivered. In the inbox, so not watched — otherwise every incoming
        // message would be echoed straight back to the service.
        fake.push_message(sys::SHIM_MSV_INBOX, MTM, svc, "Ana", "incoming", published);

        let mut b = bridge(fake, Recorder::new());
        assert_eq!(b.poll().unwrap(), 0);
        assert!(b.service().offered.is_empty());
    }

    /// A service entry sitting in a watched folder must not be read as a message. It has no
    /// body and never will.
    #[test]
    fn a_service_entry_is_not_a_reply() {
        let mut fake = MemMsv::new();
        let svc = fake.push_service(MTM, "Test service");
        // Force a service-typed entry into drafts, which is not something the platform does
        // but is exactly the shape the type check exists for.
        let id = fake.push_reply(sys::SHIM_MSV_DRAFTS, MTM, svc, "Ana", "x");
        let i = fake.entries.iter().position(|(e, _)| e.id == id).unwrap();
        fake.entries[i].0.type_uid = msg::TYPE_SERVICE;

        let mut b = bridge(fake, Recorder::new());
        assert_eq!(b.poll().unwrap(), 0);
    }

    #[test]
    fn a_refused_reply_is_flagged_and_left_where_the_user_can_see_it() {
        let mut fake = MemMsv::new();
        let svc = fake.push_service(MTM, "Test service");
        let id = fake.push_reply(sys::SHIM_MSV_DRAFTS, MTM, svc, "Ana", "nope");
        let mut b = bridge(fake, Recorder::answering(|_| Sent::Failed));

        assert_eq!(b.poll().unwrap(), 1);
        let e = b.session().entry(id).unwrap();
        assert!(e.failed());
        assert_eq!(e.parent, sys::SHIM_MSV_DRAFTS, "not deleted, not moved");
        // And not offered again, or the user would face a loop they cannot stop.
        assert_eq!(b.poll().unwrap(), 0);
    }

    // -------------------------------------------------------------- in flight --

    #[test]
    fn a_queued_reply_is_not_offered_again_until_it_is_answered() {
        let mut fake = MemMsv::new();
        let svc = fake.push_service(MTM, "Test service");
        let id = fake.push_reply(sys::SHIM_MSV_DRAFTS, MTM, svc, "Ana", "wait");
        let mut b = bridge(fake, Recorder::answering(|_| Sent::Queued(7)));

        assert_eq!(b.poll().unwrap(), 1);
        assert_eq!(b.in_flight(), 1);
        assert_eq!(b.poll().unwrap(), 0, "in flight, not re-offered");
        assert_eq!(b.session().entry(id).unwrap().parent, sys::SHIM_MSV_DRAFTS);

        b.confirm(7).unwrap();
        assert_eq!(b.in_flight(), 0);
        assert_eq!(b.session().entry(id).unwrap().parent, sys::SHIM_MSV_SENT);
        assert_eq!(b.poll().unwrap(), 0);
    }

    #[test]
    fn failing_a_queued_reply_flags_it_in_place() {
        let mut fake = MemMsv::new();
        let svc = fake.push_service(MTM, "Test service");
        let id = fake.push_reply(sys::SHIM_MSV_DRAFTS, MTM, svc, "Ana", "wait");
        let mut b = bridge(fake, Recorder::answering(|_| Sent::Queued(3)));
        b.poll().unwrap();
        b.fail(3).unwrap();
        let e = b.session().entry(id).unwrap();
        assert!(e.failed());
        assert_eq!(e.parent, sys::SHIM_MSV_DRAFTS);
    }

    /// A token nobody is holding must not panic or touch anything. It arrives when a service
    /// confirms twice, or confirms after a restart.
    #[test]
    fn an_unknown_token_is_ignored() {
        let mut b = bridge(MemMsv::new(), Recorder::new());
        b.confirm(999).unwrap();
        b.fail(999).unwrap();
    }

    #[test]
    fn tokens_are_never_zero_and_never_repeat() {
        let mut b = bridge(MemMsv::new(), Recorder::new());
        let a = b.next_token();
        let c = b.next_token();
        assert_ne!(a, 0);
        assert_ne!(a, c);
    }

    // ------------------------------------------------------------- delivering --

    #[test]
    fn delivering_puts_a_new_unread_message_in_the_inbox() {
        let mut b = bridge(MemMsv::new(), Recorder::new());
        let id = b.deliver(&Incoming::new("Ana", "bom dia\nsegunda linha")).unwrap();
        let e = b.session().entry(id).unwrap();
        assert_eq!(e.parent, sys::SHIM_MSV_INBOX);
        assert_eq!(e.details, "Ana");
        assert!(e.is_new() && e.unread() && e.complete() && e.visible());
        assert_eq!(e.service_id, b.service_id());
        assert_eq!(e.mtm_uid, MTM);
        assert_eq!(b.session().body(id).unwrap(), "bom dia\nsegunda linha");
    }

    /// The preview is the first line, not the whole body: the native list draws one line and a
    /// description with a newline in it is a row that renders wrong.
    #[test]
    fn the_preview_is_the_first_line_by_default() {
        assert_eq!(preview_of("bom dia\nsegunda"), "bom dia");
        assert_eq!(preview_of("uma linha só"), "uma linha só");
        assert_eq!(preview_of("crlf\r\nx"), "crlf");
        let mut b = bridge(MemMsv::new(), Recorder::new());
        let id = b.deliver(&Incoming::new("Ana", "linha 1\nlinha 2")).unwrap();
        assert_eq!(b.session().entry(id).unwrap().description, "linha 1");
    }

    #[test]
    fn a_message_delivered_as_read_does_not_bold_the_row() {
        let mut b = bridge(MemMsv::new(), Recorder::new());
        let id = b.deliver(&Incoming::new("Ana", "old").read()).unwrap();
        let e = b.session().entry(id).unwrap();
        assert!(!e.is_new() && !e.unread());
        assert!(e.visible(), "still listed, just not bold");
    }

    /// The scan must never author a message. If it ever did, a reply would be echoed into the
    /// inbox as though it had arrived — and the next scan would find that too.
    #[test]
    fn polling_never_creates_a_message() {
        let mut fake = MemMsv::new();
        let svc = fake.push_service(MTM, "Test service");
        fake.push_reply(sys::SHIM_MSV_DRAFTS, MTM, svc, "Ana", "ok");
        let mut b = bridge(fake, Recorder::new());
        let creates = |b: &mut MemBridge<Recorder>| {
            b.session()
                .msv()
                .calls
                .iter()
                .filter(|c| matches!(c, msg::MsvCall::CreateMessage(_)))
                .count()
        };
        let before = creates(&mut b);
        b.poll().unwrap();
        assert_eq!(creates(&mut b), before);
    }

    // ------------------------------------------------------------------ events --

    fn ev(kind: i32, a: i32, b: i32) -> sys::ShimEvent {
        sys::ShimEvent { kind, handle: 1, status: 0, a, b, c: sys::SHIM_MSV_DRAFTS, d: 1, native: 0 }
    }

    #[test]
    fn a_store_event_owes_a_rescan_and_reads_nothing() {
        let mut b = bridge(MemMsv::new(), Recorder::new());
        assert!(b.rescan_owed(), "install owes one, for replies written while we were down");
        b.poll().unwrap();
        assert!(!b.rescan_owed());

        b.handle_raw(&ev(sys::SHIM_EV_MSV, sys::SHIM_MSV_EV_CREATED, 0x2001));
        assert!(b.rescan_owed());
    }

    #[test]
    fn events_for_other_subsystems_are_ignored() {
        let mut b = bridge(MemMsv::new(), Recorder::new());
        b.poll().unwrap();
        b.handle_raw(&ev(sys::SHIM_EV_TIMER, 1, 0));
        b.handle_raw(&ev(sys::SHIM_EV_RECV, 1, 0));
        assert!(!b.rescan_owed());
    }

    /// A delete is the one kind reported straight through, because there is nothing left to
    /// re-read. It still owes a rescan: the shim caps a batch, and the entries around the
    /// deleted one may not have been reported at all.
    #[test]
    fn a_delete_reaches_the_service_and_still_owes_a_rescan() {
        let mut b = bridge(MemMsv::new(), Recorder::new());
        b.poll().unwrap();
        b.handle_raw(&ev(sys::SHIM_EV_MSV, sys::SHIM_MSV_EV_DELETED, 0x2002));
        assert_eq!(b.service().deleted, vec![0x2002]);
        assert!(b.rescan_owed());
    }

    /// The server restarting is not information about an entry, and must not be read as one —
    /// but it does mean everything known is stale.
    #[test]
    fn the_server_coming_back_owes_a_rescan() {
        let mut b = bridge(MemMsv::new(), Recorder::new());
        b.poll().unwrap();
        b.handle_raw(&ev(sys::SHIM_EV_MSV, sys::SHIM_MSV_EV_SERVER_READY, 0));
        assert!(b.rescan_owed());
        assert!(b.service().deleted.is_empty());
    }
}
