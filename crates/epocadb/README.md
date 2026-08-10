# epocadb

The dev bridge: the device side. `no_std`, `forbid(unsafe_code)`, 67 tests.

adb for a 2009 Nokia. Live logs, file push and pull, and a control channel, over two TCP
sockets the *device* opens to your machine.

```text
device ──tcp:9091 (cmd)──▶ host   REQ/OK/ERR/DATA protocol
device ──tcp:9092 (log)──▶ host   raw line stream, LF-separated
device ──UDP broadcast──▶ 255.255.255.255:9093   discovery beacon, every ~8 s
```

The host side is `tools/epocadb`, fronted by `epoc db`. The full reference is
[docs/epocadb.md](../../docs/epocadb.md); the design rationale is
[docs/spec-epocadb.md](../../docs/spec-epocadb.md).

## The device dials out

There is no `Listen`/`Accept` here, and that is deliberate: a listening socket on the
handset needs the phone to be reachable, and on a carrier network it is not. Dialling out
works from any Wi-Fi the phone can join.

## Three things that are not obvious

**Both sockets see every event.** `TcpStream::on_event` filters by handle, so handing it
an event for the other socket is free. Handing it *no* events is not: the platform clears
`tx_pending` only on `SHIM_EV_SENT`, so a stream that never sees its own send completion
issues one send and queues forever. The log socket is driven on every event, not only when
something is written to it.

**Nothing blocks and nothing spins.** `write` accepts what fits and reports how much, so
every send goes through an outbound queue that drains as completions arrive. `read`
returning 0 is a normal state, not an error — partial reads are kept and resumed.

**The log is a 2 KB ring, not a file.** Overflow drops the *oldest* line and says so in
the stream (`-- epocadb: 14 log line(s) dropped --`), because a silent gap is worse than a
short one. One line is capped at 1023 bytes, truncated on a char boundary. If you need the
whole history, that is what the file sink in `symbian::log` is for — the two are the same
call, and `epoc pull` fetches the file afterwards.

## Modules

| | |
|---|---|
| `lib` | `Bridge` — the state machine, the protocol, the two streams, the beacon |
| `ring` | the log ring buffer: oldest-drops-first, gap accounting, char-boundary truncation |
| `protocol_tests` | the wire vocabulary, parsed and asserted line by line |
| `wire_tests` | whole conversations against a scripted host, including the partial reads |

## Using it

An app does not talk to this crate directly. `symbian_app::devbridge` owns the singleton
and registers itself as a second sink for `symbian::log!`, so a line reaches the host and
the file from one call. Two calls wire it up — see the
[logging skill](../../.claude/skills/epocadb-logging/SKILL.md).

## Security, or the absence of it

None, by design: no authentication, no encryption, and the device will write any path the
host names inside its sandbox. The threat model is "same LAN, during development", and the
`dev-bridge` cargo feature is what keeps all of it out of a build you sign and hand to
someone.

---

Part of [epoc](../../README.md), a Rust SDK for Symbian S60 3rd Edition. MIT licensed; see
`LICENSE` at the repository root. Written with AI assistance,
and every hardware claim in it was measured rather than reasoned about.
