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
#include <apgtask.h>  /* TApaTask — moving our own window group behind the others */
#include <fbs.h>
#include <e32keys.h>

/* Defined further down, beside its opposite. Declared here because the key handlers above use it. */
static void ShimRaiseSelf();

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

/* Resident mode, for a launcher. When on, the End key does not close the app — it sends it to the
 * background, the way the home screen behaves — and the Applications (Menu) key is captured so
 * pressing it anywhere brings this app forward. Off by default; an ordinary app is unaffected.
 * The Menu key is hardware, delivered as up/down scan codes rather than a translated character,
 * so it is captured with CaptureKeyUpAndDowns by scan code — the way S60 launchers do it, and
 * confirmed by dissecting GDesk (imports ws32, holds SwEvent, no cenrep). Two codes because the
 * "menu/applications" key is EStdKeyMenu (0x94) on some Nokia hardware and EStdKeyApplication0
 * (0xB4) on others; capturing both covers the E72 without knowing which it emits. The handles are
 * held so they can be cancelled. */
TBool gResident = EFalse;
/* The scan codes a resident launcher captures GLOBALLY, so pressing them in any app brings the
 * launcher forward. This is JUST the menu/applications key — the one thing GDesk captures. It must
 * not include the softkeys (EStdKeyDevice0/1 on Nokia hardware) or the End key: capturing those
 * globally steals them from every other application, which froze Messaging and everything else —
 * no Options, no Back, no way out. A global capture leaks to the whole phone; keep it to the one
 * key that has to. Everything else (like End keeping us put) is handled foreground-only in
 * OfferKeyEventL, where it touches only us. */
/* ============================================================================================
 * RESIDENT LAUNCHER KEY CONTRACT — the behaviour we want, confirmed working on the E72. Do not
 * change the captured-key set without re-testing on the handset; every entry was arrived at the
 * hard way, and the wrong set either freezes other apps or breaks their D-pad.
 *
 *   Captured GLOBALLY (reach us in any app and bring the launcher to the front):
 *     EStdKeyApplication0 (0xB4)  the "casinha" / applications key — exactly and only what GDesk
 *                                 captures (confirmed by disassembling GDesk.exe).
 *     EStdKeyNo           (0xC5)  the red End key — by the user's choice, red goes home too.
 *   NOT captured, on purpose:
 *     EStdKeyMenu         (0x94)  capturing it stole the D-pad in some other apps, and it is not
 *                                 needed (the casinha is 0xB4). This was the bug.
 *     softkeys (EStdKeyDevice0/1) and everything else — a global capture is phone-wide, so
 *                                 anything past the two keys above freezes or breaks other apps.
 *   Everything else is foreground-only, handled below in OfferKeyEventL where it touches only us.
 *
 * The rule it all rests on: capture the MINIMUM globally, never a key another app needs.
 * ============================================================================================ */
/* casinha (App0) + red, PLUS the rest of the dedicated application-launch range (App1..App15,
 * 0xB5..0xC3): calendar/contacts/messaging live here, and capturing them is how the launcher gets
 * to REMAP them. This is safe in a way capturing Menu/softkeys was not — those are navigation keys
 * other apps use continuously, whereas an app-launch key only ever meant "open that app" system-
 * wide, so redirecting it to us takes nothing away from the app in front. Still the minimum that
 * does the job: no navigation keys, no softkeys. If any code in this range turns out to be
 * navigation-critical on some handset, narrow it. */
const TInt KResidentKeys[] = {
    EStdKeyApplication0, // 0xB4 casinha
    0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xBB, 0xBC, // App1..App8
    0xBD, 0xBE, 0xBF, 0xC0, 0xC1, 0xC2, 0xC3,       // App9..App15
    EStdKeyNo,           // 0xC5 red End
};
const TInt KResidentKeyCount = sizeof(KResidentKeys) / sizeof(KResidentKeys[0]);
TInt32 gKeyCaptures[KResidentKeyCount];

