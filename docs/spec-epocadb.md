# Spec: epocadb — dev bridge for Symbian

## What this is

A two-component dev tool that does what ADB does on Android, minus what is impossible
on Symbian 9.3:

| | ADB | epocadb |
|---|---|---|
| push / pull files | `adb push/pull` | ✅ |
| install an app | `adb install` | ⚠️ transfers the `.sis`; the user opens it |
| stream logs in real time | `adb logcat` | ✅ |
| discover devices on the LAN | `adb devices` | ✅ |
| interactive shell | `adb shell` | ❌ no fork/exec |
| reboot device | `adb reboot` | ❌ needs PowerMgmt |
| port forwarding | `adb forward` | ❌ low value |

## Architecture

```
┌──────────────────────────────────────────────────────────────┐
│  host (laptop)                                                │
│                                                               │
│  tools/epocadb                      CLI                         │
│  ├── push <local> <remote>                                    │
│  ├── pull <remote> <local>                                    │
│  ├── install <.sis> [--remote]                                │
│  ├── logcat                                                   │
│  ├── devices                                                  │
│  └── serve            listens for device connections          │
│       tcp:9091         (command channel)                       │
│       tcp:9092         (log stream)                            │
│       tcp:10091        (loopback only: the CLI's control port) │
│       udp:9093         (discovery beacons)                     │
│                                                               │
└──────────────────────────────┬───────────────────────────────┘
                               │ Wi-Fi (same LAN)
┌──────────────────────────────┴───────────────────────────────┐
│  device (E72)                                                 │
│                                                               │
│  crates/epocadb                                                 │
│  ├── Bridge<N: Net>           holds the two sockets            │
│  ├── cmd protocol             text-based, one per line         │
│  ├── ring buffer              logs → TCP                       │
│  └── UDP beacon               announces itself every 8 s       │
│                                                               │
│  apps/telegram/src/devbridge.rs   the application's side       │
│                                                               │
│  Driven from the event loop; the bridge is polled, never blocks.│
│  No shim changes. Uses existing TcpStream, UdpSocket and fs.    │
└──────────────────────────────────────────────────────────────┘
```

The device is the **client** — it connects out to the host. This avoids adding
`Listen`/`Accept` to the shim. The host runs `epocadb serve` which listens on two
ports; the device connects when its bearer is up.

## Protocol

Plain text, one line per message. **The command channel uses CRLF; the log channel
uses LF alone**, because the log is the ring buffer's bytes handed over unchanged and
the ring separates lines with a single `\n`. The host's reader accepts both.

```
         DEVICE ──────────────────────────────▶ HOST

  cmd channel (tcp:9091)

    REQ  <verb> [args]                           device asking for work
    OK   [detail]                                success, or a command to carry out
    ERR  <message>                               failure
    DATA <byte-count>                            binary payload follows
    <bytes>                                      raw payload

  log channel (tcp:9092)

    <line>\n                                     one log line, fire and forget
```

### Who asks whom

The device drives. It sends `REQ PING` **at most once per second**, and only when no
request is already outstanding. The host answers `OK pong` when it has nothing, or
answers with a command instead — that reply *is* the work order.

```
  device                              host
    │  REQ PING                        │
    ├─────────────────────────────────▶│
    │                        OK pong   │      nothing to do
    │◀─────────────────────────────────┤
    │                                  │
    │  REQ PING                        │
    ├─────────────────────────────────▶│
    │       OK PUSH C:\Data\f.bin 281  │      here is some work
    │◀─────────────────────────────────┤
    │              DATA 281 + <281 B>  │      header and payload, back to back
    │◀─────────────────────────────────┤
    │  OK wrote 281 bytes              │
    ├─────────────────────────────────▶│
```

A pull runs the other way: the host replies `OK PULL <path>`, and the device answers
`OK <size>` followed by `DATA <size>` and the bytes.

The rate limit and the in-flight guard are not tuning. The application polls the
bridge on every shim event — timers, key presses, redraws — so an unconditional
`REQ` per poll is thousands a minute into a 1 KB transmit buffer. Once it fills, a
line goes out cut in half and neither side can parse the channel again.

### Timeouts

Every wait is bounded, because the failure that costs the most time is the one that
just sits there:

| | |
|---|---|
| connect completes | 30 s, then retry on the backoff |
| a reply to a `REQ` | 15 s, counted as a miss |
| consecutive misses before teardown | 4 |
| reconnect backoff | 1 s doubling to 64 s, then flat |
| a host-side operation | 60 s, then abandoned and the session goes idle |

