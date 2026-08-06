/* A second thread, for work too slow to do on the GUI thread.
 *
 * rust_step runs from a CIdle on the GUI thread and must return in milliseconds. A
 * long one starves the window server, which freezes the whole phone rather than just
 * this app — there is no watchdog that rescues you. A 2048-bit modular exponentiation
 * measures 0.4-0.6 s on this hardware, and a protocol handshake needs two, so the
 * login of any real client cannot happen in the pump.
 *
 * ONE THREAD PER JOB
 *
 * The worker is created when a job is submitted and exits when the job is done, rather
 * than parking on a semaphore between jobs. Thread creation on Symbian costs a
 * millisecond or so, which against a job measured in hundreds of milliseconds is
 * noise — and it buys two things worth more than that: no synchronisation primitive to
 * get wrong, and "one job at a time" falling out of the design instead of being
 * enforced.
 *
 * The job struct is written before Resume() and read after, so the thread creation is
 * the happens-before edge and no barrier is needed.
 *
 * WHICH HEAP
 *
 * RThread::Create has an overload taking an RAllocator*, which would let the worker
 * share the GUI thread's heap. That is deliberately not used: a default RHeap is not
 * thread-safe, so two threads allocating on it race.
 *
 * With its own heap, Rust's global allocator on the worker resolves to the worker's
 * RHeap, because shim_alloc calls User::Alloc, which is per-thread. Anything allocated
 * there and freed there is fine. What is not fine is an allocation *escaping* — a Vec
 * built on the worker and dropped on the GUI thread would be a cross-heap free, which
 * is silent corruption rather than a clean failure.
 *
 * So the contract is not "the job must not allocate". It is: nothing the job allocates
 * may outlive it. A temporary is fine; a returned buffer is not, which is why the
 * output buffer is the caller's and the job only writes into it.
 *
 * CANCELLATION
 *
 * There is none. A running computation cannot be interrupted, so DoCancel waits for
 * the thread to die — which blocks the GUI thread, and is acceptable because the only
 * caller of Cancel is teardown. Completing iStatus without waiting would let the app
 * tear down the job struct while the worker still holds pointers into it.
 */

#include "symbian_shim.h"
#include "shim_priv.h"

#include <e32base.h>
#include <e32std.h>

namespace {

/* What the worker needs, written by the GUI thread before the thread starts. */
struct TJob
    {
    TInt iOpcode;
    const TUint8* iIn;
    TInt iInLen;
    TUint8* iOut;
    TInt iOutLen;
    /* The GUI thread's id, which the worker opens a handle to. An id rather than a
     * handle, and that distinction closed a crash: RHandleBase::Duplicate takes the
     * handle to copy from `this->iHandle` and uses its argument only to say whose handle
     * space that value lives in. Handing it a default-constructed RThread therefore asks
     * the kernel to duplicate handle 0 — KERN-EXEC 0, on the GUI thread, which closes the
     * application. RThread::Open(TThreadId) has no such reading. */
    TThreadId iGuiId;
    TRequestStatus* iStatus;
    };

TJob gJob;

/* The worker's entry point.
 *
 * No CTrapCleanup is created, and that is a decision rather than an omission: a
 * cleanup stack is needed only by code that leaves or pushes onto it, and rust_work is
 * Rust — it cannot leave, and the allocator it reaches is the non-leaving User::Alloc.
 * Creating one would allocate on a heap this thread is about to discard.
 */
TInt WorkerMain(TAny* aPtr)
    {
    TJob* job = static_cast<TJob*>(aPtr);

    const TInt result =
        rust_work(job->iOpcode, job->iIn, job->iInLen, job->iOut, job->iOutLen);

    /* The completion lands in the GUI thread's active scheduler like any other, which is
     * why there is nothing to poll.
     *
     * Opened here rather than handed over as a handle: a handle belongs to the thread
     * that made it, and getting one across thread boundaries is exactly where the crash
     * was. Opening by id from this side cannot be got subtly wrong.
     *
     * If Open fails the GUI thread waits forever, which is a hang rather than a crash and
     * is the better of the two failures — but there is no recovery available from here,
     * and Open on a live thread in the same process does not realistically fail. */
    RThread gui;
    if (gui.Open(job->iGuiId) == KErrNone)
        {
        gui.RequestComplete(job->iStatus, result);
        gui.Close();
        }
    return KErrNone;
    }

class CShimWorker : public CActive
    {
public:
    static CShimWorker* NewL();
    ~CShimWorker();

    TInt Submit(TInt aOpcode, const TUint8* aIn, TInt aInLen, TUint8* aOut, TInt aOutLen);

private:
    CShimWorker();
    void ConstructL();
    void RunL();
    void DoCancel();