/* A capture is not exclusive: the system's own app-key handler also captures these dedicated keys,
 * so on the E72 pressing Contacts BOTH launched Contacts (the system) AND ran our binding. WSERV
 * resolves competing captures by priority — the highest-priority capturer wins and the lower one
 * never sees the key. Capturing at a high priority is therefore what actually *remaps* the key
 * (suppresses the default launch) rather than merely adding an action on top of it. Value chosen
 * well above any ordinary app capture; the priority overload of CaptureKeyUpAndDowns takes it.
 * Raised well past 1000: on the E72 the system application (sysap) captures the dedicated-key chars
 * to launch Contacts/Calendar, and 1000 did not outrank it — so we go far higher to win the char. */
const TInt KResidentKeyPriority = 20000;

/* Capturing the scancode (up/downs) alone did NOT stop the E72 launching Contacts/Calendar: the
 * translated character event (EEventKey) rides a SEPARATE WSERV capture table, so the system's
 * handler still saw it. So we also capture the character codes for the dedicated app keys —
 * EKeyApplication0..F, the keymap's char counterparts of the App0..App15 scancodes — at the same
 * high priority, and consume them in OfferKeyEventL. That is what actually suppresses the default
 * launch. If a given handset emits some other char for these keys, the launch survives and the
 * launcher log (the b==2 RAWKEY we emit) shows the real code to widen this to. Widened to the full
 * App0..App1F char range (0xf852..0xf871) since Contacts may sit in the upper half. */
const TInt KAppCharCount = 32; /* EKeyApplication0..EKeyApplication1F */
TInt32 gCharCaptures[KAppCharCount];

/* Whether this app is the foreground app right now, tracked from HandleForegroundEventL. */
TBool gForeground = ETrue;

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

    /* Resident launcher key handling — the whole reason the app is a launcher and not just an
     * app. Hardware keys (Menu, red End, the device keys) arrive as down/up by scan code, never as
     * a translated character, so they must be handled here, above the EEventKey filter that
     * follows. Two jobs:
     *   - report every scan code to Rust as SHIM_EV_RAWKEY, so the launcher can show on screen
     *     exactly what the Menu and red keys emit on this handset — the end of guessing at codes;
     *   - act on the launcher keys: the menu/apps keys bring us to the front (pressing Menu opens
     *     the launcher), and the End/device keys are consumed so the app stays put instead of
     *     being closed or sent to the idle behind it. */
    if (gResident && (aType == EEventKeyDown || aType == EEventKeyUp))
        {
        const TInt sc = (TInt) aKeyEvent.iScanCode;
        /* The captured keys all mean the same thing to a launcher: come to the front. The menu /
         * applications key is one (GDesk captures exactly this — its handler compares the scan code
         * against EStdKeyApplication0, confirmed by disassembly). The red End key is the other, by
         * the user's choice: red should go home too, so it is captured and brings the launcher
         * forward from any app rather than closing or revealing the native idle. Only these keys
         * are captured — never the softkeys (EStdKeyDevice0/1), which would freeze every other app. */
        const TBool isHome = (sc == (TInt) EStdKeyApplication0 || sc == (TInt) EStdKeyNo);
        /* Report every captured key on BOTH edges — b=0 down, b=1 up — so the Rust side can time a
         * press-and-hold and act on release. The scancode is in `a`. */
        ShimEvent ev;
        ev.kind = SHIM_EV_RAWKEY;
        ev.handle = 0;
        ev.status = SHIM_OK;
        ev.a = sc;
        ev.b = (aType == EEventKeyUp) ? 1 : 0;
        ev.c = 0;
        ev.d = 0;
        ev.native = 0;
        ShimPushEvent(ev);
        /* casinha/red still come to the front on the way down. Every captured key is consumed: the
         * dedicated app keys are ours to remap now, and the Rust side decides what each does. */
        if (aType == EEventKeyDown && isHome)
            ShimRaiseSelf();
        return EKeyWasConsumed;
        }

    /* The dedicated app keys ALSO arrive as a translated character (EEventKey), on a capture table
     * separate from the scancode up/downs above — and it was this char event, not the scancode, that
     * the E72 was launching Contacts/Calendar from. We capture these chars in shim_set_resident, so
     * consuming them here is what finally suppresses the default launch. Report each as a RAWKEY with
     * b=2 (the "char edge", distinct from 0 down / 1 up) purely so the launcher log shows the code;
     * the binding itself already fired off the scancode, so we do not re-dispatch here. */
    if (gResident && aType == EEventKey
        && aKeyEvent.iCode >= EKeyApplication0
        && aKeyEvent.iCode <= EKeyApplication0 + (KAppCharCount - 1))
        {
        ShimEvent ev;
        ev.kind = SHIM_EV_RAWKEY;
        ev.handle = 0;
        ev.status = SHIM_OK;
        ev.a = (TInt) aKeyEvent.iCode;
        ev.b = 2;
        ev.c = 0;
        ev.d = (TInt) aKeyEvent.iScanCode;
        ev.native = 0;
        ShimPushEvent(ev);
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

            /* The red End key, as a translated character. For a resident launcher red means "go
             * home": bring the launcher to the front and consume it, the same thing the menu key
             * does — deterministically, every time. A non-resident app passes it on so the
             * framework closes it as normal. */
            if (aKeyEvent.iCode == EKeyNo)
                {
                if (gResident)
                    {
                    ShimRaiseSelf();
                    return EKeyWasConsumed;
                    }
                return EKeyWasNotConsumed;
                }
            return EKeyWasConsumed;
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

    /* A Ctrl chord is ours, and this is the one place that can say so.
     *
     * Ctrl+C, Ctrl+V and the rest arrive here rather than above, because the control character
     * they carry (0x03, 0x16 ...) is below the printable gate and in no key map. The Rust side
     * turns them into `Key::Ctrl` and acts on them — a text field pastes — so passing the same
     * press on to Avkon invites it to act a second time on a key we have already spent.
     *
     * Everything else with no name keeps going, which is what the paragraph above is about. */
    if (aKeyEvent.iModifiers & EModifierCtrl)
        return EKeyWasConsumed;

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
    static void PumpKick();
    TInt Pump();

    CShimControl* iControl;
    CIdle* iPump;
    };

