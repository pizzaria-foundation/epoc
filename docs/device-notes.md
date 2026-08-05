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

## The keyboard

This one took three probe builds, and each round the wrong thing was blamed.

**The twelve overlaid keypad keys arrive as digit scan codes.** The E72 prints a phone
keypad over the letters — `1 2 3` on R T Y, `4 5 6` on F G H, `7 8 9` on V B N, `* 0 #` on
U M J — and the window server identifies those physical keys *as the digit keys*. Pressing
R gives `iScanCode == 0x31`, the scan code of `'1'`, not of `'R'`.

It is not a mistranslation. At the window server's level, that key **is** the 1 key. The
letter identity is applied above it by Avkon's FEP, from the input mode of the focused
editor.

**We cannot get that.** `TCoeInputCapabilities::EAllText` is not enough — tried on device,
no change — and `CAknEdwinState`, the class that actually carries the input mode, is not in
the public SDK. Neither is `EAknEditorTextInputMode` as a C++ enum; it exists only in
`eikon.rh` as a resource constant. So even a full `MCoeFepAwareTextEditor` implementation,
twelve pure virtuals of it, could not say "alphabetic".

So the shim translates, and the trigger is self-identifying and needs no state: for a
letter key the window server *does* translate, so `iCode` differs from `iScanCode` (`'e'`
0x65 vs `'E'` 0x45). For these twelve it does not. `iCode == iScanCode` plus a scan code in
the table identifies exactly those twelve, and a device without the overlay is unaffected
because its scan codes never match.

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
