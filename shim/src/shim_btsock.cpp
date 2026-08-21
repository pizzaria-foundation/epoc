/* Bluetooth RFCOMM data sockets and SDP service registration — the transport the
 * remote-shell agent (apps/rshell) runs on, and the one thing about it that cannot be
 * assumed on a 2009 handset.
 *
 * Isolated behind SHIM_USE_BTSOCK, and separate from SHIM_USE_BT on purpose. USE_BT already
 * carries the registry side (btmanclient/btdevice) that the bt probe proved. This file adds
 * one import neither that probe nor anything else here has ever linked — sdpdatabase, for
 * RSdp/RSdpDatabase — plus esock and bluetooth for the RFCOMM socket itself. An import the
 * loader cannot satisfy stops the whole image with no panic and no log, so this rode alone in
 * apps/devdump/probes/btsock (and the tap-to-run apps/rfprobe) before it earned a place in a
 * resident daemon.
 *
 * # Two halves
 *
 * `shim_bt_rfcomm_probe` is the synchronous go/no-go: bring a server socket up and tear it
 * down, one error code per step, no scheduler. The rest is the asynchronous server the agent
 * uses, built exactly like the TCP sockets in shim_net.cpp — a listener plus per-connection
 * reader and writer active objects, each completion pushing a SHIM_EV_BT_* event into the
 * ring. The phone is the *server*: there is no Connect here, because the laptop dials in.
 */

#include "shim_priv.h"

#ifdef SHIM_USE_BTSOCK

#include <e32base.h>
#include <e32std.h>
#include <es_sock.h>
#include <bt_sock.h>
#include <bttypes.h>
#include <btsdp.h>

