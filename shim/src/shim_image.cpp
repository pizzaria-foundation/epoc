/* Image decoding, using Symbian's CImageDecoder.
 *
 * CImageDecoder handles JPEG, PNG, GIF, BMP and anything else the device has a
 * plugin for. It decodes into a CFbsBitmap, and asking for EColor64K gets RGB565 —
 * the same format symbian-gfx draws in — so the pixels can be copied straight into
 * a canvas with no conversion.
 *
 * WHY THIS IS AN ACTIVE OBJECT AND NOT A FUNCTION CALL
 *
 * The obvious shape is Convert() followed by User::WaitForRequest, and it is a trap
 * that costs the whole phone. Convert is asynchronous, and the plugin behind it
 * decodes in slices driven by a self-completing active object *in the calling
 * thread* — that is how the ICL avoids holding the scheduler for the length of a
 * decode. The caller here is rust_step, which the shim invokes from a CIdle, so it
 * is already inside a RunL. Waiting there means the scheduler can never dispatch
 * the decoder's RunL, the request never completes, and the GUI thread is gone: no
 * panic, no log, and the window server goes with it because it is waiting on us.
 *
 * So the decode takes the shape the rest of this shim uses for anything that completes
 * later: a CActive whose RunL pushes an event onto the ring buffer. Nothing blocks, and
 * a slow decode costs frames rather than the device.
 *
 * AND IT COPIES THE TWO SHIPPED NOKIA EXAMPLES, EXACTLY
 *
 * A decode here opened its image, read 240x320 and one frame correctly, created its
 * bitmap, issued Convert — and never completed. Five rounds went into varying one
 * property of the destination at a time (reduced size; then display mode), each on a
 * hypothesis the handset then refuted. The local Symbian v9.3 Developer Library, which
 * is in `vendor/research/s60/s60doc/`, says both of those were legal all along: with
 * EFullyScaleable set "you can specify any size", and with ECanDither set "the
 * destination display mode can be adjusted". So neither was the cause, and varying them
 * was never going to find one.
 *
 * The position this file now takes is that the destination is not a variable. It is
 * whatever `sdk/s60cppexamples/OcrExample/src/ImageHandler.cpp` and
 * `sdk/s60cppexamples/OpenGLEx/Utils/Textureutils.cpp` use, which is the only decode
 * configuration on this machine known to work on this platform:
 * `iFrameCoordsInPixels.Size()`, `EColor16M`, no options, `EPriorityStandard`. The exact
 * fit to the screen happens afterwards in Rust, where there are no codec constraints.
 *
 * IT NEEDS A FONT AND BITMAP SERVER SESSION, AND A GUI APP HIDES THAT
 *
 * Every decode this file did until August 2026 ran inside an application whose CCoeEnv had already
 * connected FBS, so the dependency was satisfied by something nobody here wrote. The first
 * headless caller found it the hard way — see FbsSession below for the measurement. The session is
 * opened here now, which is where it belongs: the facility that needs a server opens it.
 *
 * UNDERFLOW IS NOT A FAILURE
 *
 * Convert may complete with KErrUnderflow meaning "call ContinueConvert", including for
 * a complete image whose codec consumed no bytes on a pass. Neither Nokia example handles
 * it — both treat any non-zero status as terminal — so it is easy to inherit wrong from
 * them. See RunL.
 */

#include "shim_priv.h"

#include <e32std.h>
#include <f32file.h>
#include <fbs.h>
#include <imageconversion.h>

