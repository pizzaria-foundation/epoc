---
name: epocadb-logging
description: Emit logs from device code — `symbian::log!`, the `DEBUG=` switch in app.conf, the file at `C:\Data\logs_<app>.txt`, and the live epocadb stream (`epoc db serve` / `epoc logcat`). Use whenever adding, reviewing, or debugging logging, tracing, `symbian::log`, `symbian::applog`, the dev bridge, or "why is nothing printing on the phone" — and whenever instrumenting an app or SDK crate that runs on the handset.
---

# Getting a log line off the handset

Symbian gives an application no console, no logger and no debugger. A line that is not
deliberately routed somewhere is gone.

There is **one** call and **one** switch. Everything else is where the line comes out.

```rust
symbian::log!("[net] connect state={state} err={err}");
```

```
# app.conf
DEBUG=1
```

Full reference: [docs/epocadb.md](../../../docs/epocadb.md). The implementation is
`crates/symbian/src/log.rs` (the call, the switch, the file) and
`crates/symbian-app/src/devbridge.rs` (the live stream).

## What that one line does

| | the file | the live stream |
|---|---|---|
| Where | `C:\Data\logs_<app>.txt` | the host terminal, as it happens |
| Needs | nothing — no capability, no host, no network | Wi-Fi bearer + `dev-bridge` feature + `epocadb serve` running |
| Survives a crash | yes, appended per line, and across launches | only what already flushed |
| Cost | one open/append/close (a 64 KB flash write measures 23 us) | a memcpy into a 2 KB ring |
| Gets it | `symbian::log!` always | `symbian::log!` too, once `devbridge::connect` has run |

Both come from the same call. There is no second macro to remember and no decision to make
at the call site: `connect` registers the bridge as a second sink with
`symbian::log::set_sink`, so a line written before the bearer is up still lands in the file,
and the same line after it also reaches the host.

## The switch, and what "off" means

`DEBUG=0` (the default in `tools/symbuild`) does not silence the log — it **removes** it.
`symbian::log::ENABLED` is a `const bool` read from `SYMBIAN_DEBUG`, which `symbuild` exports
from `DEBUG=`, so the macro expands to `if false { … }` and the optimiser deletes the call
*and its format string*. Measured on the Telegram client: 386,700 bytes with `DEBUG=1`,
376,752 with `DEBUG=0`.

So: **write the line while the code is fresh.** Leaving instrumentation in the source costs a
release build nothing. Do not delete log lines to "clean up for release" — flip the switch.

Corollaries worth knowing:

- The build banner prints `debug log: on → C:\Data\logs_<app>.txt` or `debug log: off`. If it
  says off, no amount of `symbian::log!` will produce anything.
- It is an environment variable rather than a cargo feature on purpose: no app declares
  anything, and `tools/symnew` scaffolds `DEBUG=1`.
- `SYMBIAN_APP_NAME` (also from `symbuild`) picks the filename. Never hardcode it.

## Wiring the live stream into a new app

The file works with nothing. The stream needs four things:

1. `app.conf`: `DEV_BRIDGE_FEATURE=<crate>/dev-bridge`.
2. `Cargo.toml`: `dev-bridge = ["symbian-app/dev-bridge"]` — forward it, do not re-implement it.
3. `api.conf` (gitignored): `EPOCADB_HOST=192.168.x.x`, the **host's** LAN address. Read through
   `option_env!`, so it is baked at build time: changing it means rebuilding, not restarting.
4. Two calls, and no `#[cfg]` anywhere:

```rust
// in the raw event handler, early
if symbian_app::devbridge::on_event(ev) { self.should_exit = true; }

// once a bearer is up — never before: a socket opened on a connection that has not
// started panics esock rather than returning an error
if !symbian_app::devbridge::is_connected() {
    symbian_app::devbridge::connect(bearer_handle);
}
```

Confirm the banner says `dev bridge: on (<crate>/dev-bridge)`. `symbuild` enables the feature
only when both `EPOCADB_HOST` and `DEV_BRIDGE_FEATURE` are set.

## Host side

```
epoc db serve    # cmd (9091) + log (9092) + file transfer + control. What you want.
epoc logcat      # log only — cannot run while serve holds 9092
epoc db devices  # UDP beacon listener; proves the device is on the LAN at all
epoc pull C:\Data\logs_<app>.txt   # the file, for when nothing was watching
```

`pull` is the one that makes the file worth having: a crash nobody was watching is still
readable afterwards. `push`/`pull`/`install` reach the device through a running `serve` over
the loopback control port (10091) — they do not talk to the phone directly.

