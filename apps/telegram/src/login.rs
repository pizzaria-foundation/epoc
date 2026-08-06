//! The three login screens: phone, code, and password.
//!
//! Each screen is a title, a [`TextField`], a hint line, and an error line. The
//! [`tg_proto::auth::Login`] machine drives the protocol; this module draws what it
//! says and hands the actions back to the caller, which owns the network.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use symbian_ui::{
    chrome, paint, Align, Canvas, Frame, Handled, Key, KeyEvent, Rect, Softkey, TextField, Theme,
};

use tg_proto::auth::{self, Action, AuthError};
use tg_proto::crypto::Rng;

#[derive(Clone, Debug)]
pub enum Screen {
    /// The user types a phone number. A fixed `+` is shown before the field because
    /// the E72's Fn layer cannot produce it yet — digits only.
    Phone { field: TextField, error: Option<String> },
    /// The user types the code the server sent.
    Code { field: TextField, length: Option<i32>, error: Option<String> },
    /// The user types their two-factor password.
    Password { field: TextField, hint: String, error: Option<String> },
    /// Waiting for the network or worker.
    Waiting(&'static str),
}

/// What the screen wants the app to do next.
///
/// Same shape as [`ChatListAction`] and [`ConvAction`]: `handle_key` returns a pair,
/// and the app carries out the action after the borrow ends.
#[derive(Clone, Debug)]
pub enum LoginAction {
    SendCode(String),
    SubmitCode(String),
    SubmitPassword(Vec<u8>),
    Resend,
    Back,
    None,
}

pub struct Login {
    pub(crate) screen: Screen,
    machine: auth::Login,
    /// Kept alongside the machine so the screen can report a build with no credentials
    /// without reaching into it. Two small copies rather than an accessor on the protocol
    /// crate, which has no business knowing there is a screen.
    api_id: i32,
    api_hash_empty: bool,
    /// Whether the caller has already seen `CodeSent` and needs to present the code
    /// screen. Set here rather than inferred from the machine state, because the
    /// screen transition happens once when `CodeSent` arrives and should not repeat.
    pub(crate) code_sent: bool,
    /// Set when the server says the account needs a password.
    pub(crate) password_needed: bool,
}

/// What the caller must do, returned by every method that advances the machine.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Progress {
    Call { body: Vec<u8>, tag: u32 },
    Kdf { password: Vec<u8>, salt1: Vec<u8>, salt2: Vec<u8> },
    ModPow { base: Vec<u8>, exp: Vec<u8>, modulus: Vec<u8> },
    Migrate(u8),
    Authorized,
    Error(AuthError),
    None,
}

impl Login {
    pub fn new(api_id: i32, api_hash: &str) -> Self {
        Login {
            screen: Screen::Phone {
                field: TextField::with_limit(16),
                error: None,
            },
            machine: auth::Login::new(api_id, api_hash),
            api_id,
            api_hash_empty: api_hash.is_empty(),
            code_sent: false,
            password_needed: false,
        }
    }

    /// Build a Login for the purpose of rendering a specific screen in the preview.
    pub fn for_preview(screen: Screen) -> Self {
        Login {
            screen,
            machine: auth::Login::new(0, ""),
            api_id: 0,
            api_hash_empty: true,
            code_sent: false,
            password_needed: false,
        }
    }

    /// Whether this build can log in at all.
    ///
    /// `api_id` and `api_hash` identify the application and come from `api.conf`, which is
    /// gitignored. A build without them reaches Telegram and is answered `API_ID_INVALID` —
    /// legible, but only after a phone number, a round trip and a wait. Asking here means
    /// the screen can say it up front.
    pub fn credentials_missing(&self) -> bool {
        self.api_id == 0 || self.api_hash_empty
    }

    /// The number this login is for, so a migration can restart with it.
    pub fn phone(&self) -> &str {
        self.machine.phone()
    }

    /// Whether the caller is now authorized.
    pub fn is_authorized(&self) -> bool {
        self.machine.is_done()
    }

    /// Ask the server to send a code to `phone`.
    pub fn ask_send_code(&mut self, phone: &str) -> Progress {
        self.screen = Screen::Waiting("sending the code");
        progress(self.machine.send_code(phone))
    }

    /// Ask for another code.
    pub fn ask_resend(&mut self) -> Progress {
        self.screen = Screen::Waiting("resending the code");
        progress(self.machine.resend_code())
    }

