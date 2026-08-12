/* Loading our own polymorphic DLL, and calling through its ordinal.
 *
 * This is the one part of the DLL question that cannot be settled on the host.
 * tools/e32dump.py --expect-dll already refuses an image that is not marked as a DLL, has
 * the wrong UID1, exports nothing, or carries writable static data — the three ways the
 * build can produce a file that looks fine and is not. What no host check can reach is
 * whether the *handset's* E32 loader accepts the image and whether RLibrary::Lookup hands
 * back something callable.
 *
 * WHY EVERY STEP IS REPORTED SEPARATELY
 *
 * They fail for different reasons, and a single pass/fail would collapse four distinct
 * diagnoses into one:
 *
 *   load_err   nonzero   the loader refused it — a bad header, or an import it cannot meet
 *   uid1       wrong     it loaded, but as something other than a polymorphic DLL
 *   lookup_ok  false     it loaded and exports nothing (what a missing EXPORT_C produces)
 *   call_err   nonzero   the ordinal is real and the call reached it and it disagreed
 *   magic      wrong     the call ran and did not write through the pointer we passed
 *
 * The last one matters more than it looks: a non-null Lookup() proves an export table
 * exists, not that calling it executes our code with our arguments. Writing a sentinel and
 * echoing the argument is what turns "an address came back" into "our function ran".
 */

#include "symbian_shim.h"
#include "shim_priv.h"

#ifdef SHIM_USE_DLL_PROBE

#include <e32std.h>

namespace {

/* Mirrors apps/dlltest/inc/dlltest.h. Repeated rather than included because that header
 * belongs to the DLL and this is its caller: they agree by contract, and the contract is
 * exactly what the probe is testing. Including it would make a mismatch a compile error
 * here instead of the finding it is supposed to be. */
struct TDllTestResult
    {
    TUint32 iMagic;
    TUint32 iEcho;
    TUint32 iTicks;
    };

typedef TInt (*TDllTestEntry)(TDllTestResult* aOut, TUint32 aArg);

} /* namespace */

extern "C" {

int32_t shim_dll_call_ordinal1(const uint16_t* name, int32_t len, uint32_t arg,
                               ShimDllProbe* out)
    {
    if (!name || len <= 0 || !out)
        return SHIM_ERR_ARGUMENT;

    out->load_err = 0;
    out->uid1 = out->uid2 = out->uid3 = 0;
    out->lookup_ok = 0;
    out->call_err = 0;
    out->magic = out->echo = out->ticks = 0;

    TPtrC16 dll(reinterpret_cast<const TUint16*>(name), len);

    RLibrary lib;
    const TInt err = lib.Load(dll);
    out->load_err = (int32_t) err;
    if (err != KErrNone)
        return SHIM_OK; /* The failure is data, not an error of this call. */

    const TUidType type = lib.Type();
    out->uid1 = (uint32_t) type[0].iUid;
    out->uid2 = (uint32_t) type[1].iUid;
    out->uid3 = (uint32_t) type[2].iUid;

    const TLibraryFunction fn = lib.Lookup(1);
    if (!fn)
        {
        lib.Close();
        return SHIM_OK;
        }
    out->lookup_ok = 1;

    TDllTestResult result;
    result.iMagic = 0;
    result.iEcho = 0;
    result.iTicks = 0;

    /* No TRAP. The exported function is ours, takes a pointer and an integer, returns a
     * TInt and does not Leave — and if that assumption is ever wrong on a device, a TRAP
     * here would hide it behind an error code rather than showing it as the crash it is.
     * The whole point of this probe is to find out what actually happens. */
    out->call_err = (int32_t) ((TDllTestEntry) fn)(&result, (TUint32) arg);
    out->magic = (uint32_t) result.iMagic;
    out->echo = (uint32_t) result.iEcho;
    out->ticks = (uint32_t) result.iTicks;

    /* Closed only after the call returns: closing the library frees the code the function
     * pointer points into. */
    lib.Close();
    return SHIM_OK;
    }

int32_t shim_dll_has_ordinal(const uint16_t* name, int32_t len, int32_t ordinal)
    {
    if (!name || len <= 0 || ordinal < 1)
        return SHIM_ERR_ARGUMENT;

    TPtrC16 dll(reinterpret_cast<const TUint16*>(name), len);
    RLibrary lib;
    const TInt err = lib.Load(dll);
    if (err != KErrNone)
        return err;

    /* Lookup only. Whether the address behind the slot is callable, and with what signature,
     * is exactly what this refuses to find out — that question has already killed a process
     * once and it belongs to whoever knows the signature. */
    const TLibraryFunction fn = lib.Lookup(ordinal);
    lib.Close();
    return fn ? 1 : 0;
    }

} /* extern "C" */

#endif /* SHIM_USE_DLL_PROBE */
