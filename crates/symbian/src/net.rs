//! TCP, UDP and DNS: safe wrappers, and the state machine that drives them.
//!
//! # Why a trait, again
//!
//! Same reason as [`crate::fs`], and more so. The shim's socket API is a stream of
//! completion events carrying a handle, and the logic on top of it is where the bugs
//! live:
//!
//! - a `RECV` completion delivers *whatever arrived*, not what was asked for, so
//!   reading a length-prefixed frame means accumulating across several of them;
//! - events carry a handle, and a program with two sockets open will route one to the
//!   wrong socket if nobody checks — a bug that only appears with concurrency;
//! - a close or an error can arrive while a send is outstanding, and the pending send
//!   must be abandoned rather than reported as delivered;
//! - the receive buffer can fill, at which point issuing another read would overflow it.
//!
//! None of that needs a phone. Behind [`Net`], [`ShimNet`] is the FFI and `FakeNet` (in
//! the tests) replays event sequences, including the orderings a real network produces
//! rarely and a test can produce every time.
//!
//! # Buffers
//!
//! The shim holds a descriptor over the caller's memory for the duration of a request —
//! it does not copy — so a buffer freed or moved while a request is outstanding is read
//! by the socket server after the fact.
//!
//! [`TcpStream`] closes that by owning its buffers as `Box<[u8]>` and cancelling in
//! `Drop`. A `Box`'s contents do not move when the `Box` does, so the pointers the shim
//! holds stay valid even if the stream itself is moved.

use alloc::boxed::Box;
use alloc::vec;

use symbian_sys as sys;

use crate::error::{Error, Result};

/// Which access point to use.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Iap {
    /// Let the OS ask the user. The right choice on a first run, and the wrong one on
    /// every subsequent run — see [`Bearer`].
    Prompt,
    /// Take the configured default without asking.
    Default,
    /// A specific access point, from a previous [`Bearer::iap`].
    Id(u32),
    /// Join a connection that is already up, rather than negotiating one.
    ///
    /// The cheapest strategy and the one that should usually work: if anything else on the
    /// handset is online, this joins it. Synchronous underneath, no dialog, nothing to time
    /// out — and [`Error::NotFound`] when there is nothing to join, which is the signal to
    /// try [`Iap::Prompt`].
    Attach,
}

impl Iap {
    fn raw(self) -> i32 {
        match self {
            Iap::Prompt => sys::SHIM_IAP_PROMPT,
            Iap::Default => sys::SHIM_IAP_DEFAULT,
            Iap::Id(id) => id as i32,
            Iap::Attach => sys::SHIM_IAP_ATTACH,
        }
    }
}

/// How many connections are up on the handset right now.
///
/// The one query that separates "nothing is online" from "we cannot join what is". Both
/// look identical from a socket that never connects, and telling them apart took three
/// device runs when there was no way to ask.
pub fn connections_up() -> Result<u32> {
    let n = unsafe { sys::shim_net_connections() };
    if n < 0 {
        return Err(Error::from_code(n));
    }
    Ok(n as u32)
}

/// The access point behind connection `index`, one-based.
pub fn connection_iap(index: u32) -> Result<u32> {
    let mut iap = -1i32;
    Error::check(unsafe { sys::shim_net_connection_iap(index as i32, &mut iap) })?;
    if iap < 0 {
        return Err(Error::NotFound);
    }
    Ok(iap as u32)
}

/// An IPv4 address, host byte order — which is what the shim's ABI carries and what
/// `TInetAddr::Address()` returns.
#[derive(Copy, Clone, PartialEq, Eq)]
pub struct Ipv4(pub u32);

impl Ipv4 {
    pub const fn new(a: u8, b: u8, c: u8, d: u8) -> Self {
        Ipv4(((a as u32) << 24) | ((b as u32) << 16) | ((c as u32) << 8) | d as u32)
    }

    pub const fn octets(self) -> [u8; 4] {
        [
            (self.0 >> 24) as u8,
            (self.0 >> 16) as u8,
            (self.0 >> 8) as u8,
            self.0 as u8,
        ]
    }

    /// Parse dotted-quad. `None` for anything that is not exactly four decimal octets —
    /// including a hostname, which is how a caller decides whether it needs DNS at all.
    pub fn parse(s: &str) -> Option<Self> {
        let mut octets = [0u8; 4];
        let mut parts = 0;
        for part in s.split('.') {
            if parts == 4 || part.is_empty() || part.len() > 3 {
                return None;
            }
            let mut v = 0u32;
            for b in part.bytes() {
                if !b.is_ascii_digit() {
                    return None;
                }
                v = v * 10 + (b - b'0') as u32;
            }
            if v > 255 {
                return None;
            }
            octets[parts] = v as u8;
            parts += 1;
        }
        if parts != 4 {
            return None;
        }
        Some(Ipv4::new(octets[0], octets[1], octets[2], octets[3]))
    }
}

impl core::fmt::Debug for Ipv4 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let o = self.octets();
        write!(f, "{}.{}.{}.{}", o[0], o[1], o[2], o[3])
    }
}

/// A platform event: the shim's own type, so an app can hand the value it got from
/// `App::handle_raw` straight to [`TcpStream::on_event`] with no conversion.
pub use symbian_sys::ShimEvent as RawEvent;

/// The raw operations everything here is built from.
///
/// Deliberately the shim's shape, one call per request, so the accumulating and routing
/// happen above this line where they can be tested.
pub trait Net {
    fn net_start(&mut self, iap: Iap) -> Result<i32>;
    fn net_stop(&mut self, handle: i32);
    fn resolve(&mut self, conn: i32, host: &str) -> Result<i32>;

    /// Abandon a lookup.
    ///
    /// Necessary rather than tidy: a resolver nobody answers holds the connection it was
    /// made against, and on a handset with no route nothing is ever answered.
    fn dns_close(&mut self, handle: i32);
    fn tcp_open(&mut self, conn: i32) -> Result<i32>;
    fn tcp_connect(&mut self, handle: i32, addr: Ipv4, port: u16) -> Result<()>;
    /// The buffer must stay alive and untouched until the send completes.
    fn tcp_send(&mut self, handle: i32, buf: &[u8]) -> Result<()>;
    /// Likewise until the receive completes.
    fn tcp_recv(&mut self, handle: i32, buf: &mut [u8]) -> Result<()>;
    fn tcp_close(&mut self, handle: i32);
    fn udp_open(&mut self, conn: i32) -> Result<i32>;
    fn udp_send_to(&mut self, handle: i32, addr: Ipv4, port: u16, buf: &[u8]) -> Result<()>;
}

