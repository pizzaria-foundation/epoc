/* A Client MTM: the smallest one the Message Server will accept and instantiate.
 *
 * WHAT THIS IS FOR
 *
 * `apps/devdump`'s mtm probe registered a message type and the registry never listed it, and
 * three device trips went into theories about why. Reading the Message Server's own source
 * (SymbianSource, msgsrvnstore/server/src/MSVREG.CPP) settled it: the registration path does
 * not validate the DLL at all — it is loaded lazily on first use — and there is no capability
 * filter and no requirement for a server component. So the registration should have worked,
 * and the probe was measuring its own de-install.
 *
 * This is the thing that settles it for real: a DLL the framework can actually load and
 * instantiate. If `CClientMtmRegistry::NewMtmL` returns one of these, the registration path
 * works and everything after it — the UI Data MTM, the icon, opening a message — is ordinary
 * work rather than an open question.
 *
 * TWO CONSTRAINTS THAT SHAPE EVERY LINE BELOW
 *
 * **No writable static data.** A Symbian 9.x DLL is refused by the loader if it has any, and
 * this toolchain cannot ask for EPOCALLOWDLLDATA (elf2e32 has the flag as a `// FIXME`).
 * `tools/e32dump.py --expect-dll` is the gate. So: no file-scope mutable anything, and every
 * piece of state hangs off the object the framework hands us.
 *
 * **Static constructors never run.** elf2e32 sets KImageNoCallEntryPoint unconditionally, so
 * `_E32Dll` is never called and `__cpp_initialize__aeabi_` never runs. Anything that needs
 * initialising is initialised inside a function.
 *
 * WHAT IT DELIBERATELY DOES NOT DO
 *
 * Most of it. `CBaseMtm` has eleven pure virtuals and most of them are meaningless for a
 * service whose messages arrive from a daemon: forwarding has no meaning without a recipient
 * to forward to, addressees are a mail concept, and the MTM-specific function ids are for a UI
 * MTM to dispatch. Each of those leaves with KErrNotSupported, which is what the base class's own
 * documentation prescribes — a Client MTM is allowed to be this thin, and pretending
 * otherwise would be inventing behaviour nothing has asked for yet.
 *
 * The parts that are real are the ones the framework and the message store need: the body
 * cache, save and load, and the capability answers that decide what a caller may attempt.
 */

#include <e32base.h>
#include <f32file.h>
#include <msvapi.h>
#include <msvstd.h>
#include <msvuids.h>
#include <msvids.h>
#include <msvstore.h>
#include <mtclbase.h>
#include <txtrich.h>
#include <mtmdef.h>
#include <mtmuids.h>

#include "mtmdemo.h"

/* A breadcrumb file, written from inside the DLL.
 *
 * WHY THIS EXISTS
 *
 * The framework instantiates this DLL and the process dies, and from outside there is no way
 * to see how far it got: the probe records "about to call NewMtmL" and then nothing, because
 * the thing that died is the process doing the recording. Three explanations were plausible
 * and all three were guesses.
 *
 * So the DLL narrates itself. Each step appends a line before it is attempted, exactly as
 * the report format does one level up — and for the same reason: on a platform where a fault
 * is the process vanishing, the last line written is the diagnosis.
 *
 * It is deliberately the crudest possible I/O. Its own RFs session, opened and closed per
 * line, no buffering, no cleanup stack, nothing that can leave: an instrument that shares a
 * failure mode with what it measures is not an instrument. The cost is a file open per step,
 * which for five steps once is nothing.
 *
 * Remove this once construction is understood; it is diagnostic scaffolding, not a feature. */
void MtmDemoTrace(const TDesC& aStep)
    {
    RFs fs;
    if (fs.Connect() != KErrNone)
        return;
    RFile file;
    _LIT(KPath, "C:\\Data\\dump-mtmdemo.txt");
    /* Open-or-create, then seek to the end: RFile has no append mode, and Replace would
     * throw away the earlier steps, which are the whole point. */
    TInt err = file.Open(fs, KPath, EFileWrite | EFileShareAny);
    if (err != KErrNone)
        err = file.Create(fs, KPath, EFileWrite | EFileShareAny);
    if (err == KErrNone)
        {
        TInt pos = 0;
        file.Seek(ESeekEnd, pos);
        /* Written as 8-bit so the file is readable in any text editor without a BOM dance. */
        TBuf8<128> line;
        line.Copy(aStep.Left(120));
        line.Append(_L8("\r\n"));
        file.Write(line);
        file.Close();
        }
    fs.Close();
    }

