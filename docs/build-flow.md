# The build flow

Two pipelines: the toolchain, built once, and the per-app build that runs every
time. Every box marked `(*)` exists because of a divergence between what GNU
tooling produces and what Symbian expects — each one cost a failed install to
find, since the device refuses a malformed image by doing nothing at all.

## 1. The toolchain (one time, ~30 min)

```
  ftp.gnu.org                     archive.org                github.com
       |                               |                          |
  binutils 2.45                 S60-3.2-SDK.zip            gnupoc-package
  gcc 15.2.0                     (460 MB)                        |
       |                               |                          |
       v                               v                          v
  +----------------+          +-----------------+       +-------------------+
  | patch: restore |          | unshield (*)    |       | build host tools  |
  | arm-none-      |          | built from src  |       |  elf2e32          |
  | symbianelf (*) |          | for aarch64     |       |  makesis/signsis  |
  |                |          |                 |       |  rcomp, bmconv    |
  | + newlib-      |          | InstallShield   |       |                   |
  |   stdint.h (*) |          | -> epoc32/      |       | ported to         |
  +----------------+          +-----------------+       | OpenSSL 3 (*)     |
       |                               |                +-------------------+
       v                               v                          |
  toolchain/cross/            sdk/epoc32/                toolchain/host/bin/
  arm-none-symbianelf-*        include/  (1718 headers)   15 native tools
  + libgcc.a                   release/armv5/lib/  (690 .dso)
  + libsupc++.a (*)            release/armv5/urel/ (eexe.lib, usrt2_2.lib)
```

**Why each `(*)`:**

- **binutils lost the triple.** `arm*-*-symbianelf*` sits in the obsolete list in
  `bfd/config.bfd`. GCC still has the target (`arm/symbian.h` + `t-symbian`), so
  we re-add it to binutils as an alias for plain ARM EABI. BFD's old
  `elf32-littlearm-symbian` vector is not needed: its job was emitting a
  postlinker-friendly image, and here that is elf2e32's job.

- **No stdint for this target.** `arm*-*-eabi*` gets `newlib-stdint.h` and
  `use_gcc_stdint=provide`; symbianelf gets neither, so `__INTPTR_TYPE__` is never
  predefined and neither libgcc's coverage driver nor libstdc++'s `<functional>`
  compiles. Two lines in `config.gcc`.

- **libsupc++** supplies `__gxx_personality_v0`. Symbian's `drtaeabi.dso` covers
  the `__cxa_*` and `__aeabi_unwind_cpp_pr*` half of the ARM C++ ABI but not GCC's
  personality routine — and on Symbian 9.x a `User::Leave` *is* a C++ throw, so
  every `TRAP` needs it.

- **unshield** ships only as an x86 binary in GnuPoc. Building it for aarch64
  needed a `--build=` (its 2009 `config.guess` predates aarch64) and
  `-DPROTOTYPES=1` (the RFC 1321 MD5 uses K&R prototypes, which GCC 16 reads as
  `(void)` under C23).

- **makesis/signsis** were written against OpenSSL 1.0's transparent structs.

## 2. Per-app build

```
   src/*.cpp            crate/ (Rust)            data/*.rss      data/*.bmp
       |                     |                        |               |
       | g++ -c              | cargo build            | cpp           | bmconv (*)
       | -march=armv5t       | --target               | + rcomp       |
       | -D__SUPPORT_CPP_    |   armv5te-symbian      |               |
       |    EXCEPTIONS__ (*) |   -eabi.json           |               |
       v                     v                        v               v
     *.o              libapp.a                     *.rsc         icon.mbm
       |                     |                        |               |
       +----------+----------+                        |               |
                  |                                   |               |
                  v                                   |               |
   +--------------------------------+                 |               |
   | arm-none-symbianelf-ld         |                 |               |
   |   -shared --target1-abs        |                 |               |
   |   -T symbian-exe.lds (*)       |                 |               |
   |   eexe.lib  (_E32Startup)      |                 |               |
   |   usrt2_2.lib                  |                 |               |
   |   -l:euser.dso ... -lsupc++    |                 |               |
   +--------------------------------+                 |               |
                  |                                   |               |
              app.elf                                 |               |
                  |                                   |               |
                  | e32prep.py (*)                    |               |
                  |   clear SHF_ALLOC on .dynsym etc. |               |
                  |   zero .got                       |               |
                  v                                   |               |
              app.elf'                                |               |
                  |                                   |               |
                  | elf2e32                           |               |
                  |   --targettype=EXE --uid3=...     |               |
                  v                                   |               |
              app.exe   <- E32Image                   |               |
                  |                                   |               |
                  | e32dump.py --quiet  (build gate)  |               |
                  |   UID checksum, header CRC,       |               |
                  |   codeBase, import addends        |               |
                  v                                   v               v
                  +-----------------+-----------------+---------------+
                                    |
                                    | makesis
                                    v
                                 app.sis
                                    |
                                    | (SIGN=1 -> signsis; off by default,
                                    |  the handset has a patched installserver)
                                    v
                          epoc sideload -> phone
```

