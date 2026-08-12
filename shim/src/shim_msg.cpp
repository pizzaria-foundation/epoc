/* The Message Server: reconnaissance, writing, reading, and store events.
 *
 * WHY THIS FILE MUST BE ALONE IN ITS BINARY
 *
 * It imports msgs.dso. The E72's msgs.dll is a 2009 Nokia build and this SDK's import
 * library is not necessarily the same one; an ordinal we call that the handset does not
 * export makes the loader refuse the whole image — no panic, no log, and no report file at
 * all. That is not a hypothetical: it is exactly what six calls into CCommsDatabase did to
 * an earlier build (docs/device-notes.md, "An import that does not resolve makes the app
 * vanish"), and the conclusion drawn there was that a facility which might not resolve
 * belongs in its own binary, where failing to load costs a probe rather than the report.
 *
 * So: compiled only into apps/devdump's messaging probe, never into the launcher, never
 * into anything that has other questions to answer.
 *
 * HOW IT GREW, WHICH IS WHY IT IS IN THREE PARTS
 *
 * It started as reconnaissance: open a session, list the registered MTMs, count folder
 * entries. Enough to learn what the platform's messaging stack contains before deciding
 * whether to build on it, and nothing that could damage the user's messages.
 *
 * Then registering an MTM and writing a message, because a message in the user's inbox turned
 * out to need no MTM at all and that was worth having.
 *
 * And now reading, because traffic goes both ways. A UI MTM loaded into Nokia's Messaging
 * application writes the user's reply into the store, and something outside that process has
 * to notice and carry it out. Reading it needs the entry, its body, and — the part that is not
 * a function call — a way to be *told*.
 *
 * THE OBSERVER IS NO LONGER A STUB, AND THAT IS THE RISKIEST CODE HERE
 *
 * `CMsvSession::OpenSyncL` demands an `MMsvSessionObserver` and calls back into it on every
 * server event, from its own active object. For a one-shot probe that was noise to swallow.
 * It is now the wake-up path, and it runs under two hard rules — no allocation, no Leave —
 * plus one platform quirk that can kill the process if got wrong. See TShimMsvObserver.
 *
 * Delivery is opt-in through `shim_msv_observe`, so a probe that has no interest in events
 * behaves exactly as it did before.
 */

#include "symbian_shim.h"
#include "shim_priv.h"

#ifdef SHIM_USE_MSG

#include <e32std.h>
#include <e32base.h>
#include <msvapi.h>
#include <msvstd.h>
#include <msvids.h>
#include <msvuids.h>
#include <msvreg.h>
#include <msvstore.h>
#include <mtclreg.h>
#include <mtclbase.h>
#include <txtrich.h>

/* The entry type UIDs in symbian_shim.h, checked against the platform's own.
 *
 * These values are transcribed by hand into crates/symbian-sys, and the Rust side compares
 * an entry's type against them. A wrong one is invisible: `is_message()` simply answers false
 * for everything, and a service never recognises its own messages. So the transcription is
 * pinned here, where msvstd.hrh is in scope, and a mismatch is a build failure on the host
 * rather than a service that quietly does nothing on a handset. */
#define SHIM_STATIC_ASSERT(cond, name) typedef char name[(cond) ? 1 : -1]
SHIM_STATIC_ASSERT(SHIM_MSV_TYPE_ROOT == KUidMsvRootEntryValue, shim_msv_type_root);
SHIM_STATIC_ASSERT(SHIM_MSV_TYPE_SERVICE == KUidMsvServiceEntryValue, shim_msv_type_service);
SHIM_STATIC_ASSERT(SHIM_MSV_TYPE_FOLDER == KUidMsvFolderEntryValue, shim_msv_type_folder);
SHIM_STATIC_ASSERT(SHIM_MSV_TYPE_MESSAGE == KUidMsvMessageEntryValue, shim_msv_type_message);
SHIM_STATIC_ASSERT(SHIM_MSV_TYPE_ATTACHMENT == KUidMsvAttachmentEntryValue, shim_msv_type_attach);
/* And the folder ids, which have been transcribed since the read-only days and never checked. */
SHIM_STATIC_ASSERT(SHIM_MSV_ROOT == KMsvRootIndexEntryIdValue, shim_msv_folder_root);
SHIM_STATIC_ASSERT(SHIM_MSV_INBOX == KMsvGlobalInBoxIndexEntryIdValue, shim_msv_folder_inbox);
SHIM_STATIC_ASSERT(SHIM_MSV_OUTBOX == KMsvGlobalOutBoxIndexEntryIdValue, shim_msv_folder_outbox);
SHIM_STATIC_ASSERT(SHIM_MSV_DRAFTS == KMsvDraftEntryIdValue, shim_msv_folder_drafts);
SHIM_STATIC_ASSERT(SHIM_MSV_SENT == KMsvSentEntryIdValue, shim_msv_folder_sent);

