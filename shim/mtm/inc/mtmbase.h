/* The four MTM components, everything already learned, ready to be subclassed.
 *
 * WHAT AN MTM IS AND WHERE IT RUNS
 *
 * A message type module is how a third party puts its messages in Nokia's own Messaging
 * application: its own icon in the inbox, its own viewer when the user opens one, and Reply
 * on the menu. It is four C++ components the platform loads *into its own processes* — the
 * Message Server for one, the Messaging application for the other three.
 *
 * That last sentence is the whole reason this file exists rather than each service copying
 * apps/mtmdemo. A fault here does not cost a service; it closes the user's inbox. Every rule
 * below was paid for on a handset, and a service that copied the code would be free to
 * un-learn any of them.
 *
 * WHAT IS FIXED AND WHAT IS A HOOK
 *
 * One rule, applied throughout: **if getting it wrong kills the Messaging application, it is
 * fixed.** So the reply mechanics, the icon clamping, the resource-offset arithmetic, the
 * never-panic policy and the ordering of the publishing ChangeL are all non-virtual. What a
 * subclass supplies is identity (which resource file, which bitmap file, which dialog) and
 * policy (which menu items to offer, how many icons, what the capability answers are).
 *
 * FOUR CONSTRAINTS ON EVERY LINE IN THE .cpp FILES
 *
 * 1. **No writable static data.** A Symbian 9.x DLL is refused by the loader if it has any,
 *    and this toolchain cannot ask for EPOCALLOWDLLDATA — `tools/e32dump.py --expect-dll` is
 *    the gate and it checks `dataSize == 0 && bssSize == 0`. So no file-scope mutables of any
 *    kind. `_LIT` is fine (it is const); a counter is not, which is why the trace ledger in
 *    CMtmBaseUiData is an instance member.
 *
 * 2. **Static constructors never run.** elf2e32 sets KImageNoCallEntryPoint unconditionally,
 *    so `_E32Dll` is never called. Anything needing initialisation is initialised in a
 *    function.
 *
 * 3. **Nothing may panic.** The SDK's own reference implementation opens almost every method
 *    with `__ASSERT_ALWAYS(aContext.iMtm == ..., Panic(...))`, which is reasonable under
 *    TechView where a panic is a developer's problem. Here it is a way to close somebody's
 *    inbox because a caller passed an entry we did not expect. Every method takes the
 *    defensive branch and returns something safe.
 *
 * 4. **The shim may never be linked in.** `shim_event.cpp` alone has five file-scope
 *    mutables. That is why these sources live in `shim/mtm/` and not `shim/src/`, which
 *    symbuild globs wholesale, and why `USE_MTM` refuses to coexist with `USE_SHIM`.
 *
 * NO IMPORT_C OR EXPORT_C IN THIS HEADER
 *
 * These sources are compiled *into* the service's DLL, not shipped as a DLL of their own, so
 * the Symbian export decorations do not apply — `IMPORT_C` would tell the compiler to expect
 * these symbols from somewhere else. And the DLL's export table must hold **exactly** the four
 * factory functions, because symbuild derives the registration resource's entry points from
 * the linker's `.def`; a fifth export shifts the ordinals and the framework then calls the
 * wrong component. Only MTM_EXPORT_COMPONENTS at the bottom carries EXPORT_C.
 *
 * WHAT A SERVICE WRITES
 *
 *     class CTgClient : public CMtmBaseClient { ... };   // often nothing but NewL
 *     class CTgUiData : public CMtmBaseUiData
 *         {
 *         void GetResourceFileName(TFileName& n) const { n = KTgResource; }
 *         void GetBitmapFileName(TFileName& n) const   { n = KTgBitmaps; }
 *         };
 *     class CTgUi : public CMtmBaseUi
 *         {
 *         void GetResourceFileName(TFileName& n) const { n = KTgResource; }
 *         TInt ReplyDialogResourceId() const           { return R_TG_REPLY_QUERY; }
 *         };
 *     MTM_EXPORT_COMPONENTS(CTgClient, CTgUi, CTgUiData)
 *
 * plus a `.rss` with a four-character NAME and a reply dialog, a `.rss.in` registration
 * (share `shim/mtm/data/mtmreg.rss.in`), and the icons. See apps/mtmdemo, which is exactly
 * this and nothing more.
 */