    /// Submit the code the user typed.
    pub fn submit_code(&mut self, code: &str) -> Progress {
        self.screen = Screen::Waiting("signing in");
        progress(self.machine.submit_code(code))
    }

    /// Submit the two-factor password.
    pub fn submit_password(&mut self, password: &[u8]) -> Progress {
        self.screen = Screen::Waiting("checking the password");
        progress(self.machine.submit_password(password))
    }

    /// Feed a successful reply.
    pub fn on_reply<R: Rng>(&mut self, tag: u32, body: &[u8], rng: &mut R) -> Progress {
        let act = self.machine.on_reply(tag, body, rng);
        self.apply(act)
    }

    /// Feed an RPC error.
    pub fn on_error(&mut self, tag: u32, text: &str) -> Progress {
        let act = self.machine.on_error(tag, text);
        self.apply(act)
    }

    /// Feed the result of an [`Action::Kdf`].
    pub fn on_kdf<R: Rng>(&mut self, x: [u8; 32], rng: &mut R) -> Progress {
        progress(self.machine.on_kdf(x, rng))
    }

    /// Feed the result of an [`Action::ModPow`].
    pub fn on_modpow(&mut self, result: &[u8]) -> Progress {
        progress(self.machine.on_modpow(result))
    }

    /// Apply an action from the machine, transitioning the screen if needed.
    fn apply(&mut self, act: Action) -> Progress {
        match &act {
            Action::NeedPassword { hint } => {
                self.password_needed = true;
                self.screen = Screen::Password {
                    field: {
                        let mut f = TextField::with_limit(128);
                        f.set_masked(true);
                        f
                    },
                    hint: hint.clone(),
                    error: None,
                };
                return Progress::None;
            }
            Action::CodeSent { length } => {
                self.code_sent = true;
                self.screen = Screen::Code {
                    field: TextField::with_limit(8),
                    length: *length,
                    error: None,
                };
                return Progress::None;
            }
            Action::Failed(e) => {
                match &mut self.screen {
                    Screen::Phone { error, .. } => *error = Some(error_text(e)),
                    Screen::Code { error, .. } => *error = Some(error_text(e)),
                    Screen::Password { error, .. } => *error = Some(error_text(e)),
                    Screen::Waiting(_) => {
                        // An error while waiting means the screen that triggered the
                        // request should show it. Fall back to Phone as the safest.
                        self.screen = Screen::Phone {
                            field: TextField::with_limit(16),
                            error: Some(error_text(e)),
                        };
                    }
                }
                return Progress::Error(e.clone());
            }
            _ => {}
        }
        progress(act)
    }

    // ---- UI ----

    /// Process a key event. Returns the handling result and, when the user presses
    /// a confirm or back key, an action for the app to carry out.
    pub fn handle_key(
        &mut self,
        ev: KeyEvent,
        theme: &Theme<'_>,
        screen_rect: Rect,
    ) -> (Handled, LoginAction) {
        let digit_only = matches!(self.screen, Screen::Phone { .. } | Screen::Code { .. });
        // Take the field out, modify it, put it back — avoids borrowing self twice.
        let mut screen = core::mem::replace(&mut self.screen, Screen::Waiting(""));
        let (handled, action) = handle_screen_key(&mut screen, ev, theme, screen_rect, digit_only);
        self.screen = screen;
        (handled, action)
    }

