---
name: app-experience
description: Design and build the user experience of an app on the E72 — a 600 MHz single-core handset with a 4 MB heap, a 320×240 screen and a slow, metered link. Use when writing or reviewing any screen, flow, loading state, download, media path or cache layer, and whenever something feels slow, blocks the UI, or refetches what it already had.
---

# Building an experience on a 2009 handset

The hardware decides most of this. A design that would be fine on any modern phone —
fetch on open, re-layout per frame, block until it arrives — is not slow here, it is
*broken* here. Three things drive every decision: **one core at 600 MHz**, **a 4 MB
heap** (`app.conf: HEAP=0x1000,0x400000`), and **a link that is slow, metered by the
kilobyte and often not there at all**.

## The budget, measured on the handset

From the device self test (`docs/device-notes.md` — these replaced the scaled-from-host
guesses, and three of them change decisions):

| | E72 |
|---|---|
| full-screen fill (320×240) | 0.6 ms |
| present (RGB565 → XRGB8888 + BitBlt) | **15.1 ms** |
| frame total | 15.7 ms → 63 fps |
| 64 KB file write | ~46 ms |
| 64 KB file read | 5 ms |
| SHA-256 | 8 MB/s |
| AES-256 | **169 KB/s** |
| 2048-bit modpow | 815 ms on the GUI thread, 1933 ms on the worker |

What follows from it:

- **Drawing less does almost nothing for frame rate.** Present is 96% of the frame and
  is paid per screen area whatever the frame contains. Do not micro-optimise draw calls;
  the optimisation that would pay is a dirty-rectangle present.
- **A file write is three frames.** ~46 ms for 64 KB. A write on a key-press path is a
  visible stutter. `Trace` accepts that cost on purpose (a full rewrite per line) because
  losing the last line before a crash costs more; nothing else gets that excuse.
- **The worker thread does not make work faster, it makes it not block.** The same modpow
  went 815 ms → 1933 ms wall on the worker, because it is one core shared with a GUI that
  keeps redrawing — and the interface stayed alive for all of it. That is the trade. A
  design that assumes background work is free is wrong on this hardware.
- **AES at 169 KB/s** means anything that encrypts a payload pays real time per kilobyte.

## Rule 1 — nothing blocks the event loop

Avkon owns the loop; Rust is always a callee. Every long thing is a **state machine
driven by events**, holding its progress in a field. The app already has three to copy:

- `PendingFile` — a download assembled chunk by chunk (128 KiB per request; a single
  request for the protocol maximum produces a frame the transport rejects), bounded by
  `MAX_FILE_BYTES` because the heap holds the assembly, the arriving chunk and the
  decoder's copy at the same time.
- `decoding: Option<(usize, Decoder)>` — image decode resumed on each event, never one
  blocking call.
- the driver's request/reply tags — the reply routes back by tag, not by waiting.

If a new feature needs a `while` loop over network or disk, it is the wrong shape.

## Rule 2 — a phase that can be slow narrates itself

The access-point sweep once wrote nothing for two and a half minutes and looked hung; it
was working. The lesson from `device-notes.md` is that a silent slow phase is
indistinguishable from a dead one — for the user *and* for you.

- Feedback must land **within the frame of the key press**, even when the result is 30 s
  away. Something changed = the press registered.
- Say it **where the user is looking**. `App::say` sets both the chat-list status *and*
  the conversation's own note, because setting only the first is why a download reported
  progress to a screen that was not on top while the user stared at "abrindo…".
- Narrate with numbers when you have them: `arquivo: 240 KB, no cache` beats "loading".
- Never a modal that cannot be escaped. The user must be able to back out of a download.

## Rule 3 — the cache is a layer, not an optimisation

**Every app that touches the network needs one, designed in, not added later.** The
reason is the link, not the disk: a photo takes tens of seconds and the connection is
metered by the kilobyte. Backing out of a picture and opening it again must not pay for
it twice — and before `apps/telegram/src/media_cache.rs` existed, it did every time,
because the download path wrote one scratch file that the next download overwrote.

Read `media_cache.rs` before writing another cache. Its shape is the shape:

| decision | why |
|---|---|
| keyed by an **immutable** id | the bytes behind a Telegram file id never change — an edit gets a new id. A hit cannot be stale, so there is nothing to validate. A key whose meaning can change buys an invalidation problem this app cannot afford. |
| lives in `C:\private\<UID3>\` | the one place an unsigned app writes with no capability (`symbian::fs::private_path`) |
| flat files with a name prefix (`m<id>.bin`) | creating a directory is another call that can fail; a prefix separates just as well |
| `fs::write_atomic` always | an interrupted write must leave the previous file, never a truncated one later served as a complete download |
| a read error is a **miss**, not an error | a corrupt entry must not block the download that would fix it |
| a write failure is **silent** | the photo is already on screen; "could not cache" over it is noise |
| a per-entry cap (`MAX_CACHED = 1 MB`) | one video must not fill a 250 MB cage shared with every other app on the phone |

### Check the tiers in order, and show each as it arrives

1. **Memory** — already-parsed model (`Store`). Free.
2. **Disk** — cached bytes. 5 ms per 64 KB; effectively free next to the link.
3. **Network** — last, and only for what the first two did not have.

Show whatever exists *now* rather than waiting for the best version: `photoCachedSize`
embeds a whole JPEG in the message itself, so a thumbnail can be on screen at zero cost
while the full file downloads. Anything free that arrived with the data you already have
should be drawn before a request is made.

### Cache the layout too

Text wrapping at this size is real work, and per-frame re-layout is the classic mistake.
`conv::Transcript` lays the transcript out once and `is_stale(chat, avail_w)` recomputes
only when the width or the message count changes. Any screen that measures text should
own the same pair: a laid-out cache plus an explicit staleness rule.

### What is not a cache

Session state. The auth key in `session_store.rs` is not a cached copy of anything — it
*is* the account, it has no source to refetch from, and it is fixed-width with no parser
so a half-read cannot look like a success. Do not fold it into a cache layer.

## Network: assume slow, metered, absent, and silent

- **Every wait has a deadline.** A host or peer killed mid-session leaves a socket that
  is open and silent; no socket error reports that, only a clock. `crates/epocadb` is the
  worked example — connect timeout, reply timeout, a miss counter, backoff 1 s → 64 s.
- **One request in flight per thing.** `load_more_dialogs` returns early when a page is
  already loading or the set is complete. An in-flight guard plus a completion flag,
  always — without them a scroll fires a request per event.
- **Paginate; never fetch a whole history.** The heap is 4 MB.
- **Distinguish failures for the user.** An access point that does not exist answers in
  ~450 ms; one that is unreachable took 35 s in testing. "Sem rede" and "demorou" are
  different sentences and the user acts differently on each.
- **Offline is a normal state, not an error screen.** Cached content renders; the network
  parts say what they are waiting for. An app whose list is empty without a bearer threw
  away the cache it should have had.

## The interface itself

- 320×240, about five rows visible, **no touchscreen** — D-pad, softkeys and QWERTY.
  Every action must be reachable without a pointer, and the selection band is the only
  thing telling the user where they are.
- Use the SDK's arithmetic rather than rewriting it (`symbian_ui::list`, `edit`,
  `chrome`) — and run the `sdk-abstraction-check` skill on anything new before calling
  the screen done.
- Colours, spacings and row heights come from `Theme`/`Metrics`, never literals.
- Long text truncates on a char boundary, not a byte one.

## Before calling a flow done

- [ ] No path blocks: every slow thing is a state machine resumed on events.
- [ ] Every key press changes something visible in the same frame.
- [ ] Progress is written where the user is actually looking, not only to the home screen.
- [ ] Anything fetched over the link is cached, keyed by something immutable, written
      atomically, capped per entry, and a read failure is a miss.
- [ ] The cache is checked *before* the request, and the free/embedded version is drawn
      first.
- [ ] Layout that depends on text is cached with an explicit staleness rule.
- [ ] Every network wait has a deadline, an in-flight guard and a backoff.
- [ ] The screen still works with no bearer at all.
- [ ] Nothing new allocates per frame; no unbounded `Vec` grows against a 4 MB heap.
- [ ] Timings that matter were **measured** on the device (via `Trace` / epocadb — see the
      `epocadb-logging` skill), not estimated. A wrong measurement looks exactly like a
      right one; sanity-check it against what the hardware can physically do.
