/* The UI Data MTM: what the native Messaging application asks before it draws anything.
 *
 * This is the component that decides how a message of ours *looks* and which menu items the
 * user is offered. Delivery does not need it — a message written straight into the inbox
 * already appears — but it appears under the platform's "unknown type" envelope and cannot
 * be opened, and both of those are this component's answers.
 *
 * IT RUNS INSIDE NOKIA'S PROCESS, WHICH CHANGES EVERY RULE
 *
 * `CMtmUiDataRegistry` loads this DLL into whatever application is drawing a message list —
 * the Messaging application, and anything else that shows messages. Until now a fault in our
 * code cost a probe. From here it costs the user's Messaging application.
 *
 * That is why this file diverges from the SDK's reference implementation in one specific
 * way. `txti` opens almost every method with
 *
 *     __ASSERT_ALWAYS(aContext.iMtm==KUidMsgTypeText, Panic(ETxtiMtmUdWrongMtm));
 *
 * and that is reasonable for sample code running under TechView, where a panic is a
 * developer's problem. Here the same line is a way to kill somebody's Messaging application
 * because a caller passed an entry we did not expect. So nothing in this file panics or
 * asserts. Every method takes the defensive branch and returns something safe, because the
 * cost of being wrong is not symmetric.
 *
 * NO WRITABLE STATIC DATA, STILL
 *
 * Same constraint as the client component: a Symbian 9.x DLL with writable statics is
 * refused by the loader, and `tools/e32dump.py --expect-dll` is the gate. The icon array and
 * everything else hang off the object.
 */

#include <e32base.h>
#include <msvstd.h>
#include <msvuids.h>
#include <msvids.h>
#include <mtmdef.hrh>
#include <mtud.hrh>

#include "mtmdemo.h"

/* Where the compiled UI resource and the icon bitmaps land. `symbuild` installs both to
 * \resource\messaging\, which is where the framework looks for a UI-data component's files
 * and where every one of the platform's own MTMs keeps them. */
_LIT(KMtmDemoBitmapFile, "C:\\resource\\messaging\\mtmdemo.mbm");

/* One zoom state. The framework wants an array of the same icon at several sizes and lets
 * the caller pick; the Messaging application on this handset resolves its own icons through
 * AknSkins and only consults ours as a fallback, so shipping one size is honest rather than
 * padding the file with scaled copies nothing asks for. */
const TInt KMtmDemoZoomStates = 1;

/* Index into iIconArrays, in the order PopulateArraysL fills them. Must match the order the
 * bitmaps sit in the .mbm, because CreateBitmapsL walks the file from first to last. */
enum TMtmDemoIcon
    {
    EMtmDemoIconMessage = 0,
    EMtmDemoIconService,
    EMtmDemoIconCount
    };

CMtmDemoUiData* CMtmDemoUiData::NewL(CRegisteredMtmDll& aRegisteredDll)
    {
    CMtmDemoUiData* self = new (ELeave) CMtmDemoUiData(aRegisteredDll);
    CleanupStack::PushL(self);
    /* CBaseMtmUiData::ConstructL is what calls GetResourceFileName and PopulateArraysL — the
     * subclass must not do that work itself or it happens twice. */
    self->ConstructL();
    CleanupStack::Pop(self);
    return self;
    }

CMtmDemoUiData::CMtmDemoUiData(CRegisteredMtmDll& aRegisteredDll)
    : CBaseMtmUiData(aRegisteredDll), iTraced(0)
    {
    }

/* Record a question the Messaging application asked, the first time it asks it.
 *
 * WHY THIS IS HERE AT ALL
 *
 * `CanReplyToEntryL` was flipped to ETrue and the reply item did not appear in the menu. That
 * leaves two very different explanations — MCE never asks this component, or it asks and then
 * gates the item on something else — and no amount of reading settles which, because MCE is a
 * closed binary and the SDK documents MTMs only against Symbian's own TechView.
 *
 * So the component reports what it is asked. One trip then says whether the answers here are
 * even consulted, which is the difference between fixing our answers and abandoning this route.
 *
 * Diagnostic scaffolding. It comes out once the menu is understood. */
void CMtmDemoUiData::TraceOnce(TInt aBit, const TDesC& aWhat) const
    {
    const TUint32 mask = 1u << aBit;
    if (iTraced & mask)
        return;
    iTraced |= mask;
    MtmDemoTrace(aWhat);
    }

CMtmDemoUiData::~CMtmDemoUiData()
    {
    /* iIconArrays and iMtmSpecificFunctions belong to the base class, which frees them. */
    }

void CMtmDemoUiData::GetResourceFileName(TFileName& aFileName) const
    {
    aFileName = KMtmDemoResourceFile;
    }

