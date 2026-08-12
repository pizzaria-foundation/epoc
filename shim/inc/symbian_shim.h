/* The flat C ABI between Symbian C++ and Rust.
 *
 * This header is the contract. Both sides include it — C++ directly, Rust through
 * crates/symbian-sys, which mirrors it by hand.
 *
 * THREE RULES, all of them load-bearing.
 *
 * 1. Every `shim_*` function is a TRAP barrier and returns a Symbian error code.
 *    A Symbian Leave is a longjmp-style unwind that does not run destructors —
 *    that is why CleanupStack exists. Letting one cross a Rust frame compiled
 *    panic=abort, which has no landing pads, skips every Drop and is undefined
 *    behaviour, not merely a leak. So the leaving work stays in a private
 *    DoSomethingL() and the exported wrapper TRAPs it. The few functions that
 *    genuinely cannot Leave say so in a comment and skip the TRAP.
 *
 * 2. Rust never blocks and never owns the loop. Avkon calls
 *    CActiveScheduler::Start(); there is no taking that away. Every asynchronous
 *    completion is converted by a CActive::RunL() into a POD ShimEvent on a ring
 *    buffer, and a CIdle pump calls rust_step(), which drains the queue. That is
 *    the same shape as a winit ApplicationHandler.
 *
 * 3. Handles are opaque int32_t, never pointers. A handle table turns a
 *    use-after-free into KErrBadHandle instead of a crash, and keeps C++ object
 *    lifetimes invisible to Rust.
 *
 * Strings cross as (const uint16_t*, int32_t len) — UTF-16 code units, which
 * TPtrC16 wraps with no copy. Rust keeps UTF-8 internally and converts at the
 * boundary.
 */

#ifndef SYMBIAN_SHIM_H
#define SYMBIAN_SHIM_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ---------------------------------------------------------------- errors --
 * A subset of e32err.h, repeated so Rust has the values without parsing headers.
 * KErrPermissionDenied is the one to expect when a capability is missing. */
#define SHIM_OK                    0
#define SHIM_ERR_NOT_FOUND        -1
#define SHIM_ERR_GENERAL          -2
#define SHIM_ERR_CANCEL           -3
#define SHIM_ERR_NO_MEMORY        -4
#define SHIM_ERR_NOT_SUPPORTED    -5
#define SHIM_ERR_ARGUMENT         -6
#define SHIM_ERR_BAD_HANDLE       -8
#define SHIM_ERR_OVERFLOW         -9
#define SHIM_ERR_ALREADY_EXISTS  -11
#define SHIM_ERR_IN_USE          -14
#define SHIM_ERR_NOT_READY       -18
#define SHIM_ERR_ACCESS_DENIED   -21
#define SHIM_ERR_EOF             -25
#define SHIM_ERR_TIMED_OUT       -33
#define SHIM_ERR_DISCONNECTED    -36
#define SHIM_ERR_PERMISSION      -46

/* ----------------------------------------------------------------- events -- */
enum ShimEventKind {
    SHIM_EV_NONE = 0,
    /* Translated character in `a` — Shift, Caps Lock and the Fn layer have
     * already been applied by the window server. This is the text input stream. */
    SHIM_EV_KEY_CHAR = 1,
    /* Raw scan code in `a`, for keys with no character (softkeys, D-pad). */
    SHIM_EV_KEY_DOWN = 2,
    SHIM_EV_KEY_UP = 3,
    /* The framework wants us to repaint; `a`..`d` are the dirty rect. */
    SHIM_EV_REDRAW = 4,
    SHIM_EV_RESIZE = 5,
    /* Went to background or came back; `a` is 1 for foreground. */
    SHIM_EV_FOCUS = 6,
    SHIM_EV_TIMER = 10,
    /* Socket: `a` is the byte count, `status` the Symbian error. */
    SHIM_EV_CONNECTED = 20,
    SHIM_EV_RECV = 21,
    SHIM_EV_SENT = 22,
    SHIM_EV_CLOSED = 23,
    SHIM_EV_RESOLVED = 24,
    /* RConnection is up. `a` is the IAP id the OS actually chose, which is worth
     * persisting: passing it back to shim_net_start next time connects with no
     * prompt. */
    SHIM_EV_NET_READY = 25,
    /* A worker-thread job finished. `status` is what rust_work returned. */
    SHIM_EV_WORK_DONE = 30,
    /* An image decode finished. `a` and `b` are the decoded width and height,
     * which are the size the codec could actually deliver and not necessarily
     * the size that was asked for — see shim_image_decode_start. Call
     * shim_image_result to collect the pixels, then shim_image_close. */
    SHIM_EV_IMAGE_DONE = 40,
    /* An audio clip finished opening. `a` is its duration in milliseconds, `status` the
     * Symbian error. `handle` is the open generation, which is what tells a caller that
     * this belongs to the clip it just asked for and not to one already dismissed. */
    SHIM_EV_AUDIO_OPENED = 41,
    /* Playback ended. `status` is SHIM_OK for a clip that reached its end, KErrCancel
     * when shim_audio_stop caused it, and a real error otherwise — notably
     * SHIM_ERR_IN_USE, which arrives mid-playback when the ringtone takes the device.
     * `d` carries the platform's raw code, since "ended" and "ended by underflow" are
     * one outcome to a caller and two different facts in a probe report. */
    SHIM_EV_AUDIO_DONE = 42,
    /* A subscribed Publish & Subscribe property changed. `a` is the key, `c` the freshly
     * read integer value. From shim_prop; the daemon uses it as its stop signal. */
    SHIM_EV_PROP = 53,
    /* The app should exit; nothing may be queued after it. */
    SHIM_EV_QUIT = 90
};

/* Subtypes carried in `a` of SHIM_EV_APP_EVENT. */
#define SHIM_APP_EV_LIST  0   /* running window-group set changed (launch/exit) */
#define SHIM_APP_EV_FOCUS 1   /* foreground window group changed (app switch) */

/* Deliberately POD and fixed-size so RunL() can push one without allocating and
 * without any chance of leaving. */
typedef struct ShimEvent {
    int32_t kind;
    /* Which socket, timer or window the event belongs to; 0 when global. */
    int32_t handle;
    /* Symbian error code, SHIM_OK on success. */
    int32_t status;
    int32_t a, b, c, d;
    /* Platform-native extra. For key events this is the raw Symbian iModifiers
     * word, unmasked.
     *
     * `b` carries a three-bit summary (shift/ctrl/func) because that is all a
     * portable toolkit should care about — but a summary is exactly what made the
     * E72's keyboard bug hard to see: `b` read 00 for every key, which only ever
     * meant "none of those three", and said nothing about EModifierNumLock,
     * EModifierKeyboardExtend or EModifierPureKeycode. A diagnostic needs the whole
     * word; apps should keep using `b`. */
    int32_t native;
} ShimEvent;