namespace {

/* One session at a time. The probe is single-threaded and asks its questions in sequence,
 * so a handle table would be three fields of ceremony around a single slot. The handle is
 * still opaque and still validated, which is what rule 3 in symbian_shim.h is actually
 * for — a stale handle must become an error, not a jump through a dead pointer. */
const int32_t KMsvHandle = 1;

/* At most this many entry events per platform notification.
 *
 * One event per entry rather than one per notification, because a service must not miss the
 * single reply the Messaging application just wrote. Bounded, because a bulk delete of a
 * hundred entries would otherwise flush the whole 64-slot ring and take with it the events
 * that mattered — and the ring drops the newest when it fills, so the overflow would be the
 * events after the flood rather than the flood itself.
 *
 * The bound is only safe because an event is a hint: `d` carries the real selection size, and
 * a reader that sees more than fits re-reads the store. That is the same recovery path a
 * restarted process takes, so it is code that has to work anyway. */
const TInt KMaxIdsPerEvent = 8;

/* Required by OpenSyncL, and no longer inert.
 *
 * WHAT THIS FUNCTION MAY NOT DO
 *
 * It is called from CMsvSession's own active object, so the two rules of shim_event.cpp
 * apply in full: **it may not allocate and it may not leave.** `ShimPushEvent` is written for
 * exactly this and does neither. Nothing else in here touches the heap.
 *
 * THE ONE WAY THIS CAN KILL THE PROCESS
 *
 * `aArg1` is a `CMsvEntrySelection*` for the four entry events and is something else entirely
 * for the rest — for EMsvServerFailedToStart it is a pointer to an error code, for
 * EMsvMediaChanged a TDriveNumber *value*. So the switch is exhaustive by construction: only
 * the four cases that document a selection ever read one, and the default touches neither
 * argument.
 *
 * And `aArg2` is never dereferenced, in any case. msvapi.h says the entry events pass the
 * parent id *as* the argument while the MTM-registry events pass a *pointer* to a UID, and
 * reading past that distinction is what killed this process three runs in a row. See the note
 * at the cast.
 *
 * DELIVERY IS OPT-IN
 *
 * `iDeliver` is a member of this object rather than a file-scope flag. That matters for
 * nothing here — gObserver is already a file-scope object and this file is only ever linked
 * into an EXE — but it is the habit the MTM DLL cannot break, and having two habits is how
 * the wrong one ends up in the wrong binary. */
class TShimMsvObserver : public MMsvSessionObserver
    {
public:
    TShimMsvObserver() : iDeliver(EFalse) {}

    void HandleSessionEventL(TMsvSessionEvent aEvent, TAny* aArg1, TAny* aArg2, TAny*)
        {
        if (!iDeliver)
            return;

        TInt kind = 0;
        TBool hasSelection = EFalse;
        switch (aEvent)
            {
            case EMsvEntriesCreated:  kind = SHIM_MSV_EV_CREATED; hasSelection = ETrue; break;
            case EMsvEntriesChanged:  kind = SHIM_MSV_EV_CHANGED; hasSelection = ETrue; break;
            case EMsvEntriesDeleted:  kind = SHIM_MSV_EV_DELETED; hasSelection = ETrue; break;
            case EMsvEntriesMoved:    kind = SHIM_MSV_EV_MOVED;   hasSelection = ETrue; break;
            case EMsvMtmGroupInstalled:   kind = SHIM_MSV_EV_MTM_INSTALLED; break;
            case EMsvMtmGroupDeInstalled: kind = SHIM_MSV_EV_MTM_REMOVED;  break;
            case EMsvServerReady:         kind = SHIM_MSV_EV_SERVER_READY; break;
            case EMsvServerTerminated:
            case EMsvCloseSession:        kind = SHIM_MSV_EV_SERVER_GONE;  break;
            default:
                /* Deliberately silent, and deliberately without touching either argument:
                 * an event code this build does not know is not a reason to guess at the
                 * shape of its parameters. */
                return;
            }

        ShimEvent ev;
        ev.kind = SHIM_EV_MSV;
        ev.handle = KMsvHandle;
        ev.status = SHIM_OK;
        ev.a = kind;
        ev.b = 0;
        ev.c = 0;
        ev.d = 0;
        ev.native = 0;

        if (!hasSelection)
            {
            ShimPushEvent(ev);
            return;
            }

        const CMsvEntrySelection* sel = static_cast<const CMsvEntrySelection*>(aArg1);

        /* aArg2 is CAST, never dereferenced. This line killed the process three runs in a row.
         *
         * msvapi.h is precise, and the precision is easy to read past. For the entry events it
         * says *"aArg2 **is** the TMsvId of the parent entry"*; for EMsvMtmGroupInstalled it
         * says *"aArg2 **points to** a TUid"*. So for these four the id arrives as the pointer
         * value itself, and the first version's `*static_cast<const TMsvId*>(aArg2)` read
         * memory at address 0x1004 — a wild read inside whichever process opened the session,
         * every single time our own write came back.
         *
         * A cast touches no memory, so it cannot fault whichever reading is right. If the
         * platform ever did pass a pointer, this yields a nonsense id and nothing breaks: an
         * event is a hint, and every reader re-reads the entry from the store, which is where
         * the authoritative parent comes from. That is the property that makes this line safe
         * rather than merely correct. */
        const TMsvId parent = (TMsvId) (TInt) aArg2;
        const TInt count = sel ? sel->Count() : 0;
        ev.c = (int32_t) parent;
        ev.d = (int32_t) count;

        TInt n = count;
        if (n > KMaxIdsPerEvent)
            n = KMaxIdsPerEvent;
        for (TInt i = 0; i < n; i++)
            {
            ev.b = (int32_t) sel->At(i);
            ShimPushEvent(ev);
            }
        /* A notification with an empty selection still gets one event, because "something
         * changed under this parent" is the useful part and a reader that rescans needs to
         * be told at all. */
        if (!n)
            ShimPushEvent(ev);
        }

