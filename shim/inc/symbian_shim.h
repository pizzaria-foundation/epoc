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
#define SHIM_ERR_DIED            -13
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
    /* A raw hardware key scan code, reported only in resident mode; `a` is the scan code. For a
     * launcher to see and diagnose the Menu and End keys the character path never delivers. */
    SHIM_EV_RAWKEY = 7,
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
    /* An RFCOMM listener accepted a client. `handle` is the new accepted-socket handle
     * (>= 0) when `status` is SHIM_OK; on failure `status` is the error. Distinct from the
     * TCP events (20..24) so a daemon running both can branch without ambiguity. */
    SHIM_EV_BT_ACCEPTED = 26,
    /* Bytes arrived on an RFCOMM socket. `handle` the socket, `a` the count. */
    SHIM_EV_BT_RECV = 27,
    /* An RFCOMM send completed. `handle` the socket, `a` the count written on SHIM_OK. */
    SHIM_EV_BT_SENT = 28,
    /* An RFCOMM socket closed or its link dropped. `handle` the socket, `status` the reason. */
    SHIM_EV_BT_CLOSED = 29,
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
    /* A position update completed. `status` is SHIM_OK for a fix and the platform's own code
     * otherwise — SHIM_ERR_TIMED_OUT for a module that could not see the sky in time, and
     * SHIM_ERR_ACCESS_DENIED for a client that never called SetRequestor, which is a precondition
     * and not a capability. `a` is the number of satellites used (-1 when satellite info was not
     * requested or no fix arrived), `b` the horizontal accuracy rounded to whole metres (-1 when
     * unknown), `c` 1 when the request carried satellite info.
     *
     * The event is the notification; the fix itself is read with shim_gps_read, because latitude
     * is a double and an event carries integers. With a non-zero update interval this repeats for
     * the life of the subscription, errors included — a timeout in a tunnel is not a reason to
     * stop asking. From shim_lbs.cpp. */
    SHIM_EV_GPS_FIX = 43,
    /* The serving cell tower was read. `status` is SHIM_OK or the platform's error; `a` is 1 when
     * the modem said the location area is known, which is what decides whether the area code and
     * cell id in shim_cell_get mean anything.
     *
     * The identifiers themselves are collected with shim_cell_get rather than carried here,
     * because an event is four integers and a caller wants five with a parse behind two of them.
     * From shim_cell.cpp. */
    SHIM_EV_CELL = 44,
    /* A subscribed Publish & Subscribe property changed. `a` is the key, `c` the freshly
     * read integer value. From shim_prop; the daemon uses it as its stop signal. */
    SHIM_EV_PROP = 53,
    /* Something changed in the message store. `a` is one of SHIM_MSV_EV_*, `b` the TMsvId
     * the event is about, `c` its parent folder, `d` how many entries the platform's
     * original selection carried — this event being one of them.
     *
     * A HINT, NEVER DATA. By the time Rust reads this, the id may already be gone and the
     * flags may already have changed again. The contract is that a reader re-reads the
     * entry from the store and re-derives what to do, which is what makes a dropped ring
     * slot, a restarted daemon and a session event that arrived while nobody was listening
     * all the same recoverable case. See shim_msv_observe. */
    SHIM_EV_MSV = 60,
    /* The response headers arrived for the HTTP transaction in flight. `a` is the HTTP status
     * code. Redirects are already followed by then — the platform stack does that for GET without
     * telling anyone — so this is the status of the page that will actually load. From
     * shim_http.cpp. */
    SHIM_EV_HTTP_HEAD = 70,
    /* Body bytes arrived. `a` is the running total the stack has handed over, which is not the
     * same as what is buffered: shim_httpc_read drains a capped buffer, and the difference is
     * reported through the truncated flag rather than by the two numbers quietly disagreeing. */
    SHIM_EV_HTTP_BODY = 71,
    /* The transaction ended. `status` is SHIM_OK or the platform error — and a negative one is
     * worth keeping, since an untrusted certificate is only distinguishable from a dead server by
     * its code. `a` is the HTTP status, `b` the total body bytes, `c` the response flags, `d` the
     * number of body callbacks it took. */
    SHIM_EV_HTTP_DONE = 72,
    /* The app should exit; nothing may be queued after it. */
    SHIM_EV_QUIT = 90,

    /* Another application asked this one to open a document — a URL, in practice.
     *
     * `a` is its length in UTF-16 units; the text itself is collected with
     * `shim_app_open_request`, because an event carries integers and a URL is not one. Arrives on
     * a cold start (AppArc gave us a document name) and on a warm one (our task was sent a
     * message), and the application does not need to know which. */
    SHIM_EV_OPEN_URL = 91
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

/* Turn resident (launcher) behaviour on/off: capture the Menu key to bring this app forward, and
 * make the End key send to background instead of closing. SHIM_OK, or SHIM_ERR_NOT_READY if
 * called before the window group exists. Needs SwEvent, granted at load on a ROM-patched handset. */
int32_t shim_set_resident(int32_t on);
/* Drop this app behind the others without closing it — for a helper the user never asked to see.
 * SHIM_OK, or SHIM_ERR_NOT_READY before the UI environment exists. */
int32_t shim_app_to_background(void);
/* Bring this app back to the front, focus included — the move a resident launcher makes when
 * something else has taken the screen from it (an app restarted by the platform after being killed,
 * for one). SHIM_OK, or SHIM_ERR_NOT_READY before the UI environment exists. */
int32_t shim_app_to_foreground(void);

