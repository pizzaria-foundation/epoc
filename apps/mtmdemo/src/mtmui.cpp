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
 * The same reasoning covers replying, one dialog further along: `CAknTextQueryDialog` asks for
 * the text and the reply is left in the store for a daemon to send. What that is not is
 * Nokia's composition screen — it is a query box. Getting the real screen would mean
 * registering our application as this MTM's editor through `KUidMsvMtmQueryEditorUid`
 * (`0x10001641`, answered by the *client* component's `QueryCapability` — `smsclnt.h:14` says
 * so), and whether MCE consults that query for a third party is unmeasured. It is left for a
 * build of its own, because answering it could pre-empt the paths that now work.
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
#include <aknquerydialog.h>
#include <avkon.rsg>
#include <mtmdemo.rsg>

#include "mtmdemo.h"

/* How much of a body the viewer will show.
 *
 * A cap rather than the whole thing, because the dialog holds the text in one descriptor and
 * a message from a chat service has no natural size limit — and the failure mode of running
 * out of memory here is a dead Messaging application, not a truncated message. Truncation is
 * visible and survivable; the other is neither. */
const TInt KMtmDemoMaxViewChars = 4096;

/* And how much the user may type in a reply.
 *
 * A stack buffer, so it has to be bounded, and this is a one-line query box rather than a
 * composition screen — a limit generous for a chat reply and small enough that the descriptor
 * lives on the stack of a method running inside somebody else's application. */
const TInt KMtmDemoMaxReplyChars = 256;

CMtmDemoUi* CMtmDemoUi::NewL(CBaseMtm& aBaseMtm, CRegisteredMtmDll& aRegisteredDll)
    {
    CMtmDemoUi* self = new (ELeave) CMtmDemoUi(aBaseMtm, aRegisteredDll);
    CleanupStack::PushL(self);
    self->ConstructL();
    CleanupStack::Pop(self);
    return self;
    }

CMtmDemoUi::CMtmDemoUi(CBaseMtm& aBaseMtm, CRegisteredMtmDll& aRegisteredDll)
    : CBaseMtmUi(aBaseMtm, aRegisteredDll), iResourceOffset(0)
    {
    }

CMtmDemoUi::~CMtmDemoUi()
    {
    /* Ours to release: the base class released its own copy, not this one. Leaving it loaded
     * would hold a file open in the Messaging application for as long as it runs. */
    if (iResourceOffset && iCoeEnv)
        iCoeEnv->DeleteResourceFile(iResourceOffset);
    }

