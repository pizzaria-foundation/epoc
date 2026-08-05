/* The framebuffer, and getting it onto the screen.
 *
 * The arrangement, and why:
 *
 *   Rust draws into  iStage   16bpp RGB565, plain memory it fully owns
 *                       |
 *                       | Expand()  one linear pass, no branches
 *                       v
 *                    iBack     CFbsBitmap in the screen's own mode
 *                       |
 *                       | CWindowGc::BitBlt, in the control's Draw
 *                       v
 *                    screen
 *
 * The E72 reports EColor16MU — 32 bits per pixel — which we measured on the device
 * rather than assumed (it printed `display mode=11 bpp=32` while EColor64K is 7).
 * Rendering natively in 32bpp would skip the expansion, but it doubles the memory
 * traffic of every drawing operation, and a UI overdraws constantly: background,
 * then a bubble over it, then text over that. Drawing in 16bpp and expanding once
 * moves half as many bytes during the work and pays a single pass at the end.
 *
 * If a device ever reports EColor64K the expansion degrades to a per-row memcpy,
 * and the staging buffer could in principle be dropped entirely — but keeping it
 * means Rust always sees one pixel format, which is worth more than the copy costs.
 */

#include "shim_priv.h"

#include <fbs.h>
#include <bitdev.h>
#include <bitstd.h>
#include <gdi.h>

namespace {
CShimSurface* gSurface = NULL;
TBool gLocked = EFalse;
} /* namespace */

void ShimSetSurface(CShimSurface* aSurface)
    {
    gSurface = aSurface;
    }

CShimSurface* ShimSurface()
    {
    return gSurface;
    }

/* ------------------------------------------------------------------ surface -- */

CShimSurface::CShimSurface(TDisplayMode aMode)
    : iBack(NULL), iBackDev(NULL), iBackGc(NULL), iStage(NULL), iMode(aMode)
    {
    }

CShimSurface::~CShimSurface()
    {
    delete[] iStage;
    delete iBackGc;
    delete iBackDev;
    delete iBack;
    }

CShimSurface* CShimSurface::NewL(const TSize& aSize, TDisplayMode aMode)
    {
    CShimSurface* self = new (ELeave) CShimSurface(aMode);
    CleanupStack::PushL(self);
    self->ConstructL(aSize);
    CleanupStack::Pop(self);
    return self;
    }

void CShimSurface::ConstructL(const TSize& aSize)
    {
    AllocL(aSize);
    }

void CShimSurface::AllocL(const TSize& aSize)
    {
    delete iBackGc;  iBackGc = NULL;
    delete iBackDev; iBackDev = NULL;
    delete iBack;    iBack = NULL;
    delete[] iStage; iStage = NULL;

    iBack = new (ELeave) CFbsBitmap;
    User::LeaveIfError(iBack->Create(aSize, iMode));
    iBackDev = CFbsBitmapDevice::NewL(iBack);
    User::LeaveIfError(iBackDev->CreateContext(iBackGc));

    /* Tightly packed, unlike the bitmap: Symbian aligns CFbsBitmap scanlines to 4
     * bytes, but Rust gets to work with stride == width and one less thing to get
     * wrong. Expand() bridges the two strides. */
    iStage = new (ELeave) TUint16[aSize.iWidth * aSize.iHeight];
    Mem::FillZ(iStage, aSize.iWidth * aSize.iHeight * 2);

    iSize = aSize;
    }

void CShimSurface::ResizeL(const TSize& aSize)
    {
    if (aSize == iSize)
        return;
    AllocL(aSize);
    }

void CShimSurface::Expand(const TRect& aRect)
    {
    if (!iBack || !iStage)
        return;

    TRect r(aRect);
    r.Intersection(TRect(TPoint(0, 0), iSize));
    if (r.IsEmpty())
        return;

    /* LockHeap before DataAddress is mandatory: unlocked, the call can crash, and
     * the pointer is only valid until UnlockHeap because the font and bitmap
     * server's heap may compact. Never cache it across the pair.
     *
     * Note this is LockHeap/UnlockHeap, not BeginDataAccess/EndDataAccess — the
     * latter is what current documentation names, and it does not exist in S60 3rd
     * FP2. It arrived with Symbian^3. */
    iBack->LockHeap();
    TUint8* base = reinterpret_cast<TUint8*>(iBack->DataAddress());
    const TInt stride = iBack->DataStride();
    const TInt w = r.Width();

    if (iMode == EColor64K)
        {
        for (TInt y = r.iTl.iY; y < r.iBr.iY; y++)
            {
            Mem::Copy(base + y * stride + r.iTl.iX * 2,
                      iStage + y * iSize.iWidth + r.iTl.iX,
                      w * 2);
            }
        }
    else
        {
        for (TInt y = r.iTl.iY; y < r.iBr.iY; y++)
            {
            const TUint16* src = iStage + y * iSize.iWidth + r.iTl.iX;
            TUint32* dst = reinterpret_cast<TUint32*>(base + y * stride) + r.iTl.iX;
            for (TInt x = 0; x < w; x++)
                {
                /* Channels widen by replicating their high bits into the low ones,
                 * so 5-bit 0x1F becomes 0xFF. A plain shift would leave white at
                 * 0xF8F8F8 and darken the whole palette slightly. */
                const TUint p = src[x];
                const TUint rr = (p >> 11) & 0x1F;
                const TUint gg = (p >> 5) & 0x3F;
                const TUint bb = p & 0x1F;
                dst[x] = (((rr << 3) | (rr >> 2)) << 16)
                       | (((gg << 2) | (gg >> 4)) << 8)
                       |  ((bb << 3) | (bb >> 2));
                }
            }
        }
    iBack->UnlockHeap();
    }