A host that was killed mid-session leaves a socket that is open and silent. No socket
error reports that; only a clock does.

### Log streaming

The log channel is fire-and-forget. Lines land in a fixed ring buffer and are drained
to the socket as sends complete. When the ring overflows the oldest line is dropped, a
counter is incremented, and the gap is announced **in the log itself**:

```
connecting to DC2...
-- epocadb: 14 log line(s) dropped --
auth key negotiated (new)
```

A silent gap reads as "that code never ran", which is the most expensive wrong
conclusion available when debugging on a handset with no other output.

## Device side — `crates/epocadb`

A `no_std` crate, `#![forbid(unsafe_code)]`, with one public type:

```rust
pub struct Bridge<N: Net = ShimNet> { /* ... */ }

impl Bridge<ShimNet> {
    /// Open both sockets over the shim's network.
    pub fn connect(host: Ipv4, bearer_handle: Option<i32>) -> Result<Self>;
}

impl<N: Net> Bridge<N> {
    /// Open over a caller-supplied Net and clock, for tests.
    pub fn connect_with(net: N, host: Ipv4, bearer: Option<i32>, now_us: u64) -> Result<Self>;

    /// Drive both sockets. `now_us` is monotonic_us(), passed in so every
    /// deadline in the file is testable.
    pub fn on_event(&mut self, ev: &ShimEvent, now_us: u64);

    /// Ask the host for work on the ping interval.
    pub fn poll(&mut self, now_us: u64) -> Option<Command>;

    /// Accumulate an incoming payload across events.
    pub fn expect_data_header(&mut self);
    pub fn read_data(&mut self) -> Option<Vec<u8>>;

    pub fn log(&mut self, line: &str);
    pub fn reply(&mut self, detail: &str);
    pub fn send_data(&mut self, data: &[u8]) -> Result<()>;
    pub fn is_ready(&self) -> bool;
}
```

Generic over `Net` so the state machine can be driven against a fake in tests, and
`Bridge` on its own still means `Bridge<ShimNet>` — the device build is unchanged.

### Three invariants worth stating

1. **Both sockets get every event.** `TcpStream::on_event` filters by handle, so
   handing it another socket's event is free. Handing it *none* is not: the platform
   clears `tx_pending` only on `SHIM_EV_SENT`, so a stream that never sees its own
   send completion issues one send and then queues forever.

2. **Every send goes through a queue.** `TcpStream::write` accepts what fits and
   reports how much. With a 1 KB transmit buffer, treating that count as "all of it"
   truncates any payload bigger than a kilobyte and reports success.

3. **Buffered bytes are consumed before the socket is.** A `DATA n` header and the
   bytes it describes arrive in the same segment, so after the header is parsed the
   payload's front is already in the read buffer. Going to the socket for it strands
   those bytes and waits forever for what is already in hand. The host's reader has
   the same rule for the same reason.

### Ring buffer

Fixed size, no allocation at runtime, power-of-two enforced at compile time. Every
byte belongs to a `\n`-terminated line — `push` never writes a body without its
terminator — which is what lets the cursors be trusted. Lines that straddle the wrap
are copied in two parts; a caller-provided buffer is taken rather than a `&str`
returned, because there is nowhere to borrow a contiguous one from.

## Host side — `tools/epocadb`

A Python script with no dependencies. Nothing in the serve loop blocks on the device:
every read is buffered and resumable, every operation carries a deadline.

### `epocadb serve`
```
$ epocadb serve
epocadb serve  listening on tcp:9091 (cmd)  tcp:9092 (log)
              waiting for device...  (ctrl-c to stop)

cmd  connected from 192.168.1.42:2094
log  connected from 192.168.1.42:2095

  [log] connecting to DC2...
  [log] auth key negotiated (new)
```

Accepts one device at a time and takes work from the CLI over a loopback control
port. Other `epocadb` invocations queue commands through it.

### `epocadb push <local> <remote>`
```
$ epocadb push ./session.bin 'C:\Data\session.bin'
OK queued
```

### `epocadb pull <remote> <local>`
```
$ epocadb pull 'C:\Data\report.txt' ./out/report.txt
OK queued
```
Writes to the path given, creating parent directories.

### `epocadb install <.sis> [--remote PATH]`
```
$ epocadb install ./build/telegram.sis
OK queued
      once written, open C:\Data\telegram.sis on the device to install it
```
A transfer to a public path plus a manual step, not a silent install — see risk 3.

### `epocadb logcat`
Log-only listener on tcp:9092. Cannot run while `serve` holds the port.

