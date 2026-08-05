/* The Symbian application, and the pump that gives Rust its turn.
 *
 * Structure, lifted from examples/hello-gui which is already proven on the device:
 *
 *   E32Main -> EikStart::RunApplication(NewApplication)
 *     CShimApplication  (CAknApplication)  AppDllUid, CreateDocumentL
 *       CShimDocument   (CAknDocument)     CreateAppUiL
 *         CShimAppUi    (CAknAppUi)        owns the control and the pump
 *           CShimControl (CCoeControl)     the window, the keys, the blit
 *
 * THE INVERSION
 *
 * EikStart::RunApplication ends in CActiveScheduler::Start(), and it never returns
 * until the app exits. There is no loop for Rust to own, and no way to take one.
 * So the relationship is inverted from a normal Rust GUI program: every key press
 * and every I/O completion becomes a POD event on a ring buffer, and a CIdle
 * running at idle priority calls rust_step(), which drains the queue, updates, and
 * asks to present. That is the same shape as winit's ApplicationHandler, and it is
 * why symbian-ui was written as `handle_key` + `draw` rather than as a main loop.
 *
 * rust_step must return promptly. It runs on the GUI thread, and a long one
 * starves the window server, which makes the whole phone appear frozen — not just
 * this app.
 */

#include "shim_priv.h"

#include <eikstart.h>
#include <eikenv.h>
#include <eikappui.h>
#include <aknapp.h>
#include <akndoc.h>
#include <aknappui.h>
#include <coecntrl.h>
#include <coeinput.h>
#include <coemain.h>
#include <w32std.h>
#include <fbs.h>
#include <e32keys.h>

/* Must match UID3 in the app's _reg.rss and the --uid3 given to elf2e32, so
 * symbuild passes all three from the single value in app.conf. Getting them out of
 * step produces the least debuggable failure this platform has: AppArc finds a
 * registration for a UID no installed binary claims, so the icon appears and
 * tapping it does nothing at all — no error, no panic, no log.
 *
 * The default only exists so the file compiles standalone; the 0xE range is the
 * unprotected development block.
 */
#ifndef SHIM_APP_UID3
#define SHIM_APP_UID3 0xE1234569
#endif
const TUid KUidShimApp = { (TInt32) SHIM_APP_UID3 };

class CShimControl;

/* ---------------------------------------------------------------- key mapping --
 * The window server has already done the hard part. On EEventKey it hands us a
 * translated character in iCode with Shift, Caps Lock and the E72's Fn layer
 * applied, so that stream *is* text input — which is why the Rust toolkit needs no
 * editor widget and no input method.
 *
 * Only the keys with no character go through the scan code path. Deliberately a
 * table rather than a switch: it is the thing most likely to need adjusting once
 * more devices appear, and a table is easier to read than a ladder of cases. */
