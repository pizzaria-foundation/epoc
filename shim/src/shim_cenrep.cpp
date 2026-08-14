/* A minimal Central Repository read, for status values that live in CenRep rather than Publish&
 * Subscribe — the Bluetooth power state being the first. Isolated behind SHIM_USE_CENREP and linked
 * only into the network daemon: centralrepository is a base library, but the rule stands — a lib the
 * launcher does not need stays out of the launcher. A key whose read policy denies us returns the
 * platform error (treated as "unknown"), never a crash. */

#include "shim_priv.h"

#ifdef SHIM_USE_CENREP

#include <e32base.h>
#include <centralrepository.h>

static void DoGetL(TUint32 aRepo, TUint32 aKey, TInt* aOut)
    {
    CRepository* rep = CRepository::NewLC(TUid::Uid(aRepo));
    TInt val = 0;
    User::LeaveIfError(rep->Get((TUint32) aKey, val));
    *aOut = val;
    CleanupStack::PopAndDestroy(rep);
    }

extern "C" int32_t shim_cenrep_get(uint32_t repo, uint32_t key, int32_t* out)
    {
    if (out)
        *out = 0;
    TInt v = 0;
    TRAPD(err, DoGetL(repo, key, &v));
    if (err != KErrNone)
        return err;
    if (out)
        *out = v;
    return SHIM_OK;
    }

#else

extern "C" int32_t shim_cenrep_get(uint32_t repo, uint32_t key, int32_t* out)
    {
    (void) repo;
    (void) key;
    if (out)
        *out = 0;
    return SHIM_ERR_NOT_SUPPORTED;
    }

#endif
