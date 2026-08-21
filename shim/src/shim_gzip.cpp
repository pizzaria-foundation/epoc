/* shim_gzip.cpp — read a gzip file a piece at a time, through the platform's own zlib.
 *
 * Why this exists: a feed too large to hold in memory can still be *read* if it is read in pieces,
 * and the version worth downloading is the compressed one. One measured calendar export is 17.4 MB
 * of text and 1.65 MB gzipped — a tenth of the radio time, and the only shape of that file a 2009
 * phone should ever fetch.
 *
 * So the transport writes the compressed body to a file (shim_http_fetch_file) and this hands it
 * back inflated, in chunks the caller sizes. Memory stays flat: one input block, one output buffer,
 * whatever the caller keeps. The file on disk is also the reason a failure here is debuggable — it
 * is still there afterwards, and can be pulled off the phone and inflated on a desk.
 *
 * `libz.dll` is on the device (`docs/device-dump.txt` records it loading, with `inflate` resolving)
 * and its import library ships with the SDK. This is the platform's zlib, not a vendored copy.
 *
 * Deliberately synchronous and handle-based: inflating is CPU work with no asynchronous request
 * behind it, so unlike shim_tls this needs no worker thread and no active scheduler. It is safe to
 * call from a daemon's pump callback.
 */

#include <e32base.h>
#include <f32file.h>
#include <zlib.h>

#include "symbian_shim.h"
#include "shim_priv.h"

namespace {

/* How much compressed input is read from the file at a time. Small enough that the buffer is not
 * worth thinking about, large enough that a megabyte is a couple of hundred reads. */
const TInt KInBuf = 8 * 1024;

/* One open gzip file: the file, the inflate state, and the input block being consumed. */
struct GzFile
    {
    RFs fs;
    RFile file;
    z_stream z;
    TBool zInit;
    TUint8 in[KInBuf];
    TBool eof;          /* the file has no more bytes to give */
    TBool finished;     /* the stream reached its end marker */
    };

/* A small fixed table rather than a growable one: a caller streams one or two files at a time, and
 * a leak of a slot is then visible as a refusal instead of as memory that never comes back. */
const TInt KMaxOpen = 4;
GzFile* gOpen[KMaxOpen] = { NULL, NULL, NULL, NULL };

GzFile* fromHandle(int32_t h)
    {
    if (h < 1 || h > KMaxOpen) return NULL;
    return gOpen[h - 1];
    }

void closeSlot(TInt idx)
    {
    GzFile* g = gOpen[idx];
    if (!g) return;
    if (g->zInit) inflateEnd(&g->z);
    g->file.Close();
    g->fs.Close();
    delete g;
    gOpen[idx] = NULL;
    }

} /* namespace */

extern "C" {

/* Open `path` as a gzip stream. Writes a handle (1-based) and returns 0, or a negative error. */
int32_t shim_gunzip_open(const uint16_t* aPath, int32_t aLen, int32_t* aHandle)
    {
    if (!aPath || aLen <= 0 || !aHandle) return KErrArgument;

    TInt slot = -1;
    for (TInt i = 0; i < KMaxOpen; ++i) if (!gOpen[i]) { slot = i; break; }
    if (slot < 0) return KErrInUse;

    GzFile* g = new GzFile;
    if (!g) return KErrNoMemory;
    g->zInit = EFalse; g->eof = EFalse; g->finished = EFalse;

    TInt rc = g->fs.Connect();
    if (rc != KErrNone) { delete g; return rc; }

    TPtrC path((const TUint16*)aPath, aLen);
    rc = g->file.Open(g->fs, path, EFileRead | EFileShareReadersOnly);
    if (rc != KErrNone) { g->fs.Close(); delete g; return rc; }

    /* 15 + 32: the largest window, and "accept either a gzip or a zlib header". The transport does
     * not promise which one a server sends, and guessing wrong is an unhelpful -3 from inflate. */
    Mem::FillZ(&g->z, sizeof(g->z));
    if (inflateInit2(&g->z, 15 + 32) != Z_OK)
        {
        g->file.Close(); g->fs.Close(); delete g;
        return KErrGeneral;
        }
    g->zInit = ETrue;

    gOpen[slot] = g;
    *aHandle = slot + 1;
    return KErrNone;
    }

/* Inflate up to `aCap` bytes into `aOut`. Returns the byte count, 0 at the end of the stream, or a
 * negative error. A short return is not the end — only 0 is. */
int32_t shim_gunzip_read(int32_t aHandle, uint8_t* aOut, int32_t aCap)
    {
    GzFile* g = fromHandle(aHandle);
    if (!g || !aOut || aCap <= 0) return KErrArgument;
    if (g->finished) return 0;

    g->z.next_out = (Bytef*)aOut;
    g->z.avail_out = (uInt)aCap;

    while (g->z.avail_out > 0)
        {
        if (g->z.avail_in == 0 && !g->eof)
            {
            TPtr8 p(g->in, 0, KInBuf);
            TInt rc = g->file.Read(p);
            if (rc != KErrNone) return rc;
            if (p.Length() == 0) g->eof = ETrue;
            g->z.next_in = (Bytef*)g->in;
            g->z.avail_in = (uInt)p.Length();
            }

        int zr = inflate(&g->z, g->eof ? Z_FINISH : Z_NO_FLUSH);
        if (zr == Z_STREAM_END)
            {
            g->finished = ETrue;
            break;
            }
        if (zr == Z_BUF_ERROR && g->eof)
            {
            /* Nothing more to give and nothing more to want: a truncated stream. Report what was
             * inflated and let the caller decide — a calendar cut short is still readable up to the
             * cut, and the parser drops the half-read event at the end. */
            g->finished = ETrue;
            break;
            }
        if (zr != Z_OK && zr != Z_BUF_ERROR)
            {
            /* -3 (Z_DATA_ERROR) here means the bytes are not what the header claimed. */
            return KErrCorrupt;
            }
        if (zr == Z_OK && g->z.avail_in == 0 && g->eof)
            {
            g->finished = ETrue;
            break;
            }
        }

    return (int32_t)(aCap - (int32_t)g->z.avail_out);
    }

void shim_gunzip_close(int32_t aHandle)
    {
    if (aHandle >= 1 && aHandle <= KMaxOpen) closeSlot(aHandle - 1);
    }

} /* extern "C" */
