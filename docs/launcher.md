# A launcher, and the abstraction under it

## What a launcher is on this platform

A home-screen replacement on S60 3rd edition sounds exotic and is not. Reverse-engineering a real
one — a signed, third-party package, dissected with `tools/sisdump.py` — showed it is three
ordinary things the SDK already had most of:

1. **A fullscreen app that draws.** The `symbian_ui::App` trait, which the shim already runs with
   the status pane and softkey bar removed (`ENoScreenFurniture` in `shim_app.cpp`), so the app
   owns the whole 320×240 and sees every key.
2. **An app that starts at boot.** The S60 Startup List Management API: a compiled
   `STARTUP_ITEM_INFO` resource dropped in `\private\101f875a\import\[<UID>].rsc` (UID
   `0x101F875A` is the Software Installer's start-up import directory). Honoured only in a signed,
   trusted package — which a ROM-patched handset grants a self-signed cert.
3. **An app that can enumerate and launch other apps.** `RApaLsSession`, the application registry
   the native menu itself reads.

Only the third was missing. The reverse-engineered package installed a whole suite — a tiny
autoboot exe, a draggable panel, daemons, themes — but the *mechanism* was these three, and the
important discovery was the strategy: it **did not** overwrite the native idle's Central Repository
key. It installed its own app and autostarted it, running *alongside* the platform's idle rather
than replacing it. If it fails, the phone still boots to a working home. That is the lower-risk
model this SDK follows.

## The startup resource, byte-verified

`STARTUP_ITEM_INFO` (from `<startupitem.rh>`) is one component: an executable path, a recovery
policy, and reserved fields. `tools/symbuild` compiles it and packages it to the import directory
above. The resource this SDK emits was diffed against the real package's own start-up resource and
has the same structure — same `0x101F4A6B` compiled-resource header, same field layout — differing
only in the path it names. The one thing the other package carried that this SDK does not is a
`--` command-line argument embedded in the path; the SDK emits a bare executable name.

## The abstraction

Four layers, following the convention the custom-MTM feature established (a shim C++ boundary, a
safe wrapper in the core crate, a reusable Rust side, and a reference app):

| Layer | Where | What |
|-------|-------|------|
| AppArc ABI | `shim/src/shim_apparc.cpp` | `RApaLsSession`: refresh, count, read-by-index, launch-by-UID. Gated by `USE_APPARC`, links `apgrfx`. |
| Safe wrapper | `crates/symbian/src/apps.rs` | `AppInfo`, `installed()`, `launch()`, and the `Apps`/`ShimApps` trait with a host fake — mirrors `process.rs`. |
| The screen | `crates/symbian-launcher` | `HomeScreen<A: Apps>`: the grid, the cursor, launch-on-select. Generic over the app source so it is host-tested against a fake roster. |
| Reference app | `apps/launcher` | Wraps `HomeScreen<ShimApps>`, gives it an identity and a package. `USE_APPARC=1`, `AUTOSTART=1`, `SIGN=1`. |

Autostart itself is declarative: set `AUTOSTART=1` and `symbuild` generates the start-up resource
from the shared template `shim/launcher/data/startup.rss.in`, substituting only the app name. A
hand-written `data/<name>_startup.rss` still wins for an app that needs a non-default item.

Writing a different launcher is the same three lines with different arguments: construct a
`HomeScreen` over `ShimApps`, set a title and column count, hand it to `symbian_app::entry!`.

## Being the home: resident mode ("Replace Main") — the behaviour we want

Turning a fullscreen app into a home screen is not idle replacement (GDesk, dissected, does not
touch cenrep — it captures a key and runs resident). It is one capability, `SwEvent`, and a
deliberately tiny, **confirmed-on-the-E72** key contract in `shim_app.cpp`. Changing the captured
set without re-testing on the handset regresses it — this was expensive to get right.

**"Replace Main" is a runtime toggle** (Manage → Replace:ON/OFF), persisted, defaulting on — the
same shape as GDesk's setting. When on, `symbian::apps::set_resident(true)` captures keys and the
UI drops its Exit softkey; off, it is a plain, exitable app that captures nothing.

**Keys captured GLOBALLY — the minimum, and never a key another app needs:**

| Scan code | Key | Why |
|---|---|---|
| `0xB4` `EStdKeyApplication0` | the "casinha" / apps key | brings the launcher forward from any app — *exactly and only* what GDesk captures |
| `0xC5` `EStdKeyNo` | red End key | by the user's choice, red goes home too |

**Deliberately NOT captured:** the Menu key `0x94` `EStdKeyMenu` (capturing it stole the D-pad in
some other apps, and it is not needed — the casinha is `0xB4`); the softkeys
`EStdKeyDevice0/1` (capturing them froze every other app — no Options, no Back); everything else.
A global capture is phone-wide, so the rule is: **capture the minimum, foreground-handle the rest.**

**On the red/apps key the launcher comes to the front** (`SetOrdinalPosition(0)`), it does not
exit and does not step aside to the native idle. A resident launcher never exits from a key —
`HandleCommandL` ignores the exit command while resident; the way out is Replace:OFF or the bundled
Kill Home app.

**Signing and the clock.** The package is signed (`SIGN=1`) so the startup-list autostart is
honoured — it does come up on boot on the patched handset. A "certificate error" on install is the
phone's **clock in the past** (our cert's validity starts in the future then); fix the handset date
before suspecting the cert.

## What is next

This is the first increment — a launcher that lists and launches. The path from here:

- **Hide/show apps.** The `hidden` flag is already carried through `AppInfo`; acting on it (and on
  the native apps a user wants out of the menu) is `RApaLsSession`/`SetAppHidden`, which needs
  `WriteDeviceData`.
- **A richer home.** Icons (the reference app ships none yet, `number_of_icons = 0`), wallpaper,
  shortcuts, folders — and the open question of whether to keep the run-alongside model or take
  over the native idle through its Central Repository key.

## Tools

`tools/sisdump.py` walks a SISX package: the install manifest (where every file lands) and the
payloads (decompressed, written out by basename). It is how the reference package above was read,
and how a build here is verified — run it on `apps/launcher/build/launcher.sisx` to confirm the exe,
the registration resource, and the `[<UID>].rsc` start-up item all route where they should.