/* FOUR exports, one per MTM component slot — and the reason is not that four components are
 * wanted.
 *
 * `CObserverRegistry::HandleSessionEventL` handles EMsvMtmGroupInstalled like this:
 *
 *     User::LeaveIfError(AddRegisteredMtmDll(..., *mtmgroupdata->MtmDllInfoArray()[index], ...));
 *
 * where `index` is the *fixed slot* of the component type this registry is for: 0 server,
 * 1 client, 2 UI, 3 UI data. It indexes the array the registration declared, with no bounds
 * check. A registration that declares only a client component therefore gives an array of
 * length one, and the client registry reads element [1] — past the end, which on this
 * platform is a panic, and a panic is not a Leave, so no TRAP catches it.
 *
 * That is what killed the probe on every run: not our code, which was never reached, but the
 * session dispatching the group-installed event on the next scheduler turn after the install.
 *
 * It is also why every real MTM declares four components. The SDK's own reference does;
 * every registration in the handset's ROM does — sms.rsc names smum.dll twice to fill the UI
 * and UI-data slots. The documentation says a group *may* declare only some components, and
 * about registration it is right; about what happens afterwards it is not.
 *
 * So all four slots are filled. Only the client is real. The other three exist to occupy
 * their slot and to leave cleanly if anything ever calls them — a clean KErrNotSupported is
 * an answer, where an absent slot is a dead process and a wrong factory would be a crash
 * inside Nokia's own Messaging application.
 *
 * ORDINALS ARE NOT DECLARATION ORDER
 *
 * elf2e32 numbers exports by sorting their names, so which ordinal each of these lands on is
 * not visible from this file. `symbuild` generates the registration resource from the .def
 * the linker actually produced, so the two cannot disagree — see MTM_RESOURCE_TEMPLATE in
 * apps/mtmdemo/app.conf.
 */

extern "C" EXPORT_C CBaseMtm* NewMtmClientL(CRegisteredMtmDll& aRegisteredDll,
                                            CMsvSession& aSession)
    {
    MtmDemoTrace(_L("1 factory entered"));
    CBaseMtm* mtm = CMtmDemoClient::NewL(aRegisteredDll, aSession);
    MtmDemoTrace(_L("5 factory returning"));
    return mtm;
    }

/* The three placeholders. Each leaves, which is the documented way for a component to say it
 * does not exist — and unlike an absent array slot, a Leave is something the caller survives.
 *
 * Their signatures match the framework's typedefs so that a caller which does invoke them
 * enters a function with the stack shape it expects, rather than jumping into one that reads
 * its arguments from somewhere else. Getting that wrong is how a "safe" placeholder corrupts
 * the caller instead of refusing it. */
extern "C" EXPORT_C TAny* NewMtmServerL(CRegisteredMtmDll& /*aRegisteredDll*/,
                                        TAny* /*aServerEntry*/)
    {
    MtmDemoTrace(_L("server component asked for — leaving"));
    User::Leave(KErrNotSupported);
    return NULL;
    }

/* Real now. The viewer that runs when the user taps one of our messages — see
 * src/mtmui.cpp. */
extern "C" EXPORT_C CBaseMtmUi* NewMtmUiL(CBaseMtm& aMtm, CRegisteredMtmDll& aRegisteredDll)
    {
    MtmDemoTrace(_L("ui component asked for"));
    return CMtmDemoUi::NewL(aMtm, aRegisteredDll);
    }

/* Real now, unlike its two neighbours. The UI Data component is what gives our messages an
 * icon and decides which menu items the Messaging application offers — see
 * src/mtmuidata.cpp. */
extern "C" EXPORT_C CBaseMtmUiData* NewMtmUiDataL(CRegisteredMtmDll& aRegisteredDll)
    {
    MtmDemoTrace(_L("ui data component asked for"));
    return CMtmDemoUiData::NewL(aRegisteredDll);
    }