/* Pull one event. Returns 1 when `out` was filled, 0 when the queue is empty.
 * Cannot leave. */
int32_t shim_poll_event(ShimEvent* out);

/* How many events were dropped because the ring was full, and reset the counter.
 * A non-zero result means rust_step() is not keeping up — worth surfacing rather
 * than silently losing input. */
int32_t shim_events_dropped(void);

/* -------------------------------------------------------------- lifecycle --
 * Called by the shim INTO Rust. Rust must export these. */
void rust_app_start(void);
void rust_app_stop(void);
/* Drain events, update, redraw. Must return within a few milliseconds: it runs
 * on the GUI thread, and a long one starves the window server so the phone
 * appears frozen. */
void rust_step(void);

/* Ask the app to close. */
void shim_request_exit(void);

/* ------------------------------------------------------------ framebuffer --
 * The back buffer is a CFbsBitmap, whose pixels live in a chunk shared with the
 * font and bitmap server and mapped into the window server too. So Rust writes
 * straight into memory the window server blits from — no copy crosses a process
 * boundary. */

enum ShimPixelFormat {
    /* 16bpp, RRRRRGGG GGGBBBBB. What symbian-gfx renders natively. */
    SHIM_PF_RGB565 = 1,
    /* 32bpp 0x00RRGGBB. Reported for information; Rust is always handed RGB565
     * (see shim_fb_lock). */
    SHIM_PF_XRGB8888 = 2
};

typedef struct ShimFb {
    uint8_t* pixels;
    /* BYTES per scanline. Symbian aligns CFbsBitmap scanlines to 4 bytes, so a
     * 320-wide bitmap is not guaranteed to have stride == width * bpp. Always
     * read this; never compute it. */
    int32_t stride;
    int32_t width;
    int32_t height;
    /* Always SHIM_PF_RGB565. Present so a future format is not an ABI break. */
    int32_t format;
} ShimFb;

/* Take the lock and hand out the pixel pointer.
 *
 * The pointer is valid only until shim_fb_unlock(): CFbsBitmap::DataAddress()
 * must be preceded by BeginDataAccess() on 9.1+ or it crashes, and the server
 * heap may compact in between, so re-fetch after every lock rather than caching.
 *
 * Do not call any other shim function while holding the lock.
 *
 * Rust always receives RGB565. If the screen is 32bpp the shim renders into its
 * own RGB565 staging buffer and converts during present — one pass over 76800
 * pixels, paid only on hardware that needs it. */
int32_t shim_fb_lock(ShimFb* out);
/* Cannot leave. */
void shim_fb_unlock(void);

/* Blit the dirty rectangle to the screen and flush the window server queue.
 * Without the flush the frame sits in the client-side command buffer and appears
 * late. Pass the whole screen for a full repaint. */
int32_t shim_present(int32_t x, int32_t y, int32_t w, int32_t h);

int32_t shim_screen_size(int32_t* w, int32_t* h);
/* The mode the window server actually reports, as a ShimPixelFormat. Query it;
 * the E72's panel is 24-bit but which Symbian display mode it exposes is not
 * documented anywhere we could find. */
int32_t shim_screen_format(int32_t* format);

/* Fill a 1x1 bitmap with pure red through the documented TRgb API and return the
 * first word of its memory. Turns "which byte is red?" from a guess into a fact,
 * on whatever device this happens to be running. */
int32_t shim_probe_pixel_layout(uint32_t* out_word);

/* Is a DLL present on this device? Returns SHIM_OK if it loads, or the Symbian
 * error (KErrNotFound is -1).
 *
 * This is a real capability query, not only a diagnostic. The SDK links against a
 * device ROM we cannot inspect: Open C (libc, libcrypto, libssl, libz) shipped as a
 * separate package on S60 3rd Edition, so whether it exists is a property of the
 * handset and not of the SDK. Importing a missing DLL is the worst way to find out —
 * the E32 loader refuses to start the process, which on a phone looks exactly like
 * "the icon does nothing", with no error and no log. Asking first turns that into a
 * value.
 *
 * `name` is the DLL's filename, e.g. "libcrypto.dll" — the .dll name, not the .dso
 * import library the linker sees. */
int32_t shim_dll_present(const uint16_t* name, int32_t len);

/* This process's own UID3 (the -DSHIM_APP_UID3 value), used as the Publish & Subscribe
 * category an app publishes its own telemetry in. Zero if the build did not set it. */
uint32_t shim_own_uid3(void);

/* Fill `out` with entropy — NOT with random numbers.
 *
 * The distinction matters enough to be in the name of the thing. What comes back is a
 * mixture of Math::Random, a high-resolution counter sampled inside the loop so it catches
 * scheduling jitter, uptime, the wall clock, a stack address and the heap's free space.
 * It is not uniform and no single source in it is known to be unpredictable.
 *
 * Whitening happens in Rust, where the tested SHA-256 lives: see `symbian::random`, which
 * runs a DRBG over this and is the thing callers should actually use. Using this directly
 * as key material would be a mistake.
 *
 * Deliberately not random.dll's CSystemRandom, which is the platform's real CSPRNG: a new
 * DLL dependency is a deployment risk that cannot be tested for from the host, and this
 * runs on every launch. `examples/selftest` probes random.dll so that decision can be
 * revisited against an answer rather than a guess. */
int32_t shim_entropy(uint8_t* out, int32_t len);

/* ---------------------------------------------------------------- keyboard --
 *
 * Which mechanism turns a physical key into a character.
 *
 * Both ship in every binary and either can be selected at run time, because only the
 * handset can say which works and six rounds went into the bearer testing one guess per
 * build. It also means a FEP that does not fire leaves a working keyboard rather than
 * none. */

/* The shim's own scan-code table. Tested on hardware: letters, digits and the twelve
 * overlay keys. Does not produce the Fn symbol layer -- Fn+Q gives 'q'. */
#define SHIM_KEYBOARD_SCAN 0

/* Symbian's front-end processor. Advertises a MCoeFepAwareTextEditor through
 * CCoeControl::InputCapabilities, which is what makes CAknFepManager involve itself at
 * all -- declaring EAllText with a null editor was tried on device and changed nothing.
 *
 * If it works it gives the whole Fn layer, which is what a two-factor password needs. */
#define SHIM_KEYBOARD_FEP  1

/* Select one. SHIM_ERR_NOT_READY if the FEP editor has not been created yet, which means
 * before the control exists. */
int32_t shim_keyboard_mode(int32_t mode);
int32_t shim_keyboard_mode_get(void);

