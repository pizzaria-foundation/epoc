/* Position fixes, through the platform's Location Acquisition API.
 *
 * RPositionServer is a client-server session onto the location framework; RPositioner is a
 * sub-session onto one positioning module (integrated GPS, A-GPS, a Bluetooth receiver, or the
 * network). `lbs.dll` loads on this handset — the devdump sweep says so — and `lbs.dso` exports
 * everything used here.
 *
 * WHY THIS IS AN ACTIVE OBJECT, WHICH IS NOT A STYLE CHOICE
 *
 * NotifyPositionUpdate is asynchronous and a fix takes seconds to minutes. `User::WaitForRequest`
 * on it would be the mistake shim_tele.cpp's header documents at length and shim_process.cpp paid
 * for: on a thread with a running CActiveScheduler, waiting consumes whatever completes next —
 * possibly a completion belonging to another of this thread's active objects — and the scheduler
 * then dies with a stray-signal panic. That is a kernel panic: no Rust handler, no panic.txt, the
 * process simply is not there any more. On the GUI thread it would additionally freeze the whole
 * device for the length of a fix, because the window server is waiting on us.
 *
 * So this takes the shape every other asynchronous subsystem in this shim has: a CActive whose
 * RunL pushes an event onto the ring buffer and returns. A fix that never comes costs nothing but
 * a pending request.
 *
 * SetRequestor IS NOT OPTIONAL
 *
 * The framework refuses NotifyPositionUpdate with KErrAccessDenied unless the client has declared
 * who is asking — that declaration is what a privacy-aware platform shows the user. It is a
 * documented precondition on RPositioner::NotifyPositionUpdate, and forgetting it presents as a
 * permission error that looks like a missing capability and is not.
 *
 * ONE POSITIONER, NOT A SLOT TABLE
 *
 * Unlike shim_image.cpp and shim_net.cpp, which hand out handles because a caller genuinely wants
 * several decodes or sockets at once, there is exactly one device position and one process asking
 * for it. A second sub-session would be a second subscription to the same GPS, paying its power
 * cost twice. So the state is a singleton and the ABI carries no handle.
 *
 * SATELLITE INFO IS A REQUEST-TIME DECISION, AND THE CALLER MAKES IT
 *
 * TPositionSatelliteInfo carries the satellite counts and DoP; TPositionInfo carries only the
 * position. Which one a given module accepts is a property of that module, and this file does not
 * guess: `want_satellites` is a parameter, and the GPS probe exists to measure which one this
 * handset answers. A shim that silently retried with a different class would hide exactly the
 * answer the probe is for.
 */

#include "shim_priv.h"

#ifdef SHIM_USE_LBS

#include <e32base.h>
#include <e32std.h>
#include <lbs.h>
#include <lbssatellite.h>

namespace {

/* Who is asking, shown to the user by a privacy-aware framework. ERequestorService rather than
 * ERequestorContact because the requestor is this application, not a person it is acting for. */
_LIT(KRequestorName, "epoc SDK");

/* Microseconds per millisecond, because every interval in this ABI is milliseconds (the rest of
 * the shim's timers are) and every interval in LBS is TTimeIntervalMicroSeconds. */
const TInt64 KUsPerMs = 1000;

class CShimGps : public CActive
    {
public:
    static CShimGps* NewL(TInt aIntervalMs, TInt aTimeoutMs, TBool aWantSatellites,
                          TInt aModuleUid);
    ~CShimGps();

    /* The last completed update, whether or not it was a fix. Returns the status that came with
     * it, so a caller that reads before any completion gets KErrNotReady rather than zeroes it
     * would mistake for the Gulf of Guinea. */
    TInt Read(double* aLat, double* aLon, double* aAlt,
              double* aHAcc, double* aVAcc, int32_t* aSats, int32_t* aInView) const;

private:
    CShimGps(TInt aIntervalMs, TBool aWantSatellites);
    void ConstructL(TInt aTimeoutMs, TInt aModuleUid);
    void Arm();

