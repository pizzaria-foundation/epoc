/* Bluetooth: the state the platform's BT server keeps, read and written from our side.
 *
 * Isolated behind SHIM_USE_BT because it imports six libraries at once — btmanclient,
 * btdevice, bluetooth, btextnotifiers, esock, centralrepository — and an import the handset
 * cannot satisfy stops the image loading with no error, no log and no report file. The
 * device sweep loaded all six DLLs (docs/device-dump.txt:208-239), which proves they open,
 * not that the ordinals we call exist. That is what apps/devdump/probes/bt is for, and it is
 * the only binary that may carry this file until it has reported.
 *
 * # We do not replace the BT server
 *
 * It is in ROM and the native OBEX push, the headset profiles and the host's own btrecv.py
 * all depend on it. Everything here reads or writes the state that server keeps, so a change
 * made through this file is one the native Bluetooth screen sees, and one the native screen
 * makes is one we see.
 *
 * # Why the caches are structs and not handles
 *
 * A CBTRegistryResponse owns its device array and dies with the call that made it. Handing
 * Rust an index into an array it does not own is the lifetime bug the handle table exists to
 * prevent, so a refresh flattens what it found into plain PODs here and the caller reads
 * those. It costs 32 slots of stack-sized data and removes the whole class of problem.
 */

#include "shim_priv.h"

#ifdef SHIM_USE_BT

#include <e32base.h>
#include <e32std.h>
#include <es_sock.h>
#include <bt_sock.h>
#include <btdevice.h>
#include <btmanclient.h>
#include <btnotifierapi.h>
#include <btserversdkcrkeys.h>
#include <centralrepository.h>

