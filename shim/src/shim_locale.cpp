/* What language is this phone set to?
 *
 * One call, and it is the whole file. `User::Language()` is declared in e32std.h as
 * `IMPORT_C static TLanguage Language()` and lives in euser.dll, which every executable
 * on this platform links whether it asks to or not.
 *
 * WHY THIS ONE IS NOT GATED
 *
 * Every optional source beside it carries a USE_* gate, and each of those gates exists for
 * the same reason: the file references a class from a library that is not in the base link
 * set, so compiling it into an application that never asks would put an unresolvable import
 * on that application — and on Symbian an import that does not resolve makes the image
 * silently never load.
 *
 * None of that applies here. euser is already linked, so there is no import to add and no
 * gate to earn. Making it opt-in would mean an application has to *remember* to be able to
 * speak the user's language, and forgetting would look like a translation bug rather than a
 * missing flag.
 *
 * NO TRAP
 *
 * `User::Language()` cannot Leave: it reads a value the locale already holds and returns it.
 * Rule 1 in symbian_shim.h asks for the exemption to be stated rather than assumed, so it is
 * stated here.
 *
 * THE VALUE IS NOT INTERPRETED HERE
 *
 * TLanguage is returned raw. The mapping from its ~160 values to the two languages we speak
 * is a table, and a table belongs in Rust where a host test can cover every entry — the same
 * argument shim_hal.cpp makes for HAL attributes and shim_skin.cpp makes for skin item IDs.
 * `symbian::locale` is where it lives, and it is worth knowing that the obvious mapping is
 * wrong: English is eight different values, and Brazilian Portuguese is 76 rather than
 * anything near Portuguese's 13.
 */

#include "shim_priv.h"

#include <e32std.h>

extern "C" {

int32_t shim_locale_language(void)
    {
    return (int32_t) User::Language();
    }

} /* extern "C" */
