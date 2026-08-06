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
//! # No bearer
//!
//! The socket is opened with no `RConnection`, on whatever route is already up. Six rounds
//! of device testing went into bringing a bearer up before discovering that this works and
//! is the only path with no dialog, no negotiation and nothing that can time out — see
//! `docs/device-notes.md`. If nothing is up, the connect fails and the caller reports it,
//! which is a better failure than a two-minute sweep.

use alloc::string::String;
use alloc::vec::Vec;

use symbian::net::{Bearer, Ipv4, Progress as NetProgress, ShimNet, TcpStream};
use symbian::random::Random;
use symbian::work::{Job, ModPow};
use symbian_sys as sys;

use tg_proto::client::{Client, Step};
use tg_proto::crypto::Rng;
use tg_proto::handshake::AuthKey;
use tg_proto::rpc::{self, Update};

/// DC2, where a client with no stored configuration begins.
///
/// A literal address rather than a name: DNS is another round trip, another failure mode
/// and another dialog on some access points, and Telegram's DC addresses are stable enough
/// that `help.getConfig` is the right way to learn the others.
pub const DC2: Ipv4 = Ipv4::new(149, 154, 167, 51);
pub const DC_PORT: u16 = 443;

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
    phase: Phase,
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
    /// Open a connection and begin a handshake.
    ///
    /// `saved` skips it. The key is worth persisting: redoing the handshake costs two
    /// exponentiations and four round trips, which on this hardware is most of a login.
    pub fn start(saved: Option<(AuthKey, i32)>) -> symbian::Result<Self> {
        let mut net = ShimNet;
        let mut rng = PlatformRng(Random::new()?);

        // No RConnection: see the module docs.
        let bearer = Bearer::none();
        let mut sock = TcpStream::open(&mut net, &bearer, RX, TX)?;
        sock.connect(&mut net, DC2, DC_PORT)?;

        let (client, first) = match saved {
            Some((auth, offset)) => (Client::resume(auth, offset, &mut rng), None),
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
            phase: Phase::Connecting,
            todo: first.into_iter().collect(),
            buf: [0u8; RX],
        })
    }

    pub fn is_ready(&self) -> bool {
        self.client.is_ready()
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
            return match result {
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
                        // The server has forgotten our key. Not recoverable in place: the
                        // stored key is worthless and the handshake must be redone.
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
    if opcode != symbian::work::OP_MODPOW {
        return sys::SHIM_ERR_NOT_SUPPORTED;
    }
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

/// Build the first call a connection makes.
///
/// `initConnection` is required once per connection; `help.getConfig` needs no login and
/// makes a good first request, since a reply to it proves the encrypted layer end to end
/// before a phone number is involved.
pub fn hello() -> Vec<u8> {
    rpc::init_connection(
        // api_id and api_hash identify the application and come from my.telegram.org. The
        // placeholder is enough for help.getConfig and is not enough to log in; see the
        // tg-proto README on why there is no default worth shipping.
        6,
        "Nokia E72",
        "Symbian 9.3",
        env!("CARGO_PKG_VERSION"),
        &rpc::get_config(),
    )
}

/// Bytes for storing an auth key, and the offset that goes with it.
///
/// A fixed 280-byte record rather than anything parsed: this is written with
/// `symbian::fs::write_atomic` and read back on the next launch, and a format with a parser
/// is a format that can half-load. The magic distinguishes a real record from a truncated
/// or foreign file, which on a phone is the difference between "log in again" and a panic.
pub const STORE_MAGIC: u32 = 0x7467_4b31; // "tgK1"

pub fn encode_key(auth: &AuthKey, offset: i32) -> Vec<u8> {
    let mut v = Vec::with_capacity(268);
    v.extend_from_slice(&STORE_MAGIC.to_be_bytes());
    v.extend_from_slice(&auth.key);
    v.extend_from_slice(&auth.id.to_be_bytes());
    v.extend_from_slice(&auth.salt);
    v.extend_from_slice(&offset.to_be_bytes());
    v
}

pub fn decode_key(bytes: &[u8]) -> Option<(AuthKey, i32)> {
    if bytes.len() != 280 || u32::from_be_bytes(bytes[0..4].try_into().ok()?) != STORE_MAGIC {
        return None;
    }
    let mut key = [0u8; 256];
    key.copy_from_slice(&bytes[4..260]);
    let id = u64::from_be_bytes(bytes[260..268].try_into().ok()?);
    let mut salt = [0u8; 8];
    salt.copy_from_slice(&bytes[268..276]);
    let offset = i32::from_be_bytes(bytes[276..280].try_into().ok()?);
    // server_time is deliberately not stored. It exists only so the offset can be computed
    // once, and an absolute time from a previous launch is wrong by however long the phone
    // has been off -- keeping it would invite someone to use it.
    Some((AuthKey { key, id, salt, server_time: 0 }, offset))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_key() -> AuthKey {
        let mut k = [0u8; 256];
        for (i, b) in k.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(3);
        }
        AuthKey { key: k, id: 0xfeed_face_dead_beef, salt: [7; 8], server_time: 123 }
    }

    #[test]
    fn a_stored_key_round_trips() {
        let bytes = encode_key(&a_key(), -42);
        assert_eq!(bytes.len(), 280, "the record is a fixed width and decode checks it");
        let (back, offset) = decode_key(&bytes).expect("decode rejected what encode produced");
        assert_eq!(back.key, a_key().key);
        assert_eq!(back.id, a_key().id);
        assert_eq!(back.salt, a_key().salt);
        assert_eq!(offset, -42);
    }

    #[test]
    fn a_foreign_or_truncated_record_is_refused() {
        // The failure this prevents: reading half a key and using it. The handshake would
        // then "succeed" and every message afterwards would be rejected, which looks like
        // the network rather than like storage.
        let good = encode_key(&a_key(), 0);
        assert!(decode_key(&good[..good.len() - 1]).is_none());
        assert!(decode_key(&[]).is_none());
        let mut wrong = good.clone();
        wrong[0] ^= 0xff;
        assert!(decode_key(&wrong).is_none());
    }

    #[test]
    fn the_first_call_is_init_connection() {
        let out = hello();
        assert_eq!(&out[..4], &rpc::INVOKE_WITH_LAYER.to_le_bytes());
    }
}