namespace {

/* The 16-bit SDP UUIDs the record names. Written as literals rather than reached for through
 * headers whose constant names drift between SDK drops: these three numbers are fixed by the
 * Bluetooth assigned-numbers document and will not change.
 *
 *   0x1101  Serial Port service class (SPP)
 *   0x0100  the L2CAP protocol UUID
 *   0x0003  the RFCOMM protocol UUID
 */
const TInt KUuidSerialPort = 0x1101;
const TInt KUuidL2Cap      = 0x0100;
const TInt KUuidRfcomm     = 0x0003;

/* The primary-language ServiceName attribute (string base 0x0100, offset 0). */
const TSdpAttributeID KSdpAttrIdServiceName = 0x0100;

/* Build the ProtocolDescriptorList [ [L2CAP], [RFCOMM, channel] ] onto an existing record.
 * Leaves on failure; every caller runs it under a TRAP. */
void BuildProtocolListL(RSdpDatabase& aDb, TSdpServRecordHandle aHandle, TUint8 aChannel)
    {
    CSdpAttrValueDES* proto = CSdpAttrValueDES::NewDESL(NULL);
    CleanupStack::PushL(proto);
    proto->StartListL()
            ->BuildDESL()->StartListL()
                ->BuildUUIDL(TUUID(TInt(KUuidL2Cap)))
            ->EndListL()
            ->BuildDESL()->StartListL()
                ->BuildUUIDL(TUUID(TInt(KUuidRfcomm)))
                ->BuildUintL(TSdpIntBuf<TUint8>(aChannel))
            ->EndListL()
        ->EndListL();
    aDb.UpdateAttributeL(aHandle, KSdpAttrIdProtocolDescriptorList, *proto);
    CleanupStack::PopAndDestroy(proto);
    }

/* Register an SPP service record pointing at `aChannel`, then delete it again — the probe
 * proves the database accepts the record, it does not want to leave one advertised. */
void RegisterSppRecordL(RSdpDatabase& aDb, TUint8 aChannel)
    {
    TSdpServRecordHandle handle = 0;
    aDb.CreateServiceRecordL(TUUID(TInt(KUuidSerialPort)), handle);
    BuildProtocolListL(aDb, handle, aChannel);
    aDb.DeleteRecordL(handle);
    }

/* The persistent variant the daemon keeps advertised: create the SPP record, add the protocol
 * list and an optional (ASCII) service name, and return the handle so teardown can delete it. */
void RegisterSppRecordPersistentL(RSdpDatabase& aDb, TUint8 aChannel,
                                  const TUint16* aName, TInt aNameLen,
                                  TSdpServRecordHandle& aOut)
    {
    aDb.CreateServiceRecordL(TUUID(TInt(KUuidSerialPort)), aOut);
    BuildProtocolListL(aDb, aOut, aChannel);
    if (aName && aNameLen > 0)
        {
        /* SDP service names are 8-bit text. The agent's name is ASCII ("rshell"), so a narrow
         * copy is exact; anything non-ASCII is dropped rather than mis-encoded. */
        TBuf8<64> name8;
        const TInt n = aNameLen < 64 ? aNameLen : 64;
        for (TInt i = 0; i < n; i++)
            {
            const TUint16 u = aName[i];
            if (u < 0x80)
                name8.Append((TChar)u);
            }
        if (name8.Length() > 0)
            aDb.UpdateAttributeL(aOut, KSdpAttrIdServiceName, name8);
        }
    }

/* The synchronous probe's bring-up, leaving-safe. Every handle opened is pushed onto the
 * cleanup stack; the normal path pops-and-destroys them in reverse. No early returns, so the
 * pops balance. */
void ProbeL(ShimBtRfcommProbe& p)
    {
    RSocketServ ss;
    p.serv_err = ss.Connect();
    if (p.serv_err != KErrNone)
        return;
    CleanupClosePushL(ss);

    RSocket sock;
    p.open_err = sock.Open(ss, KBTAddrFamily, KSockStream, KRFCOMM);
    if (p.open_err != KErrNone)
        {
        CleanupStack::PopAndDestroy(&ss);
        return;
        }
    CleanupClosePushL(sock);

    TInt chan = 0;
    p.channel_err = sock.GetOpt(KRFCOMMGetAvailableServerChannel, KSolBtRFCOMM, chan);
    if (p.channel_err == KErrNone)
        p.channel = chan;

    TRfcommSockAddr addr;
    addr.SetPort((TUint)(p.channel_err == KErrNone ? chan : 0));
    p.bind_err = sock.Bind(addr);

    RSdp sdp;
    p.sdp_open_err = sdp.Connect();
    if (p.sdp_open_err == KErrNone)
        {
        CleanupClosePushL(sdp);
        RSdpDatabase db;
        const TInt dbErr = db.Open(sdp);
        if (dbErr != KErrNone)
            {
            p.sdp_open_err = dbErr;
            }
        else
            {
            CleanupClosePushL(db);
            const TUint8 recordChannel = (TUint8)(p.channel_err == KErrNone ? chan : 1);
            TRAPD(regErr, RegisterSppRecordL(db, recordChannel));
            p.sdp_reg_err = regErr;
            CleanupStack::PopAndDestroy(&db);
            }
        CleanupStack::PopAndDestroy(&sdp);
        }

    p.listen_err = sock.Listen(1);

    CleanupStack::PopAndDestroy(&sock);
    CleanupStack::PopAndDestroy(&ss);
    }

/* ============================ asynchronous server ============================ */

const TInt KMaxRfSockets = 4;

class CRfSocket;

/* One outstanding RecvOneOrMore over a caller-owned buffer. RecvOneOrMore, not Recv, for the
 * same reason shim_net.cpp gives: a length-prefixed protocol wants whatever is there now, not
 * a descriptor filled to capacity. */
class CRfReader : public CActive
    {
public:
    CRfReader(RSocket& aSocket, TInt aHandle);
    void Issue(TUint8* aBuf, TInt aCap);
private:
    void RunL();
    void DoCancel();
    RSocket& iSocket;
    TPtr8 iBuf;
    TSockXfrLength iLen;
    TInt iHandle;
    };

class CRfWriter : public CActive
    {
public:
    CRfWriter(RSocket& aSocket, TInt aHandle);
    void Issue(const TUint8* aBuf, TInt aLen);
private:
    void RunL();
    void DoCancel();
    RSocket& iSocket;
    TPtrC8 iBuf;
    TInt iHandle;
    TInt iPending;
    };

class CRfSocket
    {
public:
    RSocket iSocket;
    CRfReader* iReader;
    CRfWriter* iWriter;
    CRfSocket() : iReader(NULL), iWriter(NULL) {}
    ~CRfSocket()
        {
        /* Cancel through the active objects before Close: RSocket::Close waits forever for a
         * pending Recv — the trap shim_net.cpp documents. */
        if (iReader) iReader->Cancel();
        if (iWriter) iWriter->Cancel();
        delete iReader;
        delete iWriter;
        iSocket.Close();
        }
    };

/* The listener's Accept, as its own active object. Fills a pre-opened blank socket kept in the
 * pending slot; on completion the accepted-socket handle is that slot. */
class CRfAcceptor : public CActive
    {
public:
    CRfAcceptor(RSocket& aListen);
    void Issue(TInt aSlot);
    TBool Busy() const { return IsActive(); }
private:
    void RunL();
    void DoCancel();
    RSocket& iListen;
    TInt iSlot;
    };

RSocketServ gRfServ;   TBool gRfServOpen = EFalse;
RSocket gRfListen;     TBool gRfListenOpen = EFalse;
RSdp gRfSdp;           TBool gRfSdpOpen = EFalse;
RSdpDatabase gRfDb;    TBool gRfDbOpen = EFalse;
TSdpServRecordHandle gRfRecord = 0; TBool gRfRecordSet = EFalse;
CRfAcceptor* gRfAcceptor = NULL;
CRfSocket* gRf[KMaxRfSockets] = { NULL, NULL, NULL, NULL };

TInt RfServ(RSocketServ*& aOut)
    {
    if (!gRfServOpen)
        {
        const TInt e = gRfServ.Connect();
        if (e != KErrNone)
            return e;
        gRfServOpen = ETrue;
        }
    aOut = &gRfServ;
    return KErrNone;
    }

CRfSocket* RfFor(TInt aHandle)
    {
    if (aHandle < 0 || aHandle >= KMaxRfSockets)
        return NULL;
    return gRf[aHandle];
    }

TInt AllocRfSlot()
    {
    for (TInt i = 0; i < KMaxRfSockets; i++)
        if (!gRf[i])
            return i;
    return KErrNoMemory;
    }

/* ---- reader ---- */
CRfReader::CRfReader(RSocket& aSocket, TInt aHandle)
    : CActive(EPriorityStandard), iSocket(aSocket), iBuf(NULL, 0, 0), iHandle(aHandle)
    {
    CActiveScheduler::Add(this);
    }
void CRfReader::Issue(TUint8* aBuf, TInt aCap)
    {
    iBuf.Set(aBuf, 0, aCap);
    iSocket.RecvOneOrMore(iBuf, 0, iStatus, iLen);
    SetActive();
    }
void CRfReader::RunL()
    {
    ShimPushSimple(SHIM_EV_BT_RECV, iHandle, iStatus.Int(), iBuf.Length());
    }
void CRfReader::DoCancel()
    {
    iSocket.CancelRecv();
    }

/* ---- writer ---- */
CRfWriter::CRfWriter(RSocket& aSocket, TInt aHandle)
    : CActive(EPriorityStandard), iSocket(aSocket), iBuf(NULL, 0), iHandle(aHandle), iPending(0)
    {
    CActiveScheduler::Add(this);
    }
void CRfWriter::Issue(const TUint8* aBuf, TInt aLen)
    {
    iBuf.Set(aBuf, aLen);
    iPending = aLen;
    iSocket.Write(iBuf, iStatus);
    SetActive();
    }
void CRfWriter::RunL()
    {
    ShimPushSimple(SHIM_EV_BT_SENT, iHandle, iStatus.Int(),
                   iStatus.Int() == KErrNone ? iPending : 0);
    iPending = 0;
    }
void CRfWriter::DoCancel()
    {
    iSocket.CancelWrite();
    }

/* ---- acceptor ---- */
CRfAcceptor::CRfAcceptor(RSocket& aListen)
    : CActive(EPriorityStandard), iListen(aListen), iSlot(-1)
    {
    CActiveScheduler::Add(this);
    }
void CRfAcceptor::Issue(TInt aSlot)
    {
    iSlot = aSlot;
    iListen.Accept(gRf[aSlot]->iSocket, iStatus);
    SetActive();
    }
void CRfAcceptor::RunL()
    {
    const TInt rc = iStatus.Int();
    if (rc == KErrNone)
        {
        /* The blank socket is now connected; give it its reader and writer. */
        CRfSocket* s = gRf[iSlot];
        s->iReader = new CRfReader(s->iSocket, iSlot);
        s->iWriter = new CRfWriter(s->iSocket, iSlot);
        if (!s->iReader || !s->iWriter)
            {
            delete s;
            gRf[iSlot] = NULL;
            ShimPushSimple(SHIM_EV_BT_ACCEPTED, -1, KErrNoMemory, 0);
            }
        else
            {
            ShimPushSimple(SHIM_EV_BT_ACCEPTED, iSlot, KErrNone, 0);
            }
        }
    else
        {
        /* Accept failed: drop the blank socket that was staged for it. */
        delete gRf[iSlot];
        gRf[iSlot] = NULL;
        ShimPushSimple(SHIM_EV_BT_ACCEPTED, -1, rc, 0);
        }
    iSlot = -1;
    }
void CRfAcceptor::DoCancel()
    {
    iListen.CancelAccept();
    if (iSlot >= 0 && iSlot < KMaxRfSockets && gRf[iSlot])
        {
        delete gRf[iSlot];
        gRf[iSlot] = NULL;
        }
    iSlot = -1;
    }

/* ---- listener bring-up ---- */
void ListenStartL(TInt aBacklog, const TUint16* aName, TInt aNameLen, TInt* aOutChannel)
    {
    RSocketServ* serv = NULL;
    User::LeaveIfError(RfServ(serv));

    User::LeaveIfError(gRfListen.Open(*serv, KBTAddrFamily, KSockStream, KRFCOMM));
    gRfListenOpen = ETrue;

    TInt chan = 0;
    User::LeaveIfError(gRfListen.GetOpt(KRFCOMMGetAvailableServerChannel, KSolBtRFCOMM, chan));

    TRfcommSockAddr addr;
    addr.SetPort((TUint)chan);
    User::LeaveIfError(gRfListen.Bind(addr));
    User::LeaveIfError(gRfListen.Listen(aBacklog > 0 ? aBacklog : 1));

    User::LeaveIfError(gRfSdp.Connect());
    gRfSdpOpen = ETrue;
    User::LeaveIfError(gRfDb.Open(gRfSdp));
    gRfDbOpen = ETrue;
    RegisterSppRecordPersistentL(gRfDb, (TUint8)chan, aName, aNameLen, gRfRecord);
    gRfRecordSet = ETrue;

    gRfAcceptor = new (ELeave) CRfAcceptor(gRfListen);

    if (aOutChannel)
        *aOutChannel = chan;
    }

void DropRecord()
    {
    if (gRfRecordSet && gRfDbOpen)
        {
        TRAP_IGNORE(gRfDb.DeleteRecordL(gRfRecord));
        gRfRecordSet = EFalse;
        }
    }

} /* namespace */