    void RunL();
    void DoCancel();

private:
    RPositionServer iServer;
    RPositioner iPositioner;
    TBool iServerOpen;
    TBool iPositionerOpen;

    /* Both live for the life of the object because NotifyPositionUpdate writes into whichever one
     * was handed to it, asynchronously — a descriptor on the stack of the function that armed the
     * request is a buffer the framework writes into after that stack frame is gone. */
    TPositionInfo iPlain;
    TPositionSatelliteInfo iSatellite;
    const TBool iWantSatellites;

    const TInt iIntervalMs;
    TBool iHaveResult;
    TInt iLastStatus;
    };

CShimGps* gGps = NULL;

CShimGps::CShimGps(TInt aIntervalMs, TBool aWantSatellites)
    : CActive(EPriorityStandard),
      iServerOpen(EFalse),
      iPositionerOpen(EFalse),
      iWantSatellites(aWantSatellites),
      iIntervalMs(aIntervalMs),
      iHaveResult(EFalse),
      iLastStatus(KErrNotReady)
    {
    CActiveScheduler::Add(this);
    }

CShimGps::~CShimGps()
    {
    Cancel();
    /* Sub-session before session: the positioner belongs to the server, and closing the server
     * first leaves the framework holding a sub-session whose owner is gone. */
    if (iPositionerOpen)
        iPositioner.Close();
    if (iServerOpen)
        iServer.Close();
    }

CShimGps* CShimGps::NewL(TInt aIntervalMs, TInt aTimeoutMs, TBool aWantSatellites,
                         TInt aModuleUid)
    {
    CShimGps* self = new (ELeave) CShimGps(aIntervalMs, aWantSatellites);
    CleanupStack::PushL(self);
    self->ConstructL(aTimeoutMs, aModuleUid);
    CleanupStack::Pop(self);
    return self;
    }

void CShimGps::ConstructL(TInt aTimeoutMs, TInt aModuleUid)
    {
    User::LeaveIfError(iServer.Connect());
    iServerOpen = ETrue;

    /* Module zero means the default: the framework picks by its own criteria. A caller that names
     * a UID gets that module and no other, which is the difference between "where am I" and "where
     * am I, roughly, in the next few seconds".
     *
     * The distinction is worth an ABI parameter because this handset reports four modules with
     * wildly different bargains — measured by the GPS probe:
     *
     *   Integrated GPS  0x101fe98a   80 s to a first fix, 10 m
     *   Network based   0x10206915   12 s,               200 m
     *
     * A map wants the second one while it is opening and the first one while it is being used.
     * Asking the framework for "a position" and hoping cannot express that. */
    if (aModuleUid != 0)
        {
        TUid uid;
        uid.iUid = aModuleUid;
        User::LeaveIfError(iPositioner.Open(iServer, uid));
        }
    else
        {
        User::LeaveIfError(iPositioner.Open(iServer));
        }
    iPositionerOpen = ETrue;

    User::LeaveIfError(iPositioner.SetRequestor(
        CRequestor::ERequestorService, CRequestor::EFormatApplication, KRequestorName));

    TPositionUpdateOptions options;
    /* A non-zero interval is what turns this into a stream: the framework paces the module and the
     * client re-issues on every completion. Zero means one-shot, and Arm() honours that by not
     * re-arming — otherwise a one-shot would become a spin against the GPS. */
    options.SetUpdateInterval(TTimeIntervalMicroSeconds(iIntervalMs * KUsPerMs));
    if (aTimeoutMs > 0)
        options.SetUpdateTimeOut(TTimeIntervalMicroSeconds(aTimeoutMs * KUsPerMs));
    /* Partial updates refused. A partial update completes with KPositionPartialUpdate and carries
     * a position the module itself will not vouch for; a map that draws it puts the user somewhere
     * they are not, with no way to tell. The cost of refusing is waiting longer for the first fix,
     * which is honest. */
    options.SetAcceptPartialUpdates(EFalse);
    User::LeaveIfError(iPositioner.SetUpdateOptions(options));

    Arm();
    }

void CShimGps::Arm()
    {
    if (IsActive())
        return;
    if (iWantSatellites)
        iPositioner.NotifyPositionUpdate(iSatellite, iStatus);
    else
        iPositioner.NotifyPositionUpdate(iPlain, iStatus);
    SetActive();
    }

void CShimGps::RunL()
    {
    iLastStatus = iStatus.Int();
    iHaveResult = ETrue;

    TPosition pos;
    if (iLastStatus == KErrNone)
        {
        if (iWantSatellites)
            iSatellite.GetPosition(pos);
        else
            iPlain.GetPosition(pos);
        }

    ShimEvent e;
    e.kind = SHIM_EV_GPS_FIX;
    e.handle = 0;
    e.status = iLastStatus;
    /* Satellites used, and horizontal accuracy rounded to whole metres. Both are summaries for a
     * caller deciding whether to redraw at all; shim_gps_read carries the real numbers. -1 rather
     * than 0 for "not reported", because zero satellites and no satellite field are different
     * facts and a status bar that shows the first when it means the second is lying. */
    e.a = (iLastStatus == KErrNone && iWantSatellites) ? iSatellite.NumSatellitesUsed() : -1;
    e.b = (iLastStatus == KErrNone) ? (TInt) pos.HorizontalAccuracy() : -1;
    e.c = iWantSatellites ? 1 : 0;
    e.d = 0;
    e.native = 0;
    ShimPushEvent(e);

    /* Re-arm only for a stream. On an error this keeps the subscription alive on purpose: a
     * KErrTimedOut in a tunnel is not the end of the journey, and the framework's own update
     * interval is what stops this becoming a busy loop. */
    if (iIntervalMs > 0)
        Arm();
    }

void CShimGps::DoCancel()
    {
    if (iPositionerOpen)
        iPositioner.CancelRequest(EPositionerNotifyPositionUpdate);
    }

TInt CShimGps::Read(double* aLat, double* aLon, double* aAlt,
                    double* aHAcc, double* aVAcc, int32_t* aSats, int32_t* aInView) const
    {
    if (!iHaveResult)
        return KErrNotReady;
    if (iLastStatus != KErrNone)
        return iLastStatus;

    TPosition pos;
    if (iWantSatellites)
        iSatellite.GetPosition(pos);
    else
        iPlain.GetPosition(pos);

    if (aLat)
        *aLat = pos.Latitude();
    if (aLon)
        *aLon = pos.Longitude();
    /* Altitude and the accuracies are TReal32 and may be NaN when the module does not report
     * them. Widened rather than reinterpreted: the caller checks for NaN, which survives the
     * conversion, instead of this file inventing a sentinel. */
    if (aAlt)
        *aAlt = (double) pos.Altitude();
    if (aHAcc)
        *aHAcc = (double) pos.HorizontalAccuracy();
    if (aVAcc)
        *aVAcc = (double) pos.VerticalAccuracy();
    if (aSats)
        *aSats = iWantSatellites ? iSatellite.NumSatellitesUsed() : -1;
    if (aInView)
        *aInView = iWantSatellites ? iSatellite.NumSatellitesInView() : -1;
    return SHIM_OK;
    }

} /* namespace */

