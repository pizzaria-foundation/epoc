/* HTTP through the platform's own stack (http.dll), asynchronous, safe on the GUI thread.
 *
 * WHY THIS FILE EXISTS BESIDE shim_tls.cpp, WHICH ALSO FETCHES URLS
 *
 * shim_tls.cpp is blocking. It runs the whole fetch inside User::WaitForRequest on a private
 * worker thread, which is correct for a headless one-shot (calsync, tlsprobe) and unusable for
 * anything with a window: the browser must draw a frame while bytes are arriving, and it must be
 * able to cancel a load because the user pressed Back. Those are not features you add to a
 * blocking call.
 *
 * So this file takes the other route, and it is the one the browser is built on. RHTTPSession is
 * a state machine driven by active objects in *this* thread, and it reports progress by calling
 * MHFRunL. That is the same shape as every other asynchronous subsystem in this shim — the
 * completion pushes an event onto the ring buffer and returns — so it composes with the pump
 * instead of fighting it. Nothing here waits.
 *
 * WHAT THE PLATFORM GIVES US, WHICH IS THE WHOLE POINT
 *
 * HTTP/1.1, chunked transfer decoding, connection reuse, cookies and — the one that saves the
 * most code — redirects: thttpevent.h states that for GET and HEAD the transaction is followed
 * automatically, so a 301 costs us nothing. TLS comes the same way: the stack goes through
 * CSecureSocket, and this handset's ssl.dll is patched by the nnproject mbedTLS port to negotiate
 * TLS 1.2. We link none of that and we implement none of it.
 *
 * What we do NOT control is the flip side, and F2 exists to measure it: whose certificates the
 * handset trusts (a 2009 store), what it does with a Content-Encoding it does not know, and how
 * it times out. Hence shim_httpc_info, which reports what actually came back rather than what we
 * hoped would.
 *
 * THE BEARER IS NOT OURS
 *
 * A second RConnection would be a second bearer: on a phone with Wi-Fi and packet data both
 * available, two connections can land on different routes, and the user pays for one of them.
 * shim_net.cpp already brings one up and already owns the prompt-once-and-persist lifecycle, so
 * this file borrows its socket server handle and its RConnection through ShimNetBearer and adds
 * no policy of its own. The caller starts a bearer the usual way and passes the handle here.
 *
 * ONE TRANSACTION AT A TIME
 *
 * Deliberate, and a limit F3 lifts rather than a shape it keeps. A page is 70 requests and they
 * want to be in flight together; but a probe that measures one fetch honestly is worth more than
 * a connection pool measured never, and the slot table this would need is the same table
 * shim_image.cpp and shim_net.cpp already have twice. It goes in once, when there is something
 * to size it against.
 */

#include "shim_priv.h"

#ifdef SHIM_USE_HTTP

#include <e32std.h>
#include <e32base.h>
#include <es_sock.h>
#include <http.h>
#include <uri8.h>
#include <stringpool.h>

namespace {

/* How much UNDRAINED body may pile up here.
 *
 * Not a page-size limit, which is what it was and what made the 294 KB measured page report itself
 * truncated. This buffer holds bytes between the stack handing them over and the caller reading
 * them, and `Read` now drops what has been consumed — so a caller that drains on every body event
 * keeps this at a few kilobytes no matter how big the page is.
 *
 * The number therefore means one thing only: how far behind the caller is allowed to fall. Reaching
 * it is not a large page, it is a caller that stopped reading, and dropping bytes then is the honest
 * outcome — the alternative is growing until the process dies. */
const TInt KBodyHighWater = 1024 * 1024;

/* Response flags reported through shim_httpc_info. Transcribed in symbian-sys. */
const TInt KFlagGzipDeclared    = 1 << 0;  /* Content-Encoding said gzip */
const TInt KFlagChunked         = 1 << 1;  /* Transfer-Encoding said chunked */
const TInt KFlagGzipMagic       = 1 << 2;  /* body starts 1f 8b — the stack did NOT decode it */
const TInt KFlagBodyTruncated   = 1 << 3;  /* the caller fell a whole high-water behind */

/* Also an MHTTPDataSupplier, which is what a POST needs to hand its body to the stack.
 *
 * The two roles live on one object rather than in a helper class because they share the one thing
 * that matters here: the request body must stay alive and unmoved until the transaction ends, and
 * a member of the object that owns the transaction is the shortest way to guarantee that. The same
 * rule bit shim_image.cpp once, where a descriptor local to the exported function died while the
 * decoder was still reading through it. */
class CShimHttp : public CBase, public MHTTPTransactionCallback, public MHTTPDataSupplier
    {
public:
    static CShimHttp* NewL(TInt aServHandle, TAny* aConn);
    ~CShimHttp();