    pub fn draw(&mut self, c: &mut Canvas<'_>, theme: &Theme<'_>) {
        let screen = Rect::from_size(c.size());
        let frame = Frame::split(screen, theme, true, true);
        chrome::clear(c, theme);

        match &self.screen {
            Screen::Phone { ref field, ref error } => {
                chrome::title_bar(
                    c, frame.title, theme, "Telegram", Some("entrar"),
                );
                // A build with no api_id reaches Telegram and is told API_ID_INVALID. Said
                // here instead, before a number is typed and a round trip is spent, and
                // said in place of the error line rather than beside it -- there is no
                // error yet and inventing one would be a lie about what happened.
                let missing = self.credentials_missing();
                let line = if missing {
                    Some("sem api_id: veja apps/telegram/api.conf.example")
                } else {
                    error.as_deref()
                };
                draw_field_centered(
                    c, frame.content, theme, "Número de telefone",
                    Some("+"), Some("11 999999999"), field, line,
                );
                chrome::softkey_bar(
                    c, frame.softkeys, theme,
                    [None, if missing { None } else { Some("Avançar") }, None],
                );
            }
            Screen::Code { ref field, length, ref error } => {
                chrome::title_bar(
                    c, frame.title, theme, "Telegram", Some("código"),
                );
                draw_field_centered(
                    c, frame.content, theme, "Código",
                    None, Some("código"), field, error.as_deref(),
                );
                let hint = match length {
                    Some(n) => {
                        let mut s = String::from("Digite os ");
                        s.push_str(&itoa(*n as u32));
                        s.push_str(" dígitos");
                        s
                    }
                    None => String::from("Digite o código enviado por SMS"),
                };
                c.draw_text_in(
                    Rect::from_xywh(
                        frame.content.x0 + theme.metrics.pad,
                        frame.content.y1 - theme.metrics.pad - theme.metrics.softkey_h
                            - theme.fonts.small.line_height(),
                        frame.content.width() - theme.metrics.pad * 2,
                        theme.fonts.small.line_height(),
                    ),
                    &hint,
                    theme.fonts.small,
                    theme.palette.dim,
                    Align::Center,
                );
                chrome::softkey_bar(
                    c, frame.softkeys, theme,
                    [Some("Voltar"), Some("Entrar"), None],
                );
            }
            Screen::Password { ref field, ref hint, ref error } => {
                chrome::title_bar(
                    c, frame.title, theme, "Telegram", Some("senha"),
                );
                draw_field_centered(
                    c, frame.content, theme, "Senha de dois fatores",
                    None, Some("senha"), field, error.as_deref(),
                );
                if !hint.is_empty() {
                    c.draw_text_in(
                        Rect::from_xywh(
                            frame.content.x0 + theme.metrics.pad,
                            frame.content.y1 - theme.metrics.pad - theme.metrics.softkey_h
                                - theme.fonts.small.line_height(),
                            frame.content.width() - theme.metrics.pad * 2,
                            theme.fonts.small.line_height(),
                        ),
                        hint,
                        theme.fonts.small,
                        theme.palette.dim,
                        Align::Center,
                    );
                }
                chrome::softkey_bar(
                    c, frame.softkeys, theme,
                    [None, Some("Entrar"), None],
                );
            }
            Screen::Waiting(msg) => {
                chrome::title_bar(
                    c, frame.title, theme, "Telegram", Some(msg),
                );
                chrome::placeholder(c, frame.content, theme, msg);
                chrome::softkey_bar(
                    c, frame.softkeys, theme,
                    [None, None, Some("Cancelar")],
                );
            }
        }
    }
}

fn handle_screen_key(
    screen: &mut Screen,
    ev: KeyEvent,
    _theme: &Theme<'_>,
    _screen_rect: Rect,
    digit_only: bool,
) -> (Handled, LoginAction) {
    let none = (Handled::Ignored, LoginAction::None);

    match screen {
        Screen::Waiting(_) => {
            // "Cancelar" does not cancel the request in flight, and the reason is where
            // the socket lives rather than anything about the platform.
            //
            // Cancelling is entirely possible: `shim_tcp_close` cancels before it closes,
            // which is the whole lesson of RSocket::Close() waiting forever on a pending
            // Read, and `shim_dns_close` exists for the same reason. But this screen does
            // not own the socket — `Link` does — and nothing routes a cancellation down to
            // it.
            //
            // So what this does is move back to Phone, so the app is not stuck watching a
            // spinner. A reply that arrives afterwards still goes through `apply`, which
            // posts the code or the error, and the user sees the result rather than being
            // trapped. Wiring a real cancel means a LoginAction the caller turns into a
            // `Link` call; it is one variant away and not done.
            match ev.key {
                Key::Softkey(Softkey::Right) => {
                    *screen = Screen::Phone {
                        field: TextField::with_limit(16),
                        error: None,
                    };
                    (Handled::Consumed, LoginAction::None)
                }
                _ => none,
            }
        }
        Screen::Phone { field, .. } => {
            let handled = handle_field(field, ev, digit_only);
            if handled.is_consumed() {
                return (handled, LoginAction::None);
            }
            match ev.key {
                Key::Softkey(Softkey::Middle) | Key::Enter | Key::Select | Key::Call => {
                    let number = alloc::format!("+{}", field.text());
                    (Handled::Consumed, LoginAction::SendCode(number))
                }
                Key::Softkey(Softkey::Right) => {
                    (Handled::Consumed, LoginAction::Back)
                }
                _ => none,
            }
        }
        Screen::Code { field, .. } => {
            let handled = handle_field(field, ev, digit_only);
            if handled.is_consumed() {
                return (handled, LoginAction::None);
            }
            match ev.key {
                Key::Softkey(Softkey::Middle) | Key::Enter | Key::Select | Key::Call => {
                    let code = field.text().to_string();
                    (Handled::Consumed, LoginAction::SubmitCode(code))
                }
                Key::Softkey(Softkey::Right) => {
                    *screen = Screen::Phone {
                        field: TextField::with_limit(16),
                        error: None,
                    };
                    (Handled::Consumed, LoginAction::None)
                }
                _ => none,
            }
        }
        Screen::Password { field, .. } => {
            let handled = handle_field(field, ev, false);
            if handled.is_consumed() {
                return (handled, LoginAction::None);
            }
            match ev.key {
                Key::Softkey(Softkey::Middle) | Key::Enter | Key::Select | Key::Call => {
                    let pw = field.text().as_bytes().to_vec();
                    (Handled::Consumed, LoginAction::SubmitPassword(pw))
                }
                _ => none,
            }
        }
    }
}

