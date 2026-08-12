/* The UI MTM: the viewer, and the reply.
 *
 * See shim/mtm/inc/mtmbase.h for the four constraints and for the four platform facts this
 * file is built on, every one of them measured on an E72 and none deducible. They are repeated
 * at the line they govern, because that is where somebody changing the code will be.
 *
 * This component runs inside the Messaging application. Nothing here panics.
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

#include "mtmbase.h"

CMtmBaseUi::CMtmBaseUi(CBaseMtm& aBaseMtm, CRegisteredMtmDll& aRegisteredDll)
    : CBaseMtmUi(aBaseMtm, aRegisteredDll), iResourceOffset(0)
    {
    }

CMtmBaseUi::~CMtmBaseUi()
    {
    /* Ours to release: the base class released its own load, not this one. Leaving it would
     * hold a file open in the Messaging application for as long as it runs. */
    if (iResourceOffset && iCoeEnv)
        iCoeEnv->DeleteResourceFile(iResourceOffset);
    }

void CMtmBaseUi::ConstructL()
    {
    CBaseMtmUi::ConstructL();

    /* And load the same resource file again, for an offset of our own.
     *
     * The base class just loaded it and kept the offset in a **private** member
     * (mtmuibas.h:511), so an id from the subclass's .rsg has no way to be relocated through
     * it. A second load is the only route to an offset this component can reach — and it is a
     * second entry in CCoeEnv's list, not a second copy of the file.
     *
     * Guarded on iCoeEnv because a UI MTM can be instantiated by a process with no control
     * environment at all. Such a caller could do nothing with a dialog anyway, but there is a
     * difference between failing to open one and faulting during construction. */
    if (iCoeEnv)
        {
        TFileName resource;
        GetResourceFileName(resource);
        iResourceOffset = iCoeEnv->AddResourceFileL(resource);
        }

    /* The offset, as a number, because it is the one value that distinguishes a resource file
     * loaded where its ids expect from one relocated elsewhere.
     *
     * Measured 0x421de000 on the E72, which is exactly the base the ids in a .rsg already
     * carry when the .rss declares a NAME. That is why ReplyL adds nothing to the id. A .rss
     * without a NAME would report something different here — and would need the offset added,
     * which this library cannot detect. Hence the requirement on the subclass's resource file. */
    TBuf<64> line;
    _LIT(KOffset, "ui: resource offset 0x%x");
    line.Format(KOffset, iResourceOffset);
    Trace(line);
    }

void CMtmBaseUi::TraceFileName(TFileName& aName) const
    {
    aName.Zero();
    }

void CMtmBaseUi::Trace(const TDesC& aStep) const
    {
    TFileName path;
    TraceFileName(path);
    MtmBaseTrace(path, aStep);
    }

TInt CMtmBaseUi::MaxViewChars() const
    {
    return 4096;
    }

TInt CMtmBaseUi::MaxReplyChars() const
    {
    return 256;
    }

void CMtmBaseUi::EmptyBodyText(TDes& aText) const
    {
    _LIT(KNoBody, "(no message text)");
    aText = KNoBody;
    }

/* ------------------------------------------------------------------- the viewer -- */