## 3. The four things that actually break

Ordered by how long each took to find.

### The linker script is not optional

A `-T` script replaces ld's built-in `SECTIONS` outright, even when it contains
only symbol assignments. With one input object, orphan placement happens to
produce something that looks right — so an assignments-only script appears to
work, then collapses the moment a second object appears and `.text.foo`,
`.emb_text` and a dozen `.ARM.exidx.text.*` scatter across mismatched segments.

The script must also supply four symbols GNU ld has no concept of:

```
  Image$$ER_RO$$Base    \
  Image$$ER_RO$$Limit    |  RVCT's linker synthesises these. They are the four
  .ARM.exidx$$Base       |  words of Symbian$$CPP$$Exception$$Descriptor, whose
  .ARM.exidx$$Limit     /   address goes into the E32 header for the unwinder.
```

Not cosmetic: a Leave is a throw, so a wrong exception range breaks every TRAP.

### elf2e32 puts every allocated section in the code segment

```
  before:  [.hash][.dynsym][.dynstr 9.5KB][.rel.dyn][.text 10KB][.rodata]
           \___________ 18 KB of ELF metadata ____________/
                        all of it baked into the executable image

  after:   [.text 10KB][.rodata][.ARM.ex*][.got]     codeSize 31384 -> 12808
```

A genuine `euser.dso` has `ER_RO` as its *only* allocated section. Clearing the
ALLOC bit fixes it — but the sections must stay in the file, because elf2e32
reads `.dynsym` and `.rel.*` to build the import and relocation tables.

Parking them *after* the code matters too: left in front, they keep consuming
address space and push `codeBase` from `0x8000` to wherever `.text` lands.

### The GOT poisons every import

Symbian encodes an import as `(addend << 16) | ordinal`, taking the addend from
whatever word already sits at the import site.

```
  RVCT:     import site is a literal-pool slot containing 0
            -> 0x00000e44   addend 0, ordinal 3652        correct

  GNU ld:   .got entry pre-filled with the PLT stub's address for lazy binding
            -> 0xa3ac000e   addend 0xa3ac, ordinal 14     garbage
```

81 of 230 imports resolved to the wrong address. Symbian binds everything at
load and never uses lazy resolution, so zeroing the GOT is free — the loader
writes the real address into the slot the stub reads.

### objcopy is not safe here

The first attempt at both fixes used `objcopy --set-section-flags`. It
reclassified `.rel.plt` and silently discarded all 82 of its `R_ARM_JUMP_SLOT`
relocations — 82 imports gone, `dllRefTableCount` 12 -> 6, no warning.
`tools/e32prep.py` patches the section header table byte by byte instead: it
changes exactly the fields named and nothing else.

## 4. Why the validator runs inside the build

The device gives no diagnostic. A malformed E32 produces no error, no panic and
no log entry — you tap the icon and nothing happens. Every check that can run on
the host saves a full install round trip, so `e32dump.py --quiet` gates the
build and re-derives the UID checksum and header CRC independently of elf2e32,
which is how you catch the tool itself being wrong rather than just its input.

It is also run against a real Symbian binary carved out of the SDK's own example
SIS files with `tools/sisextract.py`. A validator that rejects ground truth is
broken, so that is its self-test.