/// [`Net`] over the shim.
#[derive(Copy, Clone, Debug, Default)]
pub struct ShimNet;

impl Net for ShimNet {
    fn net_start(&mut self, iap: Iap) -> Result<i32> {
        let mut h = 0i32;
        Error::check(unsafe { sys::shim_net_start(iap.raw(), &mut h) })?;
        Ok(h)
    }

    fn net_stop(&mut self, handle: i32) {
        unsafe { sys::shim_net_stop(handle) }
    }

    fn resolve(&mut self, conn: i32, host: &str) -> Result<i32> {
        // UTF-16 on the stack. A hostname longer than this is not a hostname.
        let mut buf = [0u16; 256];
        let mut n = 0;
        for u in host.encode_utf16() {
            if n >= buf.len() {
                return Err(Error::Overflow);
            }
            buf[n] = u;
            n += 1;
        }
        let mut h = 0i32;
        Error::check(unsafe { sys::shim_dns_resolve(conn, buf.as_ptr(), n as i32, &mut h) })?;
        Ok(h)
    }

    fn dns_close(&mut self, handle: i32) {
        unsafe { sys::shim_dns_close(handle) }
    }

    fn tcp_open(&mut self, conn: i32) -> Result<i32> {
        let mut h = 0i32;
        Error::check(unsafe { sys::shim_tcp_open(conn, &mut h) })?;
        Ok(h)
    }

    fn tcp_connect(&mut self, handle: i32, addr: Ipv4, port: u16) -> Result<()> {
        Error::check(unsafe { sys::shim_tcp_connect(handle, addr.0, port) })
    }

    fn tcp_send(&mut self, handle: i32, buf: &[u8]) -> Result<()> {
        Error::check(unsafe { sys::shim_tcp_send(handle, buf.as_ptr(), buf.len() as i32) })
    }

    fn tcp_recv(&mut self, handle: i32, buf: &mut [u8]) -> Result<()> {
        Error::check(unsafe { sys::shim_tcp_recv(handle, buf.as_mut_ptr(), buf.len() as i32) })
    }

    fn tcp_close(&mut self, handle: i32) {
        unsafe { sys::shim_tcp_close(handle) }
    }

    fn udp_open(&mut self, conn: i32) -> Result<i32> {
        let mut h = 0i32;
        Error::check(unsafe { sys::shim_udp_open(conn, &mut h) })?;
        Ok(h)
    }

    fn udp_send_to(&mut self, handle: i32, addr: Ipv4, port: u16, buf: &[u8]) -> Result<()> {
        Error::check(unsafe {
            sys::shim_udp_send_to(handle, buf.as_ptr(), buf.len() as i32, addr.0, port)
        })
    }
}

// ------------------------------------------------------------------------ bearer --

/// The access point, with the prompt-once-then-remember behaviour.
///
/// The shape a user expects, and the reason it needs a type of its own: a saved IAP can
/// stop working — the Wi-Fi network it names is gone, the profile was deleted — and the
/// only recovery is to ask again. An app that saved an id and passed it forever would
/// simply stop connecting, with an error that names the access point rather than the
/// problem.
pub struct Bearer {
    handle: i32,
    iap: Option<u32>,
    /// Set once a saved id has already failed, so the retry does not loop.
    retried: bool,
    up: bool,
}

impl Bearer {
    /// Join a connection that is already up.
    ///
    /// The strategy to try first: if anything else on the handset is online, this joins it
    /// immediately with no dialog and nothing to wait for. `Err(Error::NotFound)` when
    /// there is nothing up, and then [`Bearer::start`] is the fallback.
    pub fn attach<N: Net>(net: &mut N) -> Result<Self> {
        let handle = net.net_start(Iap::Attach)?;
        // `retried: false`, so finding nothing to join falls through to the access point
        // dialog rather than giving up. Every other program on the handset offers that
        // dialog; refusing to is why ours was the only one that could not get online.
        Ok(Bearer { handle, iap: None, retried: false, up: false })
    }

    /// Bring up a bearer. Pass the id from a previous session if there is one.
    ///
    /// Returns as soon as the request is issued; feed events to [`Self::on_event`].
    pub fn start<N: Net>(net: &mut N, saved: Option<u32>) -> Result<Self> {
        let iap = match saved {
            Some(id) => Iap::Id(id),
            None => Iap::Prompt,
        };
        let handle = net.net_start(iap)?;
        Ok(Bearer { handle, iap: saved, retried: saved.is_none(), up: false })
    }

    /// Ask which access point, whatever was saved.
    ///
    /// The third recovery step, and the one the other constructors leave no room for. [`Bearer::start`]
    /// prompts only when it has *nothing* saved, and its retry-with-a-prompt fires only when the
    /// bearer itself fails. Neither covers the case that actually happens on a phone: a saved access
    /// point that still connects and no longer carries traffic — the Wi-Fi you are joined to with no
    /// route past it. The bearer comes up, name resolution answers `KErrDndNameNotFound`, and nothing
    /// in the ladder ever asks the one question that would fix it.
    ///
    /// So this exists to be reached from a *transport* failure rather than a bearer one, and the
    /// caller should forget the saved id when it does: an access point that cannot resolve a name is
    /// not the one to try again next launch.
    ///
    /// `retried: true` — the reader has been asked, and asking twice in a row is not a recovery.
    pub fn start_prompt<N: Net>(net: &mut N) -> Result<Self> {
        let handle = net.net_start(Iap::Prompt)?;
        Ok(Bearer { handle, iap: None, retried: true, up: false })
    }

    /// Bring up the **configured default** access point, without asking and without joining.
    ///
    /// The third strategy, and the one the other two leave no room for: [`Bearer::attach`] joins
    /// whatever is already up and [`Bearer::start`] prompts when it has no saved id, so a caller
    /// that wants "the phone's own default, silently" could not say so — [`Iap::Default`] existed
    /// with nothing to reach it. A headless run needs exactly this: no dialog to answer and no
    /// dependence on another process having a connection open.
    ///
    /// `retried: true`, because there is nothing to fall back to. A failure here is the answer.
    pub fn start_default<N: Net>(net: &mut N) -> Result<Self> {
        let handle = net.net_start(Iap::Default)?;
        Ok(Bearer { handle, iap: None, retried: true, up: false })
    }

