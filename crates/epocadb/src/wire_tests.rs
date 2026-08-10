//! The bridge driven against a fake platform, socket completions and all.
//!
//! Everything here exists because the protocol tests could not fail for any of the
//! reasons the bridge actually broke. Parsing `OK pong` was never the problem; what
//! went wrong was a send that reported success after accepting a tenth of its bytes, a
//! socket that was never handed its own completions, and a payload that had already
//! arrived being waited for.
//!
//! # How the fake models the platform
//!
//! A read is issued once and completes later. [`FakeNet::tcp_recv`] copies from the
//! socket's inbox into the landing buffer *at issue time* — the only thing safe Rust
//! can do without holding the caller's pointer — and records how much it took. The test
//! then delivers `SHIM_EV_RECV` carrying that count, exactly as the shim would. So the
//! host's side of a conversation is queued up front and the fake hands it over in
//! segments, with [`Wire::max_chunk`] controlling how it is cut up.
//!
//! Sends work the same way: `tcp_send` records the bytes and leaves the send
//! outstanding until the test delivers `SHIM_EV_SENT`.

use super::*;

use alloc::vec;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

// ── the fake platform ─────────────────────────────────────────────

#[derive(Default)]
struct Wire {
    next_handle: i32,
    /// Handles handed out by `tcp_open`, in order: cmd, then log.
    opened: Vec<i32>,
    /// What the host will say on each socket. Reads take from the front.
    inbox: HashMap<i32, Vec<u8>>,
    /// Bytes the last `tcp_recv` placed in the landing buffer, awaiting a RECV event.
    fetched: HashMap<i32, usize>,
    /// Everything the device wrote, in order.
    sent: HashMap<i32, Vec<u8>>,
    /// A send awaiting its SENT event.
    send_pending: HashMap<i32, usize>,
    /// Datagrams the beacon emitted.
    beacons: Vec<Vec<u8>>,
    /// Cap on how much one read may take, for testing fragmented delivery.
    max_chunk: usize,
    /// Make every `tcp_open` after this many calls fail.
    open_ok_for: usize,
    /// Make `tcp_connect` refuse.
    connect_fails: bool,
    tcp_opens: usize,
}

#[derive(Clone)]
struct FakeNet(Rc<RefCell<Wire>>);

impl FakeNet {
    fn new() -> Self {
        FakeNet(Rc::new(RefCell::new(Wire {
            next_handle: 10,
            max_chunk: usize::MAX,
            open_ok_for: usize::MAX,
            ..Default::default()
        })))
    }
}

impl Net for FakeNet {
    fn net_start(&mut self, _iap: symbian::net::Iap) -> Result<i32> {
        Ok(1)
    }
    fn net_stop(&mut self, _handle: i32) {}
    fn resolve(&mut self, _conn: i32, _host: &str) -> Result<i32> {
        Ok(1)
    }
    fn dns_close(&mut self, _handle: i32) {}

    fn tcp_open(&mut self, _conn: i32) -> Result<i32> {
        let mut w = self.0.borrow_mut();
        w.tcp_opens += 1;
        if w.tcp_opens > w.open_ok_for {
            return Err(Error::InUse);
        }
        w.next_handle += 1;
        let h = w.next_handle;
        w.opened.push(h);
        Ok(h)
    }

    fn tcp_connect(&mut self, _handle: i32, _addr: Ipv4, _port: u16) -> Result<()> {
        if self.0.borrow().connect_fails {
            return Err(Error::NotReady);
        }
        Ok(())
    }

    fn tcp_send(&mut self, handle: i32, buf: &[u8]) -> Result<()> {
        let mut w = self.0.borrow_mut();
        w.sent.entry(handle).or_default().extend_from_slice(buf);
        w.send_pending.insert(handle, buf.len());
        Ok(())
    }

    fn tcp_recv(&mut self, handle: i32, buf: &mut [u8]) -> Result<()> {
        let mut w = self.0.borrow_mut();
        let cap = buf.len().min(w.max_chunk);
        let take = {
            let q = w.inbox.entry(handle).or_default();
            let n = q.len().min(cap);
            buf[..n].copy_from_slice(&q[..n]);
            q.drain(..n);
            n
        };
        // A read that found nothing stays outstanding with nothing in it, which is what
        // the platform does too. The test must have queued the host's side already.
        w.fetched.insert(handle, take);
        Ok(())
    }