#ifndef MTMBASE_H
#define MTMBASE_H

#include <mtclbase.h>
#include <mtudcbas.h>
#include <mtmuibas.h>

class CRegisteredMtmDll;
class CMsvSession;

/* Append one line to a file, from inside a component running in somebody else's process.
 *
 * Compiled to nothing unless MTM_TRACE is defined (app.conf: `MTM_TRACE=1`), because this is
 * called once per row per redraw in the UI-data component and a file open per call makes the
 * Messaging application visibly slow.
 *
 * WHY IT IS THIS CRUDE
 *
 * Its own RFs session, opened and closed per line, no buffering, no cleanup stack, nothing
 * that can leave. When a fault means the process vanishes, the last line written is the whole
 * diagnosis — and an instrument that shares a failure mode with what it measures is not an
 * instrument. It cost five device trips to learn that; the reply path was diagnosed line by
 * line this way.
 *
 * The path is a parameter rather than a constant because the library cannot own a file-scope
 * name (constraint 1) and because one service's path is not every service's. */
void MtmBaseTrace(const TDesC& aPath, const TDesC& aStep);

/* ------------------------------------------------------------------ the client -- */

/* The Client MTM: the message store side, with no UI at all.
 *
 * This is the component the Message Server loads. Almost all of `CBaseMtm`'s eleven pure
 * virtuals are meaningless for a service whose messages arrive from a daemon, and the base
 * class documentation prescribes leaving with KErrNotSupported for exactly those — a Client
 * MTM is allowed to be this thin.
 *
 * The two that do real work are the store methods. The third is `ReplyL`, which creates the
 * reply entry: the framework's own two-step, where the client makes the entry and the UI
 * component edits it. */
class CMtmBaseClient : public CBaseMtm
    {
public:
    ~CMtmBaseClient();

    /* --- FIXED. CBaseMtm's pure virtuals, all of them. --- */
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

    /* HOOK, defaulted. The capability answers, which decide what a caller may attempt.
     *
     * A subclass that overrides this **must chain to the base** for anything it does not
     * claim, or it silently retracts every answer below.
     *
     * These must also match CMtmBaseUiData::QueryCapability word for word — the two are read
     * by different callers, and two components of one MTM disagreeing about whether it can
     * send is how a menu item appears in one place and not another. That is not hypothetical:
     * a `CanSendMsg` of EFalse alongside a `CanReplyToEntryL` of ETrue is what kept the reply
     * item off the Messaging application's menu entirely. */
    virtual TInt QueryCapability(TUid aCapability, TInt& aResponse);

protected:
    CMtmBaseClient(CRegisteredMtmDll& aRegisteredDll, CMsvSession& aSession);
    /* FIXED: establishes a context on the root, and nothing else. See the .cpp for what used
     * to be here and what it cost. */
    void ConstructL();
    /* HOOK, empty. Where a subclass clears per-entry state it caches. Leaving that out is how
     * one message's data ends up displayed under another's. */
    virtual void ContextEntrySwitched();
    /* HOOK, empty. Fill in a path to turn tracing on for this component. */
    virtual void TraceFileName(TFileName& aName) const;
    void Trace(const TDesC& aStep) const;
    };

/* ----------------------------------------------------------------- the UI data -- */

/* The UI Data MTM: icons, and which menu items the Messaging application offers.
 *
 * `CMtmUiDataRegistry` loads this into whatever application is drawing a message list. It is
 * the component that decides how a message *looks*: without it, a delivered message still
 * appears but wears the platform's "unknown type" envelope and cannot be opened.
 *
 * Every `Can*L` here is a menu item, and every one of them must agree with what the UI
 * component actually implements. A `Can*L` answering ETrue with nothing behind it is a menu
 * item that leaves with KErrNotSupported when the user taps it. */