    /// The access point in use, once up. Persist it — that is what makes the next run
    /// silent.
    pub fn iap(&self) -> Option<u32> {
        if self.up {
            self.iap
        } else {
            None
        }
    }

    pub fn handle(&self) -> i32 {
        self.handle
    }

    pub fn is_up(&self) -> bool {
        self.up
    }

    /// Feed a platform event. Returns `Ok(true)` when the bearer has just come up.
    ///
    /// A failure with a saved id retries once with a prompt, which is the whole point of
    /// this type; a failure after that is returned.
    pub fn on_event<N: Net>(&mut self, net: &mut N, ev: &RawEvent) -> Result<bool> {
        if ev.kind != sys::SHIM_EV_NET_READY || ev.handle != self.handle {
            return Ok(false);
        }
        if ev.status == 0 {
            self.up = true;
            // `a` is the IAP the OS settled on, which may differ from what was asked
            // for — it is zero if the shim could not read it back, and a zero id is not
            // worth saving.
            if ev.a > 0 {
                self.iap = Some(ev.a as u32);
            }
            return Ok(true);
        }

        if !self.retried {
            self.retried = true;
            self.iap = None;
            net.net_stop(self.handle);
            self.handle = net.net_start(Iap::Prompt)?;
            return Ok(false);
        }
        Err(Error::from_code(ev.status))
    }

    pub fn stop<N: Net>(&mut self, net: &mut N) {
        net.net_stop(self.handle);
        self.up = false;
    }
}

// ------------------------------------------------------------------------ stream --

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum State {
    Opening,
    Connecting,
    Connected,
    /// The peer closed, or we did. Data already received is still readable.
    Closed,
    Failed(Error),
}

/// What feeding an event produced.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Progress {
    /// The event was not ours, or changed nothing observable.
    None,
    Connected,
    /// Bytes arrived and are readable with [`TcpStream::read`].
    Received(usize),
    /// A send completed; that many bytes left the queue.
    Sent(usize),
    Closed,
    Failed(Error),
}

/// A TCP connection, owning its buffers.
pub struct TcpStream {
    handle: i32,
    state: State,

    /// Received but not yet read by the caller.
    rx: Box<[u8]>,
    rx_len: usize,
    /// Where the platform writes. **Never** `rx`, and never anything whose address depends
    /// on how much is buffered.
    ///
    /// A read is issued once and completes later, and the shim holds the pointer it was
    /// given for the whole of that — `symbian_shim.h` says so. Issuing it into `rx[rx_len..]`
    /// looked like an appending read and was one, right up until the caller drained `rx`
    /// while the read was still outstanding: `rx_len` went back to zero, the shim kept
    /// writing at the old offset, and the next completion counted those bytes from the
    /// front of a buffer that still held the previous reply.
    ///
    /// On the handset that showed up as Telegram answering `res_pq` twice — the second
    /// reply arrived with 104 stale bytes glued to the front of it, and the handshake died
    /// on a constructor it had already consumed. The HTTP self test never saw it because
    /// one request and one response is one read.
    ///
    /// The fix is not better bookkeeping. It is that the address handed to the platform is
    /// a constant, so no bookkeeping can be wrong about it.
    land: Box<[u8]>,
    /// Whether a read is outstanding.
    rx_pending: bool,

    /// Queued to send. Owned, because the shim holds a pointer to it until the send
    /// completes and the caller's slice may be gone by then.
    tx: Box<[u8]>,
    tx_len: usize,
    /// How much of `tx` the outstanding send covers.
    tx_pending: usize,
}

impl TcpStream {
    /// Open a socket on `bearer`. `rx_cap` and `tx_cap` are fixed for the life of the
    /// stream, because the shim holds pointers into them.
    pub fn open<N: Net>(net: &mut N, bearer: &Bearer, rx_cap: usize, tx_cap: usize) -> Result<Self> {
        let handle = net.tcp_open(bearer.handle())?;
        Ok(TcpStream {
            handle,
            state: State::Opening,
            rx: vec![0u8; rx_cap].into_boxed_slice(),
            land: vec![0u8; rx_cap].into_boxed_slice(),
            rx_len: 0,
            rx_pending: false,
            tx: vec![0u8; tx_cap].into_boxed_slice(),
            tx_len: 0,
            tx_pending: 0,
        })
    }

    /// Open a socket without binding to a specific bearer. The stack picks the default
    /// route, which is correct when another part of the application has already brought
    /// one up. `-1` is not a valid bearer handle — the shim treats it as "no preference".
    pub fn open_default<N: Net>(net: &mut N, rx_cap: usize, tx_cap: usize) -> Result<Self> {
        let handle = net.tcp_open(-1)?;
        Ok(TcpStream {
            handle,
            state: State::Opening,
            rx: vec![0u8; rx_cap].into_boxed_slice(),
            land: vec![0u8; rx_cap].into_boxed_slice(),
            rx_len: 0,
            rx_pending: false,
            tx: vec![0u8; tx_cap].into_boxed_slice(),
            tx_len: 0,
            tx_pending: 0,
        })
    }