namespace {

/* How many devices either cache holds. A paired list on a phone is single digits and an
 * inquiry on a 320x240 screen shows a dozen; 32 is generous for both and small enough to
 * live in BSS without thinking about it. A refresh that finds more reports the full count
 * and keeps the first 32 — the caller can see it was cut. */
const TInt KMaxCache = 32;

/* Names are held at 32 UTF-16 units, matching ShimBtDevice::name. The Bluetooth maximum is
 * 248 bytes; nothing this SDK draws shows more than a couple of dozen characters, and
 * `name_len` carries the truth for a caller that cares. */
const TInt KNameUnits = 32;

ShimBtDevice gPaired[KMaxCache];
TInt gPairedCount = 0;

ShimBtDevice gFound[KMaxCache];
TInt gFoundCount = 0;

/* The registry session, opened on first use and closed by ShimBtCleanup. One session for the
 * process, for the same reason the shim keeps one RFs: every open handle is a server-side
 * subsession, and a leaked one panics at process exit naming the server rather than us. */
RBTRegServ gRegServ;
TBool gRegServOpen = EFalse;

/* ------------------------------------------------------------------ helpers -- */

void ZeroDevice(ShimBtDevice& aOut)
    {
    aOut.addr[0] = aOut.addr[1] = aOut.addr[2] = 0;
    aOut.addr[3] = aOut.addr[4] = aOut.addr[5] = 0;
    aOut.pad[0] = aOut.pad[1] = 0;
    aOut.device_class = 0;
    aOut.flags = 0;
    aOut.name_len = 0;
    for (TInt i = 0; i < KNameUnits; ++i)
        aOut.name[i] = 0;
    }

/* Copy a TBTDevAddr's six bytes out. `Des()` is a TPtrC8 over the address's own storage. */
void CopyAddr(const TBTDevAddr& aAddr, uint8_t* aOut)
    {
    const TPtrC8 des = aAddr.Des();
    const TInt n = des.Length() < KBTDevAddrSize ? des.Length() : KBTDevAddrSize;
    for (TInt i = 0; i < n; ++i)
        aOut[i] = des[i];
    }

/* Build a TBTDevAddr from six caller-supplied bytes. */
TBTDevAddr MakeAddr(const uint8_t* aAddr6)
    {
    TBuf8<KBTDevAddrSize> buf;
    buf.Copy(aAddr6, KBTDevAddrSize);
    return TBTDevAddr(buf);
    }

/* Store a 16-bit name, reporting the full length and keeping what fits. */
void StoreName(const TDesC& aName, uint16_t* aOut, int32_t& aLen)
    {
    aLen = aName.Length();
    const TInt n = aName.Length() < KNameUnits ? aName.Length() : KNameUnits;
    for (TInt i = 0; i < n; ++i)
        aOut[i] = (uint16_t) aName[i];
    }

/* A remote device's own Bluetooth name is UTF-8. BTDeviceNameConverter is btdevice's own
 * converter and can leave, so this helper is only ever called from a leaving function. */
void StoreName8L(const TDesC8& aName, uint16_t* aOut, int32_t& aLen)
    {
    TBTDeviceName8 narrow;
    const TInt n = aName.Length() < narrow.MaxLength() ? aName.Length() : narrow.MaxLength();
    narrow.Copy(aName.Left(n));
    const TBTDeviceName wide = BTDeviceNameConverter::ToUnicodeL(narrow);
    StoreName(wide, aOut, aLen);
    }

/* Flatten one registry entry. The friendly name wins when there is one: it is the name the
 * user chose, and a headset that calls itself "HS-16" is exactly the case renaming exists
 * for. SHIM_BT_FRIENDLY records which of the two the caller is looking at. */
void FlattenL(const CBTDevice& aDev, ShimBtDevice& aOut)
    {
    ZeroDevice(aOut);
    if (aDev.IsValidBDAddr())
        CopyAddr(aDev.BDAddr(), aOut.addr);
    if (aDev.IsValidDeviceClass())
        aOut.device_class = (uint32_t) aDev.DeviceClass().DeviceClass();

    if (aDev.IsValidPaired() && aDev.IsPaired())
        aOut.flags |= SHIM_BT_PAIRED;
    if (aDev.IsValidGlobalSecurity())
        {
        const TBTDeviceSecurity sec = aDev.GlobalSecurity();
        /* S60's "trusted" is not a bit of its own: it means the device connects without the
         * user being asked to authorise it, which is NoAuthorise. */
        if (sec.NoAuthorise())
            aOut.flags |= SHIM_BT_TRUSTED;
        if (sec.Banned())
            aOut.flags |= SHIM_BT_BLOCKED;
        if (sec.Encrypt())
            aOut.flags |= SHIM_BT_ENCRYPT;
        }

    if (aDev.IsValidFriendlyName() && aDev.FriendlyName().Length() > 0)
        {
        StoreName(aDev.FriendlyName(), aOut.name, aOut.name_len);
        aOut.flags |= SHIM_BT_FRIENDLY;
        }
    else if (aDev.IsValidDeviceName())
        {
        StoreName8L(aDev.DeviceName(), aOut.name, aOut.name_len);
        }
    }

/* The one registry session, opened lazily. */
void RegServL()
    {
    if (gRegServOpen)
        return;
    User::LeaveIfError(gRegServ.Connect());
    gRegServOpen = ETrue;
    }

/* ----------------------------------------------------------------- the power -- */

void PowerGetL(TInt* aOut)
    {
    CRepository* rep = CRepository::NewLC(KCRUidBluetoothPowerState);
    TInt val = 0;
    User::LeaveIfError(rep->Get(KBTPowerState, val));
    *aOut = (val == EBTPowerOn) ? 1 : 0;
    CleanupStack::PopAndDestroy(rep);
    }

TInt PowerSetCenRep(TInt aOn)
    {
    TInt err = KErrNone;
    CRepository* rep = NULL;
    TRAP(err, rep = CRepository::NewL(KCRUidBluetoothPowerState));
    if (err != KErrNone)
        return err;
    err = rep->Set(KBTPowerState, aOn ? (TInt) EBTPowerOn : (TInt) EBTPowerOff);
    delete rep;
    return err;
    }

/* The documented S60 route, and the only one that can raise the platform's own "Activate
 * Bluetooth?" query — which is also its limit: it turns the radio ON and it waits for a
 * person.
 *
 * Bounded at KNotifierBudgetUs against an RTimer, because a dialog waits for a human and a
 * probe's deadline is not a measurement of somebody's attention span (the same argument the
 * net probe makes for not dialling). A cancelled notifier still has to have its request
 * consumed, or the next User::WaitForRequest on this thread collects a completion nobody
 * asked for. */
const TInt KNotifierBudgetUs = 20 * 1000 * 1000;

TInt PowerSetNotifier()
    {
    RNotifier notifier;
    TInt err = notifier.Connect();
    if (err != KErrNone)
        return err;

    RTimer timer;
    err = timer.CreateLocal();
    if (err != KErrNone)
        {
        notifier.Close();
        return err;
        }

    TPckgBuf<TBool> param;
    TPckgBuf<TBool> result;
    TRequestStatus st;
    TRequestStatus tst;
    timer.After(tst, KNotifierBudgetUs);
    notifier.StartNotifierAndGetResponse(st, KPowerModeSettingNotifierUid, param, result);

    User::WaitForRequest(st, tst);

    if (st == KRequestPending)
        {
        /* The budget ended it. Cancel and collect, in that order. */
        notifier.CancelNotifier(KPowerModeSettingNotifierUid);
        User::WaitForRequest(st);
        err = KErrTimedOut;
        }
    else
        {
        timer.Cancel();
        User::WaitForRequest(tst);
        err = st.Int();
        }

    timer.Close();
    notifier.Close();
    return err;
    }

/* ---------------------------------------------------------- the local device -- */

void LocalGetL(ShimBtLocal* aOut)
    {
    RegServL();

    RBTLocalDevice local;
    User::LeaveIfError(local.Open(gRegServ));
    CleanupClosePushL(local);

    TBTLocalDevice rec;
    User::LeaveIfError(local.Get(rec));

    if (rec.IsValidAddress())
        CopyAddr(rec.Address(), aOut->addr);
    aOut->device_class = rec.IsValidDeviceClass() ? (uint32_t) rec.DeviceClass() : 0;
    aOut->scan_enable = rec.IsValidScanEnable() ? (int32_t) rec.ScanEnable() : -1;
    aOut->limited = rec.IsValidLimitedDiscoverable() ? (rec.LimitedDiscoverable() ? 1 : 0) : -1;
    aOut->power_setting = rec.IsValidPowerSetting() ? (int32_t) rec.PowerSetting() : -1;
    aOut->paired_only = rec.IsValidAcceptPairedOnlyMode() ? (rec.AcceptPairedOnlyMode() ? 1 : 0) : -1;
    if (rec.IsValidDeviceName())
        StoreName8L(rec.DeviceName(), aOut->name, aOut->name_len);

    CleanupStack::PopAndDestroy(); /* local */
    }

void VisibilitySetL(TInt aScanEnable)
    {
    RegServL();

    RBTLocalDevice local;
    User::LeaveIfError(local.Open(gRegServ));
    CleanupClosePushL(local);

    /* Read-modify-write: a TBTLocalDevice written back with only one field set clears every
     * other one the registry holds, because the set-mask is what says which fields count. */
    TBTLocalDevice rec;
    User::LeaveIfError(local.Get(rec));
    rec.SetScanEnable((THCIScanEnable) aScanEnable);
    User::LeaveIfError(local.Modify(rec));

    CleanupStack::PopAndDestroy(); /* local */
    }

/* -------------------------------------------------------------- the registry -- */

void PairedRefreshL(TInt* aOutCount)
    {
    RegServL();

    RBTRegistry registry;
    User::LeaveIfError(registry.Open(gRegServ));
    CleanupClosePushL(registry);

    TBTRegistrySearch search;
    search.FindBonded();

    TRequestStatus st;
    registry.CreateView(search, st);
    User::WaitForRequest(st);
    User::LeaveIfError(st.Int());

    CBTRegistryResponse* response = CBTRegistryResponse::NewL(registry);
    CleanupStack::PushL(response);
    response->Start(st);
    User::WaitForRequest(st);
    User::LeaveIfError(st.Int());

    RBTDeviceArray& results = response->Results();
    const TInt total = results.Count();
    gPairedCount = total < KMaxCache ? total : KMaxCache;
    for (TInt i = 0; i < gPairedCount; ++i)
        FlattenL(*results[i], gPaired[i]);

    CleanupStack::PopAndDestroy(response);
    registry.CloseView();
    CleanupStack::PopAndDestroy(); /* registry */

    *aOutCount = total;
    }

/* Read one device's nameless record, hand it to `aMutate`, write it back.
 *
 * GetDevice takes the record it is going to fill *and* reads the address out of it, which is
 * why the address is set on the way in. */
void ModifyDeviceL(const TBTDevAddr& aAddr, void (*aMutate)(TBTNamelessDevice&, TInt), TInt aArg)
    {
    RegServL();

    RBTRegistry registry;
    User::LeaveIfError(registry.Open(gRegServ));
    CleanupClosePushL(registry);

    TBTNamelessDevice dev;
    dev.SetAddress(aAddr);

    TRequestStatus st;
    registry.GetDevice(dev, st);
    User::WaitForRequest(st);
    User::LeaveIfError(st.Int());

    aMutate(dev, aArg);

    registry.ModifyDevice(dev, st);
    User::WaitForRequest(st);
    User::LeaveIfError(st.Int());

    CleanupStack::PopAndDestroy(); /* registry */
    }

void SetTrustedFlag(TBTNamelessDevice& aDev, TInt aTrusted)
    {
    TBTDeviceSecurity sec = aDev.IsValidGlobalSecurity() ? aDev.GlobalSecurity()
                                                         : TBTDeviceSecurity();
    /* (TBool) rather than ETrue / EFalse: the two are different enumerations, and the compiler
     * is right to say so. */
    sec.SetNoAuthorise((TBool) (aTrusted != 0));
    aDev.SetGlobalSecurity(sec);
    }

void UnpairL(const TBTDevAddr& aAddr)
    {
    RegServL();

    RBTRegistry registry;
    User::LeaveIfError(registry.Open(gRegServ));
    CleanupClosePushL(registry);

    TRequestStatus st;
    registry.UnpairDevice(aAddr, st);
    User::WaitForRequest(st);
    User::LeaveIfError(st.Int());

    CleanupStack::PopAndDestroy(); /* registry */
    }

void RenameL(const TBTDevAddr& aAddr, const TDesC& aName)
    {
    RegServL();

    RBTRegistry registry;
    User::LeaveIfError(registry.Open(gRegServ));
    CleanupClosePushL(registry);

    TRequestStatus st;
    registry.ModifyFriendlyDeviceNameL(aAddr, aName, st);
    User::WaitForRequest(st);
    User::LeaveIfError(st.Int());

    CleanupStack::PopAndDestroy(); /* registry */
    }

/* -------------------------------------------------------------- the inquiry -- */

/* One inquiry, run to completion. See the header: this blocks, so it belongs in a daemon and
 * nowhere else.
 *
 * KHostResIgnoreCache is deliberate. Without it the resolver may answer from the stack's own
 * cache, which is exactly the wrong thing for a screen whose whole purpose is "what is near
 * me now". */
void InquiryL(TInt aBudgetMs, TInt aMaxDevices, TInt* aOutFound, TInt* aOutErr)
    {
    gFoundCount = 0;
    *aOutFound = 0;
    *aOutErr = KErrNone;

    const TInt limit = (aMaxDevices > 0 && aMaxDevices < KMaxCache) ? aMaxDevices : KMaxCache;

    RSocketServ ss;
    User::LeaveIfError(ss.Connect());
    CleanupClosePushL(ss);

    RHostResolver hr;
    User::LeaveIfError(hr.Open(ss, KBTAddrFamily, KBTLinkManager));
    CleanupClosePushL(hr);

    RTimer timer;
    User::LeaveIfError(timer.CreateLocal());
    CleanupClosePushL(timer);

    TInquirySockAddr addr;
    addr.SetIAC(KGIAC);
    addr.SetAction(KHostResInquiry | KHostResName | KHostResIgnoreCache);

    TNameEntry entry;
    TRequestStatus st;
    TRequestStatus tst;
    timer.After(tst, aBudgetMs * 1000);
    hr.GetByAddress(addr, entry, st);

    for (;;)
        {
        User::WaitForRequest(st, tst);

        if (st == KRequestPending)
            {
            /* The budget ended it. Cancel, then collect the cancellation. */
            hr.Cancel();
            User::WaitForRequest(st);
            *aOutErr = KErrTimedOut;
            break;
            }

        const TInt rc = st.Int();
        if (rc != KErrNone)
            {
            /* KErrEof is how the resolver says "that was the last one" — an ordinary end,
             * not a failure. Anything else is the finding. */
            if (rc != KErrEof)
                *aOutErr = rc;
            timer.Cancel();
            User::WaitForRequest(tst);
            break;
            }

        if (gFoundCount < limit)
            {
            ShimBtDevice& slot = gFound[gFoundCount];
            ZeroDevice(slot);
            const TInquirySockAddr& found = TInquirySockAddr::Cast(entry().iAddr);
            CopyAddr(found.BTAddr(), slot.addr);
            /* The inquiry reports the class of device in three pieces; TBTDeviceClass is what
             * knows how they pack into the 24-bit word the registry stores, so the two caches
             * carry the same number rather than two encodings of it. */
            const TBTDeviceClass cod(found.MajorServiceClass(),
                                     found.MajorClassOfDevice(),
                                     found.MinorClassOfDevice());
            slot.device_class = (uint32_t) cod.DeviceClass();
            /* The host resolver's iName is already 16-bit: it is the device's Bluetooth name
             * widened by the stack, not the UTF-8 the registry stores. */
            StoreName(entry().iName, slot.name, slot.name_len);
            ++gFoundCount;
            }
        ++(*aOutFound);

        if (gFoundCount >= limit)
            {
            /* No resolver request is outstanding here: `st` completed a few lines above and
             * `Next` has not been called again. Cancelling and then waiting on `st` would be
             * waiting for a signal nobody is going to send — a hang, not a leak, and one that
             * would present as the probe timing out on a step that had already succeeded.
             * Only the timer is still armed. */
            timer.Cancel();
            User::WaitForRequest(tst);
            break;
            }

        hr.Next(entry, st);
        }

    CleanupStack::PopAndDestroy(3); /* timer, hr, ss */
    }

} /* namespace */

