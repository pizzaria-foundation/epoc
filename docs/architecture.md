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

## The worker thread

`rust_step` runs from a `CIdle` on the GUI thread and must return in milliseconds. A long
one starves the window server, which freezes the whole phone rather than just the
application, and nothing recovers from it.

A 2048-bit modular exponentiation measures **815 ms** on the E72, and an MTProto login needs
two. So the shim carries a worker: one thread per job, created on submit and gone when the
job ends. Completion comes back through `RThread::RequestComplete` into a `CActive` on the
GUI thread, so it lands in the scheduler like a timer and nothing polls.

Measured on the handset: the same exponentiation took **1933 ms** of wall time on the
worker, with **27 GUI ticks served** through it. Slower — one core, shared with an interface
that keeps redrawing. **The worker buys responsiveness, not speed**, and a design that
assumes background work is free is wrong on this device.

### The one way to break it

The worker gets its own heap, because a default `RHeap` is not thread-safe. So an allocation
made on the worker and freed on the GUI thread is a cross-heap free: silent corruption
rather than a clean failure.

The contract is therefore not "the job must not allocate" — it is that nothing the job
allocates may outlive it. `symbian::work::Job` enforces the shape by owning both buffers as
`Box<[u8]>` and holding them for the whole request; the job only ever writes into the
caller's output.

The crash that established this: `RHandleBase::Duplicate` takes the handle to copy from
`this->iHandle`, not from its argument. A default-constructed `RThread` therefore asked the
kernel to duplicate handle 0 — `KERN-EXEC 0`, on the GUI thread, which closes the
application. `RThread::Open(TThreadId)` has no such reading.


## The key convention

Three keys, three jobs, every screen. It is the native S60 arrangement, and that is the argument:
the phone trained its user for a decade before we arrived, and the screen that disagrees is the one
that feels broken.

```
  ┌──────────────────────────────────────────────┐
  │  Options            Open            Back     │
  └──────────────────────────────────────────────┘
     left softkey    D-pad centre   right softkey
     secondary       THE ACTION     way out
```

**The middle slot is not a softkey.** S60 wires it to the selection key, so it arrives as
`Key::Select` and `Softkey::Middle` never does. A screen labels the middle slot and handles
`Select`. This is not a hypothetical: the launcher's task manager shipped with its refresh bound to
`Softkey::Middle`, the arm never fired, and pressing the key opened the highlighted app instead —
the label promised one thing and the key did another.

**The left softkey is options** — refresh, a mode switch, a menu. Blank when a screen has nothing
secondary to offer, which is most of them.

**The right softkey is back**, and only ever back or exit. It is the key a user presses without
reading; making it a second action key is how an app becomes frightening.

`chrome::Softkeys` builds the bar by name (`new(options, action, back)`, `action(a, back)`,
`back(b)`) rather than as a bare array, because `[a, b, c]` reads the same whichever order the
author meant and nothing checks it. The three constructors are pinned by a test, and the crate
documentation in `symbian-ui` says the same thing where a reader of the API will find it.

Two screens in this repo had the action on the left softkey while handling `Select` — the label was
pointing at a key that did nothing. Both were corrected when the convention was written down, which
is the usual way a convention pays for itself: it turns "that screen is a bit odd" into a defect
with a name.

## Copy and paste

`Ctrl+C`, `Ctrl+X`, `Ctrl+V`, `Ctrl+A` and `Shift`+arrow work in every text field this SDK draws,
and an application writes no code for any of them. The phone's own editors have had these bindings
since 2009; a field of ours without them is the one that feels broken.

```text
  handset          symbian-app              symbian-ui            the app
  ⌃+C  ──────▶  Key::Ctrl('c')  ──────▶  TextField::handle_key ──▶  (only if the
  0x03 + Ctrl     the chord, resolved       selection, copy,          field ignored it)
                  before any key map        cut, paste
```

**A chord is not text, and the type says so.** `Ctrl+C` arrives from the window server as the
control character `0x03` — and `Ctrl+M` as `0x0D`, which is also Enter, and `Ctrl+H` as `0x08`,
which is also Backspace. `symbian-app` resolves the chord *before* the key map and the keyboard
layout, and reports `Key::Ctrl('c')` rather than `Key::Char('c')` with a modifier. The reason is the
same one behind the middle-softkey rule above: every consumer of `Key::Char` — a text field, a
list's type-to-filter, a digits-only login field — would otherwise have to remember to check
`mods.ctrl`, and the one that forgot would type `v` when the user asked to paste. An old `match` arm
simply stops matching instead, which the compiler notices for us.

**The clipboard is an argument, not a global.** `symbian-ui` is `#![forbid(unsafe_code)]` and knows
nothing about the device, so it cannot hold a mutable static and could not call the platform if it
did. `TextField::handle_key` therefore takes a `&mut dyn Clipboard`; `symbian-app::SystemClipboard`
is the device one, `NoClipboard` is for a build without it, and `MemClipboard` makes copy and paste
an ordinary host unit test. Handing one over is the whole of an application's part in it.

**A filter belongs to the field, not to the screen in front of it.** A digits-only field used to be
enforced by the caller, which inspected `Key::Char` before passing the key on. That held only while
typing was the only way text got in: pasted text is not keystrokes and walked straight past the
check. `TextField::accepting` moved the rule inside, where every route in has to pass it.

**A masked field never copies.** `Ctrl+C` in a password field is refused, because the phone's
clipboard is readable by every application on it and a password left there outlives the keypress by
a long way. Pasting *into* one is fine, and is how a password manager is used.

**It is a default, and it gets out of the way.** A chord the field could not honour — an empty
clipboard, a refused copy, a masked field — answers `Ignored` rather than `Consumed`, so a screen
can put its own behaviour *underneath* the default instead of having to pre-empt it:

```rust,ignore
// The field copies its selection; with nothing to copy, this screen copies what the cursor is on.
self.composer.handle_key(ev, clip).or_else(|| self.copy_highlighted(clip))
```

That is the general shape of overriding anything here, and there are three rungs of it: hand over a
different `Clipboard` (per app, or per screen — it is an argument, not a global); take the chord
before the field sees it; or skip `handle_key` entirely and call `paste`/`copy`/`cut`/`select_all`,
which are public precisely so an app can keep the caret arithmetic and replace the bindings.
`symbian-ui`'s `clip` module documents all three with an example of each. The editing keys are
deliberately the other way round — `Backspace` in an empty field still consumes, because a field
being typed into owns them outright.
