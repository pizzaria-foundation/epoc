# epocadb — the dev bridge for Symbian

`epocadb` is what `adb` is to Android, cut down to what a 2009 Symbian phone can actually
do. It gives you, over Wi-Fi:

- a **live log stream** from device code to your terminal,
- **file push / pull** to and from the handset,
- **device discovery** on the LAN,
- and a generic **control channel** an app on top can define its own verbs on.

This is the reference. For *why* it is shaped the way it is — the feasibility limits, the
risks, the definition of done — see the design spec, [spec-epocadb.md](spec-epocadb.md).

- Device crate: `crates/epocadb` (`no_std`, `#![forbid(unsafe_code)]`)
- Host CLI: `tools/epocadb`, fronted by `epoc db` (Python 3, no dependencies)
- Tests: `cargo test -p epocadb`, `python3 tools/test_epocadb.py`

---

## Quick start

On the laptop:

```
epoc db serve
```

In the app's gitignored `api.conf`, point the device at the laptop's LAN address and build
with the bridge on:

```
EPOCADB_HOST=192.168.1.10       # the HOST's address, read at build time
```
```
epoc build apps/telegram   # banner must say: dev bridge: on (tg/dev-bridge)
```

Install, run, and the app's `devlog!` lines appear in the `serve` terminal. That is the
whole loop. Everything below is detail.

---

## Topology

The **device is the client**. It connects *out* to the host, so the shim never needs
`listen`/`accept` (which it does not have). The host listens; the device dials in when its
Wi-Fi bearer is up.

```
        host (laptop)                                     device (E72)
  ┌────────────────────────┐                        ┌────────────────────┐
  │ epocadb serve            │   tcp:9091  cmd  ◀──────│ Bridge (cmd sock)  │
  │                        │   tcp:9092  log  ◀──────│ Bridge (log sock)  │
  │                        │   udp:9093  beacon◀─────│ Bridge (UDP)       │
  │ tcp:10091 control      │   (loopback, CLI→serve) │                    │
  └────────────────────────┘                        └────────────────────┘
```

| Port | Dir | Purpose |
|---|---|---|
| `tcp:9091` | device → host | command channel (CRLF lines + binary payloads) |
| `tcp:9092` | device → host | log stream (LF lines, fire-and-forget) |
| `udp:9093` | device → broadcast | discovery beacon, every ~8 s |
| `tcp:10091` | CLI → serve | loopback control: other `epocadb` invocations queue work here |

**Wi-Fi only.** Without a SIM, cellular access points answer `KErrEtelGsmBase`; host and
device must be on the same LAN.

---

## Wire protocol

Plain text, one message per line. **The command channel uses `CRLF`; the log channel uses
`LF` alone** — the log is the ring buffer's bytes handed over unchanged, and the ring
separates lines with a single `\n`. The host reader accepts both.

```
  cmd channel (tcp:9091)
    REQ  <verb> [args]        device asking for work
    OK   [detail]             success, or a command for the device to carry out
    ERR  <message>            failure
    DATA <byte-count>         a binary payload follows immediately
    <bytes>                   the payload

  log channel (tcp:9092)
    <line>\n                  one log line; no response, no backpressure
```

### The polling model

The device drives. It sends `REQ PING` **at most once per second**, and only when no
request is already outstanding. The host answers `OK pong` when it has nothing — or answers
with a command instead. That reply *is* the work order.

```
  device                              host
    │ REQ PING                         │
    ├─────────────────────────────────▶│
    │                        OK pong    │   nothing to do
    │◀─────────────────────────────────┤
    │ REQ PING                          │
    ├─────────────────────────────────▶│
    │      OK PUSH C:\Data\f.bin 281    │   here is some work
    │◀─────────────────────────────────┤
    │             DATA 281 + <281 B>    │   header and payload, back to back
    │◀─────────────────────────────────┤
    │ OK wrote 281 bytes                │
    ├─────────────────────────────────▶│
```

