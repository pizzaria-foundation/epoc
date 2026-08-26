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

/* The event-driven pump. The GUI build registers a "kick" — a nudge that wakes its drain pump —
 * so that pushing an event onto an empty-and-sleeping queue restarts the drain immediately, and a
 * queue that stays empty lets the pump sleep instead of spinning. ShimPushEvent calls the kick on
 * every successful push (the kick itself is cheap and idempotent when the pump is already awake).
 * A build with no registered kick (the headless daemon, which polls on a CPeriodic) is unaffected.
 * ShimEventCount lets the pump ask "is there more to drain?" before deciding to sleep. */
void ShimSetPumpKick(void (*aKick)());
TInt ShimEventCount();

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

/* Guarded by the same flag that puts the source file into the build, so an app that did not
 * opt into a facility neither compiles it nor references its cleanup. */
#ifdef SHIM_USE_PROP
/* Cancel any outstanding P&S subscription and close the handles. */
void ShimPropCleanup();
#endif

/* Close every open file and the file server session. Called from the app's
 * teardown for the same reason as the timers: a leaked RFile keeps a file server
 * handle alive past process exit, and the panic that follows names the file server
 * rather than us. */
void ShimFilesCleanup();

/* The shim's one file server session, opened on first use. Exposed so the image
 * decoder does not open a second one per decode — and so it cannot repeat the bug
 * it had, which was closing its own session while a CImageDecoder still held an
 * RFile subsession on it. There is one session and nobody but ShimFilesCleanup
 * closes it. */
class RFs;
TInt ShimFsSession(RFs*& aOut);

/* Close every socket, resolver and bearer, and the socket server session. Sockets
 * before bearers: a socket being closed still belongs to one.
 *
 * Only defined when the app set USE_NET=1, which is also what puts shim_net.cpp into
 * the build; the call site in shim_app.cpp is guarded by the same SHIM_USE_NET. */
#ifdef SHIM_USE_NET
void ShimNetCleanup();

/* The socket server handle and the RConnection behind a bearer handle from shim_net_start.
 *
 * Exists for shim_http.cpp, which has to hand both to RHTTPSession's connection info so the HTTP
 * stack goes out over the bearer this process already brought up instead of opening a second one.
 * `aConn` comes back as a bare pointer because this header is included by every shim file and must
 * not drag in es_sock.h for the two that need it; the caller casts. Fails with KErrNotReady while
 * the bearer is still coming up, which is the honest answer — RHTTPSession bound to a connection
 * that was never started is the same esock client panic that shim_net.cpp documents for sockets.
 *
 * Wait for a bearer handle to be up before calling. */
TInt ShimNetBearer(TInt aNetHandle, TInt& aServHandle, TAny*& aConn);

/* Wait for any running job and close the worker thread. Waiting is the point: the job
 * holds pointers into buffers the caller is about to free. */
void ShimWorkCleanup();
#endif

/* Close the HTTP session and abandon any transaction in flight. Before ShimNetCleanup: the
 * session holds the RConnection that cleanup is about to close. */
#ifdef SHIM_USE_HTTP
void ShimHttpCleanup();
#endif

/* Cancel every outstanding decode and free the bitmaps. Cancel before free, for the
 * usual reason: an ICL plugin mid-Convert is writing into that bitmap.
 *
 * Guarded like the net cleanup, and by the same flag that puts shim_image.cpp into the
 * build — an app that does not decode images should not import imageconversion.dll. */
#ifdef SHIM_USE_IMAGE
void ShimImageCleanup();
#endif

/* Stops playback and releases the player. Before ShimFilesCleanup, because the media
 * framework holds the clip open.
 *
 * Guarded like the image cleanup, and for the same reason: an app that plays no sound
 * should not import mediaclientaudio.dll. */
#ifdef SHIM_USE_AUDIO
void ShimAudioCleanup();
#endif

/* Cancel the position request and close the sub-session and the session, in that order — a
 * positioner outliving its server is the same orphaned-handle panic as every other pair here.
 *
 * Guarded like the audio cleanup: an app that does not want a position should not import lbs.dll,
 * and it should certainly not be holding a GPS subscription open past its own exit. */
#ifdef SHIM_USE_LBS
void ShimLbsCleanup();
#endif

/* Cancel any cell read and close the telephony session. Guarded like the rest: an app that never
 * asks which tower it is on should not import etel3rdparty. */
#ifdef SHIM_USE_CELL
void ShimCellCleanup();
#endif

/* Finalise every open statement, then close every open database. Guarded like the audio
 * cleanup: an app that stores nothing in SQL should not import sqldb.dll, and an import
 * the handset cannot satisfy stops the image loading with no error and no report file. */
#ifdef SHIM_USE_SQL
void ShimSqlCleanup();
#endif

/* Close the Bluetooth registry session and drop the cached device views. Guarded like the
 * others: an app that does not manage Bluetooth should not import btmanclient, and six
 * unsatisfied imports stop the image loading with no error and no report file. */
#ifdef SHIM_USE_BT
void ShimBtCleanup();
#endif

/* Close the RFCOMM listener, deregister its SDP record, and close every accepted socket.
 * Guarded like the others: sdpdatabase is an import an app that is not an RFCOMM server has
 * no reason to carry. */
#ifdef SHIM_USE_BTSOCK
void ShimBtsockCleanup();
#endif

/* The FEP-aware editor, or NULL when the scan-code path is selected.
 *
 * Returned from CShimControl::InputCapabilities, which the framework calls during its own
 * traversal and cannot allocate from -- so the editor is created by ShimFepInit when the
 * control is constructed. */
#ifdef SHIM_USE_FEP
class MCoeFepAwareTextEditor;
MCoeFepAwareTextEditor* ShimFepEditor();
void ShimFepInit();
void ShimFepCleanup();
#endif

#endif /* SHIM_PRIV_H */
