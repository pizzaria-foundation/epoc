/* Read-only reconnaissance over the Message Server.
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
 * WHY IT IS READ-ONLY
 *
 * The point is to learn what the platform's messaging stack contains before deciding
 * whether to build on it — which MTMs are registered, how the folders are populated. None
 * of that needs a write, and a write would put the user's actual messages at risk for a
 * reconnaissance run. Opening a session, enumerating the MTM registry and counting entries
 * is the whole surface.
 *
 * WHY THE OBSERVER IS A STUB
 *
 * CMsvSession::OpenSyncL demands an MMsvSessionObserver and will call back into it on every
 * server event. A probe that runs for a second and asks three questions has nothing to do
 * with those events, and a handler that did anything would be a second thing that could
 * fail. It swallows them.
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

namespace {

/* Required by OpenSyncL, deliberately inert. See the file comment. */
class TShimMsvObserver : public MMsvSessionObserver
    {
public:
    void HandleSessionEventL(TMsvSessionEvent, TAny*, TAny*, TAny*) {}
    };

/* One session at a time. The probe is single-threaded and asks its questions in sequence,
 * so a handle table would be three fields of ceremony around a single slot. The handle is
 * still opaque and still validated, which is what rule 3 in symbian_shim.h is actually
 * for — a stale handle must become an error, not a jump through a dead pointer. */
const int32_t KMsvHandle = 1;

TShimMsvObserver gObserver;
CMsvSession* gSession = NULL;
CClientMtmRegistry* gRegistry = NULL;

TBool Valid(int32_t aHandle)
    {
    return aHandle == KMsvHandle && gSession != NULL;
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
        {
        /* Symbian counts microseconds from year 0; Unix from 1970. 62168256000 seconds is
         * the gap, and getting it wrong puts every message in the year 1 — which sorts to
         * the bottom of the folder and looks like the message never arrived. */
        const TInt64 KUnixEpochInSymbianSeconds = MAKE_TINT64(0, 62168256000);
        entry.iDate = TTime((aMsg.unix_time + KUnixEpochInSymbianSeconds) * 1000000);
        }
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

} /* extern "C" */

#endif /* SHIM_USE_MSG */
