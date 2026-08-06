//! Logging in: phone number, code, and the two-factor password if there is one.
//!
//! ```text
//!  Idle ──send_code──▶ AwaitCode ──submit_code──▶ AwaitSignIn ─┬──▶ Authorized
//!                                                              │
//!                          SESSION_PASSWORD_NEEDED ────────────┤
//!                                                              ▼
//!                        AwaitPassword ──submit_password──▶ Kdf ──▶ Srp ──▶ AwaitCheck
//! ```
//!
//! No I/O, like everything else in this crate: requests come out as [`Action::Call`] and
//! replies go back in. The two expensive steps leave too — the 100,000-iteration key
//! derivation as [`Action::Kdf`] and each SRP exponentiation as [`Action::ModPow`] — because
//! together they are the better part of a minute on an E72 and `rust_step` must return in
//! milliseconds.
//!
//! # Migration is not optional
//!
//! An auth key belongs to one data centre, and an account lives on one. A number that does
//! not belong to the data centre you asked answers `PHONE_MIGRATE_4`, and **the whole
//! handshake has to be redone there** — new socket, new key, new session. A Brazilian
//! number is not on DC2, so this is the first thing that happens to most logins rather than
//! an edge case.
//!
//! [`Action::Migrate`] carries the number. The caller reconnects; this machine is restarted
//! from `Idle` with the same phone.
//!
//! # Errors are named
//!
//! Telegram returns strings. A UI that matches on substrings breaks the day one is reworded,
//! and `FLOOD_WAIT_42` carries a number a user needs to be told. [`AuthError`] parses them
//! once, here, and the screens match on a type.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::crypto::Rng;
use crate::rpc::{self, PasswordParams};
use crate::srp::{self, Srp};

/// Tags for the calls this machine makes, handed to `Client::call` and back on the reply.
pub mod tag {
    pub const SEND_CODE: u32 = 0x1001;
    pub const SIGN_IN: u32 = 0x1002;
    pub const GET_PASSWORD: u32 = 0x1003;
    pub const CHECK_PASSWORD: u32 = 0x1004;
    pub const RESEND_CODE: u32 = 0x1005;
}

/// What went wrong, named rather than left as a string.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum AuthError {
    /// The number was not one Telegram recognises.
    PhoneNumberInvalid,
    /// Wrong code.
    PhoneCodeInvalid,
    /// The code timed out; ask for another.
    PhoneCodeExpired,
    /// Wrong two-factor password.
    PasswordInvalid,
    /// Too many attempts. The number of seconds to wait, which the user has to be told —
    /// Telegram's waits run to hours and a client that says only "try again" is lying.
    FloodWait(u32),
    /// The number has no account. Signing up needs a name, terms of service and screens
    /// this client does not have.
    SignUpRequired,
    /// The `api_id` is not one Telegram knows. Almost always a build with no credentials in
    /// it, which is a configuration problem rather than a login one.
    ApiIdInvalid,
    /// Anything else, with the server's own words.
    Other(String),
}

impl AuthError {
    /// Classify an RPC error string.
    ///
    /// Prefix matching throughout: Telegram appends detail to several of these, and a client
    /// that compares for equality stops recognising them the day it does.
    pub fn classify(text: &str) -> AuthError {
        if let Some(rest) = text.strip_prefix("FLOOD_WAIT_") {
            return AuthError::FloodWait(rest.parse().unwrap_or(0));
        }
        if text.starts_with("PHONE_NUMBER_INVALID") {
            return AuthError::PhoneNumberInvalid;
        }
        if text.starts_with("PHONE_CODE_INVALID") || text.starts_with("PHONE_CODE_EMPTY") {
            return AuthError::PhoneCodeInvalid;
        }
        if text.starts_with("PHONE_CODE_EXPIRED") {
            return AuthError::PhoneCodeExpired;
        }
        if text.starts_with("PASSWORD_HASH_INVALID") {
            return AuthError::PasswordInvalid;
        }
        if text.starts_with("API_ID_INVALID") || text.starts_with("API_ID_PUBLISHED_FLOOD") {
            return AuthError::ApiIdInvalid;
        }
        AuthError::Other(text.to_string())
    }
}

