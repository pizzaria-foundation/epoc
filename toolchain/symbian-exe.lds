/* Linker script for a Symbian E32 executable.
 *
 * WHY A FULL SCRIPT AND NOT A FEW SYMBOL ASSIGNMENTS
 *
 * A `-T` script replaces ld's built-in SECTIONS outright, even when it contains
 * nothing but assignments. That is easy to miss: with a single input object,
 * ld's orphan placement happens to produce something that looks like the default
 * layout, so an assignments-only script appears to work. Add a second object and
 * the illusion collapses — `.text.foo`, `.text.bar`, `.emb_text`, `.constdata`
 * and a dozen `.ARM.exidx.text.*` all land as separate output sections, spread
 * across mismatched segments. elf2e32 then walks them and segfaults.
 *
 * WHAT elf2e32 NEEDS (from tools/elf2e32/elf2e32.cpp)
 *
 *   - Exactly two PT_LOAD segments. It reads them as:
 *         (p_flags & (R|X)) == (R|X)  ->  codeBase
 *         (p_flags & (R|W)) == (R|W)  ->  dataBase, bssSize = memsz - filesz
 *     A single RWX segment satisfies the first test and the data segment is then
 *     never found, so the two must be split and the read-only one must not be
 *     writable.
 *
 *   - Every SHF_ALLOC section except those named `.data`, `ER_RW` or `ER_ZI` is
 *     concatenated, in section-header order, into the code segment. It pads gaps
 *     with zeroes and complains when a section starts before where it expected.
 *     So the read-only sections must be laid out contiguously, and everything
 *     writable must be inside an output section named `.data` (or be NOBITS,
 *     which it skips because there is no raw data to copy).
 *
 *   - `Symbian$$CPP$$Exception$$Descriptor`, whose address goes into the E32
 *     header as iExceptionDescriptor. It is the four words at the end of the
 *     startup glue, and it needs the four symbols defined below.
 *
 * The dynamic metadata (.hash, .dynsym, .rel.dyn, .dynamic, .got) deliberately
 * lives in the read-only segment. Symbian's loader applies relocations to the
 * code segment at load time, so a .got that is written during loading belongs
 * there — that is how a real E32 image is arranged.
 *
 * THE FOUR RVCT SYMBOLS
 *
 * eexe.lib is prebuilt by Symbian with RVCT, whose linker synthesises
 * `Image$$<region>$$Base/Limit` and `.ARM.exidx$$Base/Limit`. GNU ld has no such
 * feature, so they arrive as undefined R_ARM_ABS32 references and we must supply
 * them. They are not cosmetic: on Symbian 9.x a User::Leave is a C++ throw, so
 * a wrong exception index range breaks every TRAP in the process.
 */

ENTRY(_E32Startup)

PHDRS
{
  ro PT_LOAD FLAGS(5);      /* PF_R | PF_X  -> elf2e32 reads this as codeBase */
  rw PT_LOAD FLAGS(6);      /* PF_R | PF_W  -> dataBase and bssSize */
  meta PT_LOAD FLAGS(4);    /* PF_R only    -> matches neither of elf2e32's tests */
  dyn PT_DYNAMIC FLAGS(4);
}

SECTIONS
{
  /* Symbian's conventional code base, and not merely cosmetic: a real
     Symbian-built image has codeBase 0x8000 (verified against an armv5 binary
     carved out of the SDK's own example SIS files), and the code has to start
     there rather than after whatever else the linker laid down first. */
  . = 0x8000;

  /* ---- code ---- */
  Image$$ER_RO$$Base = .;

  .text :
  {
    /* .emb_text carries _E32Startup. It must be merged here rather than left to
       orphan placement, which strands it in a segment of its own.
       KEEP because it also carries Symbian$$CPP$$Exception$$Descriptor, which no
       code references — the kernel finds it through the E32 header's
       exceptionDescriptor field. Under --gc-sections an unreferenced section is
       exactly what gets swept, and losing it means every TRAP misbehaves on a
       device with nothing to point at the cause. */
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
    /* Not KEEP: ld handles exidx specially and drops the entries belonging to
       text it swept, which is the behaviour we want. Keeping them all would leave
       unwind entries pointing into freed address ranges. */
    *(.ARM.exidx* .gnu.linkonce.armexidx.*)
    .ARM.exidx$$Limit = .;
  }                                                                :ro

  /* Written by the loader while relocating, so it belongs with the code. */
  /* The GOT stays in the image: the PLT stubs load from it at runtime, and the
     Symbian loader writes the resolved import addresses into it while relocating
     the code segment. .dynamic does not — nothing reads it on device, because
     imports come from the E32 import table rather than from ELF dynamic linking. */
  .got : { *(.got.plt) *(.igot.plt) *(.got) *(.igot) }             :ro

  Image$$ER_RO$$Limit = .;

  /* ---- writable ----
     A fresh page so this gets its own PT_LOAD with PF_W, which is how elf2e32
     finds dataBase. The name must be exactly `.data`: that is the string it
     checks to keep this out of the code segment. */
  . = ALIGN(0x1000);

  .data :
  {
    *(.data .data.* .gnu.linkonce.d.*)
    *(.data.rel.ro .data.rel.ro.*)
    *(.data1)
  }                                                                :rw

  /* NOBITS, so elf2e32 skips it for lack of raw data and accounts for it through
     memsz - filesz, which is exactly the ER_ZI size Symbian wants. */
  .bss (NOLOAD) :
  {
    *(.dynbss)
    *(.bss .bss.* .gnu.linkonce.b.*)
    *(COMMON)
  }                                                                :rw

  /* ---- link-time-only metadata, parked past everything else ----
     These must exist in the file (elf2e32 reads .dynsym and .rel.* to build the
     import and relocation tables) but must not end up in the image. The build
     clears their ALLOC bit with objcopy afterwards; putting them last means the
     address space they occupy is past the code rather than in front of it, which
     is what was pushing codeBase from 0x8000 up to wherever .text landed.
     Their PT_LOAD is read-only with no execute or write bit, so it matches
     neither of the two tests elf2e32 uses to find the code and data segments. */
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

  /* Debug info and anything else we do not want in the image. */
  /DISCARD/ :
  {
    *(.note.GNU-stack) *(.gnu_debuglink) *(.gnu.lto_*)
    *(.comment) *(.ARM.attributes)
    *(.interp) *(.eh_frame) *(.eh_frame_hdr)
  }
}
