/* One decode, configured seven different ways, so the handset can say which one works.
 *
 * WHY THIS EXISTS
 *
 * A photo in the Telegram client opens its image correctly — one frame, 240x320, a whole
 * JPEG with both markers — and then `CImageDecoder::Convert` never completes. Five rounds
 * of build, Bluetooth, install and test went into varying one property at a time on a
 * hypothesis the device then refuted. That is the wrong loop, and `docs/device-notes.md`
 * already says so:
 *
 *   "on a platform with no debugger, no console and no log, build the instrument instead
 *    of guessing."
 *
 * So this is the instrument. It runs the whole matrix in one install and writes down what
 * happened for each row, with the raw numbers and the elapsed milliseconds, because
 * "a timeout is a measurement of your deadline, not of the system".
 *
 * It is its own binary for the reason the same file gives: "If a facility might not
 * resolve, it belongs in its own binary, where failing to load costs a probe rather than
 * the report."
 *
 * ROW A IS THE CONTROL
 *
 * It is exactly what the two shipped Nokia examples do —
 * `sdk/s60cppexamples/OcrExample/src/ImageHandler.cpp` and
 * `sdk/s60cppexamples/OpenGLEx/Utils/Textureutils.cpp` — which is the only decode
 * configuration on this machine known to have worked on this platform. Every other row
 * changes exactly one thing from A, so a difference in outcome names its own cause.
 */

/* shim_priv.h rather than symbian_shim.h: this needs `ShimFsSession`, the shim's one file
 * server session. Using it rather than connecting a second one matters for row E — the
 * session is ShareAuto'd, and a session that is not shared is bound to the thread that
 * connected it, so a codec thread touching it panics rather than returning an error. */
#include "shim_priv.h"

#include <e32std.h>
#include <e32base.h>
#include <f32file.h>
#include <fbs.h>
#include <gdi.h>
#include <imageconversion.h>
#include <icl/imagedata.h>

/* The reference JPEG decoder's ECom implementation UID, from
 * `sdk/epoc32/include/icl/icl_uids.hrh` (KJPGDecoderImplementationUidValue).
 *
 * Row G forces it. The E72 almost certainly resolves to a Nokia hardware-accelerated
 * plugin instead — the SDK ships jpegexifplugin, jpegimageframeplugin, jpegyuvdecoder and
 * IclExtJpegApi, which is what that split looks like — and a vendor plugin that calls
 * SetSelfPending() around an accelerator that never signals produces exactly the symptom
 * being chased: no panic, no error, a request outstanding forever. Every other row also
 * reports ImplementationUid(), so which plugin answered is never a guess. */
const TUid KJpegReferenceDecoder = { 0x101F45D7 };

extern "C" {

/* Result slots, in the order the Rust side reads them. Anything not yet known is -1, so
 * "the decode never got that far" is distinguishable from "the value is zero". */
enum TSlot
    {
    ESlotOpenErr = 0,
    ESlotImplUid,
    ESlotFrames,
    ESlotHeaderDone,
    ESlotNativeW,
    ESlotNativeH,
    ESlotFrameW,
    ESlotFrameH,
    ESlotFlags,
    ESlotFrameMode,
    ESlotDestW,
    ESlotDestH,
    ESlotDestMode,
    ESlotPending,      /* 1 while the request is outstanding */
    ESlotStatus,       /* the completion code, once it completes */
    ESlotContinues,
    ESlotFrameState,   /* TFrameInfo::CurrentFrameState() — header vs frame vs complete */
    ESlotCount
    };

} /* extern "C" */

namespace {

const TInt KMaxContinues = 16;

TInt gResult[ESlotCount];

void ResetResults()
    {
    for (TInt i = 0; i < ESlotCount; i++)
        gResult[i] = -1;
    gResult[ESlotContinues] = 0;
    }

class CProbeDecode : public CActive
    {
public:
    static CProbeDecode* NewL(TInt aConfig, const TDesC& aPath,
                              const TUint8* aData, TInt aLen);
    ~CProbeDecode();

private:
    CProbeDecode();
    void StartL(TInt aConfig, const TDesC& aPath, const TUint8* aData, TInt aLen);
    void RunL();
    void DoCancel();

