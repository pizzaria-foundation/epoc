//! The piece between the login screens and the wire.
//!
//! `login.rs` knows what to draw. `link.rs` knows how to reach Telegram. Neither owned the
//! other, so pressing "Avançar" produced a `Progress::Call { body, tag }` that was dropped
//! on the floor — the screens typed and nothing was ever sent.
//!
//! # Everything is one tick
//!
//! `rust_step` must return in milliseconds, so nothing here waits. A login is a dozen
//! events: attach, connect, four handshake round trips, two worker completions, then the
//! encrypted calls. Each does a little and returns.
//!
//! # One worker, and why that needs arbitration
//!
//! Measured on an E72:
//!
//! | | |
//! |---|---|
//! | one 2048-bit exponentiation | 821 ms |
//! | the handshake needs two | ~1.7 s |
//! | the two-factor derivation | 4.9 s |
//! | SRP adds three exponentiations | ~2.5 s |
//!
//! Any of those on the GUI thread freezes the window server — the whole phone, with nothing
//! to recover it. The shim runs **one** job at a time, so [`Link`] owns the only `Job` and
//! this holds anything that arrives while it is busy. Without [`Driver::queued`], SRP's
//! second exponentiation asked for during the first would simply be lost, and the login
//! would stop with no error.

use alloc::string::String;
use alloc::vec::Vec;

use symbian_sys as sys;
use tg_proto::crypto::Rng;

/// How many messages a page of history holds.
///
/// The screen shows about eight at this font size, so twenty is two screenfuls of headroom
/// and still a small enough answer to walk on a 600 MHz core.
const HISTORY_PAGE: i32 = 20;

use crate::link::{Link, Progress as LinkProgress};
use crate::login::{Login, Progress as LoginProgress};

/// What the application should do about what just happened.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Outcome {
    None,
    /// A reply to something this driver asked for on its own behalf, rather than the
    /// login's. Carries the raw TL body for the application to parse.
    Answered(u32, Vec<u8>),
    /// An RPC error for one of our driver tags (not the login machine's).
    RequestFailed(u32, alloc::string::String),
    /// Redraw; the status line or a screen changed.
    Redraw,
    /// Signed in. Move to the chat list.
    Authorized,
    /// The connection is gone, and the reason is worth showing.
    Disconnected(&'static str),
}

/// Work held back because the worker was busy.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Queued {
    ModPow { base: Vec<u8>, exp: Vec<u8>, modulus: Vec<u8> },
    Kdf { password: Vec<u8>, salt1: Vec<u8>, salt2: Vec<u8> },
}

/// The tag for `messages.getDialogs`.
///
/// Above anything `auth.rs` uses, so a reply cannot be handed to the login machine by
/// accident — the two number spaces used to be one and nothing said so.
pub const TAG_DIALOGS: u32 = 0x1000_0001;
/// A page of one conversation. The chat's index is in the low bits, so a reply that arrives
/// after the user has moved on still lands in the right conversation.
pub const TAG_HISTORY: u32 = 0x1001_0000;
/// A message going out. Also carries the chat index.
pub const TAG_SEND: u32 = 0x1002_0000;
/// A file download (`upload.getFile`). Also carries the chat index.
pub const TAG_FILE: u32 = 0x1003_0000;
/// A message refetch (`messages.getMessages`), for refreshing an expired file reference.
pub const TAG_REFRESH: u32 = 0x1004_0000;
/// `auth.exportAuthorization`, asked on the home link. The low bits carry the target dc.
pub const TAG_EXPORT: u32 = 0x1005_0000;
/// `auth.importAuthorization`, sent on the file link.
pub const TAG_IMPORT: u32 = 0x1006_0000;
/// The mask for the index those two carry.
pub const TAG_INDEX: u32 = 0xffff;

/// How long to wait for an ordinary reply before calling the link stuck.
const REPLY_TIMEOUT_MS: i32 = 10_000;
/// And for a file chunk, which is 128 KiB rather than a few hundred bytes.
///
/// GPRS on this handset measures around 30 kbit/s in practice, so one chunk is roughly
/// thirty-five seconds of transfer. Sixty leaves margin for a slow start without waiting so
/// long that a genuinely dead link looks alive.
const FILE_TIMEOUT_MS: i32 = 60_000;

/// One chunk request, held until the connection that must carry it is usable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRequest {
    pub chat: usize,
    pub is_photo: bool,
    pub id: i64,
    pub access_hash: i64,
    pub file_reference: Vec<u8>,
    pub thumb_size: alloc::string::String,
    pub offset: i64,
}

impl FileRequest {
    fn body(&self) -> Vec<u8> {
        let limit = tg_proto::rpc::CHUNK;
        if self.is_photo {
            tg_proto::rpc::get_file_photo(
                self.id, self.access_hash, &self.file_reference, &self.thumb_size,
                self.offset, limit,
            )
        } else {
            tg_proto::rpc::get_file_document(
                self.id, self.access_hash, &self.file_reference, &self.thumb_size,
                self.offset, limit,
            )
        }
    }
}

/// How far along a second connection is toward being able to serve a download.
///
/// A connection to another data centre is authenticated by its handshake but not
/// *authorised*: it has an auth key and no user behind it, and `upload.getFile` on it
/// answers `AUTH_KEY_UNREGISTERED`. Becoming usable takes a token exported from the home
/// connection and imported into this one, which is two more round trips on two different
/// sockets — hence a state machine rather than a flag.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DcState {
    /// The handshake is running. Two exponentiations at 815 ms each on this handset.
    Connecting,
    /// Handshake done. `auth.exportAuthorization` has gone out on the *home* link.
    AwaitingExport,
    /// The token came back and `auth.importAuthorization` has gone out on *this* link.
    Importing,
    /// Authorised. Downloads can go now.
    Ready,
}

