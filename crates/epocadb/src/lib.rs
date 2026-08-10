//! Dev bridge: log streaming and file transfer for Symbian devices.
//!
//! ```text
//! device ──tcp:9091 (cmd)──▶ host   REQ/OK/ERR/DATA protocol
//! device ──tcp:9092 (log)──▶ host   raw line stream, LF-separated
//! device ──UDP broadcast──▶ 255.255.255.255:9093   device discovery beacon
//! ```
//!
//! The device connects out to the host — no `Listen`/`Accept` needed. A single
//! [`Bridge`] owns two [`TcpStream`]s and a [`RingBuffer`] for logs. The caller
//! feeds `ShimEvent`s through [`Bridge::on_event`] and the bridge drains both
//! sockets without blocking.
//!
//! # Both sockets see every event
//!
//! [`TcpStream::on_event`] filters by handle, so handing it an event for the other
//! socket is free. Handing it *no* events is not: the platform only clears
//! `tx_pending` on `SHIM_EV_SENT`, and a stream that never sees its own send
//! completion issues one send and then queues forever. That is why the log socket is
//! driven on every event and not only when something is written to it.
//!
//! # Nothing here blocks, and nothing here spins
//!
//! [`TcpStream::write`] accepts what fits and reports how much, so every send goes
//! through an outbound queue that drains as completions arrive. [`TcpStream::read`]
//! returns 0 when nothing has arrived yet, which is a normal state and not an error —
//! partial reads are kept and resumed on the next event.
//!
//! # Device discovery
//!
//! The beacon opens its own UDP socket as soon as the Bridge is constructed and
//! broadcasts every ~8 seconds regardless of TCP state — it fires during
//! Connecting, Ready and Dead alike. The host's `epocadb devices` listens on udp:9093
//! and prints what it finds.
//!
//! # State machine
//!
//! ```text
//!   Connecting ──(both Connected)──▶ Ready
//!        │  ▲                          │
//!        │  └──────── backoff ─────────┤
//!        ▼                             ▼
//!       Dead ◀──── socket failure, connect timeout, host silence
//! ```
//!
//! Sockets are only opened once a bearer is up: `open_handle` is given the bearer the
//! application already brought up, because opening one on a connection that has not
//! started panics esock rather than failing cleanly. The caller is responsible for not
//! constructing a `Bridge` before then.
//!
//! # Reconnection
//!
//! When a socket fails, the bridge enters Dead and retries with exponential
//! backoff (1 s, 2 s, 4 s, … up to 64 s). Retries continue indefinitely at the
//! max interval — the host might come back up after minutes or hours. The backoff
//! resets when the bridge reaches Ready.
//!
//! Three things can end a session, and all three are on a clock, because the failure
//! mode that costs the most debugging time is the one that just sits there: a connect
//! that never completes, a host that accepted the socket and then stopped answering,
//! and a socket the platform reports as failed.
//!
//! # Polling for commands
//!
//! ```ignore
//! let now = symbian::monotonic_us();
//! bridge.on_event(ev, now);
//! match bridge.poll(now) {
//!     Some(Command::Push { path, size: _ }) => {
//!         bridge.expect_data_header();
//!         pending_path = Some(path);
//!     }
//!     Some(Command::Pull { path }) => {
//!         let data = read_file(&path);
//!         bridge.reply(&alloc::format!("OK {}", data.len()));
//!         let _ = bridge.send_data(&data);
//!     }
//!     _ => {}
//! }
//! // On a later event, once the payload has arrived:
//! if pending_path.is_some() {
//!     if let Some(data) = bridge.read_data() {
//!         write_file(&pending_path.take().unwrap(), &data);
//!         bridge.reply("OK wrote");
//!     }
//! }
//! ```

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod ring;

use alloc::string::String;
use alloc::vec::Vec;

use ring::RingBuffer;

use symbian::net::{Ipv4, Net, Progress, ShimNet, State, TcpStream, UdpSocket};
use symbian::{Error, Result};
use symbian_sys as sys;

pub const DEFAULT_CMD_PORT: u16 = 9091;
pub const DEFAULT_LOG_PORT: u16 = 9092;
pub const BEACON_PORT: u16 = 9093;
pub const LOG_BUFFER_SIZE: usize = 2048;