/* ------------------------------------------------------------------- cpu time --
 * The only load measurement Symbian offers: cumulative microseconds a thread has spent on the
 * processor (RThread::GetCpuTime). Difference it over an interval for utilisation. Compiled in
 * only when the app sets USE_CPUTIME, because on some 9.x kernels the accounting is a build
 * option and the call answers KErrNotSupported — which is a measurement, not a failure. */
/* Sum the CPU microseconds of every thread whose full name matches `pattern` (UTF-16, e.g.
 * "foo*::*" for one process, "*::*" for all). SHIM_OK with *total_us and *threads set,
 * SHIM_ERR_NOT_SUPPORTED where the kernel does not account for it. */
int32_t shim_cpu_time(const uint16_t* pattern, int32_t pattern_len,
                      int64_t* total_us, int32_t* threads);
/* The full name of the nth running process ("name[uid]0001"). SHIM_ERR_NOT_FOUND past the end. */
int32_t shim_process_at(int32_t index, uint16_t* out, int32_t cap, int32_t* len);

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

/* --------------------------------------------------------------- position --
 * The Location Acquisition API: RPositionServer onto the framework, RPositioner onto whichever
 * module it picks. Behind USE_LBS, because lbs.dso is an import an app that does not want a
 * position has no reason to carry.
 *
 * Needs the Location capability, which is protected. This handset grants it (measured — see
 * apps/devdump's caps probe), and a stock phone would not install an unsigned package that
 * declares it.
 *
 * Nothing here blocks. A fix takes seconds to minutes, so every route to one is an event. */

/* Subscribe to position updates. Completions arrive as SHIM_EV_GPS_FIX.
 *
 * `interval_ms` 0 is a single fix and the subscription then goes quiet; any positive value is a
 * stream at that cadence, paced by the framework rather than by us. `timeout_ms` 0 lets the module
 * take as long as it takes — which for a cold GPS start is minutes, so a foreground app wants a
 * bound and a logger does not.
 *
 * `want_satellites` asks for TPositionSatelliteInfo instead of TPositionInfo, which adds the
 * satellite counts. Whether a given module accepts it is a property of that module; this is a
 * parameter rather than a guess so a probe can measure the answer.
 *
 * `module_uid` 0 lets the framework choose. A named module is chosen instead, and the reason that
 * is worth a parameter is that the modules are not interchangeable — this handset reports the
 * integrated GPS at 80 s and 10 m, and the network module at 12 s and 200 m. "Roughly where, very
 * soon" and "exactly where, eventually" are different questions and only one of them can be asked
 * at a time. Use shim_gps_module_info to find a UID.
 *
 * SHIM_ERR_ALREADY_EXISTS when a subscription is already running: there is one device position and
 * a second subscription would pay the GPS's power cost twice. Switching modules therefore means
 * shim_gps_stop followed by another start. */
int32_t shim_gps_start(int32_t interval_ms, int32_t timeout_ms, int32_t want_satellites,
                       int32_t module_uid);

/* Cancel the subscription and close the session. Safe when nothing is running. */
void shim_gps_stop(void);

/* The last completed update. Any pointer may be NULL.
 *
 * SHIM_ERR_NOT_READY before the first completion — which is the honest answer, and the reason this
 * is not a struct full of zeroes a caller would draw at latitude 0, longitude 0. When the last
 * update was an error, that error is returned rather than a stale fix.
 *
 * `alt`, `h_acc` and `v_acc` come back NaN when the module does not report them; check for it. */
int32_t shim_gps_read(double* lat, double* lon, double* alt,
                      double* h_acc, double* v_acc, int32_t* sats, int32_t* in_view);

/* How many positioning modules the framework knows about. Answerable without starting anything,
 * which is what lets a caller decide not to. */
int32_t shim_gps_module_count(int32_t* out);

/* One module's entry, by index in 0..count. `name` receives the module name in UTF-16 and may be
 * NULL; SHIM_ERR_OVERFLOW means the name was cut, and the values below are still filled.
 *
 * `out` receives, needing cap >= 10:
 *   0  module UID          1  1 when available now
 *   2  technology type (1 terminal, 2 terminal-assisted, 4 network)
 *   3  device location (1 internal, 2 external)
 *   4  cost indicator      5  power consumption
 *   6  horizontal accuracy in mm, or -1     7  vertical accuracy in mm, or -1
 *   8  time to first fix in ms              9  time to next fix in ms
 *
 * Entry 8 is the number that decides a map's UI: a module whose cold start is minutes cannot be
 * something the first frame waits on. */
int32_t shim_gps_module_info(int32_t index, uint16_t* name, int32_t name_cap,
                             int32_t* name_len, int32_t* out, int32_t out_cap);

/* ------------------------------------------------------------------- cell --
 * The serving tower's identifiers, through CTelephony. Behind USE_CELL, which adds etel3rdparty —
 * an import gated on its own because an app that wants a GPS fix has no use for it.
 *
 * Why this exists at all: on this handset there is no other route to a position indoors. The
 * platform's own network positioning module answers KErrGeneral, and both satellite modules time
 * out under a roof — measured, see docs/reference/gpsprobe.txt. A tower id and a public database
 * is what every other phone does instead.
 *
 * Asynchronous, like everything else here. See the header of shim_cell.cpp for what waiting on it
 * would cost. */

/* Ask for the serving cell. Completion arrives as SHIM_EV_CELL. Safe to call again; a read already
 * in flight is left alone rather than restarted. */
int32_t shim_cell_read(void);

