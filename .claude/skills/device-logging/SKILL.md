---
name: device-logging
description: Emit logs from device code — `symbian::log!`, the `DEBUG=` switch in app.conf, the file at `C:\Data\_logs\<app>.txt`, and reading it back with `epoc logs [app] [-f]`. Use whenever adding, reviewing, or debugging logging, tracing, `symbian::log`, `symbian::applog`, or "why is nothing printing on the phone" — and whenever instrumenting an app or SDK crate that runs on the handset.
---

# Getting a log line off the handset

Symbian gives an application no console, no logger and no debugger. A line that is not
deliberately routed somewhere is gone.

There is **one** call, **two** switches — one at build time, one at run time — and **one** file.

```rust
symbian::log!("[net] connect state={state} err={err}");
```

```
# app.conf
DEBUG=1
```

```sh
epoc logs <app>       # print C:\Data\_logs\<app>.txt
epoc logs <app> -f    # and keep printing what the app adds, until Ctrl-C
```

The implementation is `crates/symbian/src/log.rs` (the call, the switch, the file) and
ADBian's `client/rshell.py` / `client/rsh.py` (reading it back over the phone's remote shell).

## The file is the log

The leading underscore is deliberate: `C:\Data\` holds the user's own files and the phone's file
browser sorts by name, so `_logs` sits at the top beside `_app_install` — the two places this project
ever asks somebody to open on the handset, together, above the photos.

| | |
|---|---|
| Where | `C:\Data\_logs\<app>.txt` — `symbian::DATA_LOG_DIR`, created by `symbian::ensure_log_dir` |
| Needs | nothing: no capability, no host, no network, no per-app feature |
| Survives a crash | yes — appended per line, and across launches |
| Cost | one open/append/close (a 64 KB flash write measures 23 us on the E72) |
| Read back | `epoc logs <app>`, or `epoc sh --pull`, or over USB |

There used to be a second route: a TCP bridge (`epocadb`) that streamed lines to the host over
Wi-Fi while the app ran. It is gone. It needed a Wi-Fi bearer, a per-app cargo feature, a host
IP baked in at build time and a socket opened at exactly the right moment in the bearer's
lifecycle — and in exchange it delivered what a file plus `-f` delivers. It was also never once
confirmed against a real handset. **If you find yourself wanting live output, use `epoc logs -f`.**

`-f` polls the file's size over Bluetooth about once a second and prints what is new, so it is
"live" at human speed, not at frame speed. Which is the right speed: see the budget below.

## The run-time switch

`DEBUG=` decides what is in the binary. Whether a build that carries logging is *writing* it is a
second question, and the answer is a file:

```
C:\Data\_logs\<app>.on      one byte: 1 or 0. Absent = on.
```

- `symbian::log::enabled()` — is a line written now kept? (The file is read once per process.)
- `symbian::log::set_enabled(on)` — flip it, now and for every later launch. Immediate: no restart.
- `symbian::log::set_enabled_for(name, on)` — flip *another* process's, which is how an app with a
  screen switches its headless daemons. They pick it up on their next launch.

Absent means on, so this can only ever turn off something that was already compiled in — a
`DEBUG=1` build with no flag file behaves exactly as it did before the switch existed.

**Every app with a screen should offer it.** The launcher has it in Settings > General ("Debug log"),
the calendar and Telegram in their Options menus, where the entry carries its own state
("Log: ligado" / "Log: desligado") because a toggle whose effect is invisible is a toggle nobody
trusts. For a daemon, the switch is the launcher's — or the host's:

```sh
epoc sh --push /dev/null 'C:\Data\_logs\connd.on'   # a 0-byte file reads as on
```

## The build switch, and what "off" means

`DEBUG=0` (the default in `tools/symbuild`) does not silence the log — it **removes** it.
`symbian::log::ENABLED` is a `const bool` read from `SYMBIAN_DEBUG`, which `symbuild` exports
from `DEBUG=`, so the macro expands to `if false { … }` and the optimiser deletes the call
*and its format string*. Measured on the Telegram client: 386,700 bytes with `DEBUG=1`,
376,752 with `DEBUG=0`.

So: **write the line while the code is fresh.** Leaving instrumentation in the source costs a
release build nothing. Do not delete log lines to "clean up for release" — flip the switch.

Corollaries worth knowing:

- The build banner prints `debug log: on → C:\Data\_logs\<app>.txt` or `debug log: off`. If it
  says off, no amount of `symbian::log!` will produce anything.
- It is an environment variable rather than a cargo feature on purpose: no app declares
  anything, and `tools/symnew` scaffolds `DEBUG=1`.
- `SYMBIAN_APP_NAME` (also from `symbuild`) picks the filename. Never hardcode it — and never
  hardcode the directory either: `symbian::log::data_path(name)` is the one place that knows.

## Budget — it is a file on a phone, not a terminal

- A single line's body is capped at **1023 bytes** (`MAX_LINE`), truncated at a char boundary
  with ` …` appended.
- The file **starts over** once it passes `symbian::LOG_MAX` (64 KB) — see
  `fs::append_capped`. It does not drop the oldest half: that would be a read and a rewrite on
  the GUI thread. So a log that wraps has lost everything before the wrap, and `epoc logs -f`
  prints `--- log restarted (it passed its size cap)` when it sees the file shrink.
- That cap is the reason to log transitions rather than steps. 64 KB is about 700 lines.

So:

- **Never log inside `draw`, per frame, or per key event.** It is real file I/O on the GUI
  thread, and it is also the fastest way to wrap the file before the interesting part.
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

The bracket is what makes a 64 KB file greppable without a parser, and what lets one app's log
be read by somebody who does not know its internals.

Every Symbian error is negative and the log is mostly error codes — log the code, and the name
too when you have it (`-4180` and `KErrEtelGsmBase` are the same fact, and only one is
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

On the host the file writer does nothing — there is no phone. Verify the *logic* with host
tests and the *lines* on hardware. A `cargo run -p <app> --example sim` session produces no
log output, and that is expected.

## Triage

| symptom | first thing to check |
|---|---|
| nothing anywhere | did the build banner say `debug log: on`? `DEBUG=1` in `app.conf`? |
| `epoc logs` says not found | the app may never have logged: the path is resolved lazily, on the first line. Also check the name — the file is `<app>.txt`, from `NAME` in `app.conf` |
| gaps, or a short file | it wrapped at 64 KB. Log fewer, bigger facts |
| a line ends in ` …` | over 1023 bytes |
| file is missing on the phone | check `symbian::log::path_label()` — the ladder falls back to `C:\logs\`, the drive root, then the private cage, and only the last is unreadable from outside |
| `epoc logs` cannot connect | that is ADBian, not logging: is `rshelld` running on the phone? `epoc sh ping` |

## Before calling logging work done

- [ ] Every new line has a `[tag]` prefix and is under ~100 chars.
- [ ] No line is inside `draw` or on a per-event path.
- [ ] No secret, no message body, no full phone number in any of them.
- [ ] Boot-path facts are logged too — the file has them from the first line, with no host in
      the picture.
- [ ] A new app with a settings surface offers the run-time switch, and its label says which
      way it is.
- [ ] No second logging mechanism was invented. One macro, two switches, one file.
- [ ] `cargo test -p symbian` still green if you touched the log.