const SOCKET_BUF: usize = 1024;
const BEACON_INTERVAL_US: u64 = 8_000_000;
const INITIAL_BACKOFF_MS: i32 = 1000;
const MAX_BACKOFF_MS: i32 = 64_000;

/// How often the device asks the host for work. The host answers `OK pong` when it has
/// none, so this is also the granularity of every command.
const PING_INTERVAL_US: u64 = 1_000_000;

/// How long to wait for a reply before assuming it is not coming. A host that was killed
/// mid-session leaves a socket that is open and silent, which no socket error reports.
const REPLY_TIMEOUT_US: u64 = 15_000_000;

/// Unanswered polls before the session is torn down and rebuilt.
const MAX_MISSED_REPLIES: u8 = 4;

/// How long a connect may sit unresolved before it is retried. Without it a connect
/// whose completion never arrives leaves the bridge in Connecting forever.
const CONNECT_TIMEOUT_US: u64 = 30_000_000;

/// Ceiling on bytes queued for the cmd channel. A `pull` of something enormous fails
/// loudly here rather than growing the heap until the allocator gives up — on a phone
/// with 32 MB free, an unbounded queue is a crash with no message.
const MAX_OUT_QUEUE: usize = 256 * 1024;

/// The IPv4 broadcast address, for the discovery beacon.
pub const BROADCAST_ADDR: Ipv4 = Ipv4::new(255, 255, 255, 255);

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Phase {
    Connecting,
    Ready,
    Dead(&'static str),
}

/// A response from the host to a device command request.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Response {
    Ok(Option<String>),
    Err(String),
    Data(usize),
}

/// A command embedded in the host's reply to `REQ PING`.
///
/// When the host wants the device to do something, it answers the poll with a
/// command instead of `OK pong`. The device carries it out and then polls again.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Command {
    None,
    Push { path: String, size: usize },
    Pull { path: String },
    Install { path: String, size: usize },
    /// An application-defined control line, forwarded verbatim. The bridge does not
    /// interpret it — this is how a host tool steers whatever is built on top of the
    /// bridge without epocadb having to know the verbs.
    Control(String),
    Quit,
}

/// The device half of the bridge.
///
/// Generic over [`Net`] so the whole state machine can be driven against a fake in
/// tests; `Bridge` on its own means `Bridge<ShimNet>`, which is what the device builds.
pub struct Bridge<N: Net = ShimNet> {
    net: N,
    host: Ipv4,
    cmd: Option<TcpStream>,
    log: Option<TcpStream>,
    beacon: Option<UdpSocket>,
    log_buf: RingBuffer<LOG_BUFFER_SIZE>,
    phase: Phase,
    bearer_handle: Option<i32>,

    /// Received on the cmd channel and not yet consumed as a line or as payload.
    read_buf: [u8; SOCKET_BUF],
    read_len: usize,

    /// Queued for the cmd channel; drains as sends complete.
    out_cmd: Vec<u8>,
    /// Queued for the log channel. Refilled from `log_buf` only when empty, which
    /// bounds it at one drain.
    out_log: Vec<u8>,

    /// Payload being accumulated for a push/install.
    pending_data: Vec<u8>,
    pending_data_len: usize,
    waiting_data_header: bool,

    /// A `REQ` is in flight. Nothing new goes out until it is answered or times out —
    /// without this the device emits one `REQ PING` per shim event, which is thousands
    /// per minute, and the replies stop lining up with the requests that asked for them.
    awaiting_reply: bool,
    last_ping_us: u64,
    missed_replies: u8,

    last_beacon_us: u64,
    next_retry_us: u64,
    connect_deadline_us: u64,
    backoff_ms: i32,
    /// The `dropped` count already announced to the host, so a gap is reported once.
    reported_dropped: u32,
}

impl Bridge<ShimNet> {
    /// Open two sockets to the host over the shim's network.
    ///
    /// If a bearer is passed the sockets are bound to it, which guarantees they use the
    /// same route that is already up. Without a bearer the sockets use the default route,
    /// which works when another part of the application has already brought one up.
    pub fn connect(host: Ipv4, bearer_handle: Option<i32>) -> Result<Self> {
        Self::connect_with(ShimNet, host, bearer_handle, symbian::monotonic_us())
    }
}