namespace {

/* Four concurrent decodes. A transcript shows a handful of thumbnails at a time and
 * decodes them as they arrive, so the queue is the caller's; this bounds how many
 * CFbsBitmaps and ICL plugin instances can be alive at once, which on a 4 MB heap
 * matters more than parallelism does. */
const TInt KMaxDecodes = 4;

/* Above this the decode is refused rather than attempted. A CFbsBitmap lives in the
 * font and bitmap server's shared heap rather than ours, so it does not show up in
 * our own ceiling — which makes an unbounded decode a way to exhaust a server the
 * whole device shares. 1024x1024 at 16bpp is 2 MB, and nothing this client displays
 * on a 320x240 screen has any reason to be larger. */
const TInt KMaxPixels = 1024 * 1024;

/* How many times a decode may answer KErrUnderflow before it is called stuck.
 *
 * "Call ContinueConvert until it returns KErrNone" is a loop whose termination is the
 * plugin's decision, and the whole source is already in memory here — there is no more
 * data coming, so a codec still asking for some after this many rounds is not making
 * progress. Sixteen is far more than a whole image should ever need and far less than a
 * number that would spin visibly. */
const TInt KMaxContinues = 16;

/* No options, because that is what the working examples pass.
 *
 * This was EOptionAlwaysThread for a while, on the theory that the plugin's own active
 * objects were being starved by this shim's pump — a CIdle at EPriorityIdle that re-arms
 * forever and is therefore permanently ready at that priority. The theory is not silly
 * and the starvation is real for anything at or below EPriorityIdle. But the option
 * changed nothing on the handset, no ICL relay panic (25 or 26) was ever raised, and
 * neither shipped example uses it. An unconfirmed variable is worse than no variable:
 * it costs a thread per decode and it made the next measurement harder to read. */
const CImageDecoder::TOptions KDecodeOptions = CImageDecoder::EOptionNone;

/* The font and bitmap server session, opened on first use.
 *
 * WHY THIS EXISTS, AND WHAT IT COST TO FIND
 *
 * A CFbsBitmap lives in the font and bitmap server's shared heap, so creating one needs a session
 * with that server. A GUI application never asks for it: CCoeEnv connects FBS during its own
 * construction, and every decode this file has ever done happened underneath one. The dependency
 * was therefore real, invisible, and written down nowhere.
 *
 * A headless daemon has no CCoeEnv. Measured on the E72, 25 August 2026: the tile probe fetched a
 * tile (HTTP 200, 30633 bytes, 2759 ms), called Decoder::memory, and the process was gone — no
 * further log line, no report, and no panic.txt, because an FBS client panic is a kernel panic and
 * the Rust handler never runs. The last thing the log shows is the line before the bitmap.
 *
 * Connecting here rather than in the daemon entry keeps the rule this file already follows: the
 * facility that needs a server opens it. RFbsSession::Connect is reference-counted per thread, so
 * in a GUI build this is a second reference on a session CONE already holds and changes nothing.
 *
 * Lazily rather than at startup, for the same reason as shim_file.cpp's RFs: an app that decodes
 * nothing should not hold a session with a server the whole device shares. */
TBool gFbsOpen = EFalse;

TInt FbsSession()
    {
    if (gFbsOpen)
        return KErrNone;
    const TInt err = RFbsSession::Connect();
    /* KErrAlreadyExists is success with a different name: this thread already has a session,
     * which is exactly the GUI case, and treating it as a failure would break the one
     * configuration that was working before this function existed. */
    if (err != KErrNone && err != KErrAlreadyExists)
        return err;
    gFbsOpen = ETrue;
    return KErrNone;
    }

void FbsDisconnect()
    {
    if (!gFbsOpen)
        return;
    /* Balances the Connect above. In a GUI build CONE still holds its own reference, so the
     * session survives this and the framework closes it at the usual time. */
    RFbsSession::Disconnect();
    gFbsOpen = EFalse;
    }

class CShimDecode;
CShimDecode* gSlots[KMaxDecodes];

/* Same handle scheme as shim_file.cpp: slot index in the low 8 bits, generation
 * above it, never zero. A stale handle resolves to NULL and comes back as
 * SHIM_ERR_BAD_HANDLE instead of addressing whatever took the slot. */
TInt gGeneration[KMaxDecodes];

inline TInt32 MakeHandle(TInt aSlot, TInt aGeneration)
    {
    return (TInt32) (((aGeneration + 1) << 8) | (aSlot & 0xFF));
    }

TInt SlotOf(int32_t aHandle)
    {
    if (aHandle == 0)
        return KErrNotFound;
    const TInt slot = aHandle & 0xFF;
    const TInt generation = ((aHandle >> 8) & 0xFFFFFF) - 1;
    if (slot < 0 || slot >= KMaxDecodes)
        return KErrNotFound;
    if (!gSlots[slot] || gGeneration[slot] != generation)
        return KErrNotFound;
    return slot;
    }

TInt FreeSlot()
    {
    for (TInt i = 0; i < KMaxDecodes; i++)
        {
        if (!gSlots[i])
            return i;
        }
    return KErrNotFound;
    }

/* One decode: the plugin, the destination bitmap, and the completion.
 *
 * The decoder is constructed here and destroyed here, and the RFs it was opened
 * against belongs to the shim and outlives it. That ordering is the whole of the
 * previous version's second bug — it closed its own session before deleting the
 * decoder, leaving an RFile subsession on a dead session, which panics on close. */
class CShimDecode : public CActive
    {
public:
    static CShimDecode* NewFileL(TInt aSlot, const TDesC& aPath, TInt aMaxW, TInt aMaxH);
    /* Takes the raw pointer and length, not a TDesC8&, on purpose: the descriptor
     * *object* has to live as long as the decode, so this one builds its own as a
     * member. See iSource. */
    static CShimDecode* NewDataL(TInt aSlot, const TUint8* aData, TInt aLen,
                                 TInt aMaxW, TInt aMaxH);
    ~CShimDecode();

