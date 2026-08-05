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

extern "C" {

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

} /* extern "C" */