    TBool iDeliver;
    };

TShimMsvObserver gObserver;
CMsvSession* gSession = NULL;
CClientMtmRegistry* gRegistry = NULL;

TBool Valid(int32_t aHandle)
    {
    return aHandle == KMsvHandle && gSession != NULL;
    }

/* Symbian counts microseconds from year 0; Unix from 1970. 62168256000 seconds is the gap,
 * and getting it wrong puts a message in the year 1 — which sorts to the bottom of the
 * folder and looks like it never arrived.
 *
 * One pair of functions rather than the constant written out at each use, because the read
 * side and the write side disagreeing about what a timestamp means is a bug nothing would
 * catch: every message would round-trip through the shim consistently wrong. */
const TInt64 KUnixEpochInSymbianSeconds = MAKE_TINT64(0, 62168256000);

TTime ToSymbianTime(int64_t aUnix)
    {
    return TTime((aUnix + KUnixEpochInSymbianSeconds) * 1000000);
    }

int64_t ToUnixTime(const TTime& aTime)
    {
    /* Through seconds rather than microseconds, because the intermediate
     * `aTime.Int64() - KUnixEpochInSymbianSeconds * 1000000` is a year-0 microsecond count
     * and there is no reason to carry a number that large when the caller wants seconds. */
    return (int64_t) (aTime.Int64() / 1000000 - KUnixEpochInSymbianSeconds);
    }

void OpenL()
    {
    gSession = CMsvSession::OpenSyncL(gObserver);
    /* The registry is what knows which MTMs the handset has. Built here rather than
     * lazily, so that "the session opened but the registry did not" is reported by the
     * open call rather than surfacing later as an empty MTM list — an empty list and a
     * failed registry look identical from Rust otherwise. */
    gRegistry = CClientMtmRegistry::NewL(*gSession);
    }

void MtmInfoL(TInt aIndex, ShimMtmInfo* aOut)
    {
    const TUid type = gRegistry->MtmTypeUid(aIndex);
    aOut->type_uid = (uint32_t) type.iUid;
    aOut->technology_uid = (uint32_t) gRegistry->TechnologyTypeUid(type).iUid;

    const CMtmDllInfo& info = gRegistry->RegisteredMtmDllInfo(type);
    const TPtrC name = info.HumanReadableName();
    const TInt cap = (TInt) (sizeof(aOut->name) / sizeof(aOut->name[0]));
    TInt n = name.Length();
    if (n > cap)
        n = cap;
    for (TInt i = 0; i < n; i++)
        aOut->name[i] = (uint16_t) name[i];
    aOut->name_len = (int32_t) n;
    }

/* A service entry: the "account" the native Messaging application lists.
 *
 * A child of the root, always. `CanCreateEntryL` in every UI Data MTM in the reference
 * implementation checks exactly that — `aParent.Id() == KMsvRootIndexEntryIdValue` — so a
 * service created anywhere else is a service the framework will refuse to work with. */
void CreateServiceL(TUid aMtm, const TDesC16& aName, TMsvId& aCreated)
    {
    TMsvSelectionOrdering ordering;
    CMsvEntry* root = CMsvEntry::NewL(*gSession, KMsvRootIndexEntryId, ordering);
    CleanupStack::PushL(root);

    TMsvEntry entry;
    entry.iType = KUidMsvServiceEntry;
    entry.iMtm = aMtm;
    /* A service is its own service. The framework uses iServiceId to walk from a message to
     * the account that owns it, and a service pointing anywhere else breaks that walk. */
    entry.iServiceId = KMsvNullIndexEntryId;
    entry.iDetails.Set(aName);
    entry.iDate.UniversalTime();
    entry.SetVisible(ETrue);
    entry.SetComplete(ETrue);

    root->CreateL(entry);
    /* CreateL writes the assigned id back into the TMsvEntry it was given. */
    aCreated = entry.Id();
    CleanupStack::PopAndDestroy(root);
    }

/* A message, body and all.
 *
 * The ordering is the part worth reading. The entry is created *invisible and incomplete*,
 * then the body is written to its store and committed, and only then is it made visible and
 * complete in a second ChangeL. A reader that caught the entry between those two steps —
 * and the native Messaging application is exactly such a reader, watching session events —
 * would otherwise see a message with no body in it. */
void CreateMessageL(const ShimNewMessage& aMsg, TMsvId& aCreated)
    {
    TMsvSelectionOrdering ordering;
    CMsvEntry* parent = CMsvEntry::NewL(*gSession, (TMsvId) aMsg.parent_id, ordering);
    CleanupStack::PushL(parent);

    TPtrC16 details(reinterpret_cast<const TUint16*>(aMsg.details),
                    aMsg.details_len > 0 ? aMsg.details_len : 0);
    TPtrC16 description(reinterpret_cast<const TUint16*>(aMsg.description),
                        aMsg.description_len > 0 ? aMsg.description_len : 0);

    TMsvEntry entry;
    entry.iType = KUidMsvMessageEntry;
    entry.iMtm = TUid::Uid((TInt32) aMsg.mtm_uid);
    entry.iServiceId = (TMsvId) aMsg.service_id;
    if (aMsg.details_len > 0)
        entry.iDetails.Set(details);
    if (aMsg.description_len > 0)
        entry.iDescription.Set(description);
    if (aMsg.unix_time > 0)
        entry.iDate = ToSymbianTime(aMsg.unix_time);
    else
        entry.iDate.UniversalTime();
    entry.iSize = aMsg.size > 0 ? aMsg.size : (aMsg.body_len > 0 ? aMsg.body_len * 2 : 0);
    /* Deliberately not yet complete and not yet visible — see the comment above. */
    entry.SetComplete(EFalse);
    entry.SetVisible(EFalse);

    parent->CreateL(entry);
    aCreated = entry.Id();

    if (aMsg.body_len > 0)
        {
        CMsvEntry* self = CMsvEntry::NewL(*gSession, aCreated, ordering);
        CleanupStack::PushL(self);
        CMsvStore* store = self->EditStoreL();
        CleanupStack::PushL(store);

        /* CRichText needs both layers and owns neither, so all three are on the cleanup
         * stack and torn down together. */
        CParaFormatLayer* para = CParaFormatLayer::NewL();
        CleanupStack::PushL(para);
        CCharFormatLayer* chars = CCharFormatLayer::NewL();
        CleanupStack::PushL(chars);
        CRichText* body = CRichText::NewL(para, chars);
        CleanupStack::PushL(body);

        TPtrC16 text(reinterpret_cast<const TUint16*>(aMsg.body), aMsg.body_len);
        body->InsertL(0, text);
        store->StoreBodyTextL(*body);
        store->CommitL();

        CleanupStack::PopAndDestroy(5, self);   // body, chars, para, store, self
        }

    /* Now make it real. Re-read rather than reusing the local TMsvEntry: the server may have
     * set fields of its own during CreateL, and writing back a stale copy would undo them. */
    CMsvEntry* mine = CMsvEntry::NewL(*gSession, aCreated, ordering);
    CleanupStack::PushL(mine);
    TMsvEntry done = mine->Entry();
    done.SetComplete(ETrue);
    done.SetVisible(ETrue);
    done.SetNew((aMsg.flags & SHIM_MSV_NEW) != 0);
    done.SetUnread((aMsg.flags & SHIM_MSV_UNREAD) != 0);
    mine->ChangeL(done);
    CleanupStack::PopAndDestroy(mine);

    CleanupStack::PopAndDestroy(parent);
    }

/* Every service of a type, and its children with it.
 *
 * Deleting a service deletes what hangs off it, so the messages go too — which is what a
 * cleanup wants and is why this does not enumerate messages separately.
 *
 * The selection is taken before anything is deleted rather than deleting while walking:
 * CMsvEntry's child list is invalidated by a delete, and removing entries from underneath an
 * iterator is the kind of thing that works on the first two and corrupts the third. */
void DeleteServicesL(TUid aMtm, TInt& aRemoved)
    {
    TMsvSelectionOrdering ordering;
    CMsvEntry* root = CMsvEntry::NewL(*gSession, KMsvRootIndexEntryId, ordering);
    CleanupStack::PushL(root);

    CMsvEntrySelection* victims = root->ChildrenWithMtmL(aMtm);
    CleanupStack::PushL(victims);

    for (TInt i = 0; i < victims->Count(); i++)
        {
        /* One at a time and tolerant of failure: a service the user has already removed by
         * hand, or one the server refuses, must not stop the rest being cleaned up. */
        TRAPD(err, root->DeleteL(victims->At(i)));
        if (err == KErrNone)
            aRemoved++;
        }

    CleanupStack::PopAndDestroy(2, root);   // victims, root
    }

void DeleteEntryL(TMsvId aId)
    {
    TMsvSelectionOrdering ordering;
    CMsvEntry* self = CMsvEntry::NewL(*gSession, aId, ordering);
    CleanupStack::PushL(self);
    const TMsvId parentId = self->Entry().Parent();
    CleanupStack::PopAndDestroy(self);

    CMsvEntry* parent = CMsvEntry::NewL(*gSession, parentId, ordering);
    CleanupStack::PushL(parent);
    parent->DeleteL(aId);
    CleanupStack::PopAndDestroy(parent);
    }

void FolderCountL(TMsvId aId, TInt* aOut)
    {
    /* Default ordering: no grouping, no sort. Sorting would make the server walk and
     * order the whole folder to produce a number we are about to reduce to its length. */
    TMsvSelectionOrdering ordering;
    CMsvEntry* entry = CMsvEntry::NewL(*gSession, aId, ordering);
    CleanupStack::PushL(entry);
    *aOut = entry->Count();
    CleanupStack::PopAndDestroy(entry);
    }

/* --------------------------------------------------------------- the read side -- */

/* Copy a platform text field into a fixed array, reporting the full length.
 *
 * The full length rather than the copied one, because `iDetails` carries a correspondent's
 * identity and there is no documented cap on it. A silently shortened correspondent is a
 * reply addressed to the wrong person, so the caller is given what it needs to notice. */
void CopyField(const TDesC16& aFrom, uint16_t* aTo, TInt aCap, int32_t& aLen)
    {
    aLen = (int32_t) aFrom.Length();
    TInt n = aFrom.Length();
    if (n > aCap)
        n = aCap;
    for (TInt i = 0; i < n; i++)
        aTo[i] = (uint16_t) aFrom[i];
    }

void EntryInfoL(TMsvId aId, ShimMsvEntry* aOut)
    {
    TMsvSelectionOrdering ordering;
    CMsvEntry* self = CMsvEntry::NewL(*gSession, aId, ordering);
    CleanupStack::PushL(self);
    const TMsvEntry& e = self->Entry();

    aOut->id = (int32_t) e.Id();
    aOut->parent = (int32_t) e.Parent();
    aOut->service_id = (int32_t) e.iServiceId;
    aOut->mtm_uid = (uint32_t) e.iMtm.iUid;
    aOut->type_uid = (uint32_t) e.iType.iUid;
    aOut->unix_time = ToUnixTime(e.iDate);
    aOut->size = (int32_t) e.iSize;

    int32_t flags = 0;
    if (e.New())            flags |= SHIM_MSV_NEW;
    if (e.Unread())         flags |= SHIM_MSV_UNREAD;
    if (e.Complete())       flags |= SHIM_MSV_COMPLETE;
    if (e.Visible())        flags |= SHIM_MSV_VISIBLE;
    if (e.InPreparation())  flags |= SHIM_MSV_IN_PREPARATION;
    if (e.Failed())         flags |= SHIM_MSV_FAILED;
    aOut->flags = flags;

    CopyField(e.iDetails, aOut->details,
              (TInt) (sizeof(aOut->details) / sizeof(aOut->details[0])),
              aOut->details_len);
    CopyField(e.iDescription, aOut->description,
              (TInt) (sizeof(aOut->description) / sizeof(aOut->description[0])),
              aOut->description_len);

    CleanupStack::PopAndDestroy(self);
    }

/* Shared tail of the two id-list calls: copy what fits, report the whole count.
 *
 * Takes the selection rather than producing it, so the two callers differ only in how they
 * ask the server — which is the only place they should differ. */
void CopyIds(const CMsvEntrySelection& aSel, int32_t* aOut, TInt aCap, TInt* aCount)
    {
    *aCount = aSel.Count();
    TInt n = aSel.Count();
    if (n > aCap)
        n = aCap;
    for (TInt i = 0; i < n; i++)
        aOut[i] = (int32_t) aSel.At(i);
    }

void ChildrenL(TMsvId aFolder, int32_t* aOut, TInt aCap, TInt* aCount)
    {
    /* Sorted newest first, unlike FolderCountL's default ordering — because here the order
     * reaches the caller and "the most recent messages" is what a caller asking for a
     * bounded number of children almost always wants. */
    TMsvSelectionOrdering ordering(KMsvNoGrouping, EMsvSortByDateReverse, EFalse);
    CMsvEntry* folder = CMsvEntry::NewL(*gSession, aFolder, ordering);
    CleanupStack::PushL(folder);

    /* Ownership: GetChildrenL hands over the selection. */
    CMsvEntrySelection* sel = folder->ChildrenL();
    CleanupStack::PushL(sel);
    CopyIds(*sel, aOut, aCap, aCount);
    CleanupStack::PopAndDestroy(2, folder);   // sel, folder
    }

void ServicesL(TUid aMtm, int32_t* aOut, TInt aCap, TInt* aCount)
    {
    TMsvSelectionOrdering ordering;
    CMsvEntry* root = CMsvEntry::NewL(*gSession, KMsvRootIndexEntryId, ordering);
    CleanupStack::PushL(root);

    CMsvEntrySelection* sel = root->ChildrenWithMtmL(aMtm);
    CleanupStack::PushL(sel);
    CopyIds(*sel, aOut, aCap, aCount);
    CleanupStack::PopAndDestroy(2, root);   // sel, root
    }

void BodyL(TMsvId aId, uint16_t* aOut, TInt aCap, TInt* aLen)
    {
    *aLen = 0;

    TMsvSelectionOrdering ordering;
    CMsvEntry* self = CMsvEntry::NewL(*gSession, aId, ordering);
    CleanupStack::PushL(self);
    CMsvStore* store = self->ReadStoreL();
    CleanupStack::PushL(store);

    if (!store->HasBodyTextL())
        {
        /* Length 0 and success. A message with no body text is ordinary — a notification, a
         * placeholder — and making the caller tell "empty" from "missing" would invent a
         * distinction the store does not make. */
        CleanupStack::PopAndDestroy(2, self);   // store, self
        return;
        }

    /* The same three-object shape the write path uses, for the same reason: CRichText needs
     * both layers, owns neither, and all three come off the cleanup stack together. */
    CParaFormatLayer* para = CParaFormatLayer::NewL();
    CleanupStack::PushL(para);
    CCharFormatLayer* chars = CCharFormatLayer::NewL();
    CleanupStack::PushL(chars);
    CRichText* body = CRichText::NewL(para, chars);
    CleanupStack::PushL(body);

    store->RestoreBodyTextL(*body);

    const TInt total = body->DocumentLength();
    *aLen = total;
    TInt n = total;
    if (n > aCap)
        n = aCap;
    if (n > 0)
        {
        /* Extract into a TPtr over the caller's buffer — no intermediate allocation, and no
         * assumption that the whole document fits. */
        TPtr16 out(reinterpret_cast<TUint16*>(aOut), 0, n);
        body->Extract(out, 0, n);
        }

    CleanupStack::PopAndDestroy(5, self);   // body, chars, para, store, self
    }

/* Read, modify, write.
 *
 * Not "take an entry from the caller and store it", because a TMsvEntry read a moment ago and
 * written back now undoes every field the server has changed since — the same trap the create
 * path documents, seen from the other side. The caller says which bits it wants and nothing
 * else moves. */
void SetFlagsL(TMsvId aId, TInt aSet, TInt aClear)
    {
    TMsvSelectionOrdering ordering;
    CMsvEntry* self = CMsvEntry::NewL(*gSession, aId, ordering);
    CleanupStack::PushL(self);

    TMsvEntry e = self->Entry();
    /* Clear first, then set, so `set` wins on a collision — which is the documented
     * behaviour and the only one that makes a set/clear pair predictable. */
    if (aClear & SHIM_MSV_NEW)             e.SetNew(EFalse);
    if (aClear & SHIM_MSV_UNREAD)          e.SetUnread(EFalse);
    if (aClear & SHIM_MSV_COMPLETE)        e.SetComplete(EFalse);
    if (aClear & SHIM_MSV_VISIBLE)         e.SetVisible(EFalse);
    if (aClear & SHIM_MSV_IN_PREPARATION)  e.SetInPreparation(EFalse);
    if (aClear & SHIM_MSV_FAILED)          e.SetFailed(EFalse);

    if (aSet & SHIM_MSV_NEW)               e.SetNew(ETrue);
    if (aSet & SHIM_MSV_UNREAD)            e.SetUnread(ETrue);
    if (aSet & SHIM_MSV_COMPLETE)          e.SetComplete(ETrue);
    if (aSet & SHIM_MSV_VISIBLE)           e.SetVisible(ETrue);
    if (aSet & SHIM_MSV_IN_PREPARATION)    e.SetInPreparation(ETrue);
    if (aSet & SHIM_MSV_FAILED)            e.SetFailed(ETrue);

    self->ChangeL(e);
    CleanupStack::PopAndDestroy(self);
    }

/* Reparent. The context has to be the *old* parent — MoveL is a folder operation, not an
 * entry one — which is why this looks up Parent() first, exactly as DeleteEntryL does. */
void MoveEntryL(TMsvId aId, TMsvId aNewParent)
    {
    TMsvSelectionOrdering ordering;
    CMsvEntry* self = CMsvEntry::NewL(*gSession, aId, ordering);
    CleanupStack::PushL(self);
    const TMsvId parentId = self->Entry().Parent();
    CleanupStack::PopAndDestroy(self);

    if (parentId == aNewParent)
        return;   /* Already there. MoveL onto the current parent is not worth asking about. */

    CMsvEntry* parent = CMsvEntry::NewL(*gSession, parentId, ordering);
    CleanupStack::PushL(parent);
    parent->MoveL(aId, aNewParent);
    CleanupStack::PopAndDestroy(parent);
    }

} /* namespace */