/* -------------------------------------------------------------------- text --
 * Text is drawn by Symbian into the same buffer Rust owns pixels of. That gets
 * real hinted glyphs and full UCS-2 coverage for nothing, and avoids decoding
 * Symbian's undocumented RLE glyph bitmaps. symbian-gfx's own .sbf atlas remains
 * the portable path, used for the host preview and when guaranteed coverage
 * matters more than binary size. */

enum ShimSystemFont {
    SHIM_FONT_NORMAL = 0,
    SHIM_FONT_TITLE = 1,
    SHIM_FONT_ANNOTATION = 2,
    SHIM_FONT_LEGEND = 3,
    SHIM_FONT_DENSE = 4
};

int32_t shim_font_open_system(int32_t which, int32_t* handle);
/* Nearest match to a pixel design height. */
int32_t shim_font_open_size(int32_t px, int32_t bold, int32_t* handle);
/* Cannot leave. Releasing a system font is a no-op: CEikonEnv owns those. */
void shim_font_close(int32_t handle);

typedef struct ShimFontMetrics {
    int32_t height;
    int32_t ascent;
    int32_t descent;
    int32_t max_width;
} ShimFontMetrics;

int32_t shim_font_metrics(int32_t handle, ShimFontMetrics* out);
/* Width in pixels, or a negative error. */
int32_t shim_text_width(int32_t handle, const uint16_t* text, int32_t len);
/* `y` is the BASELINE, not the top: that is what CGraphicsContext::DrawText
 * takes, and pretending otherwise would misplace every string by the ascent. */
int32_t shim_text_draw(int32_t handle, int32_t x, int32_t y,
                       const uint16_t* text, int32_t len, uint32_t rgb);

/* ------------------------------------------------------------------ timers -- */
/* One-shot. Completion arrives as SHIM_EV_TIMER carrying this handle. */
int32_t shim_timer_after(int32_t ms, int32_t* handle);
/* Repeating, for a frame clock. */
int32_t shim_timer_every(int32_t ms, int32_t* handle);
void shim_timer_cancel(int32_t handle);
/* Monotonic microseconds, for measuring elapsed time. Cannot leave. */
uint64_t shim_now_us(void);
/* Seconds since the Unix epoch, for message timestamps. The device clock drifts;
 * a networked app should correct against the server rather than trust this. */
int64_t shim_unix_time(void);

/* Seconds east of UTC (local minus UTC), from User::UTCOffset.
 * Negative for the Americas, positive for Europe. */
int32_t shim_utc_offset(void);

/* ------------------------------------------------------------------ images --
 * Decoding, through CImageDecoder — which handles whatever the device has a
 * plugin for: JPEG, PNG, GIF and BMP on every S60 3rd handset.
 *
 * ASYNCHRONOUS, and that is not a preference. CImageDecoder::Convert is driven by
 * an active object in the *calling* thread — the plugin self-completes to decode
 * in slices, which is how it avoids monopolising the scheduler. Calling
 * User::WaitForRequest on it from rust_step, which is itself a CIdle callback,
 * therefore deadlocks: the scheduler cannot dispatch the decoder's RunL while we
 * sit in the wait. It is not a slow path, it is a frozen phone.
 *
 * So decoding takes the shape everything else asynchronous here takes: start it,
 * get SHIM_EV_IMAGE_DONE, collect the pixels.
 *
 * SIZE IS A REQUEST, NOT AN INSTRUCTION. The ICL only reduces by powers of two
 * (1/1, 1/2, 1/4, 1/8, and only what the codec supports — JPEG usually all four,
 * PNG usually none). max_w/max_h pick the largest reduction that still fits;
 * the event reports what came out, which is never larger than the request but is
 * rarely exactly it. Any final resampling is the caller's. */

/* Dimensions without decoding pixels. Synchronous: this only parses the header,
 * which is bounded work and does not go through Convert. */
int32_t shim_image_probe(const uint16_t* path, int32_t path_len, int32_t* w, int32_t* h);

/* Begin a decode to RGB565. Completion arrives as SHIM_EV_IMAGE_DONE carrying
 * `*handle`, with the decoded width in `a` and height in `b`.
 *
 * _mem decodes from a buffer, which is what a download actually has. The buffer
 * must stay put and unmodified until the event arrives — the decoder reads from it
 * rather than copying, and the caller's Vec is the only owner. */
int32_t shim_image_decode_start(const uint16_t* path, int32_t path_len,
                                int32_t max_w, int32_t max_h, int32_t* handle);
int32_t shim_image_decode_start_mem(const uint8_t* data, int32_t len,
                                    int32_t max_w, int32_t max_h, int32_t* handle);

/* Copy the decoded pixels out, after SHIM_EV_IMAGE_DONE reported success.
 * `out_cap` is in pixels; SHIM_ERR_OVERFLOW if it is short of width*height.
 * Fills `*w`/`*h` with the same values the event carried. */
int32_t shim_image_result(int32_t handle, uint16_t* out, int32_t out_cap,
                          int32_t* w, int32_t* h);

/* Nine diagnostic integers about a decode, in this order:
 *
 *   0  state: 1 still pending, 2 completed
 *   1  completion code, meaningful once state is 2
 *   2  frames the decoder found, or -1 if it never got that far
 *   3  native width      4  native height
 *   5  power-of-two reduction chosen, or -1
 *   6  bitmap width      7  bitmap height
 *   8  1 while the request is still outstanding
 *
 * A poll rather than a log, because a decode that never completes emits no event and
 * the shim has no channel of its own — the only way to learn anything about one is for
 * the caller to ask. Writes min(cap, 9) values. */
int32_t shim_image_describe(int32_t handle, int32_t* out, int32_t cap);

/* Release the slot. Cancels an outstanding decode; safe on a handle that already
 * completed, and safe on 0. Every successful _start needs one of these, or the
 * slot and its bitmap leak. */
void shim_image_close(int32_t handle);

/* ---------------------------------------------------------------- sockets --
 * Every ESock operation is asynchronous — there are no synchronous variants of
 * Connect, Send or Recv. Each call here returns immediately and the completion
 * arrives as an event.
 *
 * S60 will not silently pick a bearer, so a connection must be started first;
 * SHIM_IAP_PROMPT lets the OS ask the user once per session. NetworkServices is
 * the only capability any of this needs, and it is user-grantable. */

#define SHIM_IAP_PROMPT   (-1)
#define SHIM_IAP_DEFAULT  (-2)

