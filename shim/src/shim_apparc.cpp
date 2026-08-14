/* Enumerate and launch installed applications, for a launcher.
 *
 * process.cpp launches an executable by its path with RProcess::Create — right for a GUI app
 * starting its own known daemon, wrong for a launcher, which knows applications by UID and must
 * discover the ones it did not ship. That is AppArc's job: RApaLsSession is the application
 * architecture server's client side, the same registry the native menu reads, so an app listed
 * here is an app the phone itself would show, and StartApp hands it to the platform to launch
 * exactly as the shell would — with the document handling, splash and single-instance policy the
 * app registered for, none of which RProcess::Create would honour.
 *
 * The enumeration is cached between refresh() and at(): GetAllApps primes the server-side scan and
 * GetNextApp walks it once, so reading it by index later cannot re-walk a cursor that has already
 * advanced. This file compiles into an EXE (the launcher), never a DLL, so its file-static cache is
 * allowed — the no-writable-static rule is the loader's, and it applies only to polymorphic DLLs.
 *
 * No capability is required to list or start applications: launching is not a privileged act, and
 * the started app runs with whatever its own image was signed for, not the launcher's.
 */

#include "shim_priv.h"

#ifdef SHIM_USE_APPARC

#include <e32std.h>
#include <e32base.h>
#include <apgcli.h>     /* RApaLsSession, TApaAppInfo */
#include <apacmdln.h>   /* CApaCommandLine, TApaAppCapabilityBuf */
#include <apadef.h>     /* EApaCommandRun */
#include <apgtask.h>    /* TApaTaskList, TApaTask */
#include <coemain.h>    /* CCoeEnv, for the window-server session */
#include <apgwgnam.h>   /* CApaWindowGroupName — decodes a window group's application UID */

/* The app-icon path is a SEPARATE gate from the rest of AppArc, and deliberately so. GetAppIcon and
 * CApaMaskedBitmap are apgrfx imports the E72 does not satisfy — linking them into a binary makes
 * the WHOLE image fail to load silently (the same trap USE_MSG sprang, see the project memory on
 * isolating risky imports). Every other AppArc call here (GetAllApps, GetAppInfo, StartApp…) is
 * present on the handset and safe; only the icon calls are quarantined behind SHIM_USE_APPICON so a
 * launcher can use AppArc to list and launch without importing a symbol that would brick it. Only a
 * binary that opts into USE_APPICON (an isolated icon probe/daemon) pays the load risk. */
#ifdef SHIM_USE_APPICON
#include <apgicnfl.h>   /* CApaMaskedBitmap — the colour+mask pair GetAppIcon fills */
#include <fbs.h>        /* CFbsBitmap (CApaMaskedBitmap's base, and its Mask() plane) */
#include <gdi.h>        /* TRgb, for the per-pixel read */
#endif

/* Kept in step with ShimAppInfo::caption in symbian_shim.h. Captions are TBuf<256> on the
 * platform; 64 is more than any menu entry needs and keeps the POD small. */
static const TInt KCaptionMax = 64;

struct AppEntry
    {
    TUint32 iUid3;
    TBool   iHidden;
    TBool   iSystem;   /* a control-panel item (Settings/options), not a standalone program */
    TInt    iCaptionLen;
    TUint16 iCaption[KCaptionMax];
    };

/* One phone has on the order of a hundred apps; 256 is headroom, and a fixed array spares us an
 * RArray whose OOM path would have to be handled inside a refresh that must not leave. */
static const TInt KMaxApps = 256;
static AppEntry gApps[KMaxApps];
static TInt gCount = 0;
static RApaLsSession gLs;
static TBool gLsOpen = EFalse;

static TInt EnsureSession()
    {
    if (gLsOpen)
        return KErrNone;
    TInt rc = gLs.Connect();
    if (rc != KErrNone)
        return rc;
    gLsOpen = ETrue;
    return KErrNone;
    }