/* ---------------------------------------------------------------- the ABI -- */

extern "C" int32_t shim_bt_power_get(int32_t* out_on)
    {
    if (out_on)
        *out_on = 0;
    TInt on = 0;
    TRAPD(err, PowerGetL(&on));
    if (err != KErrNone)
        return err;
    if (out_on)
        *out_on = on;
    return SHIM_OK;
    }

extern "C" int32_t shim_bt_power_set(int32_t on, int32_t* out_via)
    {
    if (out_via)
        *out_via = 0;

    TInt last = KErrNotSupported;

    /* The notifier can only turn it on, and only by asking a person. */
    if (on)
        {
        last = PowerSetNotifier();
        if (last == KErrNone)
            {
            /* The notifier reports that its query was answered, not that the radio came up —
             * the user may have said no. Only the power key can say what actually happened,
             * so it is the key that decides whether this route worked. */
            TInt now = 0;
            TRAPD(readErr, PowerGetL(&now));
            if (readErr == KErrNone && now == 1)
                {
                if (out_via)
                    *out_via = SHIM_BT_VIA_NOTIFIER;
                return SHIM_OK;
                }
            }
        }

    const TInt cen = PowerSetCenRep(on);
    if (cen == KErrNone)
        {
        if (out_via)
            *out_via = SHIM_BT_VIA_CENREP;
        return SHIM_OK;
        }

    return (last == KErrNone || last == KErrNotSupported) ? cen : last;
    }