/* Join a connection that is already up, rather than negotiating one.
 *
 * What "every other program on the phone works" actually means: something else brought an
 * interface up and a client joins it. RConnection::Attach, so it is synchronous, shows no
 * dialog and cannot time out -- but it still completes through the usual event, because a
 * caller should not have to know that one strategy answers by a different mechanism.
 *
 * SHIM_ERR_NOT_FOUND when nothing is up, which is the honest answer and the signal to fall
 * through to SHIM_IAP_PROMPT.
 *
 * This replaces a path that opened a socket with no RConnection at all. That relies on a
 * *configured default connection*, not on one being up -- so on a handset with none it
 * reported success and then every connect timed out underneath it. */
#define SHIM_IAP_ATTACH   (-3)

/* Bring up a bearer. Returns immediately with a handle; completion is
 * SHIM_EV_NET_READY, whose `a` carries the IAP the OS settled on.
 *
 * `iap` is SHIM_IAP_PROMPT to let the OS ask, SHIM_IAP_DEFAULT to take the
 * configured default, or a positive id from a previous SHIM_EV_NET_READY. The
 * intended shape is: prompt on first run, remember the answer, connect silently
 * afterwards, and fall back to prompting if the saved id has gone away. */
/* How many connections are up right now, or a negative error.
 *
 * The diagnostic that separates "nothing is online" from "we cannot join what is". Both
 * look identical from a socket that never connects. */
int32_t shim_net_connections(void);

/* The access point behind connection `index`. One-based by Symbian convention, which the
 * headers do not state -- so a caller that cares should try 1 and then 0. */
int32_t shim_net_connection_iap(int32_t index, int32_t* iap);

int32_t shim_net_start(int32_t iap, int32_t* handle);

/* Releases our handle. Deliberately not RConnection::Stop(): Stop tears down the
 * shared interface and would drop every other application's connection with it. */
void shim_net_stop(int32_t handle);

/* DNS. Completion is SHIM_EV_RESOLVED with the IPv4 address in `a`. */
int32_t shim_dns_resolve(int32_t conn, const uint16_t* host, int32_t len, int32_t* handle);

/* Abandon a lookup.
 *
 * There was no way to do this, and that was a leak with teeth. A resolver that is never
 * answered stays open holding whatever connection it was made against -- and on a handset
 * with no route, no lookup is ever answered. The self test then found the bearer sweep
 * answering KErrLocked on a prompt that had waited nearly two minutes, which is what a held
 * connection looks like from the other side.
 *
 * Safe on a handle that has already completed: the slot is empty and this does nothing. */
void shim_dns_close(int32_t handle);

int32_t shim_tcp_open(int32_t conn, int32_t* handle);
/* Completion: SHIM_EV_CONNECTED. */
int32_t shim_tcp_connect(int32_t handle, uint32_t ipv4, uint16_t port);
/* `buf` must stay alive and untouched until SHIM_EV_SENT arrives. */
int32_t shim_tcp_send(int32_t handle, const uint8_t* buf, int32_t len);
/* Likewise until SHIM_EV_RECV, whose `a` is the byte count. */
int32_t shim_tcp_recv(int32_t handle, uint8_t* buf, int32_t cap);
void shim_tcp_close(int32_t handle);

/* --------------------------------------------------------------------- UDP --
 * The same RSocket with KSockDatagram, so this is the TCP path with the addresses
 * moved from connect-time to per-message.
 *
 * On SHIM_EV_RECV from a UDP socket, `a` is the byte count as usual and `b` and `c`
 * carry the sender's address and port — which a datagram socket needs and a stream
 * socket has no use for. */
int32_t shim_udp_open(int32_t conn, int32_t* handle);
int32_t shim_udp_send_to(int32_t handle, const uint8_t* buf, int32_t len,
                         uint32_t ipv4, uint16_t port);
int32_t shim_udp_recv_from(int32_t handle, uint8_t* buf, int32_t cap);

/* ---------------------------------------------------------------- worker --
 * A second thread, for work too slow to do on the GUI thread.
 *
 * rust_step runs from a CIdle on the GUI thread and must return in milliseconds: a
 * long one starves the window server, which freezes the whole phone rather than just
 * this app. A 2048-bit modular exponentiation takes 0.4-0.6 s on this hardware, so
 * the login handshake of any real protocol cannot happen in the pump.
 *
 * THE JOB MUST NOT ALLOCATE. RThread::Create gives the new thread its own heap, so
 * memory allocated on the worker and freed on the GUI thread is a cross-heap free —
 * silent corruption, not a clean failure. The contract is therefore that a job reads
 * an input buffer and writes an output buffer that the *caller* allocated, and does
 * no allocation of its own. Fixed-size arithmetic over byte slices fits; anything
 * that builds a Vec does not.
 *
 * Both buffers must stay alive and untouched until SHIM_EV_WORK_DONE arrives.
 *
 * One job at a time. A queue here would be a scheduler, and the caller already has a
 * better one; submitting while busy returns SHIM_ERR_IN_USE. */
int32_t shim_work_submit(int32_t opcode, const uint8_t* in, int32_t in_len,
                         uint8_t* out, int32_t out_len);

/* Non-zero while a job is running. */
int32_t shim_work_busy(void);

/* Implemented by the application, called on the worker thread. See the allocation
 * rule above; `symbian_app::entry!` wires this up and defaults it to a stub. */
extern int32_t rust_work(int32_t opcode, const uint8_t* in, int32_t in_len,
                         uint8_t* out, int32_t out_len);

/* ------------------------------------------------------------------- files --
 * RFile has genuine synchronous overloads that need no active scheduler, so this
 * is a plain blocking API with no event plumbing. A welcome exception. */

#define SHIM_FILE_READ   0x01
#define SHIM_FILE_WRITE  0x02
#define SHIM_FILE_CREATE 0x04
#define SHIM_FILE_APPEND 0x08

/* The app's private data cage, C:\private\<UID3>\, created if absent. Needs no
 * capability: it is our own directory and only our SID (or an AllFiles holder)
 * can reach it. */
int32_t shim_private_path(uint16_t* buf, int32_t cap, int32_t* len);

int32_t shim_file_open(const uint16_t* path, int32_t len, int32_t mode, int32_t* handle);
int32_t shim_file_read(int32_t handle, uint8_t* buf, int32_t cap, int32_t* got);
int32_t shim_file_write(int32_t handle, const uint8_t* buf, int32_t len);
int32_t shim_file_size(int32_t handle, int64_t* out);
int32_t shim_file_seek(int32_t handle, int64_t pos);
int32_t shim_file_delete(const uint16_t* path, int32_t len);

/* Rename, which is how a save is made atomic: write a temp file, close it, then
 * replace the real one in a single filesystem operation. Without this the only way
 * to update a file is to truncate and rewrite it, and a battery pull halfway
 * through leaves a session store that exists, parses as far as it goes, and is
 * wrong — the worst of the three possible outcomes.
 *
 * Overwrites the destination if it exists, which plain RFs::Rename refuses to do;
 * the shim does the delete first. */
