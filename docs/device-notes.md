# Device notes

Things the hardware turned out to do, that no document said. Each one cost a round trip or
a day, and every one of them was guessed wrong first.

There is a method in here, and it is the most useful thing on this page: **on a platform
with no debugger, no console and no log, build the instrument instead of guessing.** Every
item below that has a definite answer got it from a purpose-built probe or from comparing
against a known-good artifact — not from reasoning about what ought to be true.

## The image format

**`codeBase` must be 0x8000, and `-Ttext` will not get you there.** `-Ttext` fights
`-shared`: it relocates only `.text` and strands every other section at its natural
address. The linker script owns the layout instead.

**elf2e32 concatenates every `SHF_ALLOC` section into the code segment.** It skips only
`.data` and the RVCT-named `ER_RW`/`ER_ZI`. So the dynamic-linking metadata — `.dynsym`,
`.dynstr`, `.hash`, `.rel.*`, `.dynamic` — was baked into the image: 18 KB of dead weight
that also pushed `codeBase` off 0x8000. `tools/e32prep.py` clears the flag on those
sections and parks them in a third, read-only program header, which matches neither of
elf2e32's tests.

**GNU ld pre-fills `.got` with PLT stub addresses for lazy binding,** and elf2e32 reads
GOT contents as relocation addends. 81 of 230 imports came out pointing at `0xa3ac000e`.
`e32prep.py` zeroes it; the Symbian loader writes the real addresses in during relocation.

**Not objcopy.** `--set-section-flags` reclassified `.rel.plt` and silently discarded its
82 `R_ARM_JUMP_SLOT` relocations, dropping `dllRefTableCount` from 12 to 6 with no
complaint. That is why `e32prep.py` patches the section header table byte by byte.

**`--gc-sections` is not optional.** `compiler_builtins` ships the entire soft-float libm —
`erfc`, `lgamma`, `pow`, `cbrt`, the f128 helpers — because the target declares no system
libm. None of it is called. Unswept, it was 136 KB of a 305 KB image, 78% of the binary.

Two things must be kept by hand when sweeping: `.emb_text`, which carries
`Symbian$$CPP$$Exception$$Descriptor` that no code references (the kernel finds it through
the E32 header), and `_xxxx_call_user_invariant` / `_xxxx_call_user_handle_exception`,
which the kernel calls and no relocation names.

**A malformed E32 fails silently.** The device refuses it by doing nothing at all: no
error, no panic, no log entry, just an icon that does not respond. `e32dump.py --quiet`
runs as a build gate for exactly this reason, and it re-derives the UID checksum and header
CRC independently of elf2e32 so that the tool being wrong is also caught.

### How the `codeBase` bug was actually found

By stopping guessing. `tools/sisextract.py` was written to carve a genuine Symbian-built
ARMv5 binary out of an unrelated `.sis` — brute-forcing raw-deflate at every offset and
keeping whatever had `EPOC` at 0x10 — and then diffing header fields against ours.
`codeBase 0xc748` against `0x8000` was obvious in one line of output, after two days of it
not being obvious at all.

## The screen

**The E72 reports `EColor16MU`** — mode 11, 32 bits per pixel — where a 16bpp canvas would
have matched `EColor64K`. It is not documented anywhere findable. `shim_screen_format` asks
and `shim_probe_pixel_layout` fills a 1×1 bitmap with pure red through the documented
`TRgb` API and returns the raw word, which turns "which byte is red?" into a fact.

Get it wrong and the window server converts every blit — the difference between a few
milliseconds a frame and tens of them, with no error to tell you.

**`LockHeap`/`UnlockHeap`, not `BeginDataAccess`.** The latter is Symbian^3 and does not
exist on FP2.

**`BitBlt`, never `DrawBitmap`.** `DrawBitmap` always scales.

**`DrawNow` plus an explicit `WsSession().Flush()`.** Without the flush, the frame sits in
the client-side command buffer until it fills, and appears late.

## The pump starved every active object at its own priority

**This is the most expensive bug in this file, and it was in the SDK, not in the device.**

`CShimAppUi::ConstructL` created the pump as `CIdle::NewL(EPriorityIdle)`, and the pump's
callback returns `ETrue`, which re-arms it on every pass. So the pump is not merely
low-priority: it is **permanently ready** at that priority, with no moment at which the
scheduler lacks something to run.

Symbian dispatches the highest-priority ready object and, among equals, the one added
first. A permanently-ready object at `EPriorityIdle` therefore does not go last — it
**starves every object at that priority added after it**. Not slowly. Never.

`CImageDecoder` drives decoding from active objects inside the plugin, and on the E72's
vendor JPEG codec those sit at idle priority. So every image decode issued from
`rust_step` — which runs from the pump's own `RunL` — was queued behind a permanently-ready
object and never ran. No panic, no error, no completion: `Convert` issued, `IsActive()`
still true a minute later.

The fix is one line: `CIdle::NewL(CActive::EPriorityIdle - 1)`. The pump should be the
lowest-priority object in the process, and "strictly below the documented floor" says that
without inventing a magnitude.

**It generalises.** Anything asynchronous whose active objects land at idle priority would
have met the same silent fate — the audio work planned next among them.

### The measurement that proved it, and how nearly it was missed

`examples/imgprobe` runs the same decode seven ways. Two rows settled it, and neither was
a row anyone designed for the purpose:

| | |
|---|---|
| row A, issued from `rust_app_start()` | **241 ms**, `status=0` |
| row B, issued from a timer's `RunL` | never completed |

Row B differs from row A only in the destination *field* used — and on this image
`iOverallSizeInPixels` and `iFrameCoordsInPixels.Size()` are both 240×320, so the two rows
were byte-identical in configuration. The only real difference was **when** each was
started: `rust_app_start()` is called from `ConstructL` *before* the pump is created, so a
decode begun there queues its plugin's objects ahead of the pump and runs; anything begun
later queues behind it and does not.

The answer was in the first two lines of the first report, in the column nobody was
reading.

### What is known about the destination, and what is not

With the pump fixed, a photo decodes using the shipped examples' recipe — frame rect,
`EColor16M`, no options, `FileNewL`, `EPriorityStandard`, exactly what
`sdk/s60cppexamples/OcrExample/` and `sdk/s60cppexamples/OpenGLEx/` do.

**Whether `EColor64K`, a reduced destination, or `DataNewL` also work is unknown.** They
were each blamed in turn while the pump was the cause, so every conclusion drawn about them
was drawn from a rigged experiment. The frame reports `iFlags = 0x15` (`EColor` +
`EFullyScaleable` + `ECanDither`) and `iFrameDisplayMode = EColor16M`; the ICL guide says
`ECanDither` means the mode may be chosen and `EFullyScaleable` means any size may be
asked for. That is what the documentation permits, and it is still not a measurement.
Rows C, D and F of the probe would answer it, and would let the 24bpp→RGB565 conversion in
`CopyOut` go away.

**`KErrUnderflow` is separately real.** It means call `ContinueConvert` until `KErrNone`,
and it can arrive for a whole image: `CImageDecoderPlugin::HandleProcessFrameResult` turns
"the codec consumed no bytes this pass" into an underflow the plugin never asked for.
Neither Nokia example handles it, so it is easy to inherit wrong by copying them.

### The method, which is the actual lesson

Six rounds of build → Bluetooth → install → ask-the-user, each varying one property on a
hypothesis the handset then refuted, each costing somebody an afternoon and yielding one
bit. The authoritative documentation — the complete Symbian OS v9.3 Developer Library, ICL
guide and both worked examples — was in `vendor/research/s60/s60doc/` and
`sdk/s60cppexamples/` the whole time; the search that "established" it was unavailable was
a web search.

`examples/imgprobe` should have been the first thing built, not the seventh.

**Its own first version was the same mistake in miniature.** All seven rows ran in one
process, so when a row starved the GUI thread it took down the six-second timeout meant to
give up on it, and five rows were lost. A probe whose measurements can kill each other is
not an instrument. It now runs **one row per launch**, with the index on disk and advanced
*before* the row is attempted, so a row that wedges the phone costs one relaunch and is
recorded by the absence of a result under its own breadcrumb.

## The keyboard

This one took three probe builds, and each round the wrong thing was blamed.

**The twelve overlaid keypad keys arrive as digit scan codes.** The E72 prints a phone
keypad over the letters — `1 2 3` on R T Y, `4 5 6` on F G H, `7 8 9` on V B N, `* 0 #` on
U M J — and the window server identifies those physical keys *as the digit keys*. Pressing
R gives `iScanCode == 0x31`, the scan code of `'1'`, not of `'R'`.

It is not a mistranslation. At the window server's level, that key **is** the 1 key. The
letter identity is applied above it by Avkon's FEP, from the input mode of the focused
editor.

`TCoeInputCapabilities::EAllText` is not enough — tried on device, no change. What the FEP
reads is the input mode on the focused editor's state object, `CAknEdwinState`, through
`SetCurrentInputMode`.

**So we translate, by choice.** An earlier version of this page said the FEP path was
*impossible* because `CAknEdwinState` was absent from the public SDK. That was false — see
the grep note below — and the correction matters, because "we had no option" and "we picked
this" are different claims and only one of them was true.

Taking the FEP means implementing `MCoeFepAwareTextEditor`, twelve pure virtuals, and giving
the FEP authority over a caret and text buffer the Rust toolkit already owns. Two components
holding one buffer is the bug, not the wiring.

### The keyboard was American and the handset is Brazilian

The twelve-row translation above lived in `shim_app.cpp` and worked, and it was still the
wrong shape, because the handset's keyboard is **ABNT2**. Three things followed:

- **No accents at all.** On an ABNT2 keyboard the accent keys are dead keys. Composition is
  the FEP's job, so with no FEP the window server hands them over as *non-character* key
  codes — `~` arrives as `EKeyF21`, 0xF82A — and the letter arrives separately. Nothing
  joined them. `~` then `a` typed `a`, and "não", "você" and "ção" could not be written.
- **No Chr/Fn symbol layer** past the twelve digits, so **`+` could not be typed**, in an
  app that asks for a phone number with a country code.
- **No notion of a layout**, so there was nowhere to put a fix that would not also break
  every handset nobody has held.

**The keymap now comes out of the phone.** `examples/keydump` links `ptiengine.dll` and asks
`CPtiEngine::MappingDataForKey` for every QWERTY key in all four cases
(`EPtiCaseLower/Upper/ChrLower/ChrUpper`) under `ELangBrazilianPortuguese`, plus
`GetNumericModeKeysForQwertyL` for the digits-and-`+` bindings. `tools/mkkeymap.py` turns
that dump into a static table in `crates/symbian-keys`.

