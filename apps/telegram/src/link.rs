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
    /// Waiting for the bearer. **There is no socket yet.**
    ///
    /// Opening one against an `RConnection` that has not come up does not fail — esock
    /// panics the client, and a panicked application on this platform simply vanishes: no
    /// message, no report file, nothing on screen. That is what "it does not even open
    /// when there is no network" was.
    Bearer,
    Connecting,
    Running,
    Dead,
}

pub struct Link {
    net: ShimNet,
    bearer: Bearer,
    /// `None` until the bearer is up. See [`Phase::Bearer`].
    sock: Option<TcpStream>,
    client: Client,
    rng: PlatformRng,
    job: Job,
    /// Whether the job in flight belongs to the caller rather than to the handshake.
    work_is_caller: bool,
    /// Whether the bearer has been retried after a TCP connect failure.
    /// See the `NetProgress::Failed` arm in [`Self::on_event`].
    retried_bearer: bool,
    phase: Phase,
    dc: u8,
    /// Whether the session was loaded from disk rather than handshaked afresh.
    resumed: bool,
    /// Steps the client produced that have not been carried out yet.
    ///
    /// A queue because one event can produce several — a container of updates, or a send
    /// followed by a modpow — and each has to be drained without blocking. Reversed on push
    /// so the oldest pops first.
    todo: Vec<Step>,
    /// Scratch for draining the socket.
    buf: [u8; RX],
    /// Whether `initConnection` has gone out on this connection. See [`Link::call`].
    inited: bool,
    /// The body and tag of the last call, so it can be retried when the clock is
    /// corrected without asking the login machine to regenerate it.
    last_call: Option<(Vec<u8>, u32)>,
    /// Whether the clock has already been corrected once on this connection. See the
    /// `BadMessage` arm: correcting twice means the correction is not working.
    clock_fixed: bool,
    /// How many times the last request has been resent. See [`Link::resend_last`].
    resends: u8,
    /// Wire events worth logging, drained by the driver after every tick.
    ///
    /// Returned rather than logged here, so the driver decides the category: the same note
    /// means `[net]` from the home link and `[dc]` from the file link, and only the caller
    /// knows which one it is draining.
    notes: Vec<(&'static str, Note)>,
}

/// What a note carries besides its name.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Note {
    /// Nothing; the name is the whole message.
    Flag,
    Num(i64),
    /// Hex, for bytes that were supposed to parse and did not.
    Text(String),
}

fn save_iap(iap: u32) -> symbian::Result<()> {
    if iap == 0 { return Ok(()); }
    let mut fs = ShimFs;
    let dir = symbian::fs::private_path(&mut fs)?;
    let path = symbian::fs::Utf16Path::join(dir.as_units(), "iap.bin")?;
    symbian::fs::write_atomic(&mut fs, &path, &iap.to_le_bytes())
}

