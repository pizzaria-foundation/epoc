/* Timers, and the clock.
 *
 * This is the smallest piece of Symbian asynchrony, which makes it the right place
 * to prove the CActive-to-ring-buffer pattern before sockets need it. The shape is
 * identical: a CActive issues a request, its RunL converts the completion into a
 * POD event, and rust_step drains it. If timers work, sockets are the same code
 * with RSocket in place of RTimer.
 */

#include "shim_priv.h"

#include <e32base.h>
#include <e32std.h>
#include <hal.h>

namespace {

const TInt KMaxTimers = 8;

class CShimTimer : public CTimer
    {
public:
    static CShimTimer* NewL(TInt aHandle);
    void After(TInt aMs);
    void Every(TInt aMs);

private:
    CShimTimer(TInt aHandle);
    void ConstructL();
    void RunL();
    /* CTimer's DoCancel already calls RTimer::Cancel, so there is nothing to add. */

    TInt iHandle;
    /* Non-zero for a repeating timer: RunL re-arms itself with this interval.
     * Deliberately not CPeriodic — CPeriodic's callback is a TCallBack, not a
     * CActive completion, and keeping every asynchronous source on the same
     * CActive path means one pattern to reason about instead of two. */
    TInt iRepeatMs;
    };

CShimTimer* gTimers[KMaxTimers];

CShimTimer::CShimTimer(TInt aHandle)
    : CTimer(EPriorityStandard), iHandle(aHandle), iRepeatMs(0)
    {
    }

CShimTimer* CShimTimer::NewL(TInt aHandle)
    {
    CShimTimer* self = new (ELeave) CShimTimer(aHandle);
    CleanupStack::PushL(self);
    self->ConstructL();
    CleanupStack::Pop(self);
    return self;
    }

void CShimTimer::ConstructL()
    {
    CTimer::ConstructL();
    CActiveScheduler::Add(this);
    }

void CShimTimer::After(TInt aMs)
    {
    iRepeatMs = 0;
    Cancel();
    /* TTimeIntervalMicroSeconds32 is a signed 32-bit microsecond count, so the
     * ceiling is ~2147 seconds — about 35 minutes. Anything longer needs
     * RTimer::At with a TTime, which no caller wants yet; clamp rather than
     * silently wrap into a negative interval. */
    const TInt maxMs = 2000 * 1000;
    CTimer::After(Min(aMs, maxMs) * 1000);
    }

void CShimTimer::Every(TInt aMs)
    {
    iRepeatMs = aMs;
    Cancel();
    const TInt maxMs = 2000 * 1000;
    CTimer::After(Min(aMs, maxMs) * 1000);
    }

void CShimTimer::RunL()
    {
    ShimPushSimple(SHIM_EV_TIMER, iHandle, iStatus.Int(), 0);
    if (iRepeatMs > 0 && iStatus.Int() == KErrNone)
        {
        /* Re-arm from now rather than from the deadline. That drifts, but a UI frame
         * clock wants "not faster than this" rather than a precise cadence, and
         * catching up after a slow frame would burst several redraws back to back. */
        CTimer::After(iRepeatMs * 1000);
        }
    }

TInt AllocSlot()
    {
    for (TInt i = 0; i < KMaxTimers; i++)
        if (!gTimers[i])
            return i;
    return KErrNoMemory;
    }

CShimTimer* TimerFor(TInt aHandle)
    {
    if (aHandle < 0 || aHandle >= KMaxTimers)
        return NULL;
    return gTimers[aHandle];
    }

} /* namespace */

void ShimTimersCleanup()
    {
    for (TInt i = 0; i < KMaxTimers; i++)
        {
        if (gTimers[i])
            {
            gTimers[i]->Cancel();
            delete gTimers[i];
            gTimers[i] = NULL;
            }
        }
    }

extern "C" {

static int32_t StartTimer(int32_t ms, int32_t* handle, TBool aRepeat)
    {
    if (!handle || ms < 0)
        return SHIM_ERR_ARGUMENT;
    const TInt slot = AllocSlot();
    if (slot < 0)
        return SHIM_ERR_NO_MEMORY;

    TInt err = KErrNone;
    TRAP(err, gTimers[slot] = CShimTimer::NewL(slot));
    if (err != KErrNone)
        return err;

    if (aRepeat)
        gTimers[slot]->Every(ms);
    else
        gTimers[slot]->After(ms);

    *handle = slot;
    return SHIM_OK;
    }

int32_t shim_timer_after(int32_t ms, int32_t* handle)
    {
    return StartTimer(ms, handle, EFalse);
    }

int32_t shim_timer_every(int32_t ms, int32_t* handle)
    {
    return StartTimer(ms, handle, ETrue);
    }

void shim_timer_cancel(int32_t handle)
    {
    CShimTimer* t = TimerFor(handle);
    if (!t)
        return;
    t->Cancel();
    delete t;
    gTimers[handle] = NULL;
    }

uint64_t shim_now_us(void)
    {
    /* User::NTickCount is the nanokernel tick, about a millisecond, and it is
     * monotonic — unlike TTime, which the user can change from the clock app. For
     * measuring a frame that distinction is the whole point.
     *
     * The period is queried rather than assumed: it is not the same on every
     * device, and hardcoding 1000 would silently skew every measurement.
     *
     * ENanoTickPeriod is in MICROSECONDS, despite the name. hal_data.h says so in as
     * many words -- "The time between nanokernel ticks, in microseconds" -- and reading
     * nanoseconds off the name made this function return milliseconds while claiming
     * microseconds. Every duration the device self test printed was 1000x too small,
     * including a 2048-bit modpow that appeared to take 0 ms; the giveaway was a
     * framebuffer fill implying 66666 fps. A unit is not documentation, and a name is
     * not a unit. */
    TInt periodUs = 0;
    if (HAL::Get(HALData::ENanoTickPeriod, periodUs) != KErrNone || periodUs <= 0)
        periodUs = 1000;   /* 1 ms, the usual value */
    const TUint ticks = User::NTickCount();
    return static_cast<uint64_t>(ticks) * static_cast<uint64_t>(periodUs);
    }

int64_t shim_unix_time(void)
    {
    /* Wall clock, for message timestamps. It drifts and the user can change it, so
     * a networked client should correct against the server rather than trust it —
     * MTProto in particular rejects a msg_id more than 30 s ahead or 300 s behind. */
    TTime now;
    now.UniversalTime();
    TTime epoch(TDateTime(1970, EJanuary, 0, 0, 0, 0, 0));
    TTimeIntervalSeconds secs;
    if (now.SecondsFrom(epoch, secs) != KErrNone)
        return 0;
    return static_cast<int64_t>(secs.Int());
    }

int32_t shim_utc_offset(void)
    {
    /* Seconds east of UTC: positive for CET, negative for Brazil.
     * Added to a UTC timestamp to get local wall-clock time. */
    return User::UTCOffset().Int();
    }

} /* extern "C" */
