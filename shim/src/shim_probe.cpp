/* Capability queries about the device we happen to be running on.
 *
 * Kept apart from the rest of the shim because these answer questions rather than
 * provide services, and because a diagnostic that lives next to the code it is
 * diagnosing tends to get deleted with it.
 */

#include "symbian_shim.h"
#include "shim_priv.h"

#include <e32std.h>
#include <f32file.h>

extern "C" {

int32_t shim_dll_present(const uint16_t* name, int32_t len)
    {
    if (!name || len <= 0)
        return SHIM_ERR_ARGUMENT;

    TPtrC16 dll(reinterpret_cast<const TUint16*>(name), len);

    /* RLibrary::Load, not just a filesystem check. A DLL can exist on disk and
     * still fail to load — wrong UID3, unsatisfied imports of its own, or a
     * capability we do not hold — and every one of those would break an import
     * exactly as thoroughly as the file being absent. Loading it is the only test
     * that answers the question actually being asked. */
    RLibrary lib;
    const TInt err = lib.Load(dll);
    if (err == KErrNone)
        lib.Close();
    return err;
    }

/* This process's own UID3, the value symbuild passes as -DSHIM_APP_UID3. The Rust runtime
 * uses it as its Publish & Subscribe category, so an app can publish telemetry (present
 * stats) in its own category with no capability, and a reader can pick it up from another
 * process — reading a foreign category is free. Zero if the build did not set it. */
uint32_t shim_own_uid3(void)
    {
#ifdef SHIM_APP_UID3
    return (uint32_t) SHIM_APP_UID3;
#else
    return 0;
#endif
    }

} /* extern "C" */