/* The identifiers from the last completed read. Any pointer may be NULL.
 *
 * SHIM_ERR_NOT_READY before the first completion, and the platform's error when the last read
 * failed. SHIM_ERR_ARGUMENT when the modem gave a country or network code that is not decimal
 * digits — refused rather than partially parsed, because a query built from half an MNC returns a
 * confident answer about the wrong place.
 *
 * `area_known` is the modem's own flag. When it is 0 the area code and cell id are whatever was in
 * the struct and mean nothing. */
int32_t shim_cell_get(int32_t* mcc, int32_t* mnc, int32_t* lac, int32_t* cid,
                      int32_t* area_known);

/* Cancel any read in flight and release the telephony session. */
void shim_cell_stop(void);

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

/* Why the last worker thread ended, for a job that never answered.
 *
 * A Symbian thread that dies leaves an exit type, an exit reason and an exit category behind, and
 * that triple is a diagnosis: `KERN-EXEC 3` is a bad pointer, `E32USER-CBase 69` is a PushL with no
 * cleanup stack, `KERN-EXEC 0` is a panic raised by the code itself. Different bugs, different
 * fixes.
 *
 * This exists because that triple was on the floor for six device round trips. A job that never
 * answers was the only symptom available, and "never answered" was read as five different causes in
 * a row, each wrong, each costing a Bluetooth push of nearly a megabyte. The kernel knew the answer
 * the whole time; nothing was asking it.
 *
 * `type` is a TExitType: 0 kill, 1 terminate, 2 panic, 3 pending (still running). `reason` is the
 * panic number or the exit code. `cat` receives the category as NUL-terminated ASCII, which is the
 * half that names the subsystem.
 *
 * Valid only while the previous thread's handle is still open — that is, after a job that never
 * completed and before the next submit. Returns 0 on success, SHIM_ERR_NOT_READY if there is no
 * thread to ask about. */
int32_t shim_work_exit_info(int32_t* type, int32_t* reason, uint8_t* cat, int32_t cat_cap);

/* Push and pop one frame on the calling thread's cleanup stack.
 *
 * The narrowest possible test of one thing: whether platform C++ can allocate through the cleanup
 * stack on this thread. It exists because `iconv_open` on a worker panics `E32USER-CBase 66` — which
 * the SDK's own headers gloss as "a stack frame for the next PushL() cannot be allocated" — *after* a
 * CTrapCleanup was installed and confirmed created. Two very different bugs fit that: the cleanup
 * stack we install is not the one PushL finds, or charconv's allocation fails for a reason of its
 * own. A bare PushL separates them, and a bare PushL involves no charset conversion at all.
 *
 * Returns 0 if the push and pop completed, the leave code if it left. A panic returns nothing, and
 * `shim_work_exit_info` names it. */
int32_t shim_cleanup_probe(void);

/* Collect the document another application asked this one to open, if any.
 *
 * Writes UTF-16 into `out` and returns the number of units, 0 when there is nothing pending, or
 * SHIM_ERR_OVERFLOW when the buffer is too small — in which case the request is *kept*, so a caller
 * with a bigger buffer can still get it.
 *
 * Consumed by reading: the request has been delivered once it is in the caller's hands, and leaving
 * it behind is how a link from a previous run opens by itself on the next one. */
int32_t shim_app_open_request(uint16_t* out, int32_t cap);

/* The C libraries' heap: bytes committed, and bytes live. See shim_alloc.cpp for why this is not
 * the same question as `shim_heap_used_kb`, which reads the calling thread's allocator. */
void shim_cheap_stats(int32_t* size, int32_t* allocated);

/* Return the C heap's unused tail to the system; the bytes recovered. */
int32_t shim_cheap_compress(void);

/* The same push with no TRAP of its own — the shape of every call into platform C++ from a job.
 *
 * A cleanup stack is not a cleanup stack *frame*: frames come from TRAP, and `CleanupStack::PushL`
 * with none panics `E32USER-CBase 66`. This is the direct test of whether the caller established
 * one, which the version above hides by supplying its own. */
int32_t shim_cleanup_probe_bare(void);

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

/* Like shim_dir_list, but includes subdirectories, each written with a trailing '\' so a
 * caller can tell a directory from a file out of the NUL-separated buffer alone. For a shell
 * that has to navigate rather than only read a known directory's files. */
int32_t shim_dir_list_all(const uint16_t* path, int32_t path_len, uint16_t* buf, int32_t cap, int32_t* count);

/* One entry's metadata. Size is split because the ABI is 32-bit and a file can exceed it;
 * the date is fields rather than an epoch because Symbian's epoch is year 0 and no caller
 * wants to rediscover that. `month` and `day` are 1-based (TDateTime's are not). */
typedef struct ShimFileStat {
    uint32_t size_lo;
    uint32_t size_hi;
    int32_t year;
    int32_t month;      /* 1-12 */
    int32_t day;        /* 1-31 */
    int32_t hour;
    int32_t minute;
    int32_t second;
    int32_t attributes; /* KEntryAtt* bits, as the file server reports them */
    int32_t is_dir;
} ShimFileStat;

/* Size, modification time and attributes of one file or directory — RFs::Entry. A directory
 * path may carry its trailing '\' or not; both are accepted. */
int32_t shim_file_stat(const uint16_t* path, int32_t path_len, ShimFileStat* out);

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
/* SHIM_USE_PROC. Start a process without waiting for its rendezvous — the only one of the
 * three that is safe to call from a thread running an active scheduler (every GUI app).
 * The waiting variants use User::WaitForRequest, which on such a thread consumes another
 * request's completion and kills the process with a stray-signal panic. See the note above
 * the definition. */
