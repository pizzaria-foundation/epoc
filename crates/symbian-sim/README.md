# symbian-sim

The app, in a window, on your desktop.

```rust
// examples/sim.rs
fn main() {
    symbian_sim::run(MyApp::new()).unwrap();
}
```

```
cargo run --example sim
```

Declare it under `[dev-dependencies]`. It pulls in a windowing library, which has no
business anywhere near a `no_std` staticlib — and the device build never sees dev
dependencies at all.

## Why it exists

A device round trip is: build, package, push over Bluetooth, accept a prompt on the phone,
open Messaging, install, launch. Two minutes when it works, and it fails outright whenever
the phone's Bluetooth has gone to sleep — which it does, after every transfer.

That is a fine loop for *confirming* something and a terrible one for designing.

## What is genuinely the same

The same `App`, the same `symbian_ui` widgets, the same `symbian_gfx` rasterizer, the same
320×240 RGB565 canvas. The RGB565 → XRGB8888 expansion goes through the **same function**
the shim calls on the device, so a colour that comes out wrong here comes out wrong there.

That last point is why the conversion is not reimplemented locally: it replicates each
channel's high bits into the low ones, so white is `0xFFFFFF` and not `0xF8F8F8`. A
hand-rolled shift would make the simulator subtly brighter than the phone, which is exactly
the kind of difference that wastes an afternoon.

Scaling is nearest-neighbour, never interpolated. A smoothed 3× view would hide the
single-pixel errors this tool exists to surface.

## What it does not reproduce

**Timing.** On the device `rust_step` runs from a `CIdle` at idle priority on a 600 MHz
ARM1136 with soft float; here it runs at 60 fps on a desktop. This tool will never tell you
that a repaint is too slow. The moment a question is about speed it has to go back to the
phone.

**The full keymap.** minifb reports physical keys, so the shift layer is applied by a small
table covering letters, digits, space and a little punctuation. On the device the window
server has a real keymap — so an input bug that depends on an unusual character has to be
reproduced on hardware.

**Anything the shim does.** Files, timers and sockets are `symbian-sys` externs, which on
the host are stubs returning `SHIM_ERR_NOT_READY`. An app that reads a file will see it as
missing. For testing that logic, `symbian::fs` is written against a trait with an in-memory
implementation — that is the layer to test against, not this one.

## Keys

```
arrows        D-pad
Enter/Space   select
F1 F2 F3      left / middle / right softkey
Esc           right softkey (Back)
Tab           next theme
letters       typed into the app
Ctrl+S        write sim-frame.png at 1:1
Ctrl+Q        quit
```

`Tab` is the one worth knowing about: it cycles all five palettes against the real UI
rather than against a swatch sheet, which is the only way to tell whether a theme actually
works.

## Finding the font atlases

Searched in a few relative locations, because the working directory depends on how cargo
was invoked — the SDK root for a workspace member, the app's own directory for a standalone
project. `SYMBIAN_ASSETS` overrides. Guessing one path and failing with "no such file"
would send people looking for a missing asset rather than a wrong `cwd`.