void CMtmDemoUi::ConstructL()
    {
    /* The base class's ConstructL loads the resource file GetResourceFileName names. Ours is
     * the same file the UI-data component uses — one file, one thing to install, and neither
     * component has resources of its own beyond existing. */
    CBaseMtmUi::ConstructL();

    /* And load it again, for an offset of our own.
     *
     * The base class just loaded this same file and kept the offset private (mtmuibas.h:511),
     * so an id out of our .rsg has no way to be relocated through it. A second load is the only
     * route to an offset this component can add to — and it is a second entry in CCoeEnv's
     * list, not a second copy of the file.
     *
     * Guarded on iCoeEnv, because a UI MTM can be instantiated by a process that has no control
     * environment at all. There is nothing such a caller could do with the dialog, but there is
     * a difference between it failing to open one and this component faulting on construction. */
    if (iCoeEnv)
        iResourceOffset = iCoeEnv->AddResourceFileL(KMtmDemoResourceFile);

    /* The offset itself, reported as a number.
     *
     * Measured as 0x421de000 on the E72, which is exactly the base the ids in mtmdemo.rsg
     * already carry (0x421de002, 0x421de003). That is why nothing adds it: see the note in
     * ReplyL. Kept because it is the one number that distinguishes a resource file loaded where
     * its ids expect from one relocated somewhere else, and a future .rss with no NAME would
     * report something different. */
    TBuf<64> line;
    _LIT(KOffset, "ui resource offset 0x%x");
    line.Format(KOffset, iResourceOffset);
    MtmDemoTrace(line);
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

/* Reply: ask for the text, and leave a finished message behind.
 *
 * The two steps are the framework's — `CBaseMtm::ReplyL` makes the entry, the UI edits it —
 * and the editing is a `CAknTextQueryDialog`, for the same reason the viewer is a dialog: the
 * platform's own editor-launch mechanism has no public header, and a UI MTM is already inside
 * the application that would launch one.
 *
 * What this is *not* is Nokia's composition screen. It is a one-line query box, which is
 * enough to send a chat reply and is not enough to write mail. The alternative — registering
 * our application as this MTM's editor through `KUidMsvMtmQueryEditorUid` — is a separate
 * measurement, not an alternative implementation, because whether MCE consults that query for
 * a third party is unknown and answering it could pre-empt the paths that already work.
 *
 * Where the reply goes is aDestination, and this component does not send it. The entry lands
 * there complete and visible, and whatever is watching the store — a daemon, in a real
 * service — picks it up. That keeps the sending out of Nokia's process entirely. */
CMsvOperation* CMtmDemoUi::ReplyL(TMsvId aDestination, TMsvPartList aPartlist,
                                  TRequestStatus& aCompletionStatus)
    {
    const TMsvEntry original = iBaseMtm.Entry().Entry();
    const TMsvId serviceId = original.iServiceId;

    /* The prompt text, owned, and copied out before step one moves the context.
     *
     * Same trap as in the client component: `iDetails` is a `TPtrC` into the `CMsvEntry`'s
     * buffer (`msvstd.h`), and `CBaseMtm::ReplyL` switches context twice. Using
     * `original.iDetails` after that call reads freed memory — which is what the first version
     * of this method did, and it is why replying killed the Messaging application while opening
     * a message was fine. */
    HBufC* details = original.iDetails.AllocLC();

    /* Step one: create the entry — here, with the base class's own API, and **not** by calling
     * `iBaseMtm.ReplyL`.
     *
     * WHY NOT, BECAUSE THE OBVIOUS VERSION FROZE THE MESSAGING APPLICATION
     *
     * The framework prescribes calling `CBaseMtm::ReplyL` from here, and the client component
     * does implement it. But it returns a `CMsvOperation`, and this code has no caller to hand
     * that operation to — so the first version created one against a throwaway
     * `TRequestStatus`, deleted it, and waited on the status to balance the signal.
     *
     * `CMsvCompletedOperation` is not completed on construction, despite the name. It derives
     * from `CMsvOperation : CActive` and has `RunL`/`DoCancel` (`msvapi.h`): it signals the
     * observer on the *next turn of the active scheduler*. So `User::WaitForRequest` on this
     * thread blocks the very scheduler that has to run for that turn to happen — a deadlock on
     * the Messaging application's UI thread, which from outside is the application vanishing to
     * the home screen. The platform's own answer to this is
     * `CMsvOperationActiveSchedulerWait`, a nested scheduler loop rather than a thread block,
     * and its documentation in msvapi.h exists precisely because the direct wait is wrong.
     *
     * Rather than nest a scheduler inside MCE while it is showing a menu, this creates the
     * entry directly. Everything it needs is public `CBaseMtm` API — the context and
     * `CMsvEntry::CreateL` — so there is no operation, no request status, and nothing to wait
     * for. `CMtmDemoClient::ReplyL` stays for callers with a real status to complete. */
    MtmDemoTrace(_L("reply-u1 creating the entry"));

    TMsvEntry reply;
    reply.iType = KUidMsvMessageEntry;
    reply.iMtm = Type();
    reply.iServiceId = serviceId;
    reply.iDetails.Set(*details);
    reply.iDate.HomeTime();
    reply.SetInPreparation(ETrue);
    reply.SetVisible(EFalse);

    iBaseMtm.SwitchCurrentEntryL(aDestination);
    iBaseMtm.Entry().CreateL(reply);
    const TMsvId replyId = reply.Id();
    iBaseMtm.SwitchCurrentEntryL(replyId);

    /* Quoting the original is what aPartlist asks for, and it is skipped here: the body is
     * written from scratch below, and prefilling a one-line query box with the message being
     * replied to would leave the user deleting it before they could type. */
    (void)aPartlist;

    /* Step two: ask for the text, from a resource of our own.
     *
     * `RunLD()` — the resource-free path the header documents — was tried and it takes the
     * Messaging application down: the breadcrumb below ran and the one after the call never
     * did. So the dialog is built from `R_MTMDEMO_REPLY_QUERY` in data/mtmdemo.rss, reached
     * through the offset this component loaded for itself in ConstructL, because the base
     * class's offset for the same file is private. */
    /* THE ID IS USED AS THE .rsg GIVES IT. No offset added, and that is the whole bug fixed.
     *
     * `CAknQueryDialog::RunLD()` — the resource-free path the header documents without
     * qualification — kills the process. Isolated and measured: the breadcrumb before it was
     * written, the one after it never was, twice. So a query dialog needs a resource here.
     *
     * And `ExecuteLD(R_MTMDEMO_REPLY_QUERY + iResourceOffset)` killed it too, for a different
     * reason that the offset trace in ConstructL settled: `AddResourceFileL` returned
     * 0x421de000, and the id rcomp generated is 0x421de003 — the *same base*, already included.
     * Adding it again asked for 0x843bc003, which is no resource at all.
     *
     * So the offset is what the file loaded at, not something to add. The widely-repeated
     * `R_X + offset` idiom applies to ids generated without a NAME signature; this file has
     * `NAME MTMD` and its ids carry the base. Reading could not settle which of the two this
     * was — the returned value could.
     *
     * SetPromptL is still absent. It was removed while it was a suspect and this build changes
     * one thing; the correspondent's name can come back once the dialog is known to open. */
    TBuf<KMtmDemoMaxReplyChars> text;
    CAknTextQueryDialog* query = CAknTextQueryDialog::NewL(text);
    MtmDemoTrace(_L("reply-u2b about to ExecuteLD with the bare id"));
    const TBool confirmed = query->ExecuteLD(R_MTMDEMO_REPLY_QUERY);
    MtmDemoTrace(_L("reply-u2c dialog dismissed"));

    if (!confirmed || !text.Length())
        {
        /* Cancelled. The entry created a moment ago has to go: leaving it would put an empty
         * invisible message in the user's Drafts for every reply they thought better of, and
         * invisible is exactly the state in which they could not find it to delete. */
        iBaseMtm.SwitchCurrentEntryL(aDestination);
        iBaseMtm.Entry().DeleteL(replyId);
        CleanupStack::PopAndDestroy(details);
        MtmDemoTrace(_L("reply-u4 cancelled, entry deleted"));

        TPckgBuf<TMsvLocalOperationProgress> progress;
        progress().iId = replyId;
        progress().iError = KErrCancel;
        return CMsvCompletedOperation::NewL(Session(), Type(), progress,
                                           serviceId, aCompletionStatus,
                                           KErrCancel);
        }

    /* The body, through the client component's own store path — the same one that reads it
     * back, so there is one definition of where a body lives rather than two. */
    iBaseMtm.SwitchCurrentEntryL(replyId);
    iBaseMtm.Body().Reset();
    iBaseMtm.Body().InsertL(0, text);
    iBaseMtm.SaveMessageL();

    /* And now it is a message: visible, no longer in preparation, with the text as its
     * description so the Messaging application has a line to show in the list. This ChangeL is
     * what publishes it, and it happens after the body is committed — the other order gives
     * whatever is watching the store a visible message with nothing in it. */
    TMsvEntry entry = iBaseMtm.Entry().Entry();
    entry.iDescription.Set(text);
    entry.iDate.HomeTime();
    entry.SetInPreparation(EFalse);
    entry.SetVisible(ETrue);
    entry.SetComplete(ETrue);
    iBaseMtm.Entry().ChangeL(entry);
    CleanupStack::PopAndDestroy(details);
    MtmDemoTrace(_L("reply-u5 reply published"));

    TPckgBuf<TMsvLocalOperationProgress> progress;
    progress().iTotalNumberOfEntries = 1;
    progress().iNumberCompleted = 1;
    progress().iId = replyId;
    return CMsvCompletedOperation::NewL(Session(), Type(), progress,
                                        serviceId, aCompletionStatus);
    }

CMsvOperation* CMtmDemoUi::ForwardL(TMsvId /*aDestination*/, TMsvPartList /*aPartList*/,
                                    TRequestStatus& /*aCompletionStatus*/)
    {
    User::Leave(KErrNotSupported);
    return NULL;
    }