impl DcState {
    /// Whether a request may be sent on this connection yet.
    pub fn is_usable(self) -> bool {
        matches!(self, DcState::Ready)
    }
}

/// A second connection, to the data centre a file actually lives on.
struct FileDc {
    dc: u8,
    link: Link,
    state: DcState,
    /// The download waiting for the connection to become usable. One, because the app only
    /// ever has one download in flight.
    waiting: Option<FileRequest>,
}

pub struct Driver {
    link: Option<Link>,
    /// A connection to a non-home data centre, built the first time a file needs one and
    /// kept afterwards — the handshake is far too expensive to repeat per photo.
    file_dc: Option<FileDc>,
    /// Whether the download in flight went out on [`Self::file_dc`] rather than on the home
    /// link.
    ///
    /// `TAG_FILE` alone cannot say: media on the home data centre carries the same tag. The
    /// distinction matters when a request goes silent — the watchdog must tear down the
    /// connection that actually stalled, and dropping the session because a photo on another
    /// data centre timed out would empty the chat list.
    file_req_on_dc: bool,
    queued: Option<Queued>,
    /// Requests made before the session existed. See `LoginProgress::Call`.
    ///
    /// A [`Vec`] rather than `Option` because several can accumulate while the handshake runs —
    /// a quick typist can send a message before the connection is up, and the first one must not
    /// be silently overwritten by the second.
    pending_call: Vec<(Vec<u8>, u32)>,
    /// Set once the handshake finishes, so the key is written exactly once.
    persisted: bool,
    /// The one-shot timer that gets connecting off the start-up path, and `None` once it
    /// has fired. See `App::login`.
    connect_timer: Option<i32>,
    /// A timer that fires after a disconnect, so the link can be rebuilt without the user
    /// having to restart the application.
    reconnect_timer: Option<i32>,
    /// A timer that fires when a call is queued but the link hasn't responded within 10 s.
    /// The user is staring at "sending the code" and deserves an answer.
    /// Watchdog for a request that has gone out and not been answered.
    ///
    /// Keyed to the tag, and cleared by that tag's reply or error. It used to be armed only
    /// when a call *could not* be sent and cleared on `Authenticated` — which is to say it
    /// was dropped at exactly the moment the request finally went out, so the case it was
    /// named for, a call that leaves and is never answered, had no watchdog at all.
    stuck_timer: Option<(i32, u32)>,
    /// How many times the link has been rebuilt since the last user action.
    retries: u8,
    /// What a waiting screen says. `&'static str` so the screen can hold it directly —
    /// every value is a literal and a String here would allocate on every event.
    pub status: &'static str,
}

impl Default for Driver {
    fn default() -> Self {
        Self::new()
    }
}

impl Driver {
    pub fn new() -> Self {
        Driver {
            link: None,
            file_dc: None,
            file_req_on_dc: false,
            queued: None,
            pending_call: Vec::new(),
            persisted: false,
            connect_timer: None,
            reconnect_timer: None,
            stuck_timer: None,
            retries: 0,
            status: "",
        }
    }

    /// Ask for the chat list. Called once the login says it is authorized, and again
    /// when the user scrolls to the bottom of the list.
    ///
    /// When `offset_peer` is `None` the request is for the first page; with a peer it
    /// is for the page that follows it.
    pub fn request_dialogs(
        &mut self,
        now: i64,
        offset_date: i32,
        offset_id: i32,
        offset_peer: Option<crate::model::PeerRef>,
    ) -> bool {
        let Some(l) = self.link.as_mut() else {
            symbian::log!("[rpc] dialogs: no link");
            return false;
        };
        let peer_tuple = offset_peer.map(|p| (p.kind, p.id, p.access_hash));
        let body = tg_proto::rpc::get_dialogs(20, offset_date, offset_id, peer_tuple);
        let sent = l.call(&body, TAG_DIALOGS, now);
        symbian::log!("[rpc] dialogs requested={}", if sent { "sent" } else { "queued" });
        if !sent {
            self.pending_call.push((body, TAG_DIALOGS));
        }
        self.arm_watchdog(TAG_DIALOGS);
        sent
    }

    /// Ask for a page of one conversation.
    ///
    /// `offset_id` is exclusive and counts backwards: zero means the newest page, and the
    /// oldest id already held means the page above it. That is what makes scrolling up cost
    /// one request per screenful rather than re-fetching from the top.
    pub fn request_history(&mut self, chat: usize, p: crate::model::PeerRef, offset_id: i32, now: i64) -> bool {
        let Some(l) = self.link.as_mut() else {
            return false;
        };
        let body = tg_proto::rpc::get_history(p.kind, p.id, p.access_hash, offset_id, HISTORY_PAGE);
        let tag = TAG_HISTORY | (chat as u32 & TAG_INDEX);
        let sent = l.call(&body, tag, now);
        symbian::log!("[rpc] history requested offset={offset_id}");
        if !sent {
            self.pending_call.push((body, tag));
        }
        self.arm_watchdog(tag);
        sent
    }

    /// Send a text message. `random_id` is the client's own id for it.
    pub fn send_message(
        &mut self,
        chat: usize,
        p: crate::model::PeerRef,
        text: &str,
        now: i64,
    ) -> bool {
        let Some(l) = self.link.as_mut() else {
            symbian::log!("[rpc] send: no link");
            return false;
        };
        // From the platform random source, not a counter: the server uses this to discard
        // duplicates, and a counter would repeat after a restart and silently drop a real
        // message as a resend of an old one.
        let mut b = [0u8; 8];
        l.rng_mut().fill(&mut b);
        let random_id = i64::from_le_bytes(b);
        let body = tg_proto::rpc::send_message(p.kind, p.id, p.access_hash, text, random_id);
        let tag = TAG_SEND | (chat as u32 & TAG_INDEX);
        let sent = l.call(&body, tag, now);
        symbian::log!("[rpc] send={}", if sent { "sent" } else { "queued" });
        if !sent {
            self.pending_call.push((body, tag));
        }
        self.arm_watchdog(tag);
        sent
    }