    CImageDecoder* iDecoder;
    CFbsBitmap* iBitmap;
    TPtrC8 iSource;
    TInt iContinues;
    };

CProbeDecode* gRunning = NULL;

CProbeDecode::CProbeDecode()
    : CActive(EPriorityStandard), iDecoder(NULL), iBitmap(NULL), iContinues(0)
    {
    /* EPriorityStandard because both Nokia examples use it, and because the shim's pump is
     * a CIdle at EPriorityIdle that re-arms forever — anything at that priority behind it
     * would never be dispatched at all. */
    }

CProbeDecode::~CProbeDecode()
    {
    Cancel();
    delete iDecoder;
    delete iBitmap;
    }

CProbeDecode* CProbeDecode::NewL(TInt aConfig, const TDesC& aPath,
                                 const TUint8* aData, TInt aLen)
    {
    CProbeDecode* self = new (ELeave) CProbeDecode();
    CleanupStack::PushL(self);
    self->StartL(aConfig, aPath, aData, aLen);
    CleanupStack::Pop(self);
    return self;
    }

void CProbeDecode::StartL(TInt aConfig, const TDesC& aPath,
                          const TUint8* aData, TInt aLen)
    {
    RFs* fs = NULL;
    User::LeaveIfError(ShimFsSession(fs));

    /* The descriptor is a member: DataNewL keeps the reference for the life of the decode,
     * so a local would be dangling the moment this function returned. */
    iSource.Set(aData, aLen);

    const CImageDecoder::TOptions threaded = CImageDecoder::EOptionAlwaysThread;
    switch (aConfig)
        {
        case 4:  /* E: the only row with the threading option */
            iDecoder = CImageDecoder::FileNewL(*fs, aPath, threaded);
            break;
        case 5:  /* F: from memory rather than from a file */
            iDecoder = CImageDecoder::DataNewL(*fs, iSource);
            break;
        case 6:  /* G: force the reference codec instead of whatever ECom prefers */
            iDecoder = CImageDecoder::FileNewL(*fs, aPath, CImageDecoder::EOptionNone,
                                               KNullUid, KNullUid, KJpegReferenceDecoder);
            break;
        default: /* A, B, C, D: plain */
            iDecoder = CImageDecoder::FileNewL(*fs, aPath);
            break;
        }

    gResult[ESlotOpenErr] = KErrNone;
    gResult[ESlotImplUid] = (TInt) iDecoder->ImplementationUid().iUid;
    gResult[ESlotFrames] = iDecoder->FrameCount();
    gResult[ESlotHeaderDone] = iDecoder->IsImageHeaderProcessingComplete() ? 1 : 0;
    if (iDecoder->FrameCount() < 1)
        User::Leave(KErrCorrupt);

    const TFrameInfo info = iDecoder->FrameInfo(0);
    gResult[ESlotNativeW] = info.iOverallSizeInPixels.iWidth;
    gResult[ESlotNativeH] = info.iOverallSizeInPixels.iHeight;
    gResult[ESlotFrameW] = info.iFrameCoordsInPixels.Size().iWidth;
    gResult[ESlotFrameH] = info.iFrameCoordsInPixels.Size().iHeight;
    gResult[ESlotFlags] = (TInt) info.iFlags;
    gResult[ESlotFrameMode] = (TInt) info.iFrameDisplayMode;

    /* The destination. Row A is the examples' choice; each other row differs in one way. */
    TSize dest = info.iFrameCoordsInPixels.Size();
    if (dest.iWidth <= 0 || dest.iHeight <= 0)
        dest = info.iOverallSizeInPixels;
    TDisplayMode mode = EColor16M;

    if (aConfig == 1)        /* B: the overall size instead of the frame rect */
        {
        dest = info.iOverallSizeInPixels;
        }
    else if (aConfig == 2)   /* C: RGB565 instead of 24bpp */
        {
        mode = EColor64K;
        }
    else if (aConfig == 3)   /* D: a power-of-two reduction */
        {
        const TSize want(320, 240);
        TInt factor = iDecoder->ReductionFactor(info.iOverallSizeInPixels, want);
        if (factor > 0)
            {
            TSize reduced;
            if (iDecoder->ReducedSize(info.iOverallSizeInPixels, factor, reduced) == KErrNone)
                dest = reduced;
            }
        }

    if (dest.iWidth <= 0 || dest.iHeight <= 0)
        User::Leave(KErrCorrupt);

    iBitmap = new (ELeave) CFbsBitmap();
    User::LeaveIfError(iBitmap->Create(dest, mode));
    gResult[ESlotDestW] = dest.iWidth;
    gResult[ESlotDestH] = dest.iHeight;
    gResult[ESlotDestMode] = (TInt) mode;

    CActiveScheduler::Add(this);
    gResult[ESlotPending] = 1;
    iDecoder->Convert(&iStatus, *iBitmap, 0);
    SetActive();
    }

void CProbeDecode::RunL()
    {
    const TInt status = iStatus.Int();

    /* KErrUnderflow means "call ContinueConvert", not "this failed" — including for a
     * whole image whose codec consumed nothing on a pass, which the framework turns into
     * an underflow the plugin never asked for. Neither Nokia example handles it. */
    if (status == KErrUnderflow && iContinues < KMaxContinues && iDecoder)
        {
        iContinues++;
        gResult[ESlotContinues] = iContinues;
        iDecoder->ContinueConvert(&iStatus);
        SetActive();
        return;
        }

    gResult[ESlotPending] = 0;
    gResult[ESlotStatus] = status;
    if (iDecoder && iDecoder->FrameCount() > 0)
        gResult[ESlotFrameState] = (TInt) iDecoder->FrameInfo(0).CurrentFrameState();
    }

void CProbeDecode::DoCancel()
    {
    if (iDecoder)
        iDecoder->Cancel();
    }

} /* namespace */

