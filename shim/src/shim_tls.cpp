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
 * This file is two blocking calls — shim_https_get and shim_http_get — that bring up a bearer, resolve, connect a
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
    /* Only used by the fetch-to-file form. */
    RFs outFs;
    RFile outFile;
    TBool ssOpen, connOpen, sockOpen, fileOpen, fsOpen;
    Conn()
        : ssOpen(EFalse), connOpen(EFalse), sockOpen(EFalse), fileOpen(EFalse), fsOpen(EFalse) {}
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
    if (c.fileOpen) { c.outFile.Close(); c.fileOpen = EFalse; }
    if (c.fsOpen)   { c.outFs.Close(); c.fsOpen = EFalse; }
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
    /* ETrue for HTTPS, EFalse for cleartext HTTP. See shim_http_get for why the second exists. */
    TBool tls;
    /* Set for the fetch-to-file form: where the BODY is written, whether to ask for gzip, and what
     * came back. `out` is unused then — a body that does not fit in memory is the whole point. */
    const uint16_t* file; int32_t fileLen;
    TBool wantGzip;
    int32_t status;      /* HTTP status, filled in by the worker */
    TBool gotGzip;       /* whether the body is gzip-encoded */
    int32_t result;
    };

/* Create (replacing) the file the body is written to. The directory has to exist; the caller owns
 * that, because a shim that creates directories behind a caller's back hides a misconfigured path
 * until the day it matters. */
TInt openFileForWrite(Conn& c, TlsArgs& a)
    {
    TInt rc = c.outFs.Connect();
    if (rc != KErrNone) return rc;
    c.fsOpen = ETrue;
    TPtrC path((const TUint16*)a.file, a.fileLen);
    rc = c.outFile.Replace(c.outFs, path, EFileWrite | EFileStream);
    if (rc != KErrNone) return rc;
    c.fileOpen = ETrue;
    return KErrNone;
    }

/* Read the status code and the content encoding out of the response head. Those two are what the
 * caller cannot recover from the file afterwards; everything else it can. */
void parseHead(const TDesC8& aHead, TlsArgs& a)
    {
    /* "HTTP/1.x 200 OK" — the number after the first space. */
    TInt sp = aHead.Locate(' ');
    if (sp != KErrNotFound && aHead.Length() >= sp + 4)
        {
        TLex8 lex(aHead.Mid(sp + 1, 3));
        TInt code = 0;
        if (lex.Val(code) == KErrNone) a.status = code;
        }
    /* Case-insensitive search for the one header that changes how the file must be read. */
    HBufC8* lower = HBufC8::New(aHead.Length());
    if (lower)
        {
        TPtr8 l = lower->Des();
        l.Copy(aHead);
        l.LowerCase();
        a.gotGzip = (l.Find(_L8("content-encoding: gzip")) != KErrNotFound) ? ETrue : EFalse;
        delete lower;
        }
    }

/* Send and receive over whichever of the two sockets this request is using.
 *
 * The rest of the worker does not care which: the request bytes, the read loop and the error bases
 * are identical, and the only difference is two method calls. A wrapper rather than a copy of the
 * function, because the copy is where the two would drift. */
