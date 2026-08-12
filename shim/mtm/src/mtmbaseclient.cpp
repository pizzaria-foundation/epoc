/* The Client MTM: the message store side. See shim/mtm/inc/mtmbase.h for the four constraints
 * every line here obeys, and for what is fixed versus what a subclass may change.
 *
 * Extracted from apps/mtmdemo, which proved every line of it on an E72. The comments naming
 * what a construct cost are not history for its own sake: each one is a thing a service
 * copying this code would otherwise be free to undo.
 */

#include <e32base.h>
#include <f32file.h>
#include <msvapi.h>
#include <msvstd.h>
#include <msvuids.h>
#include <msvids.h>
#include <msvstore.h>
#include <msvipc.h>
#include <mtclbase.h>
#include <mtmdef.h>
#include <mtmuids.h>
#include <txtrich.h>

#include "mtmbase.h"

/* ------------------------------------------------------------------ the trace -- */

#ifdef MTM_TRACE
void MtmBaseTrace(const TDesC& aPath, const TDesC& aStep)
    {
    if (!aPath.Length())
        return;
    RFs fs;
    if (fs.Connect() != KErrNone)
        return;
    RFile file;
    /* Open-or-create, then seek to the end: RFile has no append mode, and Replace would throw
     * away the earlier steps, which are the whole point. */
    TInt err = file.Open(fs, aPath, EFileWrite | EFileShareAny);
    if (err != KErrNone)
        err = file.Create(fs, aPath, EFileWrite | EFileShareAny);
    if (err == KErrNone)
        {
        TInt pos = 0;
        file.Seek(ESeekEnd, pos);
        /* 8-bit, so the file reads in any text editor without a BOM dance. */
        TBuf8<160> line;
        line.Copy(aStep.Left(150));
        line.Append(_L8("\r\n"));
        file.Write(line);
        file.Close();
        }
    fs.Close();
    }
#else
void MtmBaseTrace(const TDesC&, const TDesC&)
    {
    /* Compiled away. Set MTM_TRACE=1 in app.conf to get the breadcrumbs back. */
    }
#endif

/* ----------------------------------------------------------------- the client -- */

CMtmBaseClient::CMtmBaseClient(CRegisteredMtmDll& aRegisteredDll, CMsvSession& aSession)
    : CBaseMtm(aRegisteredDll, aSession)
    {
    }

CMtmBaseClient::~CMtmBaseClient()
    {
    /* Empty, deliberately, and a subclass's should be too unless it allocated something.
     *
     * The format layers, the body cache and the addressee list all belong to CBaseMtm and are
     * freed by its destructor. An earlier version of this code created iParaFormatLayer and
     * iCharFormatLayer in ConstructL under a comment asserting the base class does not — an
     * assertion made without checking, and the reference creates neither. Overwriting the
     * base's pointers leaked what it had built and then double-freed it here.
     *
     * The double free happened inside whichever process loaded the DLL, which for the client
     * component is the framework instantiating it through CClientMtmRegistry::NewMtmL. So the
     * crash landed in the one call that made it look as though registration had failed, when
     * registration had in fact just succeeded. Three device trips went into that. */
    }

void CMtmBaseClient::ConstructL()
    {
    /* Establish a context, and nothing else.
     *
     * CBaseMtm starts with iMsvEntry null and the framework calls into a freshly built MTM
     * without necessarily setting one first, so anything touching the current entry faults.
     * The reference implementation opens on the root for exactly this reason. */
    Trace(_L("client: ConstructL, switching to root"));
    SwitchCurrentEntryL(KMsvRootIndexEntryId);
    Trace(_L("client: context established"));
    }

void CMtmBaseClient::TraceFileName(TFileName& aName) const
    {
    /* Empty: no tracing unless the subclass names a file. */
    aName.Zero();
    }

void CMtmBaseClient::Trace(const TDesC& aStep) const
    {
    TFileName path;
    TraceFileName(path);
    MtmBaseTrace(path, aStep);
    }

void CMtmBaseClient::ContextEntrySwitched()
    {
    /* Nothing, and that is a statement rather than an omission: this class keeps no per-entry
     * state. The body cache belongs to the base class, which manages it across a switch.
     *
     * A subclass with cached settings or a parsed header clears them here. Leaving that out is
     * how one message's data ends up displayed under another's. */
    }

/* ------------------------------------------------------------------ the store -- */

void CMtmBaseClient::SaveMessageL()
    {
    /* The context is whatever entry the caller switched to. Writing without one is the
     * caller's mistake, and the framework's own MTMs treat it as unreachable. */
    CMsvStore* store = iMsvEntry->EditStoreL();
    CleanupStack::PushL(store);
    StoreBodyL(*store);
    store->CommitL();
    CleanupStack::PopAndDestroy(store);
    }

