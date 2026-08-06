/* Sockets, over ESock.
 *
 * The hardest part of the shim, and the only one written against a worked example
 * rather than from the headers: sdk/s60cppexamples/Chat/src/chatinet.cpp is a TCP
 * client Nokia shipped, and sdk/s60cppexamples/DataMobility/ is an example dedicated
 * to picking an access point. Between them they settle the things that would otherwise
 * have been guessed, and two of those are traps that cost a hung phone to find:
 *
 *   RSocket::Close() waits forever for a pending Read.  Cancel first, always.
 *   (chatinet.cpp:532)
 *
 *   RConnection::Stop() tears down the shared interface and drops every other
 *   application's connection with it. Only ever Close().
 *   (applicationtriggeringconndlg.cpp:117, which says exactly that)
 *
 * WHAT IS SYNCHRONOUS AND WHAT IS NOT
 *
 * Open and Connect-the-session are synchronous: RSocketServ::Connect, RSocket::Open,
 * RConnection::Open, Bind. Everything that touches the network is not: Connect, Write,
 * RecvOneOrMore, Shutdown, GetByName, and RConnection::Start — which has both forms,
 * and the example uses the blocking one. We cannot: rust_step runs on the GUI thread.
 *
 * THREE ACTIVE OBJECTS PER SOCKET
 *
 * The example uses one, so it can have one operation outstanding, which is why its
 * SendMessageL calls CancelRead() before writing. That is fine for a chat where a
 * human types and wrong for a protocol that reads replies while sending requests. A
 * CActive has one iStatus, so concurrency means more than one of them: a control
 * object for connect and shutdown, a reader, and a writer.
 *
 * BUFFERS BELONG TO RUST
 *
 * RSocket::Write needs a descriptor that stays valid until completion, so the reader
 * and writer hold TPtr8/TPtrC8 members over Rust's memory rather than copying.
 * Copying would close a class of undefined behaviour but would also cap the message
 * size or add a chunking protocol to the ABI. The hazard is closed one level up
 * instead: symbian::net owns its buffers and cancels on Drop, so no caller can free
 * memory the socket server is still reading.
 */

#include "symbian_shim.h"
#include "shim_priv.h"

#include <e32base.h>
#include <e32std.h>
#include <es_sock.h>
#include <in_sock.h>
#include <commdbconnpref.h>
#include <commdb.h>
#include <cdbcols.h>

namespace {

/* Small on purpose. A client of this SDK opens a handful of sockets, and a fixed table
 * means the open path cannot fail for lack of memory. */
const TInt KMaxSockets = 8;
const TInt KMaxNets = 2;
const TInt KMaxResolvers = 4;

/* One session for the whole process, opened on first use. */
RSocketServ gServ;
TBool gServOpen = EFalse;

TInt Serv(RSocketServ*& out)
    {
    if (!gServOpen)
        {
        const TInt err = gServ.Connect();
        if (err != KErrNone)
            return err;
        gServOpen = ETrue;
        }
    out = &gServ;
    return KErrNone;
    }

/* ------------------------------------------------------------------ bearer -- */

/* RConnection, brought up asynchronously.
 *
 * The intended lifecycle is prompt once and never again: the first run passes
 * SHIM_IAP_PROMPT and lets the OS ask, RunL reads back which IAP was chosen with
 * GetIntSetting, and the app persists that id and passes it next time. Falling back to
 * a prompt when a saved id has gone away is the caller's job, because only the caller
 * knows whether it has a saved one. */
class CShimNet : public CActive
    {
public:
    static CShimNet* NewL(TInt aHandle, TInt aIap);
    ~CShimNet();
    void Start();
    /* Sockets and the resolver open against this, which is what binds them to the
     * bearer we brought up rather than to whatever default route exists. */
    RConnection& Conn() { return iConnection; }

private:
    CShimNet(TInt aHandle, TInt aIap);
    void ConstructL();
    void RunL();
    void DoCancel();