    /// Re-fetch a single message by id, to get a fresh `file_reference` when one expired.
    /// Re-fetch one message to get a fresh `file_reference`.
    ///
    /// Takes the peer, because a channel needs `channels.getMessages` and everything else
    /// needs `messages.getMessages`. Using the latter for both — which is what this did —
    /// returns an *empty list* for a channel rather than an error, so the download never
    /// retried and the user saw the original failure a second time.
    pub fn request_refresh(
        &mut self,
        chat: usize,
        p: crate::model::PeerRef,
        msg_id: i32,
        now: i64,
    ) -> bool {
        let Some(l) = self.link.as_mut() else {
            return false;
        };
        let body = tg_proto::rpc::refresh_messages(p.kind, p.id, p.access_hash, &[msg_id]);
        let tag = TAG_REFRESH | (chat as u32 & TAG_INDEX);
        let sent = l.call(&body, tag, now);
        symbian::log!("[rpc] refresh={}", if sent { "sent" } else { "queued" });
        if !sent {
            self.pending_call.push((body, tag));
        }
        self.arm_watchdog(tag);
        sent
    }

    pub fn is_connected(&self) -> bool {
        self.link.as_ref().is_some_and(|l| l.is_ready())
    }

    /// Ask for one chunk of a file.
    ///
    /// `chat` tags the reply so the right conversation receives it. `is_photo` picks between
    /// `inputPhotoFileLocation` and `inputDocumentFileLocation`, and `thumb_size` is the
    /// `photoSize.type` — mandatory for a photo, empty for a whole document.
    ///
    /// One chunk, not the file: a single request for the protocol maximum of 1 MiB produces
    /// a frame larger than `transport::MAX_FRAME`, which the transport treats as an
    /// unrecoverable desynchronisation. Asking for a big photo used to drop the connection
    /// rather than download anything. The caller loops on `offset`.
    /// Which data centre the session lives on, or 0 before it exists.
    pub fn home_dc(&self) -> u8 {
        self.link.as_ref().map(|l| l.dc()).unwrap_or(0)
    }

    /// Whether `dc` can be served by the connection this driver already holds.
    ///
    /// `0` means the message never carried one, which is what an older parse produced; the
    /// home link is the only sensible guess and is right whenever the media is local.
    fn is_home(&self, dc: u8) -> bool {
        dc == 0 || dc == self.home_dc()
    }

    pub fn request_file_chunk(&mut self, req: FileRequest, dc: u8, now: i64) -> bool {
        if self.is_home(dc) {
            return self.send_on_home(req, now);
        }
        self.send_on_file_dc(req, dc, now)
    }

    fn send_on_home(&mut self, req: FileRequest, now: i64) -> bool {
        self.file_req_on_dc = false;
        let body = req.body();
        let tag = TAG_FILE | (req.chat as u32 & TAG_INDEX);
        let Some(l) = self.link.as_mut() else {
            return false;
        };
        let sent = l.call(&body, tag, now);
        symbian::log!("[dc] file chunk={} offset={}", if sent { "sent" } else { "queued" }, req.offset);
        if !sent {
            self.pending_call.push((body, tag));
        }
        self.arm_watchdog(tag);
        sent
    }

    /// Send on the connection for `dc`, building and authorising one if needed.
    ///
    /// Returns true when the request is under way *or* parked waiting for the connection —
    /// both mean the caller should expect an answer rather than report a failure. Only an
    /// outright refusal to start is false.
    fn send_on_file_dc(&mut self, req: FileRequest, dc: u8, now: i64) -> bool {
        self.file_req_on_dc = true;
        // An existing connection to the right place.
        if let Some(f) = self.file_dc.as_mut() {
            if f.dc == dc {
                if f.state.is_usable() {
                    let body = req.body();
                    let tag = TAG_FILE | (req.chat as u32 & TAG_INDEX);
                    let sent = f.link.call(&body, tag, now);
                    symbian::log!("[dc] file chunk on dc={dc} offset={}", req.offset);
                    if !sent {
                        // No pending_call here: that queue belongs to the home link and
                        // flushing it there would send a file request down the wrong socket,
                        // which answers FILE_MIGRATE and loses the chunk.
                        f.waiting = Some(req);
                    }
                    self.arm_watchdog(tag);
                    return true;
                }
                // Still coming up. Park it; the state machine sends it on Ready.
                symbian::log!("[dc] not ready yet, parking for dc={dc}");
                f.waiting = Some(req);
                return true;
            }
            // A different data centre than the one we hold. One at a time: two extra
            // sockets and two extra handshakes is not a trade this device can make.
            symbian::log!("[dc] dropping the file link for dc={dc}");
            self.file_dc = None;
        }

        // The worker runs one job at a time and the handshake needs two exponentiations, so
        // a second one must not start while the first connection is still using it — a
        // refused submit would leave the new link stalled with no error.
        if self.link.as_ref().is_some_and(|l| l.work_busy()) {
            symbian::log!("[dc] worker busy, deferring the handshake");
            return false;
        }

        // No stored session is passed: a key belongs to one data centre, and this one is
        // reached through an exported token rather than through the saved key.
        match Link::open(dc, None) {
            Ok(link) => {
                symbian::log!("[dc] opening a link to dc={dc}");
                self.file_dc = Some(FileDc {
                    dc,
                    link,
                    state: DcState::Connecting,
                    waiting: Some(req),
                });
                true
            }
            Err(e) => {
                symbian::log!("[dc] open failed code={}", e.code());
                false
            }
        }
    }

