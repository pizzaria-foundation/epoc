/* The reference MTM: what a service actually writes.
 *
 * Everything that made this work is in `shim/mtm/` — the reply mechanics, the never-panic
 * policy, the resource-offset arithmetic, the four measured platform facts. What is left here
 * is identity: which resource file, which bitmaps, which dialog. That is the point of the
 * split, and this file is the demonstration of it.
 *
 * It is also the regression test for the library. This app is the one that ran on an E72 and
 * did four things: registered, drew its own icon in Nokia's inbox, opened a message in its own
 * viewer, and replied from Nokia's menu. If the extraction lost anything, the same build on the
 * same handset stops doing one of them — and nothing else in this SDK has a cheaper check than
 * that.
 *
 * A service copies this file, changes six literals and the UIDs in app.conf, and is done.
 */

#include <e32base.h>
#include <msvapi.h>
#include <msvstd.h>

#include "mtmbase.h"
#include "mtmdemo.rsg"

/* Where symbuild installs this component's two data files.
 *
 * `\resource\messaging\` is where the framework looks and where every one of the platform's own
 * UI-data components keeps them (smum.rsc, mmsui.rsc, notui.rsc). The directory is the
 * platform's; the filenames are ours. */
_LIT(KResourceFile, "C:\\resource\\messaging\\mtmdemo.rsc");
_LIT(KBitmapFile, "C:\\resource\\messaging\\mtmdemo.mbm");

/* Where the breadcrumbs go, when MTM_TRACE=1 in app.conf. Flat in C:\Data\ rather than a
 * subdirectory, because RFs::MkDirAll silently ignores a path with no trailing separator and
 * everything then lands in the per-UID3 private cage where it cannot be collected. */
_LIT(KTraceFile, "C:\\Data\\dump-mtmdemo.txt");

/* The client component. Nothing but the trace file: every one of CBaseMtm's eleven pure
 * virtuals is already implemented by the base, and the capability answers it gives are the ones
 * measured to work. */
class CMtmDemoClient : public CMtmBaseClient
    {
public:
    static CMtmDemoClient* NewL(CRegisteredMtmDll& aDll, CMsvSession& aSession)
        {
        CMtmDemoClient* self = new (ELeave) CMtmDemoClient(aDll, aSession);
        CleanupStack::PushL(self);
        self->ConstructL();
        CleanupStack::Pop(self);
        return self;
        }

private:
    CMtmDemoClient(CRegisteredMtmDll& aDll, CMsvSession& aSession)
        : CMtmBaseClient(aDll, aSession)
        {
        }

    void TraceFileName(TFileName& aName) const
        {
        aName = KTraceFile;
        }
    };

/* The UI-data component: two files, and the default policy.
 *
 * The two pure hooks have no sane default — a UI-data component whose resource file is missing
 * leaves during construction inside the Messaging application — so the compiler requires them.
 * Everything else (two icons, one zoom state, reply/open/view offered for messages, forward and
 * edit not) is the base's default and is what this app measured. */
class CMtmDemoUiData : public CMtmBaseUiData
    {
public:
    static CMtmDemoUiData* NewL(CRegisteredMtmDll& aDll)
        {
        CMtmDemoUiData* self = new (ELeave) CMtmDemoUiData(aDll);
        CleanupStack::PushL(self);
        /* CBaseMtmUiData::ConstructL is what calls GetResourceFileName and PopulateArraysL —
         * a subclass must not do that work itself or it happens twice. */
        self->ConstructL();
        CleanupStack::Pop(self);
        return self;
        }

private:
    CMtmDemoUiData(CRegisteredMtmDll& aDll) : CMtmBaseUiData(aDll) {}

    void GetResourceFileName(TFileName& aName) const
        {
        aName = KResourceFile;
        }
    void GetBitmapFileName(TFileName& aName) const
        {
        aName = KBitmapFile;
        }
    void TraceFileName(TFileName& aName) const
        {
        aName = KTraceFile;
        }
    };

/* The UI component: the same resource file, plus the id of the reply dialog in it.
 *
 * `R_MTMDEMO_REPLY_QUERY` comes from mtmdemo.rsg, which rcomp generates from data/mtmdemo.rss
 * at build stage 2p — before the C++, because this line needs it. It is passed **bare**: the
 * base adds no offset, and data/mtmdemo.rss declares `NAME MTMD`, which is what makes that
 * correct. See CMtmBaseUi::ReplyL for the measurement behind it. */
class CMtmDemoUi : public CMtmBaseUi
    {
public:
    static CMtmDemoUi* NewL(CBaseMtm& aMtm, CRegisteredMtmDll& aDll)
        {
        CMtmDemoUi* self = new (ELeave) CMtmDemoUi(aMtm, aDll);
        CleanupStack::PushL(self);
        self->ConstructL();
        CleanupStack::Pop(self);
        return self;
        }

private:
    CMtmDemoUi(CBaseMtm& aMtm, CRegisteredMtmDll& aDll) : CMtmBaseUi(aMtm, aDll) {}

    void GetResourceFileName(TFileName& aName) const
        {
        aName = KResourceFile;
        }
    TInt ReplyDialogResourceId() const
        {
        return R_MTMDEMO_REPLY_QUERY;
        }
    void TraceFileName(TFileName& aName) const
        {
        aName = KTraceFile;
        }
    };

/* The four factories. All four slots must exist or the Message Server panics on a later
 * scheduler turn than the install; the macro is what makes that impossible to get wrong. */
MTM_EXPORT_COMPONENTS(CMtmDemoClient, CMtmDemoUi, CMtmDemoUiData)