CMtmDemoClient* CMtmDemoClient::NewL(CRegisteredMtmDll& aRegisteredDll, CMsvSession& aSession)
    {
    MtmDemoTrace(_L("2 about to allocate"));
    CMtmDemoClient* self = new (ELeave) CMtmDemoClient(aRegisteredDll, aSession);
    MtmDemoTrace(_L("3 allocated; base ctor survived"));
    CleanupStack::PushL(self);
    self->ConstructL();
    CleanupStack::Pop(self);
    MtmDemoTrace(_L("4 ConstructL survived"));
    return self;
    }

CMtmDemoClient::CMtmDemoClient(CRegisteredMtmDll& aRegisteredDll, CMsvSession& aSession)
    : CBaseMtm(aRegisteredDll, aSession)
    {
    }

void CMtmDemoClient::ConstructL()
    {
    /* Establish a context, and nothing else.
     *
     * `CBaseMtm` starts with iMsvEntry null and the framework calls into a freshly built MTM
     * without necessarily setting one first, so anything touching the current entry faults.
     * The reference implementation opens on the root for exactly this reason, and that is the
     * only thing its own ConstructL does apart from its settings object.
     *
     * WHAT USED TO BE HERE, AND WHY IT KILLED THE FRAMEWORK
     *
     * This function created iParaFormatLayer and iCharFormatLayer, under a comment asserting
     * that the base class does not create them — an assertion made without checking. The
     * reference creates neither. Those members belong to CBaseMtm: overwriting its pointers
     * leaked what it had built, and the destructor below then deleted objects the base's own
     * destructor deletes again.
     *
     * The double free happened inside whichever process loaded this DLL, and on the handset
     * that was the framework instantiating us through CClientMtmRegistry::NewMtmL — so the
     * crash landed in the one call that made it look as though registration had failed, when
     * registration had in fact just succeeded. */
    MtmDemoTrace(_L("3a about to SwitchCurrentEntryL(root)"));
    SwitchCurrentEntryL(KMsvRootIndexEntryId);
    MtmDemoTrace(_L("3b SwitchCurrentEntryL returned"));
    }

CMtmDemoClient::~CMtmDemoClient()
    {
    /* Empty, deliberately. This class allocates nothing of its own: the format layers, the
     * body cache and the addressee list all belong to CBaseMtm and are freed by its
     * destructor. Freeing them here is what the previous version did. */
    }

/* ------------------------------------------------------------------ the store -- */

void CMtmDemoClient::SaveMessageL()
    {
    /* The context is whatever entry the caller switched us to. Writing without one is the
     * caller's mistake and the framework's own MTMs treat it as unreachable. */
    CMsvStore* store = iMsvEntry->EditStoreL();
    CleanupStack::PushL(store);
    StoreBodyL(*store);
    store->CommitL();
    CleanupStack::PopAndDestroy(store);
    }

void CMtmDemoClient::LoadMessageL()
    {
    CMsvStore* store = iMsvEntry->ReadStoreL();
    CleanupStack::PushL(store);
    RestoreBodyL(*store);
    CleanupStack::PopAndDestroy(store);
    }

void CMtmDemoClient::ContextEntrySwitched()
    {
    /* Called by the base class whenever the current entry changes.
     *
     * Nothing to do here, and that is a statement rather than an omission: this MTM keeps no
     * per-entry state of its own. The body cache belongs to the base class, which manages it
     * across a switch; the addressee list is the base class's too and is never populated.
     * A subclass with cached settings or a parsed header would clear them here, and leaving
     * that out is how one message's data ends up displayed under another's. */
    }

/* ------------------------------------------------------- validation and search -- */

TMsvPartList CMtmDemoClient::ValidateMessage(TMsvPartList /*aPartList*/)
    {
    /* Zero means "nothing wrong with any part asked about". A message that arrives from a
     * daemon has already been validated by whatever produced it, and inventing a rule here
     * would reject messages for a reason nothing enforces elsewhere. */
    return 0;
    }

TMsvPartList CMtmDemoClient::Find(const TDesC& /*aTextToFind*/, TMsvPartList /*aPartList*/)
    {
    /* Allowed to always answer 0 — the base class documentation says so explicitly. Global
     * find over this MTM's messages is a feature, not an obligation, and answering "found"
     * without searching would be worse than answering "not found" without searching. */
    return 0;
    }