namespace {

struct TKeyMap
    {
    TInt iCode;      /* TKeyCode from iCode on EEventKey */
    TInt iShimKind;  /* SHIM_EV_* */
    TInt iShimKey;   /* our own key id, mirrored in symbian-sys */
    };

/* Our abstract key ids. Mirrored by symbian_sys::key and translated into
 * symbian_ui::Key on the Rust side. Values are arbitrary but must not collide
 * with a Unicode scalar, because SHIM_EV_KEY_CHAR carries one of those in `a`. */
enum TShimKey
    {
    EShimKeyUp = 0x110000,
    EShimKeyDown,
    EShimKeyLeft,
    EShimKeyRight,
    EShimKeySelect,
    EShimKeySoftLeft,
    EShimKeySoftMiddle,
    EShimKeySoftRight,
    EShimKeyBackspace,
    EShimKeyDelete,
    EShimKeyEnter,
    EShimKeyCall,
    EShimKeyEnd
    };

const TKeyMap KKeyMap[] =
    {
    { EKeyUpArrow,    SHIM_EV_KEY_DOWN, EShimKeyUp },
    { EKeyDownArrow,  SHIM_EV_KEY_DOWN, EShimKeyDown },
    { EKeyLeftArrow,  SHIM_EV_KEY_DOWN, EShimKeyLeft },
    { EKeyRightArrow, SHIM_EV_KEY_DOWN, EShimKeyRight },
    /* EKeyDevice3 is the D-pad centre press. */
    { EKeyDevice3,    SHIM_EV_KEY_DOWN, EShimKeySelect },
    /* Left and right softkeys. FP2 added a middle one; EKeyDevice3 doubles as it
     * on some layouts, which is why Select and MSK land on the same id — the
     * toolkit treats both as "activate". */
    { EKeyDevice0,    SHIM_EV_KEY_DOWN, EShimKeySoftLeft },
    { EKeyDevice1,    SHIM_EV_KEY_DOWN, EShimKeySoftRight },
    { EKeyBackspace,  SHIM_EV_KEY_DOWN, EShimKeyBackspace },
    { EKeyDelete,     SHIM_EV_KEY_DOWN, EShimKeyDelete },
    { EKeyEnter,      SHIM_EV_KEY_DOWN, EShimKeyEnter },
    { EKeyYes,        SHIM_EV_KEY_DOWN, EShimKeyCall },
    { EKeyNo,         SHIM_EV_KEY_DOWN, EShimKeyEnd },
    };

const TInt KKeyMapCount = sizeof(KKeyMap) / sizeof(KKeyMap[0]);

/* ------------------------------------------------------------- the Fn layer --
 *
 * The E72's numeric layer is armed by the blue Fn key, bottom-left. That key belongs
 * to the FEP, which we are not part of, so its effect never reaches us — pressing it
 * and then R still produced 'r'.
 *
 * It does reach us as a key of its own, though: EStdKeyLeftFunc, scan code 0x18, on
 * EEventKeyDown. Our handler used to return at the top of the function for anything
 * that was not EEventKey, which is why this was invisible. So we keep the state
 * ourselves and mirror the platform's behaviour:
 *
 *   one press   -> the next key only    (armed)
 *   two presses -> stays on             (locked)
 *   press while locked -> off
 *
 * `Armed` is deliberately consumed by the next *character*, not by the next event of
 * any kind: arming Fn and then pressing Down should still scroll, and should not
 * silently spend the arm. */
TBool gFnArmed = EFalse;
TBool gFnLocked = EFalse;

void FnKeyPressed()
    {
    if (gFnLocked)
        {
        gFnLocked = EFalse;
        gFnArmed = EFalse;
        }
    else if (gFnArmed)
        {
        gFnArmed = EFalse;
        gFnLocked = ETrue;
        }
    else
        {
        gFnArmed = ETrue;
        }
    }

TBool FnActive()
    {
    return gFnArmed || gFnLocked;
    }

} /* namespace */