    RConnection iConnection;
    TCommDbConnPref iPrefs;
    TInt iHandle;
    TInt iIap;
    TBool iOpen;
    };

CShimNet* gNets[KMaxNets];

/* The RConnection for a handle, or NULL for "no bearer chosen". Opening without one is
 * legal and lets the stack pick a route — fine on a device with one bearer, a coin
 * toss on a phone with both Wi-Fi and packet data. */
RConnection* ConnFor(TInt aHandle);

CShimNet::CShimNet(TInt aHandle, TInt aIap)
    : CActive(EPriorityStandard), iHandle(aHandle), iIap(aIap), iOpen(EFalse)
    {
    }

CShimNet::~CShimNet()
    {
    Cancel();
    if (iOpen)
        {
        /* Close, never Stop. Stop would take every other application's connection
         * down with ours. */
        iConnection.Close();
        iOpen = EFalse;
        }
    }

CShimNet* CShimNet::NewL(TInt aHandle, TInt aIap)
    {
    CShimNet* self = new (ELeave) CShimNet(aHandle, aIap);
    CleanupStack::PushL(self);
    self->ConstructL();
    CleanupStack::Pop(self);
    return self;
    }

void CShimNet::ConstructL()
    {
    RSocketServ* serv = NULL;
    User::LeaveIfError(Serv(serv));
    User::LeaveIfError(iConnection.Open(*serv));
    iOpen = ETrue;
    CActiveScheduler::Add(this);
    }

void CShimNet::Start()
    {
    if (iIap >= 0)
        {
        iPrefs.SetIapId(static_cast<TUint32>(iIap));
        iPrefs.SetDialogPreference(ECommDbDialogPrefDoNotPrompt);
        iConnection.Start(iPrefs, iStatus);
        }
    else if (iIap == SHIM_IAP_PROMPT)
        {
        iPrefs.SetDialogPreference(ECommDbDialogPrefPrompt);
        iConnection.Start(iPrefs, iStatus);
        }
    else
        {
        /* No preferences at all, which is what "let the system decide" actually means.
         *
         * The first version set ECommDbDialogPrefDoNotPrompt on an otherwise empty
         * TCommDbConnPref and passed that — but an empty preference has IAP 0, so it does
         * not say "use the default", it says "connect to access point zero without
         * asking". The overload with no argument is the documented default path, and it
         * is the one the SDK's own Chat example uses. */
        iConnection.Start(iStatus);
        }
    SetActive();
    }

void CShimNet::RunL()
    {
    TInt iap = 0;
    if (iStatus.Int() == KErrNone)
        {
        /* Which access point the OS actually settled on. Reported so the caller can
         * store it and skip the prompt next time; a failure to read it is not worth
         * failing the connection over, so `a` simply stays zero. */
        _LIT(KIapId, "IAP\\Id");
        TUint32 id = 0;
        if (iConnection.GetIntSetting(KIapId, id) == KErrNone)
            iap = static_cast<TInt>(id);
        }
    ShimPushSimple(SHIM_EV_NET_READY, iHandle, iStatus.Int(), iap);
    }

void CShimNet::DoCancel()
    {
    /* Close rather than Stop, for the reason in the destructor. Cancelling a Start
     * that has not completed leaves the interface alone. */
    iConnection.Close();
    iOpen = EFalse;
    }

/* --------------------------------------------------------------------- DNS -- */

class CShimResolver : public CActive
    {
public:
    static CShimResolver* NewL(TInt aHandle, TInt aConn, const TDesC& aHost);
    ~CShimResolver();

private:
    CShimResolver(TInt aHandle);
    void ConstructL(TInt aConn, const TDesC& aHost);
    void RunL();
    void DoCancel();

