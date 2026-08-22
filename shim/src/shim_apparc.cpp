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
#endif

/* Shared by both icon routes: the bitmap type whose rows they read, and the display-mode constants
 * they ask the server to convert to. Neither header is an icon-specific risk — fbscli and gdi are
 * in every app's base library set (tools/symbuild) — so they sit outside the gates. */
#if defined(SHIM_USE_APPICON) || defined(SHIM_USE_AKNICON)
#include <fbs.h>        /* CFbsBitmap — GetScanLine lives here */
#include <gdi.h>        /* TRgb and the TDisplayMode constants */
#endif

/* A third gate, nested one level deeper than SHIM_USE_APPICON, for the icon path that goes through
 * Avkon rather than AppArc's masked bitmap. It adds a whole new library import (aknicon), so by the
 * rule in docs/device-notes.md — an import that does not resolve makes the image vanish, with no
 * panic and no log — it gets its own switch and lands in the isolated probe first, never straight
 * in the resident launcher. aknicon.dll is present on the E72 (measured, docs/device-dump.txt) and
 * aknicon.dso ships in the vendored S60 3.2 SDK, so this is an ordinary link, not the .dso-from-
 * firmware synthesis the repo elsewhere calls its riskiest line. */
#ifdef SHIM_USE_AKNICON
#include <AknIconUtils.h> /* AknIconUtils::CreateIconL / SetSize, and TScaleMode */
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

#ifdef SHIM_USE_LAUNCH_DOC

/* Launching an app AT something — a URL — rather than merely launching it.
 *
 * A separate gate from the rest of AppArc for the usual reason, and with a twist worth stating: the
 * calls below are all apgrfx, the same library `StartApp` already uses, so the *load* risk is low.
 * What is unknown is whether any of them does anything useful on this firmware. A native S60
 * browser is asked to open a URL by a convention, not by an API — there is no `OpenUrl` — and which
 * convention a given handset honours is a question only the handset answers. So all four routes are
 * compiled and the probe tries each, rather than one being chosen here on faith.
 *
 * Routes, in the order a probe should try them:
 *
 *   0  document name   CApaCommandLine::SetDocumentNameL, then StartApp at an explicit UID.
 *                      The documented way to say "open this app on this thing".
 *   1  tail end        SetTailEndL("4 <url>") — the S60 browser's own convention, where 4 is the
 *                      "open URL" command in its private command set. Undocumented and the one most
 *                      likely to actually work on the E72.
 *   2  StartDocument   RApaLsSession::StartDocument(url, TUid, ...) — explicit app, platform builds
 *                      the command line.
 *   3  resolve         RApaLsSession::StartDocument(url, ...) with no UID, letting the platform pick
 *                      the handler. Expected to fail for a URL, which is not a file and has no
 *                      recognizer, but it is the one route that would make a scheme registry
 *                      unnecessary — worth one call to find out.
 *
 * The URL arrives as UTF-16 because that is what a Symbian descriptor is and what every other
 * string in this shim already uses (see shim_file_open). */
