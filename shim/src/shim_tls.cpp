/* HTTPS over the platform secure socket, for the headless sync helper (never the UI thread).
 *
 * WHY NOT mbedTLS DIRECTLY. The device carries shinovon's mbedTLS port, but it ships as a *patch of
 * the system ssl.dll* (0x10001842): the tiny ssl.dll routes Symbian's own CSecureSocket down to
 * mbedtls under the hood, giving the whole phone TLS 1.2. mbedtls.dll is an internal component of
 * that patch — it has writable static data (dataSize/bssSize != 0) and is not built to be linked
 * and called directly. Proven on the E72: linking mbedtls.dll and calling even a pure one-shot
 * (mbedtls_sha256) faults on the first instruction into the DLL. So we do NOT touch mbedtls; we use
 * the native CSecureSocket API, which the patch has already upgraded to negotiate TLS 1.2.
 *
 * This file is one blocking call — shim_https_get — that brings up a bearer, resolves, connects a
 * TCP socket, wraps it in a CSecureSocket, does the handshake, sends an HTTP/1.1 GET and reads the
 * whole response into the caller's buffer.
 *
 * BLOCKING BY DESIGN. It uses User::WaitForRequest, which is a kernel panic on a GUI thread (see
 * the project rule). It is therefore only ever linked into a HEADLESS one-shot helper (calsync /
 * tlsprobe), which has no window server to starve.
 *
 * Certificate verification: the secure socket runs in UNATTENDED dialog mode (KSSLDialogUnattendedMode)
 * so a headless process is never blocked by a cert-trust dialog. The platform still validates the
 * chain against the device cert store; a name/chain failure surfaces as a handshake error code
 * rather than a prompt. KSoSSLDomainName carries the hostname for SNI and name checking.
 */

#include "shim_priv.h"

#ifdef SHIM_USE_TLS

#include <e32std.h>
#include <e32base.h>
#include <es_sock.h>
#include <in_sock.h>
#include <commdbconnpref.h>
#include <es_enum.h>
#include <f32file.h>

#include <securesocket.h>
#include <ssl.h>            /* KSolInetSSL, KSoSSLDomainName, KSoDialogMode, KSSLDialogUnattendedMode */

/* Breadcrumb: overwrite C:\Data\tlsstage.txt with the current step, so a hang or crash leaves the
 * last stage reached. Diagnostic only. */
static void stage(const char* tag)
    {
    RFs fs;
    if (fs.Connect() != KErrNone) return;
    _LIT(KDir,   "C:\\Data\\");
    _LIT(KStage, "C:\\Data\\tlsstage.txt");
    fs.MkDirAll(KDir);
    RFile f;
    if (f.Replace(fs, KStage, EFileWrite) == KErrNone)
        {
        TPtrC8 p((const TUint8*)tag);
        f.Write(p);
        f.Close();
        }
    fs.Close();
    }