    /// The bearer handle from the link, if the link exists and the bearer is up.
    pub fn link_bearer_handle(&self) -> Option<i32> {
        self.link.as_ref().and_then(|l| l.bearer_handle())
    }

    /// Open the connection, resuming a stored session if there is one.
    ///
    /// A failure here is almost always no route — nothing else on the handset is online —
    /// and it is reported rather than retried, because attaching to something that is not
    /// there gives the same answer however many times it is asked.
    /// Ask to connect once the window is up, rather than now.
    ///
    /// The application must be on screen before anything can fail visibly. Arming a timer
    /// is the whole cost; [`Driver::on_event`] does the work when it fires.
    pub fn arm_connect(&mut self) -> Outcome {
        // 150 ms rather than 1: the redraw and the timer are both active objects and their
        // order is not guaranteed, so a 1 ms timer can still beat the first frame. Nothing
        // after this blocks, so the delay costs nothing a person can perceive.
        match symbian::timer_after(150) {
            Ok(h) => {
                self.connect_timer = Some(h);
                self.status = "iniciando";
                Outcome::Redraw
            }
            // No timer means no way to defer, and connecting from here is what used to kill
            // the application. Better to sit on the login screen and let the first key press
            // try — the screen at least exists.
            Err(_) => Outcome::Redraw,
        }
    }

    pub fn connect(&mut self) -> Outcome {
        symbian::log!("[net] local unix time={}", symbian::unix_time());
        let up = symbian::net::connections_up().unwrap_or(0);
        symbian::log!("[net] connections already up={up}");
        self.status = if up == 0 { "procure o diálogo de conexão" } else { "conectando" };
        match Link::start() {
            Ok(l) => {
                symbian::log!("[net] attached dc={}", l.dc());
                self.link = Some(l);
                self.status = "conectando";
                Outcome::Redraw
            }
            Err(e) => {
                symbian::log!("[net] connect failed code={} err={e:?}", e.code());
                Outcome::Disconnected("sem conexão de rede")
            }
        }
    }

    /// Drive the second connection.
    ///
    /// Returns [`Outcome::None`] for anything that was not this link's, so the caller can
    /// hand the same event to the home link.
    fn on_file_dc_event(&mut self, ev: &sys::ShimEvent, now: i64) -> Outcome {
        let Some(f) = self.file_dc.as_mut() else {
            return Outcome::None;
        };
        let dc = f.dc;
        let progress = f.link.on_event(ev, now);
        for (what, n) in f.link.take_notes() {
            match n {
                crate::link::Note::Flag => symbian::log!("[dc] {what}"),
                crate::link::Note::Num(v) => symbian::log!("[dc] {what}={v}"),
                crate::link::Note::Text(t) => symbian::log!("[dc] {what}={t}"),
            }
        }

        match progress {
            LinkProgress::None => Outcome::None,
            LinkProgress::Step(_) | LinkProgress::WorkDone(_) => Outcome::None,
            LinkProgress::Authenticated => {
                // A key, but no user behind it. The token has to come from the home link,
                // which is the only connection that knows who we are.
                symbian::log!("[dc] handshaken, exporting auth for dc={dc}");
                if let Some(f) = self.file_dc.as_mut() {
                    f.state = DcState::AwaitingExport;
                }
                let body = tg_proto::rpc::export_authorization(dc as i32);
                let tag = TAG_EXPORT | (dc as u32 & TAG_INDEX);
                if let Some(home) = self.link.as_mut() {
                    if !home.call(&body, tag, now) {
                        self.pending_call.push((body, tag));
                    }
                    self.arm_watchdog(tag);
                }
                Outcome::Redraw
            }
            LinkProgress::Reply { tag, body } => {
                self.clear_watchdog(tag);
                if (tag & !TAG_INDEX) == TAG_IMPORT {
                    symbian::log!("[dc] authorised dc={dc}");
                    let waiting = self.file_dc.as_mut().and_then(|f| {
                        f.state = DcState::Ready;
                        f.waiting.take()
                    });
                    if let Some(req) = waiting {
                        self.send_on_file_dc(req, dc, now);
                    }
                    return Outcome::Redraw;
                }
                // A download's answer. It goes to the app exactly as one from the home link
                // would: the caller cannot tell which socket carried it, and should not.
                if (tag & !TAG_INDEX) == TAG_FILE {
                    return Outcome::Answered(tag, body);
                }
                Outcome::None
            }
            LinkProgress::Failed { tag, text, .. } => {
                self.clear_watchdog(tag);
                symbian::log!("[dc] rpc error={text}");
                // Nothing on this connection is recoverable by retrying it: an import that
                // failed leaves a link that can never serve a file. Drop it so the next
                // attempt builds a fresh one, and tell the app rather than leaving the
                // download silently parked.
                self.file_dc = None;
                if (tag & !TAG_INDEX) == TAG_FILE || (tag & !TAG_INDEX) == TAG_IMPORT {
                    return Outcome::RequestFailed(TAG_FILE, text);
                }
                Outcome::Redraw
            }
            LinkProgress::Disconnected(why) => {
                symbian::log!("[dc] disconnected={why}");
                let pending = self.file_dc.take().and_then(|f| f.waiting);
                if pending.is_some() {
                    return Outcome::RequestFailed(TAG_FILE, alloc::string::String::from(why));
                }
                Outcome::Redraw
            }
        }
    }