HBufC* CMtmBaseUi::BodyTextLC()
    {
    /* Returns NULL rather than leaving when there is nothing to show: a message with no body is
     * an ordinary thing — a notification, a placeholder — not an error, and the caller turns it
     * into a viewer with a placeholder line rather than a failure the user must interpret. */
    CMsvStore* store = iBaseMtm.Entry().ReadStoreL();
    CleanupStack::PushL(store);

    if (!store->HasBodyTextL())
        {
        CleanupStack::PopAndDestroy(store);
        return NULL;
        }

    /* The rich text needs both format layers and owns neither, so they are ours to make and
     * ours to free.
     *
     * The host application's own layers would be the obvious choice, but they hang off
     * CEikonEnv (eikenv.h:290) and what a UI MTM is given is the base CCoeEnv
     * (mtmuibas.h:504) — reaching them means asserting the host is an Eikon application and
     * casting on that assertion. These static factories need no such claim, and the text is
     * extracted as plain characters immediately after, so which layers they were never reaches
     * the screen. */
    CParaFormatLayer* para = CEikonEnv::NewDefaultParaFormatLayerL();
    CleanupStack::PushL(para);
    CCharFormatLayer* charFormat = CEikonEnv::NewDefaultCharFormatLayerL();
    CleanupStack::PushL(charFormat);

    CRichText* body = CRichText::NewL(para, charFormat);
    CleanupStack::PushL(body);
    store->RestoreBodyTextL(*body);

    TInt length = body->DocumentLength();
    const TInt cap = MaxViewChars();
    if (length > cap)
        length = cap;

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

CMsvOperation* CMtmBaseUi::ShowMessageL(TRequestStatus& aStatus)
    {
    const TMsvEntry entry = iBaseMtm.Entry().Entry();
    const TMsvId serviceId = entry.iServiceId;

    HBufC* text = BodyTextLC();
    if (!text)
        {
        TBuf<64> placeholder;
        EmptyBodyText(placeholder);
        text = placeholder.AllocLC();
        }

    /* R_AVKON_MESSAGE_QUERY_DIALOG is the platform's own, so the viewer needs no resource of
     * ours — unlike the reply dialog below. Avkon is already loaded in this process because it
     * *is* this process. */
    TPtr ptr = text->Des();
    CAknMessageQueryDialog* dialog = CAknMessageQueryDialog::NewL(ptr);
    /* PrepareLC puts it on the cleanup stack; RunLD takes it back off and deletes it. Between
     * those two the dialog belongs to the cleanup stack, which is why the header is set in the
     * middle and nothing else there may leave. */
    dialog->PrepareLC(R_AVKON_MESSAGE_QUERY_DIALOG);
    if (entry.iDetails.Length())
        dialog->SetHeaderTextL(entry.iDetails);
    dialog->RunLD();

    const TMsvId shown = entry.Id();
    CleanupStack::PopAndDestroy(text);

    TPckgBuf<TMsvLocalOperationProgress> progress;
    progress().iTotalNumberOfEntries = 1;
    progress().iNumberCompleted = 1;
    progress().iId = shown;
    return CMsvCompletedOperation::NewL(Session(), Type(), progress, serviceId, aStatus);
    }

CMsvOperation* CMtmBaseUi::OpenL(TRequestStatus& aStatus)
    {
    Trace(_L("ui: OpenL"));
    return ShowMessageL(aStatus);
    }

CMsvOperation* CMtmBaseUi::ViewL(TRequestStatus& aStatus)
    {
    /* Open and view are the same thing for a message that cannot be edited. They diverge the
     * moment there is an editor: open would launch it, view would stay read-only. */
    return ShowMessageL(aStatus);
    }

CMsvOperation* CMtmBaseUi::OpenL(TRequestStatus& aStatus, const CMsvEntrySelection& aSelection)
    {
    /* The selection overloads are how a caller acts on several entries at once. One viewer at a
     * time is the only thing that makes sense on this screen, so the first entry is switched to
     * and shown — and an empty selection is the caller's mistake, answered with a Leave rather
     * than an index into nothing. */
    if (!aSelection.Count())
        User::Leave(KErrArgument);
    iBaseMtm.SwitchCurrentEntryL(aSelection.At(0));
    return ShowMessageL(aStatus);
    }

CMsvOperation* CMtmBaseUi::ViewL(TRequestStatus& aStatus, const CMsvEntrySelection& aSelection)
    {
    if (!aSelection.Count())
        User::Leave(KErrArgument);
    iBaseMtm.SwitchCurrentEntryL(aSelection.At(0));
    return ShowMessageL(aStatus);
    }

/* -------------------------------------------------------------------- the reply -- */

CMsvOperation* CMtmBaseUi::ReplyL(TMsvId aDestination, TMsvPartList aPartlist,
                                  TRequestStatus& aCompletionStatus)
    {
    const TMsvEntry original = iBaseMtm.Entry().Entry();
    const TMsvId serviceId = original.iServiceId;

    /* The correspondent, owned, before anything moves the context.
     *
     * iDetails is a TPtrC into the CMsvEntry's buffer (msvstd.h), and the context switches
     * twice below. Using original.iDetails afterwards reads freed memory — which the first
     * version did, and it is why replying killed the Messaging application while opening a
     * message was fine. */
    HBufC* details = original.iDetails.AllocLC();

    /* Step one: create the entry here, with the base class's own API, and **not** by calling
     * iBaseMtm.ReplyL.
     *
     * The framework prescribes calling CBaseMtm::ReplyL from here, and CMtmBaseClient
     * implements it. But it returns a CMsvOperation and this code has no caller to hand one
     * to. The obvious version created one against a throwaway TRequestStatus, deleted it, and
     * waited on the status to balance the signal.
     *
     * CMsvCompletedOperation is **not** completed on construction, despite the name. It derives
     * from CMsvOperation : CActive and has RunL/DoCancel (msvapi.h): it signals the observer on
     * the *next turn of the active scheduler*. So User::WaitForRequest on this thread blocks
     * the very scheduler that must run for that turn to happen — a deadlock on the Messaging
     * application's UI thread, which from outside is the application vanishing to the home
     * screen with no panic to show. The platform's own answer is
     * CMsvOperationActiveSchedulerWait, a nested scheduler loop; nesting a scheduler inside MCE
     * while it shows a menu is worse than not needing one.
     *
     * Creating the entry directly needs only public CBaseMtm API, so there is no operation, no
     * request status, and nothing to wait for. CMtmBaseClient::ReplyL stays for callers with a
     * real status to complete. */
    Trace(_L("ui: reply, creating the entry"));

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

    /* aPartlist's request to quote the original is declined: the body is written from scratch
     * below, and prefilling a one-line query box with the message being replied to would leave
     * the user deleting it before they could type. A service that wants quoting overrides this
     * whole method. */
    (void)aPartlist;

    /* Step two: ask for the text, from the subclass's own resource.
     *
     * CAknQueryDialog::RunLD() — the resource-free path the header documents without
     * qualification — **kills the process** on this handset. Measured: the breadcrumb before it
     * was written and the one after it never was, twice.
     *
     * And the resource id is used **bare**. ExecuteLD(id + iResourceOffset) killed it too, for
     * a different reason the offset trace in ConstructL settled: AddResourceFileL returns
     * 0x421de000 and rcomp's ids already carry that base, so adding it again asked for
     * 0x843bc003, which is no resource at all. The widely repeated `R_X + offset` idiom applies
     * to ids generated without a NAME signature; a .rss with `NAME` carries the base already.
     * Nothing but the returned value could distinguish those two readings. */
    HBufC* buffer = HBufC::NewLC(MaxReplyChars());
    TPtr text = buffer->Des();
    CAknTextQueryDialog* query = CAknTextQueryDialog::NewL(text);
    Trace(_L("ui: reply, about to ExecuteLD"));
    const TBool confirmed = query->ExecuteLD(ReplyDialogResourceId());
    Trace(_L("ui: reply, dialog dismissed"));

    if (!confirmed || !text.Length())
        {
        /* Cancelled. The entry created a moment ago has to go: leaving it would put an empty
         * invisible message in the user's Drafts for every reply they thought better of — and
         * invisible is exactly the state in which they could not find it to delete. */
        iBaseMtm.SwitchCurrentEntryL(aDestination);
        iBaseMtm.Entry().DeleteL(replyId);
        CleanupStack::PopAndDestroy(2, details);   // buffer, details

        TPckgBuf<TMsvLocalOperationProgress> progress;
        progress().iId = replyId;
        progress().iError = KErrCancel;
        return CMsvCompletedOperation::NewL(Session(), Type(), progress, serviceId,
                                            aCompletionStatus, KErrCancel);
        }

    /* The body, through the client component's own store path — the same one that reads it
     * back, so there is one definition of where a body lives rather than two. */
    iBaseMtm.SwitchCurrentEntryL(replyId);
    iBaseMtm.Body().Reset();
    iBaseMtm.Body().InsertL(0, text);
    iBaseMtm.SaveMessageL();

    /* And now it is a message: visible, complete, no longer in preparation, with the text as
     * its description so the native list has a line to draw.
     *
     * This ChangeL is what publishes it, and it happens **after** the body is committed. The
     * other order hands whatever is watching the store a visible message with nothing in it —
     * and something is always watching: the Messaging application itself, and any service
     * daemon observing session events. `symbian_mtm::is_pending` reads exactly these four
     * flags for exactly this reason. */
    TMsvEntry entry = iBaseMtm.Entry().Entry();
    entry.iDescription.Set(text);
    entry.iDate.HomeTime();
    entry.SetInPreparation(EFalse);
    entry.SetVisible(ETrue);
    entry.SetComplete(ETrue);
    iBaseMtm.Entry().ChangeL(entry);

    CleanupStack::PopAndDestroy(2, details);   // buffer, details
    Trace(_L("ui: reply published"));

    TPckgBuf<TMsvLocalOperationProgress> progress;
    progress().iTotalNumberOfEntries = 1;
    progress().iNumberCompleted = 1;
    progress().iId = replyId;
    return CMsvCompletedOperation::NewL(Session(), Type(), progress, serviceId,
                                        aCompletionStatus);
    }

/* ------------------------------------------------------------- not implemented --
 * Each leaves with KErrNotSupported, which is the documented way for a UI MTM to say an
 * operation is not offered. CMtmBaseUiData answers EFalse to the matching Can*L, so the
 * Messaging application does not offer these in the first place — the two have to agree, and a
 * Leave here is the backstop for a caller that asks anyway.
 */

CMsvOperation* CMtmBaseUi::CloseL(TRequestStatus& /*aStatus*/)
    {
    /* Nothing to close: the viewer is modal and gone by the time OpenL returns. */
    User::Leave(KErrNotSupported);
    return NULL;
    }

CMsvOperation* CMtmBaseUi::CloseL(TRequestStatus&, const CMsvEntrySelection&)
    {
    User::Leave(KErrNotSupported);
    return NULL;
    }

CMsvOperation* CMtmBaseUi::EditL(TRequestStatus& /*aStatus*/)
    {
    /* Editing an existing message needs an editor application, and the platform's own
     * editor-launch mechanism has no public header — see mtmbase.h. */
    User::Leave(KErrNotSupported);
    return NULL;
    }

CMsvOperation* CMtmBaseUi::EditL(TRequestStatus&, const CMsvEntrySelection&)
    {
    User::Leave(KErrNotSupported);
    return NULL;
    }

CMsvOperation* CMtmBaseUi::CancelL(TRequestStatus&, const CMsvEntrySelection&)
    {
    User::Leave(KErrNotSupported);
    return NULL;
    }

CMsvOperation* CMtmBaseUi::ForwardL(TMsvId, TMsvPartList, TRequestStatus&)
    {
    User::Leave(KErrNotSupported);
    return NULL;
    }
