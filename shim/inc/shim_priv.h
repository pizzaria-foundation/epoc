/* Internal shim declarations: the parts the C++ side shares with itself and the
 * Rust side must never see.
 *
 * Kept out of symbian_shim.h on purpose. That header is the ABI contract and is
 * transcribed by hand into crates/symbian-sys, so anything appearing there is a
 * promise. These are implementation details that may change freely.
 */

#ifndef SHIM_PRIV_H
#define SHIM_PRIV_H

#include "symbian_shim.h"

#include <e32std.h>
#include <e32base.h>   /* CBase, CTimer, CActive */
#include <gdi.h>       /* TDisplayMode */

class CFbsBitmap;
class CFbsBitmapDevice;
class CFbsBitGc;

/* ------------------------------------------------------------------- events --
 * Both are callable from inside a CActive::RunL and from OfferKeyEventL: they
 * neither allocate nor leave. See shim_event.cpp for why the queue drops the
 * newest event rather than the oldest when it fills. */
void ShimPushEvent(const ShimEvent& aEvent);
void ShimPushSimple(TInt aKind, TInt aHandle, TInt aStatus, TInt aA);

/* --------------------------------------------------------------- framebuffer --
 * One instance, owned by the control, registered here so the flat C ABI can
 * reach it without every function taking a context pointer Rust would have to
 * carry. Single-threaded by construction: all drawing happens on the GUI thread,
 * because RWsSession is not thread-safe and window operations must run on the
 * thread that owns the window group. */
class CShimSurface : public CBase
    {
public:
    static CShimSurface* NewL(const TSize& aSize, TDisplayMode aMode);
    ~CShimSurface();

    /* The bitmap the window server blits from. Its pixels live in a chunk shared
     * with the font and bitmap server and mapped into the window server too, so
     * no pixel copy crosses a process boundary. */
    CFbsBitmap* Bitmap() const { return iBack; }

    /* The 16bpp buffer Rust draws into. Separate from the bitmap because the
     * device reports EColor16MU: drawing in RGB565 halves the memory traffic of
     * every operation, and a UI overdraws, so one expansion pass at present time
     * is cheaper than 32bpp throughout. */
    TUint16* Staging() const { return iStage; }
    TSize Size() const { return iSize; }
    TDisplayMode Mode() const { return iMode; }

    void ResizeL(const TSize& aSize);

    /* Expand the staging buffer into the bitmap for the given rectangle, which
     * must already be clipped to the surface. */
    void Expand(const TRect& aRect);

private:
    CShimSurface(TDisplayMode aMode);
    void ConstructL(const TSize& aSize);
    void AllocL(const TSize& aSize);

    CFbsBitmap* iBack;
    CFbsBitmapDevice* iBackDev;
    CFbsBitGc* iBackGc;
    TUint16* iStage;
    TSize iSize;
    TDisplayMode iMode;
    };

/* Set once by the control during construction. The gfx entry points refuse
 * politely (KErrNotReady) rather than crash when it is absent, which matters
 * because Rust may call them before rust_app_start in a misordered build. */
void ShimSetSurface(CShimSurface* aSurface);
CShimSurface* ShimSurface();

/* Blit a region of the surface to the screen and flush the window server queue.
 * Implemented in shim_app.cpp because it needs the control; declared here so
 * shim_gfx.cpp can call it. */
void ShimBlitToScreen(const TRect& aRect);

/* Ask the app to close. Implemented in shim_app.cpp. */
void ShimRequestExit();

/* ------------------------------------------------------------------- timers -- */
void ShimTimersCleanup();

/* Close every open file and the file server session. Called from the app's
 * teardown for the same reason as the timers: a leaked RFile keeps a file server
 * handle alive past process exit, and the panic that follows names the file server
 * rather than us. */
void ShimFilesCleanup();

#endif /* SHIM_PRIV_H */