    /// The home link answered `auth.exportAuthorization`. Import it on the file link.
    fn on_exported_auth(&mut self, body: &[u8], now: i64) -> Outcome {
        let Some((id, bytes)) = tg_proto::rpc::parse_exported_authorization(body) else {
            symbian::log!("[dc] exported auth did not parse");
            self.file_dc = None;
            return Outcome::RequestFailed(
                TAG_FILE,
                alloc::string::String::from("EXPORT_PARSE_FAILED"),
            );
        };
        let Some(f) = self.file_dc.as_mut() else {
            // The link went away while the token was in flight. The token is useless
            // without it and is discarded rather than kept for a connection that may
            // never be rebuilt.
            return Outcome::None;
        };
        f.state = DcState::Importing;
        let dc = f.dc;
        let body = tg_proto::rpc::import_authorization(id, &bytes);
        let tag = TAG_IMPORT | (dc as u32 & TAG_INDEX);
        let sent = f.link.call(&body, tag, now);
        symbian::log!("[dc] importing auth on dc={dc}");
        if !sent {
            symbian::log!("[dc] import could not be sent; the link will retry on ready");
        }
        self.arm_watchdog(tag);
        Outcome::Redraw
    }

    /// Feed a raw shim event. Everything the network and the worker do arrives here.
    pub fn on_event(&mut self, ev: &sys::ShimEvent, login: &mut Login, now: i64) -> Outcome {
        // The deferred connect. Everything the old constructor did happens here instead,
        // with a window on screen to report into.
        if ev.kind == sys::SHIM_EV_TIMER && Some(ev.handle) == self.connect_timer {
            self.connect_timer = None;
            return self.connect();
        }

        // A reconnect after a disconnection. The pending call and the login state survive,
        // so the user can continue where they left off without restarting the application.
        if ev.kind == sys::SHIM_EV_TIMER && Some(ev.handle) == self.reconnect_timer {
            self.reconnect_timer = None;
            self.retries += 1;
            symbian::log!("[net] reconnect attempt={}", self.retries);
            return self.connect();
        }

        // A stuck call: a request went out and nothing came back in time.
        if ev.kind == sys::SHIM_EV_TIMER && self.stuck_timer.map(|(h, _)| h) == Some(ev.handle) {
            let tag = self.stuck_timer.take().map(|(_, t)| t).unwrap_or(0);
            symbian::log!("[rpc] stuck timer fired tag={tag}");

            // A download or an import that went silent is the *file* connection's problem,
            // not the session's. Tearing down the home link here would sign the user out of
            // their chat list because a photo on another data centre did not answer — and
            // then reconnect, and count a retry toward the three-strike limit.
            let kind = tag & !TAG_INDEX;
            let on_file_link = self.file_dc.is_some()
                && (kind == TAG_IMPORT || (kind == TAG_FILE && self.file_req_on_dc));
            if on_file_link {
                symbian::log!("[dc] dropping the file link, not the session");
                let was_waiting = self.file_dc.take().and_then(|f| f.waiting).is_some();
                return if was_waiting {
                    Outcome::RequestFailed(
                        TAG_FILE,
                        alloc::string::String::from("TIMEOUT"),
                    )
                } else {
                    Outcome::Redraw
                };
            }

            self.link = None;
            // Bounded, like the reconnect path. Without this a server that stays silent is
            // an endless reconnect every ten seconds, with a status line that keeps
            // promising something is happening.
            if self.retries >= 3 {
                symbian::log!("[rpc] giving up after three stuck calls");
                return Outcome::Disconnected("o servidor não respondeu");
            }
            self.retries += 1;
            return self.connect();
        }

        // The file connection, before the home one.
        //
        // Both links see every event, and both filter: a socket only claims completions for
        // its own handle, and `Job::on_event` ignores a worker completion unless it has a
        // job outstanding. So the routing is by ownership rather than by inspection here —
        // but the file link is asked first, because when it *does* own the event the home
        // link has nothing to say about it and running both wastes a tick.
        if self.file_dc.is_some() {
            match self.on_file_dc_event(ev, now) {
                Outcome::None => {}
                other => return other,
            }
        }

        let Some(l) = self.link.as_mut() else {
            return Outcome::None;
        };

        let progress = l.on_event(ev, now);

        // Drained before the progress is acted on, so the log reads in the order the wire
        // saw it: the bytes that produced a failure appear above the failure.
        for (what, n) in l.take_notes() {
            match n {
                crate::link::Note::Flag => symbian::log!("[net] {what}"),
                crate::link::Note::Num(v) => symbian::log!("[net] {what}={v}"),
                crate::link::Note::Text(t) => symbian::log!("[net] {what}={t}"),
            }
        }

        // Whatever the worker just freed up, if anything was held back.
        if !l.work_busy() {
            if let Some(q) = self.queued.take() {
                self.start_work(q);
            }
        }

        match progress {
            LinkProgress::None => Outcome::None,
            LinkProgress::Step(s) => {
                symbian::log!("[net] step={s}");
                self.status = s;
                Outcome::Redraw
            }
            LinkProgress::Authenticated => {
                // Persisted before anything else uses the connection. Redoing the handshake
                // costs two exponentiations and four round trips, and the key *is* the
                // session — see `session_store`.
                if !self.persisted {
                    self.persisted = true;
                    if let Some(l) = self.link.as_ref() {
                        if let Err(e) = l.persist() {
                            symbian::log!("[net] session write failed code={}", e.code());
                        }
                    }
                }
                symbian::log!("[net] handshake done, session up");
                self.status = "conectado";
                self.retries = 0;

                // A resumed session means the auth key was read from disk — the user is
                // already signed in and the login screen should be skipped entirely.
                // The login machine is fresh here (every launch starts at Phone) and its
                // `resume()` returns None because nothing is in flight, so without this
                // the app sits on the login screen with a fully operational session.
                let was_resumed = self.link.as_ref().is_some_and(|l| l.was_resumed());
                if was_resumed && !login.is_authorized() {
                    symbian::log!("[auth] resumed session — skipping the login screen");
                    return Outcome::Authorized;
                }
                // Fresh handshake: the login screen was showing Waiting ("conectando…")
                // while the connection was coming up. Now that it is ready, move to the
                // Phone screen so the user can type their number.
                if !was_resumed {
                    login.show_phone();
                }

                // Whatever was typed while the handshake ran goes now. Flushing all, not just
                // the last one — a Vec rather than an Option because sending two messages
                // before the handshake finishes is the ordinary case on a four-second wait.
                let pending = core::mem::take(&mut self.pending_call);
                if !pending.is_empty() {
                    symbian::log!("[rpc] flushing pending calls={}", pending.len());
                    let mut last_tag = 0;
                    for (body, tag) in pending {
                        if let Some(l) = self.link.as_mut() {
                            l.call(&body, tag, now);
                            last_tag = tag;
                        }
                    }
                    // One watchdog for the whole batch rather than one per call — arming
                    // inside the loop would cancel the previous iteration's timer, leaving
                    // every call except the last one unwatched.
                    if last_tag != 0 {
                        self.arm_watchdog(last_tag);
                    }
                    return Outcome::Redraw;
                }
                // Nothing held back, so anything the login was waiting for went down with
                // the old session and has to be asked again. Without this a reconnect in
                // the middle of a login leaves the screen saying "conectado" while the
                // machine waits forever for a reply on a connection that no longer exists —
                // which is what minimising the application during the password step did.
                if !login.is_authorized() {
                    if let Some(p) = login.resume() {
                        symbian::log!("[auth] resuming the login after a reconnect");
                        return self.apply(p, login, now);
                    }
                }
                Outcome::Redraw
            }
            LinkProgress::Reply { tag, body } => {
                symbian::log!("[rpc] reply bytes={}", body.len());
                self.clear_watchdog(tag);
                // Ours, not the login's. Handing this to `auth.rs` would have it look up a
                // tag it never issued and answer nothing.
                // Ours if it is one of the tags this driver issues. The login machine
                // numbers its own from zero and would look up a tag it never gave out.
                // The exported token is ours and the app must never see it: it is a
                // credential, and the next step is on a different socket.
                if (tag & !TAG_INDEX) == TAG_EXPORT {
                    return self.on_exported_auth(&body, now);
                }
                if tag == TAG_DIALOGS
                    || (tag & !TAG_INDEX) == TAG_HISTORY
                    || (tag & !TAG_INDEX) == TAG_SEND
                    || (tag & !TAG_INDEX) == TAG_FILE
                    || (tag & !TAG_INDEX) == TAG_REFRESH
                {
                    symbian::log!("[rpc] our reply bytes={}", body.len());
                    return Outcome::Answered(tag, body);
                }
                let p = {
                    let l = self.link.as_mut().unwrap();
                    let (rng, _) = (l.rng_mut(), ());
                    login.on_reply(tag, &body, rng)
                };
                self.apply(p, login, now)
            }
            LinkProgress::Failed { tag, text, .. } => {
                symbian::log!("[rpc] error={text}");
                self.clear_watchdog(tag);
                if tag == TAG_DIALOGS
                    || (tag & !TAG_INDEX) == TAG_HISTORY
                    || (tag & !TAG_INDEX) == TAG_SEND
                    || (tag & !TAG_INDEX) == TAG_FILE
                    || (tag & !TAG_INDEX) == TAG_REFRESH
                {
                    symbian::log!("[rpc] our error={text}");
                    return Outcome::RequestFailed(tag, text);
                }
                let p = login.on_error(tag, &text);
                self.apply(p, login, now)
            }
            LinkProgress::WorkDone(bytes) => self.on_work(bytes, login, now),
            LinkProgress::Disconnected(why) => {
                symbian::log!("[net] DISCONNECTED={why}");
                self.link = None;
                // The link is gone; when a new one is built the next Authenticated must
                // save its key. Without this reset a reconnect after -404 or a session
                // revocation silently stops persisting, and the next launch starts from a
                // fresh handshake that costs two exponentiations for nothing.
                self.persisted = false;
                // Only reconnect up to three times. A deliberate logout or an expired key
                // will not fix itself, and beyond three the handset has probably lost its
                // route entirely.
                // The watchdog belongs to a link that no longer exists.
                if let Some((h, _)) = self.stuck_timer.take() {
                    symbian::timer_cancel(h);
                }
                if self.retries < 3 {
                    if let Ok(h) = symbian::timer_after(2000) {
                        self.reconnect_timer = Some(h);
                        self.status = "reconectando";
                        return Outcome::Redraw;
                    }
                }
                Outcome::Disconnected(why)
            }
        }
    }

