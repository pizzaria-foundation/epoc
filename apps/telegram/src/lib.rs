//! Telegram client for Symbian.
//!
//! Built UI-first: the screens run against [`model::Store::mock`] so layout,
//! scrolling and text entry can be finished before any MTProto exists. When the
//! protocol lands it replaces `Store::mock()` and nothing above it moves — that is
//! the whole reason `model` carries no protocol types.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod driver;
pub mod link;
pub mod login;
pub mod session_store;
pub mod selfcheck;
pub mod chats;
pub mod conv;
pub mod store_cache;
pub mod model;

use alloc::string::String;
use symbian_ui::{Canvas, Handled, KeyEvent, Rect, Theme};

use chats::{ChatList, ChatListAction};
use conv::{ConvAction, Conversation};
use login::{Login, LoginAction};
use model::{Delivery, Message, Store};

/// Which screen is in front. A two-level stack is all this app needs, so it is an
/// enum rather than a `Vec<Box<dyn Screen>>`.
enum Screen {
    Chats(ChatList),
    Conversation(Conversation),
    /// The login screens. The [`Login`] itself lives on [`App`] rather than in here.
    ///
    /// It used to be inside this variant, and `handle_raw` matched on it before doing
    /// anything — so the moment the screen became `Chats` every shim event was ignored.
    /// The connection went deaf at exactly the point it was first useful: nothing could ask
    /// for the chat list, and nothing would have read the answer.
    Login,
    /// Full-screen image viewer, shown after a photo is downloaded and decoded. The index
    /// is the conversation it was opened from: the viewer itself is the SDK's and knows
    /// nothing about chats, and backing out has to return to the transcript rather than to
    /// the chat list, which would lose the reader's place.
    Viewer(symbian_ui::Viewer, usize),
}

pub struct App {
    /// The connection, when this build is driving one. `None` for the mock and the preview,
    /// which draw the same screens with nothing behind them.
    driver: Option<driver::Driver>,
    pub store: Store,
    /// The login machine, alive for the whole run. See [`Screen::Login`].
    login: Login,
    screen: Screen,
    /// Set when the app wants to close, for the shim to act on.
    pub should_exit: bool,
    /// Set while a chat's history is loading; the screen stays on ChatList until the
    /// reply arrives, so the user never sees a conversation with just one message.
    opening_chat: Option<usize>,
    /// Whether the dialog page in flight was asked for from offset zero.
    ///
    /// Decides whether the reply replaces the list or is appended below it. That used to be
    /// decided by "is the list empty", which was right only for as long as the list could
    /// only be empty at the start: a cached list is not empty on the first reply, and
    /// "Atualizar" asks from zero with a full list on screen. Both appended a second copy of
    /// every conversation.
    dialogs_from_top: bool,
    /// The download in flight, if any.
    ///
    /// One request at a time, and everything about it in one place. It used to be a bare
    /// `Option<i32>` holding only the message id, which left the reply path unable to
    /// tell a photo from a voice note — so every download was handed to the image
    /// decoder, and opening a voice message wrote an Ogg file named `_dl.jpg` and
    /// reported that the image decode had failed.
    pending: Option<PendingFile>,
    /// A decode in flight, with the conversation to return to when it finishes. Owns the
    /// downloaded bytes, because the codec reads from them rather than copying, and
    /// closes its decoder slot on drop — the shim has four.
    decoding: Option<(usize, symbian::Decoder<symbian::ShimImages>)>,
    /// Where the running decode is reading from, so a stuck one says which path it took.
    decode_src: &'static str,
    /// Fires if a decode has not reported in [`DECODE_TIMEOUT_MS`], so a stuck one says
    /// what state it is in instead of leaving the screen on "decodificando" forever.
    decode_watchdog: Option<i32>,
}

/// A download in progress, chunk by chunk.
#[derive(Clone, Debug)]
struct PendingFile {
    /// Which message, so `FILE_REFERENCE_EXPIRED` refreshes the right one.
    msg_id: i32,
    /// What the bytes will be, since the reply itself does not say.
    kind: model::MediaKind,
    /// Which data centre holds it. Not necessarily the session's.
    dc: u8,
    /// Everything needed to ask for the next chunk, without going back to the store — which
    /// the user may have scrolled or refreshed in the meantime.
    req: driver::FileRequest,
    /// What has arrived so far. A chunk is 128 KiB and a photo is several, because a single
    /// request for the protocol maximum produces a frame the transport rejects.
    got: alloc::vec::Vec<u8>,
}

impl PendingFile {
    /// Which conversation this belongs to, for routing the reply and for the viewer's way
    /// back out of the photo.
    fn chat(&self) -> usize {
        self.req.chat
    }
}

/// The most a single download may accumulate.
///
/// The heap ceiling is 4 MB and a download holds the assembled file, the chunk it just
/// received and the decoder's own copy at the same time. A photo sized for a 320-pixel
/// screen is tens of kilobytes; anything approaching this is not something this client was
/// going to be able to show.
const MAX_FILE_BYTES: usize = 1024 * 1024;

/// How long a decode may take before it is treated as stuck rather than slow.
///
/// Generous by an order of magnitude: the bytes are already in RAM and the E72's own
/// gallery opens a screen-sized JPEG in a fraction of a second. Anything past this is not
/// a slow codec.
const DECODE_TIMEOUT_MS: i32 = 8_000;

impl App {
    pub fn new(store: Store) -> Self {
        Self {
            driver: None,
            store,
            login: Login::new(0, ""),
            screen: Screen::Chats(ChatList::new()),
            should_exit: false,
            opening_chat: None,
            dialogs_from_top: true,
            pending: None,
            decoding: None,
            decode_src: "",
            decode_watchdog: None,
        }
    }

    pub fn mock() -> Self {
        Self::new(Store::mock())
    }

    /// The login screen, with whatever credentials this build has.
    ///
    /// `link::api_id()` reads what `apps/telegram/api.conf` put there at build time, and is
    /// zero when the file was absent. Passing that through rather than hardcoding it is
    /// what makes `Login::credentials_missing` able to say so on the screen — a login that
    /// cannot succeed should tell the user before they type a phone number, not after they
    /// have waited for `API_ID_INVALID` to come back from Telegram.
    pub fn login() -> Self {
        let mut driver = driver::Driver::new();
        // Whether this build has credentials at all is the first thing to know: without them
        // every login ends in API_ID_INVALID several round trips later, and the log would
        // otherwise show a plausible-looking handshake followed by an unexplained refusal.
        symbian::log!("[net] api_id={} api_hash chars={}", link::api_id(), link::api_hash().len());
        // Nothing that touches the network happens here.
        //
        // It used to: attaching to a bearer, opening a socket and starting to connect, all
        // before the window server had drawn anything. With a route up that was merely bad
        // manners. With no route it was fatal — the socket went onto a connection that had
        // failed to come up, esock panicked the client, and the application disappeared
        // before it had a window to report anything in. "It does not even open when there
        // is no internet" was exactly that.
        //
        // A timer costs microseconds and moves all of it to after the first frame.
        let _ = driver.arm_connect();
        // The chat list from the last launch, before anything touches the network. This is
        // what makes the second opening of the application show chats instead of an empty
        // screen for as long as GPRS takes to answer `getDialogs`.
        let mut store = Store::default();
        if let Some(chats) = store_cache::load_list(&mut symbian::ShimFs) {
            symbian::log!("[store] cached chats={}", chats.len());
            store.chats = chats;
        }
        let mut login = Login::new(link::api_id(), link::api_hash());
        // If this build has no credentials, show the phone screen immediately so the error
        // ("sem api_id…") is visible — there is no network to wait for.
        if !link::has_credentials() {
            login.show_phone();
        }
        Self {
            driver: Some(driver),
            store,
            login,
            screen: Screen::Login,
            should_exit: false,
            opening_chat: None,
            dialogs_from_top: true,
            pending: None,
            decoding: None,
            decode_src: "",
            decode_watchdog: None,
        }
    }