    RThread iThread;
    TBool iThreadOpen;
    /* Bumped per job so the thread name is unique. Symbian thread names must be unique
     * within a process, and a name reused before the kernel has finished reaping the
     * previous thread comes back KErrAlreadyExists — which for a probe that runs the same
     * test repeatedly would be an intermittent failure with no obvious cause. */
    TInt iSeq;
    };

CShimWorker* gWorker = NULL;

CShimWorker::CShimWorker() : CActive(EPriorityStandard), iThreadOpen(EFalse), iSeq(0)
    {
    }

void CShimWorker::ConstructL()
    {
    CActiveScheduler::Add(this);
    }

CShimWorker* CShimWorker::NewL()
    {
    CShimWorker* self = new (ELeave) CShimWorker;
    CleanupStack::PushL(self);
    self->ConstructL();
    CleanupStack::Pop(self);
    return self;
    }

CShimWorker::~CShimWorker()
    {
    Cancel();
    if (iThreadOpen)
        {
        iThread.Close();
        iThreadOpen = EFalse;
        }
    }

TInt CShimWorker::Submit(TInt aOpcode, const TUint8* aIn, TInt aInLen, TUint8* aOut,
                         TInt aOutLen)
    {
    if (IsActive())
        return SHIM_ERR_IN_USE;

    /* A previous thread's handle, if any. The thread has exited by now — IsActive is
     * false, which means its RequestComplete already landed — so closing is safe. */
    if (iThreadOpen)
        {
        iThread.Close();
        iThreadOpen = EFalse;
        }

    gJob.iOpcode = aOpcode;
    gJob.iIn = aIn;
    gJob.iInLen = aInLen;
    gJob.iOut = aOut;
    gJob.iOutLen = aOutLen;
    gJob.iStatus = &iStatus;
    /* Our own id, for the worker to open. RThread() is the current-thread pseudo-handle,
     * which is meaningless in another thread — the id is not. */
    gJob.iGuiId = RThread().Id();

    iStatus = KRequestPending;
    SetActive();

    /* 16 KB of stack: the deepest thing that will run here is a bignum exponentiation,
     * whose working set is a few hundred bytes of fixed arrays, and Symbian's default
     * of 8 KB leaves less headroom than a recursive Rust helper might want.
     *
     * The 6-argument overload gives the thread its own heap; see the note at the top on
     * why the shared-allocator overload is not used. 4 KB minimum, 256 KB maximum — a
     * job that wants more than that is the wrong shape for this facility. */
    iSeq++;
    TName name;
    name.Format(_L("shim_worker_%d"), iSeq);
    const TInt created =
        iThread.Create(name, WorkerMain, 16 * 1024, 4 * 1024, 256 * 1024, &gJob);
    if (created != KErrNone)
        {
        /* Undo the SetActive, or the scheduler waits on a request nothing will ever
         * complete and the app hangs at exit. */
        TRequestStatus* s = &iStatus;
        User::RequestComplete(s, created);
        Cancel();
        return created;
        }
    iThreadOpen = ETrue;

    /* Resumed after the handle is recorded, so a job that finishes instantly cannot
     * complete before iThreadOpen is true. */
    iThread.Resume();
    return SHIM_OK;
    }

void CShimWorker::RunL()
    {
    ShimPushSimple(SHIM_EV_WORK_DONE, 0, iStatus.Int(), 0);
    }

void CShimWorker::DoCancel()
    {
    /* A computation cannot be interrupted, so this waits. Only teardown cancels, and
     * completing the status early would let the caller free the job's buffers while the
     * worker is still writing into them.
     *
     * ExitType is checked first: Logon on a thread that has already died completes
     * immediately with KErrDied on some builds and is an error on others, and either
     * way there is nothing to wait for. */
    if (iThreadOpen && iThread.ExitType() == EExitPending)
        {
        TRequestStatus death;
        iThread.Logon(death);
        User::WaitForRequest(death);
        }

    /* The worker completes iStatus itself, so by here it is usually already done. If
     * the thread died without completing — killed, or a panic inside the job — the
     * status has to be completed by hand or Cancel() waits forever. */
    if (iStatus.Int() == KRequestPending)
        {
        TRequestStatus* s = &iStatus;
        User::RequestComplete(s, KErrDied);
        }
    }

} /* namespace */

void ShimWorkCleanup()
    {
    delete gWorker;
    gWorker = NULL;
    }

extern "C" {

int32_t shim_work_submit(int32_t opcode, const uint8_t* in, int32_t in_len,
                         uint8_t* out, int32_t out_len)
    {
    if (in_len < 0 || out_len < 0)
        return SHIM_ERR_ARGUMENT;
    /* Null with a zero length is legal — a job may take no input or produce no output —
     * but null with a length is a caller bug worth reporting rather than crashing on. */
    if ((!in && in_len > 0) || (!out && out_len > 0))
        return SHIM_ERR_ARGUMENT;

    if (!gWorker)
        {
        TInt err = KErrNone;
        TRAP(err, gWorker = CShimWorker::NewL());
        if (err != KErrNone)
            return err;
        }
    return gWorker->Submit(opcode, in, in_len, out, out_len);
    }

int32_t shim_work_busy(void)
    {
    return (gWorker && gWorker->IsActive()) ? 1 : 0;
    }

} /* extern "C" */