impl<N: Net> Bridge<N> {
    /// Open the bridge over a caller-supplied [`Net`] and clock reading.
    pub fn connect_with(mut net: N, host: Ipv4, bearer_handle: Option<i32>, now_us: u64) -> Result<Self> {
        let conn = bearer_handle.unwrap_or(-1);

        let mut cmd = TcpStream::open_handle(&mut net, conn, SOCKET_BUF, SOCKET_BUF)?;
        cmd.connect(&mut net, host, DEFAULT_CMD_PORT)?;
        let mut log = TcpStream::open_handle(&mut net, conn, SOCKET_BUF, SOCKET_BUF)?;
        log.connect(&mut net, host, DEFAULT_LOG_PORT)?;

        // The beacon is best-effort: a device that cannot broadcast is still a device
        // that can be reached with an explicit host address.
        let beacon = UdpSocket::open(&mut net, conn).ok();

        Ok(Bridge {
            net,
            host,
            cmd: Some(cmd),
            log: Some(log),
            beacon,
            log_buf: RingBuffer::new(),
            phase: Phase::Connecting,
            bearer_handle,
            read_buf: [0u8; SOCKET_BUF],
            read_len: 0,
            out_cmd: Vec::new(),
            out_log: Vec::new(),
            pending_data: Vec::new(),
            pending_data_len: 0,
            waiting_data_header: false,
            awaiting_reply: false,
            last_ping_us: now_us,
            missed_replies: 0,
            last_beacon_us: now_us,
            next_retry_us: 0,
            connect_deadline_us: now_us + CONNECT_TIMEOUT_US,
            backoff_ms: INITIAL_BACKOFF_MS,
            reported_dropped: 0,
        })
    }