void ShimLbsCleanup()
    {
    delete gGps;
    gGps = NULL;
    }

extern "C" {

int32_t shim_gps_start(int32_t interval_ms, int32_t timeout_ms, int32_t want_satellites,
                       int32_t module_uid)
    {
    if (interval_ms < 0 || timeout_ms < 0)
        return SHIM_ERR_ARGUMENT;
    if (gGps)
        return SHIM_ERR_ALREADY_EXISTS;

    CShimGps* gps = NULL;
    TInt err = KErrNone;
    TRAP(err, gps = CShimGps::NewL(interval_ms, timeout_ms, want_satellites != 0, module_uid));
    if (err != KErrNone)
        return err;
    if (!gps)
        return SHIM_ERR_GENERAL;

    gGps = gps;
    return SHIM_OK;
    }

void shim_gps_stop(void)
    {
    ShimLbsCleanup();
    }

int32_t shim_gps_read(double* lat, double* lon, double* alt,
                      double* h_acc, double* v_acc, int32_t* sats, int32_t* in_view)
    {
    if (!gGps)
        return SHIM_ERR_NOT_READY;
    return gGps->Read(lat, lon, alt, h_acc, v_acc, sats, in_view);
    }

int32_t shim_gps_module_count(int32_t* out)
    {
    if (!out)
        return SHIM_ERR_ARGUMENT;
    *out = 0;

    /* Its own short-lived session on purpose. The inventory is a question about the framework, not
     * about a fix, and a caller must be able to ask it before deciding to start anything — which
     * includes deciding not to. */
    RPositionServer server;
    TInt err = server.Connect();
    if (err != KErrNone)
        return err;

    TUint count = 0;
    err = server.GetNumModules(count);
    server.Close();
    if (err != KErrNone)
        return err;

    *out = (int32_t) count;
    return SHIM_OK;
    }

int32_t shim_gps_module_info(int32_t index, uint16_t* name, int32_t name_cap,
                             int32_t* name_len, int32_t* out, int32_t out_cap)
    {
    if (index < 0 || !out || out_cap < 10)
        return SHIM_ERR_ARGUMENT;
    if (name_len)
        *name_len = 0;

    RPositionServer server;
    TInt err = server.Connect();
    if (err != KErrNone)
        return err;

    TPositionModuleInfo info;
    err = server.GetModuleInfoByIndex((TInt) index, info);
    if (err != KErrNone)
        {
        server.Close();
        return err;
        }

    TPositionQuality quality;
    info.GetPositionQuality(quality);

    TBuf<KPositionMaxModuleName> moduleName;
    info.GetModuleName(moduleName);

    out[0] = (int32_t) info.ModuleId().iUid;
    out[1] = info.IsAvailable() ? 1 : 0;
    out[2] = (int32_t) info.TechnologyType();
    out[3] = (int32_t) info.DeviceLocation();
    out[4] = (int32_t) quality.CostIndicator();
    out[5] = (int32_t) quality.PowerConsumption();
    /* Accuracies in millimetres and times in milliseconds, so the whole inventory crosses the ABI
     * as integers. A module that does not report one answers NaN, which converts to nothing
     * meaningful — hence the explicit -1. */
    const TReal32 hAcc = quality.HorizontalAccuracy();
    const TReal32 vAcc = quality.VerticalAccuracy();
    out[6] = (hAcc == hAcc) ? (int32_t) (hAcc * 1000.0f) : -1;
    out[7] = (vAcc == vAcc) ? (int32_t) (vAcc * 1000.0f) : -1;
    out[8] = (int32_t) (quality.TimeToFirstFix().Int64() / KUsPerMs);
    out[9] = (int32_t) (quality.TimeToNextFix().Int64() / KUsPerMs);

    server.Close();

    if (name && name_cap > 0)
        {
        const TInt n = Min(name_cap, moduleName.Length());
        for (TInt i = 0; i < n; i++)
            name[i] = moduleName[i];
        if (name_len)
            *name_len = n;
        /* An overflow is reported, not truncated silently: a module name that came back cut in
         * half is a fact the probe's report should carry. */
        if (moduleName.Length() > name_cap)
            return SHIM_ERR_OVERFLOW;
        }

    return SHIM_OK;
    }

} /* extern "C" */

#endif /* SHIM_USE_LBS */
