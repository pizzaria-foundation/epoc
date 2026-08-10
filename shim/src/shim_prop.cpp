/* Publish & Subscribe: the one-integer control channel between the controller and the
 * daemon.
 *
 * The daemon has no UI and is not in the task list, so it cannot be told to stop by closing
 * a window. Instead the controller sets a P&S property and the daemon, subscribed to it,
 * wakes and exits cleanly — which is what lets the uninstall remove \sys\bin\<app>d.exe,
 * since a running process holds its own image open.
 *
 * The category is the app's own SecureId. Symbian lets a process define and write a
 * property in its own SID category with no capability at all; only crossing into another
 * app's category needs WriteDeviceData. The controller and the daemon share one UID3, so
 * they share the category, and neither needs a capability for this.
 *
 * The subscriber is a single CActive re-arming itself on each change, posting SHIM_EV_PROP
 * with the freshly read value. One outstanding subscription per key is enough: the only
 * property this carries is the stop flag.
 */

#include "shim_priv.h"

#ifdef SHIM_USE_PROP

#include <e32std.h>
#include <e32base.h>
#include <e32property.h>

namespace {

const TInt KMaxSubs = 4;

class CPropSub : public CActive
    {
public:
    static CPropSub* NewL(TUint aCategory, TUint aKey);
    ~CPropSub();
    TUint Key() const { return iKey; }

private:
    CPropSub(TUint aCategory, TUint aKey);
    void ConstructL();
    void RunL();
    void DoCancel();

    RProperty iProp;
    TUint iCategory;
    TUint iKey;
    TBool iAttached;
    };

CPropSub* gSubs[KMaxSubs];

CPropSub::CPropSub(TUint aCategory, TUint aKey)
    : CActive(EPriorityStandard), iCategory(aCategory), iKey(aKey), iAttached(EFalse)
    {
    }

CPropSub* CPropSub::NewL(TUint aCategory, TUint aKey)
    {
    CPropSub* self = new (ELeave) CPropSub(aCategory, aKey);
    CleanupStack::PushL(self);
    self->ConstructL();
    CleanupStack::Pop(self);
    return self;
    }

void CPropSub::ConstructL()
    {
    TUid cat = TUid::Uid((TInt32) iCategory);
    User::LeaveIfError(iProp.Attach(cat, iKey));
    iAttached = ETrue;
    CActiveScheduler::Add(this);
    /* Arm before the first change. The initial value is not delivered as an event — the
     * daemon reads it once at startup if it cares; this reports transitions. */
    iProp.Subscribe(iStatus);
    SetActive();
    }

void CPropSub::RunL()
    {
    if (iStatus.Int() == KErrNone)
        {
        TInt value = 0;
        TUid cat = TUid::Uid((TInt32) iCategory);
        /* Read through the class method rather than the attached handle so a value that
         * changed twice before we ran still yields the current one, not a stale cache. */
        RProperty::Get(cat, iKey, value);
        /* The value rides in `c`; ShimPushSimple only sets `a`, so post the full event. */
        ShimEvent ev;
        ev.kind = SHIM_EV_PROP;
        ev.handle = 0;
        ev.status = KErrNone;
        ev.a = (TInt) iKey;
        ev.b = 0;
        ev.c = value;
        ev.d = 0;
        ev.native = 0;
        ShimPushEvent(ev);
        /* Re-arm for the next change. */
        iProp.Subscribe(iStatus);
        SetActive();
        }
    /* On an error completion the subscription stays down; the daemon would re-establish it
     * if that ever mattered, but a P&S subscription on a live property does not error. */
    }

void CPropSub::DoCancel()
    {
    iProp.Cancel();
    }

CPropSub::~CPropSub()
    {
    Cancel();
    if (iAttached)
        iProp.Close();
    }

} /* namespace */

void ShimPropCleanup()
    {
    for (TInt i = 0; i < KMaxSubs; i++)
        {
        if (gSubs[i])
            {
            gSubs[i]->Cancel();
            delete gSubs[i];
            gSubs[i] = NULL;
            }
        }
    }

extern "C" {

int32_t shim_prop_define(uint32_t category, uint32_t key)
    {
    TUid cat = TUid::Uid((TInt32) category);
    /* Integer property, readable and writable within the same SID category with no
     * capability. KErrAlreadyExists is success: a second define of the same key is a no-op,
     * which the controller and the daemon both do independently. */
    TInt rc = RProperty::Define(cat, key, RProperty::EInt);
    if (rc == KErrAlreadyExists)
        return SHIM_OK;
    return rc == KErrNone ? SHIM_OK : rc;
    }

int32_t shim_prop_set(uint32_t category, uint32_t key, int32_t value)
    {
    TUid cat = TUid::Uid((TInt32) category);
    TInt rc = RProperty::Set(cat, key, value);
    return rc == KErrNone ? SHIM_OK : rc;
    }

int32_t shim_prop_get(uint32_t category, uint32_t key, int32_t* out)
    {
    if (!out)
        return SHIM_ERR_ARGUMENT;
    *out = 0;
    TUid cat = TUid::Uid((TInt32) category);
    TInt value = 0;
    TInt rc = RProperty::Get(cat, key, value);
    if (rc != KErrNone)
        return rc;
    *out = value;
    return SHIM_OK;
    }

int32_t shim_prop_subscribe(uint32_t category, uint32_t key)
    {
    for (TInt i = 0; i < KMaxSubs; i++)
        if (gSubs[i] && gSubs[i]->Key() == key)
            return SHIM_OK; /* already subscribed to this key */

    TInt slot = -1;
    for (TInt i = 0; i < KMaxSubs; i++)
        if (!gSubs[i])
            {
            slot = i;
            break;
            }
    if (slot < 0)
        return SHIM_ERR_IN_USE;

    TInt err = KErrNone;
    TRAP(err, gSubs[slot] = CPropSub::NewL(category, key));
    if (err != KErrNone)
        {
        gSubs[slot] = NULL;
        return err;
        }
    return SHIM_OK;
    }

void shim_prop_unsubscribe(uint32_t /*category*/, uint32_t key)
    {
    for (TInt i = 0; i < KMaxSubs; i++)
        if (gSubs[i] && gSubs[i]->Key() == key)
            {
            gSubs[i]->Cancel();
            delete gSubs[i];
            gSubs[i] = NULL;
            return;
            }
    }

} /* extern "C" */

#endif /* SHIM_USE_PROP */
