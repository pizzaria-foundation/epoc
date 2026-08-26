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
#include <f32file.h>

namespace {

/* Overwrite C:\Data\workstage.txt with the last stage reached.
 *
 * The same breadcrumb shim_tls.cpp uses, and here for the same reason: a job that is submitted and
 * never completes leaves nothing to look at. Measured on the E72 under a headless daemon, a
 * do-nothing job never came back — and "never came back" is one word for at least five different
 * failures: the thread was not created, it was created and never ran, rust_work was not reached,
 * the completion was not posted, or the active object's RunL was never dispatched. Guessing between
 * them costs a device round trip each.
 *
 * Its own RFs per call, so it is safe from the worker thread as well as from the main one — a file
 * server session belongs to the thread that opened it. Diagnostic only, and cheap enough to leave
 * in: one job is hundreds of milliseconds. */
void Stage(const char* aTag)
    {
    RFs fs;
    if (fs.Connect() != KErrNone)
        return;
    _LIT(KPath, "C:\\Data\\workstage.txt");
    RFile f;
    if (f.Replace(fs, KPath, EFileWrite) == KErrNone)
        {
        TPtrC8 t((const TUint8*)aTag, User::StringLength((const TUint8*)aTag));
        f.Write(t);
        f.Close();
        }
    fs.Close();
    }

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
 * This used to say that no CTrapCleanup was created, and that it was a decision rather than an
 * omission: a cleanup stack is needed only by code that leaves or pushes onto it, and rust_work is
 * Rust — it cannot leave, and the allocator it reaches is the non-leaving User::Alloc.
 *
 * That reasoning was correct about Rust and wrong about the facility. A job is whatever the caller
 * passes, and the first job that was not pure Rust died on its first platform allocation. See the
 * note on the cleanup stack below for what it cost to find out.
 */
TInt WorkerMain(TAny* aPtr)
    {
    Stage("worker_entry");
    TJob* job = static_cast<TJob*>(aPtr);

    /* A cleanup stack, because a thread created here does not get one.
     *
     * The process main thread has a CTrapCleanup installed before main runs; a thread from
     * RThread::Create has nothing. Any `new (ELeave)` or `CleanupStack::PushL` on this thread then
     * panics it immediately — the thread dies, and since a dying thread completes no request, the
     * GUI side sees a job that never answered.
     *
     * That was not a theoretical worry. Measured by `apps/domprobe`, walking one call at a time:
     * malloc, snprintf, strtod and lwc_intern_string all ran here, and `iconv_open` killed the
     * thread — which is `charconv` underneath, Symbian C++ allocating through the cleanup stack.
     * libhubbub reaches it via libparserutils' input filter, so the whole HTML parser was
     * unreachable from a worker for want of these three lines.
     *
     * Rust jobs never needed it, which is why it was missing for so long: Rust allocates through
     * User::Alloc and touches no CBase class. The moment a job calls into platform C++ — and the
     * NetSurf libraries do, indirectly — it is mandatory.
     *
     * If it cannot be created there is no point running: the job would panic on its first PushL. */
    CTrapCleanup* cleanup = CTrapCleanup::New();
    if (cleanup == NULL)
        {
        Stage("no_cleanup_stack");
        RThread gui;
        if (gui.Open(job->iGuiId) == KErrNone)
            {
            gui.RequestComplete(job->iStatus, KErrNoMemory);
            gui.Close();
            }
        return KErrNoMemory;
        }

    /* Inside a TRAP, and this is the line that made the HTML parser reachable from a worker.
     *
     * A CTrapCleanup is not enough. `CleanupStack::PushL` needs a *frame* on the cleanup stack, and
     * frames are created by TRAP — with a cleanup stack but no TRAP entered, the first PushL panics
     * `E32USER-CBase 66`. On the GUI thread everything runs inside the app framework's outer TRAP,
     * so platform C++ finds a frame there and nothing looks wrong. Here there was none.
     *
     * That is why `iconv_open` — reached by libhubbub through libparserutils' input filter, which is
     * charconv underneath — killed this thread while malloc, snprintf, strtod and lwc_intern_string
     * all ran fine: they are the calls that never touch the cleanup stack.
     *
     * Measured, and by accident: the probe that was meant to test whether a bare PushL works on this
     * thread wrapped it in a TRAP, and passed. The TRAP was the difference, not the push.
     *
     * A leave is reported as the job's result. rust_work itself cannot leave — it is Rust — but
     * anything it calls into on the platform can, and before this a leave from that code would have
     * unwound past the top of the thread. */
    Stage("pre_rust_work");
    TInt result = KErrNone;
    TInt leave = KErrNone;
    TRAP(leave, result = rust_work(job->iOpcode, job->iIn, job->iInLen, job->iOut, job->iOutLen));
    if (leave != KErrNone)
        {
        Stage("rust_work_left");
        result = leave;
        }
    Stage("post_rust_work");

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
    const TInt opened = gui.Open(job->iGuiId);
    if (opened == KErrNone)
        {
        gui.RequestComplete(job->iStatus, result);
        gui.Close();
        Stage("completed");
        }
    else
        {
        Stage("gui_open_failed");
        }

    /* After the completion, not before: nothing between here and there allocates, and deleting it
     * first would leave the window in which a panic has no handler. */
    delete cleanup;
    return KErrNone;
    }

class CShimWorker : public CActive
    {
public:
    static CShimWorker* NewL();
    ~CShimWorker();