extern "C" {

int32_t shim_msv_open(int32_t* out_handle)
    {
    if (!out_handle)
        return SHIM_ERR_ARGUMENT;
    if (gSession)
        return SHIM_ERR_IN_USE;

    TRAPD(err, OpenL());
    if (err != KErrNone)
        {
        /* Half-built state is not left behind for a caller who may retry: the registry
         * can be the thing that failed, and a session with no registry would answer
         * every later question with an empty list. */
        delete gRegistry;
        gRegistry = NULL;
        delete gSession;
        gSession = NULL;
        return err;
        }
    *out_handle = KMsvHandle;
    return SHIM_OK;
    }

int32_t shim_msv_mtm_count(int32_t handle, int32_t* out)
    {
    if (!out)
        return SHIM_ERR_ARGUMENT;
    if (!Valid(handle) || !gRegistry)
        return SHIM_ERR_BAD_HANDLE;
    /* Cannot Leave: an inline read of an array length. */
    *out = (int32_t) gRegistry->NumRegisteredMtmDlls();
    return SHIM_OK;
    }

int32_t shim_msv_refresh_registry(int32_t handle)
    {
    if (!Valid(handle))
        return SHIM_ERR_BAD_HANDLE;
    /* Deleted before the new one is built, so a failure leaves no registry rather than the
     * stale one it was meant to replace — a caller that then read a count would be reading
     * the very snapshot this call exists to discard. */
    delete gRegistry;
    gRegistry = NULL;
    TRAPD(err, gRegistry = CClientMtmRegistry::NewL(*gSession));
    return err == KErrNone ? SHIM_OK : err;
    }

int32_t shim_msv_can_instantiate(int32_t handle, uint32_t mtm_uid)
    {
    if (!Valid(handle) || !gRegistry)
        return SHIM_ERR_BAD_HANDLE;
    CBaseMtm* mtm = NULL;
    TRAPD(err, mtm = gRegistry->NewMtmL(TUid::Uid((TInt32) mtm_uid)));
    /* Destroyed straight away. The question is whether the framework can produce one — the
     * object itself has nothing to say. */
    delete mtm;
    return err == KErrNone ? SHIM_OK : err;
    }

int32_t shim_msv_mtm_info(int32_t handle, int32_t index, ShimMtmInfo* out)
    {
    if (!out || index < 0)
        return SHIM_ERR_ARGUMENT;
    if (!Valid(handle) || !gRegistry)
        return SHIM_ERR_BAD_HANDLE;
    if (index >= gRegistry->NumRegisteredMtmDlls())
        return SHIM_ERR_ARGUMENT;

    TRAPD(err, MtmInfoL(index, out));
    return err == KErrNone ? SHIM_OK : err;
    }

int32_t shim_msv_folder_count(int32_t handle, int32_t folder_id, int32_t* out)
    {
    if (!out)
        return SHIM_ERR_ARGUMENT;
    if (!Valid(handle))
        return SHIM_ERR_BAD_HANDLE;

    TInt count = 0;
    TRAPD(err, FolderCountL((TMsvId) folder_id, &count));
    if (err != KErrNone)
        return err;
    *out = (int32_t) count;
    return SHIM_OK;
    }

void shim_msv_close(int32_t handle)
    {
    if (!Valid(handle))
        return;
    delete gRegistry;
    gRegistry = NULL;
    delete gSession;
    gSession = NULL;
    }

/* ---------------------------------------------------------------- the write side -- */

int32_t shim_msv_install_mtm(const uint16_t* path, int32_t len)
    {
    if (!path || len <= 0)
        return SHIM_ERR_ARGUMENT;
    if (!gSession)
        return SHIM_ERR_BAD_HANDLE;
    TPtrC16 p(reinterpret_cast<const TUint16*>(path), len);
    /* Cannot Leave: InstallMtmGroup returns TInt. */
    return gSession->InstallMtmGroup(p);
    }

int32_t shim_msv_deinstall_mtm(const uint16_t* path, int32_t len)
    {
    if (!path || len <= 0)
        return SHIM_ERR_ARGUMENT;
    if (!gSession)
        return SHIM_ERR_BAD_HANDLE;
    TPtrC16 p(reinterpret_cast<const TUint16*>(path), len);
    /* KErrNotFound here is the ordinary first-run answer and the caller is told to expect
     * it, so it is passed up rather than smoothed away. */
    return gSession->DeInstallMtmGroup(p);
    }

int32_t shim_msv_create_service(int32_t handle, uint32_t mtm_uid,
                                const uint16_t* name, int32_t name_len, int32_t* out_id)
    {
    if (!out_id || !name || name_len <= 0)
        return SHIM_ERR_ARGUMENT;
    if (!Valid(handle))
        return SHIM_ERR_BAD_HANDLE;

    TMsvId created = KMsvNullIndexEntryId;
    TRAPD(err, CreateServiceL(TUid::Uid((TInt32) mtm_uid),
                              TPtrC16(reinterpret_cast<const TUint16*>(name), name_len),
                              created));
    if (err != KErrNone)
        return err;
    *out_id = (int32_t) created;
    return SHIM_OK;
    }

int32_t shim_msv_create_message(int32_t handle, const ShimNewMessage* msg, int32_t* out_id)
    {
    if (!msg || !out_id)
        return SHIM_ERR_ARGUMENT;
    if (!Valid(handle))
        return SHIM_ERR_BAD_HANDLE;

    TMsvId created = KMsvNullIndexEntryId;
    TRAPD(err, CreateMessageL(*msg, created));
    if (err != KErrNone)
        return err;
    *out_id = (int32_t) created;
    return SHIM_OK;
    }

int32_t shim_msv_delete_entry(int32_t handle, int32_t id)
    {
    if (!Valid(handle))
        return SHIM_ERR_BAD_HANDLE;
    TRAPD(err, DeleteEntryL((TMsvId) id));
    return err == KErrNone ? SHIM_OK : err;
    }

int32_t shim_msv_delete_services(int32_t handle, uint32_t mtm_uid)
    {
    if (!Valid(handle))
        return SHIM_ERR_BAD_HANDLE;
    TInt removed = 0;
    TRAPD(err, DeleteServicesL(TUid::Uid((TInt32) mtm_uid), removed));
    if (err != KErrNone)
        return err;
    return (int32_t) removed;
    }

/* --------------------------------------------------------------- the read side --
 * Every one of these validates the handle before it touches anything, and every one wraps its
 * helper in a TRAP: a Leave crossing back into Rust, which is compiled panic=abort, skips
 * every destructor between here and there. */

int32_t shim_msv_entry(int32_t handle, int32_t id, ShimMsvEntry* out)
    {
    if (!out)
        return SHIM_ERR_ARGUMENT;
    if (!Valid(handle))
        return SHIM_ERR_BAD_HANDLE;

    /* Zeroed first, so a caller that ignores the error code reads an empty entry rather than
     * whatever was on its stack. */
    Mem::FillZ(out, sizeof(ShimMsvEntry));
    TRAPD(err, EntryInfoL((TMsvId) id, out));
    return err == KErrNone ? SHIM_OK : err;
    }

int32_t shim_msv_children(int32_t handle, int32_t folder_id,
                          int32_t* out_ids, int32_t cap, int32_t* out_count)
    {
    if (!out_count || cap < 0 || (cap > 0 && !out_ids))
        return SHIM_ERR_ARGUMENT;
    if (!Valid(handle))
        return SHIM_ERR_BAD_HANDLE;

    *out_count = 0;
    TInt count = 0;
    TRAPD(err, ChildrenL((TMsvId) folder_id, out_ids, (TInt) cap, &count));
    if (err != KErrNone)
        return err;
    *out_count = (int32_t) count;
    return SHIM_OK;
    }

int32_t shim_msv_services(int32_t handle, uint32_t mtm_uid,
                          int32_t* out_ids, int32_t cap, int32_t* out_count)
    {
    if (!out_count || cap < 0 || (cap > 0 && !out_ids))
        return SHIM_ERR_ARGUMENT;
    if (!Valid(handle))
        return SHIM_ERR_BAD_HANDLE;

    *out_count = 0;
    TInt count = 0;
    TRAPD(err, ServicesL(TUid::Uid((TInt32) mtm_uid), out_ids, (TInt) cap, &count));
    if (err != KErrNone)
        return err;
    *out_count = (int32_t) count;
    return SHIM_OK;
    }

int32_t shim_msv_body(int32_t handle, int32_t id,
                      uint16_t* out, int32_t cap, int32_t* out_len)
    {
    if (!out_len || cap < 0 || (cap > 0 && !out))
        return SHIM_ERR_ARGUMENT;
    if (!Valid(handle))
        return SHIM_ERR_BAD_HANDLE;

    *out_len = 0;
    TInt len = 0;
    TRAPD(err, BodyL((TMsvId) id, out, (TInt) cap, &len));
    if (err != KErrNone)
        return err;
    *out_len = (int32_t) len;
    return SHIM_OK;
    }

int32_t shim_msv_set_flags(int32_t handle, int32_t id, int32_t set, int32_t clear)
    {
    if (!Valid(handle))
        return SHIM_ERR_BAD_HANDLE;
    TRAPD(err, SetFlagsL((TMsvId) id, (TInt) set, (TInt) clear));
    return err == KErrNone ? SHIM_OK : err;
    }

int32_t shim_msv_move_entry(int32_t handle, int32_t id, int32_t new_parent)
    {
    if (!Valid(handle))
        return SHIM_ERR_BAD_HANDLE;
    TRAPD(err, MoveEntryL((TMsvId) id, (TMsvId) new_parent));
    return err == KErrNone ? SHIM_OK : err;
    }

int32_t shim_msv_observe(int32_t handle, int32_t enable)
    {
    if (!Valid(handle))
        return SHIM_ERR_BAD_HANDLE;
    /* No TRAP: setting a flag cannot leave. The events themselves arrive on the session's own
     * active object, so there is nothing to start here — only permission to deliver. */
    gObserver.iDeliver = enable ? ETrue : EFalse;
    return SHIM_OK;
    }

} /* extern "C" */

#endif /* SHIM_USE_MSG */