/* ------------------------------------------------- the overlaid phone keypad --
 *
 * The E72 prints a 12-key phone keypad on top of the letters:
 *
 *     1 2 3  ->  R T Y        7 8 9  ->  V B N
 *     4 5 6  ->  F G H        * 0 #  ->  U M J
 *
 * The window server identifies those twelve physical keys *as the digit keys*: the
 * R key arrives with iScanCode 0x31, which is the scan code of '1', not of 'R'. It
 * is not a mistranslation — at the window server's level, that key is the 1 key. The
 * letter identity is applied above it by Avkon's FEP, from the input mode of the
 * focused editor.
 *
 * Declaring TCoeInputCapabilities::EAllText is not enough — tried on device, no
 * change. What the FEP actually reads is the input mode on the focused editor's state
 * object, CAknEdwinState (aknedsts.h), through SetCurrentInputMode.
 *
 * An earlier version of this comment claimed that class was absent from the public
 * SDK. That was wrong, and wrong for an instructive reason: the grep that "proved" it
 * returned nothing because these headers carry a © in extended-ASCII, which makes grep
 * treat them as binary and suppress every match in silence. `grep -a` shows the class
 * on line 158 of aknedsts.h. See docs/device-notes.md.
 *
 * So the FEP path is available, and we translate anyway — a choice, not a limitation:
 *
 *   - The FEP's job is inline editing and predictive text. Taking it means
 *     implementing MCoeFepAwareTextEditor, twelve pure virtuals, and handing the FEP
 *     authority over a caret and a text buffer the Rust toolkit already owns. Two
 *     things holding one buffer is the bug, not the wiring.
 *   - This translation is tested on hardware. That is worth more than an untested
 *     alternative that is architecturally tidier.
 *
 * What it costs is real and worth stating: the FEP would give the whole Fn layer,
 * including symbols. Fn+Q should produce '!' and produces 'q', because only the twelve
 * digit keys are in the table below. Fixing that our way needs a second, larger,
 * device-specific table; fixing it the FEP's way needs the interface above. Neither is
 * done.
 *
 * The trigger is self-identifying and needs no state: for a letter key the window
 * server *does* translate, so iCode differs from iScanCode ('e' 0x65 vs 'E' 0x45).
 * For these twelve it does not, so iCode == iScanCode. Translate only then, and a
 * device without the overlay is unaffected because its scan codes never match.
 */
struct TKeypadOverlay
    {
    TInt iScanCode;   /* what the window server calls the key */
    TInt iLower;      /* the letter actually printed on it */
    };

const TKeypadOverlay KKeypadOverlay[] =
    {
    { '1', 'r' }, { '2', 't' }, { '3', 'y' }, { '*', 'u' },
    { '4', 'f' }, { '5', 'g' }, { '6', 'h' }, { '#', 'j' },
    { '7', 'v' }, { '8', 'b' }, { '9', 'n' }, { '0', 'm' },
    };

const TInt KKeypadOverlayCount = sizeof(KKeypadOverlay) / sizeof(KKeypadOverlay[0]);

/* The letter an overlaid key should produce, or 0 if this is not one of them.
 *
 * Ctrl held means "I want the digit" — we own the translation, so we own the way out
 * of it. The phone's own numeric toggle is handled by the FEP and therefore invisible
 * to us; ShimEvent::native carries the raw iModifiers word so that can be
 * investigated with the key probe rather than guessed at.
 */
TInt OverlayLetter(const TKeyEvent& aKey)
    {
    if (aKey.iCode != aKey.iScanCode)
        return 0;                       /* the window server already translated it */
    /* Any of these means "I want the digit": our own Fn state, a Func modifier if
     * the platform does report one, or Ctrl held. Ctrl stays in as a fallback that
     * works with no state at all. */
    if (FnActive()
        || (aKey.iModifiers & (EModifierFunc | EModifierLeftFunc | EModifierRightFunc))
        || (aKey.iModifiers & EModifierCtrl))
        return 0;
    for (TInt i = 0; i < KKeypadOverlayCount; i++)
        {
        if (KKeypadOverlay[i].iScanCode == aKey.iScanCode)
            {
            TInt ch = KKeypadOverlay[i].iLower;
            /* Shift and Caps Lock are ours to apply too, for the same reason: the
             * window server never produced a character here, so nothing has applied
             * them yet. */
            const TBool upper = (aKey.iModifiers & EModifierShift)
                             || (aKey.iModifiers & EModifierCapsLock);
            return upper ? (ch - 'a' + 'A') : ch;
            }
        }
    return 0;
    }

CShimControl* gControl = NULL;
TBool gExitRequested = EFalse;

/* ------------------------------------------------------------------ control -- */

class CShimControl : public CCoeControl
    {
public:
    static CShimControl* NewL(const TRect& aRect);
    ~CShimControl();

    void Draw(const TRect& aRect) const;
    TKeyResponse OfferKeyEventL(const TKeyEvent& aKeyEvent, TEventCode aType);
    TCoeInputCapabilities InputCapabilities() const;
    void SizeChanged();

    /* Called by shim_present, via ShimBlitToScreen. */
    void BlitL(const TRect& aRect);

private:
    CShimControl() {}
    void ConstructL(const TRect& aRect);

    CShimSurface* iSurface;   /* owned */
    };

