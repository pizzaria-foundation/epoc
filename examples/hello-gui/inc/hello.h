/* A real S60 application: appears in the menu, owns a window, draws its own
 * pixels, and reports what it found out about the device.
 *
 * This is deliberately the skeleton the Rust shim will use, so the structure
 * matters as much as the output:
 *
 *   E32Main -> EikStart::RunApplication(NewApplication)
 *     CHelloApplication  (CAknApplication)  -- AppDllUid, CreateDocumentL
 *       CHelloDocument   (CAknDocument)     -- CreateAppUiL
 *         CHelloAppUi    (CAknAppUi)        -- owns the control, handles commands
 *           CHelloControl (CCoeControl)     -- the window, the back buffer, keys
 *
 * The back buffer is a CFbsBitmap whose pixels we write directly. Its memory
 * lives in a chunk shared with the font and bitmap server and mapped into the
 * window server too, so CWindowGc::BitBlt copies nothing across a process
 * boundary — which is exactly why the Rust rasterizer can own that pointer.
 */

#ifndef HELLO_H
#define HELLO_H

#include <aknapp.h>
#include <akndoc.h>
#include <aknappui.h>
#include <coecntrl.h>

class CFbsBitmap;
class CFbsBitmapDevice;
class CFbsBitGc;

/* What we learned about the display at runtime. None of this is safe to assume:
 * the E72's panel is 24-bit but which Symbian display mode it exposes to
 * applications is not documented anywhere we could find, and the byte order
 * within a pixel is not either. So we measure and show the answer. */
class TDisplayFacts
    {
public:
    TInt iMode;             // TDisplayMode as an integer
    TInt iBpp;
    TInt iStride;           // bytes per scanline, as reported
    TUint32 iRedWord;       // first word of a 1x1 bitmap painted pure red
    TBool iDirectOk;        // whether DataAddress() gave us a usable pointer
    };

class CHelloControl : public CCoeControl
    {
public:
    static CHelloControl* NewL(const TRect& aRect);
    ~CHelloControl();

    // CCoeControl
    void Draw(const TRect& aRect) const;
    TKeyResponse OfferKeyEventL(const TKeyEvent& aKeyEvent, TEventCode aType);
    void SizeChanged();

private:
    CHelloControl();
    void ConstructL(const TRect& aRect);

    void CreateBackBufferL(const TSize& aSize);
    void ProbePixelLayoutL();
    /* Writes straight into the back buffer's memory, the way the Rust rasterizer
     * will. Everything else here is scaffolding; this is the part being proven. */
    void RenderL();

    CFbsBitmap* iBack;
    /* 16bpp staging buffer, standing in for the buffer the Rust rasterizer owns.
       Rendering here and expanding once beats drawing straight into a 32bpp
       surface, because a UI overdraws and this halves the traffic while it does. */
    TUint16* iStage;
    CFbsBitmapDevice* iBackDev;
    CFbsBitGc* iBackGc;
    TDisplayFacts iFacts;
    /* Which key was pressed last, so the screen visibly reacts and we can tell a
     * live app from a frozen one. */
    TInt iLastCode;
    TInt iLastScan;
    TInt iFrame;
    };

class CHelloAppUi : public CAknAppUi
    {
public:
    void ConstructL();
    ~CHelloAppUi();

    /* Public because the control calls it to quit on the right softkey. */
    void HandleCommandL(TInt aCommand);

private:
    /* Note the L: on this SDK CEikAppUi declares HandleResourceChangeL, not the
     * HandleResourceChange that later Symbian versions use. */
    void HandleResourceChangeL(TInt aType);

    CHelloControl* iControl;
    };

class CHelloDocument : public CAknDocument
    {
public:
    static CHelloDocument* NewL(CEikApplication& aApp);

private:
    CHelloDocument(CEikApplication& aApp) : CAknDocument(aApp) {}
    CEikAppUi* CreateAppUiL();
    };

class CHelloApplication : public CAknApplication
    {
private:
    TUid AppDllUid() const;
    CApaDocument* CreateDocumentL();
    };

/* Must match UID3 in hello_reg.rss and the --uid3 passed to elf2e32. The 0xE
 * range is the unprotected development block; anything below 0x80000000 belongs
 * to Symbian Signed and would be refused. */
/* TUid's member is TInt32, so a brace-initialised 0xE1234568 is a narrowing
 * conversion and an error in C++11. The cast is the whole fix; the value is
 * meant to be that bit pattern. */
const TUid KUidHelloApp = { (TInt32) 0xE1234568 };

#endif