void CMtmDemoUiData::PopulateArraysL()
    {
    /* Icons first, because ContextIcon returns a *reference* into this array and has no way
     * to fail. An empty array there is not an empty icon, it is an out-of-bounds read inside
     * the Messaging application — so if this leaves, the component fails to construct, which
     * is the outcome we want over one that constructs and faults later.
     *
     * The two bitmaps are contiguous in the .mbm and CreateBitmapsL walks first to last. */
    CreateBitmapsL(KMtmDemoZoomStates, KMtmDemoBitmapFile, 0, EMtmDemoIconCount - 1);

    /* No MTM-specific menu functions. ReadFunctionsFromResourceFileL is deliberately not
     * called: this MTM adds no commands of its own to the Messaging application's menu, and
     * an empty MTUD_FUNCTION_ARRAY would be a resource to keep in step for no gain. */
    }

const CBaseMtmUiData::CBitmapArray& CMtmDemoUiData::ContextIcon(const TMsvEntry& aContext,
                                                                TInt /*aStateFlags*/) const
    {
    /* Chosen by entry type, and clamped. The reference asserts that the entry belongs to
     * this MTM and panics if not; here a wrong entry gets the message icon, because the
     * difference between a slightly wrong picture and a dead Messaging application is the
     * whole reason this file exists.
     *
     * The clamp is not paranoia either: iIconArrays holds whatever PopulateArraysL managed to
     * load, and indexing it past the end is the same out-of-bounds panic that killed the
     * probe eight times through a different array. */
    TInt index = EMtmDemoIconMessage;
    if (aContext.iType == KUidMsvServiceEntry)
        index = EMtmDemoIconService;

    const TInt count = iIconArrays ? iIconArrays->Count() : 0;
    if (index >= count)
        index = 0;

    return *iIconArrays->At(index);
    }

/* --------------------------------------------------------------- what is offered --
 * Each of these decides whether the Messaging application offers a menu item. The return
 * value is whether the operation is allowed; aReasonResourceId is a resource id for the text
 * explaining why not, and 0 means "no explanation available" — which is honest here, since
 * this component ships no reason strings.
 *
 * The shape of the answers is the current state of the integration rather than a policy:
 * messages arrive from a daemon, nothing can be composed, and there is no UI MTM to open or
 * edit anything. Every one of these becomes ETrue as the component behind it appears.
 */

TBool CMtmDemoUiData::CanCreateEntryL(const TMsvEntry& aParent, TMsvEntry& aNewEntry,
                                      TInt& aReasonResourceId) const
    {
    aReasonResourceId = 0;
    /* A service may be created, and only under the root — which is what every UI Data MTM in
     * the reference checks, and what the framework requires of a service entry. Messages are
     * created by the daemon through the client API, not through the Messaging application. */
    if (aNewEntry.iType == KUidMsvServiceEntry)
        return aParent.Id() == KMsvRootIndexEntryId;
    return EFalse;
    }

TBool CMtmDemoUiData::CanDeleteFromEntryL(const TMsvEntry& /*aContext*/,
                                          TInt& aReasonResourceId) const
    {
    aReasonResourceId = 0;
    /* Deleting must work. It is how a user gets rid of our messages, and refusing it would
     * leave them stuck in the inbox with no way out short of uninstalling. */
    return ETrue;
    }

TBool CMtmDemoUiData::CanDeleteServiceL(const TMsvEntry& /*aService*/,
                                        TInt& aReasonResourceId) const
    {
    aReasonResourceId = 0;
    return ETrue;
    }

TBool CMtmDemoUiData::CanReplyToEntryL(const TMsvEntry& aContext,
                                       TInt& aReasonResourceId) const
    {
    aReasonResourceId = 0;
    TraceOnce(0, _L("uid-asked CanReplyToEntryL"));
    /* ETrue only because `CMtmDemoUi::ReplyL` exists in this same build — the same rule as
     * CanOpenEntryL below, and the same reason. Only messages: a service entry has nobody to
     * reply to. */
    return aContext.iType == KUidMsvMessageEntry;
    }

TBool CMtmDemoUiData::CanForwardEntryL(const TMsvEntry& /*aContext*/,
                                       TInt& aReasonResourceId) const
    {
    aReasonResourceId = 0;
    TraceOnce(3, _L("uid-asked CanForwardEntryL"));
    return EFalse;
    }

TBool CMtmDemoUiData::CanEditEntryL(const TMsvEntry& /*aContext*/,
                                    TInt& aReasonResourceId) const
    {
    aReasonResourceId = 0;
    TraceOnce(4, _L("uid-asked CanEditEntryL"));
    return EFalse;
    }