    pub fn is_ready(&self) -> bool {
        self.phase == Phase::Ready
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// How many log lines were dropped because the buffer filled faster than the socket
    /// drained it.
    pub fn dropped_logs(&self) -> u32 {
        self.log_buf.dropped
    }

    /// Feed a shim event. `now_us` is [`symbian::monotonic_us`] — passed in rather than
    /// read here so every deadline in this file is testable.
    pub fn on_event(&mut self, ev: &sys::ShimEvent, now_us: u64) {
        self.maybe_beacon(now_us);

        match self.phase {
            Phase::Dead(_) => {
                if now_us >= self.next_retry_us {
                    self.reconnect(now_us);
                }
            }
            Phase::Connecting => self.on_connecting(ev, now_us),
            Phase::Ready => self.on_ready(ev, now_us),
        }
    }

    fn on_connecting(&mut self, ev: &sys::ShimEvent, now_us: u64) {
        if let Some(ref mut cmd) = self.cmd {
            if !matches!(cmd.state(), State::Connected) {
                match cmd.on_event(&mut self.net, ev) {
                    Progress::Failed(_) | Progress::Closed => {
                        self.die("cmd socket failed", now_us);
                        return;
                    }
                    _ => {}
                }
            }
        }

        if let Some(ref mut log) = self.log {
            if !matches!(log.state(), State::Connected) {
                match log.on_event(&mut self.net, ev) {
                    Progress::Failed(_) | Progress::Closed => {
                        self.die("log socket failed", now_us);
                        return;
                    }
                    _ => {}
                }
            }
        }

        let cmd_ok = self.cmd.as_ref().is_some_and(|s| matches!(s.state(), State::Connected));
        let log_ok = self.log.as_ref().is_some_and(|s| matches!(s.state(), State::Connected));

        if cmd_ok && log_ok {
            self.phase = Phase::Ready;
            self.backoff_ms = INITIAL_BACKOFF_MS;
            self.missed_replies = 0;
            self.awaiting_reply = false;
            // Due immediately, so the first command does not wait out an interval.
            self.last_ping_us = now_us.wrapping_sub(PING_INTERVAL_US);
            self.pump_cmd();
            self.flush_logs();
        } else if now_us >= self.connect_deadline_us {
            self.die("connect timed out", now_us);
        }
    }

    fn on_ready(&mut self, ev: &sys::ShimEvent, now_us: u64) {
        // The log socket first, and unconditionally: it needs its own SHIM_EV_SENT to
        // ever send a second time.
        if let Some(ref mut log) = self.log {
            match log.on_event(&mut self.net, ev) {
                Progress::Failed(_) | Progress::Closed => {
                    self.die("log socket died", now_us);
                    return;
                }
                _ => {}
            }
        }

        if let Some(ref mut cmd) = self.cmd {
            match cmd.on_event(&mut self.net, ev) {
                Progress::Failed(_) | Progress::Closed => {
                    self.die("cmd socket died", now_us);
                    return;
                }
                _ => {}
            }
        }

        // A host that went away without closing the socket looks exactly like a host
        // with nothing to say. The difference is only visible on a clock.
        if self.awaiting_reply && now_us.wrapping_sub(self.last_ping_us) >= REPLY_TIMEOUT_US {
            self.awaiting_reply = false;
            self.missed_replies = self.missed_replies.saturating_add(1);
            if self.missed_replies >= MAX_MISSED_REPLIES {
                self.die("host stopped answering", now_us);
                return;
            }
        }

        self.pump_cmd();
        self.flush_logs();
    }

    /// Tear the session down and arm a retry.
    fn die(&mut self, reason: &'static str, now_us: u64) {
        self.schedule_retry(now_us);
        self.phase = Phase::Dead(reason);
        // Everything mid-flight belonged to a stream that no longer exists. Carrying it
        // into the next session would splice one transfer onto another.
        self.awaiting_reply = false;
        self.waiting_data_header = false;
        self.pending_data_len = 0;
        self.pending_data.clear();
        self.out_cmd.clear();
        self.read_len = 0;
        // `out_log` and `log_buf` survive: they are the record of what went wrong, and
        // the next session is where it finally gets read.
    }

    fn reconnect(&mut self, now_us: u64) {
        self.cmd = None;
        self.log = None;

        let conn = self.bearer_handle.unwrap_or(-1);
        match self.open_pair(conn) {
            Ok(()) => {
                self.phase = Phase::Connecting;
                self.connect_deadline_us = now_us + CONNECT_TIMEOUT_US;
            }
            Err(reason) => {
                self.schedule_retry(now_us);
                self.phase = Phase::Dead(reason);
            }
        }
    }

    /// Open and start connecting both sockets, or report which step refused.
    ///
    /// `connect` is checked rather than discarded: a refused connect never produces a
    /// completion event, so swallowing the error parks the bridge in Connecting until
    /// the timeout rather than retrying on the backoff it already has.
    fn open_pair(&mut self, conn: i32) -> core::result::Result<(), &'static str> {
        let mut cmd = TcpStream::open_handle(&mut self.net, conn, SOCKET_BUF, SOCKET_BUF)
            .map_err(|_| "cmd open failed")?;
        cmd.connect(&mut self.net, self.host, DEFAULT_CMD_PORT)
            .map_err(|_| "cmd connect failed")?;

        let mut log = TcpStream::open_handle(&mut self.net, conn, SOCKET_BUF, SOCKET_BUF)
            .map_err(|_| "log open failed")?;
        log.connect(&mut self.net, self.host, DEFAULT_LOG_PORT)
            .map_err(|_| "log connect failed")?;

        self.cmd = Some(cmd);
        self.log = Some(log);
        Ok(())
    }

    fn schedule_retry(&mut self, now_us: u64) {
        self.next_retry_us = now_us + (self.backoff_ms as u64) * 1000;
        self.backoff_ms = self.backoff_ms.saturating_mul(2).min(MAX_BACKOFF_MS);
    }

    // ── the beacon ────────────────────────────────────────────────

    fn maybe_beacon(&mut self, now_us: u64) {
        if now_us.wrapping_sub(self.last_beacon_us) >= BEACON_INTERVAL_US {
            self.send_beacon(now_us);
        }
    }

