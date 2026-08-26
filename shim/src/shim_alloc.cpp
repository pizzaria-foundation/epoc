/* The heap, for Rust's GlobalAlloc.
 *
 * Every function here is deliberately incapable of leaving, which is why none of
 * them TRAP. That is the whole point: `User::Alloc` returns NULL on failure while
 * `User::AllocL` *leaves*, and on Symbian 9.x a leave is a C++ throw. A throw
 * crossing a Rust frame compiled `panic=abort` — no landing pads, no unwind tables
 * — skips every Drop and is undefined behaviour, not merely a leak. So the Rust
 * allocator must see a null pointer, never an exception.
 */

#include "symbian_shim.h"

#include <e32std.h>
#include <e32debug.h>   /* RDebug */
#include <f32file.h>

extern "C" {

/* ------------------------------------------------------ the panic breadcrumb --
 *
 * Where a Rust panic went, written to disk before the process dies.
 *
 * The panic handler already knows the file and line — Rust hands them over — and
 * `User::Panic` carries both, as a 16-character category and a number. On a development
 * handset that is sometimes shown and sometimes is not, and when it is not there is
 * nothing left: no debugger, no console, no log. A reproducible crash then costs a round
 * of "what did the screen say" per attempt.
 *
 * So the location is written to a fixed path first. One line, appended, so a second crash
 * does not erase the first — the sequence matters when a panic is a consequence of an
 * earlier one.
 *
 * WHY IT IS SAFE TO DO HERE
 *
 * It must not itself panic or leave, and it must not recurse. `RFs`/`RFile` return error
 * codes rather than leaving, so no TRAP is needed; `gPanicking` stops a fault inside this
 * function from re-entering it; and a *fresh* RFs session is used rather than the shim's,
 * because the shim's may be exactly what is broken. Every error is ignored — a breadcrumb
 * that cannot be written must not become the crash it was meant to explain.
 *
 * C:\Data\ and not the private directory: this file exists to be carried off the phone,
 * and File Manager cannot see into the data cage. That needs WriteUserData; an app without
 * it simply gets no breadcrumb, which is why every error here is ignored rather than
 * reported. */
static TBool gPanicking = EFalse;

static void WritePanicBreadcrumb(const uint8_t* file, uint32_t file_len, uint32_t line)
    {
    if (gPanicking)
        return;
    gPanicking = ETrue;

    RFs fs;
    if (fs.Connect() != KErrNone)
        return;

    _LIT(KPath, "C:\\Data\\panic.txt");
    RFile f;
    TInt err = f.Open(fs, KPath, EFileWrite | EFileShareAny);
    if (err == KErrNone)
        {
        TInt size = 0;
        if (f.Size(size) == KErrNone)
            f.Seek(ESeekEnd, size);
        }
    else
        {
        err = f.Replace(fs, KPath, EFileWrite | EFileShareAny);
        }

    if (err == KErrNone)
        {
        TBuf8<128> line8;
        line8.Append(_L8("panic "));
        if (file && file_len)
            {
            /* The tail: "…/src/conv.rs" identifies the file where a long absolute path
             * identifies nothing, and 64 characters is more than any of ours needs. */
            TPtrC8 raw(file, static_cast<TInt>(file_len));
            const TInt keep = Min(raw.Length(), 64);
            line8.Append(raw.Right(keep));
            }
        else
            {
            line8.Append(_L8("<unknown>"));
            }
        line8.Append(_L8(":"));
        line8.AppendNum(static_cast<TInt>(line));
        line8.Append(_L8("\r\n"));
        f.Write(line8);
        f.Flush();
        }
    f.Close();
    fs.Close();
    }

void* shim_alloc(uint32_t size)
    {
    /* Alloc, never AllocL. */
    return User::Alloc(static_cast<TInt>(size));
    }

void* shim_realloc(void* p, uint32_t size)
    {
    /* ReAlloc may move the cell and returns NULL on failure, leaving the original
     * allocation intact — which is exactly the contract Rust's realloc expects. */
    return User::ReAlloc(p, static_cast<TInt>(size));
    }

void shim_free(void* p)
    {
    /* Free(NULL) is defined and harmless on Symbian, so no guard. */
    User::Free(p);
    }

uint32_t shim_alloc_len(const void* p)
    {
    if (!p)
        return 0;
    return static_cast<uint32_t>(User::AllocLen(p));
    }

/* Overwrite a file with one short string, from any thread.
 *
 * The primitive behind the stage breadcrumbs in shim_tls.cpp, shim_work.cpp and the DOM bridge —
 * each of which had its own copy of this, because each discovered separately that a hang leaves
 * nothing to look at unless the last step reached was written down. Its own RFs per call: a file
 * server session belongs to the thread that opened it, and these are called from worker threads.
 *
 * Failure is silent. A diagnostic that can itself fail the operation it is diagnosing is worse than
 * no diagnostic. */
int32_t shim_stage_write(const char* path, const char* text)
    {
    if (!path || !text)
        return KErrArgument;
    RFs fs;
    if (fs.Connect() != KErrNone)
        return KErrCouldNotConnect;
    TPtrC8 p8(reinterpret_cast<const TUint8*>(path), User::StringLength(reinterpret_cast<const TUint8*>(path)));
    TBuf<128> wide;
    if (p8.Length() > wide.MaxLength())
        {
        fs.Close();
        return KErrOverflow;
        }
    wide.Copy(p8);
    RFile f;
    TInt err = f.Replace(fs, wide, EFileWrite);
    if (err == KErrNone)
        {
        TPtrC8 t(reinterpret_cast<const TUint8*>(text), User::StringLength(reinterpret_cast<const TUint8*>(text)));
        f.Write(t);
        f.Close();
        }
    fs.Close();
    return err;
    }

void shim_panic(const uint8_t* file, uint32_t file_len, uint32_t line)
    {
    /* Write it down before dying. `User::Panic` carries the same two facts, and on a
     * handset that does not display them they are lost — see WritePanicBreadcrumb. */
    WritePanicBreadcrumb(file, file_len, line);

    /* Terminal, and it must stay terminal: Rust's panic handler is `-> !`, so
     * returning would be undefined behaviour on the Rust side.
     *
     * The category is capped at 16 characters because that is what User::Panic
     * accepts; the file name is more useful truncated than absent, and the line
     * number carries the precision anyway. */
    TBuf<16> category;
    if (file && file_len)
        {
        TPtrC8 raw(file, static_cast<TInt>(file_len));
        /* Keep the tail: "…/src/conv.rs" identifies the file, "/home/joshua/C…"
         * identifies nothing. */
        TInt keep = Min(raw.Length(), category.MaxLength());
        TPtrC8 tail(raw.Right(keep));
        category.Copy(tail);
        }
    else
        {
        category.Copy(_L8("rust"));
        }
    User::Panic(category, static_cast<TInt>(line));
    }

void shim_debug(const uint16_t* text, int32_t len)
    {
    if (!text || len <= 0)
        return;
    /* RDebug::Print goes nowhere on a retail handset, but it is free to leave in
     * and it is the only channel available under a debugger or in an emulator. */
    TPtrC16 s(reinterpret_cast<const TUint16*>(text), len);
    RDebug::Print(_L("%S"), &s);
    }


/* ---------------------------------------------------------------- libopus -- */
/* The C library allocation names, for vendored C that expects a hosted environment.
 *
 * libopus is decode-only and configured with VAR_ARRAYS, so most of its scratch space is
 * on the stack — but a measured link of the decode path (see docs/device-notes.md) still
 * leaves `malloc` and `free` referenced, so they have to exist.
 *
 * Pointing them at the same User::Alloc heap as everything else is the point. A second
 * allocator would make the handset's memory figures meaningless, and a decode running on
 * the worker thread would be allocating from somewhere the GUI thread cannot account for.
 * Note User::Alloc is per-thread, which is the same constraint shim_work.cpp already
 * documents: memory allocated on the worker must be freed on the worker.
 *
 * `calloc`, `realloc` and `strdup` are here for the same reason, and they were added the
 * hard way — this comment used to say `calloc` was absent because nothing referenced it,
 * and then the NetSurf libraries arrived. libcss calls `calloc` sixteen times and libdom
 * eight; libcss calls `strdup` in `stylesheet.c:205` and frees the result with `free`.
 *
 * Every one of those was being satisfied by Open C's `libc.dso`, which is the **process**
 * heap, while the matching `free` came here to the *thread's*. That is a cross-allocator
 * free, and it is silent: nothing fails at the call, the heap is simply wrong afterwards.
 * On the handset it presented as the worker thread dying without completing, which read as
 * a layout bug three steps from its cause.
 *
 * The rule the near-miss argues for: this set is not a list of symbols that happened to be
 * referenced, it is **every function that hands out memory something else here will free**.
 * Splitting it is the bug. `iconv_open`/`iconv_close` are deliberately not here — they
 * allocate and free symmetrically inside Open C and never pass a pointer across. */
/* The heap the C world allocates on, and why it is not the calling thread's.
 *
 * `User::Alloc` is per-thread, which is right for Rust here — the shim's own rule is that memory
 * allocated on the worker is freed on the worker, and `shim_work.cpp` gives the worker its own heap
 * precisely so two threads cannot race one.
 *
 * The NetSurf libraries break that rule structurally, not accidentally. libwapcaplet keeps a
 * process-global table of interned strings with **no teardown function**, and libcss interns computed
 * styles in a global arena. Both outlive any one call. And `shim_work.cpp` creates **one thread per
 * job**, so a document parsed on the worker allocates that global state on a heap that is destroyed
 * when the job ends — leaving the table pointing into freed memory for the next job to find.
 *
 * Measured, in this order: twelve documents parsed on the main thread, twelve times. The same twelve
 * on the worker: the first one never came back, and the breadcrumb said
 * `dom_hubbub_parser_create`.
 *
 * So the C libraries get a heap of their own that belongs to no thread. `aSingleThread = EFalse`
 * makes it mutex-protected, which costs a lock per allocation and buys the only arrangement in which
 * a process-global table and a per-job thread can both be correct.
 *
 * Created on first use rather than at startup: `elf2e32` sets `KImageNoCallEntryPoint`, so no static
 * constructor runs, and an app that never parses a document should not carry the chunk. */
const TInt KCHeapMin = 64 * 1024;
const TInt KCHeapMax = 16 * 1024 * 1024;

RHeap* gCHeap = NULL;

static RAllocator* CHeap()
    {
    if (!gCHeap)
        {
        /* Unnamed: a named chunk is global to the device and a second instance of this application
         * would fail to create it. */
        gCHeap = UserHeap::ChunkHeap(NULL, KCHeapMin, KCHeapMax, 4 * 1024, 4, EFalse);
        }
    /* A NULL heap here means the chunk could not be created, and every allocation below then answers
     * NULL — which the libraries treat as out of memory and report. That is the honest failure; the
     * alternative is falling back to User::Alloc and reintroducing the bug this exists to fix, on a
     * path nobody would look at again. */
    return gCHeap;
    }

/* What the C libraries' heap costs, in bytes.
 *
 * `size` is how much the chunk has committed — what the process is holding from the system — and
 * `allocated` is how much of that is live. The two answer different questions and the gap between
 * them is the interesting number:
 *
 *   - allocated grows and stays  -> something is not being freed
 *   - allocated returns, size stays -> nothing leaks; the chunk is holding its high-water mark
 *
 * Worth a function of its own because `User::AllocSize` reads the *calling thread's* allocator,
 * which is the process heap — the five NetSurf libraries allocate here instead, so every reading
 * this project had was blind to exactly the code most likely to be growing.
 *
 * Zero for both when the heap has never been created, which is a real answer: nothing has parsed. */
extern "C" void shim_cheap_stats(int32_t* size, int32_t* allocated)
    {
    if (size)
        *size = gCHeap ? (int32_t) gCHeap->Size() : 0;
    if (allocated)
        {
        TInt total = 0;
        if (gCHeap)
            (void) gCHeap->AllocSize(total);
        *allocated = (int32_t) total;
        }
    }

/* Hand back to the system whatever the C heap is holding and not using.
 *
 * `RHeap::Compress` releases the free space above the last live cell. It cannot move anything, so a
 * single live cell near the top pins everything below it — which is why this reports what it
 * actually recovered rather than claiming success. */
extern "C" int32_t shim_cheap_compress(void)
    {
    if (!gCHeap)
        return 0;
    const TInt before = gCHeap->Size();
    gCHeap->Compress();
    const TInt after = gCHeap->Size();
    return (int32_t)(before - after);
    }

void* malloc(unsigned int size)
    {
    RAllocator* h = CHeap();
    return h ? h->Alloc(static_cast<TInt>(size ? size : 1)) : NULL;
    }

/* Zeroing, with the overflow check that is the only reason `calloc` is a separate function: a
 * product that wraps would allocate a small cell and return it as a large one. libcss's counts come
 * out of a stylesheet, which is attacker-controlled. */
void* calloc(unsigned int n, unsigned int size)
    {
    if (n != 0 && size > 0xFFFFFFFFu / n)
        return NULL;
    unsigned int total = n * size;
    if (total == 0)
        total = 1;
    RAllocator* h = CHeap();
    void* p = h ? h->Alloc(static_cast<TInt>(total)) : NULL;
    if (p)
        Mem::FillZ(p, static_cast<TInt>(total));
    return p;
    }

/* The two cases C requires that `User::ReAlloc` does not do: NULL means allocate, and zero means
 * free and return NULL. Getting the second wrong leaks on every shrink-to-nothing, and libcss
 * shrinks. */
void* realloc(void* p, unsigned int size)
    {
    RAllocator* h = CHeap();
    if (!h)
        return NULL;
    if (!p)
        return h->Alloc(static_cast<TInt>(size ? size : 1));
    if (size == 0)
        {
        h->Free(p);
        return NULL;
        }
    return h->ReAlloc(p, static_cast<TInt>(size));
    }

char* strdup(const char* s)
    {
    if (!s)
        return NULL;
    RAllocator* h = CHeap();
    if (!h)
        return NULL;
    TInt n = User::StringLength(reinterpret_cast<const TUint8*>(s)) + 1;
    char* out = static_cast<char*>(h->Alloc(n));
    if (out)
        Mem::Copy(out, s, n);
    return out;
    }

void free(void* p)
    {
    /* The C heap, matching every allocator above. `RAllocator::Free(NULL)` is defined and harmless.
     *
     * If the chunk was never created there is nothing that could have been allocated, so a pointer
     * arriving here would be from somewhere else entirely — and freeing it on the calling thread's
     * heap is the cross-allocator free this whole arrangement exists to prevent. Dropping it leaks;
     * that is the lesser wrong, and it cannot happen unless `malloc` already returned NULL. */
    if (gCHeap && p)
        gCHeap->Free(p);
    }

} /* extern "C" */