class Xfer
    {
public:
    Xfer(RSocket& aSock, CSecureSocket* aSec) : iSock(aSock), iSec(aSec) {}
    void Send(const TDesC8& aData, TRequestStatus& aStatus, TSockXfrLength& aLen)
        {
        if (iSec) iSec->Send(aData, aStatus, aLen);
        else      iSock.Send(aData, 0, aStatus, aLen);
        }
    void RecvOneOrMore(TDes8& aBuf, TRequestStatus& aStatus, TSockXfrLength& aLen)
        {
        if (iSec) iSec->RecvOneOrMore(aBuf, aStatus, aLen);
        else      iSock.RecvOneOrMore(aBuf, 0, aStatus, aLen);
        }
private:
    RSocket& iSock;
    CSecureSocket* iSec;
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

    /* Wrap the TCP socket in TLS — unless this is a cleartext request. */
    CSecureSocket* sec = NULL;
    if (a.tls)
        {
        stage("sec_try");
        TInt secErr = KErrNone;
        sec = newSecure(c.sock, secErr);
        if (!sec) { stage("sec_fail"); closeConn(c); delete host8; return -3500 + secErr; }
        stage("sec_new");

        /* Headless: never pop a cert-trust dialog. SNI + cert name check via the domain name. */
        sec->SetOpt(KSoDialogMode, KSolInetSSL, KSSLDialogUnattendedMode);
        sec->SetOpt(KSoSSLDomainName, KSolInetSSL, h8);
        stage("sec_opt");

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
        stage("hs_ok");
        }
    else
        {
        stage("plain");
        }
    Xfer xfer(c.sock, sec);

    /* Build and send the request.
     *
     * The file form asks HTTP/1.0 on purpose: a 1.0 server may not use chunked transfer encoding,
     * so the body is "everything until the peer closes" and this loop needs no de-chunking on the
     * way to disk. Measured against the server that prompted it — 1.0 + gzip answers with neither
     * Content-Length nor chunking, and closes. */
    TBuf8<640> req;
    req.Append(_L8("GET "));
    for (TInt i = 0; i < a.pathLen && a.path; ++i) req.Append((TChar)((const TUint16*)a.path)[i]);
    if (a.pathLen <= 0) req.Append(_L8("/"));
    req.Append(a.file ? _L8(" HTTP/1.0\r\nHost: ") : _L8(" HTTP/1.1\r\nHost: "));
    req.Append(h8);
    req.Append(_L8("\r\nUser-Agent: ADBian"));
    if (a.wantGzip) req.Append(_L8("\r\nAccept-Encoding: gzip"));
    req.Append(_L8("\r\nConnection: close\r\n\r\n"));

    {
    TSockXfrLength sent;
    CAsyncWaiter sw;
    xfer.Send(req, sw.iStatus, sent);
    sw.Await();
    if (sw.Result() != KErrNone)
        {
        stage("send_fail");
        TInt e = sw.Result();
        if (sec) { sec->Close(); delete sec; }
        closeConn(c); delete host8;
        return -3700 + e;
        }
    }
    stage("sent");

    TInt written = 0;
    if (!a.file)
        {
        /* Read the whole response into the caller's buffer, until the peer closes. */
        for (;;)
            {
            TPtr8 p(a.out + written, 0, a.outCap - written);
            TSockXfrLength got;
            CAsyncWaiter rw;
            xfer.RecvOneOrMore(p, rw.iStatus, got);
            rw.Await();
            TInt r = rw.Result();
            if (r == KErrEof) { written += p.Length(); break; }  /* peer closed: keep last bytes */
            if (r != KErrNone) break;                /* error or reset — return what we have */
            written += p.Length();
            if (p.Length() == 0) break;
            if (a.outCap - written <= 0) break;
            }
        }
    else
        {
        /* Straight to disk. The headers are read into a small buffer first — the status and the
         * content encoding are the only things the caller cannot work out from the file — and
         * everything after the blank line is body.
         *
         * Nothing here is bounded by memory: this is the path for a body far larger than the phone
         * could hold, which is exactly why it exists. */
        stage("file_open");
        TInt frc = openFileForWrite(c, a);
        if (frc != KErrNone)
            {
            if (sec) { sec->Close(); delete sec; }
            closeConn(c); delete host8;
            return -4000 + frc;
            }

        stage("file_ok");
        /* On the heap, not in this function's frame. Six kilobytes of stack buffers here cost the
         * TLS handshake — which runs in the same frame, deeper — the room it needs, and the phone
         * answered with KERN-EXEC 3 before the secure socket was even created. A frame is allocated
         * on entry, so a buffer declared in a branch that has not run yet still charges for itself.
         */
        HBufC8* headBuf = HBufC8::New(2048);
        HBufC8* chunkBuf = HBufC8::New(4096);
        if (!headBuf || !chunkBuf)
            {
            delete headBuf; delete chunkBuf;
            if (sec) { sec->Close(); delete sec; }
            closeConn(c); delete host8;
            return KErrNoMemory;
            }
        TPtr8 head = headBuf->Des();
        TBool inBody = EFalse;
        /* A write that fails must not pass for a body that arrived: C: can be full, and a spooled
         * feed missing its middle would parse as a calendar missing its middle. */
        TInt werr = KErrNone;
        for (;;)
            {
            TPtr8 p((TUint8*)chunkBuf->Ptr(), 0, chunkBuf->Des().MaxLength());
            TSockXfrLength got;
            CAsyncWaiter rw;
            xfer.RecvOneOrMore(p, rw.iStatus, got);
            rw.Await();
            TInt r = rw.Result();
            TInt n = p.Length();

            if (n > 0)
                {
                TPtrC8 data((const TUint8*)chunkBuf->Ptr(), n);
                if (!inBody)
                    {
                    /* Accumulate until the blank line. A header block over 2 KB is not something a
                     * server we asked for a calendar should send; treat it as body and move on. */
                    TInt room = head.MaxLength() - head.Length();
                    TInt take = n < room ? n : room;
                    head.Append(data.Left(take));
                    TInt sep = head.Find(_L8("\r\n\r\n"));
                    if (sep != KErrNotFound)
                        {
                        parseHead(head.Left(sep), a);
                        stage("head_done");
                        inBody = ETrue;
                        /* Whatever followed the blank line inside this chunk is the first body. */
                        TInt bodyAt = sep + 4;
                        if (head.Length() > bodyAt)
                            {
                            TPtrC8 first = head.Mid(bodyAt);
                            werr = c.outFile.Write(first);
                            written += first.Length();
                            }
                        if (take < n && werr == KErrNone)
                            {
                            TPtrC8 rest = data.Mid(take);
                            werr = c.outFile.Write(rest);
                            written += rest.Length();
                            }
                        }
                    else if (take < n)
                        {
                        /* Header buffer full and no blank line: give up on parsing and keep bytes. */
                        parseHead(head, a);
                        inBody = ETrue;
                        TPtrC8 rest = data.Mid(take);
                        werr = c.outFile.Write(rest);
                        written += rest.Length();
                        }
                    }
                else
                    {
                    werr = c.outFile.Write(data);
                    written += n;
                    }
                }

            if (werr != KErrNone) break;  /* the disk refused — reported below, not swallowed */
            if (r == KErrEof) break;      /* peer closed: the body is complete */
            if (r != KErrNone) break;     /* error or reset — what reached disk is what we have */
            if (n == 0) break;
            }
        c.outFile.Flush();
        c.outFile.Close();
        c.fileOpen = EFalse;
        delete headBuf;
        delete chunkBuf;
        if (werr != KErrNone)
            {
            stage("write_fail");
            if (sec) { sec->Close(); delete sec; }
            closeConn(c); delete host8;
            return -4100 + werr;
            }
        }
    stage("done");

    if (sec) { sec->Close(); delete sec; }
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
static int32_t GetOverWorker(const uint16_t* host, int32_t hostLen, int32_t port,
                             const uint16_t* path, int32_t pathLen,
                             uint8_t* out, int32_t outCap, TBool aTls,
                             const uint16_t* file = NULL, int32_t fileLen = 0,
                             TBool wantGzip = EFalse, int32_t* statusOut = NULL,
                             int32_t* gzipOut = NULL)
    {
    const TBool toFile = (file != NULL && fileLen > 0);
    if (!host || hostLen <= 0) return KErrArgument;
    if (!toFile && (!out || outCap <= 0)) return KErrArgument;

    TlsArgs args;
    args.host = host; args.hostLen = hostLen; args.port = port;
    args.path = path; args.pathLen = pathLen;
    args.out = out; args.outCap = outCap; args.tls = aTls; args.result = KErrGeneral;
    args.file = NULL; args.fileLen = 0; args.wantGzip = EFalse;
    args.status = 0; args.gotGzip = EFalse;

    RThread thr;
    _LIT(KName, "adbian_tls");
    /* 64 KB stack. The TLS record buffers and the mbedtls port live on it, and 32 KB was enough
     * only while this function's own frame was small — adding two buffers to a branch of it was
     * enough to panic the handset with KERN-EXEC 3 inside CSecureSocket::NewL. The buffers moved to
     * the heap and the stack doubled: the fix and the margin, because the next thing added here
     * should not have to rediscover this.
     *
     * NULL heap = share this process heap, so the worker's allocations and the shared `out` buffer
     * are the same heap we free from. */
    const TInt KStack = 64 * 1024;
    TInt cr = thr.Create(KName, TlsThreadEntry, KStack, NULL, &args);
    if (cr != KErrNone) { stage("thr_fail"); return -3800 + cr; }

    args.file = toFile ? file : NULL;
    args.fileLen = toFile ? fileLen : 0;
    args.wantGzip = wantGzip;

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
    if (statusOut) *statusOut = args.status;
    if (gzipOut) *gzipOut = args.gotGzip ? 1 : 0;
    return args.result;
    }

int32_t shim_https_get(const uint16_t* host, int32_t hostLen, int32_t port,
                       const uint16_t* path, int32_t pathLen,
                       uint8_t* out, int32_t outCap)
    {
    return GetOverWorker(host, hostLen, port, path, pathLen, out, outCap, ETrue);
    }

/* The same GET without TLS. Cleartext, and deliberately so.
 *
 * The case it exists for: a service on the same LAN that trims a calendar feed too big for this
 * phone to read. It cannot be reached over HTTPS, because `CSecureSocket` validates the server's
 * certificate against the device's own store and a 2009 handset has no way to be told to trust a
 * certificate minted this morning for a private address — the handshake answers
 * `KErrSSLInvalidCert` (-7404), which is the correct answer to the question it was asked.
 *
 * So the choice is between cleartext on a network the user controls and no feature at all. The
 * caller decides, per URL, by writing `http://` — never a fallback, because a silent downgrade from
 * https to http is how a secret ends up on the wire. */
int32_t shim_http_get(const uint16_t* host, int32_t hostLen, int32_t port,
                      const uint16_t* path, int32_t pathLen,
                      uint8_t* out, int32_t outCap)
    {
    return GetOverWorker(host, hostLen, port, path, pathLen, out, outCap, EFalse);
    }

/* Fetch straight to a file, optionally asking for gzip. Returns the BODY byte count written, or a
 * negative error; the HTTP status and whether the body is gzip come back through the out params.
 *
 * This is the form for a body the phone cannot hold: one measured calendar export is 17.4 MB of
 * text, 1.65 MB gzipped, and the useful part of it is a few hundred kilobytes. Fetched compressed to
 * disk and then inflated in pieces (shim_gzip.cpp), memory stays flat and the file that arrived is
 * still on the phone afterwards — which on a device with no debugger is most of the diagnosis. */
int32_t shim_http_fetch_file(const uint16_t* host, int32_t hostLen, int32_t port,
                             const uint16_t* path, int32_t pathLen,
                             int32_t tls, int32_t gzip,
                             const uint16_t* filePath, int32_t filePathLen,
                             int32_t* statusOut, int32_t* gzipOut)
    {
    return GetOverWorker(host, hostLen, port, path, pathLen, NULL, 0,
                         tls ? ETrue : EFalse, filePath, filePathLen,
                         gzip ? ETrue : EFalse, statusOut, gzipOut);
    }

} /* extern "C" */

#endif /* SHIM_USE_TLS */