    TInt Submit(TInt aOpcode, const TUint8* aIn, TInt aInLen, TUint8* aOut, TInt aOutLen,
                TInt aHeapMax, TInt aStack);
    TInt ExitInfo(int32_t* aType, int32_t* aReason, TUint8* aCat, TInt aCatCap);

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
                         TInt aOutLen, TInt aHeapMax, TInt aStack)
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

    Stage("submit");
    iStatus = KRequestPending;
    SetActive();

    /* 64 KB of stack, and a heap ceiling the CALLER chooses.
     *
     * Both numbers were sized for one job and are wrong for the next. The stack was 16 KB because
     * "the deepest thing that will run here is a bignum exponentiation", whose working set is a few
     * hundred bytes of fixed arrays. The heap ceiling was 256 KB with the comment "a job that wants
     * more than that is the wrong shape for this facility" — which was true while the only jobs were
     * a modular exponentiation and a key derivation.
     *
     * Page layout is the job that breaks both. An HTML tokenizer and a DOM tree are recursive and
     * allocate per node: one measured page inflates to 700 KB of HTML before a tree is built from
     * it, so 256 KB is not a ceiling that job can be squeezed under — it is a refusal. And a
     * recursive descent over a document wants more than 16 KB of stack.
     *
     * So the ceiling is a parameter. It costs nothing to pass: on Symbian a thread heap is a chunk
     * reserved to its maximum and committed to its minimum, so a large ceiling reserves address
     * space and commits nothing. A crypto job asks for 256 KB and a layout job asks for megabytes,
     * and neither pays for the other's choice.
     *
     * The 6-argument overload gives the thread its own heap; see the note at the top on why the
     * shared-allocator overload is not used.
     *
     * The stack is a parameter for the same reason the heap became one, and it was added after the
     * heap: a job that builds an HTML tokeniser and a DOM is not a job whose deepest frame is a
     * bignum ladder. Unlike the heap ceiling, a thread stack is **committed** rather than reserved,
     * so asking for more is real memory and the bound above is not a formality. */
    iSeq++;
    TName name;
    name.Format(_L("shim_worker_%d"), iSeq);
    const TInt created =
        iThread.Create(name, WorkerMain, aStack, 4 * 1024, aHeapMax, &gJob);
    if (created == KErrNone)
        Stage("thread_created");
    else
        Stage("thread_create_failed");

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
    Stage("runl");
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

/* The heap ceiling a job gets when it does not say. What every existing caller was given, kept so
 * that adding the parameter changed no behaviour for them. */
const TInt KDefaultWorkerHeap = 256 * 1024;

/* The largest ceiling this will hand out. Not a guess about what a job needs — it is a bound on how
 * much address space one worker may reserve, so a wrong number from a caller is a refused job
 * instead of a process that cannot create a thread. */
const TInt KMaxWorkerHeap = 16 * 1024 * 1024;

/* The stack a job gets when it does not say. What every caller had before it was adjustable. */
const TInt KDefaultWorkerStack = 64 * 1024;

/* And the largest this will ask for.
 *
 * A thread stack is committed rather than reserved, so it is real memory per job. And the platform
 * has its own ceiling far below what looks reasonable. Measured on the E72 by `apps/domprobe`,
 * asking `RThread::Create` for a descending series:
 *
 *     128 KB, 112 KB, 96 KB, 88 KB  ->  KErrTooBig (-40)
 *     80 KB                         ->  thread created
 *
 * So the ceiling is between 80 and 88 KB, and 80 KB is what a job can actually have. Worth knowing
 * how this presents: `RThread::Create` refuses *before* the thread exists, so the caller sees a job
 * that never ran rather than a stack that was too small. Read as progress twice; it is refusal.
 *
 * The bound stays at the measured ceiling rather than a round number above it, so a caller asking
 * for more is refused here with an error that names the argument instead of at the kernel with one
 * that does not. */
const TInt KMaxWorkerStack = 80 * 1024;

TInt CShimWorker::ExitInfo(int32_t* aType, int32_t* aReason, TUint8* aCat, TInt aCatCap)
    {
    if (!iThreadOpen)
        return SHIM_ERR_NOT_READY;

    if (aType)
        *aType = iThread.ExitType();
    if (aReason)
        *aReason = iThread.ExitReason();

    /* The category is a TExitCategoryName, 16-bit like every Symbian descriptor, and the caller
     * wants bytes. Narrowed here rather than at the boundary: a category is ASCII by construction
     * ("KERN-EXEC", "E32USER-CBase"), so there is nothing to lose in the conversion and the Rust
     * side does not need to know what a TBuf is. */
    if (aCat && aCatCap > 0)
        {
        const TExitCategoryName name = iThread.ExitCategory();
        TInt n = name.Length();
        if (n > aCatCap - 1)
            n = aCatCap - 1;
        for (TInt i = 0; i < n; i++)
            aCat[i] = (TUint8) name[i];
        aCat[n] = 0;
        }
    return 0;
    }

int32_t shim_work_submit_ex(int32_t opcode, const uint8_t* in, int32_t in_len,
                            uint8_t* out, int32_t out_len, int32_t heap_max, int32_t stack)
    {
    if (in_len < 0 || out_len < 0)
        return SHIM_ERR_ARGUMENT;
    /* Null with a zero length is legal — a job may take no input or produce no output —
     * but null with a length is a caller bug worth reporting rather than crashing on. */
    if ((!in && in_len > 0) || (!out && out_len > 0))
        return SHIM_ERR_ARGUMENT;

    TInt heap = (heap_max > 0) ? heap_max : KDefaultWorkerHeap;
    if (heap < 4 * 1024 || heap > KMaxWorkerHeap)
        return SHIM_ERR_ARGUMENT;

    TInt stk = (stack > 0) ? stack : KDefaultWorkerStack;
    if (stk < 8 * 1024 || stk > KMaxWorkerStack)
        return SHIM_ERR_ARGUMENT;

    if (!gWorker)
        {
        TInt err = KErrNone;
        TRAP(err, gWorker = CShimWorker::NewL());
        if (err != KErrNone)
            return err;
        }
    return gWorker->Submit(opcode, in, in_len, out, out_len, heap, stack);
    }

/* The original form, unchanged for its callers: the crypto jobs this facility was built for. */
int32_t shim_work_submit(int32_t opcode, const uint8_t* in, int32_t in_len,
                         uint8_t* out, int32_t out_len)
    {
    return shim_work_submit_ex(opcode, in, in_len, out, out_len, KDefaultWorkerHeap,
            KDefaultWorkerStack);
    }

int32_t shim_work_busy(void)
    {
    return (gWorker && gWorker->IsActive()) ? 1 : 0;
    }

int32_t shim_cleanup_probe(void)
    {
    Stage("cleanup_probe");
    TInt err = KErrNone;
    TRAP(err, {
        /* Small and immediately discarded: the allocation is not the subject, the frame is. */
        TAny* p = User::AllocL(16);
        CleanupStack::PushL(p);
        CleanupStack::PopAndDestroy();
    });
    Stage("cleanup_probe_done");
    return err;
    }

int32_t shim_cleanup_probe_bare(void)
    {
    /* The same push with **no TRAP of its own**, which is the shape every call into platform C++ from
     * a job has. It passes only if the caller already established a frame — so on a worker it is a
     * direct test of the TRAP in WorkerMain, and it is what the version above accidentally hid by
     * supplying its own. */
    Stage("cleanup_bare");
    TAny* p = User::Alloc(16);
    if (p == NULL)
        return KErrNoMemory;
    CleanupStack::PushL(p);
    CleanupStack::PopAndDestroy();
    Stage("cleanup_bare_done");
    return 0;
    }

int32_t shim_work_exit_info(int32_t* type, int32_t* reason, uint8_t* cat, int32_t cat_cap)
    {
    if (!gWorker)
        return SHIM_ERR_NOT_READY;
    return gWorker->ExitInfo(type, reason, cat, cat_cap);
    }

} /* extern "C" */
