# Architecture

## The problem

Symbian's UI framework does not offer a main loop; it *is* the main loop.
`EikStart::RunApplication` ends in `CActiveScheduler::Start()` and does not return until
the application exits. There is no point at which Rust could take control, and no
supported way to run the scheduler yourself.

On top of that, Symbian's error mechanism is *Leave*, which on 9.x is a real C++ throw.
A throw crossing a Rust frame compiled `panic=abort` — no landing pads, no unwind tables —
skips every destructor. That is undefined behaviour, not merely a leak.

So the boundary is not a matter of taste. Its position is forced: **anything that can
Leave stays in C++, and Rust is always a callee.**

## The shape

```
        ┌──────────────────────────────────────────────┐
        │  Avkon  ──  CActiveScheduler::Start()        │  owns the loop
        └───────────────────────┬──────────────────────┘
                                │
        ┌───────────────────────▼──────────────────────┐
        │  shim/  (C++)                                │
        │                                              │
        │   CShimApplication → CShimDocument           │
        │      → CShimAppUi → CShimControl             │
        │                                              │
        │   OfferKeyEventL ─┐                          │
        │   RunL (timers)  ─┼─→ ring buffer of PODs    │
        │   RunL (sockets) ─┘                          │
        │                                              │
        │   CIdle (idle priority) ──→ rust_step()      │
        └───────────────────────┬──────────────────────┘
                                │  extern "C", each a TRAP barrier
        ┌───────────────────────▼──────────────────────┐
        │  symbian-sys      raw ABI, i32 error codes   │
        ├──────────────────────────────────────────────┤
        │  symbian-app      allocator, panic, entry!   │
        │  symbian          safe wrappers: fs, later net│
        ├──────────────────────────────────────────────┤
        │  symbian-ui       App, widgets, design system │
        │  symbian-gfx      rasterizer                  │
        ├──────────────────────────────────────────────┤
        │  your app         handle_key + draw           │
        └──────────────────────────────────────────────┘
```

Every async completion — a key, a timer, later a socket — becomes a plain-data `ShimEvent`
on a fixed ring buffer. A `CIdle` running at idle priority calls `rust_step()`, which
drains the queue, updates state, and draws if anything changed.

That is the same shape as a `winit` `ApplicationHandler`, which is why `App` has
`handle_key` and `draw` rather than a `main`. The platform forced it; it turns out to be
the right shape for a keypad device anyway.

### `rust_step` must return promptly

It runs on the GUI thread. A long one starves the window server, which freezes the *whole
phone* — not just this app. There is no watchdog that will save you.

### The ring buffer drops the newest, not the oldest

Input arrives in order, so losing the tail of a burst is more repeatable than losing its
middle. `shim_events_dropped()` reports the count; a non-zero value means `rust_step` is
not keeping up, and it is the number that would decide whether dirty-rect presenting is
worth building.

## The three rules of the boundary

**1. Every `extern "C"` function is a TRAP barrier.** The leaving work lives in a private
`DoThingL()`; the exported function TRAPs it and returns a `TInt`. The allocator is the
sharpest case: it calls `User::Alloc`, never `User::AllocL`, so out of memory arrives as a
null pointer rather than as an exception unwinding through Rust.

**2. Rust never owns the loop, and never exits the app.** `should_exit()` is a flag the
host acts on. An app that called the framework's exit itself would be tearing down the
scheduler from inside a callback the scheduler made.

**3. Handles are opaque `i32`, never pointers.** A handle table turns a use-after-free
into `SHIM_ERR_BAD_HANDLE` instead of a crash, and keeps C++ object lifetimes invisible to
Rust. File handles pack a generation counter, so a handle that outlives its file cannot
address whatever reopened the slot.

Strings cross as `(*const u16, i32)` — UTF-16 code units, which `TPtrC16` wraps with no
copy. Rust holds UTF-8 internally and converts at the boundary.

## Why the shim is C++ at all

It could have been Rust with `extern "C"` calls into Symbian's exported symbols. Two
things rule that out.

Symbian's API is C++ classes with virtual dispatch — `CCoeControl::OfferKeyEventL` is a
vtable slot the framework calls, so implementing a control means having a real C++ vtable
with the right layout. And a `TRAP` is a `catch`, which needs the C++ personality routine
and the exception tables that only a C++ compiler emits.

So the shim is the smallest possible amount of C++: about 1,500 lines, of which the
application chain is lifted almost verbatim from `examples/hello-gui`, which was proven on
the device first.

## Where drawing happens

`symbian-gfx` draws into a 16-bit-per-pixel buffer the shim owns. At present time the shim
expands it to whatever the screen wants and `BitBlt`s it in one call.

The E72's screen is 32bpp, and the canvas is still RGB565 — deliberately. A UI overdraws:
a row, then a highlight over it, then text over that. Drawing at 16bpp halves the memory
traffic of every one of those passes, and on a 600 MHz ARM1136 memory traffic is the
budget. One conversion of 76,800 pixels at the end costs less than doubling the cost of
everything before it.

The shim asks the window server for the mode rather than assuming it, and sets
`SetRequiredDisplayMode` to match, so the window server never silently converts a blit.

## What is deliberately not here

**No widget tree, no `Box<dyn View>`, no retained layout.** A tree buys composition and
costs allocation, trait-object indirection and a focus-traversal system. On a 320×240
screen showing five rows with one D-pad, composition is not the problem — arithmetic is.
The toolkit provides the arithmetic that is genuinely hard to get right (scrolling,
selection clamping, char-boundary-safe editing, scrollbar geometry) and unit-tests it,
because those are the bugs that actually happen.

**No async runtime.** There are no threads to run one on: `RWsSession` is not thread-safe
and all drawing must happen on the GUI thread. Completion events on a queue are what the
platform offers, so that is what the SDK exposes.

**No `std`.** Not as a purity exercise — there is no port. `core` plus `alloc` over
`User::Alloc` is the whole runtime, which also means no `f32::sqrt`, no `format!` in hot
paths, and integer arithmetic everywhere it would otherwise be tempting not to.

## The one macro

`symbian_app::entry!(MyApp::new())` expands to the allocator, the panic handler, the three
`extern "C"` entry points, the event translation and the theme.

It has to be a macro rather than a function because `#[global_allocator]` and
`#[panic_handler]` are lang items, and a lang item can be defined exactly once in a linked
program — a library cannot provide them. A macro expands in the caller's crate, so the
items land in the final staticlib while the code behind them lives in one place.

Before it existed, each app carried its own copy: about 120 lines of `unsafe`, duplicated
across three apps. Three copies is where copies start drifting.
