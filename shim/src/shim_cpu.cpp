/* Per-thread CPU time, which is the only load measurement this platform offers.
 *
 * There is no "CPU %" anywhere in Symbian: no HAL attribute, no UserHal call, no RProcess method.
 * What there is is RThread::GetCpuTime — cumulative microseconds that one thread has spent on the
 * processor. Difference it across an interval, divide by the interval, and that is utilisation;
 * sum a process's threads and that is the process's. Everything a task manager wants is built from
 * this one number.
 *
 * The catch, and the reason this file exists behind its own gate before anything draws a bar: on
 * Symbian 9.x, thread CPU-time accounting is a kernel build option. Where it is off, GetCpuTime
 * answers KErrNotSupported — and the header carries no doc comment at all to say which this is. So
 * the return code is the finding, and the isolated probe is how it gets taken.
 *
 * Enumeration is by name. TFindThread matches "process*::*" patterns against full thread names, and
 * a thread handle opened from a match is enough to ask; no capability is involved, and nothing here
 * modifies anything.
 */

#include "shim_priv.h"

#ifdef SHIM_USE_CPUTIME

#include <e32std.h>
#include <e32base.h>

extern "C" {

/* Sum the CPU time of every thread whose full name matches `pattern`, in microseconds.
 *
 * A Symbian thread's full name is "process[uid]0001::threadname", so "foo*::*" is every thread of
 * every process called foo. `*::*` is every thread on the phone.
 *
 * Returns SHIM_OK with *total_us and *threads set, SHIM_ERR_NOT_SUPPORTED if the kernel does not
 * account for thread CPU time, or the platform error. A thread that dies mid-walk is skipped rather
 * than failing the sweep — the list is a snapshot of something that is moving.
 */
int32_t shim_cpu_time(const uint16_t* pattern, int32_t pattern_len,
                      int64_t* total_us, int32_t* threads)
    {
    if (total_us)
        *total_us = 0;
    if (threads)
        *threads = 0;
    if (!pattern || pattern_len <= 0)
        return SHIM_ERR_ARGUMENT;

    TPtrC match((const TUint16*) pattern, pattern_len);
    TFindThread finder(match);
    TFullName name;

    TInt64 sum = 0;
    TInt found = 0;
    /* Sticky: one unsupported answer is the whole platform's answer, but a single thread that
     * refuses for its own reason (dying, protected) must not be mistaken for that. So an
     * unsupported result is only reported when nothing at all could be read. */
    TBool anyOk = EFalse;
    TInt lastErr = KErrNone;

    while (finder.Next(name) == KErrNone)
        {
        RThread thread;
        if (thread.Open(finder) != KErrNone)
            continue;   /* it exited between being listed and being opened */

        TTimeIntervalMicroSeconds cpu;
        const TInt rc = thread.GetCpuTime(cpu);
        thread.Close();

        if (rc != KErrNone)
            {
            lastErr = rc;
            continue;
            }
        anyOk = ETrue;
        sum += cpu.Int64();
        found++;
        }

    if (!anyOk)
        return lastErr == KErrNone ? SHIM_ERR_NOT_FOUND : lastErr;

    if (total_us)
        *total_us = sum;
    if (threads)
        *threads = found;
    return SHIM_OK;
    }

/* The name of the nth running process, for a caller that wants to walk them.
 *
 * TFindProcess yields "name[uid]0001"-shaped full names; the UID is in there, which is how a
 * caller pairs a process with an app without opening it. Returns the length written, or
 * SHIM_ERR_NOT_FOUND once the list is exhausted.
 */
int32_t shim_process_at(int32_t index, uint16_t* out, int32_t cap, int32_t* len)
    {
    if (len)
        *len = 0;
    if (!out || cap <= 0 || index < 0)
        return SHIM_ERR_ARGUMENT;

    _LIT(KAll, "*");
    TFindProcess finder(KAll);
    TFullName name;

    for (TInt i = 0; i <= index; i++)
        {
        if (finder.Next(name) != KErrNone)
            return SHIM_ERR_NOT_FOUND;
        }

    TInt n = name.Length();
    if (n > cap)
        n = cap;
    Mem::Copy(out, name.Ptr(), n * 2);
    if (len)
        *len = n;
    return SHIM_OK;
    }

} // extern "C"

#else  /* !SHIM_USE_CPUTIME */

extern "C" {

int32_t shim_cpu_time(const uint16_t* pattern, int32_t pattern_len,
                      int64_t* total_us, int32_t* threads)
    {
    (void) pattern;
    (void) pattern_len;
    if (total_us)
        *total_us = 0;
    if (threads)
        *threads = 0;
    return SHIM_ERR_NOT_SUPPORTED;
    }

int32_t shim_process_at(int32_t index, uint16_t* out, int32_t cap, int32_t* len)
    {
    (void) index;
    (void) out;
    (void) cap;
    if (len)
        *len = 0;
    return SHIM_ERR_NOT_SUPPORTED;
    }

} // extern "C"

#endif