int32_t shim_process_spawn(const uint16_t* path, int32_t path_len);
/* Whether a process built from UID3 is running now: 1 yes, 0 no, negative on error. */
int32_t shim_process_running(uint32_t uid3);
/* Kill every live process with this UID3 — the escape hatch for a resident launcher. SHIM_OK if
 * one was killed, SHIM_ERR_NOT_FOUND if none matched. Killing another process needs PowerMgmt,
 * granted at load by a ROM-patched handset. */
int32_t shim_process_kill(uint32_t uid3);

/* ----------------------------------------------------------------- apparc --
 * Enumerate and launch installed applications, for a launcher. Compiled in only when the app
 * sets USE_APPARC. Unlike process above, which launches a known executable by path, these go
 * through RApaLsSession — the same application registry the native menu reads — so the list is
 * what the phone itself would show and a launch honours the app's own registration. No
 * capability is required to list or start an application.
 *
 * The enumeration is a two-step so the caller can hold it: shim_apps_refresh() scans once and
 * caches, shim_apps_count() and shim_app_at() read the cache by index. */
/* Re-scan installed apps into the cache; returns the count (>= 0) or a negative error. */
int32_t shim_apps_refresh(void);
/* How many apps the last refresh found; 0 before the first refresh. */
int32_t shim_apps_count(void);
/* Copy entry `index` out: uid3 and hidden (1/0) always written; caption copied up to `cap` u16
 * with its length in *caption_len. SHIM_ERR_NOT_FOUND for a bad index. */
int32_t shim_app_at(int32_t index, uint32_t* uid3, uint8_t* hidden,
                    uint16_t* caption, int32_t cap, int32_t* caption_len);
/* Start the installed app with this UID3, the way the shell would. SHIM_OK on acceptance. */
int32_t shim_app_launch(uint32_t uid3);

/* Launch app `uid3` pointed at `doc` (a URL, UTF-16, `doc_len` units) by `route`.
 *
 * Only compiled when the app opts into USE_LAUNCH_DOC. There is no `OpenUrl` on S60 — a browser is
 * asked to open a URL by convention, and which convention a handset honours is a question the
 * handset answers, so `route` selects between four of them: 0 document name, 1 the browser's
 * `4 <url>` tail end, 2 StartDocument at an explicit app, 3 StartDocument letting the platform
 * resolve. See the comment on DoLaunchDocL. SHIM_OK means the platform accepted the launch, not
 * that the URL opened — nothing here can tell us that. */
int32_t shim_app_launch_doc(uint32_t uid3, const uint16_t* doc, int32_t doc_len, int32_t route);

/* Deliver `msg` (8-bit, `msg_len` bytes) to the running application `uid3`, bringing it forward.
 *
 * The way the shell hands a URL to a browser that is already open — which `StartDocument` cannot
 * do, because it starts applications rather than talking to them. SHIM_ERR_NOT_FOUND when the
 * application is not running, which is the caller's cue to start it instead. */
int32_t shim_app_task_message(uint32_t uid3, const uint8_t* msg, int32_t msg_len);

/* Put `text` (UTF-16, `len` units) on the system clipboard, in the plain-text format Avkon's Paste
 * reads. Only compiled when the app opts into USE_CLIPBOARD; every other build links a stub that
 * answers SHIM_ERR_NOT_SUPPORTED. */
int32_t shim_clip_set_text(const uint16_t* text, int32_t len);
/* Read the clipboard's plain text into `out` (at most `cap` UTF-16 units); `len` gets the count.
 * SHIM_ERR_NOT_FOUND when there is nothing to paste — an empty clipboard is a state, not a failure.
 * Same USE_CLIPBOARD gate as the write above. */
int32_t shim_clip_get_text(uint16_t* out, int32_t cap, int32_t* len);
/* Kill the installed app with this UID3 through the window server (TApaTask::KillTask) — the way
 * to stop an app that will not close itself, like a resident launcher. SHIM_OK if killed,
 * SHIM_ERR_NOT_FOUND if it has no running task. */
int32_t shim_app_kill(uint32_t uid3);
/* Ask the app to close (TApaTask::EndTask). No capability; an app that ignores it stays. This is
 * the one to use — shim_app_kill faults the caller without PowerMgmt, measured. */
int32_t shim_app_end(uint32_t uid3);

/* 1 when the keypad is locked (or the phone is in autolock), 0 when it is not, negative on error —
 * SHIM_ERR_NOT_READY without a control environment. USE_KEYLOCK. */
int32_t shim_keylock(void);
/* List the UID3s of running apps (window-server task list, front-to-back), deduped, up to `cap`.
 * Returns the count written, or a negative error / SHIM_ERR_NOT_READY. */
int32_t shim_apps_running(uint32_t* out, int32_t cap);
/* Signal strength via CTelephony: *bars 0..7 (-1 unknown), *dbm the raw value. SHIM_OK, or a
 * negative error / SHIM_ERR_NOT_SUPPORTED when not compiled in (USE_TELEPHONY). */
int32_t shim_tele_signal(int32_t* bars, int32_t* dbm);
/* Fetch app UID3's icon at `size` pixels (the same icon the native menu draws) into caller-owned
 * buffers: `rgb_out` gets RGB565 pixels, `mask_out` gets 8-bit coverage (0 transparent, 255
 * opaque), both row-major `w`*`h`. `cap` is each buffer's pixel capacity. `w`/`h` are written when
 * the bitmap size is known. SHIM_OK on success, SHIM_ERR_OVERFLOW if the buffers are too small,
 * or the platform error (e.g. KErrNotFound for an app with no icon). */