/* ============================ C ABI ============================ */

extern "C" int32_t shim_bt_rfcomm_probe(ShimBtRfcommProbe* out)
    {
    if (!out)
        return SHIM_ERR_ARGUMENT;

    out->serv_err = SHIM_BT_PROBE_SKIPPED;
    out->open_err = SHIM_BT_PROBE_SKIPPED;
    out->channel_err = SHIM_BT_PROBE_SKIPPED;
    out->channel = -1;
    out->bind_err = SHIM_BT_PROBE_SKIPPED;
    out->sdp_open_err = SHIM_BT_PROBE_SKIPPED;
    out->sdp_reg_err = SHIM_BT_PROBE_SKIPPED;
    out->listen_err = SHIM_BT_PROBE_SKIPPED;

    TRAPD(err, ProbeL(*out));
    if (err != KErrNone)
        return err;
    return SHIM_OK;
    }

extern "C" int32_t shim_btrf_listen_start(int32_t backlog, const uint16_t* name,
                                          int32_t name_len, int32_t* out_channel)
    {
    if (gRfListenOpen)
        return SHIM_ERR_IN_USE;
    TInt chan = 0;
    TRAPD(err, ListenStartL(backlog, reinterpret_cast<const TUint16*>(name), name_len, &chan));
    if (err != KErrNone)
        {
        /* Half-open teardown so a retry starts clean. */
        ShimBtsockCleanup();
        return err;
        }
    if (out_channel)
        *out_channel = chan;
    return SHIM_OK;
    }

