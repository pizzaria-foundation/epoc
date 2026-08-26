# F5 — cross-compiling the NetSurf MIT libraries for armv5

> Status: **it links.** 444 translation units, five archives, one `.sis`. Nothing has run on a
> handset — no device access in this workstream — so every claim below is a host measurement.
> `docs/plan-browser.md` F5, risk R1.

## The answer to R1

R1 said: the NetSurf libraries need `malloc`, `str*` and `snprintf`; Open C's `libc` loads on the
E72 (measured, `docs/device-notes.md:799`) but that is a property of the handset and the SDK
headers may not match. Plan B was a minimal libc over `User::Alloc`.

**Plan B is not needed, and the margin is not thin.** The five archives leave 29 symbols
undefined between them:

| | count | where they come from |
|---|---|---|
| `__aeabi_idiv` `__aeabi_idivmod` `__aeabi_uidiv` `__aeabi_uidivmod` `__aeabi_ldivmod` | 5 | libgcc, already on the link line |
| `abs bsearch calloc __errno free iconv iconv_close iconv_open malloc memcmp memcpy memmove memset realloc snprintf strdup strlen strncasecmp strncmp strncpy strtol strtoul time tolower` | 24 | `libc.dso` exports all 24 |

Measured with `nm -D sdk/epoc32/release/armv5/lib/libc.dso` (560 exports) against
`nm --undefined-only` over the five archives. No shim, no gap, no minimal libc.

Two things worth naming inside that list:

- **`iconv` resolves.** libparserutils' input filter uses it for any charset its five built-in
  codecs (UTF-8, UTF-16, ASCII, 8859-\*, ext8) do not cover. Upstream's escape hatch is
  `-DWITHOUT_ICONV_FILTER`; it is not taken, because the symbol is there. Whether the handset's
  `iconv` actually carries conversion *tables* for, say, Shift_JIS is a runtime question that no
  link-time flag answers — and note that three of the nine libraries that failed to load in the
  device sweep were `gb2312_shared`, `jisx0201` and `jisx0208`, which is exactly where CJK
  conversion would live. So: the call links, and the CJK answer is probably no. First page in a
  CJK charset settles it.
- **`__assert` does not appear**, which it did on the first pass. `-DSTMTEXPR=1` (see below)
  routes libwapcaplet's assertion macros through statement expressions instead, and `-DNDEBUG`
  removes them. Recorded because it is the kind of difference that looks like a mistake later.

## What was vendored

`vendor/netsurf-symbian/`, one directory per library, commits pinned and licences copied — the
detail is in that directory's `README.md`. All five are MIT.

**R6 is clean and stayed clean.** NetSurf's own `render/` and `content/` are GPL-2.0 and neither
is present; nothing in the tree includes a header from either. The five libraries are separate
MIT projects that NetSurf also uses, which is the only reason any of this is possible in an MIT
repository.

**No source was patched.** Not one line of the five libraries. Everything the build needs is
passed from outside: two `-D`s that upstream's own buildsystem passes, and one prefix header of
ours. That was a deliberate cost — re-pinning a version is then a fetch and a regenerate, with no
patch queue to rebase.

## What fought back

Five things, in the order they cost time.

### 1. `IMPORT_C` is defined nowhere the compiler can find it

Every prototype in `epoc32/include/stdapis` is marked `IMPORT_C`, and `IMPORT_C` lives in
`epoc32/include/gcce/gcce.h`, which the SDK expects to be force-included. So the first
`#include <assert.h>` is:

```
stdapis/assert.h:63:9: error: expected ';' before 'void'
   63 | IMPORT_C void __assert(const char *, const char *, int, const char *);
```

`-include .../gcce/gcce.h` fixes it. `symbuild`'s C++ recipe already did this; the C path is new
and had to learn it. `gcce.h` is `__cplusplus`-aware and works fine as a C prefix.

### 2. `stdbool.h` defines `true` and `false` but not `bool`

Open C's `stdapis/stdbool.h` takes the `__SYMBIAN32__ && !__WINSCW__` branch under GCCE, which
defines `true`, `false` and `__bool_true_false_are_defined` — and never defines `bool`. It was
written when the only GCCE C compiler in view predated C99. All five libraries spell their
booleans `bool`. Measured cost: 28 of libhubbub's 30 sources failed; only libwapcaplet, which
happens not to reach the header, built.

The obvious fix does not work, and this is the part worth remembering: putting the compiler's own
include directory ahead of `stdapis` so GCC's `stdbool.h` wins also puts GCC's `stdint.h` ahead
of Symbian's, and

```
gcc/.../include/stdint.h:40: error: conflicting types for 'int32_t'; have 'long int'
stdapis/sys/stdint.h:48:   note: previous declaration of 'int32_t' with type 'int'
```

`__INT32_TYPE__` is `long int` for this target and Symbian says `int`. Both are 32 bits; they are
not the same type. Symbian's `stdint.h` has to win, so Symbian's `stdbool.h` wins too, so the gap
has to be filled from outside: `vendor/netsurf-symbian/symbian/netsurf-symbian.h`, three lines,
force-included after `gcce.h`.

