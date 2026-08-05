# The Rust target spec, field by field

`armv5te-symbian-eabi.json` is not a guess. Every value below was checked by
building `core` + `alloc` against it and reading the resulting ARM objects with
`readelf`, on `rustc 1.99.0-nightly` / LLVM 22. The non-obvious entries are the
ones worth defending.

## Why the triple says `none-eabi` and not `symbianelf`

LLVM has no idea Symbian exists. The `OSType` enum in
`llvm/include/llvm/TargetParser/Triple.h` has no Symbian entry, and never did —
`arm*-*-symbianelf*` is a **GCC** target (`gcc/config.gcc` + `gcc/config/arm/symbian.h`),
which is exactly why our GCC 15.2 cross-compiler builds and why clang refuses:

```
clang: error: version 'symbianelf' in target triple 'arm-unknown-none-symbianelf' is invalid
```

LLVM parses `symbianelf` as an OS *version* string. `rustc` is more permissive
than clang's driver and will accept it, silently falling back to unknown-OS/EABI
defaults — the output is byte-identical to `armv5te-none-eabi`. So the Symbian
string buys nothing and misleads the next reader. The Symbian-ness lives in the
ELF's `e_flags` and in our linker, not in the triple.

## The three fields that will bite you if you copy an upstream spec

**`c-enum-min-bits: 32`** — upstream `armv5te-none-eabi` uses `8` (short enums).
`gcc/config/arm/symbian.h` forces the opposite via `CC1_SPEC`:
`%{!fshort-enums:%{!fno-short-enums:-fno-short-enums}}`. Symbian enums are
int-sized. Leaving this at `8` silently corrupts every `#[repr(C)] enum` crossing
the FFI boundary — no warning, no link error, just wrong values.

**`relocation-model: "static"`** — `pic` is a writable-static-data violation.
Under `pic`, a `static PTR: &[u32] = &RO;` lands in `.data.rel.ro` (flags `WA`)
instead of `.rodata` (flags `A`), and introduces `R_ARM_GOT_PREL`. On Linux that
is fine because RELRO mprotects the section after relocation. On Symbian,
`elf2e32` rejects the image outright — see `docs/wsd.md`. E32 images are
relocated at load anyway, so `static` is both correct and sufficient.

**`max-atomic-width: 0` / `atomic-cas: false`** — ARMv5TE has no `LDREX`/`STREX`.
The useful side effect is that `core::sync::atomic::AtomicU32` ceases to exist,
which closes the most common accidental route to a `.bss` section at the type
level.

## Why ARMv5TE and not ARMv6

The E72's ARM1136JF-S is genuinely ARMv6 with VFPv2 in hardware, and an
`armv6-none-eabi` spec does build and emit real `ldrex`/`strex`. We target v5TE
anyway, for three independent reasons:

1. `elf2e32` hardcodes the CPU field: `iHdr->iCpuIdentifier = (uint16)ECpuArmV5`.
2. `symbian.h` sets `TARGET_DEFAULT_WORD_RELOCATIONS 1` — Symbian wants literal
   pool addressing, never `movw`/`movt` pairs, whose `R_ARM_MOVW_ABS_NC` /
   `R_ARM_MOVT_ABS` relocations `elf2e32` does not accept. v5TE and v6 both still
   use literal pools; ARMv7+ does not, and would break.
3. The SDK import libraries we link against live in `epoc32/release/armv5/lib/`.

Revisit only with a measured need for the atomics, and re-check the relocation
census if you do.

## Other notable choices

| Field | Value | Why |
| --- | --- | --- |
| `features` | `+soft-float,+strict-align` | `+strict-align` sets `Tag_CPU_unaligned_access = Not Permitted`. ARMv5 faults on unaligned word access. |
| `abi` + `llvm-floatabi` | `eabi` + `soft` | rustc enforces consistency; omitting either fails the build. Yields `EF_ARM_ABI_FLOAT_SOFT`, matching `elf2e32 --fpu=softvfp`. |
| `frame-pointer` | `may-omit` | Upstream bare-ARM specs use `always`, costing a push/mov in every leaf. We have an 8 KB stack; the frame pointer is a luxury. |
| `has-thread-local` | `false` | Makes `#[thread_local]` a compile error instead of emitting writable `.tdata`/`.tbss`. Symbian has no ELF TLS — use `Dll::Tls()` from the shim. |
| `default-visibility` | `hidden` | Matches symbian.h. Keeps ~500 internal Rust symbols out of the export table; only `#[no_mangle] extern "C"` items get out. |
| `eh-frame-header` | `false` | Non-default (upstream defaults to `true`). Suppresses `.eh_frame_hdr`. |
| `code-model`, `disable-redzone` | omitted | Meaningless on ARM32 / x86-only respectively. |