int32_t shim_file_rename(const uint16_t* from, int32_t from_len,
                         const uint16_t* to, int32_t to_len);
void shim_file_close(int32_t handle);

/* Create a directory and any missing parents (RFs::MkDirAll). An already-existing
 * directory is success. The path should end in a backslash. Synchronous. */
int32_t shim_mkdir(const uint16_t* path, int32_t path_len);

/* List the file entries (not subdirectories) of a directory. `buf` is filled with the
 * entry names as NUL-separated UTF-16 units, and `count` receives how many fit. A
 * directory that does not exist is not an error — it lists as zero entries. Synchronous,
 * like the rest of the file API. */
int32_t shim_dir_list(const uint16_t* path, int32_t path_len, uint16_t* buf, int32_t cap, int32_t* count);

/* ------------------------------------------------------------------- alloc --
 * None of these can leave, so none of them TRAP. That is the point: the shim
 * calls User::Alloc and User::ReAlloc, never the AllocL/ReAllocL variants, so an
 * out-of-memory condition returns null instead of becoming a C++ throw that
 * would unwind through Rust frames. */
void* shim_alloc(uint32_t size);
void* shim_realloc(void* p, uint32_t size);
void shim_free(void* p);
/* Usable size of a cell, for a Rust allocator that wants to avoid a realloc. */
uint32_t shim_alloc_len(const void* p);

/* ------------------------------------------------------------------ panic --
 * Terminal. Rust's #[panic_handler] calls this; it does not return. */
void shim_panic(const uint8_t* file, uint32_t file_len, uint32_t line);

/* Write a line to the debug log (RDebug::Print). Cheap to leave in: it compiles
 * to nothing useful on a retail device but is the only way to see anything from
 * a process with no console. */
void shim_debug(const uint16_t* text, int32_t len);

/* ---------------------------------------------------------------- process --
 * Launch and query a process — for a GUI app starting its own headless daemon (see
 * USE_SHIM_DAEMON). Compiled in only when the app sets USE_PROC. No capability is required
 * to create a process from an executable already installed in \sys\bin. */
/* Create a process from a full UTF-16 path (e.g. "!:\\sys\\bin\\myappd.exe"), resume it,
 * and wait for its RProcess::Rendezvous. SHIM_OK once the child has signalled it is up;
 * the child's own capabilities, not the caller's, govern what it may then do. */
int32_t shim_process_start(const uint16_t* path, int32_t path_len);
/* Whether a process built from UID3 is running now: 1 yes, 0 no, negative on error. */
int32_t shim_process_running(uint32_t uid3);

/* ------------------------------------------------------------------ memory --
 * How much room is left, for an app that wants to know its own. Compiled in only when the
 * app sets USE_MEM.
 *
 * Cheap, and no capability: HAL for the device-wide RAM figures and User::AllocSize for
 * this process's own heap. Values are in KiB (a device has ~128 MiB,
 * which is 131072 KiB — comfortably inside an i32), or a negative Symbian error. */
int32_t shim_mem_free_kb(void);
int32_t shim_mem_total_kb(void);
int32_t shim_heap_used_kb(void);

/* ------------------------------------------------- Publish & Subscribe (P&S) --
 * A one-integer control channel between the controller and the daemon. Compiled in only
 * when the app sets USE_PROP. The category is the app's own SecureId, so defining and
 * writing need no capability; a subscriber posts SHIM_EV_PROP on every change. */
int32_t shim_prop_define(uint32_t category, uint32_t key);
int32_t shim_prop_set(uint32_t category, uint32_t key, int32_t value);
int32_t shim_prop_get(uint32_t category, uint32_t key, int32_t* out);
int32_t shim_prop_subscribe(uint32_t category, uint32_t key);
void    shim_prop_unsubscribe(uint32_t category, uint32_t key);

/* ------------------------------------------------------------------- audio -- */
/* Plays one sound file at a time through the platform's media framework.
 *
 * Format: what MMF ships as standard is AU, WAV and raw PCM, plus whatever the handset
 * adds (AMR, AAC, MP3). Notably NOT Opus — `mmf/common/mmffourcc.h` has no code for it,
 * so a Telegram voice message must be decoded to PCM in Rust and handed here wrapped in
 * a RIFF/WAVE container. The container is not optional: raw PCM is the one standard
 * format the plugin resolver cannot identify from its header.
 *
 * One clip at a time, deliberately: the sound device is a single exclusive resource
 * that the platform arbitrates between processes, and a second player in one process
 * fails with SHIM_ERR_IN_USE. Opening a new clip replaces whatever was open.
 *
 * Opening is asynchronous — a SHIM_EV_AUDIO_OPENED arrives with the duration, and only
 * then is shim_audio_play meaningful. Requires no capability. */
int32_t shim_audio_open_file(const uint16_t* path, int32_t len);
int32_t shim_audio_play(void);
/* Keeps the position, so a following play resumes rather than restarts. */
int32_t shim_audio_pause(void);
/* Ends playback and pushes SHIM_EV_AUDIO_DONE with KErrCancel. The push is this side's
 * doing: the platform does not call back after a stop, so a caller waiting for the
 * event it gets on a natural end would wait forever. */
int32_t shim_audio_stop(void);
/* Polled rather than pushed — a position event per frame would cost more than reading
 * it when a progress bar is actually being drawn. Zero when nothing is open. */
int32_t shim_audio_position_ms(void);
int32_t shim_audio_duration_ms(void);
/* Percent of the device maximum, clamped. The platform's own scale is device-specific
 * and not a percentage, so the conversion happens on the shim side. */
int32_t shim_audio_set_volume(int32_t percent);
int32_t shim_audio_close(void);

