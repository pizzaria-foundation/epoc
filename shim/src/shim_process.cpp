/* Process launch and query, for a GUI app starting its own headless daemon.
 *
 * The controller is an ordinary Avkon app; the daemon is a separate EXE in \sys\bin with
 * no UI. The controller launches it here with RProcess::Create, which needs no capability —
 * creating a process is not a privileged act, running with capabilities the image was
 * signed for is. The child declares its own caps, and gets them regardless of ours.
 *
 * `Start` waits for the child's Rendezvous rather than returning the moment Resume is
 * called. The daemon signals Rendezvous(KErrNone) once its active scheduler is up and the
 * bridge is arming, so a caller that gets SHIM_OK knows the daemon is actually running, not
 * merely that a process object was created. Without that wait the controller would report
 * "started" for a process that panicked on its first line.
 */

#include "shim_priv.h"

#ifdef SHIM_USE_PROC

#include <e32std.h>
#include <e32base.h>

extern "C" {

/* Start a process and do not wait for it.
 *
 * The one that is safe to call from a GUI thread, and the reason the others are not:
 * `User::WaitForRequest` on a thread with a running CActiveScheduler consumes whatever
 * completes next, including completions belonging to active objects. The scheduler then
 * finds a signal for a request it does not own and the process dies with a stray-signal
 * panic — a kernel panic, so the Rust handler never runs and no breadcrumb is written.
 *
 * Measured, not theorised: the launcher died on roughly two starts in three, always in the
 * `start_daemon` call for the first daemon that was not already running, always with an
 * empty panic.txt, and the surviving sessions were exactly the ones where every daemon was
 * already up and this code was therefore never reached. It is probabilistic because it
 * needs another completion to land inside the wait, and a home screen with a repeating
 * timer, two P&S subscriptions and a window server connection supplies one constantly.
 *
 * The caller loses the rendezvous, which is a real loss: SHIM_OK here means a process
 * object was created, not that the child is alive. Anyone who needs to know polls
 * shim_process_running afterwards, which is what the launcher already did with the answer
 * it was throwing away. */
int32_t shim_process_spawn(const uint16_t* path, int32_t path_len)
    {
    if (!path || path_len <= 0)
        return SHIM_ERR_ARGUMENT;

    TPtrC16 name(reinterpret_cast<const TUint16*>(path), path_len);
    RProcess proc;
    TInt rc = proc.Create(name, KNullDesC);
    if (rc != KErrNone)
        return rc;
    proc.Resume();
    proc.Close();
    return SHIM_OK;
    }

/* NOT safe from a thread running an active scheduler — see shim_process_spawn. For a
 * headless daemon or a probe whose whole job is to block on its child. */
int32_t shim_process_start(const uint16_t* path, int32_t path_len)
    {
    if (!path || path_len <= 0)
        return SHIM_ERR_ARGUMENT;

    TPtrC16 name(reinterpret_cast<const TUint16*>(path), path_len);

    RProcess proc;
    /* No command-line argument: the daemon reads everything it needs from its own private
     * dir and its build-time EPOCADB_HOST. An empty descriptor, not KNullDesC cast games. */
    TInt rc = proc.Create(name, KNullDesC);
    if (rc != KErrNone)
        return rc;

    /* Rendezvous request armed before Resume, so a child that reaches its signal fast
     * cannot complete into a request that is not yet outstanding. */
    TRequestStatus status;
    proc.Rendezvous(status);
    if (status != KRequestPending)
        {
        /* The child died before it could rendezvous — Create succeeded but the image is
         * bad, or a static ctor panicked. Kill and report. */
        proc.Kill(KErrNone);
        proc.Close();
        return status.Int() == KErrNone ? SHIM_ERR_GENERAL : status.Int();
        }

    proc.Resume();
    User::WaitForRequest(status);
    TInt signalled = status.Int();

    /* The handle can be closed now: the OS keeps the process alive independently of this
     * handle, and the controller checks liveness later by UID via shim_process_running. */
    proc.Close();

    return signalled == KErrNone ? SHIM_OK : signalled;
    }

/* NOT safe from a thread running an active scheduler — see shim_process_spawn. */
int32_t shim_process_start_timeout(const uint16_t* path, int32_t path_len, int32_t timeout_ms)
    {
    if (!path || path_len <= 0 || timeout_ms <= 0)
        return SHIM_ERR_ARGUMENT;

    TPtrC16 name(reinterpret_cast<const TUint16*>(path), path_len);

    RProcess proc;
    TInt rc = proc.Create(name, KNullDesC);
    if (rc != KErrNone)
        {
        /* The image would not load. For the launcher this is the whole point of calling:
         * it is what a missing import looks like from the outside, and it arrives here as
         * a number instead of as a probe that silently produced nothing. */
        return rc;
        }

    TRequestStatus rendezvous;
    proc.Rendezvous(rendezvous);
    if (rendezvous != KRequestPending)
        {
        proc.Kill(KErrNone);
        proc.Close();
        return rendezvous.Int() == KErrNone ? SHIM_ERR_GENERAL : rendezvous.Int();
        }

    /* The deadline. shim_process_start above waits on the rendezvous with no escape, which
     * is right for a daemon a controller cannot proceed without — but wrong for a probe,
     * where a child that neither signals nor dies would hang the one process whose job is
     * to survive its children and report on them. */
    RTimer timer;
    rc = timer.CreateLocal();
    if (rc != KErrNone)
        {
        proc.Kill(KErrNone);
        proc.Close();
        return rc;
        }
    TRequestStatus deadline;
    /* Microseconds, and a TInt32 caps at ~35 minutes — far beyond any probe's budget, so
     * the multiplication cannot overflow for a caller passing a sane timeout. */
    timer.After(deadline, timeout_ms * 1000);

    proc.Resume();
    User::WaitForRequest(rendezvous, deadline);

    int32_t result;
    if (rendezvous != KRequestPending)
        {
        /* Rendezvous won. Cancel the timer and consume its completion, or the next
         * WaitForRequest in this thread would take a stale signal for its own. */
        timer.Cancel();
        User::WaitForRequest(deadline);
        result = rendezvous.Int() == KErrNone ? SHIM_OK : rendezvous.Int();
        }
    else
        {
        /* The deadline won. Kill the child and consume its rendezvous, for the same
         * reason. A probe killed here leaves whatever it had already flushed, which is the
         * arrangement the report format is built around. */
        proc.Kill(KErrNone);
        User::WaitForRequest(rendezvous);
        result = SHIM_ERR_TIMED_OUT;
        }

    timer.Close();
    proc.Close();
    return result;
    }

int32_t shim_process_running(uint32_t uid3)
    {
    /* Match on UID3, the field that identifies the application/executable, against every
     * process on the device. TFindProcess matches by name pattern, not UID, so this walks
     * processes and compares the third UID of each. */
    TFindProcess finder;
    TFullName fullName;
    while (finder.Next(fullName) == KErrNone)
        {
        RProcess proc;
        if (proc.Open(finder) != KErrNone)
            continue;
        /* Skip a process that is already dying: its handle opens but it is on the way out,
         * and reporting it as running would make the controller refuse to relaunch. */
        TBool alive = (proc.ExitType() == EExitPending);
        TUidType type = proc.Type();
        proc.Close();
        if (alive && type[2].iUid == (TInt32) uid3)
            return 1;
        }
    return 0;
    }

/* Kill every live process whose UID3 matches — the escape hatch for a resident launcher that has
 * captured keys and will not close on its own. Same walk as shim_process_running; on a match, Kill
 * rather than report. Killing a process this one did not create needs PowerMgmt, which a
 * ROM-patched handset grants at load regardless of the image's declared capabilities. SHIM_OK if
 * at least one was killed, SHIM_ERR_NOT_FOUND if none matched. */
int32_t shim_process_kill(uint32_t uid3)
    {
    TFindProcess finder;
    TFullName fullName;
    TInt killed = 0;
    while (finder.Next(fullName) == KErrNone)
        {
        RProcess proc;
        if (proc.Open(finder) != KErrNone)
            continue;
        TBool alive = (proc.ExitType() == EExitPending);
        TUidType type = proc.Type();
        if (alive && type[2].iUid == (TInt32) uid3)
            {
            proc.Kill(0);
            killed++;
            }
        proc.Close();
        }
    return killed > 0 ? SHIM_OK : SHIM_ERR_NOT_FOUND;
    }

} /* extern "C" */

#endif /* SHIM_USE_PROC */