    /// The login screen with no credentials, for the preview and the tests.
    pub fn mock_login() -> Self {
        Self {
            driver: None,
            store: Store::mock(),
            login: Login::new(0, ""),
            screen: Screen::Login,
            should_exit: false,
            opening_chat: None,
            dialogs_from_top: true,
            pending: None,
            decoding: None,
            decode_src: "",
            decode_watchdog: None,
        }
    }

    fn on_key(&mut self, ev: KeyEvent, theme: &Theme<'_>, screen_rect: Rect) -> Handled {
        match &mut self.screen {
            Screen::Login => {
                let login = &mut self.login;
                let (handled, action) = login.handle_key(ev, theme, screen_rect);
                match action {
                    LoginAction::SendCode(number) => {
                        symbian::log!("[act] send code to {}", symbian::log::redact_phone(&number));
                        symbian::log!("ACTION send_code len={}", number.chars().count());
                        let p = login.ask_send_code(&number);
                        if let driver::Outcome::Disconnected(why) =
                            drive(&mut self.driver, p, login)
                        {
                            login.set_error(why);
                        }
                        Handled::Consumed
                    }
                    LoginAction::SubmitCode(code) => {
                        // The length, not the code. A five-digit code in a log is a live
                        // credential for the next few minutes.
                        symbian::log!("[act] submit code digits={}", code.chars().count());
                        let p = login.submit_code(&code);
                        if let driver::Outcome::Disconnected(why) =
                            drive(&mut self.driver, p, login)
                        {
                            login.set_error(why);
                        }
                        Handled::Consumed
                    }
                    LoginAction::SubmitPassword(pw) => {
                        symbian::log!("[act] submit password");
                        let p = login.submit_password(&pw);
                        if let driver::Outcome::Disconnected(why) =
                            drive(&mut self.driver, p, login)
                        {
                            login.set_error(why);
                        }
                        Handled::Consumed
                    }
                    LoginAction::Resend => {
                        symbian::log!("[act] resend");
                        let p = login.ask_resend();
                        if let driver::Outcome::Disconnected(why) =
                            drive(&mut self.driver, p, login)
                        {
                            login.set_error(why);
                        }
                        Handled::Consumed
                    }
                    LoginAction::Back => {
                        // Back on the phone screen means exit.
                        self.should_exit = true;
                        Handled::Consumed
                    }
                    LoginAction::None => handled,
                }
            }
            Screen::Chats(list) => {
                let frame = symbian_ui::Frame::split(screen_rect, theme, true, true);
                let (handled, action) =
                    list.handle_key(ev, &self.store, theme, frame.content.height());
                if handled.is_consumed() && matches!(action, ChatListAction::None) {
                    return handled;
                }
                match action {
                    ChatListAction::Open(i) => {
                        self.open_chat(i);
                        Handled::Consumed
                    }
                    ChatListAction::Exit => {
                        self.should_exit = true;
                        Handled::Consumed
                    }
                    ChatListAction::LoadMore => {
                        self.load_more_dialogs();
                        Handled::Consumed
                    }
                    ChatListAction::Refresh => {
                        self.refresh_dialogs();
                        Handled::Consumed
                    }
                    ChatListAction::None => {
                        // Handled wasn't consumed, try activate for softkeys/Select.
                        match list.activate(ev, &self.store) {
                            ChatListAction::Open(i) => {
                                self.open_chat(i);
                                Handled::Consumed
                            }
                            ChatListAction::Exit => {
                                self.should_exit = true;
                                Handled::Consumed
                            }
                            ChatListAction::Refresh => {
                                self.refresh_dialogs();
                                Handled::Consumed
                            }
                            _ => Handled::Ignored,
                        }
                    }
                }
            }
            Screen::Conversation(conv) => {
                let idx = conv.chat;
                let (handled, action) =
                    conv.handle_key(ev, &self.store.chats[idx], theme, screen_rect);
                // The selection may have moved; a photo far from it does not need its
                // inline JPEG on a 4 MB heap. Once per keypress, not once per frame.
                self.window_previews(idx);
                match action {
                    ConvAction::Back => {
                        // One write per closing, which also catches the messages sent while
                        // it was open — those never pass through a history reply.
                        if let Some(c) = self.store.chats.get(idx) {
                            store_cache::save_tail(&mut symbian::ShimFs, c);
                        }
                        let mut list = ChatList::new();
                        list.state.selected = idx;
                        self.screen = Screen::Chats(list);
                        Handled::Consumed
                    }
                    ConvAction::Send(text) => {
                        self.send(idx, text);
                        Handled::Consumed
                    }
                    ConvAction::LoadMore => {
                        self.load_older(idx);
                        Handled::Consumed
                    }
                    ConvAction::Refresh => {
                        self.refresh_conversation(idx);
                        if let Screen::Conversation(conv) = &mut self.screen {
                            conv.note = Some(self.store.status.clone());
                        }
                        Handled::Consumed
                    }
                    ConvAction::OpenMedia(msg_idx) => {
                        // No note copying here: `download_media` reports through `say`, which
                        // reaches this screen on its own — and, unlike this, keeps reaching it
                        // for every chunk that arrives afterwards.
                        self.download_media(idx, msg_idx);
                        Handled::Consumed
                    }
                    ConvAction::None => handled,
                }
            }
            Screen::Viewer(v, from_chat) => {
                let area = symbian_ui::Viewer::content(screen_rect, theme);
                let (handled, action) = v.handle_key(ev, area);
                match action {
                    symbian_ui::ViewerAction::Back => {
                        // Back to the conversation the photo was opened from. Dropping to
                        // the chat list instead loses the reader's place in a transcript
                        // they may have scrolled a long way into.
                        let chat = *from_chat;
                        self.screen = match self.store.chats.get(chat) {
                            Some(_) => Screen::Conversation(Conversation::new(chat)),
                            None => Screen::Chats(ChatList::new()),
                        };
                        Handled::Consumed
                    }
                    symbian_ui::ViewerAction::None => handled,
                }
            }
        }
    }

