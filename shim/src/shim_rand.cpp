/* Entropy, and only entropy.
 *
 * This function does not produce random numbers. It produces a pool of bytes that are
 * hopefully unpredictable and definitely not uniform, and hands them to Rust, which runs
 * them through a SHA-256 DRBG. That split is deliberate: whitening is arithmetic, Rust has
 * the tested SHA-256, and there is no reason to write a second one in C++ where it cannot
 * be tested without a phone.
 *
 * WHY NOT random.dll
 *
 * `CSystemRandom` in random.dll is the platform's real CSPRNG and would be the better
 * primitive. It is not used, and the reason is a scar: adding six CCommsDatabase calls
 * added six ordinals to an already-imported commdb.dll, and the E72 stopped loading the
 * image entirely -- no panic, no log, no report file, because no application code ran. A
 * new DLL dependency is a deployment risk that cannot be tested for from here, and this
 * facility is on the critical path for every launch.
 *
 * `examples/selftest` probes random.dll through shim_dll_present, so whether that upgrade
 * is available on this handset is a question with an answer rather than a guess. Until the
 * answer is in, euser is what every binary already imports.
 *
 * WHAT THE SOURCES ARE WORTH
 *
 * Stated plainly, because a pool described as "random" invites more trust than it earns:
 *
 *   Math::Random()   The bulk of it. Symbian 9.x seeds this per-thread from the system,
 *                    but it is not documented as cryptographically secure and should not
 *                    be assumed to be. Called once per word rather than once per pool.
 *   FastCounter()    A high-resolution counter. Sampled between the Math::Random calls, so
 *                    it captures scheduling jitter -- the gap between two samples is not
 *                    constant and is not something an observer off the device can predict.
 *   NTickCount()     Millisecond uptime. Coarse, but unknown to anyone who did not watch
 *                    the phone boot.
 *   UniversalTime()  Wall clock in microseconds. Guessable to within a second by anyone,
 *                    and in for the low bits only.
 *   A stack address  Weak on Symbian, which has little address randomisation. In because
 *                    it costs nothing and differs between builds and threads.
 *   Heap state       Available() moves with allocation history, which depends on
 *                    everything the application has done so far.
 *
 * No single one of these carries the pool. That is the point of mixing six.
 */

#include "symbian_shim.h"
#include "shim_priv.h"

#include <e32std.h>
#include <e32math.h>
#include <hal.h>

extern "C" {

int32_t shim_entropy(uint8_t* out, int32_t len)
    {
    if (!out || len <= 0)
        return SHIM_ERR_ARGUMENT;

    TInt marker = 0;
    /* The address of a local, folded in once. Reading it as an integer is the point. */
    TUint32 acc = static_cast<TUint32>(reinterpret_cast<TUint>(&marker));

    TTime now;
    now.UniversalTime();
    const TInt64 micros = now.Int64();
    acc ^= static_cast<TUint32>(micros);
    acc ^= static_cast<TUint32>(micros >> 32);

    TInt heapFree = 0;
    User::Heap().Available(heapFree);
    acc ^= static_cast<TUint32>(heapFree);

    TInt i = 0;
    while (i < len)
        {
        /* One Math::Random per word rather than one per pool: if its internal state
         * advances at all, taking more of it takes more of whatever it has. */
        acc ^= Math::Random();
        /* Sampled here, inside the loop, so what lands in the pool is the *jitter* between
         * iterations rather than one reading of a clock. A loop that is preempted midway
         * mixes in that fact; one that is not, mixes in that it was not. */
        acc ^= User::FastCounter();
        acc += User::NTickCount();
        /* A rotate between words so a source that only ever moves the low bits does not
         * only ever land in the low bits. */
        acc = (acc << 7) | (acc >> 25);

        TInt n = len - i;
        if (n > 4)
            n = 4;
        for (TInt b = 0; b < n; b++)
            out[i + b] = static_cast<uint8_t>(acc >> (8 * b));
        i += n;
        }

    return SHIM_OK;
    }

} /* extern "C" */
