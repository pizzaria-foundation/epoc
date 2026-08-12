/* The UI Data MTM: icons, and which menu items the Messaging application offers.
 *
 * See shim/mtm/inc/mtmbase.h for the four constraints. The third one — nothing may panic —
 * matters more here than anywhere else in the library, because `CMtmUiDataRegistry` loads this
 * component into whatever application is drawing a message list. That is the user's Messaging
 * application, and a fault here closes it.
 *
 * So this file diverges from the SDK's reference implementation in one specific, deliberate
 * way. `txti` opens almost every method with
 *
 *     __ASSERT_ALWAYS(aContext.iMtm==KUidMsgTypeText, Panic(ETxtiMtmUdWrongMtm));
 *
 * which is reasonable for sample code running under TechView, where a panic is a developer's
 * problem. Here the same line is a way to kill somebody's inbox because a caller passed an
 * entry we did not expect. Nothing in this file asserts or panics; every method takes the
 * defensive branch and returns something safe, because the cost of being wrong is not
 * symmetric.
 */

#include <e32base.h>
#include <msvstd.h>
#include <msvuids.h>
#include <msvids.h>
#include <mtmdef.h>
#include <mtmuids.h>
#include <mtmdef.hrh>
#include <mtud.hrh>

#include "mtmbase.h"

/* Bits in the trace ledger, one per question. */
enum TMtmBaseTraceBit
    {
    EBitCanReply = 0,
    EBitCanOpen,
    EBitCanView,
    EBitCanForward,
    EBitCanEdit,
    EBitCanCreate,
    EBitCanDelete,
    EBitContextIcon
    };

CMtmBaseUiData::CMtmBaseUiData(CRegisteredMtmDll& aRegisteredDll)
    : CBaseMtmUiData(aRegisteredDll), iTraced(0)
    {
    }

CMtmBaseUiData::~CMtmBaseUiData()
    {
    /* iIconArrays and iMtmSpecificFunctions belong to the base class, which frees them. */
    }

void CMtmBaseUiData::TraceFileName(TFileName& aName) const
    {
    aName.Zero();
    }

void CMtmBaseUiData::TraceOnce(TInt aBit, const TDesC& aWhat) const
    {
    const TUint32 mask = 1u << aBit;
    if (iTraced & mask)
        return;
    iTraced |= mask;
    TFileName path;
    TraceFileName(path);
    MtmBaseTrace(path, aWhat);
    }

void CMtmBaseUiData::PopulateArraysL()
    {
    /* Icons, and nothing else.
     *
     * They come first and they are not optional, because ContextIcon returns a *reference*
     * into this array and has no way to fail. An empty array there is not a blank icon, it is
     * an out-of-bounds read inside the Messaging application — so if this leaves, the
     * component fails to construct, which is the outcome to want over one that constructs and
     * faults on the first redraw.
     *
     * The bitmaps are contiguous in the .mbm and CreateBitmapsL walks it from first to last,
     * so the order in MTM_ICONS in app.conf *is* the order IconIndexFor returns.
     *
     * ReadFunctionsFromResourceFileL is deliberately not called: this library registers no
     * MTM-specific menu commands, and an empty MTUD_FUNCTION_ARRAY would be a resource to keep
     * in step for no gain. A service that wants its own commands calls it in an override. */
    TFileName bitmaps;
    GetBitmapFileName(bitmaps);
    CreateBitmapsL(ZoomStates(), bitmaps, 0, IconCount() - 1);
    }

TInt CMtmBaseUiData::IconCount() const
    {
    return 2;   /* message, service — in that order in the .mbm */
    }

TInt CMtmBaseUiData::ZoomStates() const
    {
    return 1;
    }

TInt CMtmBaseUiData::IconIndexFor(const TMsvEntry& aContext) const
    {
    return aContext.iType == KUidMsvServiceEntry ? 1 : 0;
    }

