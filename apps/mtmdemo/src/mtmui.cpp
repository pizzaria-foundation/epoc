/* The UI MTM: what happens when the user taps one of our messages.
 *
 * WHY THERE IS NO EDITOR APPLICATION HERE
 *
 * The obvious shape for this component is "launch an editor and return an operation that
 * watches it". Two things rule that out on this platform, and finding them out is most of
 * what the research for this file produced:
 *
 * The SDK's reference implementation does not actually do it. `CTextMtmUi::LaunchEditorApplicationL`
 * is a stub whose own comment reads *"In a real MTM, would launch the appropriate
 * editor/viewer... Here we just pretend"*. It gives the shape of the return value and no
 * mechanism.
 *
 * And S60's own mechanism is `muiu.dll` — `CMuiuMsgEditorService`, `RMuiuMsgEditorService`,
 * `CMsgEditorServerWatchingOperation` — which ships as a binary with no header and no import
 * library in this SDK. The Messaging application uses it; a third party cannot.
 *
 * WHAT IS AVAILABLE INSTEAD, AND IS BETTER FOR A VIEWER
 *
 * A UI MTM is loaded into the application that is showing the message list, and
 * `CBaseMtmUi` hands it that application's `CCoeEnv` (`mtmuibas.h:504`). So the question is
 * not how to launch a viewer but what to draw, and the answer is entirely public API:
 * `CAknMessageQueryDialog` over `R_AVKON_MESSAGE_QUERY_DIALOG`, a scrollable text dialog
 * with a heading, needing no resource of our own. Avkon is already loaded in that process —
 * it *is* that process — so this costs nothing to reach.
 *
 * That gives a read-only viewer. Replying needs an editor and therefore needs the platform
 * to know which application edits our messages, which is the `KUidMsvMtmQueryEditorUid`
 * query and a separate piece of work.
 *
 * THE SAME RULES AS THE UI DATA COMPONENT
 *
 * No writable static data, because a Symbian 9.x DLL with any is refused by the loader. And
 * nothing here panics: this runs inside the user's Messaging application, where the
 * reference implementation's `__ASSERT_ALWAYS(..., Panic(...))` habit is a way to close
 * somebody's inbox because a caller passed an entry we did not expect.
 */

#include <e32base.h>
#include <coemain.h>
#include <eikenv.h>
#include <msvapi.h>
#include <msvstd.h>
#include <msvuids.h>
#include <msvids.h>
#include <msvstore.h>
#include <msvipc.h>
#include <mtclbase.h>
#include <txtrich.h>
#include <txtfmlyr.h>
#include <aknmessagequerydialog.h>
#include <avkon.rsg>

#include "mtmdemo.h"

/* How much of a body the viewer will show.
 *
 * A cap rather than the whole thing, because the dialog holds the text in one descriptor and
 * a message from a chat service has no natural size limit — and the failure mode of running
 * out of memory here is a dead Messaging application, not a truncated message. Truncation is
 * visible and survivable; the other is neither. */
const TInt KMtmDemoMaxViewChars = 4096;

CMtmDemoUi* CMtmDemoUi::NewL(CBaseMtm& aBaseMtm, CRegisteredMtmDll& aRegisteredDll)
    {
    CMtmDemoUi* self = new (ELeave) CMtmDemoUi(aBaseMtm, aRegisteredDll);
    CleanupStack::PushL(self);
    self->ConstructL();
    CleanupStack::Pop(self);
    return self;
    }

CMtmDemoUi::CMtmDemoUi(CBaseMtm& aBaseMtm, CRegisteredMtmDll& aRegisteredDll)
    : CBaseMtmUi(aBaseMtm, aRegisteredDll)
    {
    }

CMtmDemoUi::~CMtmDemoUi()
    {
    }