int32_t shim_app_icon(uint32_t uid3, int32_t size,
                      uint16_t* rgb_out, uint8_t* mask_out, int32_t cap,
                      int32_t* w, int32_t* h);
/* Diagnostic variant: the TInt GetAppIcon overload, colour filled green. Isolates whether that
 * overload panics on MIF-icon apps the way the TSize one does. Same ABI. */
int32_t shim_app_icon_b(uint32_t uid3, int32_t size,
                        uint16_t* rgb_out, uint8_t* mask_out, int32_t cap,
                        int32_t* w, int32_t* h);
/* Variant C (USE_AKNICON): the icon read from the app's registered icon FILE through Avkon's
 * AknIconUtils, rather than from a CApaMaskedBitmap. Handles MIF (scalable) icons as well as MBM,
 * and yields a real mask plane. `bitmap_id` is the colour plane's index within that file; the mask
 * is taken to be the next index. Same returns as shim_app_icon. */
int32_t shim_app_icon_c(uint32_t uid3, int32_t size, int32_t bitmap_id,
                        uint16_t* rgb_out, uint8_t* mask_out, int32_t cap,
                        int32_t* w, int32_t* h);
/* The full path of the file this app's icon comes from (USE_AKNICON), as UTF-16 units into `out`
 * with its length in *len. Diagnostic: it says whether a fetch read the right file at all, and its
 * extension (.mbm vs .mif) says which route can read it. */
int32_t shim_app_icon_file(uint32_t uid3, uint16_t* out, int32_t cap, int32_t* len);

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
/* As shim_prop_define, but with an open read policy so a process in a different SID can read it —
 * for a bundled daemon publishing a value the launcher (a different UID) reads. */
int32_t shim_prop_define_public(uint32_t category, uint32_t key);
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

/* Entry type UIDs, from msvstd.hrh, so Rust need not carry that header.
 *
 * shim_msg.cpp asserts each of these against the platform's own constant at compile time.
 * That guard is not ceremony: the first version of the Rust side guessed these values, and a
 * wrong type UID makes every `is_message()` answer false — so a service would silently never
 * recognise one of its own messages, with nothing failing anywhere. A build error is the only
 * acceptable way for that to show up. */
#define SHIM_MSV_TYPE_ROOT        0x10000F67
#define SHIM_MSV_TYPE_SERVICE     0x10000F68
#define SHIM_MSV_TYPE_FOLDER      0x10000F69
#define SHIM_MSV_TYPE_MESSAGE     0x10000F6A
#define SHIM_MSV_TYPE_ATTACHMENT  0x10000F6B

/* Standard folder ids, so Rust need not carry msvids.h. */
#define SHIM_MSV_ROOT     0x1000
#define SHIM_MSV_INBOX    0x1002
#define SHIM_MSV_OUTBOX   0x1003
#define SHIM_MSV_DRAFTS   0x1004
#define SHIM_MSV_SENT     0x1005

int32_t shim_msv_folder_count(int32_t handle, int32_t folder_id, int32_t* out);
/* How many children of the folder are unread — counted server-side from the loaded child index, so
 * it is one operation for the whole folder. For a home-screen "N new messages" indicator. */
int32_t shim_msv_folder_unread(int32_t handle, int32_t folder_id, int32_t* out);
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

/* --------------------------------------------------- messaging, the read side --
 * Still SHIM_USE_MSG, and it adds no import: MoveL, ChildrenWithMtmL, ReadStoreL and
 * ChangeL all live in msgs.dso, which is already on the link line. So this does not add a
 * new way for the image to fail to load.
 *
 * WHY IT EXISTS
 *
 * The write side above is enough to put a message in the user's inbox. It is not enough to
 * run a service, because the traffic goes both ways: a UI MTM loaded into Nokia's Messaging
 * application writes the user's reply into the store, and something outside that process has
 * to notice and carry it out. Until this block existed, a daemon could not see the reply at
 * all — not its id, not the correspondent, not the text.
 *
 * TRUNCATION IS A NUMBER, NOT AN ERROR
 *
 * Every call that fills a buffer writes what fits and reports the FULL length, the same
 * contract as shim_sql_column_text. So the grow-and-retry loop lives on the Rust side, where
 * a host test can drive it, rather than being a special error code down here that every
 * caller has to remember to handle. */

/* Entry flags for ShimMsvEntry::flags.
 *
 * The first four are SHIM_MSV_NEW..VISIBLE above, reused deliberately: one vocabulary for
 * reading an entry's state and for writing it, so a caller cannot pass the read flag to the
 * write call and get something else. These continue the same bit space. */
#define SHIM_MSV_IN_PREPARATION 0x10
#define SHIM_MSV_FAILED         0x20

/* One entry, flattened, so Rust need not carry msvstd.h or know that TMsvEntry's text
 * fields are TPtrC into the CMsvEntry's own buffer.
 *
 * `details_len` and `description_len` are the FULL platform lengths; the arrays hold the
 * first min(len, capacity) units. The sizes are what the platform's own MTMs use for these
 * fields in practice, not a documented cap — there is none — so a caller that cares must
 * compare the length against the array size. */