`ptiengine` is **not the FEP**. It is the keymap database underneath it: a lookup function,
with no opinion about our caret or our buffer. Asking it what a key means commits us to
nothing, and we ask exactly once — offline, into a generated table — so nothing that ships
imports the DLL or allocates the engine. The import lives in a throwaway probe, which is the
rule this page already states: *if a facility might not resolve, it belongs in its own
binary, where failing to load costs a probe rather than the report.* `examples/libprobe`
answers whether `ptiengine.dll` is there first.

**No scan-code bridge is needed.** `TPtiKey`'s QWERTY values *are* `TStdScanCode` values:
`PtiDefs.h` has `EPtiKeyQwertyA = 0x41`, which is `EStdKeyA`, and names the punctuation keys
directly (`EPtiKeyQwertyComma = EStdKeyComma`). And `TPtiKey` names *positions*, not
characters — the header's own example is that unshifted `EPtiKeyQwertyHash` gives `#` in
English and `+` in Danish. That is why enumerating the standard keys reaches Ç and the
accents even though the enum has no name for either: an ABNT2 keyboard is the same grid with
different characters on it. The English dump is taken alongside as a control, and two
identical dumps mean the engine never switched language.

**What the measured dump says**, kept at `examples/keydump/keymap-brpt.txt`:

| scan | key | unshifted | shifted |
|---|---|---|---|
| 0x7A | `´` | dead, acute | dead, grave |
| 0x7E | `~` | dead, tilde | dead, circumflex |
| 0x79 | `Ç` | `ç` | `Ç` |
| 0x7D | `.` | `.` | `:` |
| 0x82 | `,` | `,` | `;` |
| 0x49 | `I` | `i` | `+` on Fn |

Four marks and no trema, so `ü` cannot be typed on this keyboard at all.

The Chr/Fn layer does **not** come from `MappingDataForKey`: asked for `EPtiCaseChrLower` it
returns the same data as `EPtiCaseLower`, so taking it at face value would put the letter in
the Chr column and make Fn+R type `r`. It comes from `GetNumericModeKeysForQwertyL`, and what
that returns is a better description of this handset anyway — the E72's Fn layer is not a
general symbol layer, it is the printed phone keypad plus what a phone number needs: the ten
digits, `*`, `#`, `+`, and `p`/`w`.

**What moved to Rust, and why.** The whole keymap and the dead-key composition are now in
`crates/symbian-keys`, and `shim_app.cpp` holds no keymap at all. It keeps the job only it
can do — receive a `TKeyEvent`, report `iCode`/`iScanCode`/`iModifiers`, and return a
`TKeyResponse`. Three reasons, in order of what they cost while the table was in C++: a
correct ABNT2 table is dozens of rows with four cases each and has to be generated from a
measurement; nothing in C++ here can be unit-tested, and a keyboard is all edge cases; and
the simulator can share a Rust table, so an accent bug is now reproducible in a window
instead of only on the phone.

The table is keyed by scan code, which retired a guard the C++ needed. It used to test
`iCode == iScanCode` to tell "the window server did not translate this" from "it did",
because its table was keyed by *character* and a real `R` key could collide with the overlaid
`1`. A scan code identifies one physical key, so there is no collision to guard against.

`Layout::PassThrough` is the default for anything unmeasured and reproduces the old
behaviour exactly: use the character the window server produced. A key the ABNT2 table does
not claim falls through the same way, which is what keeps a handset nobody has held working.

**Nothing had to change in `OfferKeyEventL`.** The expectation was that a dead key would
arrive as a non-character code above 0xF800, which this file would have to recognise and
consume so Avkon did not also act on it. It does not: the code is `0xF001..0xF005`, inside
the printable gate, so it already goes out as `SHIM_EV_KEY_CHAR` and is already consumed. A
table of dead-key scan codes was written for that job and then deleted — unreachable, and
worse than useless, because it named a mechanism the handset does not use.

### Two things that read backwards

Both cost a wrong turn, and both are the kind of thing this page exists for.

**The dead-key marker is not `KPtiKeyDataDeadKeySeparator`.** That constant (0xFFFF) is real
and marks something else — sections of the dead-key *table* blob. What appears in a key's
mapping is `0xF000..0xF005`, and the platform's own test is
`CPtiQwertyKeyMappings::IsDeadKeyCode`, inline in `PtiKeyMappings.h`. Looking for the
constant whose name says "dead key" found zero dead keys on a keyboard that has four. The
mark itself is the code unit *after* the marker, which is better than the index alone: it
means the mark comes from the device rather than from reading the plastic.

**A plausible `iCode` can be the US keymap leaking through.** keyprobe shows an unshifted
press of scan 0x7A arriving as `chr 002E '.'`. Read as "this key types a full stop", that
looks like proof the dump is wrong and the acute belongs elsewhere — and acting on it
replaced the acute with a period on a shipped build. The truth is the opposite: 0x7A is
`EStdKeyFullStop`, the *US* period key, and a window server with no FEP has no Brazilian
character to hand over, so it falls back to the US keymap. That fallback **is** the
"keyboard is kind of American" bug, not evidence about the key. The dump settles it by
listing the real period and comma separately, on 0x7D and 0x82 — two period keys and no
acute key is not a layout any keyboard has.

So dead keys resolve two ways, because the handset offers them two ways: by scan code and
case from the table, and by dead-key code when the window server sends one. A test fails if
the two ever disagree, since otherwise which character you got would depend on which path a
press happened to take.