CShimControl::~CShimControl()
    {
    ShimSetSurface(NULL);
    delete iSurface;
    if (gControl == this)
        gControl = NULL;
    }

CShimControl* CShimControl::NewL(const TRect& aRect)
    {
    CShimControl* self = new (ELeave) CShimControl;
    CleanupStack::PushL(self);
    self->ConstructL(aRect);
    CleanupStack::Pop(self);
    return self;
    }

void CShimControl::ConstructL(const TRect& aRect)
    {
    CreateWindowL();
    SetRect(aRect);

    /* Ask, never assume. The E72 reports EColor16MU (32bpp) where a 16bpp canvas
     * would have matched EColor64K, and getting this wrong means the window server
     * silently converts every blit — the difference between a few milliseconds a
     * frame and tens of them. */
    CWsScreenDevice* screen = iCoeEnv->ScreenDevice();
    const TDisplayMode mode = screen->DisplayMode();

    iSurface = CShimSurface::NewL(aRect.Size(), mode);
    ShimSetSurface(iSurface);

    /* Tell the window server our mode so it has no reason to convert. */
    Window().SetRequiredDisplayMode(mode);
    ActivateL();
    gControl = this;
    }

void CShimControl::Draw(const TRect& aRect) const
    {
    CWindowGc& gc = SystemGc();
    if (iSurface && iSurface->Bitmap())
        {
        gc.SetBrushStyle(CGraphicsContext::ENullBrush);
        /* BitBlt, never DrawBitmap: the latter always scales. */
        gc.BitBlt(aRect.iTl, iSurface->Bitmap(), aRect);
        }
    else
        {
        gc.SetBrushColor(TRgb(0, 0, 0));
        gc.Clear(aRect);
        }
    }

void CShimControl::BlitL(const TRect& aRect)
    {
    /* DrawNow does Invalidate + BeginRedraw + Draw + EndRedraw, which also stores
     * the content in the window server so later system-triggered redraws cost us
     * nothing. The explicit Flush matters: without it the frame sits in the
     * client-side command buffer until it fills, and appears late. */
    DrawNow(aRect);
    iCoeEnv->WsSession().Flush();
    }

void CShimControl::SizeChanged()
    {
    if (!iSurface)
        return;
    TInt err = KErrNone;
    TRAP(err, iSurface->ResizeL(Size()));
    if (err != KErrNone)
        {
        RDebug::Print(_L("shim: surface resize failed %d"), err);
        return;
        }
    ShimPushSimple(SHIM_EV_RESIZE, 0, SHIM_OK, Size().iWidth);
    }

/* Declare that this control accepts text, which is what makes the E72's keyboard
 * produce letters.
 *
 * The E72 overlays a phone keypad on the letters: R T Y / F G H / V B N carry 1-9,
 * U carries * and J carries #. Whether one of those keys yields a letter or a digit
 * is decided by Avkon's FEP, from the input capabilities of whatever has focus. A
 * control that declares nothing gets the *keypad* mapping, because a plain keypad is
 * the fallback — so T arrived as '2' and H as '6', while Q and W (which carry ! and
 * ", not digits) translated to letters normally. mod was 00 throughout: no modifier
 * was involved, and nothing in our own mapping table was at fault.
 *
 * EAllText with a NULL MCoeFepAwareTextEditor is deliberate. We want the FEP's key
 * translation, not its inline editing: the Rust side owns the caret and the text
 * buffer, and handing the FEP an editor interface would give it a second opinion
 * about both. NULL is a documented value for that parameter.
 */
TCoeInputCapabilities CShimControl::InputCapabilities() const
    {
    return TCoeInputCapabilities(TCoeInputCapabilities::EAllText, NULL, NULL);
    }

