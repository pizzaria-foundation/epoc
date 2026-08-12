/* The demo MTM's client component.
 *
 * See src/mtmclient.cpp for why this exists and why almost every method leaves with
 * KErrNotSupported.
 */

#ifndef MTMDEMO_H
#define MTMDEMO_H

#include <mtclbase.h>
#include <mtudcbas.h>
#include <mtmuibas.h>

class CRegisteredMtmDll;
class CMsvSession;

/* The MTM's identity. Must match data/mtmdemoreg.rss exactly: the registration names the
 * type, and every message carries it in TMsvEntry::iMtm. A mismatch means messages that
 * belong to nobody.
 *
 * In the 0xE development range, and with a technology type of its own rather than a real
 * one — sharing SMS's 0x10008A30 would put this type in the same bucket as the platform's
 * own for every framework decision keyed on technology. */
/* The UI resource both UI components open during construction. One file: neither has
 * resources of its own beyond needing to exist, and two would be two things to keep in step. */
_LIT(KMtmDemoResourceFile, "C:\\resource\\messaging\\mtmdemo.rsc");

#define KMtmDemoTypeUidValue        0xE0DD0B01
#define KMtmDemoTechnologyUidValue  0xE0DD0B02

class CMtmDemoClient : public CBaseMtm
    {
public:
    static CMtmDemoClient* NewL(CRegisteredMtmDll& aRegisteredDll, CMsvSession& aSession);
    ~CMtmDemoClient();

    /* CBaseMtm's eleven pure virtuals. The ones that do real work are the two store
     * methods and QueryCapability; the rest leave with KErrNotSupported, which is what the
     * base class prescribes for a message type that does not offer them. */
    void SaveMessageL();
    void LoadMessageL();
    TMsvPartList ValidateMessage(TMsvPartList aPartList);
    TMsvPartList Find(const TDesC& aTextToFind, TMsvPartList aPartList);
    CMsvOperation* ReplyL(TMsvId aDestination, TMsvPartList aPartlist,
                          TRequestStatus& aCompletionStatus);
    CMsvOperation* ForwardL(TMsvId aDestination, TMsvPartList aPartList,
                            TRequestStatus& aCompletionStatus);
    void AddAddresseeL(const TDesC& aRealAddress);
    void AddAddresseeL(const TDesC& aRealAddress, const TDesC& aAlias);
    void RemoveAddressee(TInt aIndex);
    void InvokeSyncFunctionL(TInt aFunctionId, const CMsvEntrySelection& aSelection,
                             TDes8& aParameter);
    CMsvOperation* InvokeAsyncFunctionL(TInt aFunctionId, const CMsvEntrySelection& aSelection,
                                        TDes8& aParameter, TRequestStatus& aCompletionStatus);
    TInt QueryCapability(TUid aCapability, TInt& aResponse);

protected:
    CMtmDemoClient(CRegisteredMtmDll& aRegisteredDll, CMsvSession& aSession);
    void ConstructL();
    void ContextEntrySwitched();
    };

/* The UI Data component: icons, and which menu items the native Messaging application
 * offers for our messages. See src/mtmuidata.cpp — in particular why nothing in it panics,
 * unlike the SDK's reference implementation, which runs under a UI where a panic is a
 * developer's problem rather than the user's Messaging application closing. */
class CMtmDemoUiData : public CBaseMtmUiData
    {
public:
    static CMtmDemoUiData* NewL(CRegisteredMtmDll& aRegisteredDll);
    ~CMtmDemoUiData();

    /* CBaseMtmUiData's eighteen pure virtuals. */
    const CBitmapArray& ContextIcon(const TMsvEntry& aContext, TInt aStateFlags) const;
    TBool CanCreateEntryL(const TMsvEntry& aParent, TMsvEntry& aNewEntry,
                          TInt& aReasonResourceId) const;
    TBool CanDeleteFromEntryL(const TMsvEntry& aContext, TInt& aReasonResourceId) const;
    TBool CanDeleteServiceL(const TMsvEntry& aService, TInt& aReasonResourceId) const;
    TBool CanReplyToEntryL(const TMsvEntry& aContext, TInt& aReasonResourceId) const;
    TBool CanForwardEntryL(const TMsvEntry& aContext, TInt& aReasonResourceId) const;
    TBool CanEditEntryL(const TMsvEntry& aContext, TInt& aReasonResourceId) const;
    TBool CanViewEntryL(const TMsvEntry& aContext, TInt& aReasonResourceId) const;
    TBool CanOpenEntryL(const TMsvEntry& aContext, TInt& aReasonResourceId) const;
    TBool CanCloseEntryL(const TMsvEntry& aContext, TInt& aReasonResourceId) const;
    TBool CanCopyMoveToEntryL(const TMsvEntry& aContext, TInt& aReasonResourceId) const;
    TBool CanCopyMoveFromEntryL(const TMsvEntry& aContext, TInt& aReasonResourceId) const;
    TBool CanCancelL(const TMsvEntry& aContext, TInt& aReasonResourceId) const;
    TInt OperationSupportedL(TInt aOperationId, const TMsvEntry& aContext) const;
    TInt QueryCapability(TUid aFunctionId, TInt& aResponse) const;
    HBufC* StatusTextL(const TMsvEntry& aContext) const;

protected:
    CMtmDemoUiData(CRegisteredMtmDll& aRegisteredDll);
    void PopulateArraysL();
    void GetResourceFileName(TFileName& aFileName) const;
    };

/* The UI component: what happens when the user taps one of our messages.
 *
 * A read-only viewer, drawn inside the Messaging application's own process with an Avkon
 * dialog — see src/mtmui.cpp for why there is no editor application and why there cannot
 * yet be one. */
class CMtmDemoUi : public CBaseMtmUi
    {
public:
    static CMtmDemoUi* NewL(CBaseMtm& aBaseMtm, CRegisteredMtmDll& aRegisteredDll);
    ~CMtmDemoUi();

    /* CBaseMtmUi's twelve pure virtuals. Open and View show the message; the rest leave. */
    CMsvOperation* OpenL(TRequestStatus& aStatus);
    CMsvOperation* CloseL(TRequestStatus& aStatus);
    CMsvOperation* EditL(TRequestStatus& aStatus);
    CMsvOperation* ViewL(TRequestStatus& aStatus);
    CMsvOperation* OpenL(TRequestStatus& aStatus, const CMsvEntrySelection& aSelection);
    CMsvOperation* CloseL(TRequestStatus& aStatus, const CMsvEntrySelection& aSelection);
    CMsvOperation* EditL(TRequestStatus& aStatus, const CMsvEntrySelection& aSelection);
    CMsvOperation* ViewL(TRequestStatus& aStatus, const CMsvEntrySelection& aSelection);
    CMsvOperation* CancelL(TRequestStatus& aStatus, const CMsvEntrySelection& aSelection);
    CMsvOperation* ReplyL(TMsvId aDestination, TMsvPartList aPartlist,
                          TRequestStatus& aCompletionStatus);
    CMsvOperation* ForwardL(TMsvId aDestination, TMsvPartList aPartList,
                            TRequestStatus& aCompletionStatus);

protected:
    CMtmDemoUi(CBaseMtm& aBaseMtm, CRegisteredMtmDll& aRegisteredDll);
    void ConstructL();
    void GetResourceFileName(TFileName& aFileName) const;

private:
    CMsvOperation* ShowMessageL(TRequestStatus& aStatus);
    HBufC* BodyTextLC();
    };

#endif /* MTMDEMO_H */