    fn send_beacon(&mut self, now_us: u64) {
        if let Some(ref mut b) = self.beacon {
            let msg = "EPOCADB 0.2 device=Nokia E72";
            let _ = b.send_to(&mut self.net, BROADCAST_ADDR, BEACON_PORT, msg.as_bytes());
        }
        self.last_beacon_us = now_us;
    }

    // ── logging ───────────────────────────────────────────────────

    pub fn log(&mut self, line: &str) {
        self.log_buf.push(line);
        if self.phase == Phase::Ready {
            self.flush_logs();
        }
    }

    fn flush_logs(&mut self) {
        // Announce a gap in the log as a line in the log. A silent gap reads as "that
        // code never ran", which is the most expensive wrong conclusion available here.
        let dropped = self.log_buf.dropped;
        if dropped != self.reported_dropped {
            self.reported_dropped = dropped;
            self.log_buf.push(&alloc::format!("-- epocadb: {dropped} log line(s) dropped --"));
        }

        // Refill only when drained, which keeps `out_log` at one drain's worth and means
        // the ring — not the heap — is what absorbs a stalled socket.
        if self.out_log.is_empty() {
            let mut scratch = [0u8; SOCKET_BUF];
            let n = self.log_buf.drain_into(&mut scratch);
            if n == 0 {
                return;
            }
            self.out_log.extend_from_slice(&scratch[..n]);
        }

        let Some(log) = self.log.as_mut() else { return };
        if let Ok(n) = log.write(&mut self.net, &self.out_log) {
            self.out_log.drain(..n);
        }
    }

    // ── the command channel ───────────────────────────────────────

    /// Send `REQ PING` on its interval, and return any command the host answered with.
    ///
    /// Returns `None` when there is nothing to report yet — no reply has arrived, or the
    /// interval has not elapsed, or a transfer owns the channel.
    pub fn poll(&mut self, now_us: u64) -> Option<Command> {
        if self.phase != Phase::Ready {
            return None;
        }

        // A transfer owns the channel until it finishes. A `REQ` sent into the middle of
        // one is read by the host as payload.
        if self.push_in_progress() {
            return None;
        }

        if self.awaiting_reply {
            let resp = self.read_response()?;
            self.awaiting_reply = false;
            self.missed_replies = 0;
            return Some(self.dispatch(resp));
        }

        if now_us.wrapping_sub(self.last_ping_us) < PING_INTERVAL_US {
            return None;
        }
        self.last_ping_us = now_us;
        if self.send_line("REQ PING").is_ok() {
            self.awaiting_reply = true;
        }
        None
    }

    fn dispatch(&mut self, resp: Response) -> Command {
        match resp {
            Response::Ok(Some(detail)) => {
                if detail == "pong" {
                    return Command::None;
                }
                match parse_command(&detail) {
                    Some(Command::Quit) => {
                        self.phase = Phase::Dead("host sent quit");
                        Command::Quit
                    }
                    Some(cmd) => cmd,
                    None => {
                        self.log(&alloc::format!("epocadb: unparsed reply: {detail}"));
                        Command::None
                    }
                }
            }
            Response::Ok(None) => Command::None,
            Response::Err(msg) => {
                self.log(&alloc::format!("epocadb: host error: {msg}"));
                Command::None
            }
            Response::Data(_) => Command::None,
        }
    }

    /// Queue a line on the cmd channel. `\r\n` is appended if absent.
    pub fn send_line(&mut self, line: &str) -> Result<()> {
        if self.cmd.is_none() {
            return Err(Error::NotReady);
        }
        let mut buf = String::from(line);
        if !buf.ends_with("\r\n") {
            buf.push_str("\r\n");
        }
        self.enqueue_cmd(buf.as_bytes())
    }

    /// Queue a line, ignoring a full queue. For replies, where there is nothing useful
    /// to do with the error and the session is about to be rebuilt anyway.
    pub fn reply(&mut self, detail: &str) {
        let _ = self.send_line(detail);
    }