/* ------------------------------------------------------------- not this layer's -- */

CMsvOperation* CMtmDemoClient::ReplyL(TMsvId aDestination, TMsvPartList aPartlist,
                                      TRequestStatus& aCompletionStatus)
    {
    /* Create the reply entry, and stop there.
     *
     * This split is the framework's, not ours: `CBaseMtmUi::ReplyL`'s own documentation
     * prescribes *"1. create a new reply entry by calling CBaseMtm::ReplyL(); 2. call EditL()
     * to allow the user to edit the reply"*. So the entry is made here, where there is no UI
     * and no assumption about one, and the editing happens in the UI component. A caller with
     * no screen at all — a daemon answering automatically — gets the useful half by calling
     * only this.
     *
     * What comes back is an entry in preparation and invisible. It becomes a real message when
     * whoever is going to fill in the body says so; until then it must not appear in the
     * user's inbox as an empty thing they cannot explain. */
    const TMsvEntry original = iMsvEntry->Entry();
    const TMsvId originalId = original.Id();
    const TMsvId serviceId = original.iServiceId;

    /* The correspondent, copied out before anything moves.
     *
     * `TMsvEntry::iDetails` and `iDescription` are `TPtrC` (`msvstd.h`) — they do not own their
     * text, they point into the buffer of the `CMsvEntry` that produced the entry. Copying the
     * struct copies the pointer, and the two `SwitchCurrentEntryL` calls below reload that
     * buffer. So a `reply.iDetails.Set(original.iDetails)` written against the local copy hands
     * `CreateL` a dangling descriptor.
     *
     * That is exactly what the first version of this function did, and it took the Messaging
     * application down on the first reply. Opening a message survived it because the viewer
     * never switches context.
     *
     * An owned copy, allocated rather than a fixed `TBuf`, because there is no documented cap
     * on this field and a chat identity is not obliged to be short. */
    HBufC* details = original.iDetails.AllocLC();

    TMsvEntry reply;
    reply.iType = KUidMsvMessageEntry;
    reply.iMtm = Type();
    reply.iServiceId = serviceId;
    /* Who the reply goes to. For this MTM the correspondent lives in iDetails — there is no
     * addressee list, because a chat identity is not a mail recipient (see AddAddresseeL). */
    reply.iDetails.Set(*details);
    reply.iDate.HomeTime();
    reply.SetInPreparation(ETrue);
    reply.SetVisible(EFalse);

    /* Created under aDestination, which the caller chooses — Drafts while it is being written,
     * or the outbox for something ready to go. Switching the context there is how CMsvEntry
     * creates a child, and the context is switched again straight afterwards because the
     * framework's contract is that it ends up on the reply. */
    MtmDemoTrace(_L("reply-c1 creating entry"));
    SwitchCurrentEntryL(aDestination);
    iMsvEntry->CreateL(reply);
    SwitchCurrentEntryL(reply.Id());
    MtmDemoTrace(_L("reply-c2 entry created, context switched"));

    /* The body starts empty, then optionally quotes the original.
     *
     * `Body()` is the base class's cache and it belongs to whatever entry was last loaded, so
     * resetting it is not tidiness — skipping it is how the previous message's text ends up in
     * a reply. `KMsvMessagePartBody` in aPartlist is the caller asking for the original to be
     * included, which is the one part of the partlist protocol this MTM honours. */
    Body().Reset();
    if (aPartlist & KMsvMessagePartBody)
        {
        /* A CMsvEntry of our own on the original, because the context is now the reply and
         * `Session().GetEntryL` hands over ownership — reading the store off it inline would
         * leak the entry on every reply. */
        CMsvEntry* source = Session().GetEntryL(originalId);
        CleanupStack::PushL(source);
        CMsvStore* store = source->ReadStoreL();
        CleanupStack::PushL(store);
        if (store->HasBodyTextL())
            store->RestoreBodyTextL(Body());
        CleanupStack::PopAndDestroy(2, source);   // store, source
        }

    /* Completed before it is returned: nothing here is asynchronous. The caller still gets a
     * CMsvOperation because that is the signature, and CMsvCompletedOperation is the
     * platform's own way to hand back one that has already finished. */
    /* The details copy has done its job — CreateL took its own copy of the text. */
    const TMsvId replyId = reply.Id();
    CleanupStack::PopAndDestroy(details);

    /* Completed before it is returned: nothing here is asynchronous. The caller still gets a
     * CMsvOperation because that is the signature, and CMsvCompletedOperation is the
     * platform's own way to hand back one that has already finished. */
    TPckgBuf<TMsvLocalOperationProgress> progress;
    progress().iTotalNumberOfEntries = 1;
    progress().iNumberCompleted = 1;
    progress().iId = replyId;
    MtmDemoTrace(_L("reply-c3 returning operation"));
    return CMsvCompletedOperation::NewL(Session(), Type(), progress,
                                        serviceId, aCompletionStatus);
    }