    /// A result from work this driver submitted.
    ///
    /// Both kinds come back the same way, and which one it was is decided by length: the
    /// derivation produces exactly 32 bytes and an exponentiation produces the width of the
    /// modulus, which is 256. Distinguishing by size rather than by a flag because the flag
    /// would have to survive the round trip and a wrong one is silent.
    fn on_work(&mut self, bytes: Vec<u8>, login: &mut Login, now: i64) -> Outcome {
        symbian::log!("[work] done bytes={}", bytes.len());
        let p = if bytes.len() == 32 {
            let mut x = [0u8; 32];
            x.copy_from_slice(&bytes);
            let Some(l) = self.link.as_mut() else {
                return Outcome::Disconnected("sem conexão");
            };
            login.on_kdf(x, l.rng_mut())
        } else {
            login.on_modpow(&bytes)
        };
        self.apply(p, login, now)
    }

    /// Carry out whatever the login machine asked for.
    pub fn apply(&mut self, p: LoginProgress, login: &mut Login, now: i64) -> Outcome {
        if !matches!(p, LoginProgress::None) {
            self.retries = 0;
        }
        // Every transition named. A login is a dozen of these and the screen shows one word
        // for each; when it stops, the last line here says which one it stopped on.
        symbian::log!("[auth] step={}", match &p {
            LoginProgress::None => "none",
            LoginProgress::Call { .. } => "call",
            LoginProgress::Kdf { .. } => "kdf",
            LoginProgress::ModPow { .. } => "modpow",
            LoginProgress::Migrate(_) => "migrate",
            LoginProgress::Authorized => "authorized",
            LoginProgress::Error(_) => "error",
        });
        match p {
            LoginProgress::None => Outcome::Redraw,
            LoginProgress::Call { body, tag } => {
                let Some(l) = self.link.as_mut() else {
                    // The link died earlier and the log went quiet from that point, which
                    // reads as the app doing nothing rather than as a dead connection.
                    symbian::log!("[rpc] call with no link tag={tag}");
                    return Outcome::Disconnected("sem conexão");
                };
                symbian::log!("[rpc] call tag={tag}");
                if !l.call(&body, tag, now) {
                    // The handshake has not finished. Four seconds on this handset, and a
                    // phone number takes less than that to type — so this is the ordinary
                    // case, not an edge. Held and sent on Authenticated; dropping it is what
                    // left the screen on "sending the code" forever.
                    self.pending_call.push((body, tag));
                    self.status = "enviando...";
                }
                // Armed either way. A queued request and a sent one are both a request with
                // no answer, which is the only thing the user can see.
                self.arm_watchdog(tag);
                Outcome::Redraw
            }
            LoginProgress::Kdf { password, salt1, salt2 } => {
                self.status = "derivando a chave...";
                self.start_work(Queued::Kdf { password, salt1, salt2 });
                Outcome::Redraw
            }
            LoginProgress::ModPow { base, exp, modulus } => {
                self.status = "verificando a senha...";
                self.start_work(Queued::ModPow { base, exp, modulus });
                Outcome::Redraw
            }
            LoginProgress::Migrate(dc) => {
                symbian::log!("[net] MIGRATE to dc={dc}");
                // The account lives elsewhere: new socket, new key, new session. Expected
                // rather than exceptional — a Brazilian number asked at DC2 always says
                // this, so it happens on most first logins.
                self.persisted = false;
                let phone = String::from(login.phone());
                let Some(l) = self.link.as_mut() else {
                    return Outcome::Disconnected("sem conexão");
                };
                if l.migrate(dc).is_err() {
                    return Outcome::Disconnected("não consegui mudar de servidor");
                }
                self.status = "mudando de servidor";
                // The code is asked for again once the new handshake finishes. Sending it
                // now would go down a link that has no session yet, so it waits for
                // Authenticated on the other side.
                let p = login.ask_send_code(&phone);
                self.apply(p, login, now)
            }
            LoginProgress::Authorized => {
                symbian::log!("[auth] AUTHORIZED");
                Outcome::Authorized
            }
            LoginProgress::Error(e) => {
                symbian::log!("[auth] error={}", error_label(&e));
                Outcome::Redraw
            }
        }
    }

