/* Memory readings: how much room is left.
 *
 * Two questions, two APIs, both uncapped:
 *   - "How much RAM does the whole device have free right now?"  HAL::Get, which reads the
 *     kernel's own figures. This is the number to watch for pressure: when it drops below a
 *     watermark, something has to give.
 *   - "How much heap is *this* process holding?"  User::AllocSize, over the current thread's
 *     allocator. There is no public way to ask that of another process (that would need the
 *     kernel), so no caller can attribute RAM to a specific app — it acts on the
 *     device-wide free figure plus a policy, not on per-app consumption. That limit is real
 *     a matter of policy for whoever reads them.
 *
 * Everything is reported in KiB. A device has ~128 MiB (131072 KiB), so an i32 never
 * overflows, and KiB is a finer unit than any watermark needs.
 */

#include "shim_priv.h"

#ifdef SHIM_USE_MEM

#include <e32std.h>
#include <hal.h>

extern "C" {

int32_t shim_mem_free_kb(void)
    {
    TInt bytes = 0;
    TInt rc = HAL::Get(HALData::EMemoryRAMFree, bytes);
    if (rc != KErrNone)
        return rc;
    return (int32_t)(bytes >> 10);
    }

int32_t shim_mem_total_kb(void)
    {
    TInt bytes = 0;
    TInt rc = HAL::Get(HALData::EMemoryRAM, bytes);
    if (rc != KErrNone)
        return rc;
    return (int32_t)(bytes >> 10);
    }

int32_t shim_heap_used_kb(void)
    {
    /* AllocSize returns the cell count and writes the total allocated bytes; a caller
     * only cares about the byte figure, as a coarse "is the daemon itself growing?" signal. */
    TInt total = 0;
    (void) User::AllocSize(total);
    if (total < 0)
        total = 0;
    return (int32_t)(total >> 10);
    }

} /* extern "C" */

#endif /* SHIM_USE_MEM */