/* --------------------------------------------------------------------- sql --
 * Symbian SQL, which is SQLite behind a client-server API (sqldb.dll).
 *
 * WHY THIS IS WORTH A SHIM AT ALL. The platform already ships the engine, indexes,
 * transactions and all. Everything this SDK has persisted so far went through
 * fs::write_atomic — a whole-file rewrite per change, which is the right answer for a
 * settings blob and the wrong one for a message store that grows.
 *
 * SQL TEXT CROSSES AS UTF-8, VALUES AS UTF-16. That asymmetry is not sloppiness: the
 * API has 8-bit overloads of Exec and Prepare (the config string documents
 * `encoding=UTF8|UTF16`), so statement text goes over as the bytes Rust already holds,
 * with no conversion. Bind and column *values* have only 16-bit overloads, so those
 * convert at the boundary like every other string in this ABI.
 *
 * NO LEAVES TO TRAP. Every call below is the non-leaving overload — `Open`, not
 * `OpenL`; `Prepare`, not `PrepareL` — each of which is implemented on the platform
 * side as a TRAP around the leaving one and returns a TInt. So this file is the
 * exception rule 1 allows for, and states: there is nothing here for a TRAP to catch.
 *
 * INDEXES ARE ZERO-BASED, unlike sqlite3's own C API where bind parameters start at 1.
 * Symbian SQL numbers both parameters and columns from 0. examples/sqlprobe verifies
 * that on the handset rather than taking the documentation's word for it.
 *
 * AN OUT-OF-RANGE INDEX KILLS THE PROCESS. Not an error return — the SQL client asserts,
 * and an assertion on this platform is a panic. A panic is not a Leave, so no TRAP
 * anywhere below this line can catch it and nothing in this ABI can report it: the
 * application simply closes. examples/sqlprobe established that by binding index 2 of a
 * two-parameter statement deliberately, and taking the app down with it.
 *
 * The consequence for everything here: the bind and column functions cannot validate
 * their index, because the platform offers no way to ask a prepared statement how many
 * parameters or columns it has. The guard therefore lives in Rust, in
 * symbian::sql::Stmt::bind, built from a `?` count taken off the statement text. Callers
 * reaching this ABI directly own that check themselves.
 *
 * Requires no capability for a database under the app's private path. Elsewhere it
 * needs whatever the location needs, exactly as with files. */

/* TSqlColumnType, flattened: the platform distinguishes ESqlInt from ESqlInt64 and
 * Rust has no use for the difference — both arrive as int64. */
#define SHIM_SQL_NULL   0
#define SHIM_SQL_INT    1
#define SHIM_SQL_REAL   2
#define SHIM_SQL_TEXT   3
#define SHIM_SQL_BLOB   4

/* shim_sql_step: a row is ready, or the statement is finished. Translated from
 * KSqlAtRow/KSqlAtEnd so Rust does not carry the platform's constants. */
#define SHIM_SQL_DONE   0
#define SHIM_SQL_ROW    1

/* Opens `path`, creating it when `create` is non-zero and the file is not there. */
int32_t shim_sql_open(const uint16_t* path, int32_t path_len, int32_t create, int32_t* handle);
/* Finalises every statement still open on this database, then closes it. Doing that here
 * rather than trusting the caller is deliberate: a statement outliving its database is a
 * handle into freed server-side state, and the panic it produces names the SQL server. */
void    shim_sql_close(int32_t db);
int32_t shim_sql_delete(const uint16_t* path, int32_t path_len);
/* One or more statements, separated by semicolons, with no parameters and no rows —
 * schema, BEGIN/COMMIT, INSERT of literals. `changed` receives the number of rows the
 * statement affected, which for DDL is zero. */
int32_t shim_sql_exec(int32_t db, const uint8_t* sql, int32_t len, int32_t* changed);
/* The database file's size in bytes. */
int32_t shim_sql_size(int32_t db, int32_t* out);
/* The engine's own message for the last failure, which is the only place the *reason* a
 * statement was rejected appears — an error code says KSqlErrGeneral and the message says
 * `no such column: nmae`. Worth the round trip when a query fails. */
int32_t shim_sql_last_error(int32_t db, uint16_t* buf, int32_t cap, int32_t* len);

int32_t shim_sql_prepare(int32_t db, const uint8_t* sql, int32_t len, int32_t* stmt);
void    shim_sql_finalize(int32_t stmt);
/* Rewinds so the statement can be stepped again. Bindings survive a reset, which is what
 * makes a prepared statement worth reusing across rows. */
int32_t shim_sql_reset(int32_t stmt);
/* SHIM_SQL_ROW, SHIM_SQL_DONE, or an error.
 *
 * SELECT ONLY. Stepping a statement that produces no row set — an INSERT, an UPDATE, a
 * CREATE — panics inside the SQL client and closes the process, for the same reason and
 * with the same lack of warning as an out-of-range index. Non-SELECT statements go through
 * shim_sql_exec_stmt. examples/sqlprobe established this by stepping a prepared INSERT. */
int32_t shim_sql_step(int32_t stmt);
/* Run a prepared non-SELECT statement to completion; `changed` receives the number of rows
 * it affected. The counterpart of shim_sql_step, and the only safe way to run a bound
 * INSERT, UPDATE or DELETE. */
int32_t shim_sql_exec_stmt(int32_t stmt, int32_t* changed);

int32_t shim_sql_bind_null(int32_t stmt, int32_t index);
int32_t shim_sql_bind_int(int32_t stmt, int32_t index, int64_t value);
int32_t shim_sql_bind_real(int32_t stmt, int32_t index, double value);
int32_t shim_sql_bind_text(int32_t stmt, int32_t index, const uint16_t* text, int32_t len);
int32_t shim_sql_bind_blob(int32_t stmt, int32_t index, const uint8_t* data, int32_t len);

int32_t shim_sql_column_type(int32_t stmt, int32_t col, int32_t* out);
int32_t shim_sql_column_int(int32_t stmt, int32_t col, int64_t* out);
int32_t shim_sql_column_real(int32_t stmt, int32_t col, double* out);
/* Both text and blob report the column's full length in `*len` whether or not it fitted,
 * and return SHIM_ERR_OVERFLOW when it did not. That is what lets a caller size a buffer
 * in one extra call instead of guessing. */
int32_t shim_sql_column_text(int32_t stmt, int32_t col, uint16_t* buf, int32_t cap, int32_t* len);
int32_t shim_sql_column_blob(int32_t stmt, int32_t col, uint8_t* buf, int32_t cap, int32_t* len);
/* Column position by name, for a SELECT whose shape is not fixed at the call site. */
int32_t shim_sql_column_index(int32_t stmt, const uint16_t* name, int32_t len, int32_t* out);

/* ------------------------------------------------------------------- HAL --
 * SHIM_USE_HAL. One generic accessor rather than a function per attribute.
 *
 * HAL::Get is already `(TInt attribute, TInt& value)` — a flat integer interface over
 * the kernel's own figures — so wrapping each attribute separately would add a hundred
 * lines of C++ that only rename things, and would put the attribute table on the wrong
 * side of the wall. The table lives in Rust (`symbian::hal`), where it is data and is
 * covered by a host test.
 *
 * `attr` is a HALData::TAttribute. Unsupported attributes return KErrNotSupported
 * rather than failing the call, which is itself the answer: a handset that does not
 * implement an attribute is telling you the hardware is absent. */
int32_t shim_hal_get(int32_t attr, int32_t* out);

/* ------------------------------------------------------------ drives -------
 * SHIM_USE_FS_INFO. What is mounted, how big, how full.
 *
 * Three calls because Symbian has three questions: which drive letters exist at all,
 * what kind of medium each is, and how much room is on it. They are separate on the
 * platform too (RFs::DriveList, RFs::Drive, RFs::Volume), and merging them would hide
 * the case that matters most — a drive that is present but has no volume mounted, which
 * is exactly what an empty card slot looks like. */