extern "C" {

/* Re-scan the installed applications into the cache. Returns the count (>= 0) or a negative
 * Symbian error. GetNextApp stops at the first non-KErrNone, so a mid-scan failure keeps whatever
 * was read rather than discarding it. */
int32_t shim_apps_refresh(void)
    {
    TInt rc = EnsureSession();
    if (rc != KErrNone)
        return rc;

    rc = gLs.GetAllApps();
    if (rc != KErrNone)
        return rc;

    gCount = 0;
    for (;;)
        {
        if (gCount >= KMaxApps)
            break;
        TApaAppInfo info;
        if (gLs.GetNextApp(info) != KErrNone)
            break;

        AppEntry& e = gApps[gCount];
        e.iUid3 = info.iUid.iUid;

        TInt n = info.iCaption.Length();
        if (n > KCaptionMax)
            n = KCaptionMax;
        for (TInt i = 0; i < n; i++)
            e.iCaption[i] = info.iCaption[i];
        e.iCaptionLen = n;

        /* Hidden is advisory here and load-bearing only in a later increment; a capability query
         * that fails must not drop the app from the list, so default to visible. */
        e.iHidden = EFalse;
        e.iSystem = EFalse;
        TApaAppCapabilityBuf cap;
        if (gLs.GetAppCapability(cap, info.iUid) == KErrNone)
            {
            e.iHidden = cap().iAppIsHidden;
            /* Control-panel items are the phone's "options" (Settings sub-panels), registered as
             * apps but not standalone programs. The launcher filters these out by default. */
            e.iSystem = (cap().iAttributes & TApaAppCapability::EControlPanelItem) != 0;
            }

        gCount++;
        }

    return gCount;
    }

/* How many apps the last refresh found. Zero before the first refresh. */
int32_t shim_apps_count(void)
    {
    return gCount;
    }

/* Copy one cached entry out by index. uid3 and hidden are always written; the caption is copied up
 * to `cap` units with the count returned in *caption_len. SHIM_ERR_NOT_FOUND for a bad index. */
int32_t shim_app_at(int32_t index, uint32_t* uid3, uint8_t* hidden,
                    uint16_t* caption, int32_t cap, int32_t* caption_len)
    {
    if (index < 0 || index >= gCount)
        return SHIM_ERR_NOT_FOUND;

    const AppEntry& e = gApps[index];
    if (uid3)
        *uid3 = e.iUid3;
    /* Flags packed into one byte to keep the ABI stable: bit 0 = hidden, bit 1 = system (control
     * panel item). The Rust side decodes both. */
    if (hidden)
        *hidden = (e.iHidden ? 1 : 0) | (e.iSystem ? 2 : 0);

    TInt n = e.iCaptionLen;
    if (caption && cap > 0)
        {
        if (n > cap)
            n = cap;
        for (TInt i = 0; i < n; i++)
            caption[i] = e.iCaption[i];
        }
    else
        {
        n = 0;
        }
    if (caption_len)
        *caption_len = n;

    return SHIM_OK;
    }

static void DoLaunchL(TUint32 aUid3)
    {
    TApaAppInfo info;
    User::LeaveIfError(gLs.GetAppInfo(info, TUid::Uid(aUid3)));

    CApaCommandLine* cmd = CApaCommandLine::NewLC();
    cmd->SetExecutableNameL(info.iFullName);
    cmd->SetCommandL(EApaCommandRun);
    User::LeaveIfError(gLs.StartApp(*cmd));
    CleanupStack::PopAndDestroy(cmd);
    }

/* Start the installed application with this UID3, the way the native shell would. SHIM_OK once the
 * platform has accepted the launch. The TRAP is the boundary: NewLC and the SetL calls can leave,
 * and a leave crossing extern "C" is undefined, so it stops here and becomes a code. */
int32_t shim_app_launch(uint32_t uid3)
    {
    TInt rc = EnsureSession();
    if (rc != KErrNone)
        return rc;

    TRAPD(err, DoLaunchL(uid3));
    return err == KErrNone ? SHIM_OK : err;
    }

/* Kill the installed app with this UID3 through the window server — the way to stop an app that
 * will not close itself, which is exactly a resident launcher. TApaTaskList::FindApp matches by
 * application UID and KillTask asks the window server to terminate it; going through the window
 * server rather than RProcess::Kill is what lets one app stop another without owning it. SHIM_OK
 * if a task was found and killed, SHIM_ERR_NOT_FOUND if the app has no running task, or
 * SHIM_ERR_NOT_READY before the window-server session exists. */
int32_t shim_app_kill(uint32_t uid3)
    {
    CCoeEnv* env = CCoeEnv::Static();
    if (!env)
        return SHIM_ERR_NOT_READY;
    TApaTaskList list(env->WsSession());
    TApaTask task = list.FindApp(TUid::Uid(uid3));
    if (!task.Exists())
        return SHIM_ERR_NOT_FOUND;
    task.KillTask();
    return SHIM_OK;
    }

/* Fill aOut with the UID3s of running applications (window-server groups, front-to-back Z order),
 * deduped, up to aCap; aCount gets the number written. The TRAP at the extern boundary catches a
 * leave from WindowGroupList, the id-array allocation, or NewLC. A window group with no application
 * UID (a raw window-server client) decodes to zero and is skipped. */
static void DoRunningL(RWsSession& aWs, TInt aOwnGroupId, uint32_t* aOut, TInt aCap, TInt& aCount)
    {
    aCount = 0;

    CArrayFixFlat<TInt>* ids = new (ELeave) CArrayFixFlat<TInt>(16);
    CleanupStack::PushL(ids);
    User::LeaveIfError(aWs.WindowGroupList(ids));

    const TInt n = ids->Count();
    for (TInt i = 0; i < n && aCount < aCap; i++)
        {
        const TInt wgId = (*ids)[i];
        /* Never list our own window group. The launcher is usually the front-most group when this is
         * called (the user just brought it forward), so leaving it in put the home at the top of the
         * task switcher — where a "kill" then killed the home itself. Excluding by group id is exact,
         * unlike matching the app UID, which did not reliably identify us. */
        if (wgId == aOwnGroupId)
            continue;
        CApaWindowGroupName* wgn = CApaWindowGroupName::NewLC(aWs, wgId);
        const TUint32 uid = (TUint32) wgn->AppUid().iUid;
        CleanupStack::PopAndDestroy(wgn);

        if (uid == 0)
            continue;

        TBool seen = EFalse;
        for (TInt j = 0; j < aCount; j++)
            if (aOut[j] == uid)
                {
                seen = ETrue;
                break;
                }
        if (seen)
            continue;

        aOut[aCount++] = uid;
        }

    CleanupStack::PopAndDestroy(ids);
    }

/* List the UID3s of the apps running right now — the window server's task list, front-to-back Z
 * order (most recent first), deduplicated. Up to `cap` into `out`; returns the count (>= 0) or a
 * negative error. SHIM_ERR_NOT_READY before a window-server session exists. Needs no RApaLsSession:
 * the window server already knows who is running. New imports are ws32's WindowGroupList and
 * apgrfx's CApaWindowGroupName — apgrfx is already exercised on-device via TApaTask::KillTask. */
int32_t shim_apps_running(uint32_t* out, int32_t cap)
    {
    CCoeEnv* env = CCoeEnv::Static();
    if (!env)
        return SHIM_ERR_NOT_READY;
    if (!out || cap < 0)
        cap = 0;

    RWsSession& ws = env->WsSession();
    const TInt ownGroup = env->RootWin().Identifier();
    TInt count = 0;
    TRAPD(err, DoRunningL(ws, ownGroup, out, cap, count));
    if (err != KErrNone)
        return err;
    return count;
    }

#ifdef SHIM_USE_APPICON

/* The widest icon row we buffer, in pixels. S60 app icons top out well under this; a wider bitmap
 * is refused (KErrTooBig) rather than smashing the stack row. Two bytes per pixel for the EColor64K
 * colour row is the larger of the two temp uses. */
static const TInt KMaxIconRow = 256;

/* Fill aRgb (RGB565) and aMask (8-bit coverage) with this app's icon at aSize pixels, the same icon
 * the native menu draws. GetAppIcon hands back a CApaMaskedBitmap — a colour plane plus a mask
 * plane — exactly the shape symbian-gfx's blit_icon consumes.
 *
 * Pixels are read with GetScanLine, not GetPixel — and that is the whole reason this file exists as
 * a separate build. GDesk, a home screen that works on this exact handset, reads icon pixels with
 * CFbsBitmap::GetScanLine (fbscli ord 109/110) and never imports GetPixel (ord 131); its E32 import
 * table is the evidence. An earlier version here used GetPixel and the image would not run. So this
 * mirrors the proven path: one GetScanLine per row, asking the font-and-bitmap server for EColor64K
 * (already RGB565) for the colour plane and EGray256 (one coverage byte per pixel) for the mask,
 * which does the display-mode conversion server-side and copies out as bytes — no per-pixel call,
 * no alignment assumption. */
static void DoIconL(TUint32 aUid3, TInt aSize,
                    TUint16* aRgb, TUint8* aMask, TInt aCap, TInt* aW, TInt* aH)
    {
    if (aSize <= 0)
        User::Leave(KErrArgument);
    if (aSize > KMaxIconRow)
        User::Leave(KErrTooBig);
    if (aCap < aSize * aSize)
        User::Leave(KErrOverflow);

    /* Detect non-MBM (MIF/scalable) icons the safe way, BEFORE touching GetAppIcon. The SDK docs
     * are explicit: GetAppIconSizes returns KErrNotSupported "if the application provides icons in
     * non-MBM format", and GetAppIcon on such an app *panics the caller* (measured on the E72 —
     * GDesk, Quickword, Email all closed the probe here) rather than failing cleanly. So ask the
     * question that answers with an error, not a panic; a non-KErrNone result leaves, and the caller
     * draws the caption. Only plain-bitmap apps reach GetAppIcon below. */
    CArrayFixFlat<TSize>* sizes = new (ELeave) CArrayFixFlat<TSize>(4);
    CleanupStack::PushL(sizes);
    User::LeaveIfError(gLs.GetAppIconSizes(TUid::Uid(aUid3), *sizes));
    CleanupStack::PopAndDestroy(sizes);

    /* NewLC() creates a fresh empty masked bitmap. The other overload, NewL(const* aSourceIcon),
     * *copies* its argument — passing NULL there dereferences it (KERN-EXEC 3), which is what closed
     * the launcher a few seconds after it opened. NewLC is apgrfx ordinal 33; GDesk does not import
     * it, but that only means GDesk did not need it, not that the handset lacks it. This build is
     * the clean test of whether ordinal 33 is present. */
    CApaMaskedBitmap* bmp = CApaMaskedBitmap::NewLC();

    /* The TSize overload (apgrfx ord 144, which GDesk uses) scales the icon to exactly this size, so
     * width and height are known here without CFbsBitmap::SizeInPixels (fbscli ord 116 — another
     * ordinal GDesk avoids). We asked for a square, we get a square. */
    const TInt w = aSize;
    const TInt h = aSize;
    const TSize want(w, h);
    User::LeaveIfError(gLs.GetAppIcon(TUid::Uid(aUid3), want, *bmp));
    if (aW)
        *aW = w;
    if (aH)
        *aH = h;

    TUint8 line[KMaxIconRow * 2];

    /* Colour plane, RGB565 straight from the server. EColor64K is 16bpp little-endian RGB565, which
     * is exactly aRgb's layout, so the row is a flat byte copy. */
    for (TInt y = 0; y < h; y++)
        {
        TPtr8 row(line, sizeof(line));
        bmp->GetScanLine(row, TPoint(0, y), w, EColor64K);
        Mem::Copy(aRgb + y * w, line, w * 2);
        }

    /* Mask plane: fully opaque, on purpose. The natural call is CApaMaskedBitmap::Mask() (apgrfx
     * ord 183), but that is the one icon symbol the E72 does not carry — a device test bricked the
     * load with it and loaded without it, and GDesk (which runs here) never imports it either. So
     * the mask is dropped: icons draw with a solid rectangle rather than a cut-out. Transparency
     * will come back through AknIconUtils (aknicon.dll, which GDesk does import) rather than the
     * masked-bitmap accessor — a follow-up, not this load-fix. */
    Mem::Fill(aMask, w * h, 255);
    CleanupStack::PopAndDestroy(bmp);
    }

/* Diagnostic variant B: the OTHER GetAppIcon overload — the TInt one (apgrfx ord 145), which
 * selects a pre-rendered icon size rather than scaling to a TSize. The colour is filled solid green
 * instead of read, so this isolates one question only: does GetAppIcon(TInt) itself panic on the
 * apps whose icons (MIF/scalable) crash the TSize path? Green squares in the probe mean "no" — the
 * scaling was the culprit; the probe vanishing means GetAppIcon panics regardless of overload. */
static void DoIconBL(TUint32 aUid3, TInt aSize, TUint16* aRgb, TUint8* aMask, TInt aCap,
                     TInt* aW, TInt* aH)
    {
    if (aSize <= 0)
        User::Leave(KErrArgument);
    if (aCap < aSize * aSize)
        User::Leave(KErrOverflow);

    CApaMaskedBitmap* bmp = CApaMaskedBitmap::NewLC();
    /* Index 0, NOT aSize: this overload's TInt is the icon-size *index* (0,1,2…), not a pixel count.
     * Passing 44 indexed out of range and panicked every app; index 0 asks for the natively
     * registered icon with no scaling — and scaling is the suspected trigger of the MIF-icon crash
     * in the TSize path. Green fill (not GetScanLine) so this isolates the GetAppIcon(index) call. */
    User::LeaveIfError(gLs.GetAppIcon(TUid::Uid(aUid3), 0, *bmp));
    const TInt w = aSize;
    const TInt h = aSize;
    if (aW)
        *aW = w;
    if (aH)
        *aH = h;
    for (TInt i = 0; i < w * h; i++)
        aRgb[i] = 0x07E0; /* green */
    Mem::Fill(aMask, w * h, 255);
    CleanupStack::PopAndDestroy(bmp);
    }

#endif // SHIM_USE_APPICON (icon helper — shim_app_icon itself is always defined)

/* Fetch app aUid3's icon at aSize into caller buffers. *w/*h are written whenever the bitmap size
 * is known (so a caller can right-size a retry), and SHIM_ERR_OVERFLOW says the buffers were too
 * small. Any other non-zero is the platform's own error (e.g. KErrNotFound for an app with no
 * icon), and the launcher simply falls back to the caption for that app. */
int32_t shim_app_icon(uint32_t uid3, int32_t size,
                      uint16_t* rgb_out, uint8_t* mask_out, int32_t cap,
                      int32_t* w, int32_t* h)
    {
    if (w)
        *w = 0;
    if (h)
        *h = 0;
    if (!rgb_out || !mask_out || size <= 0)
        return SHIM_ERR_NOT_SUPPORTED;

#ifdef SHIM_USE_APPICON
    TInt rc = EnsureSession();
    if (rc != KErrNone)
        return rc;

    TRAPD(err, DoIconL(uid3, size, rgb_out, mask_out, cap, w, h));
    if (err == KErrOverflow)
        return SHIM_ERR_OVERFLOW;
    return err == KErrNone ? SHIM_OK : err;
#else
    /* The icon path is not compiled into this binary — see the SHIM_USE_APPICON note above. The
     * launcher takes this branch and simply draws captions; no apgrfx icon symbol is imported, so
     * the image loads. */
    (void) uid3;
    (void) size;
    (void) cap;
    return SHIM_ERR_NOT_SUPPORTED;
#endif
    }

/* Diagnostic variant B — see DoIconBL. Same ABI as shim_app_icon. */
int32_t shim_app_icon_b(uint32_t uid3, int32_t size,
                        uint16_t* rgb_out, uint8_t* mask_out, int32_t cap,
                        int32_t* w, int32_t* h)
    {
    if (w)
        *w = 0;
    if (h)
        *h = 0;
    if (!rgb_out || !mask_out || size <= 0)
        return SHIM_ERR_NOT_SUPPORTED;

#ifdef SHIM_USE_APPICON
    TInt rc = EnsureSession();
    if (rc != KErrNone)
        return rc;
    TRAPD(err, DoIconBL(uid3, size, rgb_out, mask_out, cap, w, h));
    if (err == KErrOverflow)
        return SHIM_ERR_OVERFLOW;
    return err == KErrNone ? SHIM_OK : err;
#else
    (void) uid3;
    (void) size;
    (void) cap;
    return SHIM_ERR_NOT_SUPPORTED;
#endif
    }

} // extern "C"

#endif // SHIM_USE_APPARC
