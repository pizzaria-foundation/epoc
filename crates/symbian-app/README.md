# symbian-app

The device side of an application, as one macro.

```rust
#![no_std]
#![no_main]
extern crate alloc;

symbian_app::entry!(MyApp::new());
```

That is a complete device crate. The macro supplies the allocator, the panic handler, the
three `extern "C"` functions the shim calls, the event translation and the theme.

## Why a macro and not a function

`#[global_allocator]` and `#[panic_handler]` are lang items, and a lang item can be defined
exactly once in a linked program. A library cannot provide them — the moment two crates in
a dependency graph both did, nothing would link.

A macro expands in the caller's crate, so the items land in the final staticlib while the
code behind them lives here. That is the whole reason for the shape.

Before this existed, each app carried its own copy: about 120 lines of `unsafe` across the
allocator, the panic handler, the key translation and the framebuffer setup, duplicated
three times. The reference app's device crate went from 277 lines to 16.

## What the macro does per step

```
drain the event queue
  ├─ RESIZE / REDRAW      → mark dirty
  ├─ QUIT                 → ask the framework to exit
  └─ anything else        → App::handle_raw first, then App::handle_key
if App::should_exit()     → ask the framework to exit
if not dirty              → return without drawing
lock the framebuffer, App::draw, unlock, present
```

Three details in there are deliberate:

**The whole queue is drained before drawing.** Coalescing several key presses into one
repaint is the difference between keeping up and falling behind when someone holds a key
down.

**`handle_raw` is offered first**, and an app that consumes an event does not also get a
translated key. That is what lets a diagnostic see the numbers the platform sent rather
than our reading of them — and it is how `examples/keyprobe` found that the E72's twelve
keypad-overlay keys arrive as digit scan codes.

**The theme is built once per step, not once per key.** It has to be inside a closure
because `Theme` borrows its font atlases and cannot escape the scope that owns them.

## The allocator

Over `shim_alloc`, which is `User::Alloc` — never `AllocL`. `AllocL` *leaves*, and on
Symbian 9.x a leave is a C++ throw; a throw crossing a Rust frame compiled `panic=abort`
skips every `Drop`. So out of memory must arrive as a null pointer.

`RHeap` guarantees 8-byte alignment. Anything stricter takes an over-allocate path that
records the shift in the word below the returned pointer. The documented guarantee is
probably enough, but it is not something to bet a heap on, and the path costs nothing for
the 99% of allocations that never take it.

`realloc` is forwarded rather than alloc-copy-freed, so `RHeap` can grow a cell in place.

## The app is boxed

`entry!` takes one expression, not a type and an expression, and that is why: a `static`
needs a concrete type written out, and `Option<impl App>` is not one. So the app is stored
as `Option<Box<dyn App>>`.

The cost is a vtable call on `handle_key`, `draw` and `should_exit` — three per frame,
against ~76,800 pixel writes in the same frame. It is not measurable.

## Fonts

The three atlases every app links (`ui11`, `ui11b`, `ui9`, about 58 KB) are held here
rather than in each app, so a new project gets working text without deciding anything and
all apps agree on what "body" and "small" mean.

They are embedded rather than taken from Symbian's `CFont` because the Rust rasterizer is
already tested and the atlases guarantee coverage regardless of which fonts a given handset
shipped with. `symbian_gfx::Font` remains the seam if the system font is ever wanted
instead.

## Tests

Nine, all host-side, all about the translation layer — including that an unknown key id
survives as `Key::Raw` rather than being dropped, and that a lone UTF-16 surrogate is
rejected rather than becoming U+FFFD. Both are cases where the wrong behaviour is a visible
artifact in someone's text with no trail back to here.

---

Part of [epoc](../../README.md), a Rust SDK for Symbian S60 3rd Edition. MIT licensed; see
`LICENSE` at the repository root. `symbian` in this crate's name is descriptive, not a claim
on somebody else's trademark - the repository README says more. Written with AI assistance,
and every hardware claim in it was measured rather than reasoned about.