/* Bit N set means drive letter 'A'+N exists. RFs::DriveList. */
int32_t shim_drive_list(uint32_t* out_mask);

typedef struct ShimDriveInfo {
    int32_t type;        /* TDriveInfo::iType — EMediaHardDisk, EMediaFlash, ENotPresent... */
    int32_t battery;     /* TDriveInfo::iBattery */
    uint32_t drive_att;  /* KDriveAttLocal, KDriveAttRemovable, KDriveAttInternal... */
    uint32_t media_att;  /* KMediaAttWriteProtected, KMediaAttLocked... */
} ShimDriveInfo;

/* `drive` is 0 for A:, 1 for B:, and so on — TDriveNumber's own numbering. */
int32_t shim_drive_info(int32_t drive, ShimDriveInfo* out);

typedef struct ShimVolumeInfo {
    int64_t size;        /* bytes */
    int64_t free;        /* bytes */
    uint32_t unique_id;  /* TVolumeInfo::iUniqueID — changes when the medium is swapped */
    int32_t name_len;    /* units written to `name`, 0 if it has none */
    uint16_t name[32];
} ShimVolumeInfo;

/* KErrNotReady for a drive that exists with nothing mounted — an empty card slot. That
 * is a finding, not a failure, and the caller is expected to record it as one. */
int32_t shim_volume_info(int32_t drive, ShimVolumeInfo* out);

/* --------------------------------------------------------- capabilities ----
 * SHIM_USE_CAPS. What this process was granted, as the kernel sees it.
 *
 * `cap` is a TCapability. Returns 1 if held, 0 if not, negative on a bad argument.
 *
 * This answers "what did the loader grant this image", which on a ROM-patched handset
 * is the interesting half of the question but not the whole of it. The other half —
 * whether the granted capability actually opens the door — is only answerable by
 * attempting the privileged operation and recording the error, which the caller does
 * with the ordinary file and process calls. A divergence between the two is the finding:
 * the kernel saying yes while the operation says KErrPermissionDenied means the patch
 * granted the bit and something else is refusing. */
int32_t shim_has_capability(int32_t cap);

/* RFs::Att — a file's attribute word (KEntryAttReadOnly, KEntryAttHidden, ...). Its
 * value here is less the attributes than the error: attempting it on a path outside the
 * data cage is a capability probe that costs nothing and destroys nothing. */
int32_t shim_fs_att(const uint16_t* path, int32_t len, uint32_t* out);

/* ------------------------------------------------------------- messaging ---
 * SHIM_USE_MSG. Read-only reconnaissance over the Message Server.
 *
 * Imports msgs.dso, and is therefore the textbook case for living in its own binary:
 * if the handset's msgs.dll does not export what we call, the E32 loader refuses the
 * image and there is no report, no panic and no log. See docs/device-notes.md, "An
 * import that does not resolve makes the app vanish".
 *
 * Nothing here writes. Opening a session, enumerating the registered MTMs and counting
 * folder entries is the whole surface — enough to know what the platform's messaging
 * stack contains before deciding whether to build on it. */
int32_t shim_msv_open(int32_t* out_handle);
int32_t shim_msv_mtm_count(int32_t handle, int32_t* out);
/* Throw away the client-side MTM registry and build a fresh one.
 *
 * CClientMtmRegistry is a *transient copy* held per client process, initialised when it is
 * constructed and refreshed thereafter only through session events. So a count taken after
 * installing a new MTM reads a snapshot from before the install, and "registered but my copy
 * has not noticed" is indistinguishable from "not registered" — which is exactly the
 * ambiguity that made a probe report a failure it could not actually see. */
int32_t shim_msv_refresh_registry(int32_t handle);

/* Instantiate a Client MTM through the registry — the definitive test that a registration
 * worked.
 *
 * Counting the registry is weaker than it looks and was measured to be misleading: the count
 * comes from a per-process copy the session refreshes on an event that cannot be delivered
 * while the caller is still inside its own RunL. This asks the framework to do the whole
 * thing instead — find the type, load the DLL, call the factory at the registered ordinal —
 * and every one of those failing has its own error code.
 *
 * The object is destroyed immediately; the question is whether it can be made at all. */
int32_t shim_msv_can_instantiate(int32_t handle, uint32_t mtm_uid);

typedef struct ShimMtmInfo {
    uint32_t type_uid;       /* the MTM type UID: KUidMsgTypeSMS and friends */
    uint32_t technology_uid;
    int32_t name_len;
    uint16_t name[64];
} ShimMtmInfo;

int32_t shim_msv_mtm_info(int32_t handle, int32_t index, ShimMtmInfo* out);

/* Standard folder ids, so Rust need not carry msvids.h. */
#define SHIM_MSV_ROOT     0x1000
#define SHIM_MSV_INBOX    0x1002
#define SHIM_MSV_OUTBOX   0x1003
#define SHIM_MSV_DRAFTS   0x1004
#define SHIM_MSV_SENT     0x1005

int32_t shim_msv_folder_count(int32_t handle, int32_t folder_id, int32_t* out);
void shim_msv_close(int32_t handle);

/* ------------------------------------------------- messaging, the write side --
 * Still SHIM_USE_MSG, and still the same isolation rule: this is the binary that can
 * vanish, so it should be alone.
 *
 * REGISTERING AN MTM
 *
 * Dropping a compiled .mtm into C:\resource\messaging\mtm\ is NOT enough for an MTM outside
 * ROM — the Message Server has to be told. `InstallMtmGroup` is that call, and it fires
 * EMsvMtmGroupInstalled so a *running* Messaging application picks the new type up live.
 * De-install first: installing over an existing group fails, so a reinstall needs the pair.
 * KErrNotFound from the de-install is the ordinary first-run answer, not an error. */
int32_t shim_msv_install_mtm(const uint16_t* path, int32_t len);
int32_t shim_msv_deinstall_mtm(const uint16_t* path, int32_t len);

/* Create a service entry — the "account" the native Messaging application lists.
 *
 * A child of the root, with iType = KUidMsvServiceEntry and iMtm = your MTM's type UID.
 * `name` becomes iDetails, which is the string the user sees. The new id comes back in
 * `out_id` and is what every message of yours must carry as its iServiceId. */
int32_t shim_msv_create_service(int32_t handle, uint32_t mtm_uid,
                                const uint16_t* name, int32_t name_len, int32_t* out_id);

/* Flags for ShimNewMessage::flags. `NEW` and `UNREAD` together are what makes the native
 * app bold the entry and what the notification list counts. */