namespace {

/* Block on an async request WHILE dispatching active objects. CSecureSocket is a CActive-driven
 * state machine (CTlsConnection/CHandshake): StartClientHandshake/Send/Recv complete the caller's
 * TRequestStatus only after their own internal active objects run, and those run only when the
 * scheduler dispatches them. A bare User::WaitForRequest waits on the thread semaphore but never
 * dispatches, so the handshake hangs forever (measured). This waiter re-enters the running
 * scheduler with a nested CActiveSchedulerWait — safe because the daemon has a scheduler installed
 * and we are called from inside a RunL. ESOCK ops (connect/DNS) don't need this; TLS ops do. */
class CAsyncWaiter : public CActive
    {
public:
    CAsyncWaiter() : CActive(EPriorityStandard) { CActiveScheduler::Add(this); }
    ~CAsyncWaiter() { Cancel(); }
    /* Issue the async op into iStatus, then Await() runs the scheduler until it completes. We use
     * CActiveScheduler::Start()/Stop() rather than CActiveSchedulerWait because the ssl.dll wrapper
     * (CTlsConnection) itself calls CActiveScheduler::Stop() on completion, and on this S60 3.2
     * build that Stop must be paired with a matching Start — a CActiveSchedulerWait mismatches it
     * and panics E32USER-CBase 91 (measured). Our RunL also stops, guarded so whichever fires first
     * wins and the second is a no-op. Safe as a top-level run in the worker thread. */
    void Await()
        {
        iStopped = EFalse;
        SetActive();
        CActiveScheduler::Start();
        }
    using CActive::iStatus;
    TInt Result() const { return iStatus.Int(); }
private:
    void Stop() { if (!iStopped) { iStopped = ETrue; CActiveScheduler::Stop(); } }
    void RunL() { Stop(); }
    void DoCancel() { Stop(); }
    TBool iStopped;
    };

/* One connected TCP socket plus the session/connection that own it. */
struct Conn
    {
    RSocketServ ss;
    RConnection conn;
    RHostResolver hr;
    RSocket sock;
    TBool ssOpen, connOpen, sockOpen;
    Conn() : ssOpen(EFalse), connOpen(EFalse), sockOpen(EFalse) {}
    };

/* Bring up a bearer, resolve the host, open and connect a TCP socket. Blocking.
 * Stage-tagged so a failure says which step broke: -(3100+stage). */
TInt connect(Conn& c, const TDesC& aHost, TInt aPort)
    {
    TInt rc = c.ss.Connect();
    if (rc != KErrNone) return -3101;
    c.ssOpen = ETrue;
    rc = c.conn.Open(c.ss);
    if (rc != KErrNone) return -3102;
    c.connOpen = ETrue;
    TRequestStatus st;
    /* Attach to the connection the user already brought up (WiFi/GPRS). Start() with no prefs
     * returns KErrNotFound when no *default* IAP is configured — even with WiFi connected — and
     * would otherwise pop an access-point dialog a headless process cannot answer. Attaching to the
     * active connection (the recipe shim_net uses) avoids both. */
    TUint acount = 0;
    TInt eerr = c.conn.EnumerateConnections(acount);
    if (eerr == KErrNone && acount >= 1)
        {
        TPckgBuf<TConnectionInfo> info;
        TInt gerr = c.conn.GetConnectionInfo(1, info);   /* 1-based */
        if (gerr != KErrNone) gerr = c.conn.GetConnectionInfo(0, info);
        if (gerr != KErrNone) return -3110 + gerr;
        rc = c.conn.Attach(info, RConnection::EAttachTypeNormal);
        if (rc != KErrNone) return -3120 + rc;
        }
    else
        {
        TCommDbConnPref prefs;
        prefs.SetDialogPreference(ECommDbDialogPrefDoNotPrompt);
        c.conn.Start(prefs, st);
        User::WaitForRequest(st);
        if (st.Int() != KErrNone) return -3200 + st.Int();
        }
    stage("conn_ok");

    rc = c.hr.Open(c.ss, KAfInet, KProtocolInetTcp, c.conn);
    if (rc != KErrNone) return -3104;
    TNameEntry ne;
    c.hr.GetByName(aHost, ne, st);
    User::WaitForRequest(st);
    c.hr.Close();
    if (st.Int() != KErrNone) return -3300 + st.Int();   /* DNS: -3300+err */
    stage("dns_ok");
    TInetAddr addr;
    addr = TInetAddr::Cast(ne().iAddr);
    addr.SetPort(aPort);

    rc = c.sock.Open(c.ss, KAfInet, KSockStream, KProtocolInetTcp, c.conn);
    if (rc != KErrNone) return -3106;
    c.sockOpen = ETrue;
    c.sock.Connect(addr, st);
    User::WaitForRequest(st);
    if (st.Int() != KErrNone) return -3400 + st.Int();   /* TCP connect: -3400+err */
    stage("tcp_ok");
    return KErrNone;
    }

void closeConn(Conn& c)
    {
    if (c.sockOpen) { c.sock.Close(); }
    if (c.connOpen) { c.conn.Close(); }
    if (c.ssOpen)   { c.ss.Close(); }
    }

/* Wrap a connected RSocket in a CSecureSocket. Tries the registered protocol names the patched
 * ssl.dll may answer to; NewL leaves KErrNotFound for a name it does not provide, so we fall back
 * rather than guessing one. Returns NULL and sets aErr on failure. */
CSecureSocket* newSecure(RSocket& aSock, TInt& aErr)
    {
    _LIT(KTls, "TLS1.0");
    _LIT(KSsl, "SSL3.0");
    CSecureSocket* sec = NULL;
    TRAPD(e1, sec = CSecureSocket::NewL(aSock, KTls));
    if (e1 == KErrNone && sec) { aErr = KErrNone; return sec; }
    TRAPD(e2, sec = CSecureSocket::NewL(aSock, KSsl));
    if (e2 == KErrNone && sec) { aErr = KErrNone; return sec; }
    aErr = e1;   /* report the TLS attempt's error */
    return NULL;
    }

} /* namespace */