typedef struct ShimMsvEntry {
    int32_t  id;
    int32_t  parent;
    int32_t  service_id;
    uint32_t mtm_uid;
    /* KUidMsvMessageEntryValue / ...ServiceEntry / ...FolderEntry / ...AttachmentEntry. */
    uint32_t type_uid;
    /* iDate, converted out of Symbian's year-0 epoch by the same helper the write side
     * uses — the two must not disagree about what a timestamp means. */
    int64_t  unix_time;
    int32_t  size;
    int32_t  flags;             /* SHIM_MSV_* */
    int32_t  details_len;       /* full length, may exceed 64 */
    int32_t  description_len;   /* full length, may exceed 128 */
    uint16_t details[64];
    uint16_t description[128];
} ShimMsvEntry;

int32_t shim_msv_entry(int32_t handle, int32_t id, ShimMsvEntry* out);

/* Children of a folder. Writes min(count, cap) ids and reports the full count, so a caller
 * that got a short answer can size a second call. */
int32_t shim_msv_children(int32_t handle, int32_t folder_id,
                          int32_t* out_ids, int32_t cap, int32_t* out_count);

/* Service entries of one MTM type — CMsvEntry(root)->ChildrenWithMtmL.
 *
 * This is how a service finds the account it created on a previous run instead of creating a
 * second one. shim_msv_delete_services exists to clean up after not having this. */
int32_t shim_msv_services(int32_t handle, uint32_t mtm_uid,
                          int32_t* out_ids, int32_t cap, int32_t* out_count);

/* The body text, as UTF-16. Fills what fits, reports the full character count.
 *
 * An entry with no body is length 0 and SHIM_OK, not SHIM_ERR_NOT_FOUND: a message without
 * body text is an ordinary thing — a notification, a placeholder — and making the caller
 * distinguish "empty" from "missing" would be inventing a difference the store does not
 * make. */
int32_t shim_msv_body(int32_t handle, int32_t id,
                      uint16_t* out, int32_t cap, int32_t* out_len);

/* Set and clear entry flags in one ChangeL. `set` wins where the two collide.
 *
 * Read-modify-write inside the shim rather than taking a whole entry from the caller,
 * because writing back a TMsvEntry that was read a moment ago undoes every field the server
 * has changed since — which is the same trap the create path documents, seen from the other
 * side. */
int32_t shim_msv_set_flags(int32_t handle, int32_t id, int32_t set, int32_t clear);

/* Reparent an entry — CMsvEntry(oldParent)->MoveL(id, newParent).
 *
 * How a sent reply leaves the outbox, and therefore also how a service records that it is
 * done: the parent folder is durable state that survives a restart, where a set of ids held
 * in a process is not. */
int32_t shim_msv_move_entry(int32_t handle, int32_t id, int32_t new_parent);

/* Start or stop delivering session events onto the event ring as SHIM_EV_MSV.
 *
 * Off by default. A one-shot probe has nothing to do with them, and events pushed to a ring
 * nobody drains are only a dropped-event count — so this is opt-in rather than a consequence
 * of opening a session.
 *
 * The delivery is bounded: at most a handful of events per platform notification, because a
 * bulk delete of a hundred entries would otherwise flush the whole ring and take with it the
 * events that mattered. That bound is safe only because an event is a hint and the reader
 * re-reads the store; see SHIM_EV_MSV. */
int32_t shim_msv_observe(int32_t handle, int32_t enable);

/* Carried in `a` of SHIM_EV_MSV. The four entry events carry a real id in `b`; the session
 * and MTM-registry ones carry 0, because the platform's notification for those is not about
 * an entry at all. */
#define SHIM_MSV_EV_CREATED        1   /* EMsvEntriesCreated */
#define SHIM_MSV_EV_CHANGED        2   /* EMsvEntriesChanged */
#define SHIM_MSV_EV_DELETED        3   /* EMsvEntriesDeleted */
#define SHIM_MSV_EV_MOVED          4   /* EMsvEntriesMoved */
#define SHIM_MSV_EV_MTM_INSTALLED  5   /* EMsvMtmGroupInstalled */
#define SHIM_MSV_EV_MTM_REMOVED    6   /* EMsvMtmGroupDeInstalled */
#define SHIM_MSV_EV_SERVER_READY   7   /* EMsvServerReady */
#define SHIM_MSV_EV_SERVER_GONE    8   /* EMsvServerTerminated, EMsvCloseSession */

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

/* ------------------------------------------------------------------ Bluetooth --
 * SHIM_USE_BT. Imports btmanclient, btdevice, bluetooth, btextnotifiers, esock and
 * centralrepository — six at once, which is why the first binary to carry them is a probe
 * of its own and not an app anybody stands on.
 *
 * # What is ours and what is the platform's
 *
 * The Bluetooth *server* is in ROM and every other consumer depends on it: the native OBEX
 * push, the headset profiles, the host's own btrecv.py. Nothing here replaces it. These
 * calls read and write the state that server keeps — the device registry, the power CenRep
 * key, the local-device record — so a change made through them is a change the native
 * Bluetooth screen sees too, and vice versa.
 *
 * # What is deliberately NOT here
 *
 * The settings that live in Publish & Subscribe: visibility (scanning), the local name, the
 * device class, accept-paired-only, and the "registry table changed" bell. Those are
 * ordinary P&S keys under KUidSystemCategory (0x101f75b6) with the key numbers in
 * bt_subscribe.h, and shim_prop already reads, writes and subscribes to any category — so
 * putting them here would be a second way to do something that already works. Rust names
 * the constants; see crates/symbian/src/bt.rs.
 *
 * The exception is visibility, which is *also* reachable through the registry's local-device
 * record, and is offered below because the probe needs to learn which of the two the handset
 * honours. */

