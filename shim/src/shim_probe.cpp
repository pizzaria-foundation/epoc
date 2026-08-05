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

} /* extern "C" */
