#include "hello.h"

#include <eikstart.h>
#include <eikenv.h>
#include <coemain.h>
#include <w32std.h>
#include <fbs.h>
#include <bitdev.h>
#include <bitstd.h>
#include <gdi.h>
#include <e32math.h>
#include <f32file.h>

/* ------------------------------------------------------------------- trace --
 *
 * The device gives us nothing to debug with: no console, no log viewer, and when
 * the loader refuses an image the application shell swallows the error and simply
 * does nothing. So the app narrates its own startup into a file.
 *
 * Read C:\Data\rustsdk.txt afterwards with the file manager:
 *   - file absent entirely  -> the loader never started the process, so the
 *                              problem is the E32 image itself, not this code
 *   - file stops at a step  -> that step is where it died
 *
 * Deliberately dependency-free: RFs and RFile only, no cleanup stack, no leaves,
 * nothing that could itself be the thing that fails. */
LOCAL_C void Trace(const TDesC8& aStep)
    {
    RFs fs;
    if (fs.Connect() != KErrNone)
        return;
    _LIT(KDir, "C:\\Data\\");
    _LIT(KPath, "C:\\Data\\rustsdk.txt");
    fs.MkDirAll(KDir);          // harmless if it already exists

    RFile f;
    TInt err = f.Open(fs, KPath, EFileWrite | EFileShareAny);
    if (err == KErrNotFound)
        err = f.Create(fs, KPath, EFileWrite | EFileShareAny);
    if (err == KErrNone)
        {
        TInt pos = 0;
        f.Seek(ESeekEnd, pos);
        f.Write(aStep);
        f.Write(_L8("\r\n"));
        f.Flush();
        f.Close();
        }
    fs.Close();
    }

/* ---------------------------------------------------------------- control -- */

CHelloControl::CHelloControl()
    : iBack(NULL), iBackDev(NULL), iBackGc(NULL), iStage(NULL),
      iLastCode(0), iLastScan(0), iFrame(0)
    {
    }

CHelloControl::~CHelloControl()
    {
    delete[] iStage;
    delete iBackGc;
    delete iBackDev;
    delete iBack;
    }

CHelloControl* CHelloControl::NewL(const TRect& aRect)
    {
    CHelloControl* self = new (ELeave) CHelloControl;
    CleanupStack::PushL(self);
    self->ConstructL(aRect);
    CleanupStack::Pop(self);
    return self;
    }

void CHelloControl::ConstructL(const TRect& aRect)
    {
    Trace(_L8("5-control ConstructL"));
    CreateWindowL();
    Trace(_L8("6-window created"));
    SetRect(aRect);

    /* Never hardcode the display mode: ask. If the back buffer's mode differs
     * from the screen's, BitBlt silently does a per-pixel colour conversion in
     * the window server, which is the difference between a few milliseconds a
     * frame and tens of them. */
    CWsScreenDevice* screen = iCoeEnv->ScreenDevice();
    iFacts.iMode = screen->DisplayMode();

    Trace(_L8("7-display mode read"));
    CreateBackBufferL(aRect.Size());
    Trace(_L8("8-back buffer created"));
    ProbePixelLayoutL();
    Trace(_L8("9-pixel probe done"));

    /* Ask the window server for our mode so it has no reason to convert. */
    Window().SetRequiredDisplayMode((TDisplayMode) iFacts.iMode);
    ActivateL();
    Trace(_L8("10-activated"));
    RenderL();
    Trace(_L8("11-first render done"));
    }

void CHelloControl::CreateBackBufferL(const TSize& aSize)
    {
    delete iBackGc;  iBackGc = NULL;
    delete iBackDev; iBackDev = NULL;
    delete iBack;    iBack = NULL;

    iBack = new (ELeave) CFbsBitmap;
    User::LeaveIfError(iBack->Create(aSize, (TDisplayMode) iFacts.iMode));
    iBackDev = CFbsBitmapDevice::NewL(iBack);
    User::LeaveIfError(iBackDev->CreateContext(iBackGc));

    delete[] iStage;
    iStage = new (ELeave) TUint16[aSize.iWidth * aSize.iHeight];

    iFacts.iStride = iBack->DataStride();
    iFacts.iBpp = iFacts.iStride * 8 / (aSize.iWidth ? aSize.iWidth : 1);
    }