fn handle_field(field: &mut TextField, ev: KeyEvent, digit_only: bool) -> Handled {
    if digit_only {
        if let Key::Char(ch) = ev.key {
            if !ch.is_ascii_digit() {
                return Handled::Consumed;
            }
        }
    }
    field.handle_key(ev)
}

fn draw_field_centered(
    c: &mut Canvas<'_>,
    area: Rect,
    theme: &Theme<'_>,
    title: &str,
    prefix: Option<&str>,
    placeholder: Option<&str>,
    field: &TextField,
    error: Option<&str>,
) {
    let p = &theme.palette;
    let m = &theme.metrics;
    let body = theme.fonts.body;
    let title_font = theme.fonts.title;

    // Centre vertically: title, gap, field, gap, hint/error.
    let title_h = title_font.line_height();
    let field_h = body.line_height() + 8;
    let err_h = if error.is_some() { theme.fonts.small.line_height() + 4 } else { 0 };
    let total = title_h + 8 + field_h + err_h;
    let y0 = area.y0 + (area.height() - total) / 2;

    // Title
    c.draw_text_in(
        Rect::from_xywh(area.x0 + m.pad, y0, area.width() - m.pad * 2, title_h),
        title,
        title_font,
        p.text,
        Align::Center,
    );

    // Field area
    let field_y = y0 + title_h + 8;
    let field_bg = area.width() / 2 + 40;
    let field_x0 = area.x0 + (area.width() - field_bg) / 2;
    let field_r = Rect::from_xywh(field_x0, field_y, field_bg, field_h);

    paint::band(c, field_r, &p.chrome);

    // Prefix (the fixed +)
    let mut text_x = field_r.x0 + 6;
    if let Some(pre) = prefix {
        c.draw_text(
            symbian_ui::Point::new(text_x, field_r.y0 + 3 + body.ascent()),
            pre,
            body,
            p.dim,
        );
        text_x += body.measure(pre) + 2;
    }

    // Field text (or mask)
    let display = field.display();
    if display.is_empty() {
        if let Some(ph) = placeholder {
            if !ph.is_empty() {
                c.draw_text(
                    symbian_ui::Point::new(text_x, field_r.y0 + 3 + body.ascent()),
                    ph,
                    body,
                    p.dim,
                );
            }
        }
    } else {
        c.draw_text_in(
            Rect::new(
                text_x,
                field_r.y0 + 3,
                field_r.x1 - 4,
                field_r.y0 + 3 + body.line_height(),
            ),
            &display,
            body,
            p.text,
            Align::Start,
        );
    }

    // Caret. For a masked field, the display is * per char, each one byte, so
    // the byte offset of the cursor in the masked text equals the number of real
    // characters before the cursor.
    let cursor_display_offset = if field.is_masked() {
        field.text()[..field.cursor().min(field.text().len())]
            .chars()
            .count()
    } else {
        field.cursor()
    };
    let before = &display[..cursor_display_offset.min(display.len())];
    let cx = text_x + body.measure(before);
    c.fill_rect(Rect::new(cx, field_r.y0 + 3, cx + 1, field_r.y1 - 3), p.accent);

    // Error
    if let Some(e) = error {
        c.draw_text_in(
            Rect::from_xywh(
                area.x0 + m.pad,
                field_y + field_h + 2,
                area.width() - m.pad * 2,
                theme.fonts.small.line_height(),
            ),
            e,
            theme.fonts.small,
            p.unread,
            Align::Center,
        );
    }
}