    /// Append an outgoing message locally. Real sending is asynchronous, so the
    /// message appears as `Pending` and the transport later promotes it to `Sent`
    /// — the same optimistic path a real client uses.
    /// A reply to something the driver asked for on its own behalf.
    fn on_answer(&mut self, tag: u32, body: &[u8], now: i64) {
        let kind = tag & !driver::TAG_INDEX;
        let index = (tag & driver::TAG_INDEX) as usize;

        if tag == driver::TAG_DIALOGS {
            match tg_proto::chats::parse_dialogs(body) {
                Ok(d) => {
                    if self.dialogs_from_top {
                        // Asked from offset zero: this page *is* the top of the list, so it
                        // replaces what is held — the cached copy from the last launch, or
                        // the one the user pressed "Atualizar" on. Appending would put a
                        // second copy of every conversation below the first.
                        self.dialogs_from_top = false;
                        let status = core::mem::take(&mut self.store.status);
                        self.store = model::store_from_dialogs(&d, now);
                        self.store.status = status;
                    } else {
                        let n = model::merge_dialogs(&mut self.store, &d, now);
                        symbian::log!("[store] dialogs merged new={n}");
                    }
                    symbian::log!("[store] dialogs parsed={}", self.store.chats.len());
                    // The list the next launch opens with.
                    store_cache::save_list(&mut symbian::ShimFs, &self.store);
                }
                Err(_) => {
                    self.store.dialogs_loading = false;
                    symbian::log!("[store] dialogs did not parse");
                }
            }
            return;
        }

        if kind == driver::TAG_HISTORY {
            let Some(peer) = self.store.chats.get(index).and_then(|c| c.peer) else { return };
            match tg_proto::chats::parse_history(body) {
                Ok(page) => {
                    let n = match self.store.chats.get_mut(index) {
                        Some(c) => {
                            let n = model::merge_history(c, &page, peer);
                            // The tail this conversation opens with next time.
                            store_cache::save_tail(&mut symbian::ShimFs, c);
                            n
                        }
                        None => 0,
                    };
                    symbian::log!("[store] history merged new={n}");
                    // If this is the first load for a chat the user just opened, the
                    // screen is still on ChatList. Transition to Conversation now that
                    // enough messages have arrived to fill the viewport.
                    if self.opening_chat == Some(index) {
                        self.opening_chat = None;
                        self.screen = Screen::Conversation(Conversation::new(index));
                    }
                    // More messages were prepended — shift the selection down so the
                    // user stays on the same bubble they were looking at.
                    if n > 0 {
                        if let Screen::Conversation(conv) = &mut self.screen {
                            if conv.chat == index {
                                conv.state.selected += n;
                            }
                        }
                    }
                    if let Screen::Conversation(conv) = &mut self.screen {
                        if conv.chat == index && conv.note.as_deref() == Some("recarregando...") {
                            conv.note = None;
                        }
                    }
                }
                Err(_) => {
                    if let Some(c) = self.store.chats.get_mut(index) {
                        // Not retried: the same bytes would fail the same way, and a chat
                        // that keeps asking is worse than one that stops short.
                        c.loading = false;
                        c.complete = true;
                    }
                    symbian::log!("[store] history did not parse");
                }
            }
            return;
        }

        if kind == driver::TAG_REFRESH {
            // The reply carries the single message with a fresh file_reference.
            if let Some(refreshed) = tg_proto::chats::parse_history(body).ok()
                .and_then(|h| h.messages.first().cloned())
            {
                let msg_id = refreshed.id;
                if let Some(c) = self.store.chats.get_mut(index) {
                    model::refresh_media(c, msg_id, &refreshed);
                }
                symbian::log!("[media] file reference refreshed");
            } else {
                symbian::log!("[media] refresh parse failed");
            }
            return;
        }

        if kind == driver::TAG_SEND {
            // The answer is an Updates carrying the accepted message. What matters to the
            // screen is that the server took it, so the last pending one is promoted.
            if let Some(c) = self.store.chats.get_mut(index) {
                if let Some(m) = c.messages.iter_mut().rev().find(|m| m.state == Delivery::Pending) {
                    m.state = Delivery::Sent;
                }
            }
            symbian::log!("[store] message accepted by the server");
        }

        if kind == driver::TAG_FILE {
            // The reply is an upload.File carrying the downloaded bytes.
            self.show_downloaded_file(body);
        }
    }

    /// Open a conversation, from the disk if it has been read before.
    ///
    /// The wait this removes is the one the user actually feels. Without a cached tail the
    /// screen has to stay on the chat list until `getHistory` answers — otherwise the
    /// conversation flashes as a single bubble for as long as GPRS takes — so opening a
    /// chat means staring at the list for several seconds. With one, the transcript is
    /// there immediately and the request behind it is an update.
    fn open_chat(&mut self, i: usize) {
        // Opening a chat clears its unread marker, as it would once messages.readHistory
        // is wired up.
        if let Some(c) = self.store.chats.get_mut(i) {
            c.unread = 0;
        }
        let restored = match self.store.chats.get_mut(i) {
            Some(c) => store_cache::load_tail(&mut symbian::ShimFs, c),
            None => return,
        };
        if self.driver.is_some() && !restored {
            self.opening_chat = Some(i);
        } else {
            self.screen = Screen::Conversation(Conversation::new(i));
        }
        if restored {
            // From the top, not from `oldest`: what a restored conversation is missing is
            // whatever arrived since it was written, which is below it rather than above.
            // `merge_history` places the reply by id and drops what is already held.
            self.refresh_conversation(i);
        } else {
            self.load_older(i);
        }
    }

    /// Hold only the inline previews near where the user is looking in `chat`.
    ///
    /// Does nothing unless that chat is the one on screen — the band is defined by the
    /// selected bubble, and a conversation nobody is reading has no selection to centre on.
    fn window_previews(&mut self, chat: usize) {
        let selected = match &self.screen {
            Screen::Conversation(c) if c.chat == chat => c.state.selected,
            _ => return,
        };
        if let Some(c) = self.store.chats.get_mut(chat) {
            model::window_previews(c, selected, &mut symbian::ShimFs);
        }
    }

    /// Say something to the user, wherever they are looking.
    ///
    /// The status line lives on the chat list's title bar; a conversation has its own note.
    /// Setting only the first is why a download reported nothing at all: the progress went to
    /// a screen that was not on top, and the transcript the user was staring at said
    /// "abrindo…" until the picture appeared or did not.
    fn say(&mut self, text: String) {
        if let Screen::Conversation(conv) = &mut self.screen {
            conv.note = Some(text.clone());
        }
        self.store.status = text;
    }

    /// Ask for the page of dialogs below what the list already holds.
    ///
    /// Does nothing when a page is already in flight or every dialog is already here.
    fn load_more_dialogs(&mut self) {
        if self.store.dialogs_loading || self.store.dialogs_complete {
            return;
        }
        self.store.dialogs_loading = true;
        // A page from an offset, so its reply goes below what is held.
        self.dialogs_from_top = false;
        let date = self.store.dialog_offset_date;
        let id = self.store.dialog_offset_id;
        let peer = self.store.dialog_offset_peer;
        let now = symbian::unix_time();
        if let Some(d) = self.driver.as_mut() {
            if !d.request_dialogs(now, date, id, peer) {
                // Queued rather than sent is still in flight.
            }
        } else {
            self.store.dialogs_loading = false;
            self.store.dialogs_complete = true;
        }
    }

    /// Re-fetch the dialog list from the top.
    ///
    /// There is no push in this client: no `updates` subscription, no long poll. So the chat
    /// list is exactly as fresh as the last request, and a message that arrived since is
    /// invisible with no way for the user to find out. Without this the only remedy is
    /// closing the application and starting it again.
    ///
    /// Unlike [`Self::load_more_dialogs`] this asks from offset zero and replaces what is
    /// held, rather than appending a page below it.
    fn refresh_dialogs(&mut self) {
        if self.store.dialogs_loading {
            self.store.status = String::from("ja atualizando...");
            return;
        }
        let Some(d) = self.driver.as_mut() else {
            self.store.status = String::from("sem conexao");
            return;
        };
        let now = symbian::unix_time();
        // Offset zero: the newest page. `dialogs_complete` is cleared because the list is
        // about to be rebuilt and whatever was known about its end no longer applies.
        let sent = d.request_dialogs(now, 0, 0, None);
        self.store.dialogs_loading = true;
        self.store.dialogs_complete = false;
        self.dialogs_from_top = true;
        self.store.status = if sent {
            String::from("atualizando...")
        } else {
            String::from("atualizacao na fila")
        };
    }

    /// Re-fetch the newest page of one conversation, for the same reason.
    fn refresh_conversation(&mut self, chat: usize) {
        let Some(c) = self.store.chats.get(chat) else { return };
        if c.loading {
            self.store.status = String::from("ja atualizando...");
            return;
        }
        let Some(peer) = c.peer else {
            self.store.status = String::from("sem peer");
            return;
        };
        let Some(d) = self.driver.as_mut() else {
            self.store.status = String::from("sem conexao");
            return;
        };
        let now = symbian::unix_time();
        // Offset zero rather than `c.oldest`: this is "what is new", not "what is above".
        let sent = d.request_history(chat, peer, 0, now);
        if let Some(c) = self.store.chats.get_mut(chat) {
            c.loading = true;
        }
        self.store.status = if sent {
            String::from("atualizando...")
        } else {
            String::from("atualizacao na fila")
        };
    }