### Filling the table: the three device runs

1. **`examples/libprobe`.** Confirm `ptiengine.dll` loads. If it does not, `keydump` will
   not start and will not say why — a static import of a missing DLL stops the loader with
   no error, no log and no report file.
2. **`examples/keydump`.** Install, launch, read the screen. `err 0` with a non-zero `dead
   br` count and a `'+' binding` means the dump is good. `dead br 0`, or `br == en`, means
   the engine did not switch language and the dump describes the wrong keyboard — the app
   says so on its own bottom line. Fetch `keymap.txt` from `C:\private\E123456C\` over
   `epocadb` or `tools/btrecv.py`, then:

       tools/mkkeymap.py --check keymap.txt          # read it before trusting it
       tools/mkkeymap.py keymap.txt -o crates/symbian-keys/src/layout_abnt2.rs
       cargo test -p symbian-keys                    # the generated table has invariants

   `tools/testdata/keydump-synthetic.txt` is a fabricated file for testing the parser, and
   says so in its own header. It must never generate a shipped table.
3. **`examples/keyprobe`.** A sweep to check the dump against the hardware, because the two
   disagree in one place and the disagreement is not visible from the dump alone. Press each
   key and read `scan` and the code beside it. This is what caught the U key: it fires scan
   0x2A, not the `EStdKeyNkpAsterisk` (0x85) inherited from the C++ table this replaced, and
   nothing matched 0x85 so the U key typed `*`. `OVERLAY` in `tools/mkkeymap.py` is the only
   place in the pipeline where a value is written by hand rather than parsed, so it is the
   only place a mistake of that kind can hide.

Then, on the phone, in a text field: `´`+`a` → `á`, `~`+`a` → `ã`, `^`+`e` → `ê`,
`` ` ``+`a` → `à`, `´`+space → `´`, `´`+`q` → `´q`, the Ç key → `ç`, Shift+Ç → `Ç`, Fn+I →
`+`. And the two regressions most likely to be caused by all of this: `.` and `,` must still
type themselves, and the twelve overlaid keypad keys must still give letters without Fn and
digits with it.