const CBaseMtmUiData::CBitmapArray& CMtmBaseUiData::ContextIcon(const TMsvEntry& aContext,
                                                                TInt /*aStateFlags*/) const
    {
    TraceOnce(EBitContextIcon, _L("uidata: ContextIcon asked"));

    /* Chosen by the hook, and then clamped.
     *
     * The reference asserts the entry belongs to this MTM and panics if not; here a wrong
     * entry gets icon 0, because the difference between a slightly wrong picture and a dead
     * Messaging application is the whole reason this file exists.
     *
     * The clamp is not paranoia either: iIconArrays holds whatever PopulateArraysL managed to
     * load, and indexing past the end is an out-of-bounds panic — the same one that killed a
     * probe eight times through a different array. It also makes IconIndexFor safe to override
     * carelessly. */
    TInt index = IconIndexFor(aContext);
    if (index < 0)
        index = 0;
    const TInt count = iIconArrays ? iIconArrays->Count() : 0;
    if (index >= count)
        index = 0;

    return *iIconArrays->At(index);
    }

/* --------------------------------------------------------------- what is offered --
 * Each of these decides whether the Messaging application offers a menu item. The return value
 * is whether the operation is allowed; aReason is a resource id for the text explaining why
 * not, and 0 means "no explanation available" — honest here, since this library ships no
 * reason strings.
 *
 * Every default matches what CMtmBaseUi actually implements, so the pair is consistent out of
 * the box. A subclass that opens one of these up without a component behind it puts a menu
 * item on screen that leaves with KErrNotSupported when tapped.
 */

TBool CMtmBaseUiData::CanCreateEntryL(const TMsvEntry& aParent, TMsvEntry& aNewEntry,
                                      TInt& aReason) const
    {
    aReason = 0;
    TraceOnce(EBitCanCreate, _L("uidata: CanCreateEntryL asked"));
    /* A service may be created, and only under the root — which is what every UI Data MTM in
     * the reference checks and what the framework requires of a service entry. Messages are
     * created by the daemon through the client API, not through the Messaging application. */
    if (aNewEntry.iType == KUidMsvServiceEntry)
        return aParent.Id() == KMsvRootIndexEntryId;
    return EFalse;
    }

TBool CMtmBaseUiData::CanDeleteFromEntryL(const TMsvEntry& /*aContext*/, TInt& aReason) const
    {
    aReason = 0;
    TraceOnce(EBitCanDelete, _L("uidata: CanDeleteFromEntryL asked"));
    /* Deleting must work. It is how a user gets rid of our messages at all, and refusing it
     * would leave them stuck in the inbox with no way out short of uninstalling. */
    return ETrue;
    }

TBool CMtmBaseUiData::CanDeleteServiceL(const TMsvEntry& /*aService*/, TInt& aReason) const
    {
    aReason = 0;
    return ETrue;
    }

TBool CMtmBaseUiData::CanReply(const TMsvEntry& aContext) const
    {
    /* Message entries only: a service entry has nobody to reply to. */
    return aContext.iType == KUidMsvMessageEntry;
    }

TBool CMtmBaseUiData::CanOpen(const TMsvEntry& aContext) const
    {
    return aContext.iType == KUidMsvMessageEntry;
    }

TBool CMtmBaseUiData::CanView(const TMsvEntry& aContext) const
    {
    return aContext.iType == KUidMsvMessageEntry;
    }

TBool CMtmBaseUiData::CanForward(const TMsvEntry& /*aContext*/) const
    {
    /* CMtmBaseUi::ForwardL leaves. Offering it would be a menu item that fails when tapped. */
    return EFalse;
    }

TBool CMtmBaseUiData::CanEdit(const TMsvEntry& /*aContext*/) const
    {
    /* Editing means an editor application, and there is none — see mtmbase.h. */
    return EFalse;
    }