/// `PHONE_MIGRATE_4`, `NETWORK_MIGRATE_5`, `USER_MIGRATE_1`.
///
/// Three spellings of the same instruction. Recognising only the first works until someone
/// logs in from a network Telegram routes differently.
pub fn migrate_target(text: &str) -> Option<u8> {
    for prefix in ["PHONE_MIGRATE_", "NETWORK_MIGRATE_", "USER_MIGRATE_"] {
        if let Some(rest) = text.strip_prefix(prefix) {
            return rest.parse().ok();
        }
    }
    None
}

/// What the caller must do next.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Action {
    /// Send this request with this tag, and return the reply through [`Login::on_reply`].
    Call { body: Vec<u8>, tag: u32 },
    /// Derive `x` from the password, off the GUI thread — 100,000 PBKDF2 iterations.
    ///
    /// The password is in here, which is the one place in this crate it appears outside the
    /// caller's own buffer. It goes no further: the result comes back as 32 bytes and the
    /// password is dropped.
    Kdf { password: Vec<u8>, salt1: Vec<u8>, salt2: Vec<u8> },
    /// One of SRP's three exponentiations.
    ModPow { base: Vec<u8>, exp: Vec<u8>, modulus: Vec<u8> },
    /// A code was sent. `length` is how many digits, when the server says.
    CodeSent { length: Option<i32> },
    /// This account has two-factor enabled. `hint` is what the user set as a reminder.
    NeedPassword { hint: String },
    /// The account is on another data centre. Reconnect there and start again.
    Migrate(u8),
    /// Signed in.
    Authorized,
    Failed(AuthError),
}

#[derive(Clone, PartialEq, Eq, Debug)]
enum State {
    Idle,
    AwaitCode,
    AwaitSignIn,
    /// Waiting for the user to type the password.
    NeedPassword,
    /// Waiting for `account.getPassword`.
    AwaitParams,
    /// Waiting for the key derivation.
    AwaitKdf,
    /// Waiting for one of SRP's exponentiations.
    AwaitSrp,
    AwaitCheck,
    Done,
}

pub struct Login {
    state: State,
    phone: String,
    api_id: i32,
    api_hash: String,
    /// From `auth.sentCode`, and required by `auth.signIn`.
    code_hash: String,
    /// Held between the user typing it and the key derivation consuming it.
    password: Vec<u8>,
    params: Option<PasswordParams>,
    srp: Option<Srp>,
}

impl Login {
    pub fn new(api_id: i32, api_hash: &str) -> Self {
        Login {
            state: State::Idle,
            phone: String::new(),
            api_id,
            api_hash: api_hash.to_string(),
            code_hash: String::new(),
            password: Vec::new(),
            params: None,
            srp: None,
        }
    }

    /// The number this login is for, so a migration can restart with it.
    pub fn phone(&self) -> &str {
        &self.phone
    }

    pub fn is_done(&self) -> bool {
        self.state == State::Done
    }

    /// Ask for a code.
    pub fn send_code(&mut self, phone: &str) -> Action {
        self.phone = phone.to_string();
        self.state = State::AwaitCode;
        Action::Call {
            body: rpc::auth_send_code(phone, self.api_id, &self.api_hash),
            tag: tag::SEND_CODE,
        }
    }

    /// Ask for another one.
    pub fn resend_code(&mut self) -> Action {
        self.state = State::AwaitCode;
        Action::Call {
            body: rpc::auth_resend_code(&self.phone, &self.code_hash),
            tag: tag::RESEND_CODE,
        }
    }

    /// Submit what the user typed.
    pub fn submit_code(&mut self, code: &str) -> Action {
        self.state = State::AwaitSignIn;
        Action::Call {
            body: rpc::auth_sign_in(&self.phone, &self.code_hash, code),
            tag: tag::SIGN_IN,
        }
    }

