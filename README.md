# A Rust SDK for Symbian S60 3rd Edition

Write applications in Rust for a 2009 Nokia. They run on the phone.

Target: **Nokia E72** — Symbian OS 9.3, S60 3rd Ed FP2, ARM1136JF-S at 600 MHz,
320×240 landscape QVGA, hardware QWERTY, no touchscreen, ~45 MB free RAM.

Host: **aarch64 Linux**. No Wine, no x86 emulation, no Windows, anywhere in the chain.

```
tools/symnew myapp                     # scaffold a project that builds and runs
cargo run -p myapp --example sim       # see it on your desktop, right now
tools/symbuild apps/myapp              # → apps/myapp/build/myapp.sis
```

That third command produces an installable package. The first two need no phone.

## Status

Confirmed on hardware: a Rust chat UI runs on the E72, draws through its own rasterizer,
and takes input from the full QWERTY.

| | |
|---|---|
| Toolchain, packaging, install | ✅ GCC 15.2 + binutils 2.45 for `arm-none-symbianelf` |
| Drawing, text, layout | ✅ `symbian-gfx`, `symbian-ui` — 320×240, RGB565, own font atlases |
| Input | ✅ QWERTY including the overlaid keypad and the Fn layer, D-pad, softkeys |
| Timers, clock | ✅ |
| Files, atomic save | ✅ `symbian::fs` — the app's data cage, no capability needed |
| Host simulator | ✅ `symbian-sim` — the real app in a window |
| Project scaffolding | ✅ `tools/symnew` |
| TCP, DNS | ⬜ declared in the ABI, not implemented |
| UDP, HTTP | ⬜ not started |
| Crypto | ⬜ not started. See [device notes](docs/device-notes.md) on what the platform does and does not give you |
| Image decode | ⬜ no PNG or JPEG |

164 tests, all on the host.

## What you can build today

Anything offline and keyboard-driven that draws its own interface, and now — with files
working — anything that needs to remember something between launches: notes, settings,
converters, games, reference tools, viewers.

Not yet: anything that talks to a network.

## The map

```
crates/
  symbian-gfx     the rasterizer. no_std, no unsafe, no allocation while drawing
  symbian-ui      widgets and the design system: surfaces, icons, five palettes
  symbian-sys     the raw FFI boundary, mirroring shim/inc/symbian_shim.h
  symbian         safe wrappers — files today, sockets when they exist
  symbian-app     the device entry points, as one macro
  symbian-sim     the host simulator, generic over any App

shim/             the C++ side: everything that can Leave, and the event pump
tools/            symnew, symbuild, mkfont, e32dump, e32prep, btpush, serve
apps/telegram/    the reference app
examples/         hello-gui (C++), keyprobe and libprobe (device diagnostics)
docs/             start with getting-started.md
```

Each crate has its own README with the decisions behind it.

## Where to read next

| | |
|---|---|
| [docs/getting-started.md](docs/getting-started.md) | Prerequisites, the toolchain, your first app |
| [docs/architecture.md](docs/architecture.md) | Why there is a C++ shim, and what crosses the boundary |
| [docs/device-notes.md](docs/device-notes.md) | Everything the hardware taught us that no document says |
| [docs/build-flow.md](docs/build-flow.md) | The pipeline, stage by stage |

## The one-paragraph version of how it works

Avkon — Symbian's UI framework — calls `CActiveScheduler::Start()` and does not return
until the app exits. There is no loop for Rust to own and no way to take one. So control
is inverted: a small C++ shim owns the application object, turns every key press and I/O
completion into a plain-data event on a ring buffer, and a `CIdle` at idle priority calls
`rust_step()`, which drains the queue, updates, and draws. That is the same shape as a
`winit` handler, and it is why an app is a struct with `handle_key` and `draw` rather than
a `main`.

Everything that can *Leave* — Symbian's error mechanism, which on 9.x is a C++ throw —
stays on the C++ side of that boundary, because a throw crossing a Rust frame compiled
`panic=abort` skips every destructor.

## Licence

The SDK is MIT OR Apache-2.0. It is not distributed with Nokia's S60 SDK, the built
toolchain, or third-party research material — see `.gitignore` for what is excluded and
why, and [getting-started](docs/getting-started.md) for how to obtain the parts you need.
