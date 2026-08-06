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
pub mod chats;
pub mod conv;
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
    Login(Login),
}

pub struct App {
    /// The connection, when this build is driving one. `None` for the mock and the preview,
    /// which draw the same screens with nothing behind them.
    driver: Option<driver::Driver>,
    pub store: Store,
    screen: Screen,
    /// Set when the app wants to close, for the shim to act on.
    pub should_exit: bool,
}

impl App {
    pub fn new(store: Store) -> Self {
        Self { driver: None, store, screen: Screen::Chats(ChatList::new()), should_exit: false }
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
        // Asked for now rather than when a number is typed: attaching to a connection that
        // is already up takes 263 ms on this handset, and the handshake behind it is four
        // seconds. Both should be done before anyone finishes typing.
        let _ = driver.connect();
        Self {
            driver: Some(driver),
            store: Store::default(),
            screen: Screen::Login(Login::new(link::api_id(), link::api_hash())),
            should_exit: false,
        }
    }

    /// The login screen with no credentials, for the preview and the tests.
    pub fn mock_login() -> Self {
        Self { driver: None, store: Store::mock(), screen: Screen::Login(Login::new(0, "")), should_exit: false }
    }

    fn on_key(&mut self, ev: KeyEvent, theme: &Theme<'_>, screen_rect: Rect) -> Handled {
        match &mut self.screen {
            Screen::Login(login) => {
                let (handled, action) = login.handle_key(ev, theme, screen_rect);
                match action {
                    LoginAction::SendCode(number) => {
                        let p = login.ask_send_code(&number);
                        drive(&mut self.driver, p, login);
                        Handled::Consumed
                    }
                    LoginAction::SubmitCode(code) => {
                        let p = login.submit_code(&code);
                        drive(&mut self.driver, p, login);
                        Handled::Consumed
                    }
                    LoginAction::SubmitPassword(pw) => {
                        let p = login.submit_password(&pw);
                        drive(&mut self.driver, p, login);
                        Handled::Consumed
                    }
                    LoginAction::Resend => {
                        let p = login.ask_resend();
                        drive(&mut self.driver, p, login);
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
                let handled = list.handle_key(ev, &self.store, theme, frame.content.height());
                if handled.is_consumed() {
                    return handled;
                }
                match list.activate(ev, &self.store) {
                    ChatListAction::Open(i) => {
                        // Opening a chat clears its unread marker, as it would
                        // once messages.readHistory is wired up.
                        self.store.chats[i].unread = 0;
                        self.screen = Screen::Conversation(Conversation::new(i));
                        Handled::Consumed
                    }
                    ChatListAction::Exit => {
                        self.should_exit = true;
                        Handled::Consumed
                    }
                    ChatListAction::None => Handled::Ignored,
                }
            }
            Screen::Conversation(conv) => {
                let idx = conv.chat;
                let (handled, action) =
                    conv.handle_key(ev, &self.store.chats[idx], theme, screen_rect);
                match action {
                    ConvAction::Back => {
                        let mut list = ChatList::new();
                        list.state.selected = idx;
                        self.screen = Screen::Chats(list);
                        Handled::Consumed
                    }
                    ConvAction::Send(text) => {
                        self.send(idx, text);
                        Handled::Consumed
                    }
                    ConvAction::None => handled,
                }
            }
        }
    }

    /// Append an outgoing message locally. Real sending is asynchronous, so the
    /// message appears as `Pending` and the transport later promotes it to `Sent`
    /// — the same optimistic path a real client uses.
    fn send(&mut self, chat: usize, text: String) {
        let c = &mut self.store.chats[chat];
        c.messages.push(Message {
            text,
            outgoing: true,
            // Real timestamps need the device clock; the shim will supply it.
            time: c.messages.last().map(|m| m.time.clone()).unwrap_or_default(),
            state: Delivery::Pending,
        });
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

    fn paint(&mut self, c: &mut Canvas<'_>, theme: &Theme<'_>) {
        match &mut self.screen {
            Screen::Login(login) => login.draw(c, theme),
            Screen::Chats(list) => list.draw(c, &self.store, theme),
            Screen::Conversation(conv) => {
                let idx = conv.chat;
                // Split the borrow: the screen needs &mut, the chat needs &.
                let chat = self.store.chats[idx].clone();
                conv.draw(c, &chat, theme);
            }
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
            Screen::Login(l) => l.is_authorized(),
            _ => false,
        }
    }
}

/// Hand a login action to the connection, if this build has one.
///
/// A free function because both borrows are of `App` fields and the borrow checker will not
/// take `self.driver` and `self.screen` at once through methods.
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
        let Screen::Login(login) = &mut self.screen else {
            return Handled::Ignored;
        };
        let Some(d) = self.driver.as_mut() else {
            return Handled::Ignored;
        };

        match d.on_event(ev, login, now) {
            driver::Outcome::Authorized => {
                // Signed in. The chat list is still the mock until messages.getDialogs is
                // wired; what matters here is that the login screen goes away, because
                // leaving it up after a successful sign-in reads as a failure.
                self.screen = Screen::Chats(ChatList::new());
                Handled::Consumed
            }
            driver::Outcome::Disconnected(why) => {
                login.set_error(why);
                Handled::Consumed
            }
            driver::Outcome::Redraw => Handled::Consumed,
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
            let mut app = App { driver: None, store: Store::mock(), screen: super::Screen::Login(login), should_exit: false };
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
            let mut app = App { driver: None, store: Store::mock(), screen: super::Screen::Login(login), should_exit: false };
            let mut c = Canvas::from_slice(&mut buf, SCREEN);
            app.draw(&mut c, &t);
        }
    }
}