    /// Ask for the page above what this chat already holds.
    ///
    /// Does nothing when a page is already in flight or the conversation has no more —
    /// scrolling reaches the top repeatedly and each arrival must not start another round
    /// trip for the same bytes.
    fn load_older(&mut self, chat: usize) {
        let Some(c) = self.store.chats.get_mut(chat) else { return };
        // `windowed` alongside `complete`: the server has more, but the window is full and
        // whatever arrived would be trimmed off on the way in.
        if c.loading || c.complete || c.windowed {
            return;
        }
        let Some(peer) = c.peer else { return };
        let offset = c.oldest;
        c.loading = true;
        let now = symbian::unix_time();
        if let Some(d) = self.driver.as_mut() {
            if !d.request_history(chat, peer, offset, now) {
                // Queued rather than sent is still in flight; only an outright refusal
                // leaves the flag lying.
            }
        } else if let Some(c) = self.store.chats.get_mut(chat) {
            // No connection behind this build — the preview and the tests.
            c.loading = false;
            c.complete = true;
        }
    }

    fn send(&mut self, chat: usize, text: String) {
        // On the wire first, so a failure to send shows as a failed message rather than as
        // one that sits on "pending" with nothing behind it.
        let peer = self.store.chats.get(chat).and_then(|c| c.peer);
        let now = symbian::unix_time();
        let accepted = match (peer, self.driver.as_mut()) {
            (Some(p), Some(d)) => d.send_message(chat, p, &text, now),
            // The mock and the preview have no server; the message stays local and pending,
            // which is what those builds are for.
            _ => false,
        };

        let c = &mut self.store.chats[chat];
        // No driver = mock or preview, every message stays Pending — there is no server.
        let state = if self.driver.is_none() {
            Delivery::Pending
        } else if accepted {
            Delivery::Pending
        } else {
            Delivery::Failed
        };
        // Through `push_message` rather than straight onto the Vec, so the hundredth
        // message sent in one sitting drops the oldest instead of growing the transcript.
        // The screen below rebuilds and jumps to the end, so nothing here tracks an index
        // that the drop would invalidate.
        model::push_message(
            c,
            Message {
                id: 0,
                text,
                outgoing: true,
                time: model::hhmm(now),
                state,
                media: None,
            },
        );
        if !accepted && self.driver.is_some() {
            symbian::log!("[store] send refused by the driver");
        }
        c.last_outgoing = true;
        if let Screen::Conversation(conv) = &mut self.screen {
            // Force a re-wrap and jump to the new message.
            *conv = {
                let mut fresh = Conversation::new(chat);
                fresh.focus = conv::Focus::Composer;
                fresh
            };
        }
    }

    /// Re-fetch a single message to get a fresh file_reference.
    fn refresh_message(&mut self, chat_idx: usize, msg_id: i32) {
        let Some(peer) = self.store.chats.get(chat_idx).and_then(|c| c.peer) else {
            self.store.status = String::from("refresh: sem peer");
            return;
        };
        if let Some(d) = self.driver.as_mut() {
            let _ = d.request_refresh(chat_idx, peer, msg_id, symbian::unix_time());
        }
    }

    /// The user pressed Select on a media row.
    ///
    /// Downloads happen here and nowhere else. Nothing is fetched when a message arrives, is
    /// scrolled past or is drawn — the link is GPRS and the person holding the phone pays by
    /// the kilobyte, so scrolling a chat full of photos must cost nothing.
    fn download_media(&mut self, chat: usize, msg_idx: usize) {
        let media = match self.store.chats.get(chat).and_then(|c| c.messages.get(msg_idx)).and_then(|m| m.media.clone()) {
            Some(m) => m,
            None => {
                self.say(String::from("download: sem media"));
                return;
            }
        };

        // Select on a photo or a sticker that brought a preview inside the message decodes
        // *that*, here, with no round trip. This is the only place a preview is ever
        // decoded: drawing it in the transcript would mean deciding to load on the user's
        // behalf, and the whole point is that loading waits for them.
        if let Some(bytes) = media.preview() {
            let bytes = bytes.to_vec();
            symbian::log!("[media] preview: decoding inline bytes");
            self.start_photo_decode(chat, None, bytes);
            return;
        }

        // A sticker with no usable preview is not downloaded. The file is WebP, or gzipped
        // Lottie, or VP9, and this handset has a plugin for none of the three — fetching it
        // would spend the user's data to arrive at the same placeholder it already shows.
        if let model::Media::Sticker { .. } = &media {
            symbian::log!("[media] sticker: no decodable preview");
            self.say(String::from("sticker: formato WebP, sem decoder"));
            return;
        }

        let kind = match media.kind() {
            Some(k) => k,
            None => {
                self.say(String::from("download: nada para baixar"));
                return;
            }
        };

        // Already on disk from a previous opening. Checked before anything touches the
        // network, because the point of the cache is the link and not the disk: this runs
        // over GPRS, metered by the kilobyte, and backing out of a photo and opening it
        // again should not pay for it twice.
        //
        // A hit needs no `file_reference` either, so it cannot fail with
        // FILE_REFERENCE_EXPIRED the way a fresh fetch can once a day.
        if let Some(cached) = symbian::cache::get(&mut symbian::ShimFs, media.file_id()) {
            symbian::log!("[media] cache hit");
            match kind {
                model::MediaKind::Photo => {
                    self.say(String::from("do cache"));
                    self.start_photo_decode(chat, Some(media.file_id()), cached);
                }
                model::MediaKind::Voice | model::MediaKind::Audio => {
                    self.say(String::from("audio: sem suporte ainda"));
                }
                model::MediaKind::File => {
                    self.say(alloc::format!("arquivo: {} KB, no cache", cached.len() / 1024));
                }
            }
            return;
        }

        let (id, access_hash, file_reference) = match &media {
            model::Media::Photo { id, access_hash, file_reference, .. }
            | model::Media::Voice { id, access_hash, file_reference, .. }
            | model::Media::Audio { id, access_hash, file_reference, .. }
            | model::Media::File { id, access_hash, file_reference, .. } => {
                (*id, *access_hash, file_reference.clone())
            }
            model::Media::Sticker { .. } | model::Media::Unknown => {
                unreachable!("both already returned above")
            }
        };
        let thumb_size = match &media {
            model::Media::Photo { thumb_size, .. } => thumb_size.clone(),
            // Empty means the document itself rather than one of its thumbnails.
            _ => String::new(),
        };

        // One at a time: the reply carries only bytes and a tag that identifies the chat,
        // so a second request in flight would answer into the first one's bookkeeping and
        // the two would be indistinguishable when they came back.
        if self.pending.is_some() {
            self.say(String::from("download: aguarde o anterior"));
            return;
        }

        let now = symbian::unix_time();
        let msg_id = self.store.chats.get(chat)
            .and_then(|c| c.messages.get(msg_idx))
            .map(|m| m.id).unwrap_or(0);
        let is_photo = kind == model::MediaKind::Photo;
        let Some(d) = self.driver.as_mut() else {
            self.say(String::from("download: sem driver"));
            return;
        };
        let dc = media.dc_id() as u8;
        symbian::log!("[media] req file chat={chat} id={id} dc={dc}");
        let req = driver::FileRequest {
            chat,
            is_photo,
            id,
            access_hash,
            file_reference,
            thumb_size,
            offset: 0,
        };
        let sent = d.request_file_chunk(req.clone(), dc, now);
        if sent {
            // The status says what is happening, and reaching another data centre takes a
            // handshake — two 815 ms exponentiations plus round trips — so it is worth
            // saying so rather than leaving "baixando…" up for five seconds.
            let far = dc != 0 && dc != d.home_dc();
            self.pending = Some(PendingFile { msg_id, kind, dc, req, got: alloc::vec::Vec::new() });
            self.store.status = if far {
                String::from("conectando ao servidor da midia...")
            } else {
                String::from("baixando...")
            };
        } else {
            self.say(String::from("download: fila (link ocupado)"));
        }
    }