    void GetL(const TDesC8& aUrl, TBool aWantGzip,
              const TDesC8& aIfNoneMatch, const TDesC8& aIfModifiedSince,
              TInt64 aFrom = 0);
    /* One POST with a body already in memory. No streaming: every body this shim sends is a small
     * JSON document, and a supplier that can promise its whole size up front lets the stack send a
     * Content-Length instead of chunking — which is what servers with a strict parser want. */
    void PostL(const TDesC8& aUrl, const TDesC8& aContentType, const TDesC8& aBody);
    void Abort();
    TInt Read(TUint8* aOut, TInt aCap);
    TInt EffectiveUrl(TUint16* aOut, TInt aCap) const;
    TInt Validator(TBool aWantEtag, TUint16* aOut, TInt aCap) const;
    void Info(TInt& aStatus, TInt& aTotal, TInt& aHeld, TInt& aFlags, TInt& aErr) const;

    /* MHTTPTransactionCallback */
    void MHFRunL(RHTTPTransaction aTransaction, const THTTPEvent& aEvent);
    TInt MHFRunError(TInt aError, RHTTPTransaction aTransaction, const THTTPEvent& aEvent);

    /* MHTTPDataSupplier — the request body of a POST. */
    TBool GetNextDataPart(TPtrC8& aDataPart);
    void ReleaseData();
    TInt OverallDataSize();
    TInt Reset();

private:
    CShimHttp();
    void ConstructL(TInt aServHandle, TAny* aConn);
    /* Everything a GET and a POST do identically: clear the response state, open the transaction
     * on the given method, and set the User-Agent. Returns the request's header collection for the
     * caller to add to. Extracted when POST arrived rather than duplicated — two copies of the
     * reset block is how one of them silently stops clearing a field. */
    RHTTPHeaders BeginL(const TDesC8& aUrl, RStringF aMethod);
    void ReadHeadersL();
    void CollectBodyL();
    void Finish(TInt aErr);

    RHTTPSession iSess;
    RHTTPTransaction iTrans;
    TBool iSessOpen;
    TBool iTransOpen;