    TInt Width() const { return iSize.iWidth; }
    TInt Height() const { return iSize.iHeight; }
    TBool Done() const { return iDone; }
    TInt Error() const { return iError; }
    TInt CopyOut(TUint16* aOut, TInt aCap) const;
    /* Everything a caller might want to know about a decode that has not finished.
     * Exists because a decode that never completes gives no other evidence at all. */
    void Describe(TInt* aOut, TInt aCap) const;

private:
    CShimDecode(TInt aSlot);
    /* Both entry points converge here once the decoder exists: pick a legal reduced
     * size, create the bitmap at exactly that, and issue the Convert. */
    void StartL(TInt aMaxW, TInt aMaxH);
    void RunL();
    void DoCancel();

    CImageDecoder* iDecoder;
    CFbsBitmap* iBitmap;
    /* The source descriptor, for a decode from memory — a *member*, and that is the
     * point of it.
     *
     * `DataNewL` takes `const TDesC8&` and the plugin keeps the reference; the ICL
     * documentation says the descriptor's validity "depends on the decoder
     * implementation", which is another way of saying a caller must assume it is read
     * for the life of the decode. It was a local in the exported function, so it died
     * the moment that function returned — while Convert was still running. The bytes
     * behind it stayed alive (Rust owns those), but the eight bytes describing them
     * were stack that the next call reused.
     *
     * A decode reading a garbage length does not fail loudly. It waits. */
    TPtrC8 iSource;
    TSize iSize;
    /* The display mode the frame asked for, which the destination bitmap was created in
     * and which CopyOut converts from. */
    TDisplayMode iMode;
    /* Kept for diagnosis: a decode that never completes leaves no other trace. */
    TSize iNative;
    TUint32 iFrameFlags;
    TInt iFactor;
    TInt iFrames;
    /* How many ContinueConvert rounds this decode has needed. Reported, because a decode
     * that needs any is a fact about this codec worth knowing. */
    TInt iContinues;
    TInt iSlot;
    TBool iDone;
    TInt iError;
    };

CShimDecode::CShimDecode(TInt aSlot)
    : CActive(EPriorityStandard), iDecoder(NULL), iBitmap(NULL),
      iMode(EColor64K), iFrameFlags(0),
      iFactor(-1), iFrames(-1), iContinues(0),
      iSlot(aSlot), iDone(EFalse), iError(KErrNone)
    {
    /* EPriorityStandard, and NOT EPriorityIdle, which is what this was and why every
     * decode hung on "decodificando" forever.
     *
     * The pump is a CIdle at EPriorityIdle whose callback returns ETrue, so it
     * re-issues itself on every pass and is *permanently ready* at that priority. The
     * active scheduler runs the highest-priority ready object, and among equals it
     * takes the first in the list. An object added at EPriorityIdle after the pump
     * therefore never runs at all: the pump is always there, always ready, always
     * ahead. Convert() completed and RunL() was simply never dispatched.
     *
     * The reasoning that led to Idle — "a decode is the least urgent thing in the
     * queue" — was about the decode, but this object is not the decode. The work
     * happens inside the ICL plugin's own active objects; all this one does is push
     * one event when they are finished. Starving it does not save any work, it only
     * loses the answer.
     *
     * Which is also why every other active object in this shim is EPriorityStandard:
     * sockets in shim_net.cpp, timers in shim_time.cpp. Networking works on this same
     * pump, and that is the evidence this priority is the correct one. */
    }

CShimDecode::~CShimDecode()
    {
    /* Cancel first. The plugin is writing into iBitmap, so freeing it under a live
     * request is a write to freed memory in a server process's shared heap. */
    Cancel();
    delete iDecoder;
    iDecoder = NULL;
    delete iBitmap;
    iBitmap = NULL;
    }

CShimDecode* CShimDecode::NewFileL(TInt aSlot, const TDesC& aPath, TInt aMaxW, TInt aMaxH)
    {
    CShimDecode* self = new (ELeave) CShimDecode(aSlot);
    CleanupStack::PushL(self);

    RFs* fs = NULL;
    User::LeaveIfError(ShimFsSession(fs));
    self->iDecoder = CImageDecoder::FileNewL(*fs, aPath, KDecodeOptions);
    self->StartL(aMaxW, aMaxH);

    CleanupStack::Pop(self);
    return self;
    }

CShimDecode* CShimDecode::NewDataL(TInt aSlot, const TUint8* aData, TInt aLen,
                                   TInt aMaxW, TInt aMaxH)
    {
    CShimDecode* self = new (ELeave) CShimDecode(aSlot);
    CleanupStack::PushL(self);

    /* The descriptor is built into the member here, and the member is what DataNewL is
     * given. Constructing a TPtrC8 at the call site and passing it in is what broke
     * this: the plugin keeps the reference, and the caller's descriptor was a local.
     *
     * Reading from memory rather than a file is the point of this entry point — a
     * downloaded photo is already in RAM, and writing it out so the codec can read it
     * back is two passes over the flash for nothing. The caller's *bytes* must outlive
     * the decode, which symbian_shim.h says; the descriptor is ours to keep. */
    self->iSource.Set(aData, aLen);

    /* An RFs is still needed — the ICL wants one to find its plugins. */
    RFs* fs = NULL;
    User::LeaveIfError(ShimFsSession(fs));
    self->iDecoder = CImageDecoder::DataNewL(*fs, self->iSource, KDecodeOptions);
    self->StartL(aMaxW, aMaxH);

    CleanupStack::Pop(self);
    return self;
    }

void CShimDecode::StartL(TInt aMaxW, TInt aMaxH)
    {
    iFrames = iDecoder->FrameCount();
    if (iFrames < 1)
        User::Leave(KErrCorrupt);

    const TFrameInfo info = iDecoder->FrameInfo(0);
    const TSize full = info.iOverallSizeInPixels;
    iNative = full;
    iFrameFlags = info.iFlags;
    if (full.iWidth <= 0 || full.iHeight <= 0)
        User::Leave(KErrCorrupt);

    /* THE DESTINATION IS EXACTLY WHAT THE TWO SHIPPED NOKIA EXAMPLES USE.
     *
     * `aMaxW`/`aMaxH` are ignored here, and that is the point. Five rounds were spent
     * varying the destination — reduced size, then display mode — each time on a
     * hypothesis the device then refuted, and each refutation cost a build, a Bluetooth
     * transfer, an install and a person's afternoon. The remaining honest position is
     * that the destination should stop being a variable at all: copy the two examples
     * that are known to work on this platform and vary nothing.
     *
     * `sdk/s60cppexamples/OcrExample/src/ImageHandler.cpp` and
     * `sdk/s60cppexamples/OpenGLEx/Utils/Textureutils.cpp`, both:
     *
     *     iFrameInfo = iDecoder->FrameInfo(aSelectedFrame);
     *     TRect bitmapSize = iFrameInfo.iFrameCoordsInPixels;
     *     iBitmap->Create(bitmapSize.Size(), EColor16M);
     *     iDecoder->Convert(&iStatus, *iBitmap, aSelectedFrame);
     *
     * `iFrameCoordsInPixels` rather than `iOverallSizeInPixels`: for a JPEG they are the
     * same rectangle, and for a GIF sub-frame they are not — using the field the working
     * code uses costs nothing and removes a difference.
     *
     * `EColor16M` rather than EColor64K: the ICL guide says the mode may be chosen when
     * ECanDither is set, so RGB565 was legal, but "legal" is not the same as "what the
     * codec is exercised with". CopyOut converts 24bpp on the way out.
     *
     * The exact fit to the screen happens afterwards, in Rust, with an integer resample —
     * `symbian::image::fit` and `resample`. A 240x320 bitmap at 24bpp is 230 KB in the
     * font and bitmap server's heap, not ours, for the moment it takes to copy out. */
    TRect frameRect = info.iFrameCoordsInPixels;
    TSize dest = frameRect.Size();
    if (dest.iWidth <= 0 || dest.iHeight <= 0)
        dest = full;
    iFactor = 0;
    if (dest.iWidth <= 0 || dest.iHeight <= 0)
        User::Leave(KErrCorrupt);
    if (dest.iWidth * dest.iHeight > KMaxPixels)
        User::Leave(KErrTooBig);

    iMode = EColor16M;
    /* Before the bitmap, not after: Create with no session is the kernel panic described at
     * FbsSession above, and a leave here is an error the caller can read. */
    User::LeaveIfError(FbsSession());
    iBitmap = new (ELeave) CFbsBitmap();
    User::LeaveIfError(iBitmap->Create(dest, iMode));
    iSize = dest;

    CActiveScheduler::Add(this);
    iDecoder->Convert(&iStatus, *iBitmap, 0);
    SetActive();
    }

/* A fixed-order set of numbers the Rust side mirrors. Deliberately a poll rather than
 * a log: the shim has no channel of its own to a host, and a decode that never finishes
 * produces no event, so the only way to learn anything is for the caller to ask. */
void CShimDecode::Describe(TInt* aOut, TInt aCap) const
    {
    const TInt v[] = {
        iDone ? 2 : 1,          /* 0: 1 = still pending, 2 = completed */
        iError,                 /* 1: the completion code, once done */
        iFrames,                /* 2: frames the decoder found; -1 if it never got there */
        iNative.iWidth,         /* 3 */
        iNative.iHeight,        /* 4 */
        iFactor,                /* 5: power-of-two reduction chosen; -1 if never chosen */
        iSize.iWidth,           /* 6: the bitmap actually created */
        iSize.iHeight,          /* 7 */
        IsActive() ? 1 : 0,     /* 8: whether the request is still outstanding */
        (TInt) iMode,           /* 9: the display mode the bitmap was created in */
        (TInt) iFrameFlags,     /* 10: TFrameInfo::iFlags, for ECanDither/EFullyScaleable */
        iContinues,             /* 11: ContinueConvert rounds this decode has needed */
    };
    const TInt n = Min(aCap, (TInt)(sizeof(v) / sizeof(v[0])));
    for (TInt i = 0; i < n; i++)
        aOut[i] = v[i];
    }

void CShimDecode::RunL()
    {
    /* KErrUnderflow is not a failure — it is the codec asking to be called again.
     *
     * The ICL guide is explicit: "as much decoding as possible is undertaken, and
     * Convert() then completes with the error code KErrUnderflow… ContinueConvert()
     * should be used to continue converting when new data arrives. This function should
     * continue to be called until it returns the error code KErrNone rather than
     * KErrUnderflow."
     *
     * Neither shipped Nokia example handles it — both treat any non-zero status as
     * terminal — so it is an easy thing to inherit wrong by copying them, and this side
     * did: the underflow would have surfaced as a final "decode falhou: -10".
     *
     * It can also arrive for a whole, complete image. `CImageDecoderPlugin::
     * HandleProcessFrameResult` says: "if no data was consumed by ProcessFrameL(),
     * HandleProcessFrameResult() assumes that it requires more data and calls
     * RequestComplete(KErrUnderflow)". A codec that consumes nothing on a pass gets an
     * underflow it did not intend, so a client that gives up on the first one gives up
     * on images that would have finished.
     *
     * The whole source is already in hand, so there is nothing to wait for and the
     * continuation is immediate. The cap exists because "call until KErrNone" is a loop
     * whose termination depends on the plugin, and a plugin that returns KErrUnderflow
     * forever would otherwise spin for the life of the process. */
    const TInt status = iStatus.Int();
    if (status == KErrUnderflow && iContinues < KMaxContinues && iDecoder)
        {
        iContinues++;
        iDecoder->ContinueConvert(&iStatus);
        SetActive();
        return;
        }

    iDone = ETrue;
    /* A cap reached is reported as underflow rather than as success: the bitmap holds a
     * partial image, and `EPartialDecodeInvalid` decides whether even that is showable. */
    iError = status;

    /* The size is reported even on failure, because zero would be indistinguishable
     * from a decode that succeeded with nothing in it, and the caller keys its own
     * bookkeeping off the handle rather than off the size. */
    ShimEvent e;
    e.kind = SHIM_EV_IMAGE_DONE;
    e.handle = MakeHandle(iSlot, gGeneration[iSlot]);
    e.status = iError;
    e.a = iSize.iWidth;
    e.b = iSize.iHeight;
    e.c = iContinues;
    e.d = 0;
    e.native = 0;
    ShimPushEvent(e);
    }

void CShimDecode::DoCancel()
    {
    if (iDecoder)
        iDecoder->Cancel();
    }

TInt CShimDecode::CopyOut(TUint16* aOut, TInt aCap) const
    {
    if (!iDone)
        return KErrNotReady;
    if (iError != KErrNone)
        return iError;
    if (!iBitmap)
        return KErrNotReady;

    const TInt w = iSize.iWidth;
    const TInt h = iSize.iHeight;
    if (aCap < w * h)
        return KErrOverflow;

    /* DataAddress is only valid under the heap lock, and it is LockHeap rather than
     * BeginDataAccess because the latter is Symbian^3 and does not exist on FP2 —
     * see docs/device-notes.md, which learned that the hard way.
     *
     * The stride is in BYTES and is not necessarily the width times the depth: the
     * bitmap server aligns rows, so a 41-pixel-wide bitmap is wider than 41 pixels.
     * Copying w*h in one Mem::Copy would shear the image. */
    iBitmap->LockHeap(ETrue);
    const TUint8* base = reinterpret_cast<const TUint8*>(iBitmap->DataAddress());
    const TInt stride = iBitmap->DataStride();

    /* Whatever the codec chose, RGB565 comes out — the canvas has one format. Only the
     * modes an ICL plugin actually recommends are handled; anything else is refused
     * rather than reinterpreted, because a wrong guess here is a picture of noise and
     * the caller cannot tell that from a real image. */
    TInt err = KErrNone;
    for (TInt row = 0; row < h; row++)
        {
        const TUint8* s = base + row * stride;
        TUint16* d = aOut + row * w;
        switch (iMode)
            {
            case EColor64K:
                Mem::Copy(d, s, w * 2);
                break;
            case EColor16M:
                /* 24bpp, byte order blue, green, red. */
                for (TInt x = 0; x < w; x++, s += 3)
                    d[x] = (TUint16)(((s[2] & 0xF8) << 8) | ((s[1] & 0xFC) << 3) | (s[0] >> 3));
                break;
            case EColor16MU:
            case EColor16MA:
                /* 32bpp, 0xAARRGGBB in memory as B, G, R, A on this little-endian core.
                 * Alpha is dropped: the canvas is opaque. */
                for (TInt x = 0; x < w; x++, s += 4)
                    d[x] = (TUint16)(((s[2] & 0xF8) << 8) | ((s[1] & 0xFC) << 3) | (s[0] >> 3));
                break;
            default:
                err = KErrNotSupported;
                row = h;
                break;
            }
        }
    iBitmap->UnlockHeap(ETrue);
    return err;
    }

/* Free a slot and bump its generation, so the handle just released can never
 * validate again. */
void Release(TInt aSlot)
    {
    delete gSlots[aSlot];
    gSlots[aSlot] = NULL;
    gGeneration[aSlot] = (gGeneration[aSlot] + 1) & 0xFFFFFF;
    }

} /* namespace */