void CMtmBaseClient::LoadMessageL()
    {
    CMsvStore* store = iMsvEntry->ReadStoreL();
    CleanupStack::PushL(store);
    RestoreBodyL(*store);
    CleanupStack::PopAndDestroy(store);
    }

/* ------------------------------------------------------- validation and search -- */

TMsvPartList CMtmBaseClient::ValidateMessage(TMsvPartList /*aPartList*/)
    {
    /* Zero means "nothing wrong with any part asked about". A message that arrives from a
     * daemon has already been validated by whatever produced it, and inventing a rule here
     * would reject messages for a reason nothing enforces elsewhere. */
    return 0;
    }

TMsvPartList CMtmBaseClient::Find(const TDesC& /*aTextToFind*/, TMsvPartList /*aPartList*/)
    {
    /* Allowed to always answer 0 — the base class documentation says so explicitly. Global
     * find over an MTM's messages is a feature, not an obligation, and answering "found"
     * without searching would be worse than answering "not found" without searching. */
    return 0;
    }

/* ------------------------------------------------------------------- replying -- */

CMsvOperation* CMtmBaseClient::ReplyL(TMsvId aDestination, TMsvPartList aPartlist,
                                      TRequestStatus& aCompletionStatus)
    {
    /* Create the reply entry, and stop there.
     *
     * This split is the framework's: CBaseMtmUi::ReplyL's own documentation prescribes
     * *"1. create a new reply entry by calling CBaseMtm::ReplyL(); 2. call EditL() to allow
     * the user to edit the reply"*. So the entry is made here, where there is no UI and no
     * assumption of one — a daemon answering automatically gets the useful half by calling
     * only this.
     *
     * What comes back is in preparation and invisible. It becomes a real message when whoever
     * fills in the body says so; until then it must not appear in the user's folder as an
     * empty thing they cannot explain. */
    const TMsvEntry original = iMsvEntry->Entry();
    const TMsvId originalId = original.Id();
    const TMsvId serviceId = original.iServiceId;

    /* The correspondent, copied out before anything moves.
     *
     * TMsvEntry::iDetails and iDescription are TPtrC (msvstd.h) — they do not own their text,
     * they point into the buffer of the CMsvEntry that produced the entry. Copying the struct
     * copies the pointer, and the two SwitchCurrentEntryL calls below reload that buffer. So
     * `reply.iDetails.Set(original.iDetails)` written against the local copy hands CreateL a
     * dangling descriptor.
     *
     * That is exactly what the first version did, and it took the Messaging application down
     * on the first reply. Opening a message survived it because the viewer never switches
     * context. Allocated rather than a fixed TBuf because this field has no documented cap. */
    HBufC* details = original.iDetails.AllocLC();

    TMsvEntry reply;
    reply.iType = KUidMsvMessageEntry;
    reply.iMtm = Type();
    reply.iServiceId = serviceId;
    reply.iDetails.Set(*details);
    reply.iDate.HomeTime();
    reply.SetInPreparation(ETrue);
    reply.SetVisible(EFalse);

    /* Created under aDestination, which the caller chooses. Switching the context there is how
     * CMsvEntry creates a child, and the context is switched again straight afterwards because
     * the framework's contract is that it ends on the reply. */
    Trace(_L("client: creating reply entry"));
    SwitchCurrentEntryL(aDestination);
    iMsvEntry->CreateL(reply);
    const TMsvId replyId = reply.Id();
    SwitchCurrentEntryL(replyId);

    /* The body starts empty, then optionally quotes the original.
     *
     * Body() is the base class's cache and belongs to whatever entry was last loaded, so
     * resetting it is not tidiness — skipping it is how the previous message's text ends up in
     * a reply. KMsvMessagePartBody in aPartlist is the caller asking for the original to be
     * included. */
    Body().Reset();
    if (aPartlist & KMsvMessagePartBody)
        {
        /* A CMsvEntry of our own on the original, because the context is now the reply and
         * Session().GetEntryL hands over ownership — reading the store off it inline would
         * leak the entry on every reply. */
        CMsvEntry* source = Session().GetEntryL(originalId);
        CleanupStack::PushL(source);
        CMsvStore* store = source->ReadStoreL();
        CleanupStack::PushL(store);
        if (store->HasBodyTextL())
            store->RestoreBodyTextL(Body());
        CleanupStack::PopAndDestroy(2, source);   // store, source
        }

    CleanupStack::PopAndDestroy(details);   // CreateL took its own copy of the text

    /* Completed before it is returned: nothing here is asynchronous. The caller still gets a
     * CMsvOperation because that is the signature, and CMsvCompletedOperation is the
     * platform's own way to hand back one already finished.
     *
     * It is *not* completed on construction, though — it derives from CMsvOperation : CActive
     * and signals in RunL, on the next scheduler turn. A caller that waits on the status from
     * the same thread deadlocks the scheduler that has to run for that to happen. See
     * CMtmBaseUi::ReplyL, which is why it does not call this function. */
    TPckgBuf<TMsvLocalOperationProgress> progress;
    progress().iTotalNumberOfEntries = 1;
    progress().iNumberCompleted = 1;
    progress().iId = replyId;
    return CMsvCompletedOperation::NewL(Session(), Type(), progress,
                                        serviceId, aCompletionStatus);
    }