    fn tcp_close(&mut self, _handle: i32) {}

    fn udp_open(&mut self, _conn: i32) -> Result<i32> {
        let mut w = self.0.borrow_mut();
        w.next_handle += 1;
        Ok(w.next_handle)
    }

    fn udp_send_to(&mut self, _handle: i32, _addr: Ipv4, _port: u16, buf: &[u8]) -> Result<()> {
        self.0.borrow_mut().beacons.push(buf.to_vec());
        Ok(())
    }
}

// ── the harness ───────────────────────────────────────────────────

fn ev(kind: i32, handle: i32, status: i32, a: i32) -> sys::ShimEvent {
    sys::ShimEvent { kind, handle, status, a, ..Default::default() }
}

/// A timer tick: the event the application loop produces constantly and the bridge
/// rides on.
fn tick(handle: i32) -> sys::ShimEvent {
    ev(sys::SHIM_EV_TIMER, handle, 0, 0)
}

struct Harness {
    bridge: Bridge<FakeNet>,
    wire: Rc<RefCell<Wire>>,
    cmd: i32,
    log: i32,
}

impl Harness {
    /// Build a bridge with the host's side of the cmd conversation preloaded.
    fn with_script(script: &[u8]) -> Self {
        let net = FakeNet::new();
        let wire = net.0.clone();
        let bridge = Bridge::connect_with(net, Ipv4::new(192, 168, 1, 10), Some(7), 0)
            .expect("connect_with should open both sockets");
        let (cmd, log) = {
            let w = wire.borrow();
            (w.opened[0], w.opened[1])
        };
        wire.borrow_mut().inbox.entry(cmd).or_default().extend_from_slice(script);
        Harness { bridge, wire, cmd, log }
    }

    fn new() -> Self {
        Self::with_script(b"")
    }

    /// Bring both sockets up. The CONNECTED completion is what issues the first read,
    /// so anything the host has to say must already be queued.
    fn ready(&mut self, now: u64) {
        self.bridge.on_event(&ev(sys::SHIM_EV_CONNECTED, self.cmd, 0, 0), now);
        self.bridge.on_event(&ev(sys::SHIM_EV_CONNECTED, self.log, 0, 0), now);
        assert_eq!(self.bridge.phase(), Phase::Ready, "both sockets connected");
    }

    /// Deliver at most one outstanding platform completion. Returns false when there
    /// was nothing to deliver.
    fn step(&mut self, now: u64) -> bool {
        for h in [self.cmd, self.log] {
            let pending = self.wire.borrow_mut().send_pending.remove(&h);
            if let Some(n) = pending {
                self.bridge.on_event(&ev(sys::SHIM_EV_SENT, h, 0, n as i32), now);
                return true;
            }
            let got = self.wire.borrow_mut().fetched.remove(&h);
            if let Some(n) = got {
                if n > 0 {
                    self.bridge.on_event(&ev(sys::SHIM_EV_RECV, h, 0, n as i32), now);
                    return true;
                }
            }
        }
        false
    }

    /// Run completions until nothing is outstanding — one turn of the event loop,
    /// compressed.
    fn settle(&mut self, now: u64) {
        for _ in 0..4096 {
            if !self.step(now) {
                return;
            }
        }
        panic!("the platform never went quiet — a completion loop");
    }

    fn sent(&self, handle: i32) -> Vec<u8> {
        self.wire.borrow().sent.get(&handle).cloned().unwrap_or_default()
    }

    fn sent_text(&self, handle: i32) -> String {
        String::from_utf8_lossy(&self.sent(handle)).into_owned()
    }

    fn pings(&self) -> usize {
        self.sent_text(self.cmd).matches("REQ PING").count()
    }
}

// ── the log channel ───────────────────────────────────────────────