#define SHIM_MSV_NEW     0x01
#define SHIM_MSV_UNREAD  0x02
#define SHIM_MSV_COMPLETE 0x04
#define SHIM_MSV_VISIBLE 0x08

/* Everything a message needs to land in a folder. Pointers are borrowed for the duration of
 * the call only — the shim copies into descriptors and into the store before returning. */
typedef struct ShimNewMessage {
    int32_t service_id;      /* from shim_msv_create_service */
    uint32_t mtm_uid;
    int32_t parent_id;       /* SHIM_MSV_INBOX and friends */
    int64_t unix_time;       /* 0 means now */
    int32_t size;            /* iSize; 0 means "the body's length" */
    int32_t flags;           /* SHIM_MSV_* above */
    const uint16_t* details;      /* iDetails — who it is from */
    int32_t details_len;
    const uint16_t* description;  /* iDescription — subject, or a preview line */
    int32_t description_len;
    const uint16_t* body;         /* stored as rich text in the entry's CMsvStore */
    int32_t body_len;
} ShimNewMessage;

/* Create the entry, write the body, commit. `out_id` receives the new entry's TMsvId.
 *
 * The order matters and is not obvious: the entry is created first with
 * KMsvEntryInPreparationFlag implied by not being complete, then the body is written to its
 * store and committed, and only then are the visible flags set. A reader that saw the entry
 * between those steps would see a message with no body. */
int32_t shim_msv_create_message(int32_t handle, const ShimNewMessage* msg, int32_t* out_id);

/* Delete an entry. For a probe that has to be able to clean up after itself. */
int32_t shim_msv_delete_entry(int32_t handle, int32_t id);

/* Delete every service entry of a given MTM type, and everything under them.
 *
 * A probe that creates a service on each run and never removes the last one fills the user's
 * Messaging account list with copies of itself. This is the cleanup, and it is written as
 * "remove all of this type" rather than "remove the one I just made" precisely because the
 * runs that already happened left theirs behind and nothing remembers their ids.
 *
 * Returns the number of services removed, or a negative error. */
int32_t shim_msv_delete_services(int32_t handle, uint32_t mtm_uid);

/* ------------------------------------------------ the new-message notification --
 * SHIM_USE_NCN. Imports ecom.dso.
 *
 * The platform's own indicator, tone and floating note — the exact triple an arriving SMS
 * produces — reached through MNcnNotification, an ECom interface the platform publishes for
 * messaging plugins to call (KNcnNotificationInterfaceUid 0x101f8855). It is the only
 * supported route: CAknSoftNotifier and CAknSmallIndicator are exported from aknnotify.dso
 * but have no public header in this SDK, and the status-pane plugin interface is not
 * published at all.
 *
 * Two things are unverified on this handset and are what a probe is for. The interface's own
 * documentation frames it as an *email* plugin API — the parameter is called aMailBox and
 * the note it raises says "New email" — so whether it accepts a service whose technology
 * type is neither mail nor SMS is not answerable from the headers. And ncnnotification.dll
 * is an ECom plugin rather than a library, so it is absent from the SDK's import set and its
 * presence on the device was never swept.
 *
 * Hence: resolution failure returns the error rather than panicking, and a caller is
 * expected to record it. */
#define SHIM_NCN_ICON          0x01
#define SHIM_NCN_TONE          0x02
#define SHIM_NCN_SOFT_NOTE     0x04
/* Icon + tone + note. What SMS does. */
#define SHIM_NCN_NORMAL        0x07

int32_t shim_ncn_notify(int32_t service_id, int32_t indication);
/* Zero the new-message counter for a service — for when the user has read them. */
int32_t shim_ncn_mark_unread(int32_t service_id);

/* --------------------------------------------------------- loading our own DLL --
 * SHIM_USE_DLL_PROBE. The half of the DLL question the host cannot answer.
 *
 * tools/e32dump.py --expect-dll already checks, on the host, that the image is a DLL, that
 * its UID1 is right, that it exports something and that it has no writable static data.
 * What it cannot check is whether the *handset's* loader accepts it and whether
 * RLibrary::Lookup returns something callable. That is this call.
 *
 * Every step is recorded separately, because they fail for different reasons and a single
 * pass/fail would collapse four diagnoses into one. `lookup_ok` false with `load_err` zero,
 * for instance, means the image loaded and exports nothing — which is what a DLL built
 * without EXPORT_C does (see docs/device-notes.md).
 *
 * The signature is apps/dlltest's, not a general one: this exists to validate that DLL. */
typedef struct ShimDllProbe {
    int32_t load_err;    /* RLibrary::Load */
    uint32_t uid1;       /* RLibrary::Type — should be KDynamicLibraryUid, 0x10000079 */
    uint32_t uid2;
    uint32_t uid3;
    int32_t lookup_ok;   /* 1 if Lookup(1) returned non-NULL */
    int32_t call_err;    /* what the exported function returned */
    uint32_t magic;      /* the three fields it wrote back */
    uint32_t echo;
    uint32_t ticks;
} ShimDllProbe;

int32_t shim_dll_call_ordinal1(const uint16_t* name, int32_t len, uint32_t arg,
                               ShimDllProbe* out);

/* Load a DLL and look up an ordinal WITHOUT calling it.
 *
 * The half of the question that is safe to ask. Calling an unknown export means jumping to
 * an address with a signature you are guessing at; looking one up means asking the loader
 * whether the export table has that slot, which cannot fault.
 *
 * It exists because a DLL that the framework loads and calls killed the process before its
 * own first instruction ran, and that leaves two possibilities — the image does not load, or
 * the call is wrong — which nothing observable from outside separates. This separates them.
 *
 * Returns 1 if the ordinal is present, 0 if the library loaded and the ordinal is not there,
 * and the Symbian error if it would not load at all. */
int32_t shim_dll_has_ordinal(const uint16_t* name, int32_t len, int32_t ordinal);

/* --------------------------------------------------- process, with a deadline --
 * SHIM_USE_PROC. As shim_process_start, but abandons the wait after `timeout_ms`.
 *
 * shim_process_start waits on the child's Rendezvous with User::WaitForRequest and no
 * escape. A child that neither rendezvouses nor dies therefore hangs the caller for
 * good — and the caller here is the launcher whose entire job is to survive its probes.
 * "Every asynchronous request needs a way to abandon it, and the one that never
 * completes is exactly the one that needs it" (docs/device-notes.md).
 *
 * Returns SHIM_ERR_TIMED_OUT and kills the child if the deadline passes first. */
int32_t shim_process_start_timeout(const uint16_t* path, int32_t path_len, int32_t timeout_ms);

#ifdef __cplusplus
}
#endif

#endif /* SYMBIAN_SHIM_H */