void CHelloControl::ProbePixelLayoutL()
    {
    /* Paint a single pixel pure red through the documented TRgb API, then read the
     * bytes back. Turns "which byte is red, and is this 16 or 32 bits per pixel?"
     * from a guess into a measurement, on whatever device this happens to run. */
    CFbsBitmap* probe = new (ELeave) CFbsBitmap;
    CleanupStack::PushL(probe);
    User::LeaveIfError(probe->Create(TSize(1, 1), (TDisplayMode) iFacts.iMode));

    CFbsBitmapDevice* dev = CFbsBitmapDevice::NewL(probe);
    CleanupStack::PushL(dev);
    CFbsBitGc* gc = NULL;
    User::LeaveIfError(dev->CreateContext(gc));
    CleanupStack::PushL(gc);

    gc->SetBrushColor(TRgb(255, 0, 0));
    gc->SetBrushStyle(CGraphicsContext::ESolidBrush);
    gc->Clear();

    /* Bracketing DataAddress with LockHeap/UnlockHeap is mandatory: unlocked, the
     * call can crash, and the pointer is only valid until UnlockHeap because the
     * font and bitmap server's heap may compact. Never cache it.
     *
     * On this SDK the pair is LockHeap/UnlockHeap. BeginDataAccess/EndDataAccess,
     * which most modern documentation names, does not exist in S60 3rd FP2 — it
     * arrived with Symbian^3. */
    probe->LockHeap();
    iFacts.iRedWord = *probe->DataAddress();
    probe->UnlockHeap();

    CleanupStack::PopAndDestroy(3); // gc, dev, probe
    }

/* Small helpers so RenderL stays readable. Both write RGB565 words; if the device
 * turns out to be 32bpp the render below takes the CFbsBitGc path instead. */
static inline TUint16 Rgb565(TInt r, TInt g, TInt b)
    {
    return (TUint16) (((r & 0xF8) << 8) | ((g & 0xFC) << 3) | (b >> 3));
    }

void CHelloControl::RenderL()
    {
    if (!iBack || !iStage)
        return;

    const TSize size = iBack->SizeInPixels();

    /* Step 1: render into a 16bpp staging buffer. This is exactly what the Rust
     * rasterizer does — it owns a plain RGB565 buffer and knows nothing about
     * Symbian. Half the memory traffic of drawing straight into a 32bpp surface,
     * which matters because a UI touches most pixels more than once. */
    for (TInt y = 0; y < size.iHeight; y++)
        {
        TUint16* row = iStage + y * size.iWidth;
        for (TInt x = 0; x < size.iWidth; x++)
            {
            /* A gradient plus a moving diagonal, so a stuck frame is obvious. */
            TInt r = (x * 255) / (size.iWidth - 1);
            TInt b = (y * 255) / (size.iHeight - 1);
            TInt g = ((x + y + iFrame * 4) & 63) * 4;
            row[x] = Rgb565(r, g, b);
            }
        }

    /* Step 2: expand into the bitmap the window server will blit. The E72 reports
     * EColor16MU (mode 11, 32bpp), measured on the device — not the EColor64K a
     * 16bpp canvas would have matched. One linear pass, no branches.
     *
     * Channels widen by replicating their high bits into the low ones, so 5-bit
     * 0x1F becomes 0xFF; a plain shift would make white come out 0xF8F8F8. */
    iBack->LockHeap();
    TUint8* base = reinterpret_cast<TUint8*>(iBack->DataAddress());
    const TInt stride = iBack->DataStride();

    if (iFacts.iMode == EColor64K)
        {
        for (TInt y = 0; y < size.iHeight; y++)
            Mem::Copy(base + y * stride, iStage + y * size.iWidth, size.iWidth * 2);
        }
    else
        {
        for (TInt y = 0; y < size.iHeight; y++)
            {
            const TUint16* src = iStage + y * size.iWidth;
            TUint32* dst = reinterpret_cast<TUint32*>(base + y * stride);
            for (TInt x = 0; x < size.iWidth; x++)
                {
                TUint p = src[x];
                TUint r = (p >> 11) & 0x1F;
                TUint g = (p >> 5) & 0x3F;
                TUint b = p & 0x1F;
                dst[x] = (((r << 3) | (r >> 2)) << 16)
                       | (((g << 2) | (g >> 4)) << 8)
                       |  ((b << 3) | (b >> 2));
                }
            }
        }
    iBack->UnlockHeap();

    /* Text through Symbian's own font: real hinted glyphs and full UCS-2, for no
     * bytes of our own. CEikonEnv owns these fonts; do not release them. */
    const CFont* font = CEikonEnv::Static()->NormalFont();
    iBackGc->UseFont(font);
    iBackGc->SetPenColor(TRgb(255, 255, 255));
    iBackGc->SetBrushStyle(CGraphicsContext::ENullBrush);

    TInt line = font->HeightInPixels() + 2;
    TInt y = line;

    iBackGc->DrawText(_L("Rust Symbian SDK"), TPoint(6, y));
    y += line;

    /* The two answers this app exists to produce. */
    TBuf<64> buf;
    buf.Format(_L("display mode=%d bpp=%d stride=%d"),
               iFacts.iMode, iFacts.iBpp, iFacts.iStride);
    iBackGc->DrawText(buf, TPoint(6, y));
    y += line;

    buf.Format(_L("red pixel word=0x%08x"), iFacts.iRedWord);
    iBackGc->DrawText(buf, TPoint(6, y));
    y += line;

    buf.Format(_L("screen %dx%d  EColor64K=%d"), size.iWidth, size.iHeight, (TInt) EColor64K);
    iBackGc->DrawText(buf, TPoint(6, y));
    y += line;

    buf.Format(_L("last key code=%d scan=%d"), iLastCode, iLastScan);
    iBackGc->DrawText(buf, TPoint(6, y));
    y += line;

    buf.Format(_L("frame %d - press keys, RSK exits"), iFrame);
    iBackGc->DrawText(buf, TPoint(6, y));

    iBackGc->DiscardFont();
    }

