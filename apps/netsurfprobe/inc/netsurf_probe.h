/* The C ABI between the probe's two halves, and the reason the probe has two halves.
 *
 * The NetSurf public headers are C99 and are not C++-safe. Two hard errors, both
 * unfixable from outside the vendored tree:
 *
 *   libdom  six headers under include/dom/events name a parameter `namespace`
 *   libcss  include/libcss/computed.h declares `const css_computed_style *restrict`
 *
 * So the split is not stylistic. netsurf_probe.c is the only file that includes them,
 * compiled by gcc as C99; netsurfprobe.cpp owns E32Main and the report and includes
 * only this header. Nothing here mentions a NetSurf type, which is what makes it safe
 * for the C++ side to read — and it is the same boundary the eventual symbian-dom
 * crate will have to draw for Rust.
 */
#ifndef NETSURF_PROBE_H
#define NETSURF_PROBE_H

#ifdef __cplusplus
extern "C" {
#endif

/* One result line.
 *
 *   section  the library this line belongs to, set only on the first line of each
 *            group and NULL otherwise, so the C++ side emits a heading by testing a
 *            pointer instead of knowing the library names.
 *   name     what was checked.
 *   verdict  1 pass, 0 fail, -1 measurement with no verdict (the `.` line shape in
 *            crates/symbian-report's grammar).
 *   detail   the library's own return code, or the number measured. Carried through
 *            so a failing line says what the library said rather than only that
 *            something went wrong — a hubbub_error of 3 is a different bug report
 *            from a NULL parser.
 *
 * Every char* points at a string literal in the C half, so it outlives the call and
 * nothing needs copying: the two halves are one image. */
typedef struct netsurf_check {
	const char *section;
	const char *name;
	int verdict;
	int detail;
} netsurf_check;

/* Run every probe, filling up to `cap` entries, and return how many were used.
 * Returns a negative value only when `cap` is too small to start, which would be a
 * bug in the caller rather than a finding about the libraries. */
int netsurf_probe_run(netsurf_check *out, int cap);

#ifdef __cplusplus
}
#endif

#endif