#[test]
fn the_log_socket_keeps_sending_after_the_first_flush() {
    // The bug: `on_ready` drove the cmd socket and not the log socket, so the log
    // stream never saw its own SHIM_EV_SENT. `pump_tx` refuses to issue a second send
    // while one is outstanding, so exactly one flush ever reached the host and every
    // line after it queued until the transmit buffer filled and writes returned zero.
    let mut h = Harness::new();
    h.ready(0);

    for i in 0..12 {
        h.bridge.log(&alloc::format!("line-{i}"));
        h.settle(0);
    }

    let text = h.sent_text(h.log);
    for i in 0..12 {
        assert!(text.contains(&alloc::format!("line-{i}")), "line-{i} never reached the host: {text:?}");
    }
}

#[test]
fn log_lines_are_newline_terminated_and_whole() {
    let mut h = Harness::new();
    h.ready(0);
    h.bridge.log("connecting to DC2");
    h.bridge.log("auth key negotiated");
    h.settle(0);

    assert_eq!(h.sent_text(h.log), "connecting to DC2\nauth key negotiated\n");
}

#[test]
fn logs_written_before_the_socket_is_up_are_delivered_once_it_is() {
    let mut h = Harness::new();
    h.bridge.log("early line");
    assert!(h.sent(h.log).is_empty(), "nothing can be sent before Ready");

    h.ready(0);
    h.settle(0);
    assert!(h.sent_text(h.log).contains("early line"));
}