    /// The download came back. What happens next depends on what was asked for, which is
    /// why the kind was remembered when the request went out.
    fn show_downloaded_file(&mut self, body: &[u8]) {
        let pending = self.pending.take();
        symbian::log!("[media] file_dl reply received");

        let chunk = match tg_proto::rpc::parse_file(body) {
            Some(b) => b,
            None => {
                symbian::log!("[media] file_dl parse failed");
                self.say(String::from("download: parse falhou"));
                return;
            }
        };

        let Some(mut p) = pending else {
            self.say(String::from("download: sem pedido correspondente"));
            return;
        };

        // A short chunk is the end of the file: the server returns what is left and no more.
        // A full one means there is probably another, so ask for it — this is the loop the
        // previous version did not have, which is why anything past the first chunk arrived
        // truncated and no codec would open it.
        let was_full = chunk.len() == tg_proto::rpc::CHUNK as usize;
        p.got.extend_from_slice(&chunk);

        if p.got.len() > MAX_FILE_BYTES {
            symbian::log!("[media] file_dl over the size cap");
            self.say(alloc::format!("arquivo grande demais ({} KB)", p.got.len() / 1024));
            return;
        }

        if was_full {
            // Ask for the next one from where this ended. Same connection: the request
            // carries the same dc, so it routes back to whichever link served the first
            // chunk rather than starting over on the home one.
            p.req.offset = p.got.len() as i64;
            let now = symbian::unix_time();
            let (dc, req) = (p.dc, p.req.clone());
            let sent = match self.driver.as_mut() {
                Some(d) => d.request_file_chunk(req, dc, now),
                None => false,
            };
            if sent {
                self.say(alloc::format!("baixando {} KB...", p.got.len() / 1024));
                self.pending = Some(p);
            } else {
                self.say(String::from("download: interrompido"));
            }
            return;
        }

        let chat = p.chat();
        let data = p.got;

        // Photos are cached by `start_photo_decode`, which needs the file on disk to decode
        // from; everything else is cached here so the next opening costs nothing on the
        // wire. The result is ignored either way: the bytes are in hand, and reporting
        // "could not cache" over media the user is already looking at would be noise.
        if p.kind != model::MediaKind::Photo {
            symbian::cache::put(&mut symbian::ShimFs, p.req.id, &data);
        }

        match p.kind {
            model::MediaKind::Photo => self.start_photo_decode(chat, Some(p.req.id), data),
            // Voice is Ogg/Opus and the handset's codec list stops at AMR, AAC and MP3 —
            // Opus is four years younger than the phone. Saying so is better than handing
            // the bytes to the image decoder and reporting its failure, which is what this
            // did and which blamed the wrong subsystem entirely.
            model::MediaKind::Voice | model::MediaKind::Audio => {
                symbian::log!("[media] file_dl audio, no decoder yet");
                self.say(String::from("audio: sem suporte ainda"));
            }
            model::MediaKind::File => {
                let kb = data.len() / 1024;
                self.say(alloc::format!("arquivo: {kb} KB, sem visualizador"));
            }
        }
    }

    /// Hand the bytes to the device's codec and wait for `SHIM_EV_IMAGE_DONE`.
    ///
    /// Decoding from memory rather than through a file: the previous version wrote the
    /// photo to `media_dl.jpg` purely so the codec could read it back, which is two
    /// passes over a megabyte of flash for nothing. And it is asynchronous because it
    /// must be — see the note in `shim/src/shim_image.cpp` about why waiting for the
    /// result on this thread freezes the phone rather than merely slowing it.
    /// A one-line description of what is about to be handed to the codec.
    ///
    /// Two fixes aimed at the codec changed nothing, which means the question "is the codec
    /// misbehaving" was the wrong one to keep asking. This answers the question underneath
    /// it: are these bytes a whole JPEG?
    ///
    /// `FFD8` starts one and `FFD9` ends one. A file with the first and not the second is a
    /// truncated download — and a decoder handed a truncated image is entitled to sit
    /// waiting for the rest of it, which is exactly the symptom. A file with neither is not
    /// a JPEG at all, which would point back at the transport.
    fn describe_bytes(data: &[u8]) -> String {
        let n = data.len();
        let head = data.first().zip(data.get(1)).map(|(a, b)| ((*a as u16) << 8) | *b as u16);
        let tail = if n >= 2 {
            Some(((data[n - 2] as u16) << 8) | data[n - 1] as u16)
        } else {
            None
        };
        let soi = head == Some(0xFFD8);
        let eoi = tail == Some(0xFFD9);
        alloc::format!(
            "{}B {}{}",
            n,
            if soi { "SOI" } else { "no-SOI" },
            if eoi { "+EOI" } else { "+NO-EOI" },
        )
    }