fn load_iap() -> Option<u32> {
    let mut fs = ShimFs;
    let dir = symbian::fs::private_path(&mut fs).ok()?;
    let path = symbian::fs::Utf16Path::join(dir.as_units(), "iap.bin").ok()?;
    let data = symbian::fs::read(&mut fs, &path).ok()??;
    if data.len() == 4 {
        Some(u32::from_le_bytes(data[..4].try_into().ok()?))
    } else {
        None
    }
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

        let saved_iap = saved.as_ref().and_then(|s| if s.iap != 0 { Some(s.iap) } else { None })
            .or_else(load_iap);
        // Try joining an existing route first — silent, no dialog, near-instant.
        // Only fall back to the prompt or a saved IAP when nothing is online.
        let bearer = Bearer::attach(&mut net)
            .or_else(|_| Bearer::start(&mut net, saved_iap))?;
        let addr = dc_address(dc);

        let usable = saved.filter(|s| s.dc == dc);
        let resumed = usable.is_some();
        let (client, first) = match usable {
            Some(s) => (
                Client::resume(s.auth, s.time_offset, &mut rng),
                // `Step::Ready` by hand, because a resumed client is ready the moment it is
                // built and never produces one — that step only comes out of a handshake
                // finishing.
                //
                // Without it nothing ever reports `Progress::Authenticated`: the status line
                // stays on whatever the connection last said, and the driver never flushes
                // the request the user made while it was connecting. Typing a phone number
                // into a resumed session did exactly nothing, forever.
                Some(Step::Ready),
            ),
            None => {
                let (c, step) = Client::connect(&mut rng);
                (c, Some(step))
            }
        };

        Ok(Link {
            net,
            bearer,
            sock: None,
            client,
            rng,
            job: Job::new(),
            work_is_caller: false,
            retried_bearer: false,
            phase: Phase::Bearer,
            dc,
            resumed,
            todo: first.into_iter().collect(),
            buf: [0u8; RX],
            inited: false,
            last_call: None,
            clock_fixed: false,
            resends: 0,
            notes: alloc::vec![
                ("dc address, low octet", Note::Num((addr.0 & 0xff) as i64)),
                (
                if resumed { "resumed a stored session" } else { "no stored session, handshaking" },
                Note::Flag,
            ),
            ],
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
                iap: self.bearer.iap().unwrap_or(0),
            },
        )
    }

    /// Throw the stored session away.
    ///
    /// And, when the reason means this account is no longer reachable from this handset,
    /// everything cached about it. `UnknownKey` deliberately does not: it is what a data
    /// centre migration and a server-side key rotation look like, and both keep the same
    /// account — wiping the chat list there would cost a full `getDialogs` over GPRS to
    /// rebuild something that was still correct.
    pub fn forget(why: session_store::Invalidate) -> symbian::Result<()> {
        let mut fs = ShimFs;
        if matches!(
            why,
            session_store::Invalidate::LoggedOut
                | session_store::Invalidate::Revoked
                | session_store::Invalidate::Unregistered
        ) {
            crate::store_cache::clear(&mut fs);
        }
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
        if let Some(sock) = self.sock.as_mut() {
            sock.close(&mut self.net);
        }
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

    /// Whether the session was loaded from disk rather than built from a fresh handshake.
    pub fn was_resumed(&self) -> bool {
        self.resumed
    }

    /// The bearer handle, if the bearer is up. For opening supplementary sockets.
    pub fn bearer_handle(&self) -> Option<i32> {
        if self.bearer.is_up() { Some(self.bearer.handle()) } else { None }
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
        // Only on success. Assigning `ok` unconditionally clears the flag when the job is
        // refused — and a refusal means work is already in flight, so clearing it sends
        // *that* result down the handshake path instead of back to the caller.
        if ok {
            self.work_is_caller = true;
        }
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
        if ok {
            self.work_is_caller = true;
        }
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

    pub fn time_offset(&self) -> Option<i64> {
        self.client.time_offset()
    }

    /// Make a call. Only valid once [`Progress::Authenticated`] has been seen.
    /// Send a request. `false` when the session is not up yet and the caller must wait.
    ///
    /// The `false` matters. This used to return `Option` and be called with `?` discarded,
    /// so a request made before the handshake finished vanished — and since the handshake
    /// takes about four seconds on this handset, that is what happens to anyone who types a
    /// phone number quickly. The screen sat on "sending the code" with nothing in flight.
    pub fn call(&mut self, body: &[u8], tag: u32, unix_time: i64) -> bool {
        if !self.client.is_ready() {
            return false;
        }
        // The first request on a connection has to carry `initConnection`, or the server
        // answers CONNECTION_NOT_INITED and nothing else on that connection ever works.
        //
        // Wrapped around the real request rather than sent ahead of it as its own
        // `help.getConfig`: that would be a whole extra round trip, and on this handset a
        // round trip to Telegram is 600 ms of the four seconds a login takes.
        //
        // Per *connection*, not per auth key — a resumed session on a fresh socket needs it
        // again, which is why the flag lives here and starts false in `open`.
        let wrapped;
        let body = if self.inited {
            body
        } else {
            self.inited = true;
            self.note("wrapping the first call in initConnection", Note::Flag);
            wrapped = wrap_first(body);
            &wrapped
        };
        // Store a copy so a clock correction can retry without the caller knowing.
        self.last_call = Some((body.to_vec(), tag));
        match self.client.call(body, tag, unix_time, 0, &mut self.rng) {
            Ok((_, step)) => {
                self.todo.push(step);
                self.drain(unix_time);
                true
            }
            Err(_) => false,
        }
    }

    /// Feed a raw shim event.
    ///
    /// `unix_time` is the caller's clock, needed because MTProto stamps every message and
    /// this layer has no business calling `shim_unix_time` on the protocol's behalf — the
    /// application already knows what time it thinks it is.
    /// Take everything the wire has done since the last call, for the log.
    pub fn take_notes(&mut self) -> Vec<(&'static str, Note)> {
        core::mem::take(&mut self.notes)
    }

    /// Write, when there is something to write to.
    ///
    /// Before the bearer is up there is no socket, and a caller reaching for one is a bug
    /// rather than a condition to handle — but it is a bug that used to be a panic, so it
    /// answers with an error.
    fn write_out(&mut self, bytes: &[u8]) -> symbian::Result<()> {
        match self.sock.as_mut() {
            Some(sock) => sock.write(&mut self.net, bytes).map(|_| ()),
            None => Err(symbian::Error::NotReady),
        }
    }

    /// The read side of the same. A free function in all but name, so the socket and the
    /// scratch buffer can be borrowed at once.
    fn read_into(
        sock: Option<&mut TcpStream>,
        net: &mut ShimNet,
        buf: &mut [u8],
    ) -> symbian::Result<usize> {
        match sock {
            Some(sock) => sock.read(net, buf),
            None => Err(symbian::Error::NotReady),
        }
    }

    /// Events that arrive while the bearer is still coming up.
    ///
    /// The socket is opened here and nowhere else, and only after the platform has said the
    /// connection is up. A failure at this point means nothing was already online and the
    /// user declined to bring anything up — `Bearer` has already offered the access point
    /// dialog by then, which is what every other program on the handset does.
    fn on_bearer_event(&mut self, ev: &sys::ShimEvent) -> Progress {
        match self.bearer.on_event(&mut self.net, ev) {
            Ok(false) => Progress::Step("aguardando rede"),
            Ok(true) => {
                let iap = self.bearer.iap().unwrap_or(0);
                self.note("bearer up, iap", Note::Num(iap as i64));
                // Save the access point immediately so the next launch skips the dialog.
                let _ = save_iap(iap);
                let mut sock = match TcpStream::open(&mut self.net, &self.bearer, RX, TX) {
                    Ok(s) => s,
                    Err(e) => {
                        self.note("socket open failed", Note::Num(e.code() as i64));
                        return self.die("não consegui abrir o socket");
                    }
                };
                let addr = dc_address(self.dc);
                self.note("connecting to dc", Note::Num(self.dc as i64));
                if let Err(e) = sock.connect(&mut self.net, addr, DC_PORT) {
                    self.note("connect failed", Note::Num(e.code() as i64));
                    return self.die("não consegui alcançar o servidor");
                }
                self.sock = Some(sock);
                self.phase = Phase::Connecting;
                Progress::Step("conectando ao Telegram")
            }
            Err(e) => {
                self.note("bearer failed", Note::Num(e.code() as i64));
                self.die("sem conexão de rede")
            }
        }
    }

    fn note(&mut self, what: &'static str, n: Note) {
        // Bounded: a long session must not turn the note buffer into the leak. Twenty is
        // more than one tick ever produces, so in practice nothing is ever dropped.
        if self.notes.len() < 20 {
            self.notes.push((what, n));
        }
    }

    /// The head of a buffer as hex.
    ///
    /// Every static check on the failing handshake came back correct — the framing queues,
    /// the buffers are heap and survive a move, the target is `+strict-align`, and a live
    /// probe showed the server answers `res_pq` even with a year of clock error. What is
    /// left is what actually arrived, so this records it.
    ///
    /// Twenty-four bytes: the 20-byte unencrypted header plus the constructor that follows
    /// it, which is exactly the boundary the parser trips over.
    fn note_head(&mut self, what: &'static str, bytes: &[u8]) {
        let mut hex = String::new();
        for b in bytes.iter().take(24) {
            const D: &[u8; 16] = b"0123456789abcdef";
            hex.push(D[(b >> 4) as usize] as char);
            hex.push(D[(b & 15) as usize] as char);
        }
        self.note(what, Note::Text(hex));
    }

    /// Name the deepest part of a failure, with its number where it has one.
    ///
    /// `describe` gives the screen a sentence. This gives the log the constructor id, which
    /// is the thing that can be looked up in the schema.
    fn note_error(&mut self, e: &tg_proto::client::Error) {
        use tg_proto::client::Error as E;
        use tg_proto::handshake::Error as H;
        use tg_proto::tl::Error as T;
        match e {
            E::Handshake(H::Tl(t)) | E::Rpc(tg_proto::rpc::Error::Tl(t)) => match t {
                T::Truncated => self.note("tl: truncated", Note::Flag),
                T::BadLength => self.note("tl: bad length", Note::Flag),
                T::UnknownConstructor(c) => self.note("tl: unknown ctor", Note::Num(*c as i64)),
                T::Unexpected { want, got } => {
                    self.note("tl: wanted ctor", Note::Num(*want as i64));
                    self.note("tl: got ctor", Note::Num(*got as i64));
                }
            },
            E::Server(c) => self.note("transport error", Note::Num(*c as i64)),
            _ => {}
        }
    }

    pub fn on_event(&mut self, ev: &sys::ShimEvent, unix_time: i64) -> Progress {
        if matches!(self.phase, Phase::Dead) {
            return Progress::None;
        }

        // The worker first: a completion here unblocks the handshake, and checking it
        // before the socket means a modpow finishing in the same tick as a packet arriving
        // is handled in the order the protocol expects.
        // Copied out immediately: the borrow is of the job's own buffer, and everything
        // below — including saying in the log what just happened — needs the Link back.
        let finished = match self.job.on_event(ev) {
            None => None,
            Some(Ok(bytes)) => Some(Ok(bytes.to_vec())),
            Some(Err(e)) => Some(Err(e)),
        };
        if let Some(result) = finished {
            // Whose job was it?
            //
            // The Link owns the only worker, because the shim runs one at a time and two
            // owners would race for the slot -- the handshake needs two exponentiations and
            // SRP needs three plus a five-second derivation. Work the caller submitted comes
            // back as Progress::WorkDone; the handshake's is fed back in here.
            let theirs = core::mem::replace(&mut self.work_is_caller, false);
            self.note(
                if theirs { "worker done, caller's, bytes" } else { "worker done, handshake's, bytes" },
                Note::Num(result.as_ref().map(|b| b.len() as i64).unwrap_or(-1)),
            );
            return match result {
                Ok(bytes) if theirs => Progress::WorkDone(bytes),
                Ok(out) => {
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

        // The bearer, while there is no socket. Attaching answers through the event loop, a
        // failure falls through to the access point dialog, and only a bearer that is
        // actually up gets a socket opened on it.
        let Some(sock) = self.sock.as_mut() else {
            return self.on_bearer_event(ev);
        };

        match sock.on_event(&mut self.net, ev) {
            NetProgress::Connected => {
                self.note("tcp connected", Note::Flag);
                self.phase = Phase::Running;
                let greeting = self.client.greeting();
                if self.write_out(greeting).is_err() {
                    return self.die("could not send the transport greeting");
                }
                // `drain` answers for a fresh session (it has req_pq_multi to send) and for
                // a resumed one (it has the hand-queued `Step::Ready`). The fallback is now
                // only reachable if the queue is empty, which would be a bug rather than a
                // state to describe — so it says so instead of naming a handshake that is
                // not happening.
                self.drain(unix_time).unwrap_or(Progress::Step("conectado, sem nada a enviar"))
            }
            NetProgress::Received(_) => {
                let n = match Self::read_into(self.sock.as_mut(), &mut self.net, &mut self.buf) {
                    Ok(n) => n,
                    Err(_) => return self.die("read failed"),
                };
                // The borrow has to end before `feed` touches `self`, hence the copy. It is
                // a few kilobytes against a protocol that just spent 815 ms on arithmetic.
                let chunk: Vec<u8> = self.buf[..n].to_vec();
                self.note("rx bytes", Note::Num(n as i64));
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
                    Err(e) => {
                        // The bytes first, then the name. A parse failure is only diagnosable
                        // from what was parsed.
                        self.note_head("rx head", &chunk);
                        self.note_error(&e);
                        self.die(describe(&e))
                    }
                }
            }
            NetProgress::Closed => self.die("the server closed the connection"),
            NetProgress::Sent(n) => {
                self.note("tx completed, bytes", Note::Num(n as i64));
                Progress::None
            }
            NetProgress::Failed(_) => {
                if !self.retried_bearer {
                    self.retried_bearer = true;
                    self.sock = None;
                    self.bearer.stop(&mut self.net);
                    match Bearer::start(&mut self.net, None) {
                        Ok(b) => {
                            self.bearer = b;
                            self.phase = Phase::Bearer;
                            return Progress::Step("escolha um ponto de acesso");
                        }
                        Err(e) => {
                            self.note("bearer retry failed", Note::Num(e.code() as i64));
                            return self.die("sem conexão de rede");
                        }
                    }
                }
                self.die("the connection failed")
            }
            _ => Progress::None,
        }
    }

    /// Carry out queued steps until something is worth reporting.
    fn drain(&mut self, unix_time: i64) -> Option<Progress> {
        // Nothing goes out before the socket exists. The queue keeps its contents and is
        // drained by the `Connected` arm, which is also where the transport greeting is
        // written — and the greeting has to precede everything in the queue.
        if self.sock.is_none() {
            return None;
        }
        while let Some(step) = self.todo.pop() {
            match step {
                Step::Send(bytes) => {
                    self.note("tx bytes", Note::Num(bytes.len() as i64));
                    if self.write_out(&bytes).is_err() {
                        return Some(self.die("write failed"));
                    }
                }
                Step::ModPow { base, exp, modulus } => {
                    self.note("handshake modpow, modulus bytes", Note::Num(modulus.len() as i64));
                    // Stated, not assumed. This used to rely on the flag already being
                    // false, which is true only as long as nothing else ever writes it.
                    self.work_is_caller = false;
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
                    if let Some(p) = self.on_update(u, unix_time) {
                        self.flush_acks(unix_time);
                        return Some(p);
                    }
                }
            }
        }
        self.flush_acks(unix_time);
        None
    }

    fn on_update(&mut self, u: Update, unix_time: i64) -> Option<Progress> {
        match u {
            Update::Result { req_msg_id, body, .. } => {
                // Something got through, so the budget for the next request starts fresh.
                self.resends = 0;
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
            // The salt is adopted inside the client, but adopting it is only half of it:
            // `bad_server_salt` means the server *discarded* the message that used the old
            // one. Not resending it loses the request silently — and a stored session is
            // always more than an hour old, which is exactly how long a salt lasts, so this
            // is the ordinary first request of a resumed login rather than a rare case.
            Update::NewSalt { .. } => {
                self.note("server salt replaced", Note::Flag);
                self.resend_last(unix_time)
            }
            // A new session is the server telling us it lost state, not that it threw a
            // message away. Nothing to resend.
            Update::NewSession { .. } => None,
            Update::BadMessage { bad_msg_id, code } => {
                self.note("bad_msg_notification code", Note::Num(code as i64));
                self.note("rejected our msg_id at", Note::Num((bad_msg_id >> 32) as i64));
                if code != 16 && code != 17 {
                    return Some(Progress::Step("o servidor recusou uma mensagem"));
                }

                // 16 and 17 are "your msg_id is outside my window", which on a handset means
                // the clock. This one is 59 days behind, measured.
                //
                // Once. A second rejection after a correction is not a clock problem, and
                // resending into it is an infinite loop that shows a reassuring status line
                // while nothing progresses — which is exactly what it did.
                if self.clock_fixed {
                    return Some(self.die("o relógio do telefone está muito errado"));
                }
                self.clock_fixed = true;

                if !self.client.correct_time() {
                    return Some(self.die("o relógio do telefone está muito errado"));
                }
                self.note("clock corrected to", Note::Num(self.client.server_clock() as i64));
                // Straight to disk. The stored session carries the offset, and a resumed
                // one that carries the old wrong offset spends its first round trip
                // rediscovering this — every launch, since nothing would ever write the
                // corrected value.
                let _ = self.persist();

                let Some((body, tag)) = self.last_call.clone() else {
                    return Some(Progress::Step("relogio ajustado"));
                };
                self.note("retrying after clock fix, tag", Note::Num(tag as i64));
                match self.client.call(&body, tag, unix_time, 0, &mut self.rng) {
                    Ok((_, Step::Send(bytes))) => {
                        // Written here rather than queued: `drain` is not running, this is
                        // inside the update loop it feeds.
                        if self.write_out(&bytes).is_err() {
                            return Some(self.die("write failed"));
                        }
                    }
                    // Anything else means the client would not build the request again, and
                    // silently doing nothing is what makes a status line lie.
                    _ => return Some(self.die("não consegui reenviar depois de ajustar o relógio")),
                }
                Some(Progress::Step("relogio ajustado"))
            }
            _ => None,
        }
    }

    /// Resend the last request, after the server threw it away.
    ///
    /// Bounded. A salt that keeps being replaced, or a server rejecting for some reason the
    /// client cannot see, would otherwise be an endless resend behind a status line that
    /// says everything is fine.
    fn resend_last(&mut self, unix_time: i64) -> Option<Progress> {
        let (body, tag) = self.last_call.clone()?;
        if self.resends >= 3 {
            return Some(self.die("o servidor recusou o pedido várias vezes"));
        }
        self.resends += 1;
        self.note("resending, tag", Note::Num(tag as i64));
        match self.client.call(&body, tag, unix_time, 0, &mut self.rng) {
            Ok((_, Step::Send(bytes))) => {
                if self.write_out(&bytes).is_err() {
                    return Some(self.die("write failed"));
                }
                None
            }
            _ => Some(self.die("não consegui reenviar o pedido")),
        }
    }

    /// Send any outstanding acknowledgements.
    ///
    /// Not optional: an unacknowledged message is resent on a timer, forever, which costs
    /// data and battery on the device least able to spare either.
    fn flush_acks(&mut self, unix_time: i64) {
        if let Some(Step::Send(bytes)) = self.client.pending_ack(unix_time, 0, &mut self.rng) {
            let _ = self.write_out(&bytes);
        }
    }

    fn die(&mut self, why: &'static str) -> Progress {
        self.phase = Phase::Dead;
        if let Some(sock) = self.sock.as_mut() {
            sock.close(&mut self.net);
        }
        self.bearer.stop(&mut self.net);
        Progress::Disconnected(why)
    }
}

/// Name what actually failed.
///
/// This was one string — "the server sent something unreadable" — for every error the whole
/// stack can produce: transport framing, the handshake's eleven steps, the encrypted layer,
/// the RPC unwrapping. On a device with no log that is the same as saying nothing, and it
/// cost a trip to the handset to learn only that *something* was wrong.
///
/// The bearer investigation taught this exact lesson twice — name the error, print the
/// number — and it did not get applied here.
fn describe(e: &tg_proto::client::Error) -> &'static str {
    use tg_proto::client::Error as E;
    use tg_proto::handshake::Error as H;
    match e {
        E::Transport(_) => "erro de enquadramento",
        E::Session(_) => "não consegui decifrar",
        E::Rpc(_) => "resposta ilegível",
        E::NotReady => "ainda não conectado",
        E::Server(c) if *c == -404 => "o servidor esqueceu a chave",
        E::Server(_) => "o servidor recusou",
        E::Handshake(h) => match h {
            H::Tl(_) => "handshake: TL ilegível",
            H::Crypto(_) => "handshake: falha de cripto",
            H::OutOfOrder => "handshake: resposta fora de ordem",
            H::NonceMismatch => "handshake: nonce não confere",
            H::NotFactorable(_) => "handshake: não fatorei o pq",
            H::NoUsableKey => "handshake: nenhuma chave RSA nossa",
            H::ServerRejected => "handshake: servidor recusou os dados",
            H::UnknownDhPrime => "handshake: primo DH desconhecido",
            H::BadDhParams => "handshake: parâmetros DH inválidos",
            H::DhGenFailed => "handshake: geração da chave falhou",
            H::KeyMismatch => "handshake: as chaves não batem",
        },
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

/// Wrap a request so it can be the first one on a connection.
///
/// Telegram wants to know the layer and who is asking before it will answer anything, and
/// it says so with `CONNECTION_NOT_INITED` — which arrives as an ordinary RPC error on a
/// perfectly healthy session, so it reads like the request was wrong rather than missing a
/// preamble.
pub fn wrap_first(query: &[u8]) -> Vec<u8> {
    rpc::init_connection(
        api_id(),
        "Nokia E72",
        "Symbian 9.3",
        env!("CARGO_PKG_VERSION"),
        query,
    )
}

/// The same wrapper around `help.getConfig`, for a connection with nothing else to say.
///
/// Not used by the login, which wraps its own first request instead of spending a round
/// trip on this one.
pub fn hello() -> Vec<u8> {
    wrap_first(&rpc::get_config())
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

    #[test]
    fn wrapping_keeps_the_query_intact_at_the_end() {
        // The server unwraps two layers and runs what is left, so the query has to survive
        // byte for byte. A live session answered CONNECTION_NOT_INITED for want of this,
        // and the error names the missing wrapper rather than anything about the request.
        let query = rpc::get_config();
        let out = wrap_first(&query);
        assert_eq!(&out[..4], &rpc::INVOKE_WITH_LAYER.to_le_bytes());
        assert!(out.ends_with(&query), "the query did not survive the wrapping");
    }
}