The include order that results is load-bearing and is spelled out in `tools/build-netsurf` rather
than left to be rediscovered: `-I stdapis`, `-I include`, then `-isystem $GCC_INC` **last**,
because `ld` searches `-I` before `-isystem`.

### 3. Four files that upstream generates and this repo must not

| File | Generator | Why it matters |
|---|---|---|
| `libparserutils/src/charset/aliases.inc` | `perl build/make-aliases.pl` | 52 KB charset alias table |
| `libhubbub/src/tokeniser/entities.inc` | `perl build/make-entities.pl` | 333 KB named-entity table |
| `libhubbub/src/treebuilder/autogenerated-element-type.c` | `gperf` + a `sed` | perfect hash over HTML element names |
| `libcss/src/parse/properties/autogenerated_*.c` | a host C tool over `properties.gen` | **119 files**, one per CSS property |

The libcss one is the one that bites. Without it, libcss compiles perfectly and the link fails on
119 undefined `css__parse_*` symbols with nothing in the message to say a generator was skipped.
The generator is `css_property_parser_gen.c` — a host `main()`, built with the *host* compiler,
run once per line of `properties.gen`.

All four are vendored as output, so building an app needs no perl, no gperf and no host compiler.
`tools/build-netsurf regen` re-runs them when a library is re-pinned; `fetch` parks the libcss
host generator in `libcss/hostgen/` rather than in `src/`, since a target build that has to know
to skip a file is a target build that will one day forget to.

Two of the four are tables, and that is a specific hazard: a build that silently lost `aliases.inc`
or `entities.inc` would still compile and still link, and would answer "unknown charset" and
"unknown entity" to everything. `apps/netsurfprobe` therefore checks a *known* charset and the
*length* of a known element name, not just that a call returned.

`gperf` also needed one non-obvious step: upstream's Makefile pipes its output through
`sed -e 's/^\(const struct element_type_map\)/static \1/'`, because `element-type.c` `#include`s
the generated file. Skip the `sed` and the table gets external linkage; compile the generated file
separately as well (a `find`-based build will) and it is defined twice.

### 4. `_ALIGNED` — 24 "multiple definition" errors that never mention libcss

`libcss/src/stylesheet.h` ends `struct css_rule { ... } _ALIGNED;`. That is meant to be an
attribute macro. Undefined, it is a *variable declaration* — a tentative definition of an object
called `_ALIGNED` in every translation unit that includes the header. Under `-fno-common`, the
default since GCC 10, the link produces one error per property parser:

```
libcss.a(src_parse_properties_border_style.o):(.bss._ALIGNED+0x0):
  multiple definition of `_ALIGNED'; libcss.a(src_stylesheet.o): first defined here
```

NetSurf's buildsystem passes `-D_ALIGNED="__attribute__((aligned))"` and `-DSTMTEXPR=1`
(`buildsystem/makefiles/Makefile.gcc:32`) and the libraries do not survive without the first.
Found by cloning `netsurf-browser/buildsystem` and grepping it — not by reading libcss, which
never mentions where the macro is supposed to come from.

### 5. The public headers are not C++-safe, and that changed the shape of the work

This is the finding with consequences beyond F5.

```
libdom/include/dom/events/keyboard_event.h:91: error: expected ',' or '...' before 'namespace'
        dom_string *namespace, dom_string *type, ...
libcss/include/libcss/computed.h:83:  error: expected ',' or '...' before 'parent'
        const css_computed_style *restrict parent,
```

Six headers under `include/dom/events` name a parameter `namespace`; libcss declares `*restrict`.
Both are legal C99 and hard errors under `g++`. Neither is fixable from outside the vendored tree,
and patching a *header* is the worst place to keep a divergence from upstream.

So the rule is: **C++ never includes a NetSurf header.** `tools/symbuild` grew a `C_SOURCES` key
beside `CXX_SOURCES` — a separate compiler and a separate header set, not a convenience — and an
app that needs both worlds is two files with a hand-written C ABI between them.
`apps/netsurfprobe` is the worked example: `src/netsurf_probe.c` includes the libraries and knows
nothing of Symbian, `src/netsurfprobe.cpp` owns `E32Main` and the report and knows nothing of
NetSurf, and `inc/netsurf_probe.h` is 53 lines — mostly comment — around one struct of two
`const char *` and two `int`.

That boundary is not a cost — it is the same one `symbian-dom` will have to draw for Rust, drawn
early and with a working example behind it.

One smaller trap in the same area: `libdom/bindings/hubbub/parser.h` must be reached as
`<dom/bindings/hubbub/parser.h>`, the path upstream's own Makefile installs to (the vendored tree
has `include/dom/bindings` as a symlink to `../../bindings` so there is only ever one copy).
`"hubbub/parser.h"` resolves instead to *libhubbub's* `parser.h`, whose include guard is already
satisfied — so the preprocessor emits nothing and every `dom_hubbub_*` name is undeclared, with
no message suggesting a wrong path.

## What did not fight back

Worth recording, because these were the expected problems:

- **No autotools.** The libraries are plain C with no `configure`. `tools/build-netsurf` compiles
  every `.c` and calls `ar`. `tools/build-netsurf` is 207 lines including its `fetch` and `regen`
  subcommands, and most of that is comment.
- **No C99 trouble beyond `bool`.** `-std=c99` with Symbian's headers was otherwise clean.
  Designated initialisers, compound literals, `//` comments, `long long`, `<stdint.h>` — all fine.