class CMtmBaseUiData : public CBaseMtmUiData
    {
public:
    ~CMtmBaseUiData();

    /* --- FIXED. All eighteen, and none of them panics. --- */
    const CBitmapArray& ContextIcon(const TMsvEntry& aContext, TInt aStateFlags) const;
    TBool CanCreateEntryL(const TMsvEntry& aParent, TMsvEntry& aNewEntry, TInt& aReason) const;
    TBool CanDeleteFromEntryL(const TMsvEntry& aContext, TInt& aReason) const;
    TBool CanDeleteServiceL(const TMsvEntry& aService, TInt& aReason) const;
    TBool CanReplyToEntryL(const TMsvEntry& aContext, TInt& aReason) const;
    TBool CanForwardEntryL(const TMsvEntry& aContext, TInt& aReason) const;
    TBool CanEditEntryL(const TMsvEntry& aContext, TInt& aReason) const;
    TBool CanViewEntryL(const TMsvEntry& aContext, TInt& aReason) const;
    TBool CanOpenEntryL(const TMsvEntry& aContext, TInt& aReason) const;
    TBool CanCloseEntryL(const TMsvEntry& aContext, TInt& aReason) const;
    TBool CanCopyMoveToEntryL(const TMsvEntry& aContext, TInt& aReason) const;
    TBool CanCopyMoveFromEntryL(const TMsvEntry& aContext, TInt& aReason) const;
    TBool CanCancelL(const TMsvEntry& aContext, TInt& aReason) const;
    TInt OperationSupportedL(TInt aOperationId, const TMsvEntry& aContext) const;
    HBufC* StatusTextL(const TMsvEntry& aContext) const;
    /* HOOK, defaulted — and it must give the same answers as CMtmBaseClient's. */
    virtual TInt QueryCapability(TUid aCapability, TInt& aResponse) const;

protected:
    CMtmBaseUiData(CRegisteredMtmDll& aRegisteredDll);
    /* FIXED: loads the icon array from the bitmap file the hook names. */
    void PopulateArraysL();

    /* --- HOOKS, pure. No sane default exists for either. ---
     *
     * `CBaseMtmUiData::ConstructL` opens the resource file before it calls PopulateArraysL,
     * so a missing or wrong name is a Leave during construction *inside the Messaging
     * application*. A default would be some other service's file. The compiler should say so.
     */
    virtual void GetResourceFileName(TFileName& aFileName) const = 0;
    virtual void GetBitmapFileName(TFileName& aFileName) const = 0;

    /* --- HOOKS, defaulted to what apps/mtmdemo measured. --- */
    /* How many bitmaps are in the .mbm, and therefore how many CreateBitmapsL loads. Default
     * 2: a message icon and a service icon, in that order. */
    virtual TInt IconCount() const;
    /* Zoom states. Default 1, because the Messaging application on this handset resolves its
     * own icons through AknSkins and only consults ours as a fallback — shipping one size is
     * honest rather than padding the file with scaled copies nothing asks for. */
    virtual TInt ZoomStates() const;
    /* Which icon for this entry. Default: index 1 for a service entry, 0 for everything else.
     * The result is clamped by the caller, so an out-of-range answer is safe. */
    virtual TInt IconIndexFor(const TMsvEntry& aContext) const;

    /* The menu. Each defaults to what the base UI component implements, so the pair is
     * consistent out of the box: replying, opening and viewing are offered for message
     * entries; forwarding and editing are not, because CMtmBaseUi leaves on both. */
    virtual TBool CanReply(const TMsvEntry& aContext) const;
    virtual TBool CanOpen(const TMsvEntry& aContext) const;
    virtual TBool CanView(const TMsvEntry& aContext) const;
    virtual TBool CanForward(const TMsvEntry& aContext) const;
    virtual TBool CanEdit(const TMsvEntry& aContext) const;

    virtual void TraceFileName(TFileName& aName) const;

private:
    /* Record a question the Messaging application asked, the first time it asks it.
     *
     * One bit per question, because these methods run once per row per redraw and a file open
     * per call is a visibly slow inbox. `mutable` because every one of them is const, and an
     * *instance* member because a file-scope counter is writable static data, which the
     * loader refuses in a DLL. */
    void TraceOnce(TInt aBit, const TDesC& aWhat) const;
    mutable TUint32 iTraced;
    };

