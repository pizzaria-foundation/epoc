# A Rust SDK for Symbian S60 3rd Edition FP2

Target hardware: **Nokia E72** — Symbian OS 9.3, S60 3rd Ed FP2, ARM1136JF-S at
600 MHz, 320×240 landscape QVGA, hardware QWERTY, no touchscreen, ~45 MB of free
RAM. The development handset has a patched installserver, so packages install
unsigned and are not limited to user-grantable capabilities.

Host: aarch64 Fedora. Everything here builds and runs natively — no Wine, no
x86 emulation, no Windows.

## What is proven to work

The host pipeline runs end to end. This was verified by building an actual E32
image and a signed package, not by reading documentation:

```
C++  --arm-none-symbianelf-g++ 15.2.0-->  ARM ELF   Flags: 0x5000200
                                                    (EABI v5, soft-float)
ELF  --ld 2.45 -shared --target1-abs-->   app.elf
ELF  --elf2e32------------------------>   app.exe   uid1=0x1000007a "EPOC"
exe  --makesis------------------------>   app.sis
sis  --signsis------------------------>   app.sisx  cert reads back, 10y validity
```

The `0x5000200` e_flags matter more than they look: they are byte-identical to
what rustc/LLVM 22 emits for the target spec in `targets/`, which is what makes
Rust and C++ objects linkable together. Building a modern binutils rather than
using the CodeSourcery 2005 toolchain GnuPoc documents is what bought that —
2005-era binutils emits EABI v4 and would have refused the merge.

## What is not proven yet

- **Linking against the SDK's `.dso` import libraries.** The S60 3.2 SDK is still
  downloading; nothing has been linked against `euser.dso` yet.
- **Rust in the loop.** The target spec was validated by a research pass that
  built `core` + `alloc` and inspected the objects (zero writable sections, only
  `R_ARM_ABS32` reaching elf2e32), but no Rust staticlib has gone through
  `symbuild` here.
- **Anything on the device.** No binary has run on the E72.

Closing those three, in that order, is the next work.

## Layout

```
toolchain/
  host/bin/            15 native aarch64 tools: elf2e32, makesis, signsis,
                       rcomp, bmconv, mifconv, uidcrc, elftran, genstubs, ...
  cross/bin/           arm-none-symbianelf GCC 15.2.0 + binutils 2.45
  build-cross-gcc.sh   reproducible cross-toolchain build
  patches/             why binutils needed patching, and what changed
targets/
  armv5te-symbian-eabi.json   the Rust target spec
  README.md            every field defended, with measurements
tools/
  symbuild             sources -> installable .sis, one script
  mkfont.py            TTF -> .sbf glyph atlas
  preview/             renders screens to PNG so the UI can be reviewed
crates/
  symbian-gfx/         no_std software rasterizer
  symbian-ui/          widget toolkit + .sbf atlases
apps/telegram/          the client, UI-first against mock data
vendor/                 GnuPoc + the S60 3.2 SDK zip
docs/                   research notes
preview-out/            generated screenshots
```

82 tests across the workspace: `cargo test --workspace`.

## Toolchain provenance

Two upstream retirements had to be worked around.

**binutils dropped `arm-none-symbianelf`** — the triple sits in the
obsolete-targets list in `bfd/config.bfd`, and gas/ld never learned it back. GCC
*still* carries the target (`gcc/config.gcc` → `arm/symbian.h` + `t-symbian`),
which is where `__SYMBIAN32__`, the Symbian dllimport/dllexport semantics and
"don't assume GCC's libstdc++" come from. So `build-cross-gcc.sh` re-adds the
triple to binutils as an alias for the plain little-endian ARM EABI config. We do
*not* resurrect BFD's old `elf32-littlearm-symbian` vector: its job was emitting a
program-header-less image for a postlinker, and in this pipeline that role belongs
to elf2e32, which reads ordinary ARM ELF through libelf.

Because of that aliasing, `ld` no longer enables `--target1-abs` implicitly the
way it does for a real symbianelf target. `symbuild` passes it explicitly. Getting
this wrong yields `R_ARM_REL32` where Symbian needs `R_ARM_ABS32`, and elf2e32
silently drops the relocation.

**Sourcery's download portal is gone**, and the 2005/2011 sources GnuPoc expects
no longer build against a modern host compiler. Hence upstream GCC 15.2.0 +
binutils 2.45 from source. Three fixes were needed along the way:

- GCC 15's `libcody` predates GCC 16's default dialect, where `u8""` literals
  became `char8_t`. Fixed by pinning the host compiler to `-std=gnu++17`.
- `arm/t-symbian` asks for a `softfp` multilib. `-mfloat-abi=softfp` means
  "soft-float calling convention, hardware FP permitted", which is incoherent on
  armv5te with no FPU — and GCC 15 answers with an ICE in `arm_init_builtin`
  rather than a diagnostic. Fixed with `--disable-multilib`; we only ever want
  pure soft-float.