extern "C" int32_t shim_bt_local_get(ShimBtLocal* out)
    {
    if (!out)
        return SHIM_ERR_ARGUMENT;
    out->addr[0] = out->addr[1] = out->addr[2] = 0;
    out->addr[3] = out->addr[4] = out->addr[5] = 0;
    out->pad[0] = out->pad[1] = 0;
    out->device_class = 0;
    out->scan_enable = -1;
    out->limited = -1;
    out->power_setting = -1;
    out->paired_only = -1;
    out->name_len = 0;
    for (TInt i = 0; i < KNameUnits; ++i)
        out->name[i] = 0;

    TRAPD(err, LocalGetL(out));
    return err == KErrNone ? SHIM_OK : err;
    }

extern "C" int32_t shim_bt_visibility_set(int32_t scan_enable)
    {
    if (scan_enable < 0 || scan_enable > 3)
        return SHIM_ERR_ARGUMENT;
    TRAPD(err, VisibilitySetL(scan_enable));
    return err == KErrNone ? SHIM_OK : err;
    }

extern "C" int32_t shim_bt_paired_refresh(int32_t* out_count)
    {
    if (out_count)
        *out_count = 0;
    gPairedCount = 0;
    TInt total = 0;
    TRAPD(err, PairedRefreshL(&total));
    if (err != KErrNone)
        return err;
    if (out_count)
        *out_count = total;
    return SHIM_OK;
    }