#[test]
fn dropped_log_lines_are_announced_rather_than_silently_missing() {
    // A gap with no marker reads as "that code never ran", which is the most expensive
    // wrong conclusion available when debugging on a handset.
    let mut h = Harness::new();
    h.ready(0);

    // Overflow the ring without letting the socket drain it.
    for i in 0..600 {
        h.bridge.log(&alloc::format!("flood-{i}-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
    }
    assert!(h.bridge.dropped_logs() > 0, "the flood should have overflowed the ring");
    h.settle(0);

    let text = h.sent_text(h.log);
    assert!(text.contains("log line(s) dropped"), "the gap was never reported: {text:?}");
}

#[test]
fn a_stalled_log_socket_does_not_grow_the_heap() {
    // With no SENT completions the socket never drains. The ring absorbs the backlog;
    // the outbound queue must not.
    let mut h = Harness::new();
    h.ready(0);
    for i in 0..2000 {
        h.bridge.log(&alloc::format!("stalled-{i}"));
    }
    assert!(
        h.bridge.dropped_logs() > 0,
        "the ring, not the heap, should be absorbing this"
    );
}

// ── polling ───────────────────────────────────────────────────────

#[test]
fn only_one_ping_goes_out_per_interval() {
    // The bug: `poll` sent a REQ PING unconditionally, and the application calls it on
    // every shim event — timers, key presses, redraws. That is thousands of pings a
    // minute into a 1 KB transmit buffer, and once it fills, `write` truncates a line
    // mid-word and the channel desynchronises.
    let mut h = Harness::new();
    h.ready(0);

    for _ in 0..200 {
        h.bridge.on_event(&tick(0), 0);
        assert!(h.bridge.poll(0).is_none());
    }
    h.settle(0);

    assert_eq!(h.pings(), 1, "one interval, one ping: {:?}", h.sent_text(h.cmd));
}

#[test]
fn a_host_answering_instantly_does_not_unthrottle_the_poller() {
    // The other half of the rate limit. With replies arriving, the in-flight flag clears
    // on every poll, so nothing but the clock stands between the application's event
    // rate and the wire — and the application polls on every shim event.
    let mut h = Harness::with_script(&b"OK pong\r\n".repeat(60));
    h.ready(0);

    for _ in 0..60 {
        h.bridge.on_event(&tick(0), 0);
        h.bridge.poll(0);
        h.settle(0);
    }

    assert_eq!(h.pings(), 1, "sixty polls inside one interval sent {} pings", h.pings());
}

#[test]
fn no_second_ping_until_the_first_is_answered() {
    let mut h = Harness::new();
    h.ready(0);

    // Twelve seconds of events, well past the ping interval, with no reply from the host.
    for t in 0..12 {
        let now = t * 1_000_000;
        h.bridge.on_event(&tick(0), now);
        h.bridge.poll(now);
        h.settle(now);
    }

    assert_eq!(h.pings(), 1, "a request was outstanding the whole time");
}

#[test]
fn the_next_ping_goes_out_once_the_reply_lands() {
    let mut h = Harness::with_script(b"OK pong\r\n");
    h.ready(0);

    assert!(h.bridge.poll(0).is_none(), "the first poll sends, it does not answer");
    h.settle(0);
    assert_eq!(h.bridge.poll(0), Some(Command::None), "pong is a command of nothing");

    let later = 2_000_000;
    h.bridge.on_event(&tick(0), later);
    h.bridge.poll(later);
    h.settle(later);
    assert_eq!(h.pings(), 2);
}

#[test]
fn a_host_that_stops_answering_tears_the_session_down() {
    // A host killed mid-session leaves a socket that is open and silent. No socket
    // error reports that; only a clock does. Without this the bridge waits forever and
    // the developer concludes the device is wedged.
    let mut h = Harness::new();
    h.ready(0);

    let mut now = 0u64;
    for _ in 0..5 {
        h.bridge.on_event(&tick(0), now);
        h.bridge.poll(now);
        h.settle(now);
        now += REPLY_TIMEOUT_US;
    }

    assert!(
        matches!(h.bridge.phase(), Phase::Dead(_)),
        "expected Dead after repeated silence, got {:?}",
        h.bridge.phase()
    );
}

#[test]
fn quit_from_the_host_ends_the_session() {
    let mut h = Harness::with_script(b"OK QUIT\r\n");
    h.ready(0);
    h.bridge.poll(0);
    h.settle(0);

    assert_eq!(h.bridge.poll(0), Some(Command::Quit));
    assert!(matches!(h.bridge.phase(), Phase::Dead(_)));
}

#[test]
fn an_unparseable_reply_is_reported_and_does_not_stall_the_poller() {
    let mut h = Harness::with_script(b"OK WAT is this\r\n");
    h.ready(0);
    h.bridge.poll(0);
    h.settle(0);

    assert_eq!(h.bridge.poll(0), Some(Command::None));
    h.settle(0);
    assert!(h.sent_text(h.log).contains("unparsed reply"), "{:?}", h.sent_text(h.log));
}

// ── sending, and partial writes ───────────────────────────────────

#[test]
fn a_payload_larger_than_the_transmit_buffer_is_delivered_whole() {
    // The bug: `TcpStream::write` accepts what fits and reports how much, and both
    // `send_line` and `send_data` discarded the count. With a 1 KB transmit buffer, a
    // pull of anything bigger sent its first kilobyte and reported success — and the
    // host sat in `recv_exactly` waiting for bytes that were never queued.
    let mut h = Harness::new();
    h.ready(0);

    let payload: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
    h.bridge.send_data(&payload).expect("queueing a pull payload");
    h.settle(0);

    let wire = h.sent(h.cmd);
    let header = b"DATA 10000\r\n";
    assert!(wire.starts_with(header), "header missing: {:?}", &wire[..24.min(wire.len())]);
    assert_eq!(&wire[header.len()..], &payload[..], "the payload was truncated or reordered");
    assert_eq!(h.bridge.pending_out(), 0, "the queue should have drained");
}

#[test]
fn a_line_queued_behind_a_full_buffer_is_not_cut_in_half() {
    let mut h = Harness::new();
    h.ready(0);

    // Fill the transmit path, then queue lines behind it.
    h.bridge.send_data(&vec![b'x'; 4000]).unwrap();
    for i in 0..20 {
        h.bridge.reply(&alloc::format!("OK reply-{i}"));
    }
    h.settle(0);

    let text = h.sent_text(h.cmd);
    for i in 0..20 {
        assert!(
            text.contains(&alloc::format!("OK reply-{i}\r\n")),
            "reply-{i} was lost or split"
        );
    }
}

#[test]
fn a_transfer_beyond_the_queue_ceiling_fails_loudly() {
    let mut h = Harness::new();
    h.ready(0);
    let huge = vec![0u8; MAX_OUT_QUEUE + 1];
    assert_eq!(h.bridge.send_data(&huge), Err(Error::Overflow));
}

// ── receiving a push ──────────────────────────────────────────────

/// Drive a poll until it yields a command, settling the platform between attempts.
fn poll_until_command(h: &mut Harness, now: u64) -> Command {
    for _ in 0..64 {
        h.settle(now);
        if let Some(cmd) = h.bridge.poll(now) {
            if cmd != Command::None {
                return cmd;
            }
        }
        h.bridge.on_event(&tick(0), now);
    }
    panic!("no command arrived: {:?}", h.sent_text(h.cmd));
}

#[test]
fn a_push_whose_payload_shares_a_segment_with_its_header_completes() {
    // The bug that made push unusable. `read_response` fills its buffer greedily, so
    // when `DATA 5\r\nhello` arrives in one segment — which is what TCP does, since the
    // host writes the header and the bytes back to back — the payload is sitting in
    // `read_buf` the moment the header is parsed. `read_data` went to the socket for it
    // instead, and waited forever for five bytes it was already holding.
    let mut h = Harness::with_script(b"OK PUSH C:\\Data\\f.bin 5\r\nDATA 5\r\nhello");
    h.ready(0);

    let cmd = poll_until_command(&mut h, 0);
    assert_eq!(cmd, Command::Push { path: "C:\\Data\\f.bin".into(), size: 5 });

    h.bridge.expect_data_header();
    assert!(h.bridge.push_in_progress());

    let data = h.bridge.read_data().expect("the payload had already arrived");
    assert_eq!(&data, b"hello");
    assert!(!h.bridge.push_in_progress());
}

#[test]
fn a_push_delivered_one_byte_at_a_time_completes() {
    // The opposite arrival pattern: nothing shares a segment with anything. Both must
    // work, and the resumable path is the one that silently dropped what it had
    // accumulated whenever a read came back empty.
    // Bigger than the receive buffer, so it cannot all be sitting there when the
    // command is parsed — the transfer has to survive being resumed.
    let payload: Vec<u8> = (0..3000u32).map(|i| (i % 253) as u8).collect();
    let mut script =
        alloc::format!("OK PUSH C:\\Data\\big.bin {n}\r\nDATA {n}\r\n", n = payload.len())
            .into_bytes();
    script.extend_from_slice(&payload);

    let mut h = Harness::with_script(&script);
    h.wire.borrow_mut().max_chunk = 64;
    h.ready(0);

    let cmd = poll_until_command(&mut h, 0);
    assert!(matches!(cmd, Command::Push { size: 3000, .. }));
    h.bridge.expect_data_header();

    // Feed completions one at a time, asking for the payload after each.
    let mut got = None;
    for _ in 0..20_000 {
        if let Some(d) = h.bridge.read_data() {
            got = Some(d);
            break;
        }
        if !h.step(0) {
            break;
        }
    }

    assert_eq!(got.as_deref(), Some(&payload[..]), "a fragmented payload was lost");
}

#[test]
fn a_ping_is_not_sent_into_the_middle_of_a_transfer() {
    // A REQ written between the DATA header and its payload is read by the host as
    // payload, and both sides are then permanently out of step.
    let mut h = Harness::with_script(b"OK PUSH C:\\Data\\f.bin 5\r\n");
    h.ready(0);

    let cmd = poll_until_command(&mut h, 0);
    assert!(matches!(cmd, Command::Push { .. }));
    h.bridge.expect_data_header();

    let before = h.pings();
    for t in 0..10 {
        let now = t * 2_000_000;
        h.bridge.on_event(&tick(0), now);
        assert!(h.bridge.poll(now).is_none(), "poll must stay quiet mid-transfer");
        h.settle(now);
    }
    assert_eq!(h.pings(), before, "a ping was sent while a transfer was open");
}

#[test]
fn a_pull_reply_and_its_payload_reach_the_host_in_order() {
    let mut h = Harness::with_script(b"OK PULL C:\\Data\\report.txt\r\n");
    h.ready(0);

    let cmd = poll_until_command(&mut h, 0);
    assert_eq!(cmd, Command::Pull { path: "C:\\Data\\report.txt".into() });

    let body = b"the report body";
    h.bridge.reply(&alloc::format!("OK {}", body.len()));
    h.bridge.send_data(body).unwrap();
    h.settle(0);

    let text = h.sent_text(h.cmd);
    let ok_at = text.find("OK 15\r\n").expect("status line missing");
    let data_at = text.find("DATA 15\r\n").expect("data header missing");
    assert!(ok_at < data_at, "the payload overtook its status line");
    assert!(text.ends_with("the report body"));
}

// ── failure and recovery ──────────────────────────────────────────

#[test]
fn a_socket_failure_moves_to_dead_and_schedules_a_retry() {
    let mut h = Harness::new();
    h.ready(0);

    h.bridge.on_event(&ev(sys::SHIM_EV_CLOSED, h.cmd, 0, 0), 0);
    assert!(matches!(h.bridge.phase(), Phase::Dead(_)));

    // Too early: the backoff has not elapsed.
    h.bridge.on_event(&tick(0), 500_000);
    assert!(matches!(h.bridge.phase(), Phase::Dead(_)));

    // Past the first backoff step, it tries again.
    h.bridge.on_event(&tick(0), 2_000_000);
    assert_eq!(h.bridge.phase(), Phase::Connecting);
}

#[test]
fn a_connect_that_never_completes_is_retried_rather_than_waited_on_forever() {
    // The failure mode with no error attached: the socket opened, the SYN went nowhere,
    // and no completion is coming. Without a deadline the bridge sits in Connecting for
    // the life of the process.
    let mut h = Harness::new();
    assert_eq!(h.bridge.phase(), Phase::Connecting);

    h.bridge.on_event(&tick(0), CONNECT_TIMEOUT_US - 1);
    assert_eq!(h.bridge.phase(), Phase::Connecting, "not yet");

    h.bridge.on_event(&tick(0), CONNECT_TIMEOUT_US);
    assert!(matches!(h.bridge.phase(), Phase::Dead(_)), "the deadline should have fired");
}

#[test]
fn a_refused_connect_goes_to_dead_instead_of_parking_in_connecting() {
    // `connect` returning an error means no completion will ever arrive. Discarding it
    // and setting Connecting anyway wedges the bridge until the connect timeout, every
    // cycle, forever.
    let mut h = Harness::new();
    h.bridge.on_event(&ev(sys::SHIM_EV_CLOSED, h.cmd, 0, 0), 0);
    h.wire.borrow_mut().connect_fails = true;

    h.bridge.on_event(&tick(0), 2_000_000);
    match h.bridge.phase() {
        Phase::Dead(reason) => assert!(reason.contains("connect"), "got {reason:?}"),
        other => panic!("expected Dead, got {other:?}"),
    }
}

#[test]
fn the_backoff_grows_and_then_stops_growing() {
    let mut h = Harness::new();
    h.wire.borrow_mut().connect_fails = true;
    h.bridge.on_event(&ev(sys::SHIM_EV_CLOSED, h.cmd, 0, 0), 0);

    // Jump straight to each retry deadline and see how far the next one is pushed out.
    let mut gaps = Vec::new();
    let mut due = h.bridge.pending_retry_at();
    for _ in 0..10 {
        h.bridge.on_event(&tick(0), due);
        let next = h.bridge.pending_retry_at();
        gaps.push(next - due);
        due = next;
    }

    assert!(gaps.windows(2).all(|w| w[1] >= w[0]), "backoff must not shrink: {gaps:?}");
    assert_eq!(
        *gaps.last().unwrap(),
        (MAX_BACKOFF_MS as u64) * 1000,
        "backoff should settle at its ceiling, not keep doubling: {gaps:?}"
    );
}

#[test]
fn a_reconnect_that_cannot_open_a_socket_stays_dead_and_retries() {
    let mut h = Harness::new();
    h.wire.borrow_mut().open_ok_for = 2; // the two from construction, none after
    h.bridge.on_event(&ev(sys::SHIM_EV_CLOSED, h.cmd, 0, 0), 0);

    h.bridge.on_event(&tick(0), 2_000_000);
    match h.bridge.phase() {
        Phase::Dead(reason) => assert!(reason.contains("open"), "got {reason:?}"),
        other => panic!("expected Dead, got {other:?}"),
    }
}

#[test]
fn a_transfer_in_flight_is_abandoned_when_the_session_dies() {
    // Carrying half a payload into the next session splices one transfer onto another.
    let mut h = Harness::with_script(b"OK PUSH C:\\Data\\f.bin 500\r\nDATA 500\r\nonly-a-few-bytes");
    h.ready(0);
    let cmd = poll_until_command(&mut h, 0);
    assert!(matches!(cmd, Command::Push { .. }));
    h.bridge.expect_data_header();
    assert!(h.bridge.read_data().is_none(), "the payload is incomplete");
    assert!(h.bridge.push_in_progress());

    h.bridge.on_event(&ev(sys::SHIM_EV_CLOSED, h.cmd, 0, 0), 0);
    assert!(!h.bridge.push_in_progress(), "the dead session left a transfer open");
    assert_eq!(h.bridge.pending_out(), 0);
}

#[test]
fn logs_buffered_during_an_outage_are_sent_after_reconnecting() {
    let mut h = Harness::new();
    h.ready(0);
    h.bridge.on_event(&ev(sys::SHIM_EV_CLOSED, h.cmd, 0, 0), 0);

    h.bridge.log("what went wrong");

    h.bridge.on_event(&tick(0), 2_000_000);
    assert_eq!(h.bridge.phase(), Phase::Connecting);

    // The reconnect opened a fresh pair of sockets.
    let (cmd2, log2) = {
        let w = h.wire.borrow();
        (w.opened[2], w.opened[3])
    };
    h.cmd = cmd2;
    h.log = log2;
    h.ready(2_000_000);
    h.settle(2_000_000);

    assert!(
        h.sent_text(log2).contains("what went wrong"),
        "the log from the outage was lost: {:?}",
        h.sent_text(log2)
    );
}

#[test]
fn a_desynchronised_command_stream_is_dropped_rather_than_wedging() {
    // A full read buffer with no line terminator in it can never make progress: the
    // buffer cannot grow, and no delimiter is going to appear inside bytes that are
    // already there. Dropping it loses a message; holding it loses the channel.
    let mut h = Harness::with_script(&vec![b'A'; SOCKET_BUF * 3]);
    h.ready(0);

    for t in 0..8 {
        let now = t * 2_000_000;
        h.bridge.on_event(&tick(0), now);
        h.bridge.poll(now);
        h.settle(now);
    }

    assert_eq!(h.bridge.phase(), Phase::Ready, "a garbage stream should not kill the session");
    assert!(
        h.sent_text(h.log).contains("desynchronised"),
        "the drop went unreported: {:?}",
        h.sent_text(h.log)
    );
}

// ── the beacon ────────────────────────────────────────────────────

#[test]
fn the_beacon_fires_on_its_interval_in_every_phase() {
    let mut h = Harness::new();
    // Connecting.
    h.bridge.on_event(&tick(0), BEACON_INTERVAL_US);
    // Ready.
    h.ready(BEACON_INTERVAL_US);
    h.bridge.on_event(&tick(0), BEACON_INTERVAL_US * 2);
    // Dead.
    h.bridge.on_event(&ev(sys::SHIM_EV_CLOSED, h.cmd, 0, 0), BEACON_INTERVAL_US * 2);
    h.bridge.on_event(&tick(0), BEACON_INTERVAL_US * 3);

    let beacons = h.wire.borrow().beacons.clone();
    assert_eq!(beacons.len(), 3, "one beacon per interval, whatever the phase");
    assert!(String::from_utf8_lossy(&beacons[0]).starts_with("EPOCADB "));
}

#[test]
fn the_beacon_does_not_fire_faster_than_its_interval() {
    let mut h = Harness::new();
    h.ready(0);
    for t in 0..100 {
        h.bridge.on_event(&tick(0), t * 100_000); // 10 s of events, 0.1 s apart
    }
    let n = h.wire.borrow().beacons.len();
    assert!(n <= 2, "expected at most two beacons in ten seconds, got {n}");
}