/* ------------------------------------------------------------- not this layer's --
 * Each leaves with KErrNotSupported, which is what CBaseMtm's own documentation prescribes for
 * a message type that does not offer an operation.
 */

CMsvOperation* CMtmBaseClient::ForwardL(TMsvId, TMsvPartList, TRequestStatus&)
    {
    /* Forwarding needs a recipient to forward to, and this family of MTMs has no addressee
     * list — see AddAddresseeL. */
    User::Leave(KErrNotSupported);
    return NULL;
    }

void CMtmBaseClient::AddAddresseeL(const TDesC&)
    {
    /* Addressees are a mail-shaped idea. A service whose recipient is a chat identity carries
     * it in the entry's own iDetails, not in a recipient list — which is the choice the whole
     * reply path is built on. */
    User::Leave(KErrNotSupported);
    }

void CMtmBaseClient::AddAddresseeL(const TDesC&, const TDesC&)
    {
    User::Leave(KErrNotSupported);
    }

void CMtmBaseClient::RemoveAddressee(TInt)
    {
    /* Cannot leave — the signature has no way to say no, so it does nothing. */
    }

void CMtmBaseClient::InvokeSyncFunctionL(TInt, const CMsvEntrySelection&, TDes8&)
    {
    User::Leave(KErrNotSupported);
    }

CMsvOperation* CMtmBaseClient::InvokeAsyncFunctionL(TInt, const CMsvEntrySelection&, TDes8&,
                                                    TRequestStatus&)
    {
    User::Leave(KErrNotSupported);
    return NULL;
    }

/* --------------------------------------------------------------- capabilities -- */

TInt CMtmBaseClient::QueryCapability(TUid aCapability, TInt& aResponse)
    {
    switch (aCapability.iUid)
        {
        case KUidMtmQueryMaxBodySizeValue:
            /* No limit of our own. A chat message has no natural cap, and inventing one here
             * would reject text nothing else refuses. */
            aResponse = KMaxTInt;
            return KErrNone;

        case KUidMtmQuerySupportedBodyValue:
            aResponse = KMtm16BitBody;
            return KErrNone;

        case KUidMtmQueryCanReceiveMsgValue:
            aResponse = ETrue;
            return KErrNone;

        case KUidMtmQueryCanSendMsgValue:
            /* ETrue, because replying produces an outgoing message.
             *
             * This was EFalse alongside a CanReplyToEntryL of ETrue, and the two could not
             * both be right — a reply *is* a send. The visible consequence was the reply item
             * never appearing on the Messaging application's menu at all, with nothing failing
             * anywhere. Whichever of the three declarations of "can send" MCE reads (this one,
             * the UI-data component's, and send_capability in the registration), they have to
             * agree. */
            aResponse = ETrue;
            return KErrNone;

        case KUidMtmQuerySendAsMessageSendSupportValue:
            /* And separately EFalse: this is the narrow SendAs question, whether *other*
             * applications should offer this MTM in "Send via…". They should not. Nothing here
             * carries another application's data out, and a Gallery photo handed to this MTM
             * would go nowhere with no way for the user to find out. */
            aResponse = EFalse;
            return KErrNone;

        case KUidMtmQuerySupportAttachmentsValue:
        case KUidMtmQuerySupportSubjectValue:
            aResponse = EFalse;
            return KErrNone;

        case KUidMtmQueryOffLineAllowedValue:
            /* Messages are produced locally by a daemon, so there is nothing to be offline
             * from as far as the store is concerned. */
            aResponse = ETrue;
            return KErrNone;

        default:
            /* Including KUidMsvMtmQueryEditorUid (0x10001641), deliberately unanswered:
             * naming an editor application would let MCE launch it instead of calling this
             * MTM's own OpenL and ReplyL, which are the paths that work. A service that ships
             * a real editor overrides this. */
            return KErrNotSupported;
        }
    }