extern "C" int32_t shim_btrf_accept(void)
    {
    if (!gRfListenOpen || !gRfAcceptor)
        return SHIM_ERR_NOT_READY;
    if (gRfAcceptor->Busy())
        return SHIM_ERR_IN_USE;

    const TInt slot = AllocRfSlot();
    if (slot < 0)
        return SHIM_ERR_NO_MEMORY;

    RSocketServ* serv = NULL;
    const TInt se = RfServ(serv);
    if (se != KErrNone)
        return se;

    CRfSocket* s = new CRfSocket();
    if (!s)
        return SHIM_ERR_NO_MEMORY;
    /* The blank open: an unbound socket for Accept to turn into the connected one. */
    const TInt oe = s->iSocket.Open(*serv);
    if (oe != KErrNone)
        {
        delete s;
        return oe;
        }
    gRf[slot] = s;
    gRfAcceptor->Issue(slot);
    return SHIM_OK;
    }

extern "C" int32_t shim_btrf_recv(int32_t handle, uint8_t* buf, int32_t cap)
    {
    if (!buf || cap <= 0)
        return SHIM_ERR_ARGUMENT;
    CRfSocket* s = RfFor(handle);
    if (!s || !s->iReader)
        return SHIM_ERR_NOT_FOUND;
    if (s->iReader->IsActive())
        return SHIM_ERR_IN_USE;
    s->iReader->Issue(buf, cap);
    return SHIM_OK;
    }