**The Fn key was being dropped.** `EStdKeyLeftFunc` is scan code 0x18 and arrives as
`EEventKeyDown`. The shim's handler began with `if (aType != EEventKey) return
EKeyWasNotConsumed;` — so it was discarded on the first line of the function, every time.
The shim now tracks Fn itself and mirrors the platform: one press arms the next character,
two presses lock, a third releases.

**And the instrument was lying.** The first probe showed `mod 00` for every key, which read
as "no modifier involved" and was treated as evidence. It was not: the probe collapsed
`iModifiers` into three bits — shift 0x400, ctrl 0x80, func 0x2000 — of a word that also
holds `EModifierNumLock` (0x8000), `EModifierPureKeycode` (0x100000) and
`EModifierKeyboardExtend` (0x200000). `ShimEvent::native` now carries the whole unmasked
word, and any key the shim does not recognise is reported as `Key::Raw` rather than
dropped, because a silently discarded event is how the Fn key stayed invisible for two
rounds.

## A panic that says where it was

A Rust panic reaches `User::Panic(category, number)` through `shim_panic`, with the category
being the tail of the source file name and the number its line. Both facts are therefore
already known at the moment of death — and on this handset the dialog does not reliably show
them, so they died with the process. With no debugger and no console, a reproducible crash
then cost one round trip per guess.

`shim_panic` now writes the location to **`C:\Data\panic.txt`** before panicking, appended
so a second crash does not erase the first — order matters when one panic is a consequence of
another. Its own `RFs` session rather than the shim's, because the shim's may be exactly what
broke; a reentrancy flag, so a fault inside the writer cannot become the crash it was meant to
explain; and every error ignored, for the same reason. `WriteUserData` and `C:\Data\` rather
than the data cage, because a breadcrumb nobody can carry off the phone is not a breadcrumb.

**It paid for itself on the first crash.** "Scrolling to the end of the chat list closes the
app" had already survived one round of reading code and eliminating hypotheses — an empty
page, the pagination arithmetic, the list widget's bounds, the `messages.dialogsSlice` parse,
all of which turned out to be fine and now have tests saying so. The breadcrumb read
`chats.rs:659`, and the cause was visible immediately.

It was `channelForbidden` — a channel the account had lost access to. Field indices are
positional and belong to a **constructor**, not to a kind: `channel` carries `access_hash` at
index 31 and `channelForbidden` has eight fields in total. The parser picked id and title per
constructor, correctly, and then the access hash per *kind*, so any chat list containing one
read index 31 of an eight-field value. The page that happened to contain one was the second,
which is the only reason the end of the list came into it at all.

Two lessons, in the order they cost something:

- **A crash that describes itself is worth more than any amount of reading.** Four hypotheses
  were eliminated by inspection before the one-line answer arrived. The tests written along
  the way are worth keeping; the rounds spent producing them were not necessary.
- **A symptom's trigger is not its cause.** "At the end of the list" was a true and complete
  description of when it happened, and it pointed at pagination, which was innocent.

## Capabilities and the filesystem

**`C:\private\<UID3>\` needs no capability.** It is the one writable location an unsigned
app can reach. Anywhere else needs `WriteUserData` or more — and a capability an unsigned
package declares is a capability a stock phone refuses to install.

`RFs::PrivatePath` returns a *drive-relative* path with a trailing backslash, so the drive
has to be prepended by hand. `C:` on purpose, not the drive the binary was installed to: a
memory card can be removed with the app's data on it.

**`RFile` is genuinely synchronous** — `Read`, `Write`, `Seek` and `Size` all return a
`TInt` rather than completing into a `TRequestStatus`. Files need no active object at all,
which makes them by far the easiest platform service to wrap. Sockets are the opposite.

**`RFile::Read` may return less than you asked for**, at buffer boundaries inside the file
server. A single call is not a whole-file read, and treating it as one gives you a
truncated store that parses correctly and is wrong.

**`RFs::Rename` refuses to overwrite**, returning `KErrAlreadyExists`, so an atomic replace
has to delete the destination first. That opens a window where neither name holds the new
data — but the old file is intact until the rename lands, so a crash in the window loses
the update rather than corrupting it.

**`RFile` is 32-bit on 9.3.** `RFile64` arrived in Symbian^3, so an offset past 2 GB has to
be refused rather than silently truncated.

## What the platform gives you, and does not

Measured against the public SDK, not recalled:

| | |
|---|---|
| SHA-1, MD5 | ✅ `hash.dso` |
| SHA-256 | ❌ nowhere. Not in `hash.h`, not in Open C's OpenSSL |
| AES, RSA, bignum | ❌ in the public SDK — `crypto.dso` exposes certificates and signatures, not primitives |
| Random | ✅ `random.dso` |

**Open C changes that, if the handset has it.** The SDK ships `libc.dso`, `libcrypto.dso`,
`libssl.dso`, `libz.dso`, `libm.dso`, `libpthread.dso` and 259 POSIX headers — that is BSD
sockets, `fopen`, OpenSSL **0.9.8a** and zlib. `BN_mod_exp`, `AES_encrypt`,
`RSA_public_encrypt`, `RAND_bytes`, `HMAC` and `inflate` are all exported. `SHA256_Init` is
not: 0.9.8a is from 2005 and predates it, and the header does not mention SHA-256 at all.

Whether a *given phone* has the Open C runtime is a property of the handset, not of the
SDK — it shipped as a separate package on S60 3rd Edition. Importing a DLL that is not
there does not degrade: the E32 loader refuses to start the process, which presents as the
icon doing nothing. `examples/libprobe` asks with `RLibrary::Load` instead, which is also a
stronger test than checking the filesystem — a DLL can be present and still fail to load
through a wrong UID, its own unsatisfied imports, or a capability we do not hold.

## An import that does not resolve makes the app vanish

Calling six methods on `CCommsDatabase` to enumerate access points added six ordinals to
the import table from `commdb.dll` — a DLL the binary already imported, so the DLL *set*
did not change and nothing in the build complained. On the handset the application stopped
starting. No panic dialog, no log, and — the part that matters — **no report file**, even
though the report flushes from the first phase onward.

That absence is the diagnosis. A crash in application code leaves a partial file; a loader
failure leaves nothing, because no application code ever ran. So "the file did not appear
at all" and "the file stops somewhere" are different findings, and worth asking about
separately.

The E72 runs Symbian 9.3 against an SDK whose `commdb.dso` is not necessarily the same
build, and an ordinal that is absent means the loader refuses the image outright. Two rules
follow:

- **New imports are a deployment risk, not just a link-time question.** `e32dump` reports
  the count; comparing it before and after a change tells you what you added. Six calls,
  six imports, and removing them took the count from 353 back to 347.
- **Keep diagnostics off the critical path.** This was an optional probe that took the
  whole application down with it. If a facility might not resolve, it belongs in its own
  binary, where failing to load costs a probe rather than the report.

## Getting a socket open: six rounds, and none of them the socket

The transport worked from the first build. Everything that failed was above it, and each
failure looked exactly like the one below it.

**Round 1 — the deadline.** `RConnection::Start` was given 12 s. Two strategies timed out.
Read as "the network is broken".

**Round 2 — the same, longer.** 30 s. Same result, read the same way.

**Round 3 — the report that was actually an answer.** Two lines:

```
  .    IAP 1: err -1
  FAIL timed out  bearer      <- IAP 2
```

An access point that does not exist answers `KErrNotFound` in milliseconds. One that
*times out* was accepted and was still coming up when the deadline fired. So IAP 2 existed
and was working, and the sweep killed it. `err <n>` only ever prints from the async branch,
which means `RConnection::Start` had been completing correctly the whole time. Misread as
another bad guess.

**Round 4 — the guess that stopped the image loading.** Reading that timeout as a bad id led
to reading the comms database instead: six `CCommsDatabase` calls, six new ordinals from an
already-imported `commdb.dll`, and the application stopped starting. No panic, no log, **no
report file** — which is itself the diagnosis, since the report flushes from the first phase.
A crash in application code leaves a partial file; a loader failure leaves nothing.

**Round 5 — the timeout that was a person.** One access point timed out at **35013 ms** in
two separate runs, to the millisecond. A radio failing to associate does not do that; a
countdown does. Every attempt can raise a dialog and wait for a human, not only the one
named "prompt". Three rounds of tuning that number were all sized for a network.

**Round 6 — the thing that was there all along.** `RSocket::Open` and `RHostResolver::Open`
have overloads taking no `RConnection`, and `shim_net.cpp` had always called them when the
handle resolved to nothing. The stack then uses whatever route is already up — and on a
handset whose browser works, one is.

```
  ok   no bearer: socket on the existing route  no RConnection, no dialog