    /// Queue a `DATA` header and its payload.
    ///
    /// Both go on the queue in one step, so a transmit queue that fills partway through
    /// cannot separate a header from the bytes it describes.
    pub fn send_data(&mut self, data: &[u8]) -> Result<()> {
        if self.cmd.is_none() {
            return Err(Error::NotReady);
        }
        let header = alloc::format!("DATA {}\r\n", data.len());
        if self.out_cmd.len() + header.len() + data.len() > MAX_OUT_QUEUE {
            return Err(Error::Overflow);
        }
        self.out_cmd.extend_from_slice(header.as_bytes());
        self.out_cmd.extend_from_slice(data);
        self.pump_cmd();
        Ok(())
    }

    fn enqueue_cmd(&mut self, bytes: &[u8]) -> Result<()> {
        if self.out_cmd.len() + bytes.len() > MAX_OUT_QUEUE {
            return Err(Error::Overflow);
        }
        self.out_cmd.extend_from_slice(bytes);
        self.pump_cmd();
        Ok(())
    }

    /// Hand the socket as much of the outbound queue as it will take.
    ///
    /// `write` returns what it accepted, which is less than offered whenever a send is
    /// already in flight — the queue is what keeps the remainder rather than dropping it
    /// and reporting success.
    fn pump_cmd(&mut self) {
        if self.out_cmd.is_empty() {
            return;
        }
        let Some(cmd) = self.cmd.as_mut() else { return };
        if let Ok(n) = cmd.write(&mut self.net, &self.out_cmd) {
            self.out_cmd.drain(..n);
        }
    }

    /// Bytes still queued for the cmd channel.
    pub fn pending_out(&self) -> usize {
        self.out_cmd.len()
    }

    /// When the next reconnection attempt is due, in the same clock `on_event` is given.
    /// Meaningless unless the phase is [`Phase::Dead`].
    pub fn pending_retry_at(&self) -> u64 {
        self.next_retry_us
    }

    // ── reading ───────────────────────────────────────────────────

    /// Pull everything the socket has into `read_buf`.
    fn fill_read_buf(&mut self) {
        let Some(cmd) = self.cmd.as_mut() else { return };
        while self.read_len < self.read_buf.len() {
            match cmd.read(&mut self.net, &mut self.read_buf[self.read_len..]) {
                Ok(0) => break,
                Ok(n) => self.read_len += n,
                Err(_) => break,
            }
        }
    }

    fn consume_read(&mut self, n: usize) {
        let n = n.min(self.read_len);
        self.read_buf.copy_within(n..self.read_len, 0);
        self.read_len -= n;
    }

    /// Take one complete `\r\n`-terminated line, if one has arrived.
    ///
    /// Whatever follows the line stays in `read_buf` — routinely the front of a payload,
    /// since a `DATA` header and its bytes arrive in the same segment.
    fn read_response(&mut self) -> Option<Response> {
        self.fill_read_buf();

        let end = self.read_buf[..self.read_len].windows(2).position(|w| w == b"\r\n");
        let Some(end) = end else {
            // A full buffer with no line in it means the stream is no longer on a line
            // boundary. Holding onto it would wedge every later read against a buffer
            // that can never grow; dropping it at least lets the channel resynchronise.
            if self.read_len == self.read_buf.len() {
                self.read_len = 0;
                self.log_buf.push("-- epocadb: command stream desynchronised, buffer dropped --");
            }
            return None;
        };

        let resp = parse_response_line(&self.read_buf[..end]);
        self.consume_read(end + 2);
        Some(resp)
    }

    /// Signal that the next thing on the cmd channel is a `DATA` header for an incoming
    /// push/install transfer.
    pub fn expect_data_header(&mut self) {
        self.waiting_data_header = true;
        self.pending_data_len = 0;
        self.pending_data.clear();
    }

    /// True while a transfer is in progress.
    pub fn push_in_progress(&self) -> bool {
        self.waiting_data_header || self.pending_data_len > 0
    }