#ifdef SHIM_USE_IMAGE
void ShimImageCleanup()
    {
    for (TInt i = 0; i < KMaxDecodes; i++)
        {
        if (gSlots[i])
            Release(i);
        }
    /* After the decodes, never before: every one of them owns a CFbsBitmap that belongs to this
     * session. Same ordering rule as sockets before bearers, and files before their server. */
    FbsDisconnect();
    }
#endif

extern "C" {

int32_t shim_image_probe(const uint16_t* path, int32_t path_len, int32_t* w, int32_t* h)
    {
    if (!path || !w || !h || path_len <= 0)
        return SHIM_ERR_ARGUMENT;
    *w = 0;
    *h = 0;

    RFs* fs = NULL;
    TInt err = ShimFsSession(fs);
    if (err != KErrNone)
        return err;

    TPtrC p(path, path_len);
    CImageDecoder* dec = NULL;
    TRAP(err, dec = CImageDecoder::FileNewL(*fs, p, KDecodeOptions));
    if (err != KErrNone)
        return err;
    /* A NewL that neither left nor returned an object is not success. The previous
     * version returned SHIM_OK here, which handed the caller uninitialised pixels
     * and called them an image. */
    if (!dec)
        return SHIM_ERR_GENERAL;

    if (dec->FrameCount() < 1)
        {
        delete dec;
        return SHIM_ERR_NOT_FOUND;
        }
    const TFrameInfo info = dec->FrameInfo(0);
    *w = info.iOverallSizeInPixels.iWidth;
    *h = info.iOverallSizeInPixels.iHeight;

    /* The decoder goes before the session is anyone else's business — and the session
     * is not closed here at all, because it is the shim's, not ours. */
    delete dec;
    return SHIM_OK;
    }

int32_t shim_image_decode_start(const uint16_t* path, int32_t path_len,
                                int32_t max_w, int32_t max_h, int32_t* handle)
    {
    if (!path || !handle || path_len <= 0)
        return SHIM_ERR_ARGUMENT;
    *handle = 0;

    const TInt slot = FreeSlot();
    if (slot == KErrNotFound)
        return SHIM_ERR_IN_USE;

    TPtrC p(path, path_len);
    CShimDecode* dec = NULL;
    TInt err = KErrNone;
    TRAP(err, dec = CShimDecode::NewFileL(slot, p, max_w, max_h));
    if (err != KErrNone)
        return err;
    if (!dec)
        return SHIM_ERR_GENERAL;

    gSlots[slot] = dec;
    *handle = MakeHandle(slot, gGeneration[slot]);
    return SHIM_OK;
    }

int32_t shim_image_decode_start_mem(const uint8_t* data, int32_t len,
                                    int32_t max_w, int32_t max_h, int32_t* handle)
    {
    if (!data || !handle || len <= 0)
        return SHIM_ERR_ARGUMENT;
    *handle = 0;

    const TInt slot = FreeSlot();
    if (slot == KErrNotFound)
        return SHIM_ERR_IN_USE;

    CShimDecode* dec = NULL;
    TInt err = KErrNone;
    TRAP(err, dec = CShimDecode::NewDataL(slot, data, len, max_w, max_h));
    if (err != KErrNone)
        return err;
    if (!dec)
        return SHIM_ERR_GENERAL;

    gSlots[slot] = dec;
    *handle = MakeHandle(slot, gGeneration[slot]);
    return SHIM_OK;
    }

int32_t shim_image_result(int32_t handle, uint16_t* out, int32_t out_cap,
                          int32_t* w, int32_t* h)
    {
    if (!out || out_cap <= 0 || !w || !h)
        return SHIM_ERR_ARGUMENT;
    const TInt slot = SlotOf(handle);
    if (slot == KErrNotFound)
        return SHIM_ERR_BAD_HANDLE;

    CShimDecode* d = gSlots[slot];
    *w = d->Width();
    *h = d->Height();
    return d->CopyOut(out, out_cap);
    }

int32_t shim_image_describe(int32_t handle, int32_t* out, int32_t cap)
    {
    if (!out || cap <= 0)
        return SHIM_ERR_ARGUMENT;
    const TInt slot = SlotOf(handle);
    if (slot == KErrNotFound)
        return SHIM_ERR_BAD_HANDLE;
    gSlots[slot]->Describe(out, cap);
    return SHIM_OK;
    }

void shim_image_close(int32_t handle)
    {
    const TInt slot = SlotOf(handle);
    if (slot == KErrNotFound)
        return;
    Release(slot);
    }

} /* extern "C" */