static void DoLaunchDocL(TUint32 aUid3, const TDesC& aDoc, TInt aRoute)
    {
    TThreadId tid;

    if (aRoute == 2)
        {
        User::LeaveIfError(gLs.StartDocument(const_cast<TDesC&>(aDoc), TUid::Uid(aUid3), tid));
        return;
        }
    if (aRoute == 3)
        {
        User::LeaveIfError(gLs.StartDocument(const_cast<TDesC&>(aDoc), tid));
        return;
        }

    TApaAppInfo info;
    User::LeaveIfError(gLs.GetAppInfo(info, TUid::Uid(aUid3)));

    CApaCommandLine* cmd = CApaCommandLine::NewLC();
    cmd->SetExecutableNameL(info.iFullName);
    if (aRoute == 1)
        {
        /* The tail end is 8-bit and the URL is 16: narrowed a character at a time rather than
         * through CnvUtfConverter, which would drag charconv in for a string that is ASCII by
         * definition. A non-ASCII byte would be mangled here — and a non-ASCII URL is
         * percent-encoded before it ever reaches this point, so there is none to mangle. */
        HBufC8* tail = HBufC8::NewLC(aDoc.Length() + 2);
        TPtr8 t = tail->Des();
        t.Append(_L8("4 "));
        for (TInt i = 0; i < aDoc.Length(); i++)
            t.Append(static_cast<TUint8>(aDoc[i] & 0xFF));
        cmd->SetCommandL(EApaCommandRun);
        cmd->SetTailEndL(*tail);
        User::LeaveIfError(gLs.StartApp(*cmd));
        CleanupStack::PopAndDestroy(tail);
        }
    else
        {
        cmd->SetDocumentNameL(aDoc);
        cmd->SetCommandL(EApaCommandOpen);
        User::LeaveIfError(gLs.StartApp(*cmd));
        }
    CleanupStack::PopAndDestroy(cmd);
    }

/* Hand a running application a message, the way the shell hands the browser a URL.
 *
 * The route that matters most and the one missing from the first attempt. `StartApp` and
 * `StartDocument` both *start* an application; neither does anything useful to one that is already
 * running, and the browser on this handset usually is. It accepts the launch, brings nothing
 * forward, and the user sees the page that was already open — which is exactly what happened.
 *
 * `TApaTask::SendMessage` is the other half: it delivers a descriptor to a live task's window
 * group. The browser's own protocol is an 8-bit `"<command> <argument>"`, where 4 is "open this
 * URL". The message UID is not read by the browser, so it is zero.
 *
 * SHIM_ERR_NOT_FOUND when the application is not running, which is not an error — it is the
 * caller's signal to start it instead. */
int32_t shim_app_task_message(uint32_t uid3, const uint8_t* msg, int32_t msg_len)
    {
    CCoeEnv* env = CCoeEnv::Static();
    if (!env)
        return SHIM_ERR_NOT_READY;
    if (!msg || msg_len <= 0)
        return SHIM_ERR_ARGUMENT;

    TApaTaskList list(env->WsSession());
    TApaTask task = list.FindApp(TUid::Uid(uid3));
    if (!task.Exists())
        return SHIM_ERR_NOT_FOUND;

    /* Foreground first: a message delivered to a background task changes what it shows without
     * showing it, which reads as nothing having happened. */
    task.BringToForeground();
    TPtrC8 des(reinterpret_cast<const TUint8*>(msg), msg_len);
    task.SendMessage(TUid::Uid(0), des);
    return SHIM_OK;
    }

/* Launch app aUid3 pointed at the document aDoc (a URL), by route aRoute. See DoLaunchDocL for what
 * the routes are and why there is more than one. SHIM_OK once the platform accepted it — which is
 * emphatically not the same as the app having opened the URL, and no API here can tell us that. */
int32_t shim_app_launch_doc(uint32_t uid3, const uint16_t* doc, int32_t doc_len, int32_t route)
    {
    TInt rc = EnsureSession();
    if (rc != KErrNone)
        return rc;
    if (!doc || doc_len <= 0)
        return SHIM_ERR_ARGUMENT;

    TPtrC des(reinterpret_cast<const TUint16*>(doc), doc_len);
    TRAPD(err, DoLaunchDocL(uid3, des, route));
    return err == KErrNone ? SHIM_OK : err;
    }

#else  /* !SHIM_USE_LAUNCH_DOC */

/* The symbol exists in every AppArc build, the implementation only in one. Same discipline as
 * shim_cpu.cpp: gating the *symbol* would make every caller need a matching cargo feature, and a
 * missing symbol fails at link with `--no-undefined` before --gc-sections can sweep it. Gating the
 * body instead keeps the risky calls out of binaries that did not ask for them while leaving the
 * Rust side a single unconditional declaration. */
