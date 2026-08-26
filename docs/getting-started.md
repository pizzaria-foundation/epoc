# Getting started

Two paths. If you only want to write an app, skip to [Your first app](#your-first-app) —
the simulator needs nothing but Rust. Building for the phone needs the toolchain, which
takes an hour of compiling once and then never again.

## Your first app

```
epoc new myapp
cargo run -p myapp --example sim
```

A 320×240 window opens at 3×, running the app. Up and down change a number.

```
arrows        D-pad
Enter/Space   select
F1 F2 F3      left / middle / right softkey
Esc           right softkey (Back)
Tab           next theme — cycles all five palettes against the real UI
letters       typed into the app
Ctrl+S        write sim-frame.png
Ctrl+Q        quit
```

Then open `apps/myapp/src/lib.rs`. It is one struct and two methods:

```rust
impl App for MyApp {
    fn handle_key(&mut self, ev: KeyEvent, theme: &Theme<'_>, screen: Rect) -> Handled { ... }
    fn draw(&mut self, c: &mut Canvas<'_>, theme: &Theme<'_>) { ... }
}
```

There is no widget tree and no `main`. A screen owns its state, gets handed a key, and
draws — see [architecture](architecture.md) for why the platform forces that shape and why
it turns out to be the right one anyway.

`cargo test -p myapp` runs the five tests the generator wrote. They are real ones:
`symbian_ui::testing` gives you a `Theme` and a `Canvas` in one call, so testing
`handle_key` or `draw` is three lines rather than twenty of packing a font atlas by hand.

### What `symnew` generated, and why it is not one file

```
apps/myapp/
  Cargo.toml            the app: no_std-able, host-testable, no runtime items
  src/lib.rs            ← you edit this
  examples/sim.rs        four lines: symbian_sim::run(MyApp::new())
  app.conf              the build manifest. UID, capabilities, heap, stack
  data/myapp.rss        the caption the menu shows
  data/myapp_reg.rss    the registration that makes the app appear at all
  device/Cargo.toml     its own workspace root — see below
  device/src/lib.rs     one line: symbian_app::entry!(MyApp::new())
```

The split into two crates is not ceremony. `device` carries `#[global_allocator]` and
`#[panic_handler]`, and a crate that defines those **cannot be built for the host at all**
— `cargo test` links the harness against `std`, which already defines both. Keeping them
in a separate crate, excluded from the workspace, is what lets `cargo test` run your app
logic on your laptop.

The UID in `app.conf` becomes three things that must agree: the E32 header, the
registration resource, and `KUidShimApp` in the shim. `symbuild` wires all three from that
one value. Getting them out of step gives you the least debuggable failure this platform
has — the icon appears and tapping it does nothing, with no error and no log — which is
why there is a generator rather than a template to copy.

## Building for the phone

### Prerequisites

```
# Fedora
sudo dnf install gcc gcc-c++ make texinfo bison flex gmp-devel mpfr-devel \
                 libmpc-devel openssl-devel python3-pillow python3-fonttools \
                 zlib-devel
```

Plus a nightly Rust, because the target is a JSON spec and `core` has to be built for it:

```
rustup toolchain install nightly
rustup component add rust-src --toolchain nightly
```

### The Nokia SDK

Not redistributable, so it is not in this repository. You need the **S60 3rd Edition FP2
SDK** installer from Nokia and [GnuPoc](https://github.com/mstorsjo/gnupoc-package) to
extract it on Linux:

```
git clone https://github.com/mstorsjo/gnupoc-package vendor/gnupoc-git
# follow its README to unpack the SDK into ./sdk
```

`symbuild` finds it through `EPOCROOT` in `app.conf`, which defaults to `./sdk`.

One patch is needed and is kept here: `toolchain/patches/elf2e32-arm-relative.patch`.
Without it, elf2e32 segfaults on any `R_ARM_RELATIVE` relocation with symbol index 0,
which every Rust binary has. The host tools also need three small fixes for modern
compilers — an ordered pointer comparison in bmconv, and the OpenSSL 1.0 API in
makesis/signsis — applied by the same script.

### The cross compiler

```
toolchain/build-cross-gcc.sh          # ~1 hour, ~1.6 GB
```

Builds binutils 2.45 and GCC 15.2 for `arm-none-symbianelf`. Both retired the target, so
the script re-adds it: the triple is gone from `bfd/config.bfd`, `gas/configure.tgt` and
`ld/configure.tgt`, and `config.gcc` gives it no `stdint.h` at all. Every patch is
idempotent, so re-running is safe.

Modern binutils rather than the CodeSourcery 2005 toolchain GnuPoc documents, and that is
load-bearing: 2005-era binutils emits EABI v4, and would refuse to merge with the EABI v5
objects LLVM produces. Byte-identical `e_flags` between the two is what makes Rust and C++
linkable at all.

### Build and install

```
epoc build apps/myapp
```

Every stage is described in [build-flow](build-flow.md). Two of them are gates rather than
steps: `e32prep.py` rewrites the ELF section table before `elf2e32` sees it, and
`e32dump.py --quiet` refuses to package a malformed E32. That second one matters because
the device rejects a bad image by *doing nothing at all* — no error, no panic, no log — so
every check that can run on the host saves an install round trip.

Getting it onto the phone:

```
tools/epoc sideload apps/myapp/build/myapp.sis               # the phone's remote shell
python3 tools/serve.py out 8000                              # or over the LAN
```

`sideload` puts the package in `C:\Data\_app_install\` over ADBian's RFCOMM shell, where
File mgr. can tap it — which is also why it replaced the Bluetooth OBEX push: OBEX buries
the file in Messaging, and the E72 drops its ACL link after each transfer, so every second
push failed with `Unable to find service record` as though the phone had no OBEX at all.

`serve.py` is the route for a phone with no remote shell on it yet — a first install, or a
reflashed handset. It sets the `application/vnd.symbian.install` MIME type, without which
the browser saves the file as an unknown blob instead of handing it to the installer.

Packages are **unsigned** by default. The development handset runs a patched installserver
(Open4All / RomPatcher+), which drops both the signature requirement and the capability
ceiling. Set `SIGN=1` in `app.conf` for a package aimed at a stock phone; `symbuild` will
mint a self-signed certificate on first use.

## Fonts

The three atlases the device links are committed, so nothing needs generating to build.
To change sizes or coverage:

```
tools/mkfonts.sh
```

Coverage is read from the font's cmap, not by asking the rasterizer whether it drew
something — for a missing codepoint most fonts return the `.notdef` box, which has ink and
sails into the atlas as a tofu glyph. It did, once, and the delivery ticks rendered as ▯▯.

## When something does not work

The device gives you nothing: no debugger, no console, no log. So the tools are built
around getting answers *before* installing, and around asking the device rather than
guessing:

| | |
|---|---|
| `tools/e32dump.py <exe>` | decodes and validates an E32 header, re-deriving the checksums independently of elf2e32 |
| `cargo run -p preview` | renders the SDK's sheets to `preview-out/*.png` — the rasterizer, the icons at 6×, the surfaces |
| `examples/libprobe` | which optional DLLs this handset has — asks with `RLibrary::Load` rather than finding out by failing to launch |
| `examples/keyprobe` | the raw key numbers the window server delivers |

Those last two exist because guessing was wrong every time it was tried. There is a list
of what the hardware actually turned out to do in [device notes](device-notes.md), and it
is worth reading before debugging anything.

If what you are building runs during a boot — a home screen, a daemon, anything started early —
read [boot states](boot-states.md) first. The platform publishes exactly where it is in its own
start-up, the enum is not in this SDK, and every timeout anyone reached for instead of reading it
turned out to be wrong on some real boot.