extern "C" int32_t shim_bt_paired_get(int32_t index, ShimBtDevice* out)
    {
    if (!out)
        return SHIM_ERR_ARGUMENT;
    if (index < 0 || index >= gPairedCount)
        return SHIM_ERR_NOT_FOUND;
    *out = gPaired[index];
    return SHIM_OK;
    }

extern "C" int32_t shim_bt_set_trusted(const uint8_t* addr6, int32_t trusted)
    {
    if (!addr6)
        return SHIM_ERR_ARGUMENT;
    TRAPD(err, ModifyDeviceL(MakeAddr(addr6), &SetTrustedFlag, trusted));
    return err == KErrNone ? SHIM_OK : err;
    }

extern "C" int32_t shim_bt_unpair(const uint8_t* addr6)
    {
    if (!addr6)
        return SHIM_ERR_ARGUMENT;
    TRAPD(err, UnpairL(MakeAddr(addr6)));
    return err == KErrNone ? SHIM_OK : err;
    }

extern "C" int32_t shim_bt_rename(const uint8_t* addr6, const uint16_t* name, int32_t len)
    {
    if (!addr6 || (!name && len > 0) || len < 0)
        return SHIM_ERR_ARGUMENT;
    /* An empty name is a legitimate rename — it clears the friendly name and puts the
     * device's own name back — so it gets an empty descriptor rather than a rejection. A
     * literal would not do here: wchar_t is not TUint16 on this toolchain. */
    TPtrC16 wide;
    if (name && len > 0)
        wide.Set((const TUint16*) name, len);
    TRAPD(err, RenameL(MakeAddr(addr6), wide));
    return err == KErrNone ? SHIM_OK : err;
    }