```

No dialog, no negotiation, nothing that can time out. It is `Bearer::none()` now.

Two things worth keeping from that:

- **A timeout is a measurement of your deadline, not of the system.** Print the elapsed
  time next to every one. "Answered instantly" and "spent the whole budget" were printing
  the same line for three rounds, and that one missing number is what hid the answer.
- **A phase that can be slow must narrate itself.** The sweep wrote nothing for two and a
  half minutes, so a run in progress and a hung app were indistinguishable from the file.

Also settled: **the handset had no SIM.** Three access points answered `-4180`, which is
`KErrEtelGsmBase` (-4000) territory — cellular access points correctly reporting no
cellular network. Only the Wi-Fi one was ever going to work, and none of the sweep logic
could have known that.

## The optimisations, measured on the handset rather than predicted

| | before | after | predicted |
|---|---|---|---|
| AES-256 | 169 KB/s | **2461 KB/s** | 3550 KB/s |

The T-tables transferred: **14.6x**. The prediction was 21x, taken from the host ratio, and
it was too high for the reason the module doc gave before the measurement existed — an
out-of-order core gains more from a shortened dependency chain than an in-order ARM11 does.
The number that was honest all along was the ARM instruction count, which said 12% for
SHA-512 and made no claim about AES beyond "a factor of twenty, not a third".

Also confirmed present: **fepbase.dll**, **random.dll** and **cryptography.dll** all load.
The FEP path for the keyboard's Fn layer is therefore available, and `CSystemRandom` is
there if the entropy pool ever needs upgrading.

## Three error codes, three different causes

A bearer sweep on a handset with nothing available is not one failure repeated. Each code
is a complete answer arriving on its own, not a deadline firing, and they say different
things:

| code | | what it means here |
|---|---|---|
| `-1` | `KErrNotFound` | the access point does not exist. ~450 ms |
| `-4180` | ETel GSM range | packet data, and there is no SIM. ~13 s |
| `-18` | `KErrNotReady` | **Wi-Fi, and the radio is off.** ~32 s |
| `-22` | `KErrLocked` | something is holding a connection — see below |

Which settles a question three rounds of work had been circling: with no SIM and Wi-Fi off,
the handset has no route at all, and no amount of sweeping produces one. The stack was
never the problem.

The timings are as diagnostic as the codes. An access point that does not exist answers in
under half a second; one that exists and cannot come up spends ten to thirty. Printing the
elapsed time beside every result is what makes those distinguishable — and it is the one
change that would have shortened the whole bearer investigation.

## A lookup nobody answers holds the connection

`shim_dns_resolve` had no closing call. On a handset with a route that is harmless, because
every lookup completes. On one without, the resolver stays open holding whatever connection
it was made against — and the bearer sweep that follows answers `KErrLocked` on a prompt
that waited nearly two minutes.

`shim_dns_close` exists now, and the self test calls it when a lookup times out. The general
shape is worth keeping: **every asynchronous request needs a way to abandon it**, and the
one that never completes is exactly the one that needs it.

## Joining a route that is already up: RConnection::Attach

Every other program on the handset reached the network and ours did not, through three
device runs. The mechanism was wrong, and the platform has an explicit call for exactly
what was wanted:

```cpp
TUint count = 0;
conn.EnumerateConnections(count);              // es_sock.h:1172
if (count) {
    TPckgBuf<TConnectionInfo> info;
    conn.GetConnectionInfo(1, info);           // one-based, by convention
    conn.Attach(info, RConnection::EAttachTypeNormal);   // synchronous
}
```

Synchronous, no dialog, nothing to time out, and `KErrNotFound` when there is nothing to
join. All three are exported from `esock.dso`, which the shim already imports.

`EAttachTypeNormal` rather than `EAttachTypeMonitor`: monitoring watches an interface
without keeping it alive, so the idle timer would tear it down mid-transfer.

### What it replaced, and why that looked like it worked

The old path opened a socket with **no** `RConnection`, on the reasoning that the stack
would then use whatever route already existed. Its comment called that the only strategy
with no dialog, no negotiation and nothing that can time out. All three were true. All
three were irrelevant, because it also could not find or create a route.

What that path actually uses is the handset's **configured default connection** — not one
that happens to be up. On a handset with none, the socket opens, the connect is issued, and
nothing ever completes. So the strategy reported success unconditionally and three phases
timed out beneath it:

```
ok   no bearer: socket on the existing route
FAIL timed out  dns
FAIL timed out  tcp echo
FAIL timed out  http
```

**The absence of a mechanism is not a mechanism.** It cannot fail, which reads as working.

The corroborating evidence had been in the report all along: `RConnection::Start()` with no
preferences answered `KErrNotFound`, which is that same default connection saying it does
not exist.

### And the reading that was skipped

Four examples in this SDK open a connection. Two were read — `WebClient` and `Chat`, both
of which use the synchronous `Start()` with no preferences — and `es_sock.h`'s own
connection API was never read past `Start`. `Attach`, `EnumerateConnections` and
`GetConnectionInfo` are thirteen lines further down the same class.

## Measured on the handset, not estimated

From the device self test, after the clock was fixed. These replace the scaled-from-host
guesses the docs carried:

| | E72 |
|---|---|
| SHA-256 | 8 MB/s |
| AES-256 | **169 KB/s** |
| 2048-bit modpow, GUI thread | 815 ms |
| 2048-bit modpow, worker thread | 1933 ms wall |
| full-screen fill (320x240) | 0.6 ms |
| present (RGB565 -> XRGB8888 + BitBlt) | **15.1 ms** |
| frame total | 15.7 ms, so 63 fps |
| 64 KB file write | 46 ms |
| 64 KB file read | 5 ms |

Three of these change decisions.

**Present costs 96% of the frame.** The fill is 0.6 ms and the present is 15.1 ms, so
drawing less does almost nothing for frame rate — the cost is the RGB565-to-XRGB8888
expansion and the BitBlt, both proportional to screen area and paid whatever the frame
contains. The optimisation that would pay is a dirty-rectangle present; drawing fewer
pixels is not.

**AES is the slow primitive**, at 169 KB/s against SHA-256's 8 MB/s — a 48x gap where the
algorithms differ by maybe 3x in work. That is the byte-at-a-time implementation with no
T-tables, and it is the thing to optimise if a real protocol ever runs here.

**The worker thread does not make a job faster; it makes it not block.** The same modpow
is 815 ms on the GUI thread and 1933 ms on the worker, because this is a single core and
the worker is sharing it with a GUI that keeps redrawing. Wall time went up 2.4x and the
interface stayed alive for all 26 ticks of it. That is the trade, and it is the right one
— but a design that assumes background work is free is wrong on this hardware.

## Two units and one precedence, found by the device self test

**`HALData::ENanoTickPeriod` is in microseconds.** The name says nanoseconds; `hal_data.h`
says "The time between nanokernel ticks, in microseconds". Reading it as nanoseconds made
`shim_now_us` return milliseconds under a microsecond name, so every duration the SDK ever
printed was 1000x too small.

What is worth keeping is *how* it surfaced. Nothing failed. The self test reported a
2048-bit modpow at 0 ms, a 64 KB flash write at 23 us, and a framebuffer fill implying
66666 fps — all plausible-looking numbers in a passing report. The lie was only visible
by knowing what the hardware cannot do: 600 MHz ARM11 does not write flash at 2.8 GB/s.
**A measurement has no error bars, so a wrong one looks exactly like a right one.** Sanity
against physics is the only check a timing has.

**Append must be tested before create.** `symbian::fs` maps `OpenMode::Append` to
`WRITE|CREATE|APPEND`, and the shim tested `CREATE` first — which is `RFile::Replace`, which
truncates, so the following `Seek(ESeekEnd)` landed at zero and an append became an
overwrite. The host fake missed it because it models the `OpenMode` enum, one layer *above*
the flags where the bug lived. A fake above the buggy layer cannot see the bug; this one
was caught by the first run on the handset.

## Searching this SDK

**Always `grep -a`.** Most of these headers carry a `©` in the copyright line, encoded as
extended-ASCII rather than UTF-8. `grep` sees a byte outside the locale's character set,
concludes the file is binary, and **suppresses every match without saying so** — not
"binary file matches", nothing at all. `file` calls them "Non-ISO extended-ASCII text".

```
grep -n  'class RThread' sdk/epoc32/include/e32std.h    # → nothing
grep -an 'class RThread' sdk/epoc32/include/e32std.h    # → 3522:class RThread : ...
```

This produced a wrong conclusion that reached committed code and documentation: that the
FEP path for the keyboard was unavailable, because a search for `CAknEdwinState` came back
empty. The class is on line 158 of `aknedsts.h`.

It is worth naming the shape, because it has now happened twice. The key probe reported
`mod 00` for every key while masking three bits of a 32-bit word, and the inflate check
reported 72 cases passing while silently skipping nine. In all three the tool reported
success while examining less than it claimed — which is worse than a tool that fails, because
a failure gets investigated.

An empty search result is evidence of nothing until the search itself has been tested against
a string you know is there.

## Host toolchain

**`-D__SUPPORT_CPP_EXCEPTIONS__` and `-fexceptions` are load-bearing.**
`symbian_os_v9.3.hrh` switches `__LEAVE_EQUALS_THROW__` *off* unless that macro (which
"the tools" are expected to define) is present. Without it, `TRAPD` falls back to the
legacy `TTrap` mechanism, whose implementation is in no `.dso` and no `.lib` in the public
SDK — `TTrap::Trap` appears nowhere and the link simply fails.

**`-isystem <gcc include>` after `-nostdinc`.** `-nostdinc` drops the compiler's own
headers along with the host's, and Symbian 9.3 has no `stdint.h` equivalent. The shim's ABI
is written in `uint32_t`.

**`-Wno-narrowing`.** The SDK's own `vwsdef.h` brace-initialises a `TUid` from a value that
does not fit `TInt32`, which C++11 promoted from warning to error.

**Library order matters.** `scppnwdl` before `drtrvct2_2`, per the SDK's own `gcce.mk`, or
`operator new` resolves to the wrong implementation. `eexe.lib` carries `_E32Startup` and
`usrt2_2.lib` the user-side runtime; both live in `release/armv5/urel`, not in `lib/`.

**`-lsupc++`.** Symbian's `drtaeabi.dso` covers the `__cxa_*` and
`__aeabi_unwind_cpp_pr*` half of the ARM C++ ABI but not GCC's personality routine, and
with exceptions on, every `TRAP` needs it.

**`rcomp` shells out to `uidcrc`** and finds it only on `PATH` — it does not look beside
itself. Without it, `rcomp` prints "Failed to write UIDs" and **exits 0**, leaving a `.rsc`
that exists and is unusable.

**`bmconv` parses DOS-style options**, so it mistakes any argument beginning with `/` for
one. An absolute Unix path silently becomes garbage; everything handed to it must be
relative. The depth option is also glued to the filename (`/c24foo.bmp`).

**Fedora blocks SHA-1 signatures** (`rh-allow-sha1-signatures = no`), but Symbian 9.x SIS
signatures are RSA-with-SHA-1 and the device accepts nothing else.
`OPENSSL_ENABLE_SHA1_SIGNATURES=1` opts back in — only relevant with `SIGN=1`.

## Transport

**The E72 tears down its Bluetooth ACL link after each transfer** and stops answering SDP,
so the *next* push fails with `org.bluez.obex.Error.Failed: Unable to find service record`
— which reads as though the phone has no OBEX at all. A `bluetoothctl connect` refills the
SDP cache; `btpush.py` does that automatically now, and tolerates the connect itself
failing with `br-connection-page-timeout`, since the push often works anyway.

**The registration resource must go to `\private\10003a3f\import\apps\`.** 10003a3f is
AppArc's own SID, not yours, and `import` is its writable drop-box. Installing to
`\private\10003a3f\apps\` instead works on the emulator and fails silently on a device:
the classic "installs fine but never shows up".

## libopus decodes on this handset with no C library at all

**Measured**, by linking the decode path on its own and reading what was left undefined —
not by reasoning about what fixed-point ought to avoid:

```
$ arm-none-symbianelf-ld -r --gc-sections -e run opusref.o libopus.a -o out.o
$ arm-none-symbianelf-nm -u out.o
  U __aeabi_idiv  U __aeabi_idivmod  U __aeabi_uidiv
  U free  U malloc
  U memcpy  U memmove  U memset