/* ---------------------------------------------------------------- Rust ABI -- */

extern "C" {

int32_t shim_fb_lock(ShimFb* out)
    {
    if (!out)
        return SHIM_ERR_ARGUMENT;
    CShimSurface* s = ShimSurface();
    if (!s || !s->Staging())
        return SHIM_ERR_NOT_READY;
    if (gLocked)
        return SHIM_ERR_IN_USE;

    /* No Symbian heap lock is taken here, unlike the bitmap: the staging buffer is
     * ordinary memory we allocated, so the pointer is stable and Rust may hold it
     * for as long as it likes. The flag exists only to catch a caller that forgets
     * to unlock, which would otherwise show up as a mysterious missing frame. */
    gLocked = ETrue;
    out->pixels = reinterpret_cast<uint8_t*>(s->Staging());
    out->stride = s->Size().iWidth * 2;      /* bytes, tightly packed */
    out->width = s->Size().iWidth;
    out->height = s->Size().iHeight;
    out->format = SHIM_PF_RGB565;            /* always, whatever the screen is */
    return SHIM_OK;
    }

void shim_fb_unlock(void)
    {
    gLocked = EFalse;
    }

int32_t shim_present(int32_t x, int32_t y, int32_t w, int32_t h)
    {
    CShimSurface* s = ShimSurface();
    if (!s)
        return SHIM_ERR_NOT_READY;
    if (gLocked)
        return SHIM_ERR_IN_USE;
    if (w <= 0 || h <= 0)
        return SHIM_OK;

    TRect r(TPoint(x, y), TSize(w, h));
    s->Expand(r);
    /* Cannot leave: the blit goes through CWindowGc and the flush through
     * RWsSession, neither of which leaves. */
    ShimBlitToScreen(r);
    return SHIM_OK;
    }

int32_t shim_screen_size(int32_t* w, int32_t* h)
    {
    CShimSurface* s = ShimSurface();
    if (!s)
        return SHIM_ERR_NOT_READY;
    if (w) *w = s->Size().iWidth;
    if (h) *h = s->Size().iHeight;
    return SHIM_OK;
    }

int32_t shim_screen_format(int32_t* format)
    {
    CShimSurface* s = ShimSurface();
    if (!s || !format)
        return SHIM_ERR_NOT_READY;
    /* The raw TDisplayMode, deliberately: Rust maps it through
     * symbian_gfx::ScreenFormat::from_display_mode and refuses a device it has
     * never seen rather than rendering garbage. */
    *format = static_cast<int32_t>(s->Mode());
    return SHIM_OK;
    }

int32_t shim_probe_pixel_layout(uint32_t* out_word)
    {
    if (!out_word)
        return SHIM_ERR_ARGUMENT;
    CShimSurface* s = ShimSurface();
    if (!s)
        return SHIM_ERR_NOT_READY;

    TInt err = KErrNone;
    TUint32 word = 0;
    /* Paint one pixel pure red through the documented TRgb API and read the bytes
     * back. Turns "which byte is red?" from a guess into a measurement, on
     * whatever device this happens to be running. */
    TRAP(err, {
        CFbsBitmap* probe = new (ELeave) CFbsBitmap;
        CleanupStack::PushL(probe);
        User::LeaveIfError(probe->Create(TSize(1, 1), s->Mode()));
        CFbsBitmapDevice* dev = CFbsBitmapDevice::NewL(probe);
        CleanupStack::PushL(dev);
        CFbsBitGc* gc = NULL;
        User::LeaveIfError(dev->CreateContext(gc));
        CleanupStack::PushL(gc);
        gc->SetBrushColor(TRgb(255, 0, 0));
        gc->SetBrushStyle(CGraphicsContext::ESolidBrush);
        gc->Clear();
        probe->LockHeap();
        word = *probe->DataAddress();
        probe->UnlockHeap();
        CleanupStack::PopAndDestroy(3);
    });
    if (err != KErrNone)
        return err;
    *out_word = word;
    return SHIM_OK;
    }

} /* extern "C" */