extern "C" {

/* Begin one row. Returns SHIM_OK when the decode was issued; on a leave the error is
 * recorded in slot 0 and returned, which is itself a result worth having — a row that
 * cannot even open is a different finding from one that opens and hangs. */
int32_t imgprobe_start(int32_t config,
                       const uint16_t* path, int32_t path_len,
                       const uint8_t* data, int32_t len)
    {
    if (!path || path_len <= 0 || !data || len <= 0)
        return SHIM_ERR_ARGUMENT;

    delete gRunning;
    gRunning = NULL;
    ResetResults();

    TPtrC p(path, path_len);
    CProbeDecode* d = NULL;
    TInt err = KErrNone;
    TRAP(err, d = CProbeDecode::NewL(config, p, data, len));
    if (err != KErrNone)
        {
        gResult[ESlotOpenErr] = err;
        gResult[ESlotPending] = 0;
        delete d;
        return err;
        }
    gRunning = d;
    return SHIM_OK;
    }

/* Copy the current numbers out. Safe at any time, including while pending — reading a row
 * that has not finished is the whole point of having it. */
int32_t imgprobe_poll(int32_t* out, int32_t cap)
    {
    if (!out || cap <= 0)
        return SHIM_ERR_ARGUMENT;
    const TInt n = Min(cap, (TInt) ESlotCount);
    for (TInt i = 0; i < n; i++)
        out[i] = gResult[i];
    return ESlotCount;
    }

/* Abandon whatever is running. Cancel before delete: a plugin mid-Convert is writing into
 * that bitmap. */
void imgprobe_stop(void)
    {
    delete gRunning;
    gRunning = NULL;
    }

} /* extern "C" */
