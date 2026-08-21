/* The system clipboard.
 *
 * Symbian has one, and it is not a string in a global — it is a *stream store* on disk, written
 * through a stream dictionary keyed by content type. Which is why this is thirty lines instead of
 * one: putting text on the clipboard means building the same `CPlainText` document an editor would
 * have built, serialising it into the clipboard's store under the UID every Avkon editor looks for,
 * and committing. Writing the characters any other way produces a clipboard that Paste cannot read,
 * with no error anywhere — the paste simply does nothing.
 *
 * Why the SDK wants this at all: an application that will not take a URL on its command line can
 * still be handed one, if the user pastes. That is a worse experience than opening at the address
 * and a far better one than the link doing nothing, and it works for every application on the phone
 * rather than the few that parse a command line.
 *
 * Gated because it costs two imports nothing else here uses — `bafl` for the clipboard and `etext`
 * for the document — and an import that does not resolve makes the whole image fail to load with no
 * panic and no log. Only a binary that opts into USE_CLIPBOARD pays that risk.
 */

#include "shim_priv.h"

#ifdef SHIM_USE_CLIPBOARD

#include <e32std.h>
#include <e32base.h>
#include <f32file.h>
#include <baclipb.h>    /* CClipboard */
#include <txtetext.h>   /* CPlainText */
#include <s32strm.h>

/* Breadcrumb for the clipboard read: overwrite C:\Data\cal\clipstage.txt with the current step,
 * so a KERN-EXEC leaves the last call reached. Diagnostic only. */