    /// Decode and show a photo.
    ///
    /// `cache_id` is the file id when these bytes are the whole photo, and `None` when they
    /// are an inline preview — which must not be stored under the photo's own key, or the
    /// cache would serve a 90-pixel thumbnail forever after.
    fn start_photo_decode(
        &mut self,
        chat: usize,
        cache_id: Option<i64>,
        data: alloc::vec::Vec<u8>,
    ) {
        // Dropping any previous decoder first releases its slot; the shim has four, and a
        // reader opening one photo after another would otherwise reach the limit and be
        // told the decoder was busy.
        self.decoding = None;

        let shape = Self::describe_bytes(&data);
        symbian::log!("[media] decoding={shape}");

        // Decode from a file, and the file is the media cache in this app's own data cage.
        //
        // It used to write every photo to `C:\Data\imgprobe-input.jpg` and decode from
        // there, because that is the exact path and configuration `examples/imgprobe`
        // measured at 241 ms and writing there also handed the probe a real image to chew
        // on — the data cage is per-UID, and reading another application's needs `AllFiles`,
        // which an unsigned package cannot have.
        //
        // That served its purpose and then outlived it. What it cost: a file named after a
        // diagnostic sitting in the user's own Data folder, visible in File Manager and over
        // USB, plus a second full-size write of every photo onto slow storage — for a copy
        // nothing read.
        //
        // The cage path is not new code, which is the whole reason this is a safe removal: it
        // is the branch that already ran whenever the `C:\Data` write failed, which is every
        // launch of a build without `WriteUserData`. Same shim, same codec, same
        // configuration — only the directory differs, and `RFile` does not care which one it
        // opens.
        //
        // The cache write is now the *only* write, so it is no longer "for next time" — it is
        // how this time works. A photo with no cache id therefore cannot be decoded at all,
        // and says so below rather than silently falling back to `DataNewL`.
        let mut fs = symbian::ShimFs;
        let mut source: Option<(symbian::fs::Utf16Path, &'static str)> = None;
        if let Some(id) = cache_id {
            if symbian::cache::put_result(&mut fs, id, &data).is_ok() {
                if let Some(p) = symbian::cache::path(&mut fs, id) {
                    source = Some((p, "cage"));
                }
            }
        }

        let (max_w, max_h) = Self::viewer_box();
        let Some((path, src_label)) = source else {
            // Deliberately not falling back to decoding from memory.
            //
            // `DataNewL` is the one thing this shim does that no Symbian example does, and
            // it is untested on this handset — so a silent fallback to it would reintroduce
            // exactly the ambiguity that made the last five rounds unreadable: a failure
            // whose cause could be the codec or could be the path not taken.
            symbian::log!("[media] decode: nowhere to write the image");
            self.say(String::from("decode: sem arquivo para decodificar"));
            return;
        };
        let started = symbian::Decoder::file(symbian::ShimImages, &path, max_w, max_h);
        match started {
            Ok(mut d) => {
                symbian::log!("[media] file_dl decode started");
                // What the decoder made of the image, recorded now rather than on
                // completion — because a decode that never completes is exactly the case
                // that needs explaining, and by then there is nothing left to ask.
                if let Some(p) = d.progress() {
                    symbian::log!(
                        "[media] decode frames={} native={}x{} reduction={} bitmap={}x{} active={} mode={} flags={}",
                        p.frames, p.native_w, p.native_h, p.factor, p.out_w, p.out_h,
                        p.active, p.mode, p.frame_flags,
                    );
                }
                // The byte shape goes on the screen, not just in the log: the log needs a
                // host to read it and the person holding the phone does not have one.
                self.say(alloc::format!("dec {shape} src={src_label}"));
                self.decode_src = src_label;
                self.decoding = Some((chat, d));
                // A decode that is still outstanding after this is not slow, it is stuck —
                // the whole image is in memory and the E72 decodes a screen-sized JPEG in
                // well under a second. The timer turns "decodificando" forever into a line
                // that says which stage it died at.
                self.decode_watchdog = symbian::timer_after(DECODE_TIMEOUT_MS).ok();
            }
            Err(e) => {
                symbian::log!("[media] file_dl decode refused");
                self.say(alloc::format!("decode recusou: {}", e.code()));
            }
        }
    }

    /// The box a full-screen image is decoded to fit.
    ///
    /// The E72's screen. A constant for now, and wrong as an assumption the moment a
    /// second handset appears — the shim reports the real size and this should ask it.
    fn viewer_box() -> (i32, i32) {
        (320, 240)
    }

    /// A decode finished. Only called for an event whose handle matches.
    fn on_image_decoded(&mut self, status: i32) {
        // Cancelled rather than forgotten: the shim hands out the lowest free timer slot,
        // so a stale one-shot can be given the same handle and its completion would then
        // look exactly like this decode's watchdog firing.
        if let Some(h) = self.decode_watchdog.take() {
            symbian::timer_cancel(h);
        }
        let Some((chat, mut d)) = self.decoding.take() else {
            return;
        };
        if status != symbian_sys::SHIM_OK {
            symbian::log!("[media] decode failed");
            self.say(alloc::format!("decode falhou: {status}"));
            return;
        }
        match d.take() {
            Ok(img) => {
                symbian::log!("[media] decode ok, opening viewer");
                self.store.status = String::new();
                // The codec reduces by powers of two, so what came back fits inside the
                // box but rarely fills it. The exact fit is ours.
                let (max_w, max_h) = Self::viewer_box();
                let (w, h) = symbian::image::fit(img.width, img.height, max_w, max_h);
                let shown = if (w, h) == (img.width, img.height) {
                    img
                } else {
                    symbian::image::resample(&img, w, h)
                };
                let size = symbian_ui::Size::new(shown.width, shown.height);
                self.screen = Screen::Viewer(symbian_ui::Viewer::new(shown.pixels, size), chat);
            }
            Err(e) => {
                symbian::log!("[media] decode result unavailable");
                self.say(alloc::format!("decode sem pixels: {}", e.code()));
            }
        }
    }

    fn paint(&mut self, c: &mut Canvas<'_>, theme: &Theme<'_>) {
        match &mut self.screen {
            Screen::Login => self.login.draw(c, theme),
            Screen::Chats(list) => list.draw(c, &self.store, theme),
            Screen::Conversation(conv) => {
                let idx = conv.chat;
                // Split the borrow: the screen needs &mut, the chat needs &.
                let chat = self.store.chats[idx].clone();
                conv.draw(c, &chat, theme);
            }
            // The strings are the app's: the SDK's viewer ships no text.
            Screen::Viewer(v, _) => v.draw(c, theme, "Foto", "Voltar"),
        }
    }

    /// Which screen is showing, for tests and for the shim's title handling.
    pub fn in_conversation(&self) -> Option<usize> {
        match &self.screen {
            Screen::Conversation(c) => Some(c.chat),
            _ => None,
        }
    }

    /// Whether the login screen is showing and authorized.
    pub fn login_authorized(&self) -> bool {
        match &self.screen {
            Screen::Login => self.login.is_authorized(),
            _ => false,
        }
    }
}

/// Hand a login action to the connection, if this build has one.
///
/// A free function because both borrows are of `App` fields and the borrow checker will not
/// take `self.driver` and `self.screen` at once through methods.
/// Hand a login action to the connection and act on what comes back.
///
/// The return value used to be discarded, which is the same mistake the login screens made
/// before review: a `Disconnected` from a key press vanished, and the screen sat on
/// "sending the code" with nothing in flight and nothing to say so.
fn drive(
    driver: &mut Option<driver::Driver>,
    p: login::Progress,
    login: &mut Login,
) -> driver::Outcome {
    let now = symbian::unix_time();
    match driver {
        Some(d) => d.apply(p, login, now),
        // The preview and the tests draw the same screens with nothing behind them, and a
        // key press there should do nothing rather than pretend.
        None => driver::Outcome::None,
    }
}

/// The SDK's application contract. Everything that runs this app — the device entry
/// points and the host simulator — goes through here, so neither needs to know the
/// concrete type.
impl symbian_ui::App for App {
    fn handle_key(&mut self, ev: KeyEvent, theme: &Theme<'_>, screen: Rect) -> Handled {
        self.on_key(ev, theme, screen)
    }

