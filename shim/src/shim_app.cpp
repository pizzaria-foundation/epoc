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
    /* EKeyDevice3 is the D-pad centre press. Add the raw code seen on Brazilian
     * E72 handsets (0x11000C) which does not map to the standard Phidon key. */
    { EKeyDevice3,    SHIM_EV_KEY_DOWN, EShimKeySelect },
    { 0x11000C,       SHIM_EV_KEY_DOWN, EShimKeySelect },
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

/* ----------------------------------------------------------- the keyboard map --
 *
 * There is no keymap in this file, and that is the design. It used to hold one — twelve
 * rows translating the E72's overlaid phone keypad, `1 2 3` on R T Y and so on — and the
 * table has moved to `crates/symbian-keys`, generated from a dump of the handset's own
 * keymap by `tools/mkkeymap.py`.
 *
 * It moved for three reasons, in order of how much they cost while it lived here:
 *
 *   - The target handset is Brazilian and its keyboard is ABNT2, which this SDK was
 *     treating as a US QWERTY. Accents did not work at all and `+` could not be typed. A
 *     correct table is a few dozen rows with four cases each, and a table that size needs
 *     to be generated from a measurement rather than written by hand.
 *   - Nothing here can be tested. A keyboard is all edge cases, and the Rust crate has
 *     unit tests on the host for every one of them.
 *   - The simulator can share a Rust table. It could not share this one, so an accent bug
 *     was only ever reproducible on the phone.
 *
 * What stays here is the part only this file can do: it receives the `TKeyEvent` and
 * returns a `TKeyResponse`. So it reports the facts — `iCode`, `iScanCode` and the raw
 * `iModifiers` — and decides who consumes the key. Nothing here decides what a key means.
 *
 * The FEP would have supplied all of it, and this remains a choice rather than a
 * limitation. `TCoeInputCapabilities::EAllText` alone is not enough — tried on device, no
 * change; what the FEP reads is the input mode on the focused editor's state object,
 * `CAknEdwinState` (aknedsts.h), through `SetCurrentInputMode`. An earlier version of this
 * comment claimed that class was absent from the public SDK, which was wrong, and wrong
 * instructively: the grep that "proved" it returned nothing because these headers carry a
 * © in extended-ASCII, so grep treated them as binary and suppressed every match in
 * silence. `grep -a` finds the class on line 158. See docs/device-notes.md.
 *
 * Taking the FEP means implementing MCoeFepAwareTextEditor and handing it authority over a
 * caret and a text buffer the Rust toolkit already owns. Two components holding one buffer
 * is the bug, not the wiring.
 */

/* ----------------------------------------------------------------- dead keys --
 *
 * There is nothing to do here, and that took a measurement to establish.
 *
 * On an ABNT2 keyboard the accent keys produce no character of their own; they modify the
 * next one. The expectation was that with no FEP running the window server would have no
 * character to hand over and would fall back to a non-character key code above 0xF800,
 * which this file would then have to recognise and consume — otherwise Avkon would act on
 * the same press the Rust side is about to compose with.
 *
 * What the handset actually does, measured with examples/keyprobe on a Brazilian E72:
 *
 *     chr 002E '.'  scan 007A mod 00      the key alone: an ordinary full stop
 *     chr F002      scan 007A mod 01      shifted:       PtiEngine's dead-key code
 *     chr 0027 '\''  scan 007E mod 00
 *     chr F004      scan 007E mod 01
 *
 * The dead key arrives as its *PtiEngine code* in iCode — 0xF001..0xF005 — which is inside
 * the printable gate below, so it already goes out as SHIM_EV_KEY_CHAR and is already
 * consumed. `symbian_keys::layout_abnt2::DEAD_CODES` turns the code into its mark on the
 * Rust side.
 *
 * A table of dead-key scan codes stood here to make the consume decision. It was
 * unreachable, and worse than useless: it named a mechanism the handset does not use.
 */

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
#ifdef SHIM_USE_FEP
    /* Before CreateWindowL, so the editor exists by the time the framework first asks for
     * this control's input capabilities. */
    ShimFepInit();
#endif
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
    /* The editor pointer is the whole difference. EAllText with NULL was tried on device
     * and changed nothing: the capability says what kind of input this control accepts,
     * and the editor is what the FEP actually talks to.
     *
     * NULL here when the scan-code path is selected, which puts the control back exactly
     * where it was -- so switching modes at run time really does compare the two. */
#ifdef SHIM_USE_FEP
    return TCoeInputCapabilities(TCoeInputCapabilities::EAllText, ShimFepEditor(), NULL);