/* The single app-ui instance, so the free-function-style pump kick registered with the event ring
 * can reach the pump. One GUI process has one app-ui and one pump; set in ConstructL, cleared in
 * the destructor. */
CShimAppUi* gShimAppUi = NULL;

/* Registered with the event ring: wake the drain pump when an event lands on a queue that had let
 * it go to sleep. IsActive() makes it idempotent — a push while the pump is mid-drain finds it
 * already awake and does nothing, which matters because CIdle::Start on an active object panics. */
void CShimAppUi::PumpKick()
    {
    if (gShimAppUi && gShimAppUi->iPump && !gShimAppUi->iPump->IsActive())
        gShimAppUi->iPump->Start(TCallBack(&CShimAppUi::PumpCallback, gShimAppUi));
    }

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
     * time to redraw. It drains the event ring, and then — this is the 2026 change — sleeps
     * if the ring came up empty (Pump returns EFalse) instead of re-arming unconditionally.
     * A push onto the empty queue wakes it again through PumpKick, registered just below.
     *
     * Why it changed: re-arming every pass made the pump *permanently ready*. In the
     * foreground that only wasted a battery on a phone that is barely touched; in the
     * background it was worse than wasteful. There is no window-server work above a
     * backgrounded app to yield to, so the CIdle span rust_step() flat out, and active-object
     * priority — which orders work only *within* our thread — bought nothing against another
     * *process*. The nanokernel round-robins threads by thread priority, and a thread that
     * never blocks takes its whole timeslice every pass. A trivial foreground app (Calculator)
     * never noticed; Messaging, which does real work per keypress, went sluggish enough that
     * its D-pad read as frozen, and only ever while this launcher sat behind it. Sleeping when
     * idle makes a backgrounded app cost nothing, which is what lets a resident home screen
     * live behind a heavy app at all.
     *
     * The priority still matters for the passes it does run. Symbian dispatches the
     * highest-priority ready object and, among equals, the one added first, so a ready object
     * at EPriorityIdle starves every object added after it at that priority. `CImageDecoder`
     * drives its decode from active objects that sit at idle priority on the E72's vendor JPEG
     * codec; `examples/imgprobe` measured a decode issued from `rust_app_start` (queued ahead
     * of the pump) finish in 241 ms while the byte-identical decode issued from inside
     * `rust_step` never completed. One below EPriorityIdle says "lowest in the process" without
     * inventing a magnitude. */
    iPump = CIdle::NewL(CActive::EPriorityIdle - 1);
    gShimAppUi = this;
    ShimSetPumpKick(&CShimAppUi::PumpKick);
    iPump->Start(TCallBack(&CShimAppUi::PumpCallback, this));
    }