void CMtmDemoUi::ConstructL()
    {
    /* The base class's ConstructL loads the resource file GetResourceFileName names. Ours is
     * the same file the UI-data component uses — one file, one thing to install, and neither
     * component has resources of its own beyond existing. */
    CBaseMtmUi::ConstructL();
    }

void CMtmDemoUi::GetResourceFileName(TFileName& aFileName) const
    {
    aFileName = KMtmDemoResourceFile;
    }

/* ------------------------------------------------------------------- the viewer -- */

/* Read the body out of the current entry's store.
 *
 * Returns NULL rather than leaving when there is nothing to show: a message with no body is
 * an ordinary thing (a notification, a placeholder), not an error, and the caller turns it
 * into an empty viewer rather than a failure the user has to interpret. */
HBufC* CMtmDemoUi::BodyTextLC()
    {
    CMsvStore* store = iBaseMtm.Entry().ReadStoreL();
    CleanupStack::PushL(store);

    if (!store->HasBodyTextL())
        {
        CleanupStack::PopAndDestroy(store);
        return NULL;
        }

    /* The rich text needs both format layers and does not own them, so they are ours to make
     * and ours to free.
     *
     * The application's *own* layers would be the obvious choice, but they hang off
     * `CEikonEnv` and what a UI MTM is given is a `CCoeEnv*` (`mtmuibas.h:504`) — reaching
     * them means asserting that the host is an Eikon application and casting on that
     * assertion. The default layers are a static factory, need no such claim, and the text
     * is extracted as plain characters immediately afterwards: the formatting is never drawn,
     * so which layers they are does not reach the screen. */
    CParaFormatLayer* para = CEikonEnv::NewDefaultParaFormatLayerL();
    CleanupStack::PushL(para);
    CCharFormatLayer* charFormat = CEikonEnv::NewDefaultCharFormatLayerL();
    CleanupStack::PushL(charFormat);

    CRichText* body = CRichText::NewL(para, charFormat);
    CleanupStack::PushL(body);
    store->RestoreBodyTextL(*body);

    TInt length = body->DocumentLength();
    if (length > KMtmDemoMaxViewChars)
        length = KMtmDemoMaxViewChars;

    if (!length)
        {
        CleanupStack::PopAndDestroy(4, store);   // body, charFormat, para, store
        return NULL;
        }

    HBufC* text = HBufC::NewL(length);
    TPtr ptr = text->Des();
    body->Extract(ptr, 0, length);

    CleanupStack::PopAndDestroy(4, store);   // body, charFormat, para, store
    CleanupStack::PushL(text);
    return text;
    }

/* Show one message. Synchronous: the dialog runs, the user dismisses it, and the operation
 * is already finished by the time it is returned.
 *
 * The framework's contract is that these return a CMsvOperation the caller can watch.
 * `CMsvCompletedOperation` is the platform's own way to say "this was done before you asked"
 * — the reference uses it for exactly this and it is what keeps a synchronous action from
 * needing an active object it would never use. */
CMsvOperation* CMtmDemoUi::ShowMessageL(TRequestStatus& aStatus)
    {
    const TMsvEntry entry = iBaseMtm.Entry().Entry();

    HBufC* text = BodyTextLC();
    if (!text)
        {
        _LIT(KNoBody, "(no message text)");
        text = KNoBody().AllocLC();
        }

    TPtr ptr = text->Des();
    CAknMessageQueryDialog* dialog = CAknMessageQueryDialog::NewL(ptr);
    /* PrepareLC puts it on the cleanup stack; RunLD takes it back off and deletes it. Between
     * those two the dialog is owned by the cleanup stack, which is why the header is set in
     * the middle and nothing else may leave. */
    dialog->PrepareLC(R_AVKON_MESSAGE_QUERY_DIALOG);
    if (entry.iDetails.Length())
        dialog->SetHeaderTextL(entry.iDetails);
    dialog->RunLD();

    CleanupStack::PopAndDestroy(text);

    TPckgBuf<TMsvLocalOperationProgress> progress;
    progress().iTotalNumberOfEntries = 1;
    progress().iNumberCompleted = 1;
    progress().iId = entry.Id();
    return CMsvCompletedOperation::NewL(Session(), Type(), progress,
                                        entry.iServiceId, aStatus);
    }