TBool CMtmBaseUiData::CanReplyToEntryL(const TMsvEntry& aContext, TInt& aReason) const
    {
    aReason = 0;
    TraceOnce(EBitCanReply, _L("uidata: CanReplyToEntryL asked"));
    return CanReply(aContext);
    }

TBool CMtmBaseUiData::CanForwardEntryL(const TMsvEntry& aContext, TInt& aReason) const
    {
    aReason = 0;
    TraceOnce(EBitCanForward, _L("uidata: CanForwardEntryL asked"));
    return CanForward(aContext);
    }

TBool CMtmBaseUiData::CanEditEntryL(const TMsvEntry& aContext, TInt& aReason) const
    {
    aReason = 0;
    TraceOnce(EBitCanEdit, _L("uidata: CanEditEntryL asked"));
    return CanEdit(aContext);
    }

TBool CMtmBaseUiData::CanViewEntryL(const TMsvEntry& aContext, TInt& aReason) const
    {
    aReason = 0;
    TraceOnce(EBitCanView, _L("uidata: CanViewEntryL asked"));
    return CanView(aContext);
    }

TBool CMtmBaseUiData::CanOpenEntryL(const TMsvEntry& aContext, TInt& aReason) const
    {
    aReason = 0;
    TraceOnce(EBitCanOpen, _L("uidata: CanOpenEntryL asked"));
    return CanOpen(aContext);
    }

TBool CMtmBaseUiData::CanCloseEntryL(const TMsvEntry& /*aContext*/, TInt& aReason) const
    {
    aReason = 0;
    /* Nothing to close: the viewer is modal and gone by the time OpenL returns. */
    return EFalse;
    }

TBool CMtmBaseUiData::CanCopyMoveToEntryL(const TMsvEntry& /*aContext*/, TInt& aReason) const
    {
    aReason = 0;
    return EFalse;
    }

TBool CMtmBaseUiData::CanCopyMoveFromEntryL(const TMsvEntry& /*aContext*/, TInt& aReason) const
    {
    aReason = 0;
    return EFalse;
    }

TBool CMtmBaseUiData::CanCancelL(const TMsvEntry& /*aContext*/, TInt& aReason) const
    {
    aReason = 0;
    /* Nothing of ours is ever in progress from the Messaging application's point of view: the
     * daemon does the work and the entry appears finished. */
    return EFalse;
    }

TInt CMtmBaseUiData::OperationSupportedL(TInt /*aOperationId*/,
                                         const TMsvEntry& /*aContext*/) const
    {
    /* 0 means supported; anything else is a resource id for the reason. This library registers
     * no operations of its own, so it is asked about none — and a non-zero id here would have
     * to point at a string it does not ship. */
    return 0;
    }

HBufC* CMtmBaseUiData::StatusTextL(const TMsvEntry& /*aContext*/) const
    {
    /* The per-entry status line the Messaging application shows for things in progress —
     * "Sending", "Waiting". Nothing of ours is ever in that state.
     *
     * NULL is not the answer: callers dereference this. An empty allocated descriptor is the
     * way to say "no status", and it is the caller's to free. */
    return HBufC::NewL(1);
    }

TInt CMtmBaseUiData::QueryCapability(TUid aCapability, TInt& aResponse) const
    {
    /* The same answers CMtmBaseClient gives, word for word and deliberately so.
     *
     * These are read by different callers — SendAs and the Messaging application among them —
     * and two components of one MTM disagreeing about whether it can send is how a menu item
     * appears in one place and not another. A `CanSendMsg` of EFalse here alongside a
     * `CanReplyToEntryL` of ETrue above is what kept the reply item off the menu entirely.
     *
     * Duplicated rather than delegated because this component has no pointer to the client
     * one: they are separate objects the framework builds independently. Keeping the two lists
     * identical is a maintenance rule, and the comment is the enforcement. */
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
            aResponse = ETrue;
            return KErrNone;

        case KUidMtmQuerySendAsMessageSendSupportValue:
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