void CShimAppUi::HandleForegroundEventL(TBool aForeground)
    {
    /* Push a focus event so Rust knows whether we are in the foreground.
     * `a` is 1 for foreground, 0 for background. */
    ShimPushSimple(SHIM_EV_FOCUS, 0, SHIM_OK, aForeground ? 1 : 0);
    /* Track it for the resident key handling: the End key is only consumed while we are the
     * foreground app, so it never leaks into another application. */
    gForeground = aForeground;

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
    /* Unhook the kick before deleting the pump: an event pushed during teardown (a late RunL, a
     * cleanup that logs) must not try to Start a pump that is being freed. */
    ShimSetPumpKick(NULL);
    gShimAppUi = NULL;
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
#ifdef SHIM_USE_SQL
    /* Before the files only for tidiness — the SQL server owns its own file handles and
     * none of ours. What does matter is that it runs at all: a connection left open past
     * process exit keeps server-side state alive until the server notices. */
    ShimSqlCleanup();
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

    /* Sleep unless there is more to drain. rust_step polls the ring empty in the common case, so
     * this normally returns EFalse and the pump stops until the next ShimPushEvent kicks it awake
     * — zero CPU while nothing is happening, which is almost always, on a phone barely touched.
     * If rust_step left events behind (it processes a bounded batch, or a step pushed a follow-up),
     * ShimEventCount() is non-zero and we re-arm to finish them on the next pass. Either way, work
     * that arrives while we sleep re-arms us through PumpKick, so nothing is stranded in the ring. */
    return ShimEventCount() > 0 ? ETrue : EFalse;
    }