extern "C" int32_t shim_bt_close(void)
    {
    ShimBtCleanup();
    return SHIM_OK;
    }

extern "C" int32_t shim_bt_inquiry_sync(int32_t budget_ms, int32_t max_devices,
                                        int32_t* out_found)
    {
    if (out_found)
        *out_found = 0;
    if (budget_ms <= 0)
        return SHIM_ERR_ARGUMENT;

    TInt found = 0;
    TInt inner = KErrNone;
    TRAPD(err, InquiryL(budget_ms, max_devices, &found, &inner));
    if (out_found)
        *out_found = found;
    if (err != KErrNone)
        return err;
    return inner == KErrNone ? SHIM_OK : inner;
    }

extern "C" int32_t shim_bt_found_get(int32_t index, ShimBtDevice* out)
    {
    if (!out)
        return SHIM_ERR_ARGUMENT;
    if (index < 0 || index >= gFoundCount)
        return SHIM_ERR_NOT_FOUND;
    *out = gFound[index];
    return SHIM_OK;
    }

void ShimBtCleanup()
    {
    if (gRegServOpen)
        {
        gRegServ.Close();
        gRegServOpen = EFalse;
        }
    gPairedCount = 0;
    gFoundCount = 0;
    }

#else

extern "C" int32_t shim_bt_power_get(int32_t* out_on)
    {
    if (out_on)
        *out_on = 0;
    return SHIM_ERR_NOT_SUPPORTED;
    }

extern "C" int32_t shim_bt_power_set(int32_t on, int32_t* out_via)
    {
    (void) on;
    if (out_via)
        *out_via = 0;
    return SHIM_ERR_NOT_SUPPORTED;
    }

extern "C" int32_t shim_bt_local_get(ShimBtLocal* out)
    {
    (void) out;
    return SHIM_ERR_NOT_SUPPORTED;
    }

extern "C" int32_t shim_bt_visibility_set(int32_t scan_enable)
    {
    (void) scan_enable;
    return SHIM_ERR_NOT_SUPPORTED;
    }

extern "C" int32_t shim_bt_paired_refresh(int32_t* out_count)
    {
    if (out_count)
        *out_count = 0;
    return SHIM_ERR_NOT_SUPPORTED;
    }

extern "C" int32_t shim_bt_paired_get(int32_t index, ShimBtDevice* out)
    {
    (void) index;
    (void) out;
    return SHIM_ERR_NOT_SUPPORTED;
    }

extern "C" int32_t shim_bt_set_trusted(const uint8_t* addr6, int32_t trusted)
    {
    (void) addr6;
    (void) trusted;
    return SHIM_ERR_NOT_SUPPORTED;
    }

extern "C" int32_t shim_bt_unpair(const uint8_t* addr6)
    {
    (void) addr6;
    return SHIM_ERR_NOT_SUPPORTED;
    }

extern "C" int32_t shim_bt_rename(const uint8_t* addr6, const uint16_t* name, int32_t len)
    {
    (void) addr6;
    (void) name;
    (void) len;
    return SHIM_ERR_NOT_SUPPORTED;
    }

extern "C" int32_t shim_bt_close(void)
    {
    return SHIM_ERR_NOT_SUPPORTED;
    }

extern "C" int32_t shim_bt_inquiry_sync(int32_t budget_ms, int32_t max_devices,
                                        int32_t* out_found)
    {
    (void) budget_ms;
    (void) max_devices;
    if (out_found)
        *out_found = 0;
    return SHIM_ERR_NOT_SUPPORTED;
    }

extern "C" int32_t shim_bt_found_get(int32_t index, ShimBtDevice* out)
    {
    (void) index;
    (void) out;
    return SHIM_ERR_NOT_SUPPORTED;
    }

#endif