TKeyResponse CShimControl::OfferKeyEventL(const TKeyEvent& aKeyEvent, TEventCode aType)
    {
    /* The Fn key never produces an EEventKey — it is a modifier, so it only ever
     * arrives as down/up. Catch it here, before the EEventKey filter that used to
     * hide it. */
    if (aType == EEventKeyDown
        && (aKeyEvent.iScanCode == EStdKeyLeftFunc
            || aKeyEvent.iScanCode == EStdKeyRightFunc))
        {
        FnKeyPressed();
        return EKeyWasConsumed;
        }

    /* Otherwise only EEventKey carries a translated character. Down and up would
     * duplicate every keystroke. */
    if (aType != EEventKey)
        return EKeyWasNotConsumed;

    TInt mods = 0;
    if (aKeyEvent.iModifiers & EModifierShift) mods |= 1;
    if (aKeyEvent.iModifiers & EModifierCtrl)  mods |= 2;
    /* Our own Fn state counts as Func: the app should not have to know that the
     * platform's Fn key is invisible to it and ours is synthesized. */
    if ((aKeyEvent.iModifiers & EModifierFunc) || FnActive()) mods |= 4;

    for (TInt i = 0; i < KKeyMapCount; i++)
        {
        if (KKeyMap[i].iCode == aKeyEvent.iCode)
            {
            ShimEvent e;
            e.kind = SHIM_EV_KEY_DOWN;
            e.handle = 0;
            e.status = SHIM_OK;
            e.a = KKeyMap[i].iShimKey;
            e.b = mods;
            e.c = aKeyEvent.iRepeats;
            e.d = aKeyEvent.iScanCode;
            e.native = aKeyEvent.iModifiers;
            ShimPushEvent(e);

            /* The red End key is captured by the system to close the app. Consuming
             * it would fight the platform, so it is reported and passed on. */
            return (aKeyEvent.iCode == EKeyNo) ? EKeyWasNotConsumed : EKeyWasConsumed;
            }
        }

    /* Anything else with a printable code is text. */
    if (aKeyEvent.iCode >= 0x20 && aKeyEvent.iCode != 0x7F)
        {
        const TInt overlay = OverlayLetter(aKeyEvent);
        ShimEvent e;
        e.kind = SHIM_EV_KEY_CHAR;
        e.handle = 0;
        e.status = SHIM_OK;
        /* a UCS-2 scalar, below 0x110000 by construction. */
        e.a = overlay ? overlay : aKeyEvent.iCode;
        e.b = mods;
        e.c = aKeyEvent.iRepeats;
        e.d = aKeyEvent.iScanCode;
        e.native = aKeyEvent.iModifiers;
        ShimPushEvent(e);
        /* One press of Fn covers one character. A lock survives. */
        gFnArmed = EFalse;
        return EKeyWasConsumed;
        }

    /* Anything left is a key we have no name for. Report it rather than drop it: the
     * toolkit has Key::Raw for exactly this, and a silently discarded key is how the
     * Fn key stayed invisible through two rounds of on-device debugging. */
    ShimEvent e;
    e.kind = SHIM_EV_KEY_DOWN;
    e.handle = 0;
    e.status = SHIM_OK;
    e.a = aKeyEvent.iCode;
    e.b = mods;
    e.c = aKeyEvent.iRepeats;
    e.d = aKeyEvent.iScanCode;
    e.native = aKeyEvent.iModifiers;
    ShimPushEvent(e);
    return EKeyWasNotConsumed;
    }

/* -------------------------------------------------------------------- appui -- */

class CShimAppUi : public CAknAppUi
    {
public:
    void ConstructL();
    ~CShimAppUi();

    void HandleCommandL(TInt aCommand);

private:
    void HandleResourceChangeL(TInt aType);
    static TInt PumpCallback(TAny* aSelf);
    TInt Pump();

    CShimControl* iControl;
    CIdle* iPump;
    };