`singlethread` stays `false`: Symbian is preemptively multithreaded, and `true`
would let LLVM elide memory barriers we actually need.

## ABI compatibility with our binutils

This is where building a modern cross-toolchain paid off. LLVM 22 emits
`e_flags = 0x05000200` (`EF_ARM_EABI_VER5` + `ABI_FLOAT_SOFT`), and binutils 2.45
emits VER5 too, so they merge cleanly. Had we used the CodeSourcery 2005 toolchain
that GnuPoc documents (binutils 2.16, `EABI_VER4`), `elf32_arm_merge_private_bfd_data`
would have hard-errored on the version mismatch and `.ARM.attributes` would have
been an unrecognised section — fixable with an ELF byte-poke plus
`objcopy --remove-section`, but not something to live with.

## Building

Rust is compiled to a **static archive** and never links. `arm-none-symbianelf-ld`
owns layout, `.ARM.exidx` merging, the `Symbian$$CPP$$Exception$$Descriptor`
symbol and `.dso` import resolution; `elf2e32` then produces the E32 image. That
keeps LLVM's Symbian ignorance confined to the object boundary.

```
cargo +nightly build --release \
  -Zjson-target-spec \
  -Zbuild-std=core,alloc \
  -Zbuild-std-features=compiler-builtins-mem \
  --target targets/armv5te-symbian-eabi.json
```

`-Zjson-target-spec` is newly mandatory — JSON target specs were destabilised, and
without it cargo refuses:

```
error: `.json` target specs require -Zjson-target-spec to be added to the cargo invocation
```

It is a **cargo** flag only; bare `rustc` wants `-Zunstable-options --target foo.json`.

`compiler-builtins-mem` is required, not optional: without it `memcpy`, `memset`,
`memmove` and `memcmp` are undefined. With it they are emitted `FUNC WEAK HIDDEN`,
so they will not collide with `memcpy` from `euser`/`scppnwdl` or Open C's
`libc.dll` at final link.

## Measured cost

A real `alloc`-using function (`Vec::sort_unstable` + `String::push` + slice math),
`panic=immediate-abort`, `--gc-sections`:

```
.text        4128 bytes
.ARM.exidx     16 bytes
```

~4 KB for working Rust with heap collections. The unlinked archive is 819 KB,
almost all of it `compiler_builtins` soft-float routines for `f16`/`f128` that sit
in their own sections and never get pulled — `function-sections` plus
`--gc-sections` is doing real work, so do not disable either.

## Stack budget — the one real trap

Default main-thread stack is 8 KB (`elf2e32`'s `iStackSize = 0x2000`). Ranking all
1233 functions by frame size with `-Zemit-stack-sizes` gives a median frame of
**8 bytes** — `core` is extremely frugal — with one glaring exception:

```
1520 B  core::num::flt2dec::strategy::grisu::format_shortest
1184 B  core::fmt::float::float_to_exponential_common_exact::<f64>
1168 B  core::fmt::float::float_to_decimal_common_exact::<f64>
 968 B  core::num::flt2dec::strategy::grisu::format_exact
```

Formatting one `f64` with `{}` costs ~1.7 KB across two frames — 21% of the stack
on a single `write!`. So: **do not format floats in Rust here.** Return the value
and format it in C++ with `TRealFormat`, or stay in fixed point. Integer and slice
formatting are cheap.

`panic = "immediate-abort"` removes `grisu`/`flt2dec` from the image entirely and
drops the worst-case frame to ~250 B. It needs `cargo-features = ["panic-immediate-abort"]`
as the first line of `Cargo.toml` and `"panic-strategy": "immediate-abort"` in the
JSON. Adopt it once panic location strings stop being useful.

There are no stack probes and no guard-page recovery on this target: overflow is a
silent `KERN-EXEC 3`. Budget conservatively, and raise `EPOCSTACKSIZE` to `0x8000`
if any float formatting survives.