    RHostResolver iResolver;
    TNameEntry iEntry;
    TInt iHandle;
    TBool iOpen;
    };

CShimResolver* gResolvers[KMaxResolvers];

CShimResolver::CShimResolver(TInt aHandle)
    : CActive(EPriorityStandard), iHandle(aHandle), iOpen(EFalse)
    {
    }

CShimResolver::~CShimResolver()
    {
    Cancel();
    if (iOpen)
        {
        iResolver.Close();
        iOpen = EFalse;
        }
    }

CShimResolver* CShimResolver::NewL(TInt aHandle, TInt aConn, const TDesC& aHost)
    {
    CShimResolver* self = new (ELeave) CShimResolver(aHandle);
    CleanupStack::PushL(self);
    self->ConstructL(aConn, aHost);
    CleanupStack::Pop(self);
    return self;
    }

void CShimResolver::ConstructL(TInt aConn, const TDesC& aHost)
    {
    RSocketServ* serv = NULL;
    User::LeaveIfError(Serv(serv));

    /* Bound to the bearer when there is one. A lookup that went out over a different
     * route than the socket is how you get an address the connection cannot reach —
     * and on a phone that is not hypothetical, since the Wi-Fi and packet-data DNS
     * servers answer differently for anything on the local network. */
    RConnection* conn = ConnFor(aConn);
    if (conn)
        User::LeaveIfError(iResolver.Open(*serv, KAfInet, KProtocolInetTcp, *conn));
    else
        User::LeaveIfError(iResolver.Open(*serv, KAfInet, KProtocolInetTcp));
    iOpen = ETrue;
    CActiveScheduler::Add(this);

    /* Asynchronous, unlike the Chat example, which issues GetByName and then blocks in
     * User::WaitForRequest with a timer for the timeout. That is reasonable in a
     * dialog-driven app and impossible here: it would stall the GUI thread for as long
     * as the lookup takes.
     *
     * The timeout is therefore the caller's: it has shim_timer_after already, and only
     * it knows how long it is willing to wait. */
    iResolver.GetByName(aHost, iEntry, iStatus);
    SetActive();
    }

void CShimResolver::RunL()
    {
    TUint32 addr = 0;
    if (iStatus.Int() == KErrNone)
        {
        TNameRecord record = iEntry();
        /* IPv4 only. TInetAddr::Cast on a v6 result would return a mapped address and
         * Address() would silently give the low 32 bits, so the family is checked. */
        TInetAddr& inet = TInetAddr::Cast(record.iAddr);
        if (inet.Family() == KAfInet)
            addr = inet.Address();
        }
    ShimPushSimple(SHIM_EV_RESOLVED, iHandle, iStatus.Int(),
                   static_cast<TInt>(addr));
    }

void CShimResolver::DoCancel()
    {
    iResolver.Cancel();
    }

/* ------------------------------------------------------------------ socket -- */

class CShimSocket;

/* The reader and the writer are separate active objects sharing one RSocket, so a read
 * and a write can be outstanding at the same time. Each holds a descriptor over the
 * caller's buffer for the duration of its request. */
class CSockReader : public CActive
    {
public:
    CSockReader(RSocket& aSocket, TInt aHandle, TBool aDatagram);
    void Issue(TUint8* aBuf, TInt aCap);

private:
    void RunL();
    void DoCancel();

    RSocket& iSocket;
    TPtr8 iBuf;
    TSockXfrLength iLen;
    /* Only meaningful for a datagram socket, where the caller needs to know who sent
     * the message. */
    TInetAddr iFrom;
    TInt iHandle;
    TBool iDatagram;
    };

class CSockWriter : public CActive
    {
public:
    CSockWriter(RSocket& aSocket, TInt aHandle, TBool aDatagram);
    void Issue(const TUint8* aBuf, TInt aLen);
    void IssueTo(const TUint8* aBuf, TInt aLen, TUint32 aAddr, TUint aPort);

private:
    void RunL();
    void DoCancel();

    RSocket& iSocket;
    TPtrC8 iBuf;
    TInetAddr iTo;
    TInt iHandle;
    TInt iPending;
    TBool iDatagram;
    };

/* Connect and shutdown, which are mutually exclusive with each other but not with a
 * read or a write in flight. */
class CSockCtl : public CActive
    {
public:
    CSockCtl(RSocket& aSocket, TInt aHandle);
    void Connect(TUint32 aAddr, TUint aPort);
    void Shutdown();

private:
    void RunL();
    void DoCancel();

    RSocket& iSocket;
    TInetAddr iAddr;
    TInt iHandle;
    /* Which event the completion should produce. Connect and shutdown share one
     * iStatus, so the object has to remember what it asked for. */
    TInt iEvent;
    };

class CShimSocket
    {
public:
    static CShimSocket* NewL(TInt aHandle, TInt aConn, TBool aDatagram);
    ~CShimSocket();

