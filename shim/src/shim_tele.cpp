/* Telephony reads for the status bar: signal strength (and, later, network mode). Isolated behind
 * SHIM_USE_TELEPHONY and linked only into the network daemon (apps/netd), never the launcher — the
 * etel3rdparty import is a load risk, and quarantining it keeps a lib the handset might not satisfy
 * away from the home screen (the same rule as the icon path).
 *
 * # BROKEN, and the sentence below is how: "a daemon can wait synchronously on them"
 *
 * It cannot. `User::WaitForRequest` on a thread with a running `CActiveScheduler` consumes whatever
 * completes next — including a completion belonging to one of that thread's own active objects. The
 * scheduler then finds a signal for a request it does not own and the thread dies with a stray-signal
 * panic. Which is a kernel panic, so the Rust handler never runs, no `panic.txt` is written, and the
 * process simply is not there any more.
 *
 * `shim_process.cpp` documents that exact rule, having paid for it: the launcher died on roughly two
 * starts in three. And a daemon is not exempt — `shim_daemon.cpp` runs an active scheduler too, and
 * netd arms both a timer and a Publish&Subscribe subscriber. Every poll is a coin toss for whichever
 * of those completes first.
 *
 * Measured on the E72, 22 August 2026: nine netd sessions in one log that write their `start` line
 * and nothing else — no `signal bars=`, which is the very next line `publish` emits. Sessions that
 * survived are the ones where nothing else happened to complete during the wait.
 *
 * The fix is not a longer timeout or a cancel: it is to stop waiting on this thread. Either an active
 * object that posts a `SHIM_EV_*` when the modem answers (the shape every other asynchronous call in
 * this shim already has — see `shim_net.cpp`), or a `CActiveSchedulerWait` so the scheduler keeps
 * running underneath. Until one of those exists, `apps/netd` does not call this: the signal cell is
 * dead on a handset with no SIM anyway, and a coin toss that kills a daemon is not worth an
 * indicator that always reads -1. */

#include "shim_priv.h"

#ifdef SHIM_USE_TELEPHONY

#include <e32base.h>
#include <etel3rdparty.h>

/* Fill signal bars (0..7, or -1 unknown) and the raw dBm. One CTelephony, one synchronous
 * GetSignalStrength — the daemon blocks the few ms the modem takes. */
static void DoSignalL(TInt* aBars, TInt* aDbm)
    {
    CTelephony* tel = CTelephony::NewLC();

    CTelephony::TSignalStrengthV1 sig;
    CTelephony::TSignalStrengthV1Pckg pkg(sig);
    TRequestStatus st;
    tel->GetSignalStrength(st, pkg);
    User::WaitForRequest(st);
    if (st.Int() == KErrNone)
        {
        *aBars = sig.iBar;
        *aDbm = sig.iSignalStrength;
        }
    else
        {
        *aBars = -1;
        *aDbm = 0;
        }

    CleanupStack::PopAndDestroy(tel);
    }

extern "C" int32_t shim_tele_signal(int32_t* bars, int32_t* dbm)
    {
    if (bars)
        *bars = -1;
    if (dbm)
        *dbm = 0;

    TInt b = -1;
    TInt d = 0;
    TRAPD(err, DoSignalL(&b, &d));
    if (err != KErrNone)
        return err;
    if (bars)
        *bars = b;
    if (dbm)
        *dbm = d;
    return SHIM_OK;
    }

#else

extern "C" int32_t shim_tele_signal(int32_t* bars, int32_t* dbm)
    {
    if (bars)
        *bars = -1;
    if (dbm)
        *dbm = 0;
    return SHIM_ERR_NOT_SUPPORTED;
    }

#endif
