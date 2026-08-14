/* Telephony reads for the status bar: signal strength (and, later, network mode). Isolated behind
 * SHIM_USE_TELEPHONY and linked only into the network daemon (apps/netd), never the launcher — the
 * etel3rdparty import is a load risk, and quarantining it keeps a lib the handset might not satisfy
 * away from the home screen (the same rule as the icon path). CTelephony's calls are asynchronous;
 * a daemon can wait synchronously on them, which is what makes this a plain blocking read. */

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