/* Flags in ShimBtDevice::flags. Symbian has no "trusted" bit: S60's trusted means "needs no
 * authorisation before a connection", which is TBTDeviceSecurity::NoAuthorise. */
#define SHIM_BT_PAIRED    0x01
#define SHIM_BT_TRUSTED   0x02   /* NoAuthorise — connects without asking the user */
#define SHIM_BT_BLOCKED   0x04   /* Banned */
#define SHIM_BT_ENCRYPT   0x08
#define SHIM_BT_FRIENDLY  0x10   /* `name` came from FriendlyName, not the device's own name */

/* One remote device, from the registry or from an inquiry.
 *
 * `name_len` is the FULL length; the array holds the first min(len, 32) units. 32 because the
 * Bluetooth name maximum is 248 *bytes* of UTF-8 and no list on a 320x240 screen shows more
 * than a couple of dozen characters — a caller that needs the rest has the length to know it
 * was cut. */
typedef struct ShimBtDevice {
    uint8_t  addr[6];
    uint8_t  pad[2];
    uint32_t device_class;
    int32_t  flags;          /* SHIM_BT_* */
    int32_t  name_len;
    uint16_t name[32];
} ShimBtDevice;

/* This handset's own Bluetooth record. Every `int32_t` is -1 when the registry says the field
 * was never set, which is a different thing from zero: an unset scan-enable is not
 * "invisible", it is "the record does not say". */
typedef struct ShimBtLocal {
    uint8_t  addr[6];
    uint8_t  pad[2];
    uint32_t device_class;
    int32_t  scan_enable;    /* THCIScanEnable: 0 none, 1 inquiry, 2 page, 3 both */
    int32_t  limited;        /* limited-discoverable flag */
    int32_t  power_setting;
    int32_t  paired_only;
    int32_t  name_len;
    uint16_t name[32];       /* the local Bluetooth name, widened from UTF-8 */
} ShimBtLocal;

/* Is the radio on? Reads KCRUidBluetoothPowerState (0x10204DA9) key KBTPowerState — the key
 * apps/netd already publishes for the launcher's status bar. SHIM_OK with *out 0 or 1. */
int32_t shim_bt_power_get(int32_t* out_on);

/* Turn the radio on or off, and say how.
 *
 * Two routes, tried in order, because which one this handset honours is a measured fact and
 * not a documented one:
 *
 *   1. RNotifier + KPowerModeSettingNotifierUid (0x100059E2) — the documented S60 route.
 *      It raises the platform's own "Activate Bluetooth?" query, so it can only turn the
 *      radio ON, and it needs a user. Skipped entirely when `on` is 0.
 *   2. A CenRep write to the power key. Silent, no dialog, and undocumented as a *write*:
 *      btserversdkcrkeys.h describes the key as one the BT server updates.
 *
 * `*out_via` reports which route answered: SHIM_BT_VIA_NOTIFIER, SHIM_BT_VIA_CENREP, or 0 if
 * neither did. The return code is the last error seen when nothing worked. */
#define SHIM_BT_VIA_NOTIFIER 1
#define SHIM_BT_VIA_CENREP   2
int32_t shim_bt_power_set(int32_t on, int32_t* out_via);

/* This handset's own record — RBTRegServ + RBTLocalDevice::Get. */
int32_t shim_bt_local_get(ShimBtLocal* out);

/* Set the scan-enable through the registry's local-device record (THCIScanEnable 0..3).
 * The P&S set-scanning key is the other route; see the header note above. */
int32_t shim_bt_visibility_set(int32_t scan_enable);

/* Re-read the paired-device view and report how many there are — RBTRegistry::CreateView
 * with TBTRegistrySearch::FindBonded, then CBTRegistryResponse.
 *
 * The results are cached inside the shim until the next refresh, because a
 * CBTRegistryResponse owns its array and a caller holding an index into one it does not own
 * is exactly the lifetime problem handles exist to prevent. Read them out with
 * shim_bt_paired_get. */
int32_t shim_bt_paired_refresh(int32_t* out_count);

/* One device from the last refresh. SHIM_ERR_NOT_FOUND past the end. */
int32_t shim_bt_paired_get(int32_t index, ShimBtDevice* out);

/* Trust or untrust — read the nameless record, flip NoAuthorise, ModifyDevice.
 *
 * Read-modify-write inside the shim rather than taking a whole record from the caller, for
 * the same reason shim_msv_set_flags does: writing back a record read a moment ago undoes
 * every field the server has changed since. */
int32_t shim_bt_set_trusted(const uint8_t* addr6, int32_t trusted);

/* Forget a device — RBTRegistry::UnpairDevice. The link key goes; the record may remain as
 * a seen-but-unpaired device, which is the platform's behaviour and not ours. */
int32_t shim_bt_unpair(const uint8_t* addr6);

/* Rename — ModifyFriendlyDeviceNameL. The friendly name is ours to set; the device's own
 * Bluetooth name belongs to the device. */
int32_t shim_bt_rename(const uint8_t* addr6, const uint16_t* name, int32_t len);

/* Close the registry session and drop the cached view. Called from teardown; safe to call
 * when nothing was ever opened. */
int32_t shim_bt_close(void);