namespace {

/* Arguments handed to the worker thread. All pointers are into the caller's (same-process) memory,
 * valid for the whole call because the daemon thread blocks on the worker's logon. */
struct TlsArgs
    {
    const uint16_t* host; int32_t hostLen; int32_t port;
    const uint16_t* path; int32_t pathLen;
    uint8_t* out; int32_t outCap;
    int32_t result;
    };

/* The actual HTTPS GET, run IN THE WORKER THREAD under that thread's own top-level scheduler.
 * Every CSecureSocket op is driven by a CAsyncWaiter (CActiveSchedulerWait) — safe here because
 * this scheduler is not already running, so the wait is a normal top-level run, not a nested one
 * inside the daemon's pump callback (which panics). Returns bytes written or a negative error. */
TInt TlsWorkerL(TlsArgs& a)
    {
    TPtrC hostw((const TUint16*)a.host, a.hostLen);
    HBufC8* host8 = HBufC8::New(a.hostLen + 1);
    if (!host8) return KErrNoMemory;
    TPtr8 h8 = host8->Des();
    for (TInt i = 0; i < a.hostLen; ++i) h8.Append((TChar)hostw[i]);

    stage("start");
    Conn c;
    TInt rc = connect(c, hostw, a.port);
    if (rc != KErrNone) { closeConn(c); delete host8; return rc; }

    /* Wrap the TCP socket in TLS. */
    TInt secErr = KErrNone;
    CSecureSocket* sec = newSecure(c.sock, secErr);
    if (!sec) { stage("sec_fail"); closeConn(c); delete host8; return -3500 + secErr; }
    stage("sec_new");

    /* Headless: never pop a cert-trust dialog. SNI + cert name check via the domain name. */
    sec->SetOpt(KSoDialogMode, KSolInetSSL, KSSLDialogUnattendedMode);
    sec->SetOpt(KSoSSLDomainName, KSolInetSSL, h8);
    stage("sec_opt");

    {
    CAsyncWaiter hw;
    sec->StartClientHandshake(hw.iStatus);
    stage("hs_issued");
    hw.Await();
    if (hw.Result() != KErrNone)
        {
        stage("hs_fail");
        TInt e = hw.Result();
        sec->Close(); delete sec; closeConn(c); delete host8;
        return -3600 + e;             /* handshake failure: -3600+err */
        }
    }
    stage("hs_ok");

    /* Build and send the request. */
    TBuf8<640> req;
    req.Append(_L8("GET "));
    for (TInt i = 0; i < a.pathLen && a.path; ++i) req.Append((TChar)((const TUint16*)a.path)[i]);
    if (a.pathLen <= 0) req.Append(_L8("/"));
    req.Append(_L8(" HTTP/1.1\r\nHost: "));
    req.Append(h8);
    req.Append(_L8("\r\nUser-Agent: ADBian\r\nConnection: close\r\n\r\n"));

    {
    TSockXfrLength sent;
    CAsyncWaiter sw;
    sec->Send(req, sw.iStatus, sent);
    sw.Await();
    if (sw.Result() != KErrNone)
        {
        stage("send_fail");
        TInt e = sw.Result();
        sec->Close(); delete sec; closeConn(c); delete host8;
        return -3700 + e;
        }
    }
    stage("sent");

    /* Read the whole response until the peer closes (Connection: close). */
    TInt written = 0;
    for (;;)
        {
        TPtr8 p(a.out + written, 0, a.outCap - written);
        TSockXfrLength got;
        CAsyncWaiter rw;
        sec->RecvOneOrMore(p, rw.iStatus, got);
        rw.Await();
        TInt r = rw.Result();
        if (r == KErrEof) { written += p.Length(); break; }  /* peer closed: keep last bytes */
        if (r != KErrNone) break;                /* error or reset — return what we have */
        written += p.Length();
        if (p.Length() == 0) break;
        if (a.outCap - written <= 0) break;
        }
    stage("done");

    sec->Close();
    delete sec;
    closeConn(c);
    delete host8;
    return written;
    }

/* Worker-thread entry: give this thread its own cleanup stack and a FRESH active scheduler, then
 * run the blocking TLS work. The scheduler is installed but not started, so the CAsyncWaiter inside
 * TlsWorkerL runs it top-level. Returning normally sets a.result; a panic here is reported to the
 * daemon thread via the thread exit reason. */
TInt TlsThreadEntry(TAny* aArg)
    {
    TlsArgs* a = (TlsArgs*)aArg;
    CTrapCleanup* cleanup = CTrapCleanup::New();
    if (!cleanup) { a->result = KErrNoMemory; return KErrNoMemory; }
    CActiveScheduler* sched = new CActiveScheduler;
    if (!sched) { delete cleanup; a->result = KErrNoMemory; return KErrNoMemory; }
    CActiveScheduler::Install(sched);
    TRAPD(err, a->result = TlsWorkerL(*a));
    if (err != KErrNone) a->result = err;
    delete sched;
    delete cleanup;
    return KErrNone;
    }

} /* namespace */