A pull is the mirror: the host replies `OK PULL <path>`, the device answers `OK <size>`
then `DATA <size>` and the bytes.

The rate limit and the in-flight guard are not tuning knobs — they are correctness. The app
polls the bridge on **every** shim event (timers, keys, redraws), so an unconditional `REQ`
per poll would be thousands a minute into a 1 KB transmit buffer; once it fills, a line goes
out cut in half and neither side can parse the channel again.

### Commands the host can send in a pong reply

| Reply | Becomes | Meaning |
|---|---|---|
| `OK pong` | `Command::None` | nothing to do |
| `OK PUSH <path> <size>` | `Command::Push` | host will send `DATA <size>` + bytes; device writes `<path>` |
| `OK PULL <path>` | `Command::Pull` | device replies `OK <size>` + `DATA` + bytes |
| `OK INSTALL <path> <size>` | `Command::Install` | like Push, to an install location |
| `OK CTL <line>` | `Command::Control(line)` | **generic passthrough** — the bridge does not interpret it; the app on top does |
| `OK QUIT` | `Command::Quit` | tear the session down |

`CTL` is the extension point: epocadb stays a dumb pipe and whatever runs on it parses the
line. See *Building on the control channel*.

### Timeouts and recovery

Every wait is bounded — the worst failure on a device with no console is the one that just
sits there.

| | |
|---|---|
| connect completes | 30 s, else retry on the backoff |
| reply to a `REQ` | 15 s, counted as a miss |
| consecutive misses → teardown | 4 |
| reconnect backoff | 1 s doubling to 64 s, then flat, forever |
| host-side operation | 60 s, then abandoned; the session goes idle |

A host killed mid-session leaves a socket that is open and silent — no socket error reports
that, only the reply clock does. A command stream that desynchronises (a full read buffer
with no line terminator) is dropped and announced rather than wedging the channel.

---

## Host CLI (`epoc db`, i.e. `tools/epocadb`)

```
epoc db serve                     # cmd + log + file transfer + control. The usual one.
epoc logcat                    # log only; cannot run while serve holds 9092
epoc db devices                   # UDP beacon listener — proves the device is on the LAN
epoc db push  <local> <remote>    # e.g. push ./app.sis "C:\Data\app.sis"
epoc pull  <remote> <local>    # writes to the local path you name (dirs created)
epoc db install <.sis> [--remote] # transfer a .sis; open it on the device to install
```

`push`, `pull` and `install` do not talk to the device directly — they hand a line to a
running `serve` over the loopback control port (`10091`), which forwards it to the device on
the next poll. So `serve` must already be running; if it is not, they say so and exit.

`serve` renders the log stream, colouring each line by its leading `[tag]` (`[net]`, `[ui]`,
`[mem]`, `[gfx]`, `[step]`, `[recv]`, `[log]`) when stdout is a terminal. A line with no tag,
or with a tag the CLI does not know, prints as-is.

Paths are tab-separated internally, because a Symbian path is full of backslashes and may
contain spaces — a space is not a separator anything can rely on.

---

## Device API (`crates/epocadb`)

One type, generic over the network so the whole state machine is testable against a fake:

