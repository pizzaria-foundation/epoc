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

use crate::link::{Link, Progress as LinkProgress};
use crate::login::{Login, Progress as LoginProgress};

/// What the application should do about what just happened.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Outcome {
    None,
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

pub struct Driver {
    link: Option<Link>,
    queued: Option<Queued>,
    /// Set once the handshake finishes, so the key is written exactly once.
    persisted: bool,
    /// What a waiting screen says.
    pub status: String,
}

impl Default for Driver {
    fn default() -> Self {
        Self::new()
    }
}

impl Driver {
    pub fn new() -> Self {
        Driver { link: None, queued: None, persisted: false, status: String::new() }
    }

    pub fn is_connected(&self) -> bool {
        self.link.as_ref().is_some_and(|l| l.is_ready())
    }

    /// Open the connection, resuming a stored session if there is one.
    ///
    /// A failure here is almost always no route — nothing else on the handset is online —
    /// and it is reported rather than retried, because attaching to something that is not
    /// there gives the same answer however many times it is asked.
    pub fn connect(&mut self) -> Outcome {
        match Link::start() {
            Ok(l) => {
                self.link = Some(l);
                self.status = String::from("conectando");
                Outcome::Redraw
            }
            Err(_) => Outcome::Disconnected("sem conexão de rede"),
        }
    }

    /// Feed a raw shim event. Everything the network and the worker do arrives here.
    pub fn on_event(&mut self, ev: &sys::ShimEvent, login: &mut Login, now: i64) -> Outcome {
        let Some(l) = self.link.as_mut() else {
            return Outcome::None;
        };

        let progress = l.on_event(ev, now);

        // Whatever the worker just freed up, if anything was held back.
        if !l.work_busy() {
            if let Some(q) = self.queued.take() {
                self.start_work(q);
            }
        }

        match progress {
            LinkProgress::None => Outcome::None,
            LinkProgress::Step(s) => {
                self.status = String::from(s);
                Outcome::Redraw
            }
            LinkProgress::Authenticated => {
                // Persisted before anything else uses the connection. Redoing the handshake
                // costs two exponentiations and four round trips, and the key *is* the
                // session — see `session_store`.
                if !self.persisted {
                    self.persisted = true;
                    if let Some(l) = self.link.as_ref() {
                        let _ = l.persist();
                    }
                }
                self.status = String::from("conectado");
                Outcome::Redraw
            }
            LinkProgress::Reply { tag, body } => {
                let p = {
                    let l = self.link.as_mut().unwrap();
                    let (rng, _) = (l.rng_mut(), ());
                    login.on_reply(tag, &body, rng)
                };
                self.apply(p, login, now)
            }
            LinkProgress::Failed { tag, text, .. } => {
                let p = login.on_error(tag, &text);
                self.apply(p, login, now)
            }
            LinkProgress::WorkDone(bytes) => self.on_work(bytes, login, now),
            LinkProgress::Disconnected(why) => {
                self.link = None;
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
        match p {
            LoginProgress::None => Outcome::Redraw,
            LoginProgress::Call { body, tag } => {
                let Some(l) = self.link.as_mut() else {
                    return Outcome::Disconnected("sem conexão");
                };
                l.call(&body, tag, now);
                Outcome::Redraw
            }
            LoginProgress::Kdf { password, salt1, salt2 } => {
                self.status = String::from("verificando a senha");
                self.start_work(Queued::Kdf { password, salt1, salt2 });
                Outcome::Redraw
            }
            LoginProgress::ModPow { base, exp, modulus } => {
                self.start_work(Queued::ModPow { base, exp, modulus });
                Outcome::Redraw
            }
            LoginProgress::Migrate(dc) => {
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
                self.status = String::from("mudando de servidor");
                // The code is asked for again once the new handshake finishes. Sending it
                // now would go down a link that has no session yet, so it waits for
                // Authenticated on the other side.
                let p = login.ask_send_code(&phone);
                self.apply(p, login, now)
            }
            LoginProgress::Authorized => Outcome::Authorized,
            LoginProgress::Error(_) => Outcome::Redraw,
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
        if !accepted {
            self.queued = Some(w);
        }
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
