//! The connection, on a phone.
//!
//! `tg-proto` knows the protocol and touches nothing. `symbian` knows the platform and
//! knows no protocol. This is the seam: it owns a socket, a worker thread, a random source
//! and a `Client`, and turns raw shim events into [`Progress`].
//!
//! ```text
//!   RawEvent ──▶ Link::on_event ──▶ Progress
//! ```
//!
//! # Everything is one tick
//!
//! Avkon owns the scheduler and `rust_step` must return in milliseconds, so nothing here
//! blocks. A login is perhaps a dozen events: DNS, connect, four handshake round trips, two
//! worker completions, then the encrypted calls. Each one does a little and returns.
//!
//! The two exponentiations go to [`symbian::work::Job`], which is why a login takes about
//! four seconds of wall time on an E72 and the interface stays alive through all of it.
//!
//! # The bearer
//!
//! [`symbian::net::Bearer::attach`] joins a connection that is already up — the browser's,
//! or whatever else is online. Synchronous underneath, no dialog, nothing to time out, and
//! `NotFound` when there is nothing to join.
//!
//! An earlier version opened the socket with **no** `RConnection` and called that "use
//! whatever route is up". It is not: that path uses the handset's *configured default
//! connection*, and on one with none it reports success and then never connects. It cost
//! two device runs. `docs/device-notes.md` has the account.

use alloc::string::String;
use alloc::vec::Vec;

use symbian::fs::ShimFs;
use symbian::net::{Bearer, Ipv4, Progress as NetProgress, ShimNet, TcpStream};
use symbian::random::Random;
use symbian::work::{Job, ModPow};
use symbian_sys as sys;

use tg_proto::client::{Client, Step};
use tg_proto::crypto::Rng;
use tg_proto::handshake::AuthKey;
use tg_proto::rpc::{self, Update};

use crate::session_store;

/// DC2, where a client with no stored configuration begins.
///
/// A literal address rather than a name: DNS is another round trip, another failure mode
/// and another dialog on some access points, and Telegram's DC addresses are stable enough
/// that `help.getConfig` is the right way to learn the others.
pub const DC_PORT: u16 = 443;

/// Telegram's production data centres, by number.
///
/// Hardcoded rather than read from `help.getConfig`: `config#cc1a241e` is a sixty-field
/// constructor with sixteen flag-conditional members, and these five addresses have been
/// stable for years. Parsing it to learn one address would be the largest structure in the
/// crate, written to avoid a five-line table.
///
/// Index 0 is unused so `DC_ADDRESSES[n]` is data centre `n`.
pub const DC_ADDRESSES: [Ipv4; 6] = [
    Ipv4::new(149, 154, 167, 51), // 0: unused, aliased to DC2 so a bad index still connects
    Ipv4::new(149, 154, 175, 53),
    Ipv4::new(149, 154, 167, 51),
    Ipv4::new(149, 154, 175, 100),
    Ipv4::new(149, 154, 167, 91),
    Ipv4::new(91, 108, 56, 130),
];

/// Where a client with no stored session begins.
pub const DEFAULT_DC: u8 = 2;

pub fn dc_address(dc: u8) -> Ipv4 {
    DC_ADDRESSES[(dc as usize).min(DC_ADDRESSES.len() - 1)]
}

/// Receive buffer. A `Config` reply is about a kilobyte compressed and the transport
/// reassembles across reads, so this only has to be larger than one socket delivery.
const RX: usize = 4096;
const TX: usize = 4096;

/// `symbian::random::Random` as the protocol's [`Rng`].
///
/// A newtype rather than an impl on `Random` itself, because `symbian` must not depend on
/// `tg-proto` — the platform crate knowing about one application's protocol would be the
/// wrong direction entirely.
pub struct PlatformRng(pub Random);

impl Rng for PlatformRng {
    fn fill(&mut self, out: &mut [u8]) {
        self.0.fill(out);
    }
}