    /// Submit the two-factor password.
    ///
    /// Does not send it. SRP's whole point is that the password never crosses the wire, so
    /// this begins a key derivation and three exponentiations that together produce a proof.
    pub fn submit_password(&mut self, password: &[u8]) -> Action {
        self.password = password.to_vec();
        // The parameters expire: srp_B is per-request and a proof built against a stale one
        // is rejected as a wrong password. So they are fetched again here rather than
        // reused from whatever arrived with SESSION_PASSWORD_NEEDED.
        self.state = State::AwaitParams;
        Action::Call { body: rpc::account_get_password(), tag: tag::GET_PASSWORD }
    }

    /// Feed a successful reply.
    pub fn on_reply<R: Rng>(&mut self, tag: u32, body: &[u8], rng: &mut R) -> Action {
        match tag {
            tag::SEND_CODE | tag::RESEND_CODE => match rpc::parse_sent_code(body) {
                Ok(sent) => {
                    self.code_hash = sent.phone_code_hash;
                    Action::CodeSent { length: sent.code_length }
                }
                Err(_) => Action::Failed(AuthError::Other("auth.sentCode".to_string())),
            },
            tag::SIGN_IN | tag::CHECK_PASSWORD => self.on_authorization(body),
            tag::GET_PASSWORD => self.on_password_params(body, rng),
            _ => Action::Failed(AuthError::Other("unexpected reply".to_string())),
        }
    }

    /// Feed an RPC error.
    pub fn on_error(&mut self, _tag: u32, text: &str) -> Action {
        // Migration first: it is not a failure, it is an instruction, and it arrives in
        // place of the answer to whatever was asked.
        if let Some(dc) = migrate_target(text) {
            self.state = State::Idle;
            return Action::Migrate(dc);
        }
        if text.starts_with("SESSION_PASSWORD_NEEDED") {
            self.state = State::NeedPassword;
            // The hint is not in this error, and asking for it now costs a round trip
            // before the user has typed anything. It arrives with the parameters instead.
            return Action::NeedPassword { hint: String::new() };
        }
        if text.starts_with("AUTH_RESTART") {
            // The server wants the flow started over. Not an error to show.
            self.state = State::Idle;
            return Action::Call {
                body: rpc::auth_send_code(&self.phone, self.api_id, &self.api_hash),
                tag: tag::SEND_CODE,
            };
        }
        Action::Failed(AuthError::classify(text))
    }

    /// Feed the result of an [`Action::Kdf`].
    pub fn on_kdf<R: Rng>(&mut self, x: [u8; 32], rng: &mut R) -> Action {
        // The password has done its work. Cleared rather than left in the struct: it is the
        // account, and this object outlives the derivation.
        self.password.clear();

        let Some(p) = self.params.clone() else {
            return Action::Failed(AuthError::Other("no password parameters".to_string()));
        };
        match Srp::start(p.srp_id, &p.p, p.g, &p.srp_b, &p.salt1, &p.salt2, x, rng) {
            Ok((s, srp::Step::ModPow { base, exp, modulus })) => {
                self.srp = Some(s);
                self.state = State::AwaitSrp;
                Action::ModPow { base, exp, modulus }
            }
            Ok((_, srp::Step::Done { .. })) => {
                Action::Failed(AuthError::Other("srp finished too early".to_string()))
            }
            Err(e) => Action::Failed(AuthError::Other(alloc::format!("srp: {e:?}"))),
        }
    }

    /// Feed the result of an [`Action::ModPow`].
    pub fn on_modpow(&mut self, result: &[u8]) -> Action {
        let Some(s) = self.srp.as_mut() else {
            return Action::Failed(AuthError::Other("no srp in flight".to_string()));
        };
        match s.on_modpow(result) {
            Ok(srp::Step::ModPow { base, exp, modulus }) => Action::ModPow { base, exp, modulus },
            Ok(srp::Step::Done { srp_id, a, m1 }) => {
                self.srp = None;
                self.state = State::AwaitCheck;
                Action::Call {
                    body: rpc::auth_check_password(srp_id, &a, &m1),
                    tag: tag::CHECK_PASSWORD,
                }
            }
            Err(e) => Action::Failed(AuthError::Other(alloc::format!("srp: {e:?}"))),
        }
    }