    RSocket iSocket;
    CSockCtl* iCtl;
    CSockReader* iReader;
    CSockWriter* iWriter;
    TBool iDatagram;

private:
    CShimSocket(TBool aDatagram);
    void ConstructL(TInt aHandle, TInt aConn);
    };

CShimSocket* gSockets[KMaxSockets];

/* ---- reader ---- */

CSockReader::CSockReader(RSocket& aSocket, TInt aHandle, TBool aDatagram)
    : CActive(EPriorityStandard),
      iSocket(aSocket),
      iBuf(NULL, 0, 0),
      iHandle(aHandle),
      iDatagram(aDatagram)
    {
    CActiveScheduler::Add(this);
    }

void CSockReader::Issue(TUint8* aBuf, TInt aCap)
    {
    iBuf.Set(aBuf, 0, aCap);
    if (iDatagram)
        {
        iSocket.RecvFrom(iBuf, iFrom, 0, iStatus);
        }
    else
        {
        /* RecvOneOrMore, not Recv: Recv waits until the descriptor is full, which for a
         * stream means blocking until exactly `cap` bytes arrive. A protocol reading a
         * length-prefixed frame wants whatever is there now. */
        iSocket.RecvOneOrMore(iBuf, 0, iStatus, iLen);
        }
    SetActive();
    }

void CSockReader::RunL()
    {
    ShimEvent e;
    e.kind = SHIM_EV_RECV;
    e.handle = iHandle;
    e.status = iStatus.Int();
    e.a = iBuf.Length();
    e.b = iDatagram ? static_cast<TInt>(iFrom.Address()) : 0;
    e.c = iDatagram ? static_cast<TInt>(iFrom.Port()) : 0;
    e.d = 0;
    e.native = 0;
    ShimPushEvent(e);
    }

void CSockReader::DoCancel()
    {
    iSocket.CancelRecv();
    }

/* ---- writer ---- */

CSockWriter::CSockWriter(RSocket& aSocket, TInt aHandle, TBool aDatagram)
    : CActive(EPriorityStandard),
      iSocket(aSocket),
      iBuf(NULL, 0),
      iHandle(aHandle),
      iPending(0),
      iDatagram(aDatagram)
    {
    CActiveScheduler::Add(this);
    }

void CSockWriter::Issue(const TUint8* aBuf, TInt aLen)
    {
    iBuf.Set(aBuf, aLen);
    iPending = aLen;
    iSocket.Write(iBuf, iStatus);
    SetActive();
    }

void CSockWriter::IssueTo(const TUint8* aBuf, TInt aLen, TUint32 aAddr, TUint aPort)
    {
    iBuf.Set(aBuf, aLen);
    iPending = aLen;
    iTo.SetFamily(KAfInet);
    iTo.SetAddress(aAddr);
    iTo.SetPort(aPort);
    iSocket.SendTo(iBuf, iTo, 0, iStatus);
    SetActive();
    }

void CSockWriter::RunL()
    {
    /* RSocket::Write on a stream is all-or-nothing — it completes with an error rather
     * than a short count — so a success means the whole buffer went, and `a` reports
     * that rather than a length nobody would have to check. */
    ShimPushSimple(SHIM_EV_SENT, iHandle, iStatus.Int(),
                   iStatus.Int() == KErrNone ? iPending : 0);
    iPending = 0;
    }

void CSockWriter::DoCancel()
    {
    /* Write and SendTo are cancelled by different calls, and CancelAll would take the
     * outstanding read down with them — which is the whole thing the separate active
     * objects exist to avoid. */
    if (iDatagram)
        iSocket.CancelSend();
    else
        iSocket.CancelWrite();
    }

/* ---- control ---- */

CSockCtl::CSockCtl(RSocket& aSocket, TInt aHandle)
    : CActive(EPriorityStandard), iSocket(aSocket), iHandle(aHandle), iEvent(0)
    {
    CActiveScheduler::Add(this);
    }

void CSockCtl::Connect(TUint32 aAddr, TUint aPort)
    {
    iAddr.SetFamily(KAfInet);
    iAddr.SetAddress(aAddr);
    iAddr.SetPort(aPort);
    iEvent = SHIM_EV_CONNECTED;
    iSocket.Connect(iAddr, iStatus);
    SetActive();
    }

void CSockCtl::Shutdown()
    {
    iEvent = SHIM_EV_CLOSED;
    iSocket.Shutdown(RSocket::ENormal, iStatus);
    SetActive();
    }

void CSockCtl::RunL()
    {
    ShimPushSimple(iEvent, iHandle, iStatus.Int(), 0);
    }

void CSockCtl::DoCancel()
    {
    /* Specific, not CancelAll: CancelAll would cancel the reader and writer too, which
     * is exactly what having three active objects is meant to prevent. A Shutdown has
     * no cancel of its own — it either completes or the socket is closed under it — so
     * cancelling a control request means cancelling a Connect. */
    if (iEvent == SHIM_EV_CONNECTED)
        iSocket.CancelConnect();
    }

/* ---- socket ---- */

CShimSocket::CShimSocket(TBool aDatagram)
    : iCtl(NULL), iReader(NULL), iWriter(NULL), iDatagram(aDatagram)
    {
    }

CShimSocket::~CShimSocket()
    {
    /* Order matters and this is the trap the example documents: RSocket::Close() waits
     * forever for a pending Read. Every request is cancelled, through the active
     * objects so their own state stays consistent, before the socket is closed. */
    if (iReader)
        iReader->Cancel();
    if (iWriter)
        iWriter->Cancel();
    if (iCtl)
        iCtl->Cancel();

    delete iReader;
    delete iWriter;
    delete iCtl;

    iSocket.Close();
    }

CShimSocket* CShimSocket::NewL(TInt aHandle, TInt aConn, TBool aDatagram)
    {
    CShimSocket* self = new (ELeave) CShimSocket(aDatagram);
    CleanupStack::PushL(self);
    self->ConstructL(aHandle, aConn);
    CleanupStack::Pop(self);
    return self;
    }

void CShimSocket::ConstructL(TInt aHandle, TInt aConn)
    {
    RSocketServ* serv = NULL;
    User::LeaveIfError(Serv(serv));

    const TUint type = iDatagram ? KSockDatagram : KSockStream;
    const TUint proto = iDatagram ? KProtocolInetUdp : KProtocolInetTcp;

    /* Opening against the RConnection rather than the bare session is what binds this
     * socket to the bearer we brought up. Without it the socket would use whatever
     * default route exists, which on a phone with both Wi-Fi and packet data is not a
     * choice worth leaving to chance. */
    RConnection* conn = ConnFor(aConn);
    if (conn)
        User::LeaveIfError(iSocket.Open(*serv, KAfInet, type, proto, *conn));
    else
        User::LeaveIfError(iSocket.Open(*serv, KAfInet, type, proto));

    iCtl = new (ELeave) CSockCtl(iSocket, aHandle);
    iReader = new (ELeave) CSockReader(iSocket, aHandle, iDatagram);
    iWriter = new (ELeave) CSockWriter(iSocket, aHandle, iDatagram);
    }

/* ---- slot bookkeeping ---- */

template <class T>
TInt AllocSlot(T** table, TInt max)
    {
    for (TInt i = 0; i < max; i++)
        if (!table[i])
            return i;
    return KErrNoMemory;
    }

CShimSocket* SocketFor(TInt aHandle)
    {
    if (aHandle < 0 || aHandle >= KMaxSockets)
        return NULL;
    return gSockets[aHandle];
    }

RConnection* ConnFor(TInt aHandle)
    {
    if (aHandle < 0 || aHandle >= KMaxNets || !gNets[aHandle])
        return NULL;
    return &gNets[aHandle]->Conn();
    }

} /* namespace */