/// What the caller should show.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Progress {
    /// Nothing observable changed.
    None,
    /// A step finished; the text is for a status line.
    Step(&'static str),
    /// An auth key exists. Persist it.
    Authenticated,
    /// A reply to a call made with [`Link::call`].
    Reply { tag: u32, body: Vec<u8> },
    /// The result of work submitted through [`Link::submit_modpow`] or
    /// [`Link::submit_kdf`]. Never the handshake's own, which is handled internally.
    WorkDone(Vec<u8>),
    /// The server refused a call.
    Failed { tag: u32, code: i32, text: String },
    /// The connection is gone. Everything must be rebuilt.
    Disconnected(&'static str),
}

enum Phase {
    Connecting,
    Running,
    Dead,
}

pub struct Link {
    net: ShimNet,
    bearer: Bearer,
    sock: TcpStream,
    client: Client,
    rng: PlatformRng,
    job: Job,
    /// Whether the job in flight belongs to the caller rather than to the handshake.
    work_is_caller: bool,
    phase: Phase,
    dc: u8,
    /// Steps the client produced that have not been carried out yet.
    ///
    /// A queue because one event can produce several — a container of updates, or a send
    /// followed by a modpow — and each has to be drained without blocking. Reversed on push
    /// so the oldest pops first.
    todo: Vec<Step>,
    /// Scratch for draining the socket.
    buf: [u8; RX],
}

impl Link {
    /// Open a connection, resuming a stored session if there is one.
    ///
    /// This is what "staying logged in" is. Being signed in to Telegram is not a token the
    /// client holds — it is a property the server attaches to an auth key — so a key read
    /// back from disk *is* the session, and the handshake is skipped entirely.
    ///
    /// It is also most of a login by cost: two exponentiations at 815 ms each and four round
    /// trips, against one file read.
    pub fn start() -> symbian::Result<Self> {
        let mut fs = ShimFs;
        let saved = session_store::load(&mut fs);
        let dc = saved.as_ref().map(|s| s.dc).unwrap_or(DEFAULT_DC);
        Self::open(dc, saved)
    }

    /// Connect to `dc`, using `saved` if it belongs there.
    ///
    /// A key belongs to one data centre. Carrying one to another answers `-404`, so a
    /// mismatch discards it and handshakes afresh rather than spending a round trip finding
    /// out — which is also what happens on `PHONE_MIGRATE_n`.
    pub fn open(dc: u8, saved: Option<session_store::Stored>) -> symbian::Result<Self> {
        let mut net = ShimNet;
        let mut rng = PlatformRng(Random::new()?);

        // Join whatever is already up. If nothing is, this fails with NotFound and the
        // caller has to bring a bearer up -- which is a real answer, unlike the path this
        // replaced, which reported success and then never connected.
        let bearer = Bearer::attach(&mut net)?;
        let mut sock = TcpStream::open(&mut net, &bearer, RX, TX)?;
        sock.connect(&mut net, dc_address(dc), DC_PORT)?;

        let usable = saved.filter(|s| s.dc == dc);
        let (client, first) = match usable {
            Some(s) => (Client::resume(s.auth, s.time_offset, &mut rng), None),
            None => {
                let (c, step) = Client::connect(&mut rng);
                (c, Some(step))
            }
        };

        Ok(Link {
            net,
            bearer,
            sock,
            client,
            rng,
            job: Job::new(),
            work_is_caller: false,
            phase: Phase::Connecting,
            dc,
            todo: first.into_iter().collect(),
            buf: [0u8; RX],
        })
    }

    /// Which data centre this link is talking to.
    pub fn dc(&self) -> u8 {
        self.dc
    }

    /// Write the negotiated session to disk.
    ///
    /// Called on [`Progress::Authenticated`]. Failing to save is not failing to connect —
    /// the session works for this run either way — so the error is returned rather than
    /// killing the link, and a caller that ignores it gets a client that logs in every
    /// launch instead of one that does not work.
    pub fn persist(&self) -> symbian::Result<()> {
        let auth = self.client.auth_key().ok_or(symbian::Error::NotFound)?;
        let mut fs = ShimFs;
        session_store::save(
            &mut fs,
            &session_store::Stored {
                dc: self.dc,
                auth: auth.clone(),
                salt: auth.salt,
                time_offset: self.client.time_offset().unwrap_or(0),
            },
        )
    }

    /// Throw the stored session away.
    pub fn forget(why: session_store::Invalidate) -> symbian::Result<()> {
        let mut fs = ShimFs;
        session_store::clear(&mut fs, why)
    }

    /// Move to another data centre.
    ///
    /// Not a reconnection: an auth key belongs to one data centre, so this is a new socket,
    /// a new handshake and a new key. Two exponentiations and four round trips — about four
    /// seconds on this hardware — which is why the result is worth persisting and why the
    /// stored session records which data centre it came from.
    ///
    /// The old link is closed here rather than left to `Drop`, because both would otherwise
    /// hold a socket at once and the shim has eight.
    pub fn migrate(&mut self, dc: u8) -> symbian::Result<()> {
        self.sock.close(&mut self.net);
        self.bearer.stop(&mut self.net);

        // Any stored key was for the previous data centre and is now useless. Discarding it
        // here means the next launch starts at the right one instead of learning this again.
        let _ = Self::forget(session_store::Invalidate::UnknownKey);

        let fresh = Self::open(dc, None)?;
        *self = fresh;
        Ok(())
    }

    pub fn is_ready(&self) -> bool {
        self.client.is_ready()
    }

    /// Run an exponentiation on the worker, for a caller with its own state machine.
    ///
    /// SRP needs three of these and the handshake needs two, and the shim's worker takes
    /// one job at a time — so they share this rather than each holding a `Job` and racing.
    /// The result arrives as [`Progress::WorkDone`]. `false` when the worker is busy, and
    /// then the caller must hold the work and try again.
    pub fn submit_modpow(&mut self, base: &[u8], exp: &[u8], modulus: &[u8]) -> bool {
        if self.job.is_busy() {
            return false;
        }
        let ok = self.job.submit(&ModPow { base, exp, modulus }).is_ok();
        self.work_is_caller = ok;
        ok
    }

    /// Run the two-factor key derivation on the worker.
    ///
    /// 4.9 s on an E72, measured. On the GUI thread that is five seconds of frozen window
    /// server — the whole phone, not just this application.
    pub fn submit_kdf(&mut self, password: &[u8], salt1: &[u8], salt2: &[u8]) -> bool {
        if self.job.is_busy() {
            return false;
        }
        let ok = self
            .job
            .submit_kdf(&symbian::work::Kdf { password, salt1, salt2 })
            .is_ok();
        self.work_is_caller = ok;
        ok
    }

    pub fn work_busy(&self) -> bool {
        self.job.is_busy()
    }

    /// The random source. SRP needs one for its own secret and the Link owns the only one.
    pub fn rng_mut(&mut self) -> &mut PlatformRng {
        &mut self.rng
    }

    pub fn auth_key(&self) -> Option<&AuthKey> {
        self.client.auth_key()
    }

    pub fn time_offset(&self) -> Option<i32> {
        self.client.time_offset()
    }

    /// Make a call. Only valid once [`Progress::Authenticated`] has been seen.
    pub fn call(&mut self, body: &[u8], tag: u32, unix_time: i64) -> Option<Progress> {
        let (_, step) = self.client.call(body, tag, unix_time, 0, &mut self.rng).ok()?;
        self.todo.push(step);
        self.drain(unix_time)
    }

    /// Feed a raw shim event.
    ///
    /// `unix_time` is the caller's clock, needed because MTProto stamps every message and
    /// this layer has no business calling `shim_unix_time` on the protocol's behalf — the
    /// application already knows what time it thinks it is.
    pub fn on_event(&mut self, ev: &sys::ShimEvent, unix_time: i64) -> Progress {
        if matches!(self.phase, Phase::Dead) {
            return Progress::None;
        }

        // The worker first: a completion here unblocks the handshake, and checking it
        // before the socket means a modpow finishing in the same tick as a packet arriving
        // is handled in the order the protocol expects.
        if let Some(result) = self.job.on_event(ev) {
            // Whose job was it?
            //
            // The Link owns the only worker, because the shim runs one at a time and two
            // owners would race for the slot -- the handshake needs two exponentiations and
            // SRP needs three plus a five-second derivation. Work the caller submitted comes
            // back as Progress::WorkDone; the handshake's is fed back in here.
            let theirs = core::mem::replace(&mut self.work_is_caller, false);
            return match result {
                Ok(bytes) if theirs => Progress::WorkDone(bytes.to_vec()),
                Ok(bytes) => {
                    let out = bytes.to_vec();
                    match self.client.on_modpow(&out, &mut self.rng) {
                        Ok(steps) => {
                            self.todo.extend(steps.into_iter().rev());
                            self.drain(unix_time).unwrap_or(Progress::Step("key material"))
                        }
                        Err(_) => self.die("the handshake rejected the exponentiation"),
                    }
                }
                Err(_) => self.die("the worker thread failed"),
            };
        }

        match self.sock.on_event(&mut self.net, ev) {
            NetProgress::Connected => {
                self.phase = Phase::Running;
                // The transport greeting must precede everything, including the first
                // handshake message, and it is not part of any frame.
                let greeting = self.client.greeting();
                if self.sock.write(&mut self.net, greeting).is_err() {
                    return self.die("could not send the transport greeting");
                }
                self.drain(unix_time).unwrap_or(Progress::Step("connected"))
            }
            NetProgress::Received(_) => {
                let n = match self.sock.read(&mut self.net, &mut self.buf) {
                    Ok(n) => n,
                    Err(_) => return self.die("read failed"),
                };
                // The borrow has to end before `feed` touches `self`, hence the copy. It is
                // a few kilobytes against a protocol that just spent 815 ms on arithmetic.
                let chunk: Vec<u8> = self.buf[..n].to_vec();
                match self.client.feed(&chunk, &mut self.rng) {
                    Ok(steps) => {
                        self.todo.extend(steps.into_iter().rev());
                        self.drain(unix_time).unwrap_or(Progress::None)
                    }
                    Err(tg_proto::client::Error::Server(-404)) => {
                        // The server has forgotten the key. Not recoverable in place, and
                        // the stored copy is worthless -- keeping it means every launch
                        // spends a round trip rediscovering that.
                        let _ = Self::forget(session_store::Invalidate::UnknownKey);
                        self.die("the server no longer knows this auth key")
                    }
                    Err(_) => self.die("the server sent something unreadable"),
                }
            }
            NetProgress::Closed => self.die("the server closed the connection"),
            NetProgress::Failed(_) => self.die("the connection failed"),
            _ => Progress::None,
        }
    }

    /// Carry out queued steps until something is worth reporting.
    fn drain(&mut self, unix_time: i64) -> Option<Progress> {
        while let Some(step) = self.todo.pop() {
            match step {
                Step::Send(bytes) => {
                    if self.sock.write(&mut self.net, &bytes).is_err() {
                        return Some(self.die("write failed"));
                    }
                }
                Step::ModPow { base, exp, modulus } => {
                    let job = ModPow { base: &base, exp: &exp, modulus: &modulus };
                    if self.job.submit(&job).is_err() {
                        return Some(self.die("the worker thread would not take the job"));
                    }
                    // Deliberately reported: it is the slow part of a login, and a status
                    // line that goes quiet for four seconds reads as a freeze.
                    return Some(Progress::Step("computing the key"));
                }
                Step::Ready => {
                    // Acks first, so the ack for anything already received goes out before
                    // the caller starts making calls of its own.
                    self.flush_acks(unix_time);
                    return Some(Progress::Authenticated);
                }
                Step::Update(u) => {
                    if let Some(p) = self.on_update(u) {
                        self.flush_acks(unix_time);
                        return Some(p);
                    }
                }
            }
        }
        self.flush_acks(unix_time);
        None
    }

    fn on_update(&mut self, u: Update) -> Option<Progress> {
        match u {
            Update::Result { req_msg_id, body, .. } => {
                let tag = self.client.tag_of(req_msg_id)?;
                Some(Progress::Reply { tag, body })
            }
            Update::RpcError { req_msg_id, code, text } => {
                // Some errors mean the key itself is dead: the user ended the session from
                // another device, or the account was deactivated. Those have to reach the
                // stored copy, or the next launch resumes a session the server has already
                // thrown away and the client retries forever.
                if let Some(why) = session_store::invalidating(&text) {
                    let _ = Self::forget(why);
                }
                let tag = self.client.tag_of(req_msg_id).unwrap_or(0);
                Some(Progress::Failed { tag, code, text })
            }
            // The salt is adopted inside the client; nothing for the UI to say about it.
            Update::NewSalt { .. } | Update::NewSession { .. } => None,
            Update::BadMessage { code, .. } => {
                // 16 and 17 mean the msg_id was outside the server's window, which on a
                // handset means the clock. Not fatal, and not silently ignorable either.
                Some(Progress::Step(if code == 16 || code == 17 {
                    "the clock is out of step with the server"
                } else {
                    "the server rejected a message"
                }))
            }
            _ => None,
        }
    }

    /// Send any outstanding acknowledgements.
    ///
    /// Not optional: an unacknowledged message is resent on a timer, forever, which costs
    /// data and battery on the device least able to spare either.
    fn flush_acks(&mut self, unix_time: i64) {
        if let Some(Step::Send(bytes)) = self.client.pending_ack(unix_time, 0, &mut self.rng) {
            let _ = self.sock.write(&mut self.net, &bytes);
        }
    }

    fn die(&mut self, why: &'static str) -> Progress {
        self.phase = Phase::Dead;
        self.sock.close(&mut self.net);
        self.bearer.stop(&mut self.net);
        Progress::Disconnected(why)
    }
}

/// The worker thread's side of a [`ModPow`].
///
/// Wired to `rust_work` through `symbian_app::entry!`, so it runs on the worker with its
/// own heap. Nothing it allocates may escape: the output buffer belongs to the caller and
/// this only writes into it.
pub fn work(opcode: i32, input: &[u8], out: &mut [u8]) -> i32 {
    match opcode {
        symbian::work::OP_MODPOW => {
            let Some((base, exp, modulus)) = symbian::work::decode_modpow(input) else {
                return sys::SHIM_ERR_ARGUMENT;
            };
            let Ok(m) = symbian_crypto::Modulus::new(modulus) else {
                return sys::SHIM_ERR_ARGUMENT;
            };
            // `out` is the GUI thread's buffer and modpow writes into it directly. A Vec here would
            // be allocated on the worker's heap and freed on the GUI thread's, which is a cross-heap
            // free and silent corruption rather than a clean failure.
            match symbian_crypto::modpow(base, exp, &m, out) {
                Ok(()) => sys::SHIM_OK,
                Err(_) => sys::SHIM_ERR_ARGUMENT,
            }
        }
        symbian::work::OP_KDF => {
            let Some((password, salt1, salt2)) = symbian::work::decode_kdf(input) else {
                return sys::SHIM_ERR_ARGUMENT;
            };
            if out.len() < 32 {
                return sys::SHIM_ERR_ARGUMENT;
            }
            let x = tg_proto::srp::derive_x(password, salt1, salt2);
            out[..32].copy_from_slice(&x);
            sys::SHIM_OK
        }
        _ => sys::SHIM_ERR_NOT_SUPPORTED,
    }
}

/// Build the first call a connection makes.
///
/// `initConnection` is required once per connection; `help.getConfig` needs no login and
/// makes a good first request, since a reply to it proves the encrypted layer end to end
/// before a phone number is involved.
pub fn hello() -> Vec<u8> {
    rpc::init_connection(
        api_id(),
        "Nokia E72",
        "Symbian 9.3",
        env!("CARGO_PKG_VERSION"),
        &rpc::get_config(),
    )
}

/* The application's credentials.
 *
 * Read at build time from `apps/telegram/api.conf`, which is gitignored: `api_id` and
 * `api_hash` identify the *application* rather than any user, and Telegram bans pairs that
 * turn up in public repositories -- so a committed one is a client that stops working for
 * everyone at once.
 *
 * `option_env!` rather than `env!` so a tree without the file still builds. What that
 * produces is a binary whose `auth.sendCode` answers API_ID_INVALID, which is a legible
 * failure and in fact a useful test result: it proves the request was built, encrypted,
 * routed and understood by Telegram. */

/// The application id, or `0` when the build had no credentials.
pub fn api_id() -> i32 {
    match option_env!("TG_API_ID") {
        Some(s) => parse_i32(s),
        None => 0,
    }
}

pub fn api_hash() -> &'static str {
    option_env!("TG_API_HASH").unwrap_or("")
}

/// Whether this build can log in at all.
///
/// Worth asking before showing a phone-number field: a login that cannot succeed should
/// say so on the screen rather than after the user has typed a number and waited.
pub fn has_credentials() -> bool {
    api_id() != 0 && !api_hash().is_empty()
}

/// `str::parse` is not const and `option_env!` gives a `&str`; this runs once.
fn parse_i32(s: &str) -> i32 {
    let mut v: i32 = 0;
    for b in s.as_bytes() {
        if !b.is_ascii_digit() {
            return 0;
        }
        v = v.saturating_mul(10).saturating_add((b - b'0') as i32);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_call_is_init_connection() {
        let out = hello();
        assert_eq!(&out[..4], &rpc::INVOKE_WITH_LAYER.to_le_bytes());
    }
}