extern "C" {

/* One-shot HTTPS GET. host/path are UTF-16 (ASCII content); writes the raw HTTP response (status
 * line + headers + body) into out, returns the byte count, or a negative error.
 *
 * The whole thing runs on a DEDICATED WORKER THREAD. CSecureSocket is a CActive state machine that
 * must be driven by a top-level scheduler; the daemon calls us from inside its pump's RunL, where a
 * nested CActiveSchedulerWait panics (measured). A private thread with its own scheduler is the
 * clean fix — this thread just blocks on the worker's logon (thread death completes it directly, no
 * AO dispatch needed) and returns its result. */
int32_t shim_https_get(const uint16_t* host, int32_t hostLen, int32_t port,
                       const uint16_t* path, int32_t pathLen,
                       uint8_t* out, int32_t outCap)
    {
    if (!host || hostLen <= 0 || !out || outCap <= 0)
        return KErrArgument;

    TlsArgs args;
    args.host = host; args.hostLen = hostLen; args.port = port;
    args.path = path; args.pathLen = pathLen;
    args.out = out; args.outCap = outCap; args.result = KErrGeneral;

    RThread thr;
    _LIT(KName, "adbian_tls");
    /* 32 KB stack (TLS record buffers + mbedtls live on it); NULL heap = share this process heap,
     * so the worker's allocations and the shared `out` buffer are the same heap we free from. */
    const TInt KStack = 32 * 1024;
    TInt cr = thr.Create(KName, TlsThreadEntry, KStack, NULL, &args);
    if (cr != KErrNone) { stage("thr_fail"); return -3800 + cr; }

    TRequestStatus logon;
    thr.Logon(logon);
    thr.Resume();
    User::WaitForRequest(logon);

    TExitType et = thr.ExitType();
    TInt exitReason = thr.ExitReason();
    TExitCategoryName cat = thr.ExitCategory();   /* capture BEFORE Close */
    thr.Close();

    if (et == EExitPanic)
        {
        /* Record the panic category+reason to a SEPARATE file so the worker's last stage survives
         * in tlsstage.txt. e.g. "E32USER-CBase 91" pins the fault exactly. */
        RFs fs;
        if (fs.Connect() == KErrNone)
            {
            _LIT(KPan, "C:\\Data\\tlspanic.txt");
            RFile f;
            if (f.Replace(fs, KPan, EFileWrite) == KErrNone)
                {
                TBuf8<80> line;
                line.Copy(cat);
                line.Append(_L8(" reason="));
                line.AppendNum(exitReason);
                f.Write(line);
                f.Close();
                }
            fs.Close();
            }
        return -3900 - exitReason;
        }
    return args.result;
    }

} /* extern "C" */

#endif /* SHIM_USE_TLS */