void ShimNetCleanup()
    {
    for (TInt i = 0; i < KMaxSockets; i++)
        {
        delete gSockets[i];
        gSockets[i] = NULL;
        }
    for (TInt i = 0; i < KMaxResolvers; i++)
        {
        delete gResolvers[i];
        gResolvers[i] = NULL;
        }
    /* Bearers last: a socket being closed still belongs to one. */
    for (TInt i = 0; i < KMaxNets; i++)
        {
        delete gNets[i];
        gNets[i] = NULL;
        }
    if (gServOpen)
        {
        gServ.Close();
        gServOpen = EFalse;
        }
    }

extern "C" {

/* Enumerate the access points this handset actually has.
 *
 * Written because the self test was sweeping IAP ids 1 through 8 and hoping. The report
 * that came back said "IAP 1: err -1" -- KErrNotFound, arriving as a *completion*, which
 * was the proof that RConnection::Start works fine and that the ids were the problem.
 * Guessing an id is guessing; the comms database knows.
 *
 * Snapshotted into a fixed array on the first call rather than held open, because the
 * caller wants a count before it wants any names, and reopening the database per row
 * would be three table scans to print four lines.
 */
namespace {

struct TIapInfo
    {
    TUint32 iId;
    TBuf<KCommsDbSvrMaxFieldLength> iName;
    TBuf<KCommsDbSvrMaxFieldLength> iService;
    };

const TInt KMaxIaps = 16;
TIapInfo gIaps[KMaxIaps];
TInt gIapCount = -1;   /* -1 means "not looked yet" */

void SnapshotIapsL()
    {
    CCommsDatabase* db = CCommsDatabase::NewL();
    CleanupStack::PushL(db);
    CCommsDbTableView* view = db->OpenTableLC(TPtrC(IAP));

    TInt n = 0;
    TInt err = view->GotoFirstRecord();
    while (err == KErrNone && n < KMaxIaps)
        {
        view->ReadUintL(TPtrC(COMMDB_ID), gIaps[n].iId);
        view->ReadTextL(TPtrC(COMMDB_NAME), gIaps[n].iName);
        /* The service type distinguishes Wi-Fi from packet data, which is the difference
         * between "this will work on my desk" and "this costs money". */
        view->ReadTextL(TPtrC(IAP_SERVICE_TYPE), gIaps[n].iService);
        n++;
        err = view->GotoNextRecord();
        }

    CleanupStack::PopAndDestroy(2);   /* view, db */
    gIapCount = n;
    }

TInt CopyOut(const TDesC& aFrom, uint16_t* aOut, int32_t aCap)
    {
    const TInt n = aFrom.Length() < aCap ? aFrom.Length() : aCap;
    for (TInt i = 0; i < n; i++)
        aOut[i] = static_cast<uint16_t>(aFrom[i]);
    return n;
    }

} /* namespace */

int32_t shim_net_iap_count(void)
    {
    if (gIapCount < 0)
        {
        TInt err = KErrNone;
        TRAP(err, SnapshotIapsL());
        if (err != KErrNone)
            {
            gIapCount = -1;
            return err;
            }
        }
    return gIapCount;
    }

int32_t shim_net_iap_info(int32_t index, int32_t* id, uint16_t* name, int32_t name_cap,
                          int32_t* name_len, uint16_t* service, int32_t service_cap,
                          int32_t* service_len)
    {
    const int32_t count = shim_net_iap_count();
    if (count < 0)
        return count;
    if (index < 0 || index >= count)
        return SHIM_ERR_ARGUMENT;

    if (id)
        *id = static_cast<int32_t>(gIaps[index].iId);
    if (name && name_cap > 0 && name_len)
        *name_len = CopyOut(gIaps[index].iName, name, name_cap);
    if (service && service_cap > 0 && service_len)
        *service_len = CopyOut(gIaps[index].iService, service, service_cap);
    return SHIM_OK;
    }

int32_t shim_net_start(int32_t iap, int32_t* handle)
    {
    if (!handle)
        return SHIM_ERR_ARGUMENT;
    *handle = 0;
    const TInt slot = AllocSlot(gNets, KMaxNets);
    if (slot < 0)
        return SHIM_ERR_IN_USE;

    TInt err = KErrNone;
    TRAP(err, gNets[slot] = CShimNet::NewL(slot, iap));
    if (err != KErrNone)
        return err;

    gNets[slot]->Start();
    *handle = slot;
    return SHIM_OK;
    }

void shim_net_stop(int32_t handle)
    {
    if (handle < 0 || handle >= KMaxNets)
        return;
    delete gNets[handle];
    gNets[handle] = NULL;
    }

int32_t shim_dns_resolve(int32_t conn, const uint16_t* host, int32_t len,
                         int32_t* handle)
    {
    if (!host || len <= 0 || !handle)
        return SHIM_ERR_ARGUMENT;
    *handle = 0;
    const TInt slot = AllocSlot(gResolvers, KMaxResolvers);
    if (slot < 0)
        return SHIM_ERR_IN_USE;

    TPtrC16 name(reinterpret_cast<const TUint16*>(host), len);
    TInt err = KErrNone;
    TRAP(err, gResolvers[slot] = CShimResolver::NewL(slot, conn, name));
    if (err != KErrNone)
        return err;
    *handle = slot;
    return SHIM_OK;
    }

static int32_t OpenSocket(int32_t conn, int32_t* handle, TBool aDatagram)
    {
    if (!handle)
        return SHIM_ERR_ARGUMENT;
    *handle = 0;
    const TInt slot = AllocSlot(gSockets, KMaxSockets);
    if (slot < 0)
        return SHIM_ERR_IN_USE;

    TInt err = KErrNone;
    TRAP(err, gSockets[slot] = CShimSocket::NewL(slot, conn, aDatagram));
    if (err != KErrNone)
        return err;
    *handle = slot;
    return SHIM_OK;
    }

int32_t shim_tcp_open(int32_t conn, int32_t* handle)
    {
    return OpenSocket(conn, handle, EFalse);
    }

int32_t shim_udp_open(int32_t conn, int32_t* handle)
    {
    return OpenSocket(conn, handle, ETrue);
    }

int32_t shim_tcp_connect(int32_t handle, uint32_t ipv4, uint16_t port)
    {
    CShimSocket* s = SocketFor(handle);
    if (!s)
        return SHIM_ERR_BAD_HANDLE;
    if (s->iCtl->IsActive())
        return SHIM_ERR_IN_USE;
    s->iCtl->Connect(ipv4, port);
    return SHIM_OK;
    }

int32_t shim_tcp_send(int32_t handle, const uint8_t* buf, int32_t len)
    {
    CShimSocket* s = SocketFor(handle);
    if (!s)
        return SHIM_ERR_BAD_HANDLE;
    if (!buf || len <= 0)
        return SHIM_ERR_ARGUMENT;
    if (s->iWriter->IsActive())
        return SHIM_ERR_IN_USE;
    s->iWriter->Issue(buf, len);
    return SHIM_OK;
    }

int32_t shim_tcp_recv(int32_t handle, uint8_t* buf, int32_t cap)
    {
    CShimSocket* s = SocketFor(handle);
    if (!s)
        return SHIM_ERR_BAD_HANDLE;
    if (!buf || cap <= 0)
        return SHIM_ERR_ARGUMENT;
    if (s->iReader->IsActive())
        return SHIM_ERR_IN_USE;
    s->iReader->Issue(buf, cap);
    return SHIM_OK;
    }

int32_t shim_udp_send_to(int32_t handle, const uint8_t* buf, int32_t len,
                         uint32_t ipv4, uint16_t port)
    {
    CShimSocket* s = SocketFor(handle);
    if (!s)
        return SHIM_ERR_BAD_HANDLE;
    if (!buf || len <= 0)
        return SHIM_ERR_ARGUMENT;
    if (!s->iDatagram)
        return SHIM_ERR_NOT_SUPPORTED;
    if (s->iWriter->IsActive())
        return SHIM_ERR_IN_USE;
    s->iWriter->IssueTo(buf, len, ipv4, port);
    return SHIM_OK;
    }

int32_t shim_udp_recv_from(int32_t handle, uint8_t* buf, int32_t cap)
    {
    CShimSocket* s = SocketFor(handle);
    if (!s)
        return SHIM_ERR_BAD_HANDLE;
    if (!s->iDatagram)
        return SHIM_ERR_NOT_SUPPORTED;
    return shim_tcp_recv(handle, buf, cap);
    }

void shim_tcp_close(int32_t handle)
    {
    if (handle < 0 || handle >= KMaxSockets)
        return;
    /* The destructor cancels before closing; see the comment there. A graceful
     * Shutdown is the caller's business — if it wants the peer to see a FIN rather than
     * a reset it should issue one and wait for SHIM_EV_CLOSED first. */
    delete gSockets[handle];
    gSockets[handle] = NULL;
    }

} /* extern "C" */