Wi-Fi only. Without a SIM, cellular access points answer `KErrEtelGsmBase`; host and device
must sit on the same LAN.

## Budget — the stream is a 2 KB ring, not a log file

- `LOG_BUFFER_SIZE = 2048` bytes total pending (`crates/epocadb/src/lib.rs`).
- A single line's body is capped at **1023 bytes**, truncated at a char boundary. The file
  applies the same cap (`symbian::log`'s `MAX_LINE`) and appends ` …`.
- Overflow drops the **oldest** line and announces the gap: `-- epocadb: 14 log line(s)
  dropped --`. Seeing that means you are logging faster than a 1 KB-per-flush socket drains.
- The file restarts once it passes `symbian::LOG_MAX` (64 KB) rather than growing without
  bound — the newest lines are the ones a diagnosis needs.

So:

- **Never log inside `draw`, per frame, or per key event.** The bridge is polled on every shim
  event, and the file sink is real I/O on the GUI thread.
- Log **state transitions, decisions and errors** — where a wrong assumption becomes visible.
  One line per transition, not one per step toward it.
- Keep a line under ~100 chars. Numbers and error codes, not prose.

## Format

A bracketed category, then `verb key=value`:

```
[net] connect state=2 err=-4180
[auth] key negotiated dc=4 new=1
[store] dialogs merged new=7 total=42
[ui] screen=Conversation rows=18
```

The bracket is what `epocadb serve` colours by (`net`, `ui`, `mem`, `gfx`, `step`, `recv`,
`log`); an unknown tag is not an error, just uncoloured. It is also what makes a 60 KB file
greppable without a parser.

Every Symbian error is negative and the log is mostly error codes — log the code, and the
name too when you have it (`-4180` and `KErrEtelGsmBase` are the same fact, and only one is
greppable in the SDK).

## What never goes in a log line

The auth key, an API secret, a 2FA password, message bodies, a full phone number. A log gets
pasted into a chat window; a log with an auth key in it *is* the account. Log the **length**,
the **error** and the **shape** instead.

`symbian::log::redact_phone` is the masking to use (first 3 and last 2 characters, the rest
starred), and its unit tests pin it — including the short-string case, where it reveals
nothing rather than panicking.

## SDK crates still do not log on their own

`symbian-gfx`, `symbian-ui`, `symbian-crypto`, `symbian-audio` are `no_std`,
`forbid(unsafe_code)` libraries. A crate reports by **returning a typed error or a counter**;
the app decides whether that is worth a line. `symbian::log!` being reachable from a crate is
not permission to use it there — if you are tempted, the missing thing is a return value.

`symbian::log` itself is the exception by construction: a facility apps *call*, holding its
own state because there is exactly one log per process.

## The simulator sees only the logic

On the host both sinks are no-ops: the file writer does nothing without a phone, and
`dev-bridge` is off. Verify the *logic* with host tests, the *stream* on hardware. A
`cargo run -p <app> --example sim` session produces no log output, and that is expected.

## Triage

| symptom | first thing to check |
|---|---|
| nothing anywhere | did the build banner say `debug log: on`? `DEBUG=1` in `app.conf`? |
| file but no stream | banner said `dev bridge: on`? `EPOCADB_HOST` in `api.conf`? did `connect` run after the bearer came up? |
| device never appears | `epoc db devices` — a beacon means it is on the LAN; silence means Wi-Fi is not up |
| connected, then silence | 15 s reply timeout × 4 misses tears the session down; it reconnects on a 1→64 s backoff |
| gaps in the sequence | look for `-- epocadb: N log line(s) dropped --`; you are over budget |
| a line ends in ` …` | over 1023 bytes |
| `logcat` refuses to bind | `serve` already holds 9092 |
| file is missing on the phone | check `symbian::log::path_label()` — the ladder falls back to `C:\logs\`, the drive root, then the private cage, and only the last is unreadable from outside |
| a `pull` says not found | the app may never have logged: the path is resolved lazily, on the first line |

## Before calling logging work done

- [ ] Every new line has a `[tag]` prefix and is under ~100 chars.
- [ ] No line is inside `draw` or on a per-event path.
- [ ] No secret, no message body, no full phone number in any of them.
- [ ] Boot-path facts are logged too — the file has them even though the bridge is not up yet.
- [ ] No second logging mechanism was invented. One macro, one switch.
- [ ] `cargo test -p symbian` / `cargo test -p epocadb` / `python3 tools/test_epocadb.py` still
      green if you touched the log, the bridge, or the host script.