    /* The POST body, owned here for the life of the transaction. Empty for a GET. */
    RBuf8 iReqBody;
    RBuf8 iBody;        /* what we kept, up to KBodyCap */
    TInt iRead;         /* how much of iBody the caller has drained */
    TInt iTotal;        /* every body byte the stack handed us, kept or dropped */
    TInt iChunks;       /* how many EGotResponseBodyData callbacks it took */
    /* The response's validators, kept so the caller can store them beside the body and make the
     * next request conditional. Bounded: an ETag is short by design and a date is fixed width, so a
     * server sending something enormous is refused rather than allocated for. */
    TBuf8<128> iEtag;
    TBuf8<64> iLastModified;
    /* The body's first two bytes, accumulated across parts. See CollectBodyL. */
    TUint8 iSniff[2];
    TInt iSniffLen;
    TBool iSniffed;
    TInt iStatusCode;   /* the HTTP status, 0 until headers arrive */
    TInt iFlags;
    TInt iErr;          /* the platform error for a failed transaction */
    TBool iDone;
    };

CShimHttp* gHttp = NULL;
/* Byte offset for the next GET, armed by shim_httpc_range_from and consumed by it. See there. */
static TInt64 gRangeFrom = 0;

CShimHttp::CShimHttp()
    : iSessOpen(EFalse), iTransOpen(EFalse), iRead(0), iTotal(0), iChunks(0),
      iSniffLen(0), iSniffed(EFalse), iStatusCode(0), iFlags(0), iErr(KErrNone), iDone(EFalse)
    {
    iSniff[0] = 0;
    iSniff[1] = 0;
    }

CShimHttp* CShimHttp::NewL(TInt aServHandle, TAny* aConn)
    {
    CShimHttp* self = new (ELeave) CShimHttp;
    CleanupStack::PushL(self);
    self->ConstructL(aServHandle, aConn);
    CleanupStack::Pop(self);
    return self;
    }

void CShimHttp::ConstructL(TInt aServHandle, TAny* aConn)
    {
    iBody.CreateL(0);

    iSess.OpenL();
    iSessOpen = ETrue;

    /* Bind the session to the bearer shim_net already brought up.
     *
     * Without these two properties the stack opens its own RConnection, which means a second
     * bearer and — on a phone with a saved access point it has not been told about — the CommsDat
     * prompt appearing over whatever the application was drawing. The socket server goes in as a
     * handle and the connection as a POINTER, which is what the property expects and is why this
     * takes a TAny* rather than something typed: shim_priv.h must not need es_sock.h. */
    RStringPool sp = iSess.StringPool();
    RHTTPConnectionInfo ci = iSess.ConnectionInfo();
    ci.SetPropertyL(sp.StringF(HTTP::EHttpSocketServ, RHTTPSession::GetTable()),
                    THTTPHdrVal(aServHandle));
    ci.SetPropertyL(sp.StringF(HTTP::EHttpSocketConnection, RHTTPSession::GetTable()),
                    THTTPHdrVal(REINTERPRET_CAST(TInt, aConn)));
    }

CShimHttp::~CShimHttp()
    {
    Abort();
    if (iSessOpen)
        {
        iSess.Close();
        iSessOpen = EFalse;
        }
    iBody.Close();
    iReqBody.Close();
    }

void CShimHttp::Abort()
    {
    if (iTransOpen)
        {
        /* Cancel before Close. Close on a live transaction leaves the filters holding a
         * transaction the client has stopped listening to, and the symptom is a callback into a
         * deleted object — which on this platform is a process that vanishes. */
        iTrans.Cancel();
        iTrans.Close();
        iTransOpen = EFalse;
        }
    }

RHTTPHeaders CShimHttp::BeginL(const TDesC8& aUrl, RStringF aMethod)
    {
    Abort();

    iBody.Close();
    iBody.CreateL(0);
    iRead = 0;
    iTotal = 0;
    iChunks = 0;
    iSniffLen = 0;
    iSniffed = EFalse;
    iStatusCode = 0;
    iFlags = 0;
    iErr = KErrNone;
    iDone = EFalse;
    iEtag.Zero();
    iLastModified.Zero();

    TUriParser8 uri;
    User::LeaveIfError(uri.Parse(aUrl));

    RStringPool sp = iSess.StringPool();
    iTrans = iSess.OpenTransactionL(uri, *this, aMethod);
    iTransOpen = ETrue;

    RHTTPHeaders hdr = iTrans.Request().GetHeaderCollection();

    /* A User-Agent, because a server that sees none often answers differently — and a probe whose
     * measurements depend on a header we forgot to send is measuring the wrong thing.
     *
     * It used to be the bare product token `SymbianRustSdk/0.1`, which is honest and useless: no
     * server has ever heard of it, so every site that keeps a light page for small phones served
     * the heavy one instead. What is here now names the handset, and every part of it is true — this
     * really is a Nokia E72 running Symbian OS 9.3 / S60 3.2, and content negotiation for these
     * phones keys on exactly those tokens.
     *
     * What is deliberately NOT here is `AppleWebKit` or `BrowserNG`. Those name an engine, and this
     * is not that engine; claiming one would be asking for markup written for a renderer we do not
     * have. `Mozilla/5.0` stays, which is the ritual every browser including Chrome performs and
     * means nothing to anybody. If a site turns out to serve its heavy page anyway and its light one
     * only to WebKit, that is a measurement to make and then decide on, not a guess to bury here.
     *
     * SDK-wide, so it goes out from every app built on this shim. That is right for the part that
     * describes the device and harmless for the rest: an API client's server does not read it. */
    RStringF ua = sp.OpenFStringL(_L8("Mozilla/5.0 (SymbianOS/9.3; Series60/3.2 NokiaE72-1/031.023;"
            " Profile/MIDP-2.1 Configuration/CLDC-1.1) SymbianRustSdk/0.1"));
    CleanupClosePushL(ua);
    hdr.SetFieldL(sp.StringF(HTTP::EUserAgent, RHTTPSession::GetTable()), THTTPHdrVal(ua));
    CleanupStack::PopAndDestroy(&ua);

    /* An Accept, which exists because of the User-Agent above rather than beside it.
     *
     * Claiming a Symbian handset invites a server to answer in WML — the WAP page it still keeps for
     * these phones — and nothing here can parse WML, so it would arrive as tags rendered as prose.
     * Listing the two HTML types and a wildcard says which of its pages to send: the light *HTML*
     * one. The wildcard is last and unweighted so an image or a stylesheet is still served. */
    RStringF accept = sp.OpenFStringL(_L8("text/html,application/xhtml+xml,*/*"));
    CleanupClosePushL(accept);
    hdr.SetFieldL(sp.StringF(HTTP::EAccept, RHTTPSession::GetTable()), THTTPHdrVal(accept));
    CleanupStack::PopAndDestroy(&accept);

    return hdr;
    }

void CShimHttp::GetL(const TDesC8& aUrl, TBool aWantGzip,
                     const TDesC8& aIfNoneMatch, const TDesC8& aIfModifiedSince,
                     TInt64 aFrom)
    {
    RStringPool sp = iSess.StringPool();
    /* The body of the previous request, if any. Freed here rather than in Abort, because Abort
     * runs from the destructor too and the RBuf8 is closed there anyway. */
    iReqBody.Close();
    RHTTPHeaders hdr = BeginL(aUrl, sp.StringF(HTTP::EGET, RHTTPSession::GetTable()));

    if (aWantGzip)
        {
        /* Ask for gzip and then look at what came back. Two answers are useful and they are
         * different: a Content-Encoding of gzip with a body that starts 1f 8b means the stack
         * handed us the compressed bytes and symbian::zlib has work to do; the same header with a
         * body that does not means the stack decoded it and we get compression for free. This is
         * the single most valuable thing F2 measures, because it decides whether F3 needs an
         * inflate stage at all. */
        hdr.SetFieldL(sp.StringF(HTTP::EAcceptEncoding, RHTTPSession::GetTable()),
                      THTTPHdrVal(sp.StringF(HTTP::EGzip, RHTTPSession::GetTable())));
        }

    /* Conditional request. Either validator alone is enough, and sending both is what RFC 2616
     * says a client with both should do: a server that understands neither simply answers 200 and
     * we are no worse off than an unconditional GET. */
    if (aIfNoneMatch.Length() > 0)
        {
        RStringF v = sp.OpenFStringL(aIfNoneMatch);
        CleanupClosePushL(v);
        hdr.SetFieldL(sp.StringF(HTTP::EIfNoneMatch, RHTTPSession::GetTable()), THTTPHdrVal(v));
        CleanupStack::PopAndDestroy(&v);
        }
    if (aIfModifiedSince.Length() > 0)
        {
        RStringF v = sp.OpenFStringL(aIfModifiedSince);
        CleanupClosePushL(v);
        hdr.SetFieldL(sp.StringF(HTTP::EIfModifiedSince, RHTTPSession::GetTable()),
                      THTTPHdrVal(v));
        CleanupStack::PopAndDestroy(&v);
        }

    /* Resume: ask for everything from aFrom onwards. `Range: bytes=N-` and no end, which is the
     * form for "the rest of it" — the client knows what it has and not what is left.
     *
     * This is what makes a download on 2G survivable. A 320 KB package on this handset drops often
     * enough that restarting from zero means it may never arrive, and the queue writes down how many
     * bytes are already on disk for exactly this call. The answer to a Range request is 206 with the
     * remainder; a server that does not support it answers 200 with the whole thing, which is also
     * correct — the caller compares what it asked for with what it got and truncates its partial file
     * if it has to start over.
     *
     * Set on the request rather than plumbed through a new transaction type because that is all it
     * is: one more header, alongside the two validators above. */
    if (aFrom > 0)
        {
        TBuf8<48> range;
        range.Append(_L8("bytes="));
        range.AppendNum(aFrom);
        range.Append(_L8("-"));
        RStringF v = sp.OpenFStringL(range);
        CleanupClosePushL(v);
        hdr.SetFieldL(sp.StringF(HTTP::ERange, RHTTPSession::GetTable()), THTTPHdrVal(v));
        CleanupStack::PopAndDestroy(&v);
        }

    iTrans.SubmitL();
    }

void CShimHttp::PostL(const TDesC8& aUrl, const TDesC8& aContentType, const TDesC8& aBody)
    {
    RStringPool sp = iSess.StringPool();

    /* The body is copied into a member BEFORE the transaction is opened, and that ordering is the
     * whole safety argument: the stack reads through the pointer this object hands it, at a time
     * of its choosing, long after PostL has returned. A descriptor over the caller's buffer would
     * be a promise Rust never made. */
    iReqBody.Close();
    iReqBody.CreateL(aBody.Length());
    iReqBody.Copy(aBody);

    RHTTPHeaders hdr = BeginL(aUrl, sp.StringF(HTTP::EPOST, RHTTPSession::GetTable()));

    RStringF ct = sp.OpenFStringL(aContentType);
    CleanupClosePushL(ct);
    hdr.SetFieldL(sp.StringF(HTTP::EContentType, RHTTPSession::GetTable()), THTTPHdrVal(ct));
    CleanupStack::PopAndDestroy(&ct);

    /* Naming this object as the body supplier is what turns the transaction into one that sends
     * something. Without it the stack submits a POST with no body, and a server answers 400 to a
     * request that looks, from this side, perfectly well formed. */
    iTrans.Request().SetBody(*this);

    iTrans.SubmitL();
    }

/* --------------------------------------------------------------- MHTTPDataSupplier --
 * One part, all of it, always. The bodies this shim sends are a few hundred bytes of JSON, so
 * there is nothing to stream and no state to keep: GetNextDataPart hands over the whole buffer and
 * says ETrue for "that was the last part".
 *
 * OverallDataSize answering a real number rather than KErrNotFound is what lets the stack send a
 * Content-Length instead of chunking the request. Servers vary in how they feel about a chunked
 * POST, and a length is free here because the body is already whole in memory. */

TBool CShimHttp::GetNextDataPart(TPtrC8& aDataPart)
    {
    aDataPart.Set(iReqBody);
    return ETrue;
    }

void CShimHttp::ReleaseData()
    {
    /* Nothing to release: the buffer is a member and outlives the transaction by construction.
     * Freeing it here would be freeing it under a retry the stack might still do. */
    }

TInt CShimHttp::OverallDataSize()
    {
    return iReqBody.Length();
    }

TInt CShimHttp::Reset()
    {
    /* The stack asking to send the body again — a redirect or an authentication round. The whole
     * buffer is still here, and GetNextDataPart is stateless, so there is genuinely nothing to
     * undo. */
    return KErrNone;
    }

/* Copy one response header into a caller descriptor, if the server sent it.
 *
 * Both come back as raw text rather than pooled strings: an ETag is an opaque quoted token the
 * server invents and a date is a formatted string, so neither is in the string table and comparing
 * them as pooled values would fail for every real server. */
static void ReadRawField(RHTTPHeaders aHdr, RStringPool aPool, TInt aField, TDes8& aOut)
    {
    THTTPHdrVal val;
    if (aHdr.GetField(aPool.StringF(aField, RHTTPSession::GetTable()), 0, val) != KErrNone)
        return;

    TPtrC8 text;
    if (val.Type() == THTTPHdrVal::KStrVal)
        text.Set(val.Str().DesC());
    else if (val.Type() == THTTPHdrVal::KStrFVal)
        text.Set(val.StrF().DesC());
    else
        return;

    /* Truncated rather than dropped: a validator too long for the buffer is still better used as
     * nothing than as a prefix, so the length check refuses it outright. A prefix would be sent
     * back as If-None-Match and match nothing forever, which is a cache that never hits and never
     * says why. */
    if (text.Length() > 0 && text.Length() <= aOut.MaxLength())
        aOut.Copy(text);
    }

void CShimHttp::ReadHeadersL()
    {
    RHTTPResponse resp = iTrans.Response();
    iStatusCode = resp.StatusCode();

    RStringPool sp = iSess.StringPool();
    RHTTPHeaders hdr = resp.GetHeaderCollection();
    THTTPHdrVal val;

    if (hdr.GetField(sp.StringF(HTTP::EContentEncoding, RHTTPSession::GetTable()), 0, val)
        == KErrNone)
        {
        /* The field may come back as a pooled string or as raw text depending on whether the
         * stack recognised the token, and only one of those two forms compares equal to EGzip —
         * so both are checked. A probe that missed gzip because of the representation would
         * report "no compression" about a server that compressed. */
        if (val.Type() == THTTPHdrVal::KStrFVal)
            {
            if (val.StrF().Index(RHTTPSession::GetTable()) == HTTP::EGzip)
                iFlags |= KFlagGzipDeclared;
            }
        else if (val.Type() == THTTPHdrVal::KStrVal)
            {
            if (val.Str().DesC().CompareF(_L8("gzip")) == 0)
                iFlags |= KFlagGzipDeclared;
            }
        }

    if (hdr.GetField(sp.StringF(HTTP::ETransferEncoding, RHTTPSession::GetTable()), 0, val)
        == KErrNone)
        {
        iFlags |= KFlagChunked;
        }

    ReadRawField(hdr, sp, HTTP::EETag, iEtag);
    ReadRawField(hdr, sp, HTTP::ELastModified, iLastModified);

    ShimPushSimple(SHIM_EV_HTTP_HEAD, 0, KErrNone, iStatusCode);
    }

void CShimHttp::CollectBodyL()
    {
    MHTTPDataSupplier* body = iTrans.Response().Body();
    if (!body)
        return;

    TPtrC8 chunk;
    body->GetNextDataPart(chunk);
    iChunks++;
    iTotal += chunk.Length();

    /* The first two bytes of the BODY answer the gzip question — not the first two bytes of the
     * first chunk, which is what this used to read and is a false negative waiting to happen.
     *
     * A body arrives in as many parts as the stack feels like: one measured page came in 95 of
     * them. Nothing says the first one holds two bytes, and the old test (`iBody.Length() == 0 &&
     * chunk.Length() >= 2`) skipped the check entirely once anything had been buffered — so a
     * one-byte opening part meant the magic was never looked for again. The flag would read "not
     * compressed" about a body that was, and `needs_inflate` would then hand deflate bytes to a
     * caller expecting HTML. A false negative here is silent corruption, which is why it is worth
     * two bytes of state instead of a cheaper test. */
    for (TInt i = 0; i < chunk.Length() && iSniffLen < 2; i++)
        iSniff[iSniffLen++] = chunk[i];
    if (iSniffLen == 2 && !iSniffed)
        {
        iSniffed = ETrue;
        if (iSniff[0] == 0x1f && iSniff[1] == 0x8b)
            iFlags |= KFlagGzipMagic;
        }

    const TInt held = iBody.Length() - iRead;
    const TInt room = KBodyHighWater - held;
    if (room > 0)
        {
        const TInt take = (chunk.Length() < room) ? chunk.Length() : room;
        iBody.ReAllocL(iBody.Length() + take);
        iBody.Append(chunk.Left(take));
        if (take < chunk.Length())
            iFlags |= KFlagBodyTruncated;
        }
    else if (chunk.Length() > 0)
        {
        iFlags |= KFlagBodyTruncated;
        }

    /* ReleaseData is what asks for the next part. Forgetting it is a transaction that stops after
     * one chunk and never reports an error — it simply never completes. */
    body->ReleaseData();

    ShimPushSimple(SHIM_EV_HTTP_BODY, 0, KErrNone, iTotal);
    }

void CShimHttp::Finish(TInt aErr)
    {
    if (iDone)
        return;
    iDone = ETrue;
    iErr = aErr;

    ShimEvent e;
    e.kind = SHIM_EV_HTTP_DONE;
    e.handle = 0;
    e.status = aErr;
    e.a = iStatusCode;
    e.b = iTotal;
    e.c = iFlags;
    e.d = iChunks;
    e.native = aErr;
    ShimPushEvent(e);
    }

void CShimHttp::MHFRunL(RHTTPTransaction /*aTransaction*/, const THTTPEvent& aEvent)
    {
    switch (aEvent.iStatus)
        {
        case THTTPEvent::EGotResponseHeaders:
            ReadHeadersL();
            break;

        case THTTPEvent::EGotResponseBodyData:
            CollectBodyL();
            break;

        case THTTPEvent::EResponseComplete:
            /* The body is finished; the transaction is not. ESucceeded still has to arrive, and
             * completing here would report a result while the stack still owns the transaction. */
            break;

        case THTTPEvent::ESucceeded:
            Finish(KErrNone);
            break;

        case THTTPEvent::EFailed:
            /* EFailed carries no code of its own, so KErrGeneral is the honest answer here — a
             * TLS or DNS failure arrives instead as a NEGATIVE iStatus and keeps its code. That
             * distinction is what makes an untrusted certificate diagnosable (R7) rather than
             * showing up as a generic failure. */
            Finish(KErrGeneral);
            break;

        default:
            if (aEvent.iStatus < 0)
                Finish(aEvent.iStatus);
            break;
        }
    }

TInt CShimHttp::MHFRunError(TInt aError, RHTTPTransaction /*aTransaction*/,
                            const THTTPEvent& /*aEvent*/)
    {
    /* A leave inside MHFRunL lands here. Returning KErrNone means "handled": the framework must
     * not tear anything down behind us, because the caller still has to read the result. */
    Finish(aError);
    return KErrNone;
    }

TInt CShimHttp::Read(TUint8* aOut, TInt aCap)
    {
    if (!aOut || aCap <= 0)
        return KErrArgument;
    const TInt left = iBody.Length() - iRead;
    if (left <= 0)
        return 0;
    const TInt take = (left < aCap) ? left : aCap;
    Mem::Copy(aOut, iBody.Ptr() + iRead, take);
    iRead += take;

    /* Drop what the caller has taken.
     *
     * Without this the buffer is the whole body: `iRead` advanced and nothing was ever released, so
     * a 294 KB page held 294 KB here until the transaction ended — on a handset with about 45 MB
     * free, while the inflated copy and the DOM are also being built. Compacting here is what turns
     * this from a buffer of the response into a buffer of the *backlog*.
     *
     * Only when the caller has drained everything, so the common case is one Delete of the whole
     * thing rather than a memmove per read. */
    if (iRead == iBody.Length())
        {
        iBody.Delete(0, iRead);
        iRead = 0;
        }
    return take;
    }

TInt CShimHttp::EffectiveUrl(TUint16* aOut, TInt aCap) const
    {
    /* The request's URI, not the one the caller asked for.
     *
     * That is the whole point: the platform's redirect filter follows a 301 for GET by calling
     * SetURIL on this same request, so after the transaction ends this reads back where the bytes
     * actually came from. A browser needs it and cannot compute it — every relative link on the page
     * resolves against this, and `http://google.com/` answers with the content of
     * `https://www.google.com/`. Resolving links against what was typed would point them at the
     * wrong host, on the wrong scheme, and the failure would look like a broken site.
     *
     * ASCII out of an 8-bit descriptor, widened here because every string crossing this shim is
     * UTF-16. A percent-encoded URI is ASCII by construction, so nothing is lost. */
    if (!iTransOpen || !aOut || aCap <= 0)
        return KErrNotReady;

    const TDesC8& uri = iTrans.Request().URI().UriDes();
    const TInt len = uri.Length();
    if (len > aCap)
        return KErrOverflow;
    for (TInt i = 0; i < len; i++)
        aOut[i] = static_cast<TUint16>(uri[i]);
    return len;
    }

TInt CShimHttp::Validator(TBool aWantEtag, TUint16* aOut, TInt aCap) const
    {
    if (!aOut || aCap <= 0)
        return KErrArgument;
    const TDesC8& src = aWantEtag ? (const TDesC8&)iEtag : (const TDesC8&)iLastModified;
    const TInt len = src.Length();
    if (len > aCap)
        return KErrOverflow;
    for (TInt i = 0; i < len; i++)
        aOut[i] = static_cast<TUint16>(src[i]);
    return len;
    }

void CShimHttp::Info(TInt& aStatus, TInt& aTotal, TInt& aHeld, TInt& aFlags, TInt& aErr) const
    {
    aStatus = iStatusCode;
    aTotal = iTotal;
    aHeld = iBody.Length();
    aFlags = iFlags;
    aErr = iErr;
    }

} /* namespace */

