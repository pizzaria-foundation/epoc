# symbian-keys

Physical-keyboard layouts and dead-key composition. `no_std`, no dependencies, 33 tests.

## What it fixes

The target handset is a **Brazilian E72**, whose keyboard is **ABNT2**, and until this
crate existed the SDK treated it as a US QWERTY. Three user-visible consequences:

1. **No accents at all.** ABNT2 accent keys are dead keys, and dead-key composition is the
   FEP's job. We are not the FEP, so `~` arrived as a non-character key code (`EKeyF21`,
   0xF82A), the letter arrived separately, and nothing joined them. `~` then `a` typed `a`.
   Nobody could write "não", "você" or "ção".
2. **No Chr/Fn symbol layer** past the twelve overlaid digits, so `+` could not be typed —
   in an application that asks for a phone number with a country code.
3. **No notion of a layout**, so there was nowhere to put a fix that would not also break
   handsets we have never held.

## Why the translation is here and not in the shim

The C++ shim keeps the job only it can do: receive the `TKeyEvent`, return a
`TKeyResponse`. Everything semantic — which character a key means in which case, which
keys are dead, what `´` plus `a` is — is plain data and plain arithmetic. In a `no_std`
crate with no dependencies that buys two things the C++ could not:

- it is **testable on the host**, and a keyboard is all edge cases;
- the **simulator** uses the same layout, so an accent bug reproduces in a window instead
  of only on the phone.

## Where the tables come from

Measured, never reasoned about. `docs/device-notes.md` records three rounds of on-device
debugging spent on keyboard behaviour that had been deduced, and each round blamed the
wrong layer.

So `layout_abnt2` is **generated** by `tools/mkkeymap.py` from a dump of the handset's own
keymap, read out of `ptiengine.dll` by `examples/keydump`. `ptiengine` is the platform's
keymap engine, the layer *underneath* the FEP — asking it what a key means is not the same
as letting the FEP own our text buffer, which is the thing we are not doing. And it is
asked exactly once, offline: the answer is baked into a static table, so nothing that
ships imports the DLL or pays for the engine.

## Using it

```rust
use symbian_keys::{Keyboard, Layout, Press};

let mut kb = Keyboard::new(Layout::Abnt2E72);
// An unmapped key falls through to the character the platform already translated.
let press = Press { scan: 0x45, code: b'e' as u16, shift: false, func: false, ctrl: false };
let stroke = kb.press(press);
```

One press in, up to **two** characters out: a dead key followed by a letter it cannot
combine with produces both, and the application should see two ordinary keystrokes. That
is why `Stroke` is not a single `char`, and it is the case that a naive implementation gets
wrong — silently, by swallowing the accent.

`symbian-app`'s `entry!` already drives this, so an app written on the SDK sees composed
`Key::Char`s and never touches this crate.

## Modules

| | |
|---|---|
| `layout` | the `Layout` enum and the table shape: base, shifted, Fn, and which keys are dead |
| `layout_abnt2` | the E72 ABNT2 table. **Generated** — edit `tools/mkkeymap.py`, not this |
| `compose` | dead key plus letter to a codepoint, and the two-characters-out fallback |
| `tests_abnt2` | every accent the layout claims, composed and asserted |

## Adding a layout

Get the handset, build `examples/keydump`, run it, and feed the dump to
`tools/mkkeymap.py`. Do not hand-write a table from a picture of a keyboard: that is what
the three debugging rounds were.

---

Part of [epoc](../../README.md), a Rust SDK for Symbian S60 3rd Edition. MIT licensed; see
`LICENSE` at the repository root. `symbian` in this crate's name is descriptive, not a claim
on somebody else's trademark - the repository README says more. Written with AI assistance,
and every hardware claim in it was measured rather than reasoned about.
