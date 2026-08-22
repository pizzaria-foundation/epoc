/* Is the keypad locked?
 *
 * `RAknKeyLock::IsKeyLockEnabled`, whose own header says it answers ETrue "if the keys have been
 * locked normally *or* the phone is in autolock state" — both of which mean the same thing to a
 * caller: the phone is in a pocket and nobody is reading the screen.
 *
 * # Why this and not a Publish & Subscribe key
 *
 * The P&S route is the one every write-up mentions (KCoreAppUIsAutolockStatus), and the keys it
 * names are **not defined on this handset**: read over the remote shell, `0x101F8767` keys 1 and 2
 * and `0x101F8763` key 1 all answer KErrNotFound. Guessing further at a category UID that the public
 * SDK does not ship a header for is how a facility ends up "working" against a key nobody publishes.
 *
 * `aknkeylock.h` is in the SDK, the class is `RAknKeyLock`, and `_ZN11RAknKeyLock16IsKeyLockEnabledEv`
 * is exported from `avkon.dso` — checked, not assumed. Avkon is already in the base library set of
 * every USE_SHIM build, so this costs **no new import**: the same argument USE_CPUTIME makes, and the
 * same reason it is still a gate — a binary that never asks should not carry the code.
 *
 * # The session
 *
 * `RAknKeyLock` is an `RNotifier`, so it is a session to the Avkon notifier server. One per process,
 * opened on first use and left open: a Connect/Close pair per call would be two IPC round trips to
 * answer a question a home screen asks every ten seconds.
 *
 * Needs a control environment (the notifier server is a UI service), so a headless daemon gets
 * SHIM_ERR_NOT_READY rather than an answer. That is why the launcher reads this and the daemons are
 * told by it, rather than each asking for itself.
 */

#include "symbian_shim.h"
#include "shim_priv.h"

#include <e32base.h>
#include <aknkeylock.h>
#include <coemain.h>

namespace {

RAknKeyLock gKeyLock;
TBool gOpen = EFalse;

TInt DoKeyLockL(TInt& aLocked)
    {
    if (!CCoeEnv::Static())
        return SHIM_ERR_NOT_READY;
    if (!gOpen)
        {
        const TInt err = gKeyLock.Connect();
        if (err != KErrNone)
            return err;
        gOpen = ETrue;
        }
    aLocked = gKeyLock.IsKeyLockEnabled() ? 1 : 0;
    return SHIM_OK;
    }

} /* namespace */

extern "C" {

int32_t shim_keylock(void)
    {
    TInt locked = 0;
    TInt rc = SHIM_ERR_NOT_READY;
    TRAPD(err, rc = DoKeyLockL(locked));
    if (err != KErrNone)
        return err;
    if (rc != SHIM_OK)
        return rc;
    return locked;
    }

} /* extern "C" */