fn progress(act: Action) -> Progress {
    match act {
        Action::Call { body, tag } => Progress::Call { body, tag },
        Action::Kdf { password, salt1, salt2 } => Progress::Kdf { password, salt1, salt2 },
        Action::ModPow { base, exp, modulus } => Progress::ModPow { base, exp, modulus },
        Action::CodeSent { .. } => Progress::None,
        Action::NeedPassword { .. } => Progress::None,
        Action::Migrate(dc) => Progress::Migrate(dc),
        Action::Authorized => Progress::Authorized,
        Action::Failed(e) => Progress::Error(e),
    }
}

fn error_text(e: &AuthError) -> String {
    match e {
        AuthError::PhoneNumberInvalid => "Número não reconhecido pelo Telegram".into(),
        AuthError::PhoneCodeInvalid => "Código incorreto. Verifique e tente de novo".into(),
        AuthError::PhoneCodeExpired => {
            "O código expirou. Solicite um novo e tente de novo".into()
        }
        AuthError::PasswordInvalid => "Senha incorreta".into(),
        AuthError::FloodWait(n) => alloc::format!("Muitas tentativas. Aguarde {n} segundos"),
        AuthError::SignUpRequired => {
            "Este número não tem conta. Este cliente não pode criar uma".into()
        }
        AuthError::ApiIdInvalid => "Erro de configuração (api_id inválido)".into(),
        AuthError::Other(s) => s.clone(),
    }
}

