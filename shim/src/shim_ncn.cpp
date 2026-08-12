/* The platform's own new-message notification: indicator, tone and floating note.
 *
 * WHY THIS ROUTE AND NOT THE OBVIOUS ONE
 *
 * What we want is the exact triple an arriving SMS produces. The classes that produce it —
 * CAknSoftNotifier and CAknSmallIndicator — are exported from aknnotify.dso and have **no
 * public header in this SDK**; the status-pane indicator is driven by an ECom plugin whose
 * interface is not published either. Declaring those classes by hand would work and would
 * bind us to an unpublished ABI inside a 2009 ROM.
 *
 * MNcnNotification is the supported alternative: an ECom interface the platform publishes
 * precisely so a messaging plugin can raise that notification, with the notification list
 * itself owning the indicator, the tone and the note. One call, no internal ABI.
 *
 * TWO THINGS THIS FILE CANNOT ASSUME
 *
 * The interface's own documentation frames it as an *email* plugin API — the parameter is
 * named aMailBox and the note it raises reads "New email" — so whether it accepts a service
 * whose technology type is neither mail nor SMS is not answerable from the header.
 *
 * And ncnnotification.dll is an ECom *plugin*, not a library: it is absent from the SDK's
 * import set, so nothing in the device sweep ever asked whether it is present.
 *
 * Both are why every failure here is returned as a code rather than trapped and smoothed.
 * A caller is expected to record what came back; that is the measurement.
 *
 * NO STATE
 *
 * Deliberately no file-scope object of any kind: the interface is resolved, used and
 * destroyed inside each call. It costs an ECom resolution per notification, which is
 * nothing against the tone it plays — and it keeps this file linkable into a DLL, where
 * writable static data is refused outright.
 */

#include "symbian_shim.h"
#include "shim_priv.h"

#ifdef SHIM_USE_NCN

#include <e32std.h>
#include <e32base.h>
#include <bamdesca.h>
#include <msvstd.h>
#include <MNcnNotification.h>

namespace {

/* An empty descriptor array, implemented rather than allocated.
 *
 * NewMessages wants an MDesCArray of per-message detail lines. We have none to give — the
 * notification list then renders its own generic text, which is what we want anyway.
 *
 * The obvious way to satisfy that parameter is CDesCArrayFlat, and it costs an import of
 * bafl.dso for a container that will hold nothing. MDesCArray is an abstract interface with
 * two methods, so implementing it here is five lines and no import at all — and in a binary
 * whose whole risk is that an unsatisfied import stops it loading, five lines is the cheaper
 * side of that trade by a wide margin. */
class TEmptyDesCArray : public MDesC16Array
    {
public:
    TInt MdcaCount() const { return 0; }
    /* Unreachable while MdcaCount is zero, and it must still be defined: it is pure virtual,
     * and returning a live empty descriptor is safer than anything clever. */
    TPtrC16 MdcaPoint(TInt) const { return TPtrC16(); }
    };

void NotifyL(TMsvId aService, TInt aIndication)
    {
    MNcnNotification* ncn = MNcnNotification::CreateMNcnNotificationL();
    /* CleanupDeletePushL, not PushL: MNcnNotification is a plain M-class, not a CBase, so
     * the cleanup stack has to be told to `delete` it rather than to treat it as a CBase.
     * The header says so explicitly, and getting it wrong corrupts the stack. */
    CleanupDeletePushL(ncn);

    TEmptyDesCArray info;
    const TInt err = ncn->NewMessages(aService,
                                      (MNcnNotification::TIndicationType) aIndication,
                                      info);
    CleanupStack::PopAndDestroy();  /* ncn */

    /* The interface's own error, surfaced as a Leave so the TRAP below is the single exit.
     * It is a value, not a fault: "the notification list refused a non-mail service" is
     * exactly the sort of thing this call exists to find out. */
    User::LeaveIfError(err);
    }

void MarkUnreadL(TMsvId aService)
    {
    MNcnNotification* ncn = MNcnNotification::CreateMNcnNotificationL();
    CleanupDeletePushL(ncn);
    const TInt err = ncn->MarkUnread(aService);
    CleanupStack::PopAndDestroy();  /* ncn */
    User::LeaveIfError(err);
    }

} /* namespace */

extern "C" {

int32_t shim_ncn_notify(int32_t service_id, int32_t indication)
    {
    if (indication == 0)
        return SHIM_ERR_ARGUMENT;
    TRAPD(err, NotifyL((TMsvId) service_id, indication));
    return err == KErrNone ? SHIM_OK : err;
    }

int32_t shim_ncn_mark_unread(int32_t service_id)
    {
    TRAPD(err, MarkUnreadL((TMsvId) service_id));
    return err == KErrNone ? SHIM_OK : err;
    }

} /* extern "C" */

#endif /* SHIM_USE_NCN */
