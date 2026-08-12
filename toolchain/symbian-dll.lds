/* Linker script for a Symbian E32 polymorphic DLL.
 *
 * This is toolchain/symbian-exe.lds with the entry point changed and one extra
 * concern. Read that file first: every comment in it about elf2e32's two-PT_LOAD
 * requirement, the `.data` name check, the parked `:meta` block and the four
 * RVCT-synthesised symbols applies here unchanged, because elf2e32 walks a DLL
 * and an EXE through the same code path.
 *
 * WHAT IS DIFFERENT
 *
 *   - ENTRY(_E32Dll) rather than _E32Startup. `_E32Dll` comes from edll.lib
 *     (uc_dll_.o) instead of eexe.lib, and like `_E32Startup` it lives in
 *     `.emb_text` and is GLOBAL HIDDEN — so the same KEEP() below roots it, and
 *     its hidden visibility keeps it out of .dynsym where it would otherwise be
 *     mistaken for an export.
 *
 *   - `Symbian$$CPP$$Exception$$Descriptor` is again at the end of that glue
 *     (offset 0x14 in uc_dll_.o) and again referenced by nothing, so the KEEP is
 *     load-bearing for exactly the same reason.
 *
 * WHAT elf2e32 EXPORTS, AND WHY THIS MATTERS MORE FOR A DLL
 *
 * findExports() (vendor/gnupoc-git/tools/elf2e32/elf2e32.cpp:156) builds the
 * export table from `.dynsym`: every STB_GLOBAL symbol of type FUNC or OBJECT
 * with a nonzero value, sorted by name, numbered from 1. It is not reading a
 * `.def` file unless one is passed. So *what lands in .dynsym is the ABI*, and a
 * stray global from a static library becomes an export that shifts every ordinal
 * after it.
 *
 * The defence is not in this script — it is the `--version-script` symbuild
 * passes for a DLL target, which localises everything except the names the app
 * explicitly exports. This is noted here because the two files have to agree:
 * changing the export set is a version-script edit, not a layout edit.
 *
 * WRITABLE STATIC DATA
 *
 * A Symbian 9.x DLL may not have initialised writable static data unless it
 * declares EPOCALLOWDLLDATA. `.data` is still laid out below so the two-segment
 * requirement holds and elf2e32 finds a dataBase, but a DLL built through this
 * script should keep it empty — no non-const globals. `.bss` is NOBITS and is
 * accounted through memsz - filesz as usual.
 */

ENTRY(_E32Dll)

PHDRS
{
  ro PT_LOAD FLAGS(5);      /* PF_R | PF_X  -> elf2e32 reads this as codeBase */
  rw PT_LOAD FLAGS(6);      /* PF_R | PF_W  -> dataBase and bssSize */
  meta PT_LOAD FLAGS(4);    /* PF_R only    -> matches neither of elf2e32's tests */
  dyn PT_DYNAMIC FLAGS(4);
}

SECTIONS
{
  /* Same 0x8000 as an EXE. The Symbian loader relocates either kind, and a real
     armv5 DLL carved out of the SDK's own SIS files has codeBase 0x8000 too. */
  . = 0x8000;

  /* ---- code ---- */
  Image$$ER_RO$$Base = .;

  .text :
  {
    /* Carries _E32Dll and Symbian$$CPP$$Exception$$Descriptor, neither of which
       anything references — the kernel finds the descriptor through the E32
       header. KEEP or --gc-sections takes both. */
    KEEP(*(.emb_text))
    *(.text.clean)
    *(.text .stub .text.* .gnu.linkonce.t.*)
    *(.plt) *(.iplt)
    *(.glue_7t) *(.glue_7) *(.vfp11_veneer) *(.v4_bx)
  }                                                                :ro

  .rodata :
  {
    *(.rodata .rodata.* .gnu.linkonce.r.*)
    /* Symbian's own name for read-only data; RVCT-built objects use it. */
    *(.constdata .constdata.*)
    *(.rodata1)
  }                                                                :ro

  .ARM.extab : { *(.ARM.extab* .gnu.linkonce.armextab.*) }         :ro

  .ARM.exidx :
  {
    .ARM.exidx$$Base = .;
    *(.ARM.exidx* .gnu.linkonce.armexidx.*)
    .ARM.exidx$$Limit = .;
  }                                                                :ro

  /* Written by the loader while relocating, so it belongs with the code. */
  .got : { *(.got.plt) *(.igot.plt) *(.got) *(.igot) }             :ro

  Image$$ER_RO$$Limit = .;

  /* ---- writable ----
     Kept empty by convention for a DLL (see the header comment), but the output
     section must still exist and must still be named exactly `.data`, because
     that is the string elf2e32 checks to keep it out of the code segment. */
  . = ALIGN(0x1000);

  .data :
  {
    *(.data .data.* .gnu.linkonce.d.*)
    *(.data.rel.ro .data.rel.ro.*)
    *(.data1)
  }                                                                :rw

  .bss (NOLOAD) :
  {
    *(.dynbss)
    *(.bss .bss.* .gnu.linkonce.b.*)
    *(COMMON)
  }                                                                :rw

  /* ---- link-time-only metadata, parked past everything else ----
     elf2e32 reads .dynsym and .rel.* to build the export, import and relocation
     tables, but none of it belongs in the image. tools/e32prep.py clears the
     ALLOC bit afterwards; putting them last keeps the address space they occupy
     behind the code rather than in front of it. */
  . = ALIGN(0x1000);

  .hash          : { *(.hash) }                                    :meta
  .gnu.hash      : { *(.gnu.hash) }                                :meta
  .dynsym        : { *(.dynsym) }                                  :meta
  .dynstr        : { *(.dynstr) }                                  :meta
  .gnu.version   : { *(.gnu.version) }                             :meta
  .gnu.version_d : { *(.gnu.version_d) }                           :meta
  .gnu.version_r : { *(.gnu.version_r) }                           :meta
  .rel.dyn       :
  {
    *(.rel.init) *(.rel.fini)
    *(.rel.text .rel.text.* .rel.gnu.linkonce.t.*)
    *(.rel.rodata .rel.rodata.* .rel.gnu.linkonce.r.*)
    *(.rel.data .rel.data.* .rel.gnu.linkonce.d.*)
    *(.rel.constdata)
    *(.rel.got) *(.rel.bss) *(.rel.dyn)
    *(.rel.ARM.extab*) *(.rel.emb_text)
  }                                                                :meta
  .rel.plt       : { *(.rel.plt) *(.rel.iplt) }                    :meta
  .dynamic       : { *(.dynamic) }                                 :meta :dyn

  /DISCARD/ :
  {
    *(.note.GNU-stack) *(.gnu_debuglink) *(.gnu.lto_*)
    *(.comment) *(.ARM.attributes)
    *(.interp) *(.eh_frame) *(.eh_frame_hdr)
  }
}