$ arm-none-symbianelf-size out.o
   text 109666
```

Three findings, each of which changed a decision:

**No libm.** Not one floating-point function survives `FIXED_POINT=1` plus
`DISABLE_FLOAT_API`. This mattered because there is no C library here at all — the cross
compiler ships only the freestanding headers (`stddef.h`, `stdarg.h`, `limits.h`,
`float.h`) and `sdk/epoc32/include` is Symbian's C++ API. `vendor/libopus/compat/math.h`
therefore *declares* `sqrt`, `pow` and the rest and deliberately defines none of them: a
hand-written `sqrt` that is slightly wrong yields audio that plays and sounds bad, which
is far harder to attribute than a missing symbol. The link proves none is reachable.

**107 KB of text** for the whole decode path after `--gc-sections`. That is the real
price of playing voice messages, and it is worth stating because the naive check does not
show it: linking the client against `libopus.a` without calling anything adds *nothing*,
because the linker drops it all. `nm | grep -c opus` on the binary read 0 while the build
looked like a success.

**`malloc` and `free` are still referenced** despite `opus_decoder_init` into caller-owned
memory and `VAR_ARRAYS` for scratch. They are defined in `shim_alloc.cpp` against the same
`User::Alloc` heap as everything else, because a second allocator would make the handset's
memory figures meaningless. `calloc` is *not* defined — the link does not ask for it, and
a missing symbol names its caller while a silently satisfied one does not.

The gotcha that cost a build: `-DOPUS_ARM_ASM=0` does **not** disable the ARM assembly.
Upstream guards on `#if defined(OPUS_ARM_ASM)`, so defining it to zero switches the
assembly *on* and then fails to find headers. It must be absent.