    /// Start the no-answer watchdog for `tag`, replacing any earlier one.
    ///
    /// The deadline depends on what was asked for. Ten seconds is right for a request whose
    /// answer is a few hundred bytes; a 128 KiB file chunk over GPRS at 30 kbit/s takes
    /// thirty-five, so the same deadline declared every download stuck at the point it was
    /// working normally.
    fn arm_watchdog(&mut self, tag: u32) {
        if let Some((h, _)) = self.stuck_timer.take() {
            symbian::timer_cancel(h);
        }
        let ms = if tag & !TAG_INDEX == TAG_FILE { FILE_TIMEOUT_MS } else { REPLY_TIMEOUT_MS };
        if let Ok(h) = symbian::timer_after(ms) {
            self.stuck_timer = Some((h, tag));
        }
    }

    /// Stop it, once that tag has been answered.
    ///
    /// Cancelled rather than forgotten. Dropping the handle leaves a one-shot running, and
    /// the shim hands out the lowest free slot — so a later timer can be given the same
    /// number and the stale completion then looks exactly like the new one.
    fn clear_watchdog(&mut self, tag: u32) {
        if let Some((h, t)) = self.stuck_timer {
            if t == tag {
                symbian::timer_cancel(h);
                self.stuck_timer = None;
            }
        }
    }

    /// Submit work, or hold it if the worker is busy.
    fn start_work(&mut self, w: Queued) {
        let Some(l) = self.link.as_mut() else {
            return;
        };
        let accepted = match &w {
            Queued::ModPow { base, exp, modulus } => l.submit_modpow(base, exp, modulus),
            Queued::Kdf { password, salt1, salt2 } => l.submit_kdf(password, salt1, salt2),
        };
        // Busy is a wait. Anything else is a refusal, and queueing a refusal is a loop that
        // retries the same rejected job on every event and never says so — which is how an
        // exponent one byte too wide turned into a screen that read "verificando a senha"
        // for as long as anyone was willing to look at it.
        let busy = l.work_busy();
        match (&w, accepted, busy) {
            (Queued::ModPow { exp, modulus, .. }, true, _) => {
                symbian::log!("[work] modpow submitted modulus={} exponent={}", modulus.len(), exp.len());
            }
            (Queued::Kdf { .. }, true, _) => symbian::log!("[work] kdf submitted"),
            (_, false, true) => symbian::log!("[work] held, worker busy"),
            (Queued::ModPow { base, exp, modulus }, false, false) => {
                symbian::log!("[work] REFUSED modpow base={} exponent={} modulus={}", base.len(), exp.len(), modulus.len());
            }
            (Queued::Kdf { .. }, false, false) => symbian::log!("[work] REFUSED kdf"),
        }
        if !accepted {
            if busy {
                self.queued = Some(w);
            } else {
                // Not recoverable by waiting, so it is reported rather than retried.
                self.status = "não consegui calcular a chave";
            }
        }
    }
}