    /// Open a socket on a specific bearer, by handle.
    ///
    /// For a caller that keeps its bearer somewhere this cannot borrow from — a long-lived
    /// connection held by one part of an app while another opens sockets on it.
    pub fn open_handle<N: Net>(net: &mut N, bearer_handle: i32, rx_cap: usize, tx_cap: usize) -> Result<Self> {
        let handle = net.tcp_open(bearer_handle)?;
        Ok(TcpStream {
            handle,
            state: State::Opening,
            rx: vec![0u8; rx_cap].into_boxed_slice(),
            land: vec![0u8; rx_cap].into_boxed_slice(),
            rx_len: 0,
            rx_pending: false,
            tx: vec![0u8; tx_cap].into_boxed_slice(),
            tx_len: 0,
            tx_pending: 0,
        })
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn connect<N: Net>(&mut self, net: &mut N, addr: Ipv4, port: u16) -> Result<()> {
        if self.state != State::Opening {
            return Err(Error::InUse);
        }
        net.tcp_connect(self.handle, addr, port)?;
        self.state = State::Connecting;
        Ok(())
    }

    /// Queue bytes. Returns how many were accepted, which is less than offered when the
    /// queue is full — the caller loops, exactly as with a real socket.
    pub fn write<N: Net>(&mut self, net: &mut N, data: &[u8]) -> Result<usize> {
        if self.state != State::Connected {
            return Err(Error::NotFound);
        }
        let room = self.tx.len() - self.tx_len;
        let take = room.min(data.len());
        self.tx[self.tx_len..self.tx_len + take].copy_from_slice(&data[..take]);
        self.tx_len += take;
        self.pump_tx(net)?;
        Ok(take)
    }

    /// Drain received bytes into `out`. Returns how many were copied.
    pub fn read<N: Net>(&mut self, net: &mut N, out: &mut [u8]) -> Result<usize> {
        let take = self.rx_len.min(out.len());
        out[..take].copy_from_slice(&self.rx[..take]);
        // Shift the remainder down. A ring buffer would avoid the copy, and would also
        // mean the shim's pointer into the buffer could wrap mid-request — which it
        // cannot express. At these sizes the copy is not worth that.
        self.rx.copy_within(take..self.rx_len, 0);
        self.rx_len -= take;
        // Draining may have made room, so a read that was held back can go now.
        self.pump_rx(net)?;
        Ok(take)
    }

    /// How much is waiting to be read.
    pub fn available(&self) -> usize {
        self.rx_len
    }

    /// Feed a platform event.
    pub fn on_event<N: Net>(&mut self, net: &mut N, ev: &RawEvent) -> Progress {
        // Events carry a handle and there may be several sockets. Checking it is what
        // keeps one socket's completion from being consumed by another — a bug that
        // cannot happen with one socket and always happens with two.
        if ev.handle != self.handle {
            return Progress::None;
        }
        match ev.kind {
            sys::SHIM_EV_CONNECTED => {
                if ev.status != 0 {
                    return self.fail(ev.status);
                }
                self.state = State::Connected;
                // Issue a read immediately. A server that speaks first would otherwise
                // have its greeting sitting in the socket server with nobody asking.
                if self.pump_rx(net).is_err() {
                    return self.fail(sys::SHIM_ERR_GENERAL);
                }
                Progress::Connected
            }

            sys::SHIM_EV_RECV => {
                self.rx_pending = false;
                // KErrEof and KErrDisconnected are the peer closing, not a fault.
                if ev.status == -25 || ev.status == -36 {
                    self.state = State::Closed;
                    return Progress::Closed;
                }
                if ev.status != 0 {
                    return self.fail(ev.status);
                }
                let n = ev.a.max(0) as usize;
                // A zero-length read on a stream socket is also end of stream.
                if n == 0 {
                    self.state = State::Closed;
                    return Progress::Closed;
                }
                // Out of the landing buffer and into the queue, now that the platform is
                // finished with it. `pump_rx` never asks for more than the free space, so
                // the `min` is a guard rather than a truncation that could lose bytes.
                let take = n.min(self.rx.len() - self.rx_len);
                self.rx[self.rx_len..self.rx_len + take].copy_from_slice(&self.land[..take]);
                self.rx_len += take;
                if self.pump_rx(net).is_err() {
                    return self.fail(sys::SHIM_ERR_GENERAL);
                }
                Progress::Received(take)
            }

            sys::SHIM_EV_SENT => {
                let sent = self.tx_pending;
                self.tx_pending = 0;
                if ev.status != 0 {
                    // The queued bytes are dropped rather than retried: at this level we
                    // cannot know whether the peer saw them, and silently resending is
                    // how a protocol gets a duplicate message.
                    self.tx_len = 0;
                    return self.fail(ev.status);
                }
                self.tx.copy_within(sent..self.tx_len, 0);
                self.tx_len -= sent;
                if self.pump_tx(net).is_err() {
                    return self.fail(sys::SHIM_ERR_GENERAL);
                }
                Progress::Sent(sent)
            }

            sys::SHIM_EV_CLOSED => {
                self.state = State::Closed;
                // A close while a send was queued abandons it. Reporting those bytes as
                // sent would be a lie the caller acts on.
                self.tx_len = 0;
                self.tx_pending = 0;
                Progress::Closed
            }

            _ => Progress::None,
        }
    }

    /// Issue a read if one is not already outstanding and there is room for it.
    ///
    /// The room check is the backpressure: with a full buffer, asking for more would
    /// give the shim a zero-length slice, which it rejects as an argument error rather
    /// than as "not now".
    fn pump_rx<N: Net>(&mut self, net: &mut N) -> Result<()> {
        if self.rx_pending || self.state != State::Connected {
            return Ok(());
        }
        if self.rx_len >= self.rx.len() {
            return Ok(());
        }
        // Always from the front of the landing buffer. The length shrinks as the queue
        // fills, but the address does not move, which is the whole point of it.
        let free = self.rx.len() - self.rx_len;
        net.tcp_recv(self.handle, &mut self.land[..free])?;
        self.rx_pending = true;
        Ok(())
    }

    fn pump_tx<N: Net>(&mut self, net: &mut N) -> Result<()> {
        if self.tx_pending > 0 || self.tx_len == 0 || self.state != State::Connected {
            return Ok(());
        }
        net.tcp_send(self.handle, &self.tx[..self.tx_len])?;
        self.tx_pending = self.tx_len;
        Ok(())
    }

    fn fail(&mut self, status: i32) -> Progress {
        let e = Error::from_code(status);
        self.state = State::Failed(e);
        Progress::Failed(e)
    }

    /// The handle, for a caller routing events across several sockets itself.
    pub fn handle(&self) -> i32 {
        self.handle
    }

    /// Close now. Called by `Drop` too; explicit when the caller wants to close before
    /// the value goes out of scope.
    pub fn close<N: Net>(&mut self, net: &mut N) {
        if self.handle >= 0 {
            net.tcp_close(self.handle);
            self.handle = -1;
            self.state = State::Closed;
        }
    }
}

impl Drop for TcpStream {
    fn drop(&mut self) {
        // The shim cancels every outstanding request before closing the socket, which is
        // what keeps it from reading these buffers after they are freed. Drop goes
        // through ShimNet directly because a Drop cannot be handed the caller's `Net` —
        // and on the host every extern is a stub, so this is a no-op there.
        if self.handle >= 0 {
            ShimNet.tcp_close(self.handle);
        }
    }
}

// -------------------------------------------------------------------------- DNS --

/// A hostname lookup in flight.
pub struct Lookup {
    handle: i32,
}

impl Lookup {
    pub fn start<N: Net>(net: &mut N, bearer: &Bearer, host: &str) -> Result<Self> {
        Ok(Lookup { handle: net.resolve(bearer.handle(), host)? })
    }