```rust
pub struct Bridge<N: Net = ShimNet> { /* … */ }

impl Bridge<ShimNet> {
    /// Open both sockets over the shim's network. Must not be called before a bearer is
    /// up — a socket opened on a connection that has not started panics esock.
    pub fn connect(host: Ipv4, bearer_handle: Option<i32>) -> Result<Self>;
}

impl<N: Net> Bridge<N> {
    pub fn connect_with(net: N, host: Ipv4, bearer: Option<i32>, now_us: u64) -> Result<Self>;

    /// Drive both sockets. `now_us` is monotonic_us(), passed in so every deadline is
    /// testable. Call on every shim event.
    pub fn on_event(&mut self, ev: &ShimEvent, now_us: u64);

    /// Ask the host for work on the ping interval; returns any command it answered with.
    pub fn poll(&mut self, now_us: u64) -> Option<Command>;

    pub fn is_ready(&self) -> bool;
    pub fn phase(&self) -> Phase;          // Connecting | Ready | Dead(reason)
    pub fn dropped_logs(&self) -> u32;

    pub fn log(&mut self, line: &str);     // queue a log line (LF channel)
    pub fn reply(&mut self, detail: &str); // answer a command on the cmd channel
    pub fn send_line(&mut self, line: &str) -> Result<()>;
    pub fn send_data(&mut self, data: &[u8]) -> Result<()>;   // DATA header + payload, together

    pub fn expect_data_header(&mut self);  // an incoming push follows
    pub fn push_in_progress(&self) -> bool;
    pub fn read_data(&mut self) -> Option<Vec<u8>>;  // accumulate a payload across events

    pub fn pending_out(&self) -> usize;    // bytes still queued on the cmd channel
    pub fn pending_retry_at(&self) -> u64; // when the next reconnect is due (Dead only)
}
```

`Command` is `None | Push{path,size} | Pull{path} | Install{path,size} | Control(String) |
Quit`.

### The event loop shape

```rust
let now = symbian::monotonic_us();
bridge.on_event(ev, now);
if bridge.is_ready() {
    // finish a push that spans several events, first — it owns the channel
    if pending_push.is_some() {
        if let Some(data) = bridge.read_data() {
            write_file(&pending_push.take().unwrap(), &data);
            bridge.reply("OK wrote");
        }
    } else {
        match bridge.poll(now) {
            Some(Command::Push { path, size: _ }) => { bridge.expect_data_header(); pending_push = Some(path); }
            Some(Command::Pull { path })          => serve_pull(&mut bridge, &path),
            Some(Command::Control(line))          => handle_control(&line, &mut bridge),
            Some(Command::Quit)                   => should_exit = true,
            _ => {}
        }
    }
}
```

Three invariants make this correct, all inherited from how the shim delivers I/O:

1. **Both sockets see every event.** `TcpStream::on_event` filters by handle, so feeding it
   another socket's event is free; feeding it *none* is not — the platform clears the
   send-in-flight flag only on `SHIM_EV_SENT`, so a stream that never sees its own send
   completion sends once and then queues forever.
2. **Every send goes through a queue.** `write` accepts what fits and reports how much;
   treating that count as "all of it" truncates any payload past ~1 KB and reports success.
3. **Buffered bytes are consumed before the socket.** A `DATA n` header and its payload
   arrive in the same TCP segment, so after the header is parsed the payload's front is
   already in the read buffer — going to the socket for it strands those bytes.

### Embedding it in an app — the feature-gate pattern

The whole of the bridge lives behind a cargo feature so a signed release build carries
neither the crate nor its sockets. The pattern is in `apps/telegram/src/devbridge.rs`: two
sibling modules, `enabled` (real) and `disabled` (a zero-sized struct whose methods vanish),
selected by `#[cfg(feature = "dev-bridge")]`, both exporting the same `DevBridge`. The rest
of the app holds a `DevBridge` and calls it unconditionally — no `#[cfg]` leaks out.

To wire a new app, see the checklist in the `epocadb-logging` skill (`Cargo.toml`,
`devbridge.rs`, `app.conf`, `api.conf`, the `handle_raw` call, connect-after-bearer, and the
build banner).

---

## Logging

There are three ways to get text off the device; they answer different questions.

| | `Trace` | `devlog!` → epocadb | `symbian::applog` |
|---|---|---|
| Where | `C:\Data\logs_<app>.txt` | the host terminal, live |
| Needs | nothing — no capability, no host, no network | bearer + `dev-bridge` + `serve` running |
| Survives a crash | yes, appended per line and across launches | only what already flushed |
| Retrieved by | `epoc pull` afterwards | watching it happen |

One call reaches both:

```rust
symbian::log!("[net] connect state={state} err={err}");
```