### `epocadb devices`
```
$ epocadb devices
Device                         Address            Last seen
---------------------------------------------------------
EPOCADB 0.2 device=Nokia E72     192.168.1.42       3s ago
```
Listens for the UDP beacon. Devices unheard from for 30 s drop off the list — a
device that has gone quiet is not one you can push to.

## Build integration

`tools/symbuild` enables the `dev-bridge` cargo feature when `EPOCADB_HOST` is set in
the app's gitignored `api.conf`, and not otherwise:

```
==> telegram  (uid3 0xE1234569, caps: NetworkServices+WriteUserData)
--> api credentials: id 2040
--> epocadb host: 192.168.1.10
--> rust: device
    dev bridge: on (tg/dev-bridge)
```

The app names its own feature in `app.conf` via `DEV_BRIDGE_FEATURE`. With the
feature off, `apps/telegram/src/devbridge.rs` compiles to a zero-sized struct whose
methods do nothing, `epocadb` is not a dependency at all, and the binary carries no
listener.

## Testing

| | |
|---|---|
| `cargo test -p epocadb` | 66 tests: ring buffer, protocol parsing, and the state machine against a fake `Net` |
| `python3 tools/test_epocadb.py` | 15 tests: line framing, and the serve loop end to end against a fake device over loopback |

The wire tests exist because the parsing tests could not fail for any of the reasons
the bridge actually broke. Each one names the bug it pins; every critical fix was
checked by reintroducing the bug and confirming the test goes red.

## Risks and open questions

1. **Wi-Fi-only in practice.** Without a SIM, cellular access points answer
   `KErrEtelGsmBase` — the bridge only works over Wi-Fi. The host and device
   must be on the same LAN. Acceptable for development; documented.

2. **Socket count.** The bridge uses 2 TCP sockets plus 1 UDP. That leaves 5 of
   the 8-total budget. An app that already uses sockets (Telegram, 1-2) still
   has room.

3. **Install is a transfer plus a manual step.** Writing to
   `\private\10003a3f\import\apps\` needs capabilities this bridge does not hold, so
   `install` puts the `.sis` in `C:\Data\` and the user opens it from File Manager.
   Making it automatic means `RApaLsSession::UpdateAppList` in the shim (~30 lines of
   C++) *and* a capability the app would then carry in release. Not worth it yet.

4. **Host address is a build-time constant.** The device reads `EPOCADB_HOST` through
   `option_env!`, so changing the host IP means rebuilding. The beacon solves the
   reverse direction (finding the device); finding the *host* from the device would
   need the address in a config file the app reads at startup. Worth doing if the
   rebuild becomes annoying.

5. **Security.** No authentication, no encryption, and the device will write any path
   the host names within its sandbox. The threat model is "same LAN, during
   development", and the `dev-bridge` feature is what keeps that out of a release
   build. Do not enable it in anything you sign and hand to someone.

6. **UDP broadcast needs no socket option.** Unlike BSD sockets, esock has no
   `SO_BROADCAST` equivalent to set — `KSIBroadcast` in `es_sock.h` is a protocol
   capability flag, not a settable option. `shim_udp_send_to` to 255.255.255.255
   works as-is.

## Phase 3 — TCP server (listen/accept)

If the polling model proves limiting, add `Listen` + `Accept` to `shim_net.cpp`:

```
shim_tcp_listen(handle, port, queue)
shim_tcp_accept(listen_handle, *new_handle)
SHIM_EV_ACCEPTED  = 26
```

~150 lines of C++, one new `CActive` subclass (`CSockAcceptor`). The `Bridge`
then starts in listen mode instead of connect mode; the host connects to the
device instead of the reverse. The protocol is identical; only the socket setup
differs.

## Definition of done

- [x] `cargo test --workspace` green, with tests for the ring buffer and protocol
- [x] Socket-level tests for the state machine, against a fake `Net`
- [x] Host-side tests, including the serve loop end to end
- [x] `epocadb serve` + device connects and streams logs
- [x] `epocadb push` copies a file to the device
- [x] `epocadb pull` copies a file from the device
- [x] `epocadb devices` lists what is on the LAN
- [x] `dev-bridge` feature flag gates the crate out of release builds
- [ ] `epocadb install` picked up automatically — deliberately not done, see risk 3
- [ ] Verified end to end on the handset — the tests cover both halves against
      fakes, but the two have never spoken to each other over real Wi-Fi
- [ ] Device with no Wi-Fi degrades gracefully (report file fallback)
