/* What the kernel thinks this process was granted.
 *
 * WHY THIS IS ONLY HALF THE QUESTION
 *
 * RProcess::HasCapability reports what the loader stamped on this image. On a handset with
 * a patched installserver that is worth knowing — it says whether the patch actually lifted
 * the ceiling, or merely stopped refusing the package. But it is not the same question as
 * "can this process do the privileged thing", and treating it as such is how a diagnostic
 * comes to report success while examining less than it claims.
 *
 * The other half is answered by *attempting the operation* and recording the error code:
 * opening a file in another application's data cage, writing to Z:, renaming a drive. Those
 * need no new shim — they are the ordinary file calls, and `shim_fs_att` below is there so
 * a probe can attempt one against an arbitrary path without creating or destroying
 * anything.
 *
 * A caller is expected to report both, side by side. The divergence is the finding: the
 * kernel saying the capability is held while the operation still answers
 * KErrPermissionDenied means something other than platform security is refusing, and that
 * is a fact no amount of reading either answer alone would produce.
 */

#include "shim_priv.h"

#ifdef SHIM_USE_CAPS

#include <e32std.h>
#include <f32file.h>

extern "C" {

int32_t shim_has_capability(int32_t cap)
    {
    /* TCapability's valid range is 0..ECapability_Limit. Out-of-range values are rejected
     * here rather than passed down, because HasCapability's contract for them is not
     * something to discover on a device. */
    if (cap < 0 || cap >= ECapability_Limit)
        return SHIM_ERR_ARGUMENT;
    /* RProcess() default-constructs to the current process. Cannot Leave. */
    return RProcess().HasCapability((TCapability) cap) ? 1 : 0;
    }

int32_t shim_fs_att(const uint16_t* path, int32_t len, uint32_t* out)
    {
    if (!path || len <= 0 || !out)
        return SHIM_ERR_ARGUMENT;

    RFs fs;
    TInt rc = fs.Connect();
    if (rc != KErrNone)
        return rc;

    TPtrC16 name(reinterpret_cast<const TUint16*>(path), len);
    TUint att = 0;
    rc = fs.Att(name, att);
    if (rc == KErrNone)
        *out = (uint32_t) att;
    fs.Close();
    /* The error is the point at least as often as the value. KErrPermissionDenied on a
     * path inside another app's cage is the capability probe's actual result. */
    return rc == KErrNone ? SHIM_OK : rc;
    }

} /* extern "C" */

#endif /* SHIM_USE_CAPS */