void CShimAppUi::ConstructL()
    {
    /* ENoScreenFurniture removes the status pane and the CBA. That hands us the
     * whole 320x240 and — the part that matters — lets softkey presses reach our
     * control instead of being consumed by Avkon's button group. Confirmed on
     * device. */
    BaseConstructL(CAknAppUi::EAknEnableSkin | CEikAppUi::ENoScreenFurniture);

    iControl = CShimControl::NewL(ApplicationRect());
    /* Without this the control never sees a key event, focus or not. */
    AddToStackL(iControl);

    rust_app_start();

    /* Idle priority: the pump runs whenever the scheduler has nothing more urgent,
     * which is exactly the right time to redraw. Returning ETrue from the callback
     * reschedules it, so this is a cooperative loop that always yields. */
    iPump = CIdle::NewL(CActive::EPriorityIdle);
    iPump->Start(TCallBack(&CShimAppUi::PumpCallback, this));
    }

CShimAppUi::~CShimAppUi()
    {
    if (iPump)
        {
        iPump->Cancel();
        delete iPump;
        }
    /* Tell Rust before tearing down the surface it may still hold a pointer to. */
    rust_app_stop();
    ShimTimersCleanup();
    ShimFilesCleanup();
#ifdef SHIM_USE_NET
    /* Compiled in only when the app opted into networking, because shim_net.cpp is
     * only compiled then — see the source selection in tools/symbuild. */
    ShimNetCleanup();
    ShimWorkCleanup();
#endif
    if (iControl)
        {
        RemoveFromStack(iControl);
        delete iControl;
        }
    }

TInt CShimAppUi::PumpCallback(TAny* aSelf)
    {
    return static_cast<CShimAppUi*>(aSelf)->Pump();
    }

TInt CShimAppUi::Pump()
    {
    /* rust_step is `extern "C"` and Rust is built panic=abort, so it cannot unwind
     * into us. It also must not leave — it has no way to, since it never calls a
     * leaving function directly, only shim entry points that all TRAP. */
    rust_step();

    if (gExitRequested)
        {
        gExitRequested = EFalse;
        Exit();
        return EFalse;   /* stop rescheduling; we are going away */
        }
    return ETrue;
    }

void CShimAppUi::HandleCommandL(TInt aCommand)
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

void CShimAppUi::HandleResourceChangeL(TInt aType)
    {
    CAknAppUi::HandleResourceChangeL(aType);
    if (aType == KEikDynamicLayoutVariantSwitch && iControl)
        iControl->SetRect(ApplicationRect());
    }

/* ------------------------------------------------------ private shim helpers -- */

void ShimBlitToScreen(const TRect& aRect)
    {
    if (!gControl)
        return;
    TInt err = KErrNone;
    TRAP(err, gControl->BlitL(aRect));
    if (err != KErrNone)
        RDebug::Print(_L("shim: blit failed %d"), err);
    }

void ShimRequestExit()
    {
    gExitRequested = ETrue;
    }

extern "C" void shim_request_exit(void)
    {
    ShimRequestExit();
    }

/* ------------------------------------------------- document / application -- */

class CShimDocument : public CAknDocument
    {
public:
    static CShimDocument* NewL(CEikApplication& aApp)
        {
        return new (ELeave) CShimDocument(aApp);
        }

private:
    CShimDocument(CEikApplication& aApp) : CAknDocument(aApp) {}
    CEikAppUi* CreateAppUiL() { return new (ELeave) CShimAppUi; }
    };

class CShimApplication : public CAknApplication
    {
private:
    TUid AppDllUid() const { return KUidShimApp; }
    CApaDocument* CreateDocumentL() { return CShimDocument::NewL(*this); }
    };

LOCAL_C CApaApplication* NewApplication()
    {
    /* Plain new, not new (ELeave): this factory must not leave. */
    return new CShimApplication;
    }

GLDEF_C TInt E32Main()
    {
    return EikStart::RunApplication(NewApplication);
    }
