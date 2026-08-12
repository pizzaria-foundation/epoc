/* HAL, passed through rather than wrapped.
 *
 * HAL::Get is already the shape the shim ABI wants — `(TInt attribute, TInt& value)`,
 * flat integers, no descriptors, no allocation, no Leave. Wrapping each attribute in its
 * own exported function would add a hundred lines that only rename things, and would put
 * the attribute table in C++ where nothing can test it.
 *
 * So the table lives in Rust (`symbian::hal`), where it is data with a host test over it,
 * and this file is the one call that carries it across.
 *
 * KErrNotSupported is not an error here. A handset returns it for an attribute its
 * hardware does not implement, which is precisely the kind of thing a device inventory
 * exists to discover — so it is passed up unchanged for the caller to record as a finding.
 */

#include "shim_priv.h"

#ifdef SHIM_USE_HAL

#include <e32std.h>
#include <hal.h>

extern "C" {

int32_t shim_hal_get(int32_t attr, int32_t* out)
    {
    if (!out)
        return SHIM_ERR_ARGUMENT;
    TInt value = 0;
    /* Cannot Leave: HAL::Get returns an error code. No TRAP needed, per rule 1 in
     * symbian_shim.h — the exemption is stated rather than assumed. */
    const TInt rc = HAL::Get((HALData::TAttribute) attr, value);
    if (rc != KErrNone)
        return rc;
    *out = (int32_t) value;
    return SHIM_OK;
    }

} /* extern "C" */

#endif /* SHIM_USE_HAL */