int32_t shim_app_launch_doc(uint32_t, const uint16_t*, int32_t, int32_t)
    {
    return SHIM_ERR_NOT_SUPPORTED;
    }

#endif // SHIM_USE_LAUNCH_DOC

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
static TInt DoKillL(uint32_t aUid3)
    {
    CCoeEnv* env = CCoeEnv::Static();
    if (!env)
        return SHIM_ERR_NOT_READY;
    TApaTaskList list(env->WsSession());
    TApaTask task = list.FindApp(TUid::Uid(aUid3));
    if (!task.Exists())
        return SHIM_ERR_NOT_FOUND;
    task.KillTask();
    return SHIM_OK;
    }

/* Ask the app with this UID3 to close, through the window server.
 *
 * `EndTask` where `KillTask` kills: it posts the window group a close event and the application
 * exits on its own, which needs no capability at all. That distinction is the whole reason this
 * function exists — see the measurement in shim_app_kill below.
 *
 * The cost, stated: an application that ignores the event stays. That is the trade against a caller
 * that dies, and this is a task switcher, not a supervisor. */
static TInt DoEndL(uint32_t aUid3)
    {
    CCoeEnv* env = CCoeEnv::Static();
    if (!env)
        return SHIM_ERR_NOT_READY;
    TApaTaskList list(env->WsSession());
    TApaTask task = list.FindApp(TUid::Uid(aUid3));
    if (!task.Exists())
        return SHIM_ERR_NOT_FOUND;
    task.EndTask();
    return SHIM_OK;
    }

int32_t shim_app_end(uint32_t uid3)
    {
    TRAPD(err, err = DoEndL(uid3));
    return err;
    }

/* MEASURED, on the E72, 22 August 2026: this **faults the caller**.
 *
 * `TApaTask::KillTask` is `RThread::Kill` on a thread in another process, and that needs PowerMgmt.
 * Without it the kernel does not return an error — a capability violation on an executive call
 * panics the calling thread. So the launcher's task switcher took the launcher down every time
 * somebody closed an app: the log said `[recent] kill … step=ws` and the next line was a fresh
 * process, with no panic file, because a fault is not a panic and not a leave. The TRAP added an
 * hour earlier changed nothing, which is what proved it was not a leave.
 *
 * The misreading that hid it: "the ROM patch grants every capability" is about the *installer's
 * ceiling* — a patched installserver accepts any declaration. It does not hand a process
 * capabilities its own image never declared, and a process holds exactly what its E32 header says.
 *
 * Kept, because it is the right call for an app that declares PowerMgmt and means it. Everything
 * else should use shim_app_end. */
