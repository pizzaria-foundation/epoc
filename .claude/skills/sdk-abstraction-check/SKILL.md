---
name: sdk-abstraction-check
description: Decide whether a component, screen, widget, flow or helper written inside `apps/*` belongs in an SDK crate (symbian-gfx / symbian-ui / symbian / a new crate) instead. Run it while building or reviewing any app-level UI or platform code, and before declaring such work done.
---

# Does this belong in the SDK?

Every screen written in `apps/*` is a chance to either grow the SDK or fork it by
accident. The check is cheap and the default answer is "not yet" — but it must be
*asked*, and the verdict stated, not skipped.

Run it at two moments: **before writing** (does this already exist?) and **before
finishing** (does this now deserve to move?).

## Step 1 — before writing: does the SDK already do it?

Reimplementing SDK arithmetic in an app is a bug, not a style preference. The
scroll/caret/contrast maths in `symbian-ui` exists because those are the bugs that
actually happen on this device, and each one is unit-tested.

| about to write | use instead |
|---|---|
| scroll offset, selection movement, visible-row loop, scrollbar thumb | `symbian_ui::list` — `ListState`, `Rows`, `Uniform`, `for_visible`, `scrollbar` |
| text input, caret, backspace, char-boundary handling, masked field | `symbian_ui::edit::TextField` (`handle_key`, `display`, `take`) |
| title bar, softkey bar, screen split, selection band, avatar, badge, placeholder | `symbian_ui::chrome` (`Frame::split`, `title_bar`, `softkey_bar`, `selection`, …) |
| a raised/sunken band, separator, pill, highlight, frame | `symbian_ui::paint` |
| any colour literal, any spacing literal, any row height | `Theme`, `Palette`, `Metrics`, `Space`, `Surface::raised/sunken` |
| an arrow, tick, chevron, speaker, hourglass… | `symbian_ui::icon` — 20 shapes, theme-coloured |
| files, atomic save, time, sockets | `symbian::fs`, `symbian::monotonic_us`, `symbian::net` |

A hardcoded `Color::rgb(...)`, or a literal `18` where `theme.metrics.title_h` was
meant, is the loudest signal that this step was skipped: it breaks the moment a palette
or a metric changes, and theme-swap-without-the-widget-knowing was the whole point of
the token layer.

## Step 2 — the house rule

From `crates/symbian-ui/README.md`: there is no retained widget tree and no
`Box<dyn View>` on purpose. **The SDK owns arithmetic, pixels and the platform. The app
owns composition and its domain.** On a 320×240 screen with one D-pad, composition is
not the hard part — arithmetic is, and arithmetic is what is worth extracting and
unit-testing.

So the question is never "is this reusable code?" It is "is the hard part of this
general?"

## Step 3 — the five questions

Promote when essentially all are yes:

1. **Second consumer.** Would an unrelated app — a notes app, a converter, a viewer —
   want this? Can you name it?
2. **No domain vocabulary.** Can the signature be written with primitives and
   `gfx`/`ui` types only? If `Chat`, `Message`, `Login` or `Store` appear in it, the
   answer is no until they are designed out.
3. **The hard part is general.** Pixel geometry, scroll/caret maths, layout, encoding,
   a platform quirk — versus "which screen comes after this one", which is the app.
4. **Testable on the host.** Can it be proven with `cargo test` and no handset? The SDK
   is 164 host tests; code that can only be checked by flashing a phone does not belong
   in it.
5. **Fits the constraints.** `no_std` + `alloc`, `forbid(unsafe_code)`, no allocation
   while drawing, no global state.

Keep it in the app when: there is one call site and no nameable second consumer; the
generalisation needs a bool that only distinguishes the app's own two cases; a feature
flag would be needed to serve two callers; or the interesting part is the app's state
machine wearing a widget as a hat.

## Step 4 — where it goes

```
symbian-gfx    pixels, geometry, fonts, rasterising          no_std, no alloc while drawing
symbian-ui     visual vocabulary + screen arithmetic         tokens, paint, chrome, list, edit, icon, theme
symbian        safe platform services                        fs, time, net
symbian-app    device entry points
symbian-crypto primitives
epocadb          the dev bridge (dev-only, feature-gated)
new crate      only when it needs its own dependency graph or an opposed design
apps/<app>     composition, domain, flow
```

A new crate is a real decision, not a folder — `docs/plan-declarative-ui-sdk.md` is what
that case looks like written up (a declarative layer *over* `symbian-ui`, explicitly not
replacing it). Do not spawn one without that level of argument.

## Step 5 — the halfway move (usually the right one)

When the answer is "maybe, once there is a second caller", do not promote and do not
shrug. **Shape it for promotion where it sits:**

- a free function, not a method on the app struct;
- primitives and `gfx` types in the signature, no `&self`, no `Store`;
- its own unit test next to it;
- one comment saying what would flip it:

```rust
// SDK candidate: `symbian-ui::chrome`. Pure geometry, no domain types.
// Move when a second app needs a two-line list row — one caller is not a pattern.
```

That costs nothing now and turns the eventual move into a file rename instead of a
redesign.

## Step 6 — when you do promote

1. Move it; delete the app-side copy rather than leaving both (two implementations
   diverge in a week).
2. Strip domain types out of the signature — if that is impossible, the promotion was
   wrong.
3. Add host tests for the *arithmetic*, including the off-by-one cases (a thumb one
   pixel past its track, a caret inside a multi-byte char, an index into a list that
   shrank).
4. Update that crate's `README.md`. Every crate README carries the decisions behind the
   crate; a new public item with no rationale there is half-landed.
5. Keep the app-side call thin — the app should read as composition, not as a wrapper
   over a wrapper.
6. `cargo test --workspace` green, `no_std` intact (`cargo build -p <crate>
   --no-default-features` if the crate has a `std` feature).

## Report the verdict

When finishing app work that touched a component, screen or flow, close with three
lines — even when nothing moves:

```
SDK check
  candidate: two-line list row with unread badge (chats.rs)
  verdict:   stays in the app — only caller, and the badge rule is Telegram's
  flips when: a second app needs a two-line row, or the badge loses its domain meaning
```

A deferral with a stated threshold is a good answer. Silence is not.
