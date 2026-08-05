# symbian-sys

The raw FFI boundary. A hand-written mirror of `shim/inc/symbian_shim.h`, and nothing
else — no safe wrappers, no state, no logic. That is [`symbian`](../symbian)'s job.

## What crosses, and the three rules it obeys

The C++ side of this boundary is documented in `shim/inc/symbian_shim.h`. The rules that
shape this file:

**1. Every `extern "C"` function is a TRAP barrier.** On Symbian 9.x a Leave *is* a C++
throw, and a throw crossing a Rust frame compiled `panic=abort` — no landing pads, no
unwind tables — skips every `Drop` and is undefined behaviour, not merely a leak. So no
function here can leave: the shim TRAPs internally and returns a `TInt`. The allocator
in particular calls `User::Alloc`, never `User::AllocL`, so out-of-memory arrives as a
null pointer rather than as an exception.

**2. Rust never owns the loop.** Avkon calls `CActiveScheduler::Start()` and it does not
return until the app exits. Every async completion becomes a POD `ShimEvent` on a ring
buffer, and a `CIdle` pump calls `rust_step()`, which drains it. Same shape as a `winit`
`ApplicationHandler`.

**3. Handles are opaque `i32`, never pointers.** A handle table turns a use-after-free
into `SHIM_ERR_BAD_HANDLE` instead of a crash, and keeps C++ object lifetimes invisible
to Rust. The file handles pack a generation counter for the same reason.

Strings cross as `(*const u16, i32)` — UTF-16 code units, which `TPtrC16` wraps with no
copy. Rust keeps UTF-8 internally and converts at the boundary.

## It compiles for the host

Every extern sits behind `#[cfg(target_vendor = "symbian")]`, with a stub returning
`SHIM_ERR_NOT_READY` otherwise. That is what keeps `cargo test --workspace` working
without a cross-compiler, and it means a host build of anything above this crate links
and runs — it just cannot do I/O.

`symbian` is not a vendor rustc knows about, so the `check-cfg` declaration in
`Cargo.toml` is what stops every one of those attributes from emitting an
`unexpected_cfgs` warning and burying the real ones.

## Two details that are load-bearing

**Key ids live above `0x110000`.** `SHIM_EV_KEY_CHAR` carries a Unicode scalar in the
`a` field, and `SHIM_EV_KEY_DOWN` carries an abstract key id in the same field. Putting
the key ids above the highest possible scalar means the two can never be confused, and
there is a test asserting it.

**`ShimEvent` has a `native` field, and it exists because of a real bug.** `b` carries a
portable three-bit modifier summary (shift/ctrl/func), which is all a toolkit should care
about — and that summary is exactly what made the E72's keyboard bug hard to see. It read
`00` for every key, which only ever meant "none of those three", and said nothing about
`EModifierNumLock` (0x8000), `EModifierPureKeycode` (0x100000) or
`EModifierKeyboardExtend` (0x200000). `native` carries the whole unmasked `iModifiers`
word. Apps should keep using `b`; a diagnostic needs the truth.

There is also a test asserting `size_of::<ShimEvent>() == 32`. If the C++ struct and this
one drift, every event is misread, and the symptom would look like anything but that.

## Status

20 of the 42 declared functions are implemented on the C++ side. The header declares the
whole eventual surface deliberately: C++ needs no definition for a declaration nobody
calls, and `--no-undefined` only complains about *referenced* symbols, so the
unimplemented half costs nothing until it is wanted.

| | |
|---|---|
| alloc, panic, debug | done |
| events | done |
| framebuffer, screen | done |
| timers, clock | done |
| files | done |
| device queries (`shim_dll_present`) | done |
| TCP, DNS | declared only |
| platform text and fonts | declared only |
| UDP | not declared |