- **`-Wall` is clean** across all 444 files. Not one warning from library code.
- **No threading, no locale, no `setjmp`.** None of the five reaches for any of them.

## Reproducing it from a clone

`/vendor/` is gitignored, so a clone has none of this. Two files are force-added — the vendor
README and our prefix header — and `tools/build-netsurf` turns them back into a build:

```
tools/build-netsurf fetch      # the five at their pinned commits
tools/build-netsurf regen      # the four generated files (perl, gperf, cc)
tools/build-netsurf            # 444 objects -> five archives
tools/symbuild apps/netsurfprobe
```

Verified by deleting `libwapcaplet` and `libdom` from the vendored tree and refetching: both came
back byte-identical to what had been placed there by hand (`diff -r --no-dereference`), and
`regen` reproduced libcss's 119 property parsers to the same archive size.

## Measurements

Toolchain: `arm-none-symbianelf-gcc 15.2.0`, `-O2 -march=armv5t -msoft-float -std=c99`,
`-ffunction-sections -fdata-sections`.

### Per library

| Library | `.c` files | archive | text | data | bss |
|---|---|---|---|---|---|
| libwapcaplet | 1 | 4,814 | 1,388 | 0 | 4 |
| libparserutils | 15 | 112,652 | 31,481 | 23,096 | 16 |
| libhubbub | 30 | 382,628 | 304,281 | 424 | 6 |
| libdom | 95 | 693,546 | 147,911 | 56 | 36 |
| libcss | 303 | 1,033,656 | 394,127 | 3,444 | 23,052 |
| **total** | **444** | **2,227,296** | **879,188** | **27,020** | **23,114** |

libhubbub's 304 KB of text is mostly the named-entity table; libparserutils' 23 KB of data is the
charset alias table. Both are the generated files, and both are the price of the tokeniser being
correct rather than of the code being large.

Build time: **16.7 s cold**, **0.34 s warm** (mtime-incremental, `tools/build-netsurf`).

### The probe image

`apps/netsurfprobe`, an EXE that links all five archives and calls into each:

| | |
|---|---|
| `netsurfprobe.exe` | 729,464 bytes |
| codeSize | 705,228 |
| dataSize | 12,812 |
| bssSize | 2,664 |
| imports | 68 across 4 DLLs — `euser`, `efsrv`, `libc`, `drtaeabi` |
| `netsurfprobe.sis` | 246,804 bytes |
| `e32dump.py` | header validated, internally consistent |

879 KB of archive text becomes 705 KB of image code: `--gc-sections` removed about 174 KB the
probe does not reach. A real browser will reach more of it, so **705 KB is a floor, not a
ceiling** — and the plan's RAM risk (R3) has a code-size sibling that F7 will have to weigh
against the E72's ~50 MB.

The four imports are the measurement the probe was built to make. No shim, no Avkon, no window
server: the fewest imports that can carry this stack, so a load failure on the handset can only
be about `libc` or about the image.

## What is unresolved

1. **Nothing has run.** Every number above is from the host. The probe writes
   `C:\Data\netsurfprobe.txt` in `crates/symbian-report`'s grammar with 26 lines — 23 verdicts and 3 measurements; the questions
   it will answer are whether Open C's `malloc` serves an allocation-heavy C library from inside a
   Symbian EXE, and whether the parsers give the right answers on ARM.
2. **`__UHEAP_MARKEND` is armed in the probe** and may well panic. Open C's `malloc` on this
   platform is the process heap, so a leak anywhere in the five libraries' teardown paths shows up
   as a panic and a missing `== END` line. That is deliberate — a leak check belongs in the one
   binary whose whole subject is whether a foreign allocator behaves — but it means the first
   device run may fail for a reason that is a finding rather than a fault.
3. **The charset question is open.** `iconv` links; whether the handset carries the conversion
   tables is unknown, and the device sweep's failure to load `gb2312_shared`, `jisx0201` and
   `jisx0208` is weak evidence that CJK is absent.
4. **No Rust binding yet.** F5 was asked for the cross-compile, and `symbian-dom` (the plan's
   `binding rs → C`) is not in this workstream. The C ABI boundary the probe demonstrates is what
   it will build on.
5. **The archives are 705 KB of code before the browser exists.** F7 will link more of it than
   the probe does, and the E72 has ~50 MB free. That is not a blocker; it is a number that should
   be in front of whoever sizes the tab budget in F9.

## Nothing that needs a decision

Recorded explicitly, since the brief asked for it: nothing here turned out to be a licence
question, a new subsystem or a libc gap. R1 came back negative, R6 was never in doubt, and the one
architectural consequence — that C++ may not include these headers — was answered by a small
header and a new `C_SOURCES` key rather than by a policy.
