/* Prefix header for the vendored NetSurf MIT libraries, force-included by
 * tools/build-netsurf after the SDK's gcce.h.
 *
 * One gap, and it is real: stdapis/stdbool.h takes the
 * `__SYMBIAN32__ && !__WINSCW__` branch on GCCE, which defines `true`, `false`
 * and __bool_true_false_are_defined but never defines `bool` — the header was
 * written when the only GCCE C compiler in view was pre-C99. Every one of the
 * five libraries spells its booleans `bool`, so without this nothing but
 * libwapcaplet compiles (measured: 28 of 30 libhubbub sources failed).
 *
 * The obvious fix — put the compiler's own include dir ahead of stdapis so
 * GCC's stdbool.h wins — does not work, and the reason is worth recording:
 * GCC's stdint.h sits in that same directory and types int32_t from
 * __INT32_TYPE__, which is `long int` for this target, while Symbian's
 * sys/stdint.h says `int`. Both are 32 bits and they are not the same type, so
 * every file that reaches both headers dies on "conflicting types for
 * 'int32_t'". Symbian's stdint.h has to win, which means Symbian's stdbool.h
 * wins too, which means the gap has to be filled here.
 */
#ifndef NETSURF_SYMBIAN_H
#define NETSURF_SYMBIAN_H

#include <stdbool.h>

#ifndef __cplusplus
#ifndef bool
#define bool _Bool
#endif
#endif

#endif