/* An inquiry, run to completion before returning — RHostResolver over KBTLinkManager.
 *
 * DAEMON ONLY, AND THE ABI SAYS SO IN ITS NAME. An inquiry takes on the order of ten
 * seconds. Called from rust_step on the GUI thread it would starve the window server, which
 * freezes the whole phone and not just the caller (docs/architecture.md, "rust_step must
 * return promptly"). A GUI app gets the CActive version, which arrives with the app that
 * needs it; this one exists so a headless probe can answer "does an inquiry work at all"
 * without first building the asynchronous machinery to ask.
 *
 * Bounded twice over: `budget_ms` against an RTimer, `max_devices` against the cache. Both
 * bounds are reported rather than silently applied — `*out_found` is what was collected, and
 * the return code is SHIM_ERR_TIMED_OUT when the budget ended it. Read results out with
 * shim_bt_found_get. */
int32_t shim_bt_inquiry_sync(int32_t budget_ms, int32_t max_devices, int32_t* out_found);

/* One device from the last inquiry. SHIM_ERR_NOT_FOUND past the end. */
int32_t shim_bt_found_get(int32_t index, ShimBtDevice* out);

/* ------------------------------------------------------------ RFCOMM sockets -- */

/* The result of shim_bt_rfcomm_probe: one Symbian error code per step of bringing an RFCOMM
 * server socket up, so a single call answers "does the remote-shell agent's transport work
 * on this handset at all" without building the asynchronous accept/read/write machinery
 * first. KErrNone (0) is success for each step; a step not reached is left as
 * SHIM_BT_PROBE_SKIPPED so "failed" and "never attempted" cannot be confused.
 *
 * Guarded by SHIM_USE_BTSOCK, which adds sdpdatabase — an import neither the bt probe nor
 * anything else here has linked — so it rides in an isolated probe until that probe reports.
 * See the header of shim_btsock.cpp. */
#define SHIM_BT_PROBE_SKIPPED (-0x7fffffff)
typedef struct ShimBtRfcommProbe {
    int32_t serv_err;     /* RSocketServ::Connect */
    int32_t open_err;     /* RSocket::Open over KRFCOMM */
    int32_t channel_err;  /* GetOpt(KRFCOMMGetAvailableServerChannel) */
    int32_t channel;      /* the server channel it handed back, -1 if unknown */
    int32_t bind_err;     /* Bind(TBTSockAddr) on that channel */
    int32_t sdp_open_err; /* RSdp::Connect + RSdpDatabase::Open */
    int32_t sdp_reg_err;  /* CreateServiceRecord + protocol-descriptor/name attributes */
    int32_t listen_err;   /* Listen() */
} ShimBtRfcommProbe;

/* Run the RFCOMM/SDP bring-up sequence once, synchronously, tearing everything down before
 * returning, and report each step into *out. Returns SHIM_OK when the sequence ran (even if a
 * step failed — read the struct), or an error if it could not run at all (e.g. out is null,
 * or a leave escaped the TRAP). DAEMON ONLY: opening the socket server and SDP is fast, but
 * this belongs to the headless probe, alongside the daemon that will use the real
 * asynchronous version. */
int32_t shim_bt_rfcomm_probe(ShimBtRfcommProbe* out);

/* ---- RFCOMM server, asynchronous ---- */
/*
 * The transport for the remote-shell agent. Mirrors the TCP socket API in shim_net.cpp: a
 * listener plus per-connection reader/writer active objects, each completion pushing a
 * SHIM_EV_BT_* event into the ring. The phone is the *server* — the shim has no Connect for
 * RFCOMM, because the laptop dials in. One listener per process (the shell serves one client
 * at a time); accepted sockets live in a small handle table.
 *
 * DAEMON ONLY in spirit — every call is non-blocking and returns at once, and the completions
 * arrive as events, so this is safe under the daemon pump. All are SHIM_ERR_NOT_SUPPORTED in a
 * build without USE_BTSOCK.
 */

/* Open the RFCOMM listener: claim a server channel, bind, register a persistent SPP SDP record
 * for `aServiceName`, and Listen with `backlog`. Synchronous; on success sets *out_channel to
 * the channel advertised. The record stays until shim_btrf_listen_stop or teardown. */
int32_t shim_btrf_listen_start(int32_t backlog, const uint16_t* name, int32_t name_len,
                               int32_t* out_channel);

/* Start one asynchronous Accept into a fresh accepted-socket slot. Completion arrives as
 * SHIM_EV_BT_ACCEPTED, whose `handle` is the new socket on success. Only one accept may be
 * outstanding at a time; a second call while one is pending is SHIM_ERR_IN_USE. */
int32_t shim_btrf_accept(void);

/* Start an asynchronous receive of up to `cap` bytes. `buf` must stay valid and untouched
 * until SHIM_EV_BT_RECV arrives for this handle. `a` on that event is the byte count. */
int32_t shim_btrf_recv(int32_t handle, uint8_t* buf, int32_t cap);

/* Start an asynchronous send. `buf` must stay valid and untouched until SHIM_EV_BT_SENT.
 * RFCOMM Write is all-or-nothing, so success means the whole buffer went. */
int32_t shim_btrf_send(int32_t handle, const uint8_t* buf, int32_t len);

/* Close one accepted socket, cancelling any outstanding recv/send first. */
int32_t shim_btrf_close(int32_t handle);

/* Deregister the SDP record and close the listener. Accepted sockets are left alone — close
 * them with shim_btrf_close. Safe to call when nothing is open. */
int32_t shim_btrf_listen_stop(void);

#ifdef __cplusplus
}
#endif

#endif /* SYMBIAN_SHIM_H */