TBool CMtmDemoUiData::CanViewEntryL(const TMsvEntry& aContext,
                                    TInt& aReasonResourceId) const
    {
    aReasonResourceId = 0;
    TraceOnce(2, _L("uid-asked CanViewEntryL"));
    /* Only messages. Viewing a service entry would open a dialog on an entry that has no body
     * and never will. */
    return aContext.iType == KUidMsvMessageEntry;
    }

TBool CMtmDemoUiData::CanOpenEntryL(const TMsvEntry& aContext,
                                    TInt& aReasonResourceId) const
    {
    aReasonResourceId = 0;
    TraceOnce(1, _L("uid-asked CanOpenEntryL"));
    /* ETrue only because the UI component that answers it exists in this same build.
     *
     * These two flags and `CMtmDemoUi::OpenL` have to change together, always. The whole
     * cost of this integration so far has been components disagreeing about what exists:
     * a registration naming a factory that was not there, capabilities that refused the load,
     * a menu item pointing at a component that leaves. */
    return aContext.iType == KUidMsvMessageEntry;
    }

TBool CMtmDemoUiData::CanCloseEntryL(const TMsvEntry& /*aContext*/,
                                     TInt& aReasonResourceId) const
    {
    aReasonResourceId = 0;
    return EFalse;
    }

TBool CMtmDemoUiData::CanCopyMoveToEntryL(const TMsvEntry& /*aContext*/,
                                          TInt& aReasonResourceId) const
    {
    aReasonResourceId = 0;
    return EFalse;
    }

TBool CMtmDemoUiData::CanCopyMoveFromEntryL(const TMsvEntry& /*aContext*/,
                                            TInt& aReasonResourceId) const
    {
    aReasonResourceId = 0;
    return EFalse;
    }

TBool CMtmDemoUiData::CanCancelL(const TMsvEntry& /*aContext*/,
                                 TInt& aReasonResourceId) const
    {
    aReasonResourceId = 0;
    /* Nothing of ours is ever in progress from the Messaging application's point of view:
     * the daemon does the work and the entry appears finished. */
    return EFalse;
    }

TInt CMtmDemoUiData::OperationSupportedL(TInt /*aOperationId*/,
                                         const TMsvEntry& /*aContext*/) const
    {
    /* 0 means supported; anything else is a resource id for the reason. This MTM registers no
     * operations of its own, so it is asked about none — and a non-zero id here would have to
     * point at a string this component does not ship. */
    return 0;
    }

TInt CMtmDemoUiData::QueryCapability(TUid aCapability, TInt& aResponse) const
    {
    /* The same answers the client component gives, and deliberately so: these are read by
     * different callers — SendAs and the Messaging application among them — and two
     * components of one MTM disagreeing about whether it can send is how a menu item appears
     * in one place and not another. */
    switch (aCapability.iUid)
        {
        case KUidMtmQueryMaxBodySizeValue:
            aResponse = KMaxTInt;
            return KErrNone;

        case KUidMtmQuerySupportedBodyValue:
            aResponse = KMtm16BitBody;
            return KErrNone;

        case KUidMtmQueryCanReceiveMsgValue:
            aResponse = ETrue;
            return KErrNone;

        case KUidMtmQueryCanSendMsgValue:
            /* ETrue, and the previous EFalse was a contradiction rather than a policy.
             *
             * `CanReplyToEntryL` above says a user may reply, and a reply is an outgoing
             * message. An MTM that offers replying and then answers "cannot send" is telling
             * two different stories to two different callers, and the reply item not appearing
             * in the Messaging application's menu is what that inconsistency looks like from
             * outside. Which of the two MCE actually reads is what the trace above measures. */
            aResponse = ETrue;
            return KErrNone;

        case KUidMtmQuerySendAsMessageSendSupportValue:
            /* Separately EFalse, and deliberately: this is the SendAs question — whether other
             * applications should offer this MTM in "Send via…". They should not. Nothing here
             * sends; the entry is left in the store for a daemon, and a Gallery item handed to
             * this MTM would go nowhere with no way for the user to find out. */
            aResponse = EFalse;
            return KErrNone;

        case KUidMtmQuerySupportAttachmentsValue:
        case KUidMtmQuerySupportSubjectValue:
            aResponse = EFalse;
            return KErrNone;

        case KUidMtmQueryOffLineAllowedValue:
            aResponse = ETrue;
            return KErrNone;

        default:
            return KErrNotSupported;
        }
    }

HBufC* CMtmDemoUiData::StatusTextL(const TMsvEntry& /*aContext*/) const
    {
    /* The per-entry status line the Messaging application shows for things in progress —
     * "Sending", "Waiting". Nothing of ours is ever in that state.
     *
     * NULL is not the answer: callers dereference this. An empty allocated descriptor is the
     * way to say "no status", and it is the caller's to free. */
    return HBufC::NewL(1);
    }