int32_t shim_app_kill(uint32_t uid3)
    {
    /* TRAPped, like every other function in this file, and it was the one that was not.
     *
     * `TApaTaskList::FindApp` walks the window server's group list, which allocates — it is
     * `RWsSession::WindowGroupList` underneath, the same call `DoRunningL` is TRAPped for. A leave
     * crossing an `extern "C"` boundary is undefined behaviour, and on this handset the observable
     * form of that is the whole application disappearing with no panic file and nothing in any log:
     * exactly what the launcher did when an app was closed from the task switcher.
     *
     * Whether that is what killed it is still being measured (the launcher logs each step of its
     * kill path now). This is here either way: the rule this file states in three other places
     * cannot have an exception nobody noticed. */
    TRAPD(err, err = DoKillL(uid3));
    return err;
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

#if defined(SHIM_USE_APPICON) || defined(SHIM_USE_AKNICON)

/* The widest icon row we buffer, in pixels. S60 app icons top out well under this; a wider bitmap
 * is refused (KErrTooBig) rather than smashing the stack row. Two bytes per pixel for the EColor64K
 * colour row is the larger of the two temp uses. */
static const TInt KMaxIconRow = 256;

/* The bitmap's REAL size, which is not necessarily the size that was asked for.
 *
 * This is the lesson that cost a device round. AknIconUtils::SetSize documents, in as many words,
 * that "there is no guarantee of its size, except that it is non-negative" — and it does nothing at
 * all if the bitmap is not a CAknBitmap. Reading a fixed 44x44 out of a bitmap that turned out to be
 * 22x22 walks off the end of the pixel data: on the E72 one app whose icon happened to be big enough
 * drew skewed, and every other app closed the probe. Neither symptom pointed at the size, because
 * both look like the fetch itself failing.
 *
 * So the size is asked for, never assumed. SizeInPixels is fbscli ordinal 116 — an ordinal GDesk
 * does not import, and therefore one more thing that could in principle fail to resolve. It earns
 * its risk: without it this path cannot be made correct, only lucky. */
static void ActualSize(CFbsBitmap* aBmp, TInt aCap, TInt& aW, TInt& aH, TInt* aWOut, TInt* aHOut)
    {
    const TSize size = aBmp->SizeInPixels();
    aW = size.iWidth;
    aH = size.iHeight;
    /* Report the size to the caller BEFORE refusing it. An icon larger than the buffer is not a
     * failure, it is a buffer that was too small — and the caller can only allocate the right one
     * if it is told what "right" is. Leaving without answering is what made an oversized icon
     * indistinguishable from an app with no icon at all. */
    if (aWOut)
        *aWOut = aW;
    if (aHOut)
        *aHOut = aH;
    if (aW <= 0 || aH <= 0)
        User::Leave(KErrCorrupt);
    if (aW > KMaxIconRow)
        User::Leave(KErrTooBig);
    if (aW * aH > aCap)
        User::Leave(KErrOverflow);
    }

/* Copy one bitmap into caller buffers, row by row, the way GDesk does it: GetScanLine (fbscli ord
 * 109/110) and never GetPixel (ord 131). The server does the display-mode conversion and copies out
 * bytes — EColor64K is already RGB565, EGray256 is one coverage byte per pixel — so there is no
 * per-pixel call and no assumption about the bitmap's own mode. Shared by both icon routes.
 *
 * aW/aH must be the colour bitmap's real size (see ActualSize). The mask is checked separately and
 * independently: AknIconUtils sizes bitmap and mask together in theory, but a mask that disagrees is
 * exactly the sort of thing that reads off the end of a buffer, and an icon with no transparency is
 * a far better outcome than a closed app. */
static void ReadPlanes(CFbsBitmap* aColour, CFbsBitmap* aMask, TInt aW, TInt aH,
                       TUint16* aRgb, TUint8* aMaskOut)
    {
    TUint8 line[KMaxIconRow * 2];

    for (TInt y = 0; y < aH; y++)
        {
        TPtr8 row(line, sizeof(line));
        aColour->GetScanLine(row, TPoint(0, y), aW, EColor64K);
        Mem::Copy(aRgb + y * aW, line, aW * 2);
        }

    TBool masked = EFalse;
    if (aMask)
        {
        const TSize ms = aMask->SizeInPixels();
        masked = (ms.iWidth >= aW && ms.iHeight >= aH);
        }

    if (masked)
        {
        for (TInt y = 0; y < aH; y++)
            {
            TPtr8 row(line, sizeof(line));
            aMask->GetScanLine(row, TPoint(0, y), aW, EGray256);
            Mem::Copy(aMaskOut + y * aW, line, aW);
            }
        }
    else
        {
        /* No usable mask plane: opaque, so the icon draws as a solid rectangle rather than not at
         * all — and, more to the point, rather than reading pixels that are not there. */
        Mem::Fill(aMaskOut, aW * aH, 255);
        }
    }
#endif

#ifdef SHIM_USE_APPICON

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

    /* Refuse anything this route cannot read, BEFORE touching GetAppIcon, which panics the caller
     * rather than failing when it cannot cope.
     *
     * The guard used to be GetAppIconSizes, on the SDK's word that it "returns KErrNotSupported if
     * the application provides icons in non-MBM format". On the E72 it does not. Measured: Speeddial
     * (Z:\Resource\Apps\Speeddial_aif.mif) and File manager (ap_Q0_FileManager_aif.mif) both passed
     * this guard and then closed the probe inside GetAppIcon. The documented question is simply not
     * answered truthfully by this firmware.
     *
     * So ask a question the platform cannot get wrong: what file is the icon in? Two answers are
     * refusals, and both are findings rather than failures.
     *
     *  - No file at all. Measured on app 10207839: no icon file, yet GetAppIcon still reports
     *    success and hands back a 38x31 bitmap — the same size it hands back for Adobe Reader,
     *    which is how "the icon loaded but it is not that app's icon" happens. A default or a
     *    leftover drawn confidently is worse than a caption, so this route declines it.
     *  - A .mif. Scalable icons are what this handset actually ships, and this route cannot read
     *    them; DoIconCL can.
     *
     * That leaves plain .mbm files, which is the only thing this route was ever able to do. */
    HBufC* file = NULL;
    User::LeaveIfError(gLs.GetAppIcon(TUid::Uid(aUid3), file));
    if (!file)
        User::Leave(KErrNotFound);
    CleanupStack::PushL(file);
    _LIT(KMbm, ".mbm");
    /* Right(n) panics if n exceeds the length, so a pathologically short name is checked for first
     * — this is a guard, and a guard that can panic is not one. */
    const TBool isMbm = file->Length() >= 4 && file->Right(4).CompareF(KMbm) == 0;
    CleanupStack::PopAndDestroy(file);
    if (!isMbm)
        User::Leave(KErrNotSupported);

    /* NewLC() creates a fresh empty masked bitmap. The other overload, NewL(const* aSourceIcon),
     * *copies* its argument — passing NULL there dereferences it (KERN-EXEC 3), which is what closed
     * the launcher a few seconds after it opened. NewLC is apgrfx ordinal 33; GDesk does not import
     * it, but that only means GDesk did not need it, not that the handset lacks it. This build is
     * the clean test of whether ordinal 33 is present. */
    CApaMaskedBitmap* bmp = CApaMaskedBitmap::NewLC();

    /* The TSize overload is apgrfx ord 144, the one GDesk uses. aSize is a REQUEST, not a promise.
     *
     * This used to assume the answer came back at exactly the size asked for — "we asked for a
     * square, we get a square" — and skipped SizeInPixels to keep the import set down to GDesk's.
     * The handset disagreed. Measured on the E72: Adobe Reader fetched fine but drew skewed, its
     * rows walking sideways, and every app with a smaller icon closed the probe outright. Both are
     * one bug. The platform hands back the nearest registered icon, which is usually smaller than
     * 44; reading 44 pixels across and 44 rows down out of a 24x24 bitmap wraps the rows if the
     * data happens to extend that far and reads off the end if it does not.
     *
     * So the size is read, not assumed, and the caller is told what it actually got — the buffers
     * were allocated for aSize*aSize, which is the ceiling, and anything at or under it fits. */
    const TSize want(aSize, aSize);
    User::LeaveIfError(gLs.GetAppIcon(TUid::Uid(aUid3), want, *bmp));

    TInt w = 0;
    TInt h = 0;
    ActualSize(bmp, aCap, w, h, aW, aH);
    if (aW)
        *aW = w;
    if (aH)
        *aH = h;

    /* NULL mask, on purpose. The natural call is CApaMaskedBitmap::Mask() (apgrfx ord 183), but
     * that is the one icon symbol the E72 does not carry — a device test bricked the load with it
     * and loaded without it, and GDesk (which runs here) never imports it either. So the mask is
     * dropped and ReadPlanes fills it opaque: icons draw with a solid rectangle rather than a
     * cut-out. Transparency comes back through DoIconCL below, which gets a real mask plane from
     * AknIconUtils instead of from the masked-bitmap accessor. */
    ReadPlanes(bmp, NULL, w, h, aRgb, aMask);
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

#ifdef SHIM_USE_AKNICON

/* Variant C: the icon by FILE, through Avkon, instead of by CApaMaskedBitmap. This is the route
 * that fixes all three known defects of the path above at once, and it is the one the launcher
 * should end up on.
 *
 *   1. It never constructs a CApaMaskedBitmap, so apgrfx ordinal 33 (NewLC) stops mattering — the
 *      one ordinal in this file whose presence on the E72 has never been confirmed either way.
 *   2. AknIconUtils hands back a real mask plane, so icons draw as cut-outs instead of the opaque
 *      rectangles the missing Mask() (ord 183) forces on the route above.
 *   3. It reads MIF (scalable, SVG-T) icons as happily as MBM ones. Those are exactly the apps —
 *      GDesk, Quickword, Email measured on this handset — where GetAppIcon *panics the caller*
 *      instead of failing, and which the route above therefore has to refuse up front.
 *
 * The HBufC*& overload of GetAppIcon (apgcli.h) answers with the icon file's full path rather than
 * its pixels; it is apgrfx, already linked everywhere, and touches no masked bitmap.
 *
 * aBitmapId is a parameter rather than a constant because the right value is a measurement we do
 * not have yet: bitmap and mask sit at consecutive indices within the file, but MBM indices start
 * at 0 while mifconv-generated MIF indices are conventionally offset. The probe sweeps the
 * candidates and the answer gets written down; only then does a caller hardcode one. */
static void DoIconCL(RApaLsSession& aLs, TUint32 aUid3, TInt aSize, TInt aBitmapId,
                     TUint16* aRgb, TUint8* aMask, TInt aCap, TInt* aW, TInt* aH)
    {
    if (aSize <= 0)
        User::Leave(KErrArgument);
    if (aSize > KMaxIconRow)
        User::Leave(KErrTooBig);
    if (aCap < aSize * aSize)
        User::Leave(KErrOverflow);

    /* Ownership of the buffer transfers to us. A registered app with no icon file answers with an
     * error here — an ordinary result, which the caller reads as "draw the caption". */
    HBufC* file = NULL;
    User::LeaveIfError(aLs.GetAppIcon(TUid::Uid(aUid3), file));
    if (!file)
        User::Leave(KErrNotFound);
    CleanupStack::PushL(file);

    /* CreateIconL allocates both planes and transfers them to us. They carry no pixels yet: for an
     * MBM that is the scale step, for a MIF it is the SVG-T rasterisation, and both happen in
     * SetSize. A mask id is always requested; a file that has none yields a NULL mask, which
     * ReadPlanes treats as opaque rather than as an error. */
    CFbsBitmap* bmp = NULL;
    CFbsBitmap* mask = NULL;
    AknIconUtils::CreateIconL(bmp, mask, *file, aBitmapId, aBitmapId + 1);
    CleanupStack::PushL(bmp);
    CleanupStack::PushL(mask);

    /* EAspectRatioNotPreserved so the result is exactly the size asked for. App icons are square
     * and the caller asks for a square, so nothing is distorted — and it spares us reading the
     * size back with CFbsBitmap::SizeInPixels (fbscli ord 116), another ordinal GDesk avoids.
     * SetSize sizes the bitmap and its mask together, whichever of the two is passed. */
    User::LeaveIfError(
        AknIconUtils::SetSize(bmp, TSize(aSize, aSize), EAspectRatioNotPreserved));

    /* Same rule as route A, and here the documentation says it outright: SetSize "does nothing" if
     * the bitmap is not a CAknBitmap, and even on success gives "no guarantee of its size, except
     * that it is non-negative". Ask. */
    TInt w = 0;
    TInt h = 0;
    ActualSize(bmp, aCap, w, h, aW, aH);
    if (aW)
        *aW = w;
    if (aH)
        *aH = h;

    ReadPlanes(bmp, mask, w, h, aRgb, aMask);
    CleanupStack::PopAndDestroy(3, file);
    }

#endif // SHIM_USE_AKNICON

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

/* The path of the file an app's icon actually comes from.
 *
 * Diagnostic, and the one that matters most when a fetch *succeeds* and draws the wrong picture:
 * pixels alone cannot tell you whether the platform handed you the wrong image out of the right
 * file or the right image out of the wrong file. The extension answers a second question for free —
 * `.mbm` is a plain bitmap, `.mif` is scalable, and that is what decides which route can read it.
 *
 * Lives behind SHIM_USE_AKNICON because it is the HBufC*& GetAppIcon overload, an apgrfx ordinal
 * nothing else here imports; the icon file route already depends on it. */
int32_t shim_app_icon_file(uint32_t uid3, uint16_t* out, int32_t cap, int32_t* len)
    {
    if (len)
        *len = 0;
    if (!out || cap <= 0)
        return SHIM_ERR_NOT_SUPPORTED;

#ifdef SHIM_USE_AKNICON
    TInt rc = EnsureSession();
    if (rc != KErrNone)
        return rc;

    HBufC* file = NULL;
    rc = gLs.GetAppIcon(TUid::Uid(uid3), file);
    if (rc != KErrNone)
        return rc;
    if (!file)
        return SHIM_ERR_NOT_FOUND;

    /* Ownership transferred to us, so it is deleted here whatever happens next. */
    const TInt n = file->Length() < cap ? file->Length() : cap;
    Mem::Copy(out, file->Ptr(), n * 2);
    if (len)
        *len = n;
    delete file;
    return SHIM_OK;
#else
    (void) uid3;
    return SHIM_ERR_NOT_SUPPORTED;
#endif
    }

/* Variant C — the Avkon route; see DoIconCL. Same ABI as shim_app_icon plus `bitmap_id`, the index
 * of the colour plane within the app's icon file (the mask is taken to be the next one). */
int32_t shim_app_icon_c(uint32_t uid3, int32_t size, int32_t bitmap_id,
                        uint16_t* rgb_out, uint8_t* mask_out, int32_t cap,
                        int32_t* w, int32_t* h)
    {
    if (w)
        *w = 0;
    if (h)
        *h = 0;
    if (!rgb_out || !mask_out || size <= 0)
        return SHIM_ERR_NOT_SUPPORTED;

#ifdef SHIM_USE_AKNICON
    /* On the CALLING thread, which for a GUI app is the UI thread. That is not a detail.
     *
     * A previous version ran this on a sacrificial worker thread, reasoning that a panic kills only
     * the thread and the caller could then treat "this app cannot be asked" as an ordinary error.
     * It is sound reasoning and it does not work here: measured on the E72, the fetch returned an
     * error for *every* app when run that way, having succeeded for those same apps on the main
     * thread. AknIconUtils is an Avkon UI utility and wants the UI thread's environment; a bare
     * thread with its own RFbsSession and RApaLsSession is not enough. It also cost a thread
     * creation, two server connections and a blocking wait per icon, which made the launcher
     * unusable long before it made it correct.
     *
     * So a panic here is not catchable, and the protection is not in this file: the caller writes
     * down which app it is about to ask about, and an entry still there at the next start names the
     * app that killed the process. Remembering beats catching, because catching is not on offer. */
    TInt rc = EnsureSession();
    if (rc != KErrNone)
        return rc;
    TRAPD(err, DoIconCL(gLs, uid3, size, bitmap_id, rgb_out, mask_out, cap, w, h));
    if (err == KErrOverflow)
        return SHIM_ERR_OVERFLOW;
    return err == KErrNone ? SHIM_OK : err;
#else
    (void) uid3;
    (void) size;
    (void) bitmap_id;
    (void) cap;
    return SHIM_ERR_NOT_SUPPORTED;
#endif
    }

} // extern "C"

#endif // SHIM_USE_APPARC