void ShimHttpCleanup()
    {
    delete gHttp;
    gHttp = NULL;
    }

extern "C" {

/* Open the HTTP session over an already-started bearer. `net` is a handle from shim_net_start. */
int32_t shim_httpc_open(int32_t net)
    {
    if (gHttp)
        return SHIM_OK;

    TInt serv = 0;
    TAny* conn = NULL;
    const TInt err = ShimNetBearer(net, serv, conn);
    if (err != KErrNone)
        return err;

    CShimHttp* h = NULL;
    TRAPD(trapped, h = CShimHttp::NewL(serv, conn));
    if (trapped != KErrNone)
        return trapped;
    gHttp = h;
    return SHIM_OK;
    }

/* Narrow UTF-16 with ASCII content into an 8-bit buffer, refusing anything above ASCII.
 *
 * Refusing rather than mangling, because these are a URL and two validators: a mangled URL fetches
 * the wrong page, and a mangled ETag matches nothing forever, which is a cache that never hits and
 * never says why. */
static TInt Narrow(const uint16_t* in, TInt len, TDes8& out)
    {
    if (len > out.MaxLength())
        return KErrOverflow;
    out.Zero();
    for (TInt i = 0; i < len; i++)
        {
        const uint16_t c = in[i];
        if (c == 0 || c > 0x7f)
            return KErrArgument;
        out.Append(static_cast<TChar>(c));
        }
    return KErrNone;
    }

/* Start a GET, optionally conditional. Completion arrives as SHIM_EV_HTTP_DONE.
 *
 * `if_none_match` and `if_modified_since` may be NULL or empty, which is an unconditional request.
 * Given either, a server that agrees the copy is current answers 304 with no body — which is the
 * whole value: the round trip still happens, so the answer is current, and the page does not. */
int32_t shim_httpc_get_cond(const uint16_t* url, int32_t len, int32_t want_gzip,
                            const uint16_t* if_none_match, int32_t inm_len,
                            const uint16_t* if_modified_since, int32_t ims_len)
    {
    if (!gHttp)
        return SHIM_ERR_NOT_READY;
    if (!url || len <= 0 || len > 1024)
        return KErrArgument;

    HBufC8* narrow = HBufC8::New(len);
    if (!narrow)
        return KErrNoMemory;
    TPtr8 p = narrow->Des();
    TInt err = Narrow(url, len, p);
    if (err != KErrNone)
        {
        delete narrow;
        return err;
        }

    TBuf8<128> inm;
    TBuf8<64> ims;
    if (if_none_match && inm_len > 0)
        {
        err = Narrow(if_none_match, inm_len, inm);
        if (err != KErrNone) { delete narrow; return err; }
        }
    if (if_modified_since && ims_len > 0)
        {
        err = Narrow(if_modified_since, ims_len, ims);
        if (err != KErrNone) { delete narrow; return err; }
        }

    TRAPD(trapped, gHttp->GetL(p, want_gzip != 0, inm, ims, gRangeFrom));
    gRangeFrom = 0;
    delete narrow;
    return (trapped == KErrNone) ? SHIM_OK : trapped;
    }

/* The unconditional form. */
int32_t shim_httpc_get(const uint16_t* url, int32_t len, int32_t want_gzip)
    {
    return shim_httpc_get_cond(url, len, want_gzip, NULL, 0, NULL, 0);
    }

/* Resume the next GET from a byte offset.
 *
 * A one-shot that arms the following `shim_httpc_get*` and clears itself, rather than a parameter on
 * every entry point. Two reasons: the existing signatures have four callers in this workspace and
 * widening them all for something one of them wants is churn, and a resume is genuinely a property of
 * *this* request rather than of the session. Cleared even when the GET fails, so a refused resume
 * cannot leak into an unrelated fetch later — which would be a download that silently started in the
 * middle. */
int32_t shim_httpc_range_from(int64_t offset)
    {
    if (offset < 0)
        return KErrArgument;
    gRangeFrom = offset;
    return SHIM_OK;
    }

int32_t shim_httpc_post(const uint16_t* url, int32_t len,
                        const uint8_t* content_type, int32_t ct_len,
                        const uint8_t* body, int32_t body_len)
    {
    if (!gHttp)
        return SHIM_ERR_NOT_READY;
    if (!url || len <= 0 || len > 1024)
        return KErrArgument;
    if (!content_type || ct_len <= 0 || ct_len > 128)
        return KErrArgument;
    /* The body arrives as bytes, not UTF-16: a JSON document is already the caller's business to
     * encode, and narrowing one here would be this file guessing at somebody else's charset.
     * 64 KB is far past any request this shim exists to send. */
    if (!body || body_len < 0 || body_len > 64 * 1024)
        return KErrArgument;

    HBufC8* narrow = HBufC8::New(len);
    if (!narrow)
        return KErrNoMemory;
    TPtr8 p = narrow->Des();
    TInt err = Narrow(url, len, p);
    if (err != KErrNone)
        {
        delete narrow;
        return err;
        }

    TPtrC8 ct(content_type, ct_len);
    TPtrC8 payload(body, body_len);
    /* Both descriptors are read inside PostL and copied there — see the note at the top of it.
     * Neither outlives this function, which is why neither may be handed to the stack directly. */
    TRAPD(trapped, gHttp->PostL(p, ct, payload));
    delete narrow;
    return (trapped == KErrNone) ? SHIM_OK : trapped;
    }

/* The response's ETag (`want_etag` non-zero) or Last-Modified, as UTF-16 units.
 *
 * Read after SHIM_EV_HTTP_HEAD and before the next GET. Zero means the server sent none, which is
 * a page that cannot be revalidated — a fact the caller has to know rather than guess. */
int32_t shim_httpc_validator(int32_t want_etag, uint16_t* out, int32_t cap)
    {
    if (!gHttp)
        return SHIM_ERR_NOT_READY;
    return gHttp->Validator(want_etag != 0, out, cap);
    }

/* Drain buffered body bytes. Returns the count copied, 0 when nothing is held. */
int32_t shim_httpc_read(uint8_t* out, int32_t cap)
    {
    if (!gHttp)
        return SHIM_ERR_NOT_READY;
    return gHttp->Read(out, cap);
    }

/* What the response actually was. Every out param is optional. */
int32_t shim_httpc_info(int32_t* status, int32_t* total, int32_t* held,
                        int32_t* flags, int32_t* err)
    {
    if (!gHttp)
        return SHIM_ERR_NOT_READY;
    TInt s = 0, t = 0, h = 0, f = 0, e = 0;
    gHttp->Info(s, t, h, f, e);
    if (status) *status = s;
    if (total) *total = t;
    if (held) *held = h;
    if (flags) *flags = f;
    if (err) *err = e;
    return SHIM_OK;
    }

/* Where the bytes actually came from, after any redirect the stack followed silently.
 *
 * Read it after SHIM_EV_HTTP_DONE and before the next GET, which replaces the transaction. Returns
 * the number of UTF-16 units written, or a negative error. */
int32_t shim_httpc_url(uint16_t* out, int32_t cap)
    {
    if (!gHttp)
        return SHIM_ERR_NOT_READY;
    return gHttp->EffectiveUrl(out, cap);
    }

/* Abandon the transaction in flight, keeping the session. This is Back being pressed. */
int32_t shim_httpc_cancel(void)
    {
    if (!gHttp)
        return SHIM_ERR_NOT_READY;
    gHttp->Abort();
    return SHIM_OK;
    }

void shim_httpc_close(void)
    {
    ShimHttpCleanup();
    }

} /* extern "C" */

#endif /* SHIM_USE_HTTP */