    /// `Ok(Some(addr))` once resolved, `Ok(None)` while still waiting.
    pub fn on_event(&mut self, ev: &RawEvent) -> Result<Option<Ipv4>> {
        if ev.kind != sys::SHIM_EV_RESOLVED || ev.handle != self.handle {
            return Ok(None);
        }
        if ev.status != 0 {
            return Err(Error::from_code(ev.status));
        }
        let raw = ev.a as u32;
        if raw == 0 {
            // Resolved, but to nothing usable — an AAAA-only name, or a record the shim
            // could not read as IPv4. Reported as not-found rather than as 0.0.0.0,
            // which would be attempted and fail confusingly later.
            return Err(Error::NotFound);
        }
        Ok(Some(Ipv4(raw)))
    }
}

// ------------------------------------------------------------------------ UDP --

/// A UDP socket, for the beacon and device discovery.
pub struct UdpSocket {
    handle: i32,
}

impl UdpSocket {
    pub fn open<N: Net>(net: &mut N, bearer_handle: i32) -> Result<Self> {
        let handle = net.udp_open(bearer_handle)?;
        Ok(UdpSocket { handle })
    }

    pub fn send_to<N: Net>(&mut self, net: &mut N, addr: Ipv4, port: u16, data: &[u8]) -> Result<()> {
        net.udp_send_to(self.handle, addr, port, data)
    }
}

impl Drop for UdpSocket {
    fn drop(&mut self) {
        if self.handle >= 0 {
            ShimNet.tcp_close(self.handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// A [`Net`] that records calls and lets a test hand back events in any order.
    #[derive(Default)]
    struct FakeNet {
        next_handle: i32,
        /// Every issued read, as (handle, capacity).
        reads: Vec<(i32, usize)>,
        /// Every issued write, as (handle, bytes).
        writes: Vec<(i32, Vec<u8>)>,
        starts: Vec<Iap>,
        stops: Vec<i32>,
        closes: Vec<i32>,
        /// Make the next call of a given kind fail.
        fail_recv: bool,
        /// Bytes the peer has waiting. `tcp_recv` writes the next one into whatever buffer
        /// it is handed, which is what the platform does and what a fake that only records
        /// the length cannot express — the reason the stale-buffer bug survived every test
        /// in this file.
        queued: Vec<Vec<u8>>,
        /// The address of every buffer a read was issued into, as an integer. Never
        /// dereferenced; it is here to assert the platform's pointer does not move.
        read_at: Vec<usize>,
    }

    impl FakeNet {
        fn new() -> Self {
            FakeNet { next_handle: 10, ..Default::default() }
        }
        fn alloc(&mut self) -> i32 {
            self.next_handle += 1;
            self.next_handle
        }
    }

    impl Net for FakeNet {
        fn net_start(&mut self, iap: Iap) -> Result<i32> {
            self.starts.push(iap);
            Ok(self.alloc())
        }
        fn net_stop(&mut self, handle: i32) {
            self.stops.push(handle);
        }
        fn resolve(&mut self, _conn: i32, _host: &str) -> Result<i32> {
            Ok(self.alloc())
        }
        fn dns_close(&mut self, _handle: i32) {}
        fn tcp_open(&mut self, _conn: i32) -> Result<i32> {
            Ok(self.alloc())
        }
        fn tcp_connect(&mut self, _h: i32, _a: Ipv4, _p: u16) -> Result<()> {
            Ok(())
        }
        fn tcp_send(&mut self, h: i32, buf: &[u8]) -> Result<()> {
            self.writes.push((h, buf.to_vec()));
            Ok(())
        }
        fn tcp_recv(&mut self, h: i32, buf: &mut [u8]) -> Result<()> {
            if self.fail_recv {
                return Err(Error::InUse);
            }
            self.reads.push((h, buf.len()));
            self.read_at.push(buf.as_ptr() as usize);
            if !self.queued.is_empty() {
                let next = self.queued.remove(0);
                let n = next.len().min(buf.len());
                buf[..n].copy_from_slice(&next[..n]);
            }
            Ok(())
        }
    fn tcp_close(&mut self, h: i32) {
        self.closes.push(h);
    }
    fn udp_open(&mut self, _conn: i32) -> Result<i32> {
        Ok(self.alloc())
    }
    fn udp_send_to(&mut self, h: i32, _addr: Ipv4, _port: u16, buf: &[u8]) -> Result<()> {
        self.writes.push((h, buf.to_vec()));
        Ok(())
    }
}

    fn ev(kind: i32, handle: i32, status: i32, a: i32) -> RawEvent {
        RawEvent { kind, handle, status, a, ..Default::default() }
    }

    fn connected(net: &mut FakeNet) -> (Bearer, TcpStream) {
        let mut bearer = Bearer::start(net, None).unwrap();
        let h = bearer.handle();
        bearer.on_event(net, &ev(sys::SHIM_EV_NET_READY, h, 0, 7)).unwrap();

        let mut s = TcpStream::open(net, &bearer, 16, 16).unwrap();
        s.connect(net, Ipv4::new(10, 0, 0, 1), 9).unwrap();
        let sh = s.handle();
        assert_eq!(s.on_event(net, &ev(sys::SHIM_EV_CONNECTED, sh, 0, 0)), Progress::Connected);
        (bearer, s)
    }

    // ---- the receive path ----

    #[test]
    fn a_second_reply_does_not_arrive_with_the_first_one_glued_to_it() {
        // Exactly what the handset did. Telegram answered `res_pq`, the client read it and
        // sent `req_DH_params`, and the next read came back as `res_pq` again with the real
        // answer behind it: 104 stale bytes in front of 552 new ones. The handshake died on
        // a constructor it had already consumed.
        //
        // The cause was where the read was issued, not what the server sent. A read into
        // `rx[rx_len..]` is still outstanding when the caller drains `rx` and `rx_len` drops
        // to zero, and from then on the platform is writing 104 bytes past where the
        // bookkeeping thinks it is.
        //
        // One request and one response never sees it, which is why the HTTP self test
        // passed and every program on the phone worked except this one.
        let mut net = FakeNet::new();
        let mut bearer = Bearer::start(&mut net, None).unwrap();
        let bh = bearer.handle();
        bearer.on_event(&mut net, &ev(sys::SHIM_EV_NET_READY, bh, 0, 7)).unwrap();

        let mut s = TcpStream::open(&mut net, &bearer, 64, 64).unwrap();
        s.connect(&mut net, Ipv4::new(10, 0, 0, 1), 9).unwrap();
        let h = s.handle();

        net.queued = alloc::vec![b"first...".to_vec(), b"second!!".to_vec()];
        assert_eq!(s.on_event(&mut net, &ev(sys::SHIM_EV_CONNECTED, h, 0, 0)), Progress::Connected);

        let mut out = [0u8; 64];
        assert_eq!(s.on_event(&mut net, &ev(sys::SHIM_EV_RECV, h, 0, 8)), Progress::Received(8));
        assert_eq!(s.read(&mut net, &mut out).unwrap(), 8);
        assert_eq!(&out[..8], b"first...");

        assert_eq!(s.on_event(&mut net, &ev(sys::SHIM_EV_RECV, h, 0, 8)), Progress::Received(8));
        assert_eq!(s.read(&mut net, &mut out).unwrap(), 8);
        assert_eq!(
            &out[..8],
            b"second!!",
            "the previous reply came back instead of the new one"
        );
    }

    #[test]
    fn every_read_is_issued_into_the_same_address() {
        // The invariant the fix rests on. The platform keeps the pointer it was handed
        // until the read completes, so that pointer must not depend on how much happens to
        // be buffered — no amount of care in the bookkeeping is worth as much as the
        // address being a constant.
        let mut net = FakeNet::new();
        let mut bearer = Bearer::start(&mut net, None).unwrap();
        let bh = bearer.handle();
        bearer.on_event(&mut net, &ev(sys::SHIM_EV_NET_READY, bh, 0, 7)).unwrap();

        let mut s = TcpStream::open(&mut net, &bearer, 64, 64).unwrap();
        s.connect(&mut net, Ipv4::new(10, 0, 0, 1), 9).unwrap();
        let h = s.handle();
        s.on_event(&mut net, &ev(sys::SHIM_EV_CONNECTED, h, 0, 0));

        // A partial drain, which leaves the queue non-empty and used to move the target.
        let mut out = [0u8; 3];
        for _ in 0..4 {
            s.on_event(&mut net, &ev(sys::SHIM_EV_RECV, h, 0, 8));
            let _ = s.read(&mut net, &mut out);
        }
        assert!(net.read_at.len() >= 4, "no reads were issued");
        assert!(
            net.read_at.windows(2).all(|w| w[0] == w[1]),
            "the address handed to the platform moved: {:?}",
            net.read_at
        );
    }

    // ---- addresses ----

    #[test]
    fn dotted_quad_parses_and_round_trips() {
        assert_eq!(Ipv4::parse("192.168.15.74"), Some(Ipv4::new(192, 168, 15, 74)));
        assert_eq!(Ipv4::parse("0.0.0.0"), Some(Ipv4::new(0, 0, 0, 0)));
        assert_eq!(Ipv4::parse("255.255.255.255").unwrap().octets(), [255, 255, 255, 255]);
    }

    #[test]
    fn a_hostname_is_not_a_dotted_quad() {
        // How a caller decides whether it needs DNS. Anything ambiguous must come back
        // None, or a hostname would be attempted as an address and fail as a timeout.
        for s in [
            "example.com",
            "1.2.3",
            "1.2.3.4.5",
            "256.0.0.1",
            "1.2.3.a",
            "",
            "1.2..4",
            "01.02.03.0004",
            "1.2.3.4 ",
        ] {
            assert!(Ipv4::parse(s).is_none(), "{s:?} parsed as an address");
        }
    }

    // ---- bearer ----

    #[test]
    fn a_first_run_prompts_and_remembers() {
        let mut net = FakeNet::new();
        let mut b = Bearer::start(&mut net, None).unwrap();
        assert_eq!(net.starts, vec![Iap::Prompt]);
        assert!(!b.is_up());
        assert_eq!(b.iap(), None, "no IAP is known until the bearer is up");

        let h = b.handle();
        assert!(b.on_event(&mut net, &ev(sys::SHIM_EV_NET_READY, h, 0, 42)).unwrap());
        assert!(b.is_up());
        assert_eq!(b.iap(), Some(42), "the id the OS chose is what gets persisted");
    }

    #[test]
    fn a_saved_iap_connects_without_prompting() {
        let mut net = FakeNet::new();
        let mut b = Bearer::start(&mut net, Some(42)).unwrap();
        assert_eq!(net.starts, vec![Iap::Id(42)]);
        let h = b.handle();
        assert!(b.on_event(&mut net, &ev(sys::SHIM_EV_NET_READY, h, 0, 42)).unwrap());
    }

    #[test]
    fn a_saved_iap_that_no_longer_works_falls_back_to_a_prompt() {
        // The reason Bearer exists. A stored access point can disappear — the network is
        // gone, the profile was deleted — and an app that kept passing the id would
        // simply stop connecting, reporting an error about the access point rather than
        // about the situation.
        let mut net = FakeNet::new();
        let mut b = Bearer::start(&mut net, Some(42)).unwrap();
        let h = b.handle();

        assert!(!b.on_event(&mut net, &ev(sys::SHIM_EV_NET_READY, h, -1, 0)).unwrap());
        assert_eq!(net.starts, vec![Iap::Id(42), Iap::Prompt]);
        assert_eq!(net.stops, vec![h], "the failed bearer must be released");

        let h2 = b.handle();
        assert_ne!(h2, h);
        assert!(b.on_event(&mut net, &ev(sys::SHIM_EV_NET_READY, h2, 0, 99)).unwrap());
        assert_eq!(b.iap(), Some(99));
    }

    #[test]
    fn a_prompt_that_fails_is_not_retried_forever() {
        let mut net = FakeNet::new();
        let mut b = Bearer::start(&mut net, None).unwrap();
        let h = b.handle();
        let r = b.on_event(&mut net, &ev(sys::SHIM_EV_NET_READY, h, -1, 0));
        assert!(r.is_err(), "a failed prompt has nothing left to fall back to");
        assert_eq!(net.starts.len(), 1);
    }

    #[test]
    fn a_bearer_ignores_another_bearers_event() {
        let mut net = FakeNet::new();
        let mut b = Bearer::start(&mut net, None).unwrap();
        assert!(!b
            .on_event(&mut net, &ev(sys::SHIM_EV_NET_READY, b.handle() + 1, 0, 1))
            .unwrap());
        assert!(!b.is_up());
    }

    // ---- stream ----

    #[test]
    fn connecting_issues_a_read_immediately() {
        // A server that speaks first — SMTP, IRC, an echo service that greets — would
        // otherwise have its greeting sitting unclaimed until the client happened to ask.
        let mut net = FakeNet::new();
        let (_b, s) = connected(&mut net);
        assert_eq!(net.reads, vec![(s.handle(), 16)]);
    }

    #[test]
    fn a_partial_read_accumulates_across_events() {
        // The property a single recv cannot give: RECV delivers what arrived, not what
        // was asked for. A frame reader that treated one completion as a whole message
        // would truncate every message that crossed a packet boundary.
        let mut net = FakeNet::new();
        let (_b, mut s) = connected(&mut net);
        let h = s.handle();

        assert_eq!(s.on_event(&mut net, &ev(sys::SHIM_EV_RECV, h, 0, 5)), Progress::Received(5));
        assert_eq!(s.available(), 5);
        assert_eq!(s.on_event(&mut net, &ev(sys::SHIM_EV_RECV, h, 0, 3)), Progress::Received(3));
        assert_eq!(s.available(), 8);

        // And each completion re-issues a read into the *remaining* room, not the whole
        // buffer, or the second read would overwrite the first read's bytes.
        assert_eq!(net.reads, vec![(h, 16), (h, 11), (h, 8)]);
    }

    #[test]
    fn a_full_receive_buffer_stops_asking_for_more_until_drained() {
        // Backpressure. With no room, issuing another read would hand the shim a
        // zero-length slice, which it rejects as an argument error rather than as
        // "not now" — so the read has to be withheld and resumed on drain.
        let mut net = FakeNet::new();
        let (_b, mut s) = connected(&mut net);
        let h = s.handle();

        // Fill the 16-byte buffer in two arrivals, so the withholding is a consequence of
        // the buffer being full rather than of any single event.
        s.on_event(&mut net, &ev(sys::SHIM_EV_RECV, h, 0, 10));
        s.on_event(&mut net, &ev(sys::SHIM_EV_RECV, h, 0, 6));
        assert_eq!(s.available(), 16);
        assert_eq!(net.reads, vec![(h, 16), (h, 6)], "no read issued with a full buffer");

        let mut out = [0u8; 8];
        assert_eq!(s.read(&mut net, &mut out).unwrap(), 8);
        assert_eq!(net.reads.last(), Some(&(h, 8)), "draining resumes reading");
        assert_eq!(s.state(), State::Connected);
    }

    #[test]
    fn a_zero_length_read_is_end_of_stream() {
        let mut net = FakeNet::new();
        let (_b, mut s) = connected(&mut net);
        let h = s.handle();
        assert_eq!(s.on_event(&mut net, &ev(sys::SHIM_EV_RECV, h, 0, 0)), Progress::Closed);
        assert_eq!(s.state(), State::Closed);
    }

    #[test]
    fn eof_and_disconnected_are_a_close_not_a_failure() {
        // KErrEof and KErrDisconnected mean the peer went away, which is the normal end
        // of a connection. Treating them as errors makes every clean shutdown look like
        // a fault.
        for status in [-25, -36] {
            let mut net = FakeNet::new();
            let (_b, mut s) = connected(&mut net);
            let h = s.handle();
            assert_eq!(
                s.on_event(&mut net, &ev(sys::SHIM_EV_RECV, h, status, 0)),
                Progress::Closed,
                "status {status}"
            );
            assert_eq!(s.state(), State::Closed);
        }
    }

    #[test]
    fn writes_queue_and_drain_in_order() {
        let mut net = FakeNet::new();
        let (_b, mut s) = connected(&mut net);
        let h = s.handle();

        assert_eq!(s.write(&mut net, b"abc").unwrap(), 3);
        // One send outstanding, so the second write queues rather than issuing.
        assert_eq!(s.write(&mut net, b"def").unwrap(), 3);
        assert_eq!(net.writes, vec![(h, b"abc".to_vec())]);

        assert_eq!(s.on_event(&mut net, &ev(sys::SHIM_EV_SENT, h, 0, 0)), Progress::Sent(3));
        assert_eq!(net.writes, vec![(h, b"abc".to_vec()), (h, b"def".to_vec())]);
    }

    #[test]
    fn a_full_send_queue_accepts_a_partial_write() {
        let mut net = FakeNet::new();
        let (_b, mut s) = connected(&mut net);
        // 16-byte queue, 20 offered.
        assert_eq!(s.write(&mut net, &[0u8; 20]).unwrap(), 16);
        // And nothing more until the outstanding send completes.
        assert_eq!(s.write(&mut net, b"x").unwrap(), 0);
    }

    #[test]
    fn a_close_while_a_send_is_queued_abandons_it() {
        // Reporting those bytes as sent would be a lie the caller acts on — a protocol
        // would move to the next state believing its request went out.
        let mut net = FakeNet::new();
        let (_b, mut s) = connected(&mut net);
        let h = s.handle();
        s.write(&mut net, b"request").unwrap();
        s.write(&mut net, b"more").unwrap();

        assert_eq!(s.on_event(&mut net, &ev(sys::SHIM_EV_CLOSED, h, 0, 0)), Progress::Closed);
        let before = net.writes.len();
        // Nothing further is sent, and the completion for the in-flight send does not
        // resurrect the queue.
        s.on_event(&mut net, &ev(sys::SHIM_EV_SENT, h, 0, 0));
        assert_eq!(net.writes.len(), before);
    }

    #[test]
    fn a_failed_connect_leaves_the_stream_failed() {
        let mut net = FakeNet::new();
        let mut b = Bearer::start(&mut net, None).unwrap();
        let bh = b.handle();
        b.on_event(&mut net, &ev(sys::SHIM_EV_NET_READY, bh, 0, 1)).unwrap();
        let mut s = TcpStream::open(&mut net, &b, 16, 16).unwrap();
        s.connect(&mut net, Ipv4::new(10, 0, 0, 1), 9).unwrap();
        let h = s.handle();

        let p = s.on_event(&mut net, &ev(sys::SHIM_EV_CONNECTED, h, -33, 0));
        assert!(matches!(p, Progress::Failed(_)));
        assert!(matches!(s.state(), State::Failed(_)));
        // And no read was ever issued, so nothing points into the buffers.
        assert!(net.reads.is_empty());
    }

    #[test]
    fn writing_before_connected_is_refused() {
        let mut net = FakeNet::new();
        let mut b = Bearer::start(&mut net, None).unwrap();
        let bh = b.handle();
        b.on_event(&mut net, &ev(sys::SHIM_EV_NET_READY, bh, 0, 1)).unwrap();
        let mut s = TcpStream::open(&mut net, &b, 16, 16).unwrap();
        assert!(s.write(&mut net, b"x").is_err());
    }

    #[test]
    fn connecting_twice_is_refused() {
        let mut net = FakeNet::new();
        let mut b = Bearer::start(&mut net, None).unwrap();
        let bh = b.handle();
        b.on_event(&mut net, &ev(sys::SHIM_EV_NET_READY, bh, 0, 1)).unwrap();
        let mut s = TcpStream::open(&mut net, &b, 16, 16).unwrap();
        s.connect(&mut net, Ipv4::new(10, 0, 0, 1), 9).unwrap();
        assert_eq!(s.connect(&mut net, Ipv4::new(10, 0, 0, 2), 9).unwrap_err(), Error::InUse);
    }

    #[test]
    fn one_streams_event_is_not_consumed_by_another() {
        // The bug that cannot happen with one socket and always happens with two.
        let mut net = FakeNet::new();
        let mut b = Bearer::start(&mut net, None).unwrap();
        let bh = b.handle();
        b.on_event(&mut net, &ev(sys::SHIM_EV_NET_READY, bh, 0, 1)).unwrap();

        let mut a = TcpStream::open(&mut net, &b, 16, 16).unwrap();
        let mut c = TcpStream::open(&mut net, &b, 16, 16).unwrap();
        a.connect(&mut net, Ipv4::new(10, 0, 0, 1), 9).unwrap();
        c.connect(&mut net, Ipv4::new(10, 0, 0, 2), 9).unwrap();
        assert_ne!(a.handle(), c.handle());

        let for_a = ev(sys::SHIM_EV_CONNECTED, a.handle(), 0, 0);
        assert_eq!(c.on_event(&mut net, &for_a), Progress::None);
        assert_eq!(c.state(), State::Connecting, "c must not have taken a's completion");
        assert_eq!(a.on_event(&mut net, &for_a), Progress::Connected);
    }

    #[test]
    fn read_shifts_the_remainder_down() {
        let mut net = FakeNet::new();
        let (_b, mut s) = connected(&mut net);
        let h = s.handle();
        s.rx[..6].copy_from_slice(b"abcdef");
        s.rx_len = 6;
        s.rx_pending = false;

        let mut out = [0u8; 2];
        assert_eq!(s.read(&mut net, &mut out).unwrap(), 2);
        assert_eq!(&out, b"ab");
        assert_eq!(s.available(), 4);

        let mut rest = [0u8; 8];
        assert_eq!(s.read(&mut net, &mut rest).unwrap(), 4);
        assert_eq!(&rest[..4], b"cdef", "the remainder must survive the shift intact");
        assert_eq!(h, s.handle());
    }

    #[test]
    fn reading_an_empty_stream_yields_nothing_rather_than_failing() {
        let mut net = FakeNet::new();
        let (_b, mut s) = connected(&mut net);
        let mut out = [0u8; 4];
        assert_eq!(s.read(&mut net, &mut out).unwrap(), 0);
    }

    // ---- DNS ----

    #[test]
    fn a_lookup_reports_its_address() {
        let mut net = FakeNet::new();
        let mut b = Bearer::start(&mut net, None).unwrap();
        let bh = b.handle();
        b.on_event(&mut net, &ev(sys::SHIM_EV_NET_READY, bh, 0, 1)).unwrap();

        let mut l = Lookup::start(&mut net, &b, "example.com").unwrap();
        let h = l.handle;
        assert_eq!(
            l.on_event(&ev(sys::SHIM_EV_RESOLVED, h, 0, 0x0A00_0001)).unwrap(),
            Some(Ipv4::new(10, 0, 0, 1))
        );
    }

    #[test]
    fn a_lookup_that_resolves_to_nothing_is_an_error_not_an_address() {
        // An AAAA-only name, or a record the shim could not read as IPv4. Returning
        // 0.0.0.0 would be attempted and fail later as a timeout, pointing at the wrong
        // thing entirely.
        let mut net = FakeNet::new();
        let mut b = Bearer::start(&mut net, None).unwrap();
        let bh = b.handle();
        b.on_event(&mut net, &ev(sys::SHIM_EV_NET_READY, bh, 0, 1)).unwrap();
        let mut l = Lookup::start(&mut net, &b, "v6only.example").unwrap();
        let h = l.handle;
        assert_eq!(
            l.on_event(&ev(sys::SHIM_EV_RESOLVED, h, 0, 0)).unwrap_err(),
            Error::NotFound
        );
    }

    #[test]
    fn a_lookup_ignores_another_lookups_event() {
        let mut net = FakeNet::new();
        let mut b = Bearer::start(&mut net, None).unwrap();
        let bh = b.handle();
        b.on_event(&mut net, &ev(sys::SHIM_EV_NET_READY, bh, 0, 1)).unwrap();
        let mut l = Lookup::start(&mut net, &b, "a.example").unwrap();
        let other = l.handle + 1;
        assert_eq!(l.on_event(&ev(sys::SHIM_EV_RESOLVED, other, 0, 0x7F000001)).unwrap(), None);
    }
}
