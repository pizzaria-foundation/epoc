# epoc - a Rust SDK for Symbian S60 3rd Edition

Write applications in Rust for a 2009 Nokia. They run on the phone.

EPOC is what this operating system was called before it was called Symbian.

    Target   Nokia E72 - Symbian OS 9.3, S60 3rd Ed FP2, ARM1136JF-S at 600 MHz,
             320x240 landscape QVGA, hardware QWERTY, no touchscreen, ~45 MB free RAM
    Host     aarch64 Linux. No Wine, no x86 emulation, no Windows, anywhere in the chain


## Quick start

    tools/epoc new myapp                   scaffold a project that builds and runs
    cargo run -p myapp --example sim       see it on your desktop, right now
    tools/epoc build apps/myapp            -> apps/myapp/build/myapp.sis

The third command produces an installable package. The first two need no phone.

One front door for everything:

    epoc new <name> [uid3]        scaffold
    epoc build <app-dir> [clean]  sources -> .sis
    epoc db <args...>             the dev bridge: serve, logcat, push, pull, install
    epoc preview                  render the SDK's contact sheets to preview-out/
    epoc push <file>              send a file to the phone over Bluetooth
    epoc serve                    serve out/ over the LAN so the phone can fetch a .sis

`epoc db` fronts `tools/epocadb`; `new` and `build` front `tools/symnew` and
`tools/symbuild`. All three still work when called directly.


## Status

Confirmed on hardware: a Rust chat client runs on the E72, draws through its own
rasterizer, takes input from the full QWERTY, and talks to Telegram over TCP.

    Toolchain, packaging, install   done    GCC 15.2 + binutils 2.45, arm-none-symbianelf
    Drawing, text, layout           done    symbian-gfx, symbian-ui: RGB565, own atlases
    Input                           done    QWERTY, overlaid keypad, Fn layer, D-pad, softkeys
    Keyboard layouts, accents       done    symbian-keys: ABNT2 from the handset's own keymap
    Timers, clock                   done
    Files, atomic save              done    symbian::fs, in the app's data cage, no capability
    TCP, DNS, bearer selection      done    symbian::net, over ESock
    UDP                             done    used by the dev bridge's discovery beacon
    Crypto                          done    symbian-crypto: SHA-1/256/512, HMAC, AES, IGE,
                                            bignum, PBKDF2, DRBG - none of it from the platform
    Image decode                    done    symbian::image, through the handset's own codecs:
                                            JPEG, PNG, GIF, BMP. Not WebP, which postdates it
    Audio decode                    done    symbian-audio: Ogg/Opus in, playable WAV out
    Device logging                  done    symbian::log!, switched by DEBUG= in app.conf
    Dev bridge                      done    epocadb: live logs, file push/pull, over Wi-Fi
    Host simulator                  done    symbian-sim: the real app in a window
    Contact sheets                  done    symbian-preview: any screen to a PNG
    Project scaffolding             done    epoc new
    HTTP                            todo    nothing yet; TCP is there to build it on
    TLS                             todo    see docs/ on why this matters for what you can port

463 tests, all on the host.


## Projects

Applications built on this SDK. Each lives in its own repository and depends on
this one by revision.

    tg      github.com/Lab2021/tg      Telegram client. MTProto 2.0 written from
                                       scratch, the login exchange, chat list,
                                       conversations, photo and voice messages.
                                       The reference application, and the reason
                                       this SDK exists.

Built something? Open a pull request adding it here. What the list is for is
proving the SDK works for more than one program.


## Using it from your own project

The SDK is consumed as a git dependency, pinned by revision:

    [dependencies]
    symbian = { git = "ssh://git@github.com/Lab2021/epoc", rev = "..." }
    symbian-ui = { git = "ssh://git@github.com/Lab2021/epoc", rev = "..." }

    [dev-dependencies]
    symbian-sim = { git = "ssh://git@github.com/Lab2021/epoc", rev = "..." }

SSH rather than HTTPS while this repository is private: an https git dependency to
a private repo fails with "revision not found", which is a permission problem
wearing the wrong hat. Cargo's built-in git client may also fail ssh-agent
authentication where the `git` CLI succeeds, so a consuming project wants:

    # .cargo/config.toml
    [net]
    git-fetch-with-cli = true

The **device build needs a checkout**, not just the crates: the toolchain, the C++
shim and the packaging live here and no crate can carry them. Clone this repository
and run `tools/epoc build <your-app-dir>` from it. See `tg` for a working example of
both halves, including the `[patch]` block for working on an app and the SDK at once.