    /// Read a `DATA` header and its payload, accumulating across events.
    ///
    /// Returns `Some(data)` when the transfer is complete and `None` when more events are
    /// needed. Call it on every event while a push is outstanding.
    pub fn read_data(&mut self) -> Option<Vec<u8>> {
        if self.waiting_data_header {
            match self.read_response() {
                Some(Response::Data(n)) => {
                    self.waiting_data_header = false;
                    if n > MAX_OUT_QUEUE {
                        self.log(&alloc::format!("epocadb: refusing {n}-byte transfer"));
                        self.reply(&alloc::format!("ERR transfer too large: {n}"));
                        return None;
                    }
                    self.pending_data = Vec::with_capacity(n);
                    self.pending_data_len = n;
                }
                Some(other) => {
                    self.waiting_data_header = false;
                    self.log(&alloc::format!("epocadb: expected DATA, got {other:?}"));
                    return None;
                }
                None => return None,
            }
        }

        if self.pending_data_len == 0 {
            return None;
        }

        // The payload's first bytes are almost always already here: `read_response`
        // fills `read_buf` greedily, and the host writes `DATA n\r\n` and the bytes back
        // to back, so TCP delivers them together. Reading past them straight from the
        // socket strands them and waits forever for bytes that already arrived.
        let want = self.pending_data_len - self.pending_data.len();
        let take = self.read_len.min(want);
        if take > 0 {
            self.pending_data.extend_from_slice(&self.read_buf[..take]);
            self.consume_read(take);
        }

        while self.pending_data.len() < self.pending_data_len {
            let mut chunk = [0u8; 256];
            let want = (self.pending_data_len - self.pending_data.len()).min(chunk.len());
            let cmd = self.cmd.as_mut()?;
            match cmd.read(&mut self.net, &mut chunk[..want]) {
                // Not an error: the rest has not arrived. Resume on the next event.
                Ok(0) => return None,
                Ok(got) => self.pending_data.extend_from_slice(&chunk[..got]),
                Err(_) => {
                    self.pending_data_len = 0;
                    self.pending_data.clear();
                    return None;
                }
            }
        }

        self.pending_data_len = 0;
        Some(core::mem::take(&mut self.pending_data))
    }
}

// ── protocol parsing ──────────────────────────────────────────────

fn parse_response_line(line: &[u8]) -> Response {
    let text = core::str::from_utf8(line).unwrap_or("");
    if let Some(detail) = text.strip_prefix("OK ") {
        Response::Ok(Some(detail.into()))
    } else if text == "OK" {
        Response::Ok(None)
    } else if let Some(msg) = text.strip_prefix("ERR ") {
        Response::Err(msg.into())
    } else if let Some(count) = text.strip_prefix("DATA ") {
        Response::Data(count.trim().parse().unwrap_or(0))
    } else {
        Response::Err(alloc::format!("unknown response: {text}"))
    }
}

fn parse_command(detail: &str) -> Option<Command> {
    let (verb, args) = match detail.find(' ') {
        Some(i) => (&detail[..i], &detail[i + 1..]),
        None => (detail, ""),
    };

    match verb {
        "pong" => Some(Command::None),
        "PUSH" => {
            let (path, size_str) = split_last(args)?;
            Some(Command::Push { path: path.into(), size: size_str.parse().ok()? })
        }
        "PULL" => {
            if args.is_empty() {
                return None;
            }
            Some(Command::Pull { path: args.into() })
        }
        "INSTALL" => {
            let (path, size_str) = split_last(args)?;
            Some(Command::Install { path: path.into(), size: size_str.parse().ok()? })
        }
        "QUIT" => Some(Command::Quit),
        // Application-defined control, forwarded verbatim. `CTL` with no argument is
        // still a control command (an empty line), not a parse failure.
        "CTL" => Some(Command::Control(args.into())),
        _ => None,
    }
}

fn split_last(s: &str) -> Option<(&str, &str)> {
    let i = s.rfind(' ')?;
    Some((&s[..i], &s[i + 1..]))
}

/// Log a formatted line through the bridge.
#[macro_export]
macro_rules! devlog {
    ($bridge:expr, $($arg:tt)*) => {
        $bridge.log(&::alloc::format!($($arg)*));
    };
}

/// Build a protocol request line.
pub fn build_request(verb: &str, args: &str) -> String {
    let mut s = String::from("REQ ");
    s.push_str(verb);
    if !args.is_empty() {
        s.push(' ');
        s.push_str(args);
    }
    s.push_str("\r\n");
    s
}

#[cfg(test)]
mod protocol_tests;
#[cfg(test)]
mod wire_tests;
