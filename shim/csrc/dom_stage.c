/* The shared breadcrumb. See `dom_bridge.h`.
 *
 * In C rather than C++ because both callers are on this side of the C++ boundary, and in its own file
 * because `dom_bridge.c` and `css_select.c` both want it and neither should include the other.
 *
 * It cannot use the shim's file layer: that keeps a per-process RFs session opened by the GUI thread,
 * and a file server session belongs to the thread that opened it. So this opens its own, the same way
 * `shim_tls.cpp` and `shim_work.cpp` do for the same reason.
 */

#include "dom_bridge.h"

/* Declared here rather than by including the C++ shim header, which this file must never see. */
int32_t shim_stage_write(const char *path, const char *text);

void dom_stage(const char *tag)
{
	shim_stage_write("C:\\Data\\domstage.txt", tag);
}