## A package with no registration resource installs and never appears

`audioprobe` built, packaged, transferred over Bluetooth and installed — reporting success
at every step — and was not in the menu. The cause was an absent
`data/<name>_reg.rss`, so no `_reg.rsc` was compiled into
`\private\10003a3f\import\apps\`, which is the exact path that puts an application in the
S60 menu.

The failure is silent on both sides, which is what makes it expensive: nothing in the
build, the package or the installer says anything is missing. `tools/symbuild` now refuses
to build without one. Same symptom, different cause, already documented in the shipped
`.rss` comments: installing to `\private\10003a3f\apps\` instead of `import\apps\` works
on the emulator and fails silently on a device.

## Audio playback, measured

Row A of `examples/audioprobe` on the E72: an 8 kHz mono PCM16 WAV written by the
application opened, reported its duration as 900 ms against a 900 ms clip, played, and was
heard. So the platform did not substitute the sample rate, and
`CMdaAudioPlayerUtility::OpenFileL` accepts a RIFF/WAVE file this repository generates.

Two things the SDK's own examples do not say, both from the reference and both load
bearing:

- **`Stop()` does not deliver `MapcPlayComplete`.** A state machine that waits for the
  callback after stopping waits forever with no error. `shim_audio.cpp` pushes the event
  at the call site instead; both shipped Nokia examples set their state there too.
- **The default priority preference is `EMdaPriorityPreferenceTimeAndQuality`, which
  fails** rather than degrades when something else holds the device. A ringtone would turn
  a voice message into silence with no message. The shim asks for
  `EMdaPriorityPreferenceTime`, which permits degraded output — the right trade for
  speech, and what `AudioStreamExample` uses.

Playback needs **no capability**. `UserEnvironment` is for recording; `MultimediaDD`
appears on the priority-taking overloads but its own documentation says it grants
*precedence*, not access, and the SDK's `CLFExample` plays audio under `capability none`.