The switch is `DEBUG=` in `app.conf`, and `DEBUG=0` removes the call and its format string
rather than silencing it — `symbian::log::ENABLED` is a `const` read from `SYMBIAN_DEBUG`,
which `tools/symbuild` exports. Measured on the Telegram client: 386,700 bytes on, 376,752
off.

`symbian_app::devbridge::connect` registers the bridge as a second sink through
`symbian::log::set_sink`, which is why the same line reaches the host once a bearer is up and
the file regardless. The file path is chosen by a ladder (`C:\Data\` first, then
`C:\logs\`, the drive root, and the private cage last); `symbian::log::path_label()` reports
which rung won.

### Log format

`AREA verb key=value key=value` — uppercase area first, so `rg` over a captured session
works without a parser. Log **state transitions, decisions and errors**, one line each,
under ~100 chars. Log Symbian error codes as numbers *and* names (`-4180` and
`KErrEtelGsmBase` are one fact; only one is greppable). Never log a secret, a message body,
or the middle of a phone number — a log gets pasted into a chat window.

### The ring buffer is 2 KB, not a log file

- `LOG_BUFFER_SIZE = 2048` bytes total pending.
- A single line's body is capped at `N/2 − 1` = **1023 bytes**, truncated at a char
  boundary; longer lines lose their tail.
- On overflow the **oldest** line is dropped and the gap is announced in-band:
  `-- epocadb: 14 log line(s) dropped --`. Seeing that means you are logging faster than a
  1 KB-per-flush socket drains — so never log inside `draw` or on a per-event path.

---

## Building on the control channel (`CTL`)

`epocadb` deliberately does not know anyone's verbs. To add device-side control from the host:

1. Host: send `CTL <your line>` over the control channel. `serve` forwards it as
   `OK CTL <your line>` in a pong reply.
2. Device: `bridge.poll()` returns `Command::Control("<your line>")`. Parse it and act.

Replies go back on the **log** channel, so watch `serve` output for them. An app that defines
no control verbs should answer `ERR …` rather than drop the line, so a host tool is not left
waiting — which is what `symbian_app::devbridge` does out of the box.

---

## Security

None, by design: no authentication, no encryption, and the device will write any path the
host names within its sandbox. The threat model is "same LAN, during development." The
`dev-bridge` cargo feature is what keeps all of this out of a build you sign and hand to
someone — do not enable it in a release. The file sink is a different matter: it needs no
capability and opens no socket, so `DEBUG=1` in a shipped build leaks only what you chose to
log, to the phone's own disk.

---

## Testing

| | |
|---|---|
| `cargo test -p epocadb` | ring buffer, protocol parsing, and the full state machine against a fake `Net` (see `crates/epocadb/src/wire_tests.rs`) |
| `python3 tools/test_epocadb.py` | line framing and the serve loop end to end against a fake device over loopback |

The wire tests exist because the parsing tests could not fail for any of the reasons the
bridge actually broke (a send reporting success after one segment, a socket never handed its
own completions, a payload already in the buffer being waited for). Each names the bug it
pins; every critical fix was checked by reintroducing the bug and watching the test go red.

---

## Troubleshooting

| symptom | first thing to check |
|---|---|
| no lines at all | build banner said `dev bridge: on`? `EPOCADB_HOST` in `api.conf`? |
| device never appears | `epoc db devices` — a beacon means it is on the LAN; silence means Wi-Fi is not up |
| connected, then silence | 15 s reply timeout × 4 misses tears the session down; it reconnects on a 1→64 s backoff |
| gaps in the sequence | look for `-- epocadb: N log line(s) dropped --`; you are over the 2 KB budget |
| a line ends mid-word | over 1023 bytes |
| `push`/`monitor` says "no serve" | start `epoc db serve` first — the CLI talks to it, not the device |
| `logcat` refuses to bind | `serve` already holds `9092` |
| host IP changed, still silent | `EPOCADB_HOST` is `option_env!` — baked at build time; rebuild, don't just restart |