/* -------------------------------------------------------------------- the UI -- */

/* The UI MTM: what happens when the user taps one of our messages, and when they reply.
 *
 * WHY THERE IS NO EDITOR APPLICATION
 *
 * The obvious shape is "launch an editor and return an operation that watches it". Both routes
 * to it are closed. The SDK's reference `CTextMtmUi::LaunchEditorApplicationL` is a stub whose
 * own comment says *"In a real MTM, would launch the appropriate editor/viewer... Here we just
 * pretend"*. And S60's real mechanism is `muiu.dll` — `CMuiuMsgEditorService`,
 * `RMuiuMsgEditorService` — which ships as a binary with no header and no import library.
 *
 * What replaces it is better for this purpose anyway: a UI MTM is loaded *into* the
 * application showing the message list and is handed that application's `CCoeEnv`
 * (`mtmuibas.h:504`). So it draws, rather than launching. Avkon is already in that process
 * because it *is* that process.
 *
 * Four things here were measured on an E72 and none of them is deducible:
 *
 * - `CAknQueryDialog::RunLD()`, the resource-free path the header documents without
 *   qualification, **kills the process**. A query dialog needs a resource and `ExecuteLD`.
 * - The resource id is used **bare**. `CCoeEnv::AddResourceFileL` returned 0x421de000 and the
 *   ids rcomp generates already carry that base, because the file declares a `NAME`. Adding
 *   the offset asks for a resource that does not exist, and that is a dead application.
 * - `CMsvCompletedOperation` is **not completed on construction** despite the name: it is a
 *   `CActive` that signals in `RunL`. Waiting on it from this thread deadlocks the scheduler
 *   that has to run for that to happen, which from outside is the phone jumping to the home
 *   screen with no panic to show.
 * - `TMsvEntry::iDetails` is a `TPtrC` into the CMsvEntry's buffer, and switching context
 *   reloads that buffer. It must be copied before any switch.
 */