    /// Everything the network and the worker thread do arrives here.
    ///
    /// The shim delivers socket completions, timer ticks and worker results as raw events;
    /// `Driver` turns them into progress and this decides what the screen becomes. Returning
    /// `Ignored` for events nobody claimed lets the toolkit handle its own.
    fn handle_raw(&mut self, ev: &symbian_ui::RawEvent) -> Handled {
        let now = symbian::unix_time();

        // The dev bridge runs on its own sockets, independent of the driver's, and
        // does nothing at all in a build without the `dev-bridge` feature.
        if symbian_app::devbridge::on_event(ev) {
            self.should_exit = true;
            return Handled::Consumed;
        }

        // Before the driver, which knows about sockets and nothing about codecs. Matched
        // on the handle rather than the kind alone, so a completion from a decode this
        // app already abandoned is discarded instead of opening a viewer over whatever
        // the user navigated to since.
        // A decode that has not reported in eight seconds. The image is already in RAM, so
        // this is not slowness — it is a request that will never complete, and the only
        // evidence about why lives inside the shim until something asks for it.
        if ev.kind == symbian_sys::SHIM_EV_TIMER && Some(ev.handle) == self.decode_watchdog {
            self.decode_watchdog = None;
            let p = self.decoding.as_mut().and_then(|(_, d)| d.progress());
            match p {
                Some(p) => {
                    symbian::log!(
                        "[media] DECODE STUCK frames={} native={}x{} reduction={} bitmap={}x{} \
still active={} mode={} flags={} done={} error={}",
                        p.frames, p.native_w, p.native_h, p.factor, p.out_w, p.out_h,
                        p.active, p.mode, p.frame_flags, p.done, p.error,
                    );

                    // On the screen too, because the log needs a host to read it and the
                    // person holding the phone does not have one.
                    self.say(alloc::format!(
                        "travou {} {}x{} f{} r{} a{} m{} fl{:x} c{}",
                        self.decode_src, p.native_w, p.native_h, p.frames, p.factor,
                        p.active as i32, p.mode, p.frame_flags, p.continues
                    ));
                }
                None => self.say(String::from("travou: decoder sumiu")),
            }
            // Dropped, so the slot goes back and the next attempt is not refused for want
            // of one. There are four.
            self.decoding = None;
            return Handled::Consumed;
        }

        if ev.kind == symbian_sys::SHIM_EV_IMAGE_DONE {
            let mine = self.decoding.as_ref().is_some_and(|(_, d)| d.owns(ev.handle));
            if mine {
                self.on_image_decoded(ev.status);
                return Handled::Consumed;
            }
            return Handled::Ignored;
        }

        // Keys the toolkit will not turn into a character, recorded before anything else
        // sees them.
        //
        // The Brazilian E72 has a Ç key and it produces nothing, and the atlas is not the
        // reason — it carries every Latin-1 codepoint including U+00E7. So either the
        // window server sends something other than the character, or it sends a scan code
        // with no code at all, and guessing between those is what the key probe exists to
        // avoid. One press with this build says which.
        //
        // Only the unusual ones: an ASCII character key would fill the log with the phone
        // number someone is typing.
        let interesting = match ev.kind {
            symbian_sys::SHIM_EV_KEY_CHAR => !(0x20..0x7f).contains(&ev.a),
            symbian_sys::SHIM_EV_KEY_DOWN => !matches!(
                ev.a,
                symbian_sys::key::UP
                    | symbian_sys::key::DOWN
                    | symbian_sys::key::LEFT
                    | symbian_sys::key::RIGHT
                    | symbian_sys::key::SELECT
                    | symbian_sys::key::SOFT_LEFT
                    | symbian_sys::key::SOFT_MIDDLE
                    | symbian_sys::key::SOFT_RIGHT
                    | symbian_sys::key::BACKSPACE
                    | symbian_sys::key::ENTER
            ),
            _ => false,
        };
        if interesting {
            symbian::log!(
                "[ui] {}={} scan={} modifiers={}",
                if ev.kind == symbian_sys::SHIM_EV_KEY_CHAR { "key char" } else { "key down" },
                ev.a, ev.d, ev.native,
            );
        }


        // The driver runs for the whole session, not only while the login screen is up.
        // Gating this on the screen is what made the connection go deaf the moment someone
        // signed in.
        let Some(d) = self.driver.as_mut() else {
            return Handled::Ignored;
        };

        let outcome = d.on_event(ev, &mut self.login, now);
        self.login.set_status(d.status);
        self.login.connected = d.is_connected();
        match outcome {
            driver::Outcome::Redraw => Handled::Consumed,
            driver::Outcome::Authorized => {
                self.screen = Screen::Chats(ChatList::new());
                // The list shows the cached one until this comes back, and nothing at all if
                // there is no cache. Asked for here rather than when the list is first
                // drawn, because drawing must not start a round trip.
                d.request_dialogs(now, 0, 0, None);
                self.dialogs_from_top = true;
                if !symbian_app::devbridge::is_connected() {
                    let h = self.driver.as_ref().and_then(|d| d.link_bearer_handle());
                    symbian_app::devbridge::connect(h);
                }
                Handled::Consumed
            }
            driver::Outcome::Answered(tag, body) => {
                self.on_answer(tag, &body, now);
                Handled::Consumed
            }
            driver::Outcome::RequestFailed(tag, text) => {
                let kind = tag & !driver::TAG_INDEX;
                if kind == driver::TAG_FILE {
                    if text.starts_with("FILE_REFERENCE_EXPIRED") {
                        // The reference is per-message and expires; the fix is to re-fetch
                        // the message and try again with the fresh one. Which message comes
                        // from the pending record rather than from the tag, which only
                        // carries the chat.
                        let p = self.pending.take();
                        let idx = p.as_ref().map_or((tag & driver::TAG_INDEX) as usize, |p| p.chat());
                        let msg_id = p.as_ref().map_or(0, |p| p.msg_id);
                        symbian::log!("[act] refreshing file");
                        self.refresh_message(idx, msg_id);
                    } else if let Some(dc) = tg_proto::auth::file_migrate_target(&text) {
                        // The dc the message claimed was absent or wrong. Retry on the one
                        // the server named, from the same offset, keeping whatever chunks
                        // already arrived — and *without* moving the session, which is what
                        // the other MIGRATE errors ask for and would sign the user out.
                        symbian::log!("[media] file lives on another dc, retrying there");
                        let now = symbian::unix_time();
                        let retry = self.pending.as_mut().map(|p| {
                            p.dc = dc;
                            p.req.offset = p.got.len() as i64;
                            p.req.clone()
                        });
                        let sent = match (retry, self.driver.as_mut()) {
                            (Some(req), Some(d)) => d.request_file_chunk(req, dc, now),
                            _ => false,
                        };
                        if sent {
                            self.say(alloc::format!("buscando no servidor {dc}..."));
                        } else {
                            self.pending = None;
                            self.store.status =
                                String::from("download: outro servidor, sem rota");
                        }
                    } else {
                        self.pending = None;
                        let msg = alloc::format!("download: {text}");
                        self.store.status = msg.clone();
                        if let Screen::Conversation(conv) = &mut self.screen {
                            conv.note = Some(msg);
                        }
                    }
                }
                Handled::Consumed
            }
            driver::Outcome::Disconnected(why) => {
                self.login.set_error(why);
                Handled::Consumed
            }
            driver::Outcome::None => Handled::Ignored,
        }
    }

    fn draw(&mut self, c: &mut Canvas<'_>, theme: &Theme<'_>) {
        self.paint(c, theme)
    }

    fn should_exit(&self) -> bool {
        self.should_exit
    }

