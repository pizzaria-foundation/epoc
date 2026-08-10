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
 * `calloc` is not defined, because the measured link does not reference it. If it ever
 * appears, it should be added here rather than papered over with a linker flag — a
 * missing symbol names its caller, and a silently satisfied one does not. */
void* malloc(unsigned int size)
    {
    return User::Alloc(static_cast<TInt>(size));
    }

void free(void* p)
    {
    User::Free(p);
    }

} /* extern "C" */