- `libgcc`'s coverage component fails to compile (`__INTPTR_TYPE__` is not
  defined for this target). Since we do not want libgcov, `libgcc.a` is built as
  a direct make target and installed by hand. It carries the `__aeabi_*` helpers
  — `idiv`, `uidiv`, `uidivmod`, `ldivmod`, `dadd`, `ddiv`, `l2d`,
  `unwind_cpp_pr0` — which is all that was wanted.

The GnuPoc host tools needed porting too: `bmconv` had an ordered
pointer-vs-zero comparison GCC 16 rejects, and `makesis`/`signsis` were written
against OpenSSL 1.0's transparent structs. The OpenSSL 3 port replaces
stack-allocated `EVP_MD_CTX`/`X509_OBJECT` with the heap accessors, swaps the
removed `EVP_dss1()` for `EVP_sha1()` (which is what it always was), drops
`ERR_GET_FUNC`, and reduces the `X509_OBJECT_free_contents` dance to a plain
`X509_free`.

## Signing

Off by default — the dev phone's patched installserver takes unsigned packages
and lifts the capability ceiling. Set `SIGN=1` in `app.conf` for a stock handset;
`symbuild` then mints a 10-year self-signed RSA-2048 certificate on first use.

One trap if you do sign on Fedora or RHEL: the system crypto policy sets
`rh-allow-sha1-signatures = no`, and Symbian 9.x SIS signatures are
RSA-with-SHA-1 (`1.2.840.113549.1.1.5`) with no alternative the device accepts.
`symbuild` exports `OPENSSL_ENABLE_SHA1_SIGNATURES=1`, which covers both
`openssl req` and signsis, since they share libcrypto.

## Architecture

**Rust compiles to a static archive and never links.** `arm-none-symbianelf-ld`
owns layout, `.ARM.exidx` merging, the `Symbian$$CPP$$Exception$$Descriptor`
symbol and `.dso` import resolution. This keeps LLVM's total ignorance of Symbian
confined to the object boundary — see `targets/README.md`.

**Ship as an EXE, not a DLL.** elf2e32 rejects a DLL with any writable data:

```cpp
if (isDllp) {
    if (!AllowDllData()) {
        if (iHdr->iDataSize) throw Elf2e32Error(DLLHASINITIALISEDDATAERROR, ...);
        if (iHdr->iBssSize)  throw Elf2e32Error(DLLHASUNINITIALISEDDATAERROR, ...);
    }
}
```

The check is entirely inside `if (isDllp)`, so EXEs are unrestricted. Worth
knowing that a `no_std` Rust build would actually pass it anyway — `core` and
`alloc` contain no `.data`, `.bss`, `.tdata` or `.tbss` at all — but an EXE means
the question never arises, and gives us `EPOCSTACKSIZE`/`EPOCHEAPSIZE` control
that DLLs do not get.

**A Symbian Leave must never cross into Rust.** On 9.x, `User::Leave` is a
longjmp-style unwind that does not run destructors; that is why `CleanupStack`
exists. Crossing Rust frames compiled `panic=abort` — which have no landing pads
— skips every `Drop` and is undefined behaviour, not merely a leak. So every
`extern "C"` shim function is a TRAP barrier returning a `TInt` error code, and
the leaving implementation stays private to C++:

```cpp
extern "C" int32_t shim_socket_connect(int32_t h, uint32_t ip, uint16_t port) {
    TInt err = KErrNone;
    TRAP(err, DoSocketConnectL(h, ip, port));
    return err;
}
```

Correspondingly the Rust allocator must call the non-leaving `User::Alloc`, never
`User::AllocL`, so that OOM never becomes a throw crossing Rust frames.

**Rust cannot own the event loop.** The Avkon framework calls
`CActiveScheduler::Start()`, and there is no taking that away. Symbian I/O is all
asynchronous through active objects, so the design inverts: each
`CActive::RunL()` converts its completion into a POD event and pushes it onto a
ring buffer, and a `CIdle`/`CPeriodic` pump calls `rust_step()`, which drains via
`shim_poll_event()`. That shape maps cleanly onto a `winit`-style handler and lets
a `no_std` executor run inside `rust_step()`.

Never poll `TRequestStatus` directly from the shim. Reading the word is safe, but
it does not consume the thread's semaphore signal, and every mismatch leaves a
stray signal that corrupts the next wait. Let the scheduler consume it, and poll
the ring buffer instead.

One pleasant exception: `RFile::Read`/`Write` have genuine synchronous overloads
that need no active scheduler, so file I/O can be a plain blocking C ABI with no
event plumbing.