## The map

    crates/
      symbian-gfx      the rasterizer. no_std, no unsafe, no allocation while drawing
      symbian-ui       widgets and the design system: surfaces, icons, palettes, the viewer
      symbian-keys     physical keyboard layouts and dead-key composition
      symbian-sys      the raw FFI boundary, mirroring shim/inc/symbian_shim.h
      symbian          safe wrappers: files, sockets, images, the disk cache, the log
      symbian-app      the device entry points as one macro, and the dev bridge
      symbian-audio    Ogg/Opus in, playable RIFF/WAVE out - codecs the handset lacks
      symbian-crypto   hashes and ciphers the platform does not ship
      symbian-preview  host-side contact sheets: any screen to a PNG
      symbian-sim      the host simulator, generic over any App
      epocadb          the device side of the dev bridge
      opus             the vendored libopus, and the only unsafe in the audio path

    shim/              the C++ side: everything that can Leave, and the event pump
    tools/             epoc (new, build, db, preview), mkfont, e32dump, e32prep, btpush
    examples/          hello-gui (C++), keyprobe and libprobe (device diagnostics)
    docs/              start with getting-started.md

Each crate has its own README with the decisions behind it.


## Where to read next

    docs/getting-started.md   prerequisites, the toolchain, your first app
    docs/architecture.md      why there is a C++ shim, and what crosses the boundary
    docs/device-notes.md      everything the hardware taught us that no document says
    docs/build-flow.md        the pipeline, stage by stage
    docs/epocadb.md           the dev bridge: live logs, file push/pull, the wire protocol


## How it works, in one paragraph

Avkon - Symbian's UI framework - calls `CActiveScheduler::Start()` and does not
return until the app exits. There is no loop for Rust to own and no way to take
one. So control is inverted: a small C++ shim owns the application object, turns
every key press and I/O completion into a plain-data event on a ring buffer, and a
`CIdle` at idle priority calls `rust_step()`, which drains the queue, updates, and
draws. That is the same shape as a `winit` handler, and it is why an app is a struct
with `handle_key` and `draw` rather than a `main`.

Everything that can *Leave* - Symbian's error mechanism, which on 9.x is a C++
throw - stays on the C++ side of that boundary, because a throw crossing a Rust
frame compiled `panic=abort` skips every destructor.


## The name is not ours

EPOC and Symbian are somebody else's names, and both appear all over this repository.

EPOC was Psion's, and became Symbian OS. Symbian was Symbian Ltd's, then Nokia's, and the
trademark sits with Accenture today. Neither name belongs to this project and neither is
claimed by it.

That covers the project name `epoc`, and it covers **the `symbian-` prefix on every crate
here**: `symbian`, `symbian-gfx`, `symbian-ui`, `symbian-sys`, `symbian-app`,
`symbian-keys`, `symbian-audio`, `symbian-crypto`, `symbian-preview`, `symbian-sim`. Those
names are descriptive - each one says which platform the crate targets and nothing more.
None of them is published to crates.io, none implies endorsement by or affiliation with any
rights holder, and if one of them would rather we did not use the word, we will rename.
Renaming is a `sed` and a revision bump; the code does not care what it is called.

Nokia's S60 SDK, the platform headers and the import libraries are not redistributed
here; `docs/getting-started.md` says how to obtain them. Same for the built toolchain and
the third-party research material - `.gitignore` lists what is excluded and why.


## How this was written

With AI assistance, throughout. Claude wrote and reviewed a large share of this code and
most of these comments, and the commit trailers say so rather than hiding it.

It was still made with care, and the way to check that is not to take our word for it:

- Every claim about the hardware was **measured, not reasoned about**.
  `docs/device-notes.md` is a log of assumptions that turned out wrong, and the probes in
  `examples/` exist because guessing cost us device round trips. The keyboard tables are
  generated from a dump of the handset's own keymap for exactly this reason.
- The comments say **why**, not what. Where a decision looks strange, the comment names
  the failure that produced it - a truncating append, a socket that panics esock, a
  contact sheet that lied about its own pixels.
- **463 tests, all on the host**, because the interesting bugs are in loops and edge cases
  and a phone is a terrible place to find them.
- Nothing here is claimed to work that has not run on a real E72.

Neither the assistance nor the care is a substitute for review. Read the code.


## Licence

MIT. See `LICENSE`.