CMsvOperation* CMtmDemoClient::ForwardL(TMsvId, TMsvPartList, TRequestStatus&)
    {
    User::Leave(KErrNotSupported);
    return NULL;
    }

void CMtmDemoClient::AddAddresseeL(const TDesC&)
    {
    /* Addressees are a mail-shaped idea. A service where the recipient is a chat identity
     * carries it in the entry's own fields, not in a recipient list. */
    User::Leave(KErrNotSupported);
    }

void CMtmDemoClient::AddAddresseeL(const TDesC&, const TDesC&)
    {
    User::Leave(KErrNotSupported);
    }

void CMtmDemoClient::RemoveAddressee(TInt)
    {
    /* Cannot leave — the signature has no way to say no, so it does nothing. */
    }

void CMtmDemoClient::InvokeSyncFunctionL(TInt, const CMsvEntrySelection&, TDes8&)
    {
    User::Leave(KErrNotSupported);
    }

CMsvOperation* CMtmDemoClient::InvokeAsyncFunctionL(TInt, const CMsvEntrySelection&, TDes8&,
                                                    TRequestStatus&)
    {
    User::Leave(KErrNotSupported);
    return NULL;
    }

/* --------------------------------------------------------------- capabilities -- */

TInt CMtmDemoClient::QueryCapability(TUid aCapability, TInt& aResponse)
    {
    /* What a caller may attempt. These answers are read by SendAs when it builds the
     * system-wide "Send via…" list and by the Messaging application when it decides which
     * menu items to offer, so they are the closest thing this layer has to a user interface.
     *
     * Deliberately conservative: this MTM can receive and it carries a body, and it claims
     * nothing else. Claiming a capability we do not implement is how a menu item appears and
     * then fails. */
    switch (aCapability.iUid)
        {
        case KUidMtmQueryMaxBodySizeValue:
            /* No limit worth enforcing here; the transport decides. */
            aResponse = KMaxTInt;
            return KErrNone;

        case KUidMtmQuerySupportedBodyValue:
            aResponse = KMtm16BitBody;
            return KErrNone;

        case KUidMtmQueryCanReceiveMsgValue:
            aResponse = ETrue;
            return KErrNone;

        case KUidMtmQueryCanSendMsgValue:
            /* ETrue, matching the UI-data component word for word — see the longer note there.
             * `ReplyL` above produces an outgoing message, so "cannot send" was a contradiction
             * with this MTM's own behaviour.
             *
             * What this does *not* claim is that this MTM delivers anything. It writes the
             * message into the store; a daemon outside Nokia's process picks it up. The
             * distinction lives in the SendAs answer below, which is the question that decides
             * whether other applications offer us. */
            aResponse = ETrue;
            return KErrNone;

        case KUidMtmQuerySendAsMessageSendSupportValue:
            /* Not for SendAs. Nothing here carries a message out on another application's
             * behalf, and appearing in "Send via…" would take a Gallery photo and lose it. */
            aResponse = EFalse;
            return KErrNone;

        case KUidMtmQuerySupportAttachmentsValue:
        case KUidMtmQuerySupportSubjectValue:
            aResponse = EFalse;
            return KErrNone;

        case KUidMtmQueryOffLineAllowedValue:
            /* Messages are produced locally by a daemon, so there is nothing to be offline
             * from as far as this layer is concerned. */
            aResponse = ETrue;
            return KErrNone;

        default:
            /* The documented answer for a query this MTM has no opinion on. */
            return KErrNotSupported;
        }
    }