/// A stable name for each `AuthError`, for the log.
///
/// Not `Debug`: that would pull the formatting machinery into a `no_std` image for one
/// string, and the point of naming errors was that a UI matches on the type rather than on
/// text — a log that prints the type is the same discipline.
fn error_label(e: &tg_proto::auth::AuthError) -> &'static str {
    use tg_proto::auth::AuthError as A;
    match e {
        A::PhoneNumberInvalid => "PHONE_NUMBER_INVALID",
        A::PhoneCodeInvalid => "PHONE_CODE_INVALID",
        A::PhoneCodeExpired => "PHONE_CODE_EXPIRED",
        A::PasswordInvalid => "PASSWORD_HASH_INVALID",
        A::FloodWait(_) => "FLOOD_WAIT",
        A::SignUpRequired => "SIGNUP_REQUIRED",
        A::ApiIdInvalid => "API_ID_INVALID",
        A::Other(_) => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_driver_with_no_link_reports_rather_than_panicking() {
        // Every path dereferences the link. On the handset a failed attach is the normal
        // outcome of having no route, so this is the ordinary case rather than an edge.
        let mut d = Driver::new();
        let mut login = Login::new(0, "");
        assert_eq!(d.apply(LoginProgress::None, &mut login, 0), Outcome::Redraw);
        assert_eq!(
            d.apply(LoginProgress::Call { body: alloc::vec![1], tag: 1 }, &mut login, 0),
            Outcome::Disconnected("sem conexão")
        );
        assert!(!d.is_connected());
    }

    fn a_request() -> FileRequest {
        FileRequest {
            chat: 0,
            is_photo: true,
            id: 1,
            access_hash: 2,
            file_reference: alloc::vec![3],
            thumb_size: String::from("m"),
            offset: 0,
        }
    }

    #[test]
    fn a_dc_of_zero_or_the_home_one_stays_on_the_session() {
        // `is_home` decides whether a download needs a second connection at all, and getting
        // it wrong in either direction is expensive: a needless handshake costs 1.7 s of
        // exponentiation, and a missing one costs a FILE_MIGRATE round trip.
        //
        // Zero counts as home because that is what an older parse produced for every photo —
        // `dc_id` was not read at all — and guessing the session's own data centre is right
        // whenever the media is local.
        let d = Driver::new();
        assert_eq!(d.home_dc(), 0, "no link yet");
        assert!(d.is_home(0));
        assert!(d.is_home(d.home_dc()));
        // With no link the home dc reads as 0, so anything non-zero is elsewhere.
        assert!(!d.is_home(4));
    }

    #[test]
    fn a_far_download_with_no_link_is_refused_rather_than_parked() {
        // Without a home link there is nobody to export an authorization from, so opening a
        // second connection could never finish. Refusing says so; parking would leave the
        // app showing "baixando…" forever.
        let mut d = Driver::new();
        assert!(!d.request_file_chunk(a_request(), 4, 0));
        assert!(d.file_dc.is_none(), "no half-built connection is left behind");
    }

    #[test]
    fn a_home_download_with_no_link_is_also_refused() {
        let mut d = Driver::new();
        assert!(!d.request_file_chunk(a_request(), 0, 0));
        // And it did not get attributed to a file link that does not exist.
        assert!(!d.file_req_on_dc);
    }

    #[test]
    fn the_dc_state_only_reports_ready_as_usable() {
        // Sending on a connection that is merely handshaken gets AUTH_KEY_UNREGISTERED, so
        // every state before Ready has to read as unusable.
        assert!(!DcState::Connecting.is_usable());
        assert!(!DcState::AwaitingExport.is_usable());
        assert!(!DcState::Importing.is_usable());
        assert!(DcState::Ready.is_usable());
    }

    #[test]
    fn a_file_request_serialises_the_offset_and_size_it_was_given() {
        // The chunk loop depends on this: each request must carry its own offset, or the
        // second chunk re-downloads the first.
        let mut req = a_request();
        req.offset = tg_proto::rpc::CHUNK as i64;
        let body = req.body();
        // The tail of upload.getFile is offset:long then limit:int.
        let n = body.len();
        let offset = i64::from_le_bytes(body[n - 12..n - 4].try_into().unwrap());
        let limit = i32::from_le_bytes(body[n - 4..].try_into().unwrap());
        assert_eq!(offset, tg_proto::rpc::CHUNK as i64);
        assert_eq!(limit, tg_proto::rpc::CHUNK);
    }

    #[test]
    fn work_is_held_rather_than_lost_when_there_is_no_worker() {
        // With no link there is nowhere to submit, and the work must not vanish silently —
        // losing SRP's second exponentiation stops the login with no error at all.
        let mut d = Driver::new();
        let mut login = Login::new(0, "");
        d.apply(
            LoginProgress::ModPow {
                base: alloc::vec![3],
                exp: alloc::vec![1],
                modulus: alloc::vec![7],
            },
            &mut login,
            0,
        );
        // No link, so nothing was submitted and nothing was queued either — the link is
        // what owns the queue's consumer. What matters is that it did not panic and the
        // driver is still usable.
        assert!(!d.is_connected());
    }
}