extern "C" int32_t shim_btrf_send(int32_t handle, const uint8_t* buf, int32_t len)
    {
    if (!buf || len <= 0)
        return SHIM_ERR_ARGUMENT;
    CRfSocket* s = RfFor(handle);
    if (!s || !s->iWriter)
        return SHIM_ERR_NOT_FOUND;
    if (s->iWriter->IsActive())
        return SHIM_ERR_IN_USE;
    s->iWriter->Issue(buf, len);
    return SHIM_OK;
    }

extern "C" int32_t shim_btrf_close(int32_t handle)
    {
    if (handle < 0 || handle >= KMaxRfSockets)
        return SHIM_ERR_ARGUMENT;
    if (gRf[handle])
        {
        delete gRf[handle];
        gRf[handle] = NULL;
        }
    return SHIM_OK;
    }

extern "C" int32_t shim_btrf_listen_stop(void)
    {
    if (gRfAcceptor)
        {
        gRfAcceptor->Cancel();
        delete gRfAcceptor;
        gRfAcceptor = NULL;
        }
    DropRecord();
    if (gRfDbOpen)   { gRfDb.Close();   gRfDbOpen = EFalse; }
    if (gRfSdpOpen)  { gRfSdp.Close();  gRfSdpOpen = EFalse; }
    if (gRfListenOpen) { gRfListen.Close(); gRfListenOpen = EFalse; }
    return SHIM_OK;
    }

void ShimBtsockCleanup()
    {
    if (gRfAcceptor)
        {
        gRfAcceptor->Cancel();
        delete gRfAcceptor;
        gRfAcceptor = NULL;
        }
    for (TInt i = 0; i < KMaxRfSockets; i++)
        {
        delete gRf[i];
        gRf[i] = NULL;
        }
    DropRecord();
    if (gRfDbOpen)   { gRfDb.Close();   gRfDbOpen = EFalse; }
    if (gRfSdpOpen)  { gRfSdp.Close();  gRfSdpOpen = EFalse; }
    if (gRfListenOpen) { gRfListen.Close(); gRfListenOpen = EFalse; }
    if (gRfServOpen) { gRfServ.Close(); gRfServOpen = EFalse; }
    }

#else

extern "C" int32_t shim_bt_rfcomm_probe(ShimBtRfcommProbe* out)
    {
    (void) out;
    return SHIM_ERR_NOT_SUPPORTED;
    }
extern "C" int32_t shim_btrf_listen_start(int32_t backlog, const uint16_t* name,
                                          int32_t name_len, int32_t* out_channel)
    {
    (void) backlog; (void) name; (void) name_len;
    if (out_channel) *out_channel = 0;
    return SHIM_ERR_NOT_SUPPORTED;
    }
extern "C" int32_t shim_btrf_accept(void) { return SHIM_ERR_NOT_SUPPORTED; }
extern "C" int32_t shim_btrf_recv(int32_t handle, uint8_t* buf, int32_t cap)
    { (void) handle; (void) buf; (void) cap; return SHIM_ERR_NOT_SUPPORTED; }
extern "C" int32_t shim_btrf_send(int32_t handle, const uint8_t* buf, int32_t len)
    { (void) handle; (void) buf; (void) len; return SHIM_ERR_NOT_SUPPORTED; }
extern "C" int32_t shim_btrf_close(int32_t handle) { (void) handle; return SHIM_ERR_NOT_SUPPORTED; }
extern "C" int32_t shim_btrf_listen_stop(void) { return SHIM_ERR_NOT_SUPPORTED; }

#endif /* SHIM_USE_BTSOCK */
