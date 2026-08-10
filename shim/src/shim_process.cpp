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

} /* extern "C" */

#endif /* SHIM_USE_PROC */
