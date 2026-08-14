/* The event ring buffer.
 *
 * This is the seam that lets Rust be a callee. Avkon owns
 * `CActiveScheduler::Start()` and every Symbian I/O completion arrives inside a
 * `CActive::RunL()`, so there is no loop for Rust to own. Instead each RunL
 * converts its completion into a POD event, drops it here, and returns; a CIdle
 * pump later calls `rust_step()`, which drains the queue.
 *
 * Two properties matter and both are why this is a fixed array rather than
 * anything cleverer:
 *
 *   - It cannot allocate. RunL runs with the cleanup stack in an unknown state
 *     and an allocation failure there would leave via a path nobody is trapping.
 *   - It cannot leave. Same reason.
 *
 * When the ring fills we drop the *newest* event and count it. Dropping the
 * oldest would be worse: input arrives in order and losing the middle of a
 * keystroke sequence is more confusing than losing the tail, which the user can
 * simply repeat. `shim_events_dropped` exists so the drop is visible rather than
 * mysterious — a non-zero count means rust_step is too slow, which is a real
 * measurement and not a guess.
 */

#include "symbian_shim.h"

#include <e32std.h>

namespace {

/* 64 events is ~12 key presses plus room for redraw and timer traffic. The E72
 * cannot generate input faster than a person can type, so overflow means
 * rust_step blocked, not that the queue is too small. */
const TInt KQueueSize = 64;

ShimEvent gQueue[KQueueSize];
TInt gHead = 0;   /* next slot to write */
TInt gTail = 0;   /* next slot to read  */
TInt gCount = 0;
TInt gDropped = 0;

/* The pump's wake-up. NULL in a build that never registers one (the headless daemon polls on a
 * CPeriodic and needs no nudge); set by the GUI shim to restart its sleeping drain pump. */
void (*gPumpKick)() = NULL;

} /* namespace */

void ShimSetPumpKick(void (*aKick)())
    {
    gPumpKick = aKick;
    }

TInt ShimEventCount()
    {
    return gCount;
    }

/* Called from RunL and from OfferKeyEventL. Not part of the Rust-facing ABI, so
 * it is declared in shim_priv.h rather than symbian_shim.h. */
void ShimPushEvent(const ShimEvent& aEvent)
    {
    if (gCount >= KQueueSize)
        {
        gDropped++;
        return;
        }
    gQueue[gHead] = aEvent;
    gHead = (gHead + 1) % KQueueSize;
    gCount++;
    /* Wake the drain pump. Cheap and idempotent: the kick no-ops if the pump is already awake, so
     * paying it on every push (rather than only on the empty→non-empty edge) costs nothing and
     * keeps this function free of any assumption about the pump's state. */
    if (gPumpKick)
        gPumpKick();
    }

void ShimPushSimple(TInt aKind, TInt aHandle, TInt aStatus, TInt aA)
    {
    ShimEvent e;
    e.kind = aKind;
    e.handle = aHandle;
    e.status = aStatus;
    e.a = aA;
    e.b = 0;
    e.c = 0;
    e.d = 0;
    /* Every field, always. ShimEvent is a stack POD, so a field left unset ships
     * whatever was on the stack — and it crosses into Rust, where a garbage value in
     * a field the app happens to read is a bug with no trail back to here. */
    e.native = 0;
    ShimPushEvent(e);
    }

extern "C" {

int32_t shim_poll_event(ShimEvent* out)
    {
    if (!out || gCount == 0)
        return 0;
    *out = gQueue[gTail];
    gTail = (gTail + 1) % KQueueSize;
    gCount--;
    return 1;
    }

int32_t shim_events_dropped(void)
    {
    TInt n = gDropped;
    gDropped = 0;
    return n;
    }

} /* extern "C" */