static void ClipStage(const char* tag)
    {
    RFs fs;
    if (fs.Connect() != KErrNone) return;
    _LIT(KDir,   "C:\\Data\\cal\\");
    _LIT(KStage, "C:\\Data\\cal\\clipstage.txt");
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

extern "C" {

/* The stream dictionary key Avkon's editors read on Paste: KClipboardUidTypePlainText, from
 * txtetext.h (already included for CPlainText). The value hardcoded here before was wrong
 * (0x10003A69 vs the real 0x10003A1D), which made a paste look up a key nothing writes. */

static void DoSetTextL(RFs& aFs, const TDesC& aText)
    {
    CClipboard* cb = CClipboard::NewForWritingLC(aFs);
    CPlainText* text = CPlainText::NewL();
    CleanupStack::PushL(text);
    text->InsertL(0, aText);
    /* Into the clipboard's own store, under the dictionary key Paste looks up. `CopyToStoreL`
     * writes the stream and registers it; committing is what makes it visible to other processes,
     * and without it the whole thing is discarded when the object dies. */
    text->CopyToStoreL(cb->Store(), cb->StreamDictionary(), 0, text->DocumentLength());
    CleanupStack::PopAndDestroy(text);
    cb->CommitL();
    CleanupStack::PopAndDestroy(cb);
    }

/* The other direction: whatever plain text is on the clipboard, into aOut.
 *
 * `PasteFromStoreL` is the exact counterpart of the `CopyToStoreL` above — same store, same
 * dictionary, same UID — which is what makes this read a clipboard written by any Avkon editor and
 * not only by us. The dictionary is consulted first because an empty clipboard and a clipboard
 * holding something that is not text are the same answer to a caller wanting text, and neither is a
 * failure: KErrNotFound here becomes "nothing to paste" one layer up.
 *
 * `Extract` rather than `Read`: CPlainText stores its characters in segments, and `Read` returns
 * only as far as the end of the current one — which would silently truncate a long paste at a
 * boundary nothing in the API makes visible. */
static void DoGetTextL(RFs& aFs, uint16_t* aOut, TInt aCap, TInt& aLen)
    {
    aLen = 0;
    ClipStage("open");
    CClipboard* cb = CClipboard::NewForReadingLC(aFs);
    ClipStage("opened");
    if (cb->StreamDictionary().At(KClipboardUidTypePlainText) == KNullStreamId)
        User::Leave(KErrNotFound);
    ClipStage("has_text");

    CPlainText* text = CPlainText::NewL();
    CleanupStack::PushL(text);
    ClipStage("newl");
    text->PasteFromStoreL(cb->Store(), cb->StreamDictionary(), 0);
    ClipStage("pasted");

    TInt n = text->DocumentLength();
    if (n > aCap)
        n = aCap;
    if (n > 0)
        {
        TPtr out(reinterpret_cast<TUint16*>(aOut), aCap);
        text->Extract(out, 0, n);
        aLen = out.Length();
        }
    ClipStage("extracted");
    CleanupStack::PopAndDestroy(text);
    CleanupStack::PopAndDestroy(cb);
    }

/* Put `text` (UTF-16, `len` units) on the system clipboard as plain text.
 *
 * Its own file server session: the clipboard is a file, this may be called from a daemon with no
 * session of its own, and holding one open for the life of the process to serve a copy that happens
 * a few times a day is the wrong trade. SHIM_OK once committed. */
int32_t shim_clip_set_text(const uint16_t* text, int32_t len)
    {
    if (!text || len <= 0)
        return SHIM_ERR_ARGUMENT;

    RFs fs;
    TInt rc = fs.Connect();
    if (rc != KErrNone)
        return rc;

    TPtrC des(reinterpret_cast<const TUint16*>(text), len);
    TRAPD(err, DoSetTextL(fs, des));
    fs.Close();
    return err == KErrNone ? SHIM_OK : err;
    }

/* Read the clipboard's plain text into `out` (at most `cap` UTF-16 units), writing the count to
 * `len`. SHIM_ERR_NOT_FOUND when there is nothing to paste — which includes a clipboard that has
 * never been written, since then the file itself is absent and NewForReadingLC leaves.
 *
 * Text longer than `cap` is truncated rather than refused: a paste that delivers the first `cap`
 * characters of a very long clipboard is useful, and the caller sizes the buffer for what its field
 * can hold anyway. */
/* What the reader thread is handed. `out` is the caller's buffer (same process, so any thread may
 * write it); `len`/`rc` come back. */
struct ClipReadArgs
    {
    uint16_t* out;
    int32_t cap;
    int32_t len;
    int32_t rc;
    };

/* Read the clipboard on a private thread. The Avkon clipboard store has been observed to *panic*
 * (a KERN-EXEC, not a leave) rather than fail cleanly on some contents/handsets, and a panic on the
 * GUI thread takes the whole app down with no trace. Isolating the read on its own thread turns
 * that panic into the thread's exit reason, which the caller reports as an error instead of dying.
 * No active scheduler is needed: CClipboard/CPlainText are synchronous. */
static TInt ClipReadThread(TAny* aArg)
    {
    ClipReadArgs* a = (ClipReadArgs*)aArg;
    CTrapCleanup* cleanup = CTrapCleanup::New();
    if (!cleanup) { a->rc = KErrNoMemory; return KErrNoMemory; }
    RFs fs;
    TInt rc = fs.Connect();
    if (rc == KErrNone)
        {
        TInt n = 0;
        TRAPD(err, DoGetTextL(fs, a->out, a->cap, n));
        fs.Close();
        a->len = n;
        a->rc = err;
        }
    else
        a->rc = rc;
    delete cleanup;
    return KErrNone;
    }

int32_t shim_clip_get_text(uint16_t* out, int32_t cap, int32_t* len)
    {
    if (!out || cap <= 0 || !len)
        return SHIM_ERR_ARGUMENT;
    *len = 0;

    ClipReadArgs args;
    args.out = out; args.cap = cap; args.len = 0; args.rc = KErrGeneral;

    RThread thr;
    _LIT(KName, "shim_clip_read");
    /* 32 KB stack; NULL heap = share this process heap so any transient allocation is consistent. */
    TInt cr = thr.Create(KName, ClipReadThread, 32 * 1024, NULL, &args);
    if (cr != KErrNone)
        return cr;

    TRequestStatus st;
    thr.Logon(st);
    thr.Resume();
    User::WaitForRequest(st);
    TExitType et = thr.ExitType();
    TInt reason = thr.ExitReason();
    thr.Close();

    if (et == EExitPanic)
        return -(4000 + reason);   /* the clipboard read panicked; -(4000+reason) names it */
    if (args.rc != KErrNone)
        return args.rc;
    *len = args.len;
    return SHIM_OK;
    }

} // extern "C"

#else /* !SHIM_USE_CLIPBOARD */

extern "C" int32_t shim_clip_set_text(const uint16_t*, int32_t)
    {
    return SHIM_ERR_NOT_SUPPORTED;
    }

extern "C" int32_t shim_clip_get_text(uint16_t*, int32_t, int32_t*)
    {
    return SHIM_ERR_NOT_SUPPORTED;
    }

#endif /* SHIM_USE_CLIPBOARD */