    fn on_password_params<R: Rng>(&mut self, body: &[u8], _rng: &mut R) -> Action {
        match rpc::parse_password(body) {
            Ok(Some(p)) => {
                let hint = p.hint.clone();
                let (salt1, salt2) = (p.salt1.clone(), p.salt2.clone());
                self.params = Some(p);
                if self.password.is_empty() {
                    // Fetched before the user typed anything, which happens when the caller
                    // asks for the hint up front. Wait for the password.
                    self.state = State::NeedPassword;
                    return Action::NeedPassword { hint };
                }
                self.state = State::AwaitKdf;
                Action::Kdf { password: self.password.clone(), salt1, salt2 }
            }
            // No password on the account, but the server said one was needed. Contradictory,
            // and reported rather than papered over: retrying sign-in would loop.
            Ok(None) => Action::Failed(AuthError::Other("no password set".to_string())),
            Err(e) => Action::Failed(AuthError::Other(alloc::format!("account.password: {e:?}"))),
        }
    }

    fn on_authorization(&mut self, body: &[u8]) -> Action {
        if body.len() < 4 {
            return Action::Failed(AuthError::Other("short authorization".to_string()));
        }
        let ctor = u32::from_le_bytes([body[0], body[1], body[2], body[3]]);
        match ctor {
            rpc::AUTH_AUTHORIZATION => {
                self.state = State::Done;
                // The user is in the reply and is not parsed. Nothing shows a profile yet,
                // and `User` is a forty-field constructor to read a name this client gets
                // from the dialog list anyway.
                Action::Authorized
            }
            rpc::AUTH_AUTHORIZATION_SIGNUP => Action::Failed(AuthError::SignUpRequired),
            other => Action::Failed(AuthError::Other(alloc::format!("auth.{other:#010x}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::testing::CountingRng;
    use crate::tl::{Reader, Writer};

    fn login() -> Login {
        Login::new(12345, "abcdef")
    }

    #[test]
    fn errors_are_classified_rather_than_passed_through() {
        assert_eq!(AuthError::classify("PHONE_CODE_INVALID"), AuthError::PhoneCodeInvalid);
        assert_eq!(AuthError::classify("PHONE_CODE_EXPIRED"), AuthError::PhoneCodeExpired);
        assert_eq!(AuthError::classify("PASSWORD_HASH_INVALID"), AuthError::PasswordInvalid);
        assert_eq!(AuthError::classify("API_ID_INVALID"), AuthError::ApiIdInvalid);
        // The number matters: Telegram's waits run to hours, and "try again" without it is
        // a client telling the user nothing.
        assert_eq!(AuthError::classify("FLOOD_WAIT_42"), AuthError::FloodWait(42));
        assert_eq!(AuthError::classify("FLOOD_WAIT_86400"), AuthError::FloodWait(86400));
        // Unknown ones keep the server's words rather than becoming a generic failure.
        assert_eq!(
            AuthError::classify("SOMETHING_NEW"),
            AuthError::Other("SOMETHING_NEW".to_string())
        );
    }

    #[test]
    fn all_three_spellings_of_migrate_are_recognised() {
        // Recognising only PHONE_MIGRATE works until someone logs in from a network
        // Telegram routes differently, and then the client loops on an error it treats as
        // fatal.
        assert_eq!(migrate_target("PHONE_MIGRATE_4"), Some(4));
        assert_eq!(migrate_target("NETWORK_MIGRATE_5"), Some(5));
        assert_eq!(migrate_target("USER_MIGRATE_1"), Some(1));
        assert_eq!(migrate_target("PHONE_CODE_INVALID"), None);
    }

    #[test]
    fn send_code_produces_the_request_and_a_migration_restarts_it() {
        let mut l = login();
        let a = l.send_code("+5511999999999");
        let Action::Call { body, tag } = a else { panic!("expected a call") };
        assert_eq!(tag, tag::SEND_CODE);
        let mut r = Reader::new(&body);
        assert_eq!(r.ctor().unwrap(), rpc::AUTH_SEND_CODE);
        assert_eq!(r.bytes().unwrap(), b"+5511999999999");
        assert_eq!(r.int().unwrap(), 12345);
        assert_eq!(r.bytes().unwrap(), b"abcdef");

        // A Brazilian number is not on DC2, so this is the common path rather than an edge.
        assert_eq!(l.on_error(tag::SEND_CODE, "PHONE_MIGRATE_4"), Action::Migrate(4));
        assert_eq!(l.phone(), "+5511999999999", "the number must survive a migration");
    }

    #[test]
    fn a_sent_code_is_remembered_and_used_to_sign_in() {
        let mut l = login();
        l.send_code("+1");

        let mut w = Writer::new();
        w.ctor(rpc::AUTH_SENT_CODE)
            .uint(0)
            .ctor(0xc000_bba2) // auth.sentCodeTypeSms
            .int(5)
            .string("HASH123");
        let mut rng = CountingRng(0);
        assert_eq!(
            l.on_reply(tag::SEND_CODE, &w.finish(), &mut rng),
            Action::CodeSent { length: Some(5) }
        );

        // The hash ties the code to the request that produced it; signing in without it is
        // rejected as an invalid code, which points the user at the wrong thing.
        let Action::Call { body, tag } = l.submit_code("12345") else { panic!() };
        assert_eq!(tag, tag::SIGN_IN);
        let mut r = Reader::new(&body);
        assert_eq!(r.ctor().unwrap(), rpc::AUTH_SIGN_IN);
        assert_eq!(r.uint().unwrap(), 1);
        assert_eq!(r.bytes().unwrap(), b"+1");
        assert_eq!(r.bytes().unwrap(), b"HASH123");
        assert_eq!(r.bytes().unwrap(), b"12345");
    }

    #[test]
    fn a_successful_sign_in_is_authorized() {
        let mut l = login();
        let mut w = Writer::new();
        w.ctor(rpc::AUTH_AUTHORIZATION).uint(0);
        let mut rng = CountingRng(0);
        assert_eq!(l.on_reply(tag::SIGN_IN, &w.finish(), &mut rng), Action::Authorized);
        assert!(l.is_done());
    }

    #[test]
    fn an_account_that_does_not_exist_is_named() {
        // Signing up needs a name, terms of service and screens this client does not have.
        // Reported rather than retried.
        let mut l = login();
        let mut w = Writer::new();
        w.ctor(rpc::AUTH_AUTHORIZATION_SIGNUP).uint(0);
        let mut rng = CountingRng(0);
        assert_eq!(
            l.on_reply(tag::SIGN_IN, &w.finish(), &mut rng),
            Action::Failed(AuthError::SignUpRequired)
        );
    }

    #[test]
    fn two_factor_goes_through_the_kdf_and_three_exponentiations() {
        let mut l = login();
        let mut rng = CountingRng(0);

        // The server asks for a password in place of an authorization.
        assert_eq!(
            l.on_error(tag::SIGN_IN, "SESSION_PASSWORD_NEEDED"),
            Action::NeedPassword { hint: String::new() }
        );

        // The user types one. Parameters are fetched fresh: srp_B is per-request and a
        // proof against a stale one is rejected as a wrong password.
        let Action::Call { tag, body } = l.submit_password(b"hunter2") else { panic!() };
        assert_eq!(tag, tag::GET_PASSWORD);
        assert_eq!(Reader::new(&body).ctor().unwrap(), rpc::ACCOUNT_GET_PASSWORD);

        let params = password_reply();
        let a = l.on_reply(tag::GET_PASSWORD, &params, &mut rng);
        let Action::Kdf { password, salt1, salt2 } = a else { panic!("expected a Kdf, got {a:?}") };
        assert_eq!(password, b"hunter2");
        assert_eq!(salt1, alloc::vec![0x11u8; 16]);
        assert_eq!(salt2, alloc::vec![0x22u8; 16]);

        // The derivation comes back and SRP begins.
        let a = l.on_kdf([7u8; 32], &mut rng);
        assert!(matches!(a, Action::ModPow { .. }), "expected the first exponentiation");

        // Three of them, then the proof.
        let mut steps = 0;
        let mut act = a;
        while let Action::ModPow { base, exp, modulus } = act {
            steps += 1;
            assert!(steps <= 4, "srp asked for more than three exponentiations");
            let m = symbian_crypto::Modulus::new(&modulus).unwrap();
            let mut out = alloc::vec![0u8; modulus.len()];
            symbian_crypto::modpow(&base, &exp, &m, &mut out).unwrap();
            act = l.on_modpow(&out);
        }
        assert_eq!(steps, 3, "srp should need exactly three exponentiations");

        let Action::Call { tag, body } = act else { panic!("expected checkPassword, got {act:?}") };
        assert_eq!(tag, tag::CHECK_PASSWORD);
        let mut r = Reader::new(&body);
        assert_eq!(r.ctor().unwrap(), rpc::AUTH_CHECK_PASSWORD);
        assert_eq!(r.ctor().unwrap(), rpc::INPUT_CHECK_PASSWORD_SRP);
    }

    #[test]
    fn the_password_does_not_outlive_the_derivation() {
        // It is the account. This object is held for the whole login, and a copy of the
        // password sitting in it after the proof is built is a copy with no reason to exist.
        let mut l = login();
        let mut rng = CountingRng(0);
        l.on_error(tag::SIGN_IN, "SESSION_PASSWORD_NEEDED");
        l.submit_password(b"hunter2");
        l.on_reply(tag::GET_PASSWORD, &password_reply(), &mut rng);
        assert!(!l.password.is_empty(), "the password should be held until the kdf runs");
        l.on_kdf([7u8; 32], &mut rng);
        assert!(l.password.is_empty(), "the password outlived the derivation");
    }

    #[test]
    fn a_wrong_password_is_named_rather_than_generic() {
        let mut l = login();
        assert_eq!(
            l.on_error(tag::CHECK_PASSWORD, "PASSWORD_HASH_INVALID"),
            Action::Failed(AuthError::PasswordInvalid)
        );
    }

    /// An `account.password` with two-factor enabled, built through the walker's own table.
    fn password_reply() -> Vec<u8> {
        use crate::schema as sc;
        use crate::walk;

        let p = crate::srp_test_prime();
        let algo = {
            let c = walk::ctor(
                sc::PASSWORDKDFALGOSHA256SHA256PBKDF2HMACSHA512ITER100000SHA256MODPOW_CTOR,
            )
            .unwrap();
            let mut w = Writer::new();
            w.ctor(c.id)
                .bytes(&alloc::vec![0x11u8; 16])
                .bytes(&alloc::vec![0x22u8; 16])
                .int(3)
                .bytes(&p);
            w.finish()
        };

        let c = walk::ctor(sc::ACCOUNT_PASSWORD_CTOR).unwrap();
        let mut w = Writer::new();
        w.ctor(c.id);
        // has_password (bit 2) gates current_algo, srp_B and srp_id; hint is bit 3.
        let flags: u32 = (1 << 2) | (1 << 3);
        let mut word = 0;
        for (i, f) in c.fields.iter().enumerate() {
            if f.k == walk::K_FLAGS {
                w.uint(if word == 0 { flags } else { 0 });
                word += 1;
                continue;
            }
            if f.f >= 0 {
                let bit = (f.f as u16 & 0xff) as u32;
                if flags & (1 << bit) == 0 {
                    continue;
                }
                if f.k == walk::K_TRUE {
                    continue;
                }
            }
            if i == sc::ACCOUNT_PASSWORD_CURRENT_ALGO {
                w.raw(&algo);
            } else if i == sc::ACCOUNT_PASSWORD_SRP_B {
                w.bytes(&alloc::vec![0x07u8; 256]);
            } else if i == sc::ACCOUNT_PASSWORD_SRP_ID {
                w.long(4242);
            } else if i == sc::ACCOUNT_PASSWORD_HINT {
                w.string("o de sempre");
            } else if f.k == walk::K_STRING {
                w.bytes(&[]);
            } else if f.k == walk::K_BOXED {
                // new_algo and new_secure_algo: any valid constructor of the right family.
                w.raw(&algo);
            } else {
                w.int(0);
            }
        }
        w.finish()
    }
}