/// Small unsigned integer to text without `core::fmt`.
fn itoa(mut v: u32) -> String {
    let mut buf = [0u8; 10];
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    String::from_utf8_lossy(&buf[i..]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian_ui::{BitmapFont, Fonts, Size};

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

    fn theme() -> Theme<'static> {
        let data: &'static [u8] = Box::leak(atlas().into_boxed_slice());
        let f: &'static BitmapFont<'static> =
            Box::leak(Box::new(BitmapFont::new(data).unwrap()));
        Theme::dark(Fonts { body: f, strong: f, small: f, title: f })
    }

    struct TestRng(u32);

    impl Rng for TestRng {
        fn fill(&mut self, out: &mut [u8]) {
            for x in out.iter_mut() {
                self.0 = self.0.wrapping_mul(0x41C6_4E6D).wrapping_add(0x6073);
                *x = (self.0 >> 24) as u8;
            }
        }
    }

    #[test]
    fn drawing_every_login_screen_stays_inside_the_framebuffer() {
        let t = theme();
        let mut buf = alloc::vec![0u16; (320 * 240) as usize];
        let sz = Size::new(320, 240);

        // Phone screen
        {
            let mut login = Login::new(12345, "abcdef");
            let mut c = symbian_ui::Canvas::from_slice(&mut buf, sz);
            login.draw(&mut c, &t);
        }

        // Code screen
        {
            let mut login = Login::new(12345, "abcdef");
            login.code_sent = true;
            login.screen = Screen::Code {
                field: TextField::with_limit(8),
                length: Some(5),
                error: None,
            };
            let mut c = symbian_ui::Canvas::from_slice(&mut buf, sz);
            login.draw(&mut c, &t);
        }

        // Password screen
        {
            let mut login = Login::new(12345, "abcdef");
            login.password_needed = true;
            login.screen = Screen::Password {
                field: {
                    let mut f = TextField::with_limit(128);
                    f.set_masked(true);
                    f
                },
                hint: "dica do usuário".into(),
                error: None,
            };
            let mut c = symbian_ui::Canvas::from_slice(&mut buf, sz);
            login.draw(&mut c, &t);
        }

        // Error on phone screen
        {
            let mut login = Login::new(12345, "abcdef");
            login.screen = Screen::Phone {
                field: TextField::with_limit(16),
                error: Some("Número não reconhecido".into()),
            };
            let mut c = symbian_ui::Canvas::from_slice(&mut buf, sz);
            login.draw(&mut c, &t);
        }

        // Waiting screen
        {
            let mut login = Login::new(12345, "abcdef");
            login.screen = Screen::Waiting("connecting…");
            let mut c = symbian_ui::Canvas::from_slice(&mut buf, sz);
            login.draw(&mut c, &t);
        }
    }

    #[test]
    fn phone_screen_accepts_only_digits_and_emits_send_code_on_confirm() {
        let t = theme();
        let r = Rect { x0: 0, y0: 0, x1: 320, y1: 240 };
        let mut login = Login::new(12345, "abcdef");

        // Non-digits are swallowed.
        let (h, a) = login.handle_key(KeyEvent::new(Key::Char('a')), &t, r);
        assert_eq!(h, Handled::Consumed);
        assert!(matches!(a, LoginAction::None));

        // Digits are accepted.
        login.handle_key(KeyEvent::new(Key::Char('1')), &t, r);
        login.handle_key(KeyEvent::new(Key::Char('2')), &t, r);

        match &login.screen {
            Screen::Phone { field, .. } => {
                assert_eq!(field.text(), "12");
                assert!(!field.text().contains('a'));
            }
            _ => panic!(),
        }

        // Confirm emits the action with the + prepended.
        let (h, a) =
            login.handle_key(KeyEvent::new(Key::Softkey(Softkey::Middle)), &t, r);
        assert_eq!(h, Handled::Consumed);
        match a {
            LoginAction::SendCode(number) => assert_eq!(number, "+12"),
            _ => panic!("expected SendCode, got {a:?}"),
        }
    }

    #[test]
    fn send_code_produces_a_call() {
        let mut login = Login::new(12345, "abcdef");
        let p = login.ask_send_code("+5511999999999");
        match p {
            Progress::Call { tag, .. } => assert_eq!(tag, auth::tag::SEND_CODE),
            other => panic!("expected Call, got {other:?}"),
        }
    }

    #[test]
    fn code_sent_transitions_to_the_code_screen() {
        let mut login = Login::new(12345, "abcdef");
        login.ask_send_code("+5511");
        let mut rng = TestRng(0);

        use tg_proto::tl::Writer;
        let mut w = Writer::new();
        w.ctor(tg_proto::rpc::AUTH_SENT_CODE)
            .uint(0)
            .ctor(0xc000_bba2) // auth.sentCodeTypeSms
            .int(5)
            .string("HASH123");

        let p = login.on_reply(auth::tag::SEND_CODE, &w.finish(), &mut rng);
        assert!(matches!(p, Progress::None));
        assert!(matches!(login.screen, Screen::Code { .. }));
        assert!(login.code_sent);
    }

    #[test]
    fn an_error_shows_on_the_current_screen() {
        let mut login = Login::new(12345, "abcdef");
        let p = login.on_error(auth::tag::SEND_CODE, "PHONE_NUMBER_INVALID");
        assert!(matches!(p, Progress::Error(AuthError::PhoneNumberInvalid)));
        match &login.screen {
            Screen::Phone { error, .. } => {
                assert!(error.is_some());
            }
            _ => panic!(),
        }
    }

    #[test]
    fn migration_returns_the_new_data_centre() {
        let mut login = Login::new(12345, "abcdef");
        let p = login.on_error(auth::tag::SEND_CODE, "PHONE_MIGRATE_4");
        assert_eq!(p, Progress::Migrate(4));
        login.ask_send_code("+55");
        assert_eq!(login.phone(), "+55");
    }

    #[test]
    fn flood_wait_shows_the_number_of_seconds() {
        let e = AuthError::FloodWait(3600);
        let t = error_text(&e);
        assert!(
            t.contains("3600"),
            "the user must be told how long to wait: '{t}'"
        );

        let e = AuthError::FloodWait(86400);
        let t = error_text(&e);
        assert!(t.contains("86400"));
    }

    #[test]
    fn password_is_masked_and_emits_submit_password_on_confirm() {
        let t = theme();
        let r = Rect { x0: 0, y0: 0, x1: 320, y1: 240 };
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

        // Type and check masking.
        match &mut login.screen {
            Screen::Password { field, .. } => {
                field.insert_str("hunter2");
                assert_eq!(field.text(), "hunter2");
                assert_eq!(field.display(), "*******");
            }
            _ => panic!(),
        }

        // Confirm submits the password bytes.
        let (h, a) =
            login.handle_key(KeyEvent::new(Key::Softkey(Softkey::Middle)), &t, r);
        assert_eq!(h, Handled::Consumed);
        match a {
            LoginAction::SubmitPassword(pw) => assert_eq!(pw, b"hunter2"),
            _ => panic!("expected SubmitPassword, got {a:?}"),
        }
    }
}