void CHelloControl::Draw(const TRect& aRect) const
    {
    CWindowGc& gc = SystemGc();
    if (iBack)
        {
        gc.SetBrushStyle(CGraphicsContext::ENullBrush);
        /* BitBlt, never DrawBitmap: the latter always scales. */
        gc.BitBlt(aRect.iTl, iBack, aRect);
        }
    else
        {
        gc.SetBrushColor(TRgb(0, 0, 0));
        gc.Clear(aRect);
        }
    }

void CHelloControl::SizeChanged()
    {
    if (!iBack)
        return;
    if (iBack->SizeInPixels() == Size())
        return;
    /* Reallocating per frame would be wasteful; per resize is correct. A leave
     * here would be during a non-leaving callback, so it is trapped. */
    TRAPD(err, CreateBackBufferL(Size()); RenderL());
    if (err != KErrNone)
        RDebug::Print(_L("hello: back buffer resize failed %d"), err);
    }

TKeyResponse CHelloControl::OfferKeyEventL(const TKeyEvent& aKeyEvent, TEventCode aType)
    {
    /* EEventKey carries the translated character; the window server has already
     * applied Shift, Caps Lock and the Fn layer. On a QWERTY device this stream
     * *is* text input, which is why the Rust toolkit needs no editor widget. */
    if (aType != EEventKey)
        return EKeyWasNotConsumed;

    iLastCode = aKeyEvent.iCode;
    iLastScan = aKeyEvent.iScanCode;
    iFrame++;

    /* Right softkey exits. Reported as EKeyDevice1 when there is no CBA to eat it
     * — one of the things this app is here to confirm. */
    if (aKeyEvent.iCode == EKeyDevice1)
        {
        static_cast<CHelloAppUi*>(iCoeEnv->AppUi())->HandleCommandL(EEikCmdExit);
        return EKeyWasConsumed;
        }

    RenderL();
    DrawNow();
    return EKeyWasConsumed;
    }

/* ------------------------------------------------------------------ appui -- */

void CHelloAppUi::ConstructL()
    {
    Trace(_L8("3-appui ConstructL"));
    /* ENoScreenFurniture removes the status pane and the CBA, which hands us the
     * whole 320x240 and — the part that matters — lets softkey presses reach our
     * control instead of being consumed by Avkon's button group. */
    BaseConstructL(CAknAppUi::EAknEnableSkin | CEikAppUi::ENoScreenFurniture);

    Trace(_L8("4-BaseConstructL ok"));
    iControl = CHelloControl::NewL(ApplicationRect());
    /* Without this the control never sees a key event, regardless of focus. */
    AddToStackL(iControl);
    Trace(_L8("12-on control stack - startup complete"));
    }

CHelloAppUi::~CHelloAppUi()
    {
    if (iControl)
        {
        RemoveFromStack(iControl);
        delete iControl;
        }
    }

void CHelloAppUi::HandleCommandL(TInt aCommand)
    {
    switch (aCommand)
        {
        case EEikCmdExit:
        case EAknSoftkeyExit:
            Exit();
            break;
        default:
            break;
        }
    }

void CHelloAppUi::HandleResourceChangeL(TInt aType)
    {
    CAknAppUi::HandleResourceChangeL(aType);
    if (aType == KEikDynamicLayoutVariantSwitch && iControl)
        iControl->SetRect(ApplicationRect());
    }

/* ------------------------------------------------- document / application -- */

CHelloDocument* CHelloDocument::NewL(CEikApplication& aApp)
    {
    return new (ELeave) CHelloDocument(aApp);
    }

CEikAppUi* CHelloDocument::CreateAppUiL()
    {
    Trace(_L8("2-CreateAppUiL"));
    return new (ELeave) CHelloAppUi;
    }

TUid CHelloApplication::AppDllUid() const
    {
    return KUidHelloApp;
    }

CApaDocument* CHelloApplication::CreateDocumentL()
    {
    return CHelloDocument::NewL(*this);
    }

/* ------------------------------------------------------------------- entry -- */

LOCAL_C CApaApplication* NewApplication()
    {
    Trace(_L8("1-NewApplication"));
    /* Plain new, not new (ELeave): this factory must not leave. */
    return new CHelloApplication;
    }

GLDEF_C TInt E32Main()
    {
    /* RunApplication installs the CTrapCleanup and the CActiveScheduler, builds
     * CEikonEnv, walks the factory chain and then calls
     * CActiveScheduler::Start(). We never own the loop — which is the constraint
     * the whole Rust design is shaped around. */
    Trace(_L8("0-E32Main entered"));
    TInt r = EikStart::RunApplication(NewApplication);
    Trace(_L8("13-RunApplication returned"));
    return r;
    }