    fn title(&self) -> &str {
        "Telegram"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // App is imported for its methods: handle_key and draw are trait methods now, so the
    // trait has to be in scope to call them.
    use symbian_ui::{App as _, BitmapFont, Fonts, Key, Size, Softkey, TextField};

    fn atlas() -> alloc::vec::Vec<u8> {
        let chars: alloc::vec::Vec<char> = (0x20u32..0x500).filter_map(char::from_u32).collect();
        let mut idx = alloc::vec::Vec::new();
        let mut blob = alloc::vec::Vec::new();
        for ch in &chars {
            idx.extend_from_slice(&(*ch as u32).to_le_bytes());
            idx.extend_from_slice(&(blob.len() as u32).to_le_bytes());
            idx.extend_from_slice(&[6, 8, 6, 0]);
            idx.extend_from_slice(&0i16.to_le_bytes());
            idx.extend_from_slice(&8i16.to_le_bytes());
            blob.extend(core::iter::repeat(0x80u8).take(48));
        }
        let mut v = alloc::vec::Vec::new();
        v.extend_from_slice(b"SBF1");
        v.extend_from_slice(&12u16.to_le_bytes());
        v.extend_from_slice(&9i16.to_le_bytes());
        v.extend_from_slice(&3i16.to_le_bytes());
        v.extend_from_slice(&(chars.len() as u16).to_le_bytes());
        v.push(1);
        v.push(6);
        v.extend_from_slice(&0u16.to_le_bytes());
        v.extend_from_slice(&idx);
        v.extend_from_slice(&blob);
        v
    }

    const SCREEN: Size = Size::new(320, 240);

    #[test]
    fn opening_a_chat_clears_its_unread_count_and_returning_keeps_the_place() {
        let data = atlas();
        let f = BitmapFont::new(&data).unwrap();
        let t = Theme::dark(Fonts { body: &f, strong: &f, small: &f, title: &f });
        let r = Rect::from_size(SCREEN);
        let mut app = App::mock();

        app.handle_key(KeyEvent::new(Key::Down), &t, r); // select chat 1
        assert!(app.store.chats[1].unread > 0);
        app.handle_key(KeyEvent::new(Key::Select), &t, r);
        assert_eq!(app.in_conversation(), Some(1));
        assert_eq!(app.store.chats[1].unread, 0);

        app.handle_key(KeyEvent::new(Key::Softkey(Softkey::Right)), &t, r);
        assert_eq!(app.in_conversation(), None);
        // Coming back should land on the chat we just left, not the top.
        match &app.screen {
            Screen::Chats(l) => assert_eq!(l.state.selected, 1),
            _ => panic!(),
        }
    }

    /// Hold Down past the bottom of the chat list.
    ///
    /// The gesture that crashed on device: scroll to the last conversation, then keep
    /// pressing. Past the end `ChatList` returns `LoadMore`, which is the one path that asks
    /// for a *second* page — and therefore the one path a first page never exercises.
    ///
    /// Pressed far more times than there are rows, because "press it more" was the report and
    /// a single extra press would not have found a state that only appears on the second or
    /// third.
    #[test]
    fn pressing_down_past_the_end_of_the_chat_list_is_survivable() {
        let data = atlas();
        let f = BitmapFont::new(&data).unwrap();
        let t = Theme::dark(Fonts { body: &f, strong: &f, small: &f, title: &f });
        let r = Rect::from_size(SCREEN);
        let mut app = App::mock();

        let n = app.store.chats.len();
        assert!(n > 0, "the mock store must have chats for this to mean anything");
        for _ in 0..(n + 20) {
            app.handle_key(KeyEvent::new(Key::Down), &t, r);
        }
        match &app.screen {
            Screen::Chats(l) => {
                assert!(l.state.selected < app.store.chats.len(), "selection left the list");
            }
            _ => panic!("the chat list should still be on screen"),
        }

        // And a repaint afterwards, because a selection that survived the key handler can
        // still be out of range for the drawing code that indexes `store.chats` directly.
        let mut buf = alloc::vec![0u16; (SCREEN.w * SCREEN.h) as usize];
        let mut c = Canvas::from_slice(&mut buf, SCREEN);
        app.draw(&mut c, &t);
    }

    /// The reply to that request, when the server has nothing left to send.
    ///
    /// An exhausted page is the normal end of pagination, not an error, and it must leave the
    /// list usable: not still claiming to load, and not asking again forever.
    #[test]
    fn an_empty_second_page_ends_pagination_cleanly() {
        let data = atlas();
        let f = BitmapFont::new(&data).unwrap();
        let t = Theme::dark(Fonts { body: &f, strong: &f, small: &f, title: &f });
        let r = Rect::from_size(SCREEN);
        let mut app = App::mock();
        let before = app.store.chats.len();

        for _ in 0..(before + 5) {
            app.handle_key(KeyEvent::new(Key::Down), &t, r);
        }

        // The exhausted page, exactly as the wire carries it.
        let mut w = tg_proto::tl::Writer::new();
        w.ctor(tg_proto::schema::MESSAGES_DIALOGSSLICE_CTOR)
            .int(before as i32)
            .raw(&empty_vector())
            .raw(&empty_vector())
            .raw(&empty_vector())
            .raw(&empty_vector());
        app.dialogs_from_top = false;
        app.on_answer(driver::TAG_DIALOGS, &w.finish(), 1_700_000_000);

        assert_eq!(app.store.chats.len(), before, "an empty page must add nothing");
        assert!(!app.store.dialogs_loading, "still claiming to load after the last page");
        assert!(app.store.dialogs_complete, "must stop asking, or every Down spends a request");

        let mut buf = alloc::vec![0u16; (SCREEN.w * SCREEN.h) as usize];
        let mut c = Canvas::from_slice(&mut buf, SCREEN);
        app.draw(&mut c, &t);
    }

    fn empty_vector() -> alloc::vec::Vec<u8> {
        let mut w = tg_proto::tl::Writer::new();
        w.ctor(0x1cb5c415).uint(0);
        w.finish()
    }

    #[test]
    fn sending_appends_a_pending_outgoing_message() {
        let data = atlas();
        let f = BitmapFont::new(&data).unwrap();
        let t = Theme::dark(Fonts { body: &f, strong: &f, small: &f, title: &f });
        let r = Rect::from_size(SCREEN);
        let mut app = App::mock();
        app.handle_key(KeyEvent::new(Key::Select), &t, r);

        let before = app.store.chats[0].messages.len();
        for ch in "teste".chars() {
            app.handle_key(KeyEvent::new(Key::Char(ch)), &t, r);
        }
        app.handle_key(KeyEvent::new(Key::Softkey(Softkey::Middle)), &t, r);

        let msgs = &app.store.chats[0].messages;
        assert_eq!(msgs.len(), before + 1);
        let last = msgs.last().unwrap();
        assert_eq!(last.text, "teste");
        assert!(last.outgoing);
        assert_eq!(last.state, Delivery::Pending);
    }

    #[test]
    fn right_softkey_on_the_chat_list_asks_to_exit() {
        let data = atlas();
        let f = BitmapFont::new(&data).unwrap();
        let t = Theme::dark(Fonts { body: &f, strong: &f, small: &f, title: &f });
        let r = Rect::from_size(SCREEN);
        let mut app = App::mock();
        assert!(!app.should_exit);
        app.handle_key(KeyEvent::new(Key::Softkey(Softkey::Right)), &t, r);
        assert!(app.should_exit);
    }

    #[test]
    fn drawing_every_screen_stays_inside_the_framebuffer() {
        let data = atlas();
        let f = BitmapFont::new(&data).unwrap();
        let t = Theme::dark(Fonts { body: &f, strong: &f, small: &f, title: &f });
        let r = Rect::from_size(SCREEN);
        let mut buf = alloc::vec![0u16; (SCREEN.w * SCREEN.h) as usize];
        let mut app = App::mock();

        // Chat list, then every conversation. Canvas clipping would panic on an
        // out-of-range write, so completing this is the assertion.
        {
            let mut c = Canvas::from_slice(&mut buf, SCREEN);
            app.draw(&mut c, &t);
        }
        for i in 0..app.store.chats.len() {
            let mut app = App::mock();
            for _ in 0..i {
                app.handle_key(KeyEvent::new(Key::Down), &t, r);
            }
            app.handle_key(KeyEvent::new(Key::Select), &t, r);
            let mut c = Canvas::from_slice(&mut buf, SCREEN);
            app.draw(&mut c, &t);
        }

        // Login screens: phone, code, password.
        {
            let mut login = App::mock_login();
            let mut c = Canvas::from_slice(&mut buf, SCREEN);
            login.draw(&mut c, &t);
        }
        // Code screen
        {
            use login::{Login, Screen};
            let mut login = Login::new(12345, "abcdef");
            login.code_sent = true;
            login.screen = Screen::Code {
                field: TextField::with_limit(8),
                length: Some(5),
                error: None,
            };
            let mut app = App { driver: None, store: Store::mock(), login, screen: super::Screen::Login, should_exit: false, opening_chat: None, dialogs_from_top: true, pending: None, decoding: None, decode_src: "", decode_watchdog: None };
            let mut c = Canvas::from_slice(&mut buf, SCREEN);
            app.draw(&mut c, &t);
        }
        // Password screen
        {
            use login::{Login, Screen};
            let mut login = Login::new(12345, "abcdef");
            login.password_needed = true;
            login.screen = Screen::Password {
                field: {
                    let mut f = TextField::with_limit(128);
                    f.set_masked(true);
                    f
                },
                hint: String::new(),
                error: None,
            };
            let mut app = App { driver: None, store: Store::mock(), login, screen: super::Screen::Login, should_exit: false, opening_chat: None, dialogs_from_top: true, pending: None, decoding: None, decode_src: "", decode_watchdog: None };
            let mut c = Canvas::from_slice(&mut buf, SCREEN);
            app.draw(&mut c, &t);
        }
    }
}
