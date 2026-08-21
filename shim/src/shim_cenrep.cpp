/* A minimal Central Repository read, for status values that live in CenRep rather than Publish&
 * Subscribe — the Bluetooth power state being the first. Isolated behind SHIM_USE_CENREP and linked
 * only into the network daemon: centralrepository is a base library, but the rule stands — a lib the
 * launcher does not need stays out of the launcher. A key whose read policy denies us returns the
 * platform error (treated as "unknown"), never a crash.
 *
 * Writing came later and for one purpose: repository 0x101F876F key 0x2 is the setting that names
 * the application the phone treats as its idle screen, and it is writable with WriteDeviceData —
 * read out of the handset's own ROM defaults rather than guessed. A home screen that is merely
 * launched at boot always loses a race with the platform's idle; one that IS the idle never runs
 * that race. Strings as well as integers, because that key holds the UID as decimal text. */

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

static void DoGetStrL(TUint32 aRepo, TUint32 aKey, uint16_t* aBuf, int32_t aCap, int32_t* aLen)
    {
    CRepository* rep = CRepository::NewLC(TUid::Uid(aRepo));
    /* A settings value, not a document: 512 units is far more than any UID or path this is used
     * for, and a fixed buffer keeps the leave-safety trivial. */
    TBuf<512> val;
    User::LeaveIfError(rep->Get((TUint32) aKey, val));
    if (val.Length() > aCap)
        User::Leave(KErrOverflow);
    for (TInt i = 0; i < val.Length(); i++)
        aBuf[i] = val[i];
    *aLen = val.Length();
    CleanupStack::PopAndDestroy(rep);
    }

static void DoSetL(TUint32 aRepo, TUint32 aKey, TInt aValue)
    {
    CRepository* rep = CRepository::NewLC(TUid::Uid(aRepo));
    User::LeaveIfError(rep->Set((TUint32) aKey, aValue));
    CleanupStack::PopAndDestroy(rep);
    }

static void DoSetStrL(TUint32 aRepo, TUint32 aKey, const uint16_t* aText, int32_t aLen)
    {
    CRepository* rep = CRepository::NewLC(TUid::Uid(aRepo));
    TPtrC16 val(reinterpret_cast<const TUint16*>(aText), aLen);
    User::LeaveIfError(rep->Set((TUint32) aKey, val));
    CleanupStack::PopAndDestroy(rep);
    }

extern "C" int32_t shim_cenrep_get_string(uint32_t repo, uint32_t key, uint16_t* buf, int32_t cap, int32_t* len)
    {
    if (!buf || cap <= 0 || !len)
        return SHIM_ERR_ARGUMENT;
    *len = 0;
    TInt out = 0;
    TRAPD(err, DoGetStrL(repo, key, buf, cap, &out));
    if (err != KErrNone)
        return err;
    *len = out;
    return SHIM_OK;
    }

extern "C" int32_t shim_cenrep_set(uint32_t repo, uint32_t key, int32_t value)
    {
    TRAPD(err, DoSetL(repo, key, value));
    return err == KErrNone ? SHIM_OK : err;
    }

extern "C" int32_t shim_cenrep_set_string(uint32_t repo, uint32_t key, const uint16_t* text, int32_t len)
    {
    if (!text || len < 0)
        return SHIM_ERR_ARGUMENT;
    TRAPD(err, DoSetStrL(repo, key, text, len));
    return err == KErrNone ? SHIM_OK : err;
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

extern "C" int32_t shim_cenrep_get_string(uint32_t repo, uint32_t key, uint16_t* buf, int32_t cap, int32_t* len)
    {
    (void) repo; (void) key; (void) buf; (void) cap;
    if (len)
        *len = 0;
    return SHIM_ERR_NOT_SUPPORTED;
    }

extern "C" int32_t shim_cenrep_set(uint32_t repo, uint32_t key, int32_t value)
    {
    (void) repo; (void) key; (void) value;
    return SHIM_ERR_NOT_SUPPORTED;
    }

extern "C" int32_t shim_cenrep_set_string(uint32_t repo, uint32_t key, const uint16_t* text, int32_t len)
    {
    (void) repo; (void) key; (void) text; (void) len;
    return SHIM_ERR_NOT_SUPPORTED;
    }

#endif