## symbian-gfx

`no_std`, `forbid(unsafe_code)`, no interior mutability, no statics — which is
what keeps the object free of writable sections. It knows about pixels and
glyphs and nothing about Symbian, so the same code runs on the host and the
widget layer above it can be developed without a device.

- RGB565 blend that interleaves red/blue into one 32-bit lane and green into
  another, so each pixel costs two multiply-adds instead of six.
- Clip/translate stack, so a widget drawing at its own (0,0) cannot escape its
  box even when it miscalculates. Negative coordinates clip rather than wrap.
- Explicit stride, because Symbian aligns `CFbsBitmap` scanlines to 4 bytes and a
  320-pixel-wide bitmap is not guaranteed to have `stride == width`.
- Integer-only geometry throughout. `core` has no `f32::sqrt`, and float code is
  worth avoiding here regardless: formatting a single `f64` costs ~1.7 KB of
  stack against an 8 KB default.
- `.sbf` bitmap font atlas, fully validated once at construction so glyph lookup
  is a binary search with no re-checking.

## symbian-ui

There is no retained widget tree here and no `Box<dyn View>`. A screen is a plain
struct that owns its state, handles a key event, and draws — and the toolkit
supplies the parts that are genuinely hard: scrolling and selection arithmetic
(`list`), char-boundary-safe editing (`edit`), and the screen furniture
(`chrome`).

That is a deliberate trade. A widget tree buys composition and costs allocation,
trait-object indirection and a focus-traversal system. On a screen showing five
rows at a time with one D-pad driving everything, composition is not the problem
— arithmetic is. Splitting the arithmetic out and testing it catches the bugs that
actually happen: a scrollbar thumb one pixel past its track, a caret landing
inside a Cyrillic character, a list still selecting row 19 after the list shrank
to three rows. That last one was a real defect, found by a test written before the
device existed.

It also matches Symbian, where the framework owns `CActiveScheduler::Start()` and
Rust is always a callee. `handle_key` and `draw` are exactly what the shim will
call.

## Fonts

`tools/mkfont.py` rasterizes a TTF into the `.sbf` atlas. Coverage is read from
the font's cmap via fontTools, not by asking Pillow whether it drew anything —
for a missing codepoint most fonts happily return the `.notdef` box, which has ink
and would sail into the atlas as a tofu glyph. It did, the first time, and the
delivery ticks rendered as ▯▯.

`--font` repeats to form a fallback chain; the first font with a given codepoint
wins, and the first font listed sets the vertical metrics. Noto Sans has no
U+2713, so the ticks come from Noto Sans Symbols 2:

```
python3 tools/mkfont.py \
  --font /usr/share/fonts/google-noto/NotoSans-Regular.ttf \
  --font /usr/share/fonts/google-noto/NotoSansSymbols2-Regular.ttf \
  --size 12 --out crates/symbian-ui/assets/ui12.sbf
```

551 glyphs at 12px is ~47 KB — Latin, Latin Extended-A, Greek, Cyrillic and the
symbols the widgets draw. The 27 codepoints it reports as absent are unassigned
Unicode positions in the Greek block, which is correct.

On device the system `CFont` is the better source: real metrics, full UCS-2, zero
bytes shipped. `Font` is a trait for exactly that reason — the atlas is what makes
the host preview possible.

## Seeing the UI without a device

```
cargo run -p preview        # writes preview-out/*.png at 2x
```

`tools/preview` drives the real `App` through synthetic key events and writes PNGs
with a dependency-free encoder (stored deflate blocks — a screenshot does not need
compression). Being able to look at a screen caught things tests did not: the tofu
ticks, and a transcript that sat at the top of the viewport instead of hanging
from the composer the way every chat client does.

## Open questions that need the device

- **Display mode.** The panel is 24-bit, so the Symbian mode is probably
  `EColor16MU` (32bpp) rather than `EColor64K` (16bpp RGB565). This is unconfirmed
  and must be queried with `CWsScreenDevice::DisplayMode()`, never assumed. If it
  is 16MU, the canvas needs a second pixel format or every present pays a
  conversion blit in the window server. Probe the byte order too, by filling a
  1×1 bitmap with `TRgb(255,0,0)` and reading the bytes back.
- **`CFbsBitmap::DataAddress()` needs `BeginDataAccess()` first** on 9.1+, or it
  crashes; the pointer is only valid until `EndDataAccess()` because the font and
  bitmap server heap can compact. Re-fetch after every lock.
- **Softkeys.** With `ENoScreenFurniture` there is no CBA, so softkey presses
  should reach our control — but Avkon may still intercept the right softkey.
  FP2 also adds a middle softkey, so design for three, not two.
- **No SHA-256** in S60v3's `hash.h` (SHA-1 and MD5 only); implement it in Rust.