void CShimAppUi::HandleCommandL(TInt aCommand)
    {
    switch (aCommand)
        {
        case EEikCmdExit:
        case EAknSoftkeyExit:
            /* The red End key ends here: the framework turns an unconsumed EKeyNo into an exit
             * command. A resident launcher must not die from that — a home screen steps aside to
             * the idle behind it and stays alive, so the Menu/Apps key can bring it back. That is
             * exactly what GDesk does (its handler captures the apps key and never touches red, and
             * it survives being backgrounded). So when resident, go to the background instead of
             * exiting; only a non-resident launcher actually closes. */
            /* A resident launcher never exits from the red key: the key handler already brought it
             * to the front, so if an exit command still arrives it is ignored. Only a non-resident
             * launcher actually closes. */
            if (!gResident)
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

/* Turn resident (launcher) behaviour on or off. On: capture the Menu key so pressing it brings
 * this app forward, and make End consume-and-stay rather than close (both handled in
 * OfferKeyEventL). The Menu key is captured by SCAN code with CaptureKeyUpAndDowns — it is a
 * hardware key that does not arrive as a translated character, so capturing by key code (the
 * earlier EKeyApplication0 attempt) never matched. Both EStdKeyMenu and EStdKeyApplication0 are
 * captured because Nokia hardware differs on which the "menu" key emits. Capturing needs SwEvent;
 * the launcher declares it, matching GDesk. Safe before the window group exists — returns
 * SHIM_ERR_NOT_READY — though the launcher calls it from rust_app_start, by which point CCoeEnv
 * is up. */
/* Bring our own window group to the front, the way the shell does it.
 *
 * `RWindowGroup::SetOrdinalPosition(0)` looks like it does this and does not do it reliably: it
 * moves the group to the front *of its own priority band*, and it does not move focus. The symptom
 * on this handset is a launcher that comes up on roughly one press in three — pressed again and
 * again until something else happens to reshuffle the z-order.
 *
 * `TApaTask::BringToForeground` is the operation the shell performs, focus included. Same class of
 * mistake as trying to open a URL in a browser that is already running: the call that *starts*
 * something is not the call that talks to something already up. */
static void ShimRaiseSelf()
    {
    CCoeEnv* env = CCoeEnv::Static();
    if (!env)
        return;
    TApaTask task(env->WsSession());
    task.SetWgId(env->RootWin().Identifier());
    task.BringToForeground();
    }

/* Drop this application behind whatever else is on screen, without closing it.
 *
 * For a helper the user never asked to see. A GUI app is brought to the foreground when it is
 * started, which is right for something launched from a menu and wrong for a background job the
 * home screen kicked off — the icon builder needs Avkon (so it cannot be a headless daemon) but has
 * no business taking the screen from the launcher that started it.
 *
 * The same TApaTask move shim_net.cpp uses to get out of the way after opening a connection. */
extern "C" int32_t shim_app_to_background(void)
    {
    CCoeEnv* env = CCoeEnv::Static();
    if (!env)
        return SHIM_ERR_NOT_READY;
    TApaTask task(env->WsSession());
    task.SetWgId(env->RootWin().Identifier());
    task.SendToBackground();
    return SHIM_OK;
    }

/* Bring this application back to the front, focus and all.
 *
 * The mirror of shim_app_to_background, and it exists for one measured reason: killing another
 * app's task can leave *that* app in front — the platform restarts some of them, and a window group
 * dying reshuffles the z-order — which puts the user back in the application they just asked to
 * close. A task manager that has to be dug back out is not one.
 *
 * Same call the captured Menu key makes (see ShimRaiseSelf on why it is TApaTask rather than
 * RWindowGroup::SetOrdinalPosition). */
extern "C" int32_t shim_app_to_foreground(void)
    {
    CCoeEnv* env = CCoeEnv::Static();
    if (!env)
        return SHIM_ERR_NOT_READY;
    ShimRaiseSelf();
    return SHIM_OK;
    }

extern "C" int32_t shim_set_resident(int32_t on)
    {
    CCoeEnv* env = CCoeEnv::Static();
    if (!env)
        return SHIM_ERR_NOT_READY;
    RWindowGroup& wg = env->RootWin();
    if (on && !gResident)
        {
        /* CaptureKeyUpAndDowns returns a handle (>= 0) or a negative error. Capture each key we
         * care about; a refused one just will not route to us, the others still do. */
        for (TInt i = 0; i < KResidentKeyCount; i++)
            gKeyCaptures[i] = wg.CaptureKeyUpAndDowns(KResidentKeys[i], 0, 0, KResidentKeyPriority);
        /* ...and the character events for the app keys, the other half of the remap (see above). */
        for (TInt i = 0; i < KAppCharCount; i++)
            gCharCaptures[i] = wg.CaptureKey(EKeyApplication0 + i, 0, 0, KResidentKeyPriority);
        gResident = ETrue;
        }
    else if (!on && gResident)
        {
        for (TInt i = 0; i < KResidentKeyCount; i++)
            {
            if (gKeyCaptures[i] >= 0)
                wg.CancelCaptureKeyUpAndDowns(gKeyCaptures[i]);
            gKeyCaptures[i] = -1;
            }
        for (TInt i = 0; i < KAppCharCount; i++)
            {
            if (gCharCaptures[i] >= 0)
                wg.CancelCaptureKey(gCharCaptures[i]);
            gCharCaptures[i] = -1;
            }
        gResident = EFalse;
        }
    return SHIM_OK;
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
