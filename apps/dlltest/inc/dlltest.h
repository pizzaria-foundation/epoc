/* dlltest — the ABI between the probe and the polymorphic DLL it loads.
 *
 * Shared by the DLL (which defines the function) and by whatever loads it
 * (which reaches it through RLibrary::Lookup(1), never by linking, so this
 * header describes a contract rather than an import).
 */
#ifndef DLLTEST_H
#define DLLTEST_H

#include <e32def.h>

/* Written into TDllTestResult::iMagic. Chosen to be recognisable in a hex dump
 * and to be nothing a zeroed or uninitialised buffer could produce. */
const TUint32 KDllTestMagic = 0x5A1234A5;

/* The ordinal the loader asks for. One export, so elf2e32's name sort cannot
 * put anything else here — see the EXPORTS note in app.conf. */
const TInt KDllTestOrdinal = 1;

struct TDllTestResult
	{
	/* KDllTestMagic. Proves code in the DLL ran and wrote through the pointer
	   the caller passed, which a non-null Lookup() result on its own does not. */
	TUint32 iMagic;
	/* The argument, echoed. Proves the call frame carried it across the
	   boundary rather than the callee reading a register that happened to hold
	   the right value. */
	TUint32 iEcho;
	/* User::TickCount() from inside the DLL. Its only job is to make the DLL
	   import something from euser: the export table and the import table are
	   separate mechanisms, and a DLL that exports correctly can still fail to
	   resolve its own imports. A plausible nonzero value here is the evidence
	   that both halves work. */
	TUint32 iTicks;
	};

/* The signature RLibrary::Lookup(1) returns, cast from TLibraryFunction. */
typedef TInt (*TDllTestEntry)(TDllTestResult* aOut, TUint32 aArg);

#endif