CMsvOperation* CMtmDemoUi::OpenL(TRequestStatus& aStatus)
    {
    return ShowMessageL(aStatus);
    }

CMsvOperation* CMtmDemoUi::ViewL(TRequestStatus& aStatus)
    {
    /* Open and view are the same thing for a message that cannot be edited. They diverge the
     * moment there is an editor: open would launch it, view would stay read-only. */
    return ShowMessageL(aStatus);
    }

CMsvOperation* CMtmDemoUi::OpenL(TRequestStatus& aStatus, const CMsvEntrySelection& aSelection)
    {
    /* The selection overloads are what a caller uses to act on several entries at once. One
     * viewer at a time is the only thing that makes sense on this screen, so the first entry
     * is switched to and shown — and an empty selection is the caller's mistake, answered
     * with a Leave rather than an index into nothing. */
    if (!aSelection.Count())
        User::Leave(KErrArgument);
    iBaseMtm.SwitchCurrentEntryL(aSelection.At(0));
    return ShowMessageL(aStatus);
    }

CMsvOperation* CMtmDemoUi::ViewL(TRequestStatus& aStatus, const CMsvEntrySelection& aSelection)
    {
    if (!aSelection.Count())
        User::Leave(KErrArgument);
    iBaseMtm.SwitchCurrentEntryL(aSelection.At(0));
    return ShowMessageL(aStatus);
    }

/* ------------------------------------------------------------- not this stage's --
 * Each leaves with KErrNotSupported, which is the documented way for a UI MTM to say an
 * operation is not offered. The UI-data component answers `EFalse` to the matching `Can*L`,
 * so the Messaging application does not offer these in the first place — the two have to
 * agree, and a Leave here is the backstop for a caller that asks anyway.
 */

CMsvOperation* CMtmDemoUi::CloseL(TRequestStatus& /*aStatus*/)
    {
    /* Nothing to close: the viewer is modal and gone by the time OpenL returns. */
    User::Leave(KErrNotSupported);
    return NULL;
    }

CMsvOperation* CMtmDemoUi::CloseL(TRequestStatus& /*aStatus*/, const CMsvEntrySelection& /*aSelection*/)
    {
    User::Leave(KErrNotSupported);
    return NULL;
    }

CMsvOperation* CMtmDemoUi::EditL(TRequestStatus& /*aStatus*/)
    {
    /* Editing needs an editor application and a way for the platform to know which one —
     * the KUidMsvMtmQueryEditorUid query. Neither exists yet. */
    User::Leave(KErrNotSupported);
    return NULL;
    }

CMsvOperation* CMtmDemoUi::EditL(TRequestStatus& /*aStatus*/, const CMsvEntrySelection& /*aSelection*/)
    {
    User::Leave(KErrNotSupported);
    return NULL;
    }

CMsvOperation* CMtmDemoUi::CancelL(TRequestStatus& /*aStatus*/, const CMsvEntrySelection& /*aSelection*/)
    {
    User::Leave(KErrNotSupported);
    return NULL;
    }

CMsvOperation* CMtmDemoUi::ReplyL(TMsvId /*aDestination*/, TMsvPartList /*aPartlist*/,
                                  TRequestStatus& /*aCompletionStatus*/)
    {
    User::Leave(KErrNotSupported);
    return NULL;
    }

CMsvOperation* CMtmDemoUi::ForwardL(TMsvId /*aDestination*/, TMsvPartList /*aPartList*/,
                                    TRequestStatus& /*aCompletionStatus*/)
    {
    User::Leave(KErrNotSupported);
    return NULL;
    }