class CMtmBaseUi : public CBaseMtmUi
    {
public:
    ~CMtmBaseUi();

    /* --- FIXED. All twelve. --- */
    CMsvOperation* OpenL(TRequestStatus& aStatus);
    CMsvOperation* ViewL(TRequestStatus& aStatus);
    CMsvOperation* OpenL(TRequestStatus& aStatus, const CMsvEntrySelection& aSelection);
    CMsvOperation* ViewL(TRequestStatus& aStatus, const CMsvEntrySelection& aSelection);
    CMsvOperation* CloseL(TRequestStatus& aStatus);
    CMsvOperation* CloseL(TRequestStatus& aStatus, const CMsvEntrySelection& aSelection);
    CMsvOperation* EditL(TRequestStatus& aStatus);
    CMsvOperation* EditL(TRequestStatus& aStatus, const CMsvEntrySelection& aSelection);
    CMsvOperation* CancelL(TRequestStatus& aStatus, const CMsvEntrySelection& aSelection);
    CMsvOperation* ReplyL(TMsvId aDestination, TMsvPartList aPartlist,
                          TRequestStatus& aCompletionStatus);
    CMsvOperation* ForwardL(TMsvId aDestination, TMsvPartList aPartList,
                            TRequestStatus& aCompletionStatus);

protected:
    CMtmBaseUi(CBaseMtm& aBaseMtm, CRegisteredMtmDll& aRegisteredDll);
    /* FIXED: the base class's ConstructL, then a *second* load of the same resource file for
     * an offset this component can use — CBaseMtmUi keeps its own offset private
     * (mtmuibas.h:511). Guarded on iCoeEnv, and released in the destructor. */
    void ConstructL();

    /* --- HOOKS, pure. --- */
    virtual void GetResourceFileName(TFileName& aFileName) const = 0;
    /* The `R_..._REPLY_QUERY` id from the subclass's own .rsg, used **bare**.
     *
     * Pure because a wrong id here is a dead Messaging application, and a default would be
     * some other service's id. The subclass's `.rss` **must** declare a four-character `NAME`;
     * a file without one gets ids with no base and would need the offset added, and this
     * library has no way to tell which it was handed. `tools/symnew` scaffolds one that does. */
    virtual TInt ReplyDialogResourceId() const = 0;

    /* --- HOOKS, defaulted. --- */
    /* How much of a body the viewer shows. Default 4096.
     *
     * A cap rather than the whole thing: the dialog holds the text in one descriptor, a chat
     * message has no natural size limit, and running out of memory here kills the user's
     * Messaging application. Truncation is visible and survivable; the other is neither. */
    virtual TInt MaxViewChars() const;
    /* How much the user may type in a reply. Default 256, and it **must match** the
     * `maxlength` in the reply dialog resource. */
    virtual TInt MaxReplyChars() const;
    /* Shown when an entry has no body text. Default "(no message text)". */
    virtual void EmptyBodyText(TDes& aText) const;
    virtual void TraceFileName(TFileName& aName) const;

    void Trace(const TDesC& aStep) const;

private:
    CMsvOperation* ShowMessageL(TRequestStatus& aStatus);
    HBufC* BodyTextLC();
    TInt iResourceOffset;
    };

/* --------------------------------------------------------------- the exports -- */

/* The four factory functions, in one line. Use once, at file scope, in exactly one .cpp.
 *
 * A macro rather than four functions each service writes, for two reasons that are not
 * stylistic. All four slots must exist: `CObserverRegistry::HandleSessionEventL` indexes the
 * declared component array by *fixed slot* (0 server, 1 client, 2 UI, 3 UI data) with no
 * bounds check, so a registration declaring three components panics the Message Server on a
 * later scheduler turn than the install — which took five device trips and reading the OS
 * source to find. And the server placeholder must carry the framework's exact signature, so
 * that a caller which does invoke it enters a function with the stack shape it expects rather
 * than one reading its arguments from somewhere else.
 *
 * The ordinals these land on are **not** declaration order: elf2e32 sorts exports by name.
 * `tools/symbuild` generates the registration resource from the linker's own `.def`, so the
 * two cannot disagree. */
#define MTM_EXPORT_COMPONENTS(ClientClass, UiClass, UiDataClass)                            \
    extern "C" EXPORT_C CBaseMtm* NewMtmClientL(CRegisteredMtmDll& aDll, CMsvSession& aSes) \
        {                                                                                   \
        return ClientClass::NewL(aDll, aSes);                                               \
        }                                                                                   \
    extern "C" EXPORT_C TAny* NewMtmServerL(CRegisteredMtmDll&, TAny*)                      \
        {                                                                                   \
        /* The slot exists; the component does not. A Leave is something the caller          \
         * survives, unlike an absent array slot. */                                        \
        User::Leave(KErrNotSupported);                                                      \
        return NULL;                                                                        \
        }                                                                                   \
    extern "C" EXPORT_C CBaseMtmUi* NewMtmUiL(CBaseMtm& aMtm, CRegisteredMtmDll& aDll)      \
        {                                                                                   \
        return UiClass::NewL(aMtm, aDll);                                                   \
        }                                                                                   \
    extern "C" EXPORT_C CBaseMtmUiData* NewMtmUiDataL(CRegisteredMtmDll& aDll)              \
        {                                                                                   \
        return UiDataClass::NewL(aDll);                                                     \
        }

#endif /* MTMBASE_H */