#else
    /* Built without the FEP. EAllText alone was tried on device and changed nothing, so
     * this is the pre-FEP behaviour exactly -- which is what makes a build with USE_FEP=0
     * a clean control rather than a half-configured one. */
    return TCoeInputCapabilities(TCoeInputCapabilities::EAllText, NULL, NULL);
#endif
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

    /* Anything else with a printable code is text.
     *
     * `< 0xF800` is the missing piece that cost a day on a Brazilian E72. Symbian
     * reserves the range 0xF800+ for non-character key codes (EKeyUpArrow,
     * EKeyPrintScreen, EKeyF21, etc. — see e32keys.h ENonCharacterKeyBase).
     *
     * These codes satisfied `>= 0x20 && != 0x7F`, so dead keys like ~ (which the
     * window server reports as EKeyF21, 0xF82A, when the FEP is not active) were
     * pushed as SHIM_EV_KEY_CHAR. On the Rust side char::from_u32(0xF82A) returned
     * None — the event was silently dropped, the dead key never composed, and ~
     * followed by 'a' typed 'a' instead of 'ã'. *
     * Because the character "was consumed", `return EKeyWasConsumed` at the end
     * prevented the unknown-key handler from running, so they didn't even survive
     * as Key::Raw. The `< 0xF800` bound puts them through the right path. */
    if (aKeyEvent.iCode >= 0x20 && aKeyEvent.iCode < 0xF800)
        {
        ShimEvent e;
        e.kind = SHIM_EV_KEY_CHAR;
        e.handle = 0;
        e.status = SHIM_OK;
        /* a UCS-2 scalar, below 0x110000 by construction. The *character* is not decided
         * here: iScanCode goes out in `d` and the Rust layout table has the final say, so
         * the twelve overlaid keypad keys are resolved there rather than in this file. */
        e.a = aKeyEvent.iCode;
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
    void HandleForegroundEventL(TBool aForeground);

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

    /* BELOW idle, not at it — and that one is worth the paragraph.
     *
     * The pump runs whenever the scheduler has nothing more urgent, which is the right
     * time to redraw. Returning ETrue from the callback reschedules it, so this is a
     * cooperative loop that always yields. But re-arming every pass also means the pump is
     * *permanently ready*: there is never a moment when the scheduler does not have it
     * available to run.
     *
     * Symbian dispatches the highest-priority ready object and, among equals, the one
     * added first. So a permanently-ready object at EPriorityIdle does not merely go last
     * — it starves every object at that same priority that is added after it. Forever.
     * Not slowly: never.
     *
     * That is not hypothetical. `CImageDecoder` drives its decode from active objects
     * inside the plugin, and on the E72's vendor JPEG codec those sit at idle priority.
     * `examples/imgprobe` measured both halves of it: a decode issued from `rust_app_start`
     * — before this line runs, so the plugin's objects are queued ahead of the pump —
     * completed in 241 ms, and the byte-identical decode issued later from inside
     * `rust_step` never completed at all. Same image, same configuration, same plugin.
     *
     * One below EPriorityIdle rather than some large negative number: this should be the
     * lowest-priority object in the process, and "strictly below the documented floor" says
     * exactly that without inventing a magnitude. */
    iPump = CIdle::NewL(CActive::EPriorityIdle - 1);
    iPump->Start(TCallBack(&CShimAppUi::PumpCallback, this));
    }

void CShimAppUi::HandleForegroundEventL(TBool aForeground)
    {
    /* Push a focus event so Rust knows whether we are in the foreground.
     * `a` is 1 for foreground, 0 for background. */
    ShimPushSimple(SHIM_EV_FOCUS, 0, SHIM_OK, aForeground ? 1 : 0);

    /* The framework needs to manage sound system state, key resources,
     * and other per-application foreground/background transitions.
     * Nokia's own FocusEvent example always calls the base class, and
     * skipping it is what prevents the framework from doing its job.
     * The restart-on-resource-change path that CAknAppUi also handles
     * is only triggered when a resource change actually occurred,
     * which it never does for this application. */
    CAknAppUi::HandleForegroundEventL(aForeground);
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
#ifdef SHIM_USE_IMAGE
    /* Before the files, not after: a decoder holds an RFile subsession on the shim's
     * file server session, so closing the session first orphans a handle whose close
     * then panics. Same ordering rule as sockets before bearers below. */
    ShimImageCleanup();
#endif
#ifdef SHIM_USE_AUDIO
    /* Before the files for the same reason: the media framework keeps the clip open. */
    ShimAudioCleanup();
#endif
    ShimFilesCleanup();
#ifdef SHIM_USE_FEP
    ShimFepCleanup();
#endif
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
