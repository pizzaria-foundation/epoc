/* dlltest — the smallest polymorphic DLL that can prove the toolchain builds one.
 *
 * WHY THIS EXISTS
 *
 * Every plugin interface on this platform is a polymorphic DLL entered through an
 * ordinal: MTMs, ECom implementations, FEPs, recognisers. Until now this toolchain
 * has only ever produced EXEs, so "can we build a DLL the Symbian loader accepts"
 * was an open question sitting under every one of those. This answers it with the
 * smallest possible object, so that when it fails the fault has nowhere to hide.
 *
 * Most of the answer is obtainable on the host: tools/e32dump.py --expect-dll
 * checks the image type, the UID1, a non-empty export table and the absence of
 * writable static data. The one part that is not is whether the *handset's*
 * loader accepts it and whether RLibrary::Lookup(1) returns something callable.
 * That is what apps/devdump's 40-dll probe asks.
 *
 * WHAT IT DELIBERATELY DOES NOT HAVE
 *
 * No static writable data — not one non-const global. A Symbian 9.x DLL with
 * initialised writable statics needs EPOCALLOWDLLDATA, and the loader refuses the
 * image without it. Keeping the DLL data-free removes that variable from the
 * experiment entirely; if a later, real DLL needs data, that is the moment to
 * teach symbuild the flag, not now.
 *
 * No E32Dll(TDllReason) either. That entry point was required on EKA1 and is not
 * on EKA2 (Symbian 9.x): edll.lib defines _E32Dll_Body itself, so a DLL with no
 * initialisation supplies nothing.
 *
 * AND NO STATIC CONSTRUCTORS, WHICH IS NOT A STYLE CHOICE
 *
 * elf2e32 sets KImageNoCallEntryPoint in the header unconditionally
 * (vendor/gnupoc-git/tools/elf2e32/elf2e32.cpp:351) — for EXEs too; every .exe
 * this toolchain has ever shipped carries it. The loader therefore does not call
 * _E32Dll, so nothing runs __cpp_initialize__aeabi_ and no static constructor in a
 * DLL built here will ever execute.
 *
 * That costs this DLL nothing, because it has no statics. It will matter a great
 * deal to the first real one: a C++ plugin that expects file-scope objects to be
 * alive by the time its ordinal is called would find them zeroed, with no
 * diagnostic. Initialise from inside the exported function instead.
 */

#include <e32std.h>
#include "dlltest.h"

/* extern "C" so the exported name is the one app.conf's EXPORTS names, with no
 * mangling to keep in sync between the two files.
 *
 * EXPORT_C is not decoration and not optional. GCC's arm-none-symbianelf target
 * gives every symbol hidden visibility by default — the platform's own convention,
 * since on Symbian a DLL exports only what it declares — and a hidden symbol never
 * reaches .dynsym, which is the only place elf2e32 looks when it builds the export
 * table. Without this the DLL builds, links and packages with an empty export
 * table; tools/e32dump.py --expect-dll is what catches it.
 *
 * So there are two mechanisms and they do different jobs: EXPORT_C makes a symbol
 * exportable at all, and the version script symbuild generates from EXPORTS pins
 * the set to exactly what app.conf declares. Neither replaces the other.
 */
extern "C" EXPORT_C TInt SymbianDllTestEntry(TDllTestResult* aOut, TUint32 aArg)
	{
	if (!aOut)
		{
		/* The caller is the probe and the probe passes a real buffer, so this is
		   unreachable — but a DLL that faults takes its loader's process down,
		   and the loader here is the thing trying to report on it. */
		return KErrArgument;
		}
	aOut->iMagic = KDllTestMagic;
	aOut->iEcho = aArg;
	aOut->iTicks = User::TickCount();
	return KErrNone;
	}
