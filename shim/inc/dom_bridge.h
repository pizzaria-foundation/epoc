/* The DOM bridge: HTML in, a styled tree out, with the NetSurf libraries staying on this side.
 *
 * WHY THERE IS A C LAYER AT ALL
 *
 * libdom's public API is not callable from anything but C. Every accessor is a `static inline`
 * dispatching through a per-node vtable, wrapped in a macro of the same name — `nm libdom.a` has no
 * `dom_node_get_first_child` to link against, only the underscore-prefixed default implementations,
 * and calling those directly would bypass the dispatch for exactly the HTML subclasses a browser
 * cares about. libcss is worse in one way and better in another: its `css_computed_*` accessors are
 * real symbols, but selecting a style needs a handler of 36 function pointers, each answering a
 * question about a node — which is 36 DOM queries.
 *
 * So the whole walk happens here and the result crosses once, as bytes. Rust never holds a
 * `dom_node *`, never touches a reference count, and never links a macro.
 *
 * THE TWO HALVES OF THIS DIRECTORY
 *
 * `dom_bridge.c`  — parse, walk, map computed styles onto our own style struct, emit the buffer.
 * `css_select.c`  — the `css_select_handler`, the UA stylesheet, and the select context.
 *
 * They meet only at the three functions in the "select" section below. That boundary is deliberate:
 * the handler is 36 mechanical DOM queries and the walk is one algorithm, and neither should have to
 * be read to change the other.
 */

#ifndef DOM_BRIDGE_H
#define DOM_BRIDGE_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ------------------------------------------------------------------ the Rust-facing ABI --
 *
 * One call. `html` is the document as it came off the wire, `out` a buffer the caller owns, and the
 * result is the byte count written or a negative error.
 *
 * The buffer is the caller's for the same reason the worker's output is: this runs on the worker
 * thread, whose heap is its own, and anything allocated here and freed on the GUI thread would be a
 * cross-heap free — silent corruption rather than a clean failure.
 *
 * The format is `crates/symbian-layout/src/wire.rs`, which validates every field on the way in. A
 * mistake on this side arrives there as bytes, so it is checked rather than trusted.
 */

/* Colours the document does not choose for itself, so this side does not have to know a theme.
 * 0xAARRGGBB, matching `symbian_gfx::Color`. */
typedef struct dom_palette {
	uint32_t text;
	uint32_t dim;
	uint32_t link;
} dom_palette;

/* Parse and select, writing the styled tree into `out`.
 *
 * Returns the number of bytes written, or one of DOM_ERR_* below. A page too large for `out` is
 * refused rather than truncated: the format's header carries its own length, so a prefix decodes as
 * nothing and "nothing" reported as success is the worse of the two failures. */
int32_t dom_build(const uint8_t *html, int32_t html_len, int32_t width,
		const dom_palette *palette, uint8_t *out, int32_t out_cap);

#define DOM_ERR_ARGUMENT   (-1)  /* a null pointer or a non-positive length */
#define DOM_ERR_NO_MEMORY  (-2)
#define DOM_ERR_PARSE      (-3)  /* libhubbub or libdom refused the document */
#define DOM_ERR_CSS        (-4)  /* the select context could not be built */
#define DOM_ERR_OVERFLOW   (-5)  /* the tree does not fit in `out` */
#define DOM_ERR_INTERNAL   (-6)

/* Write `tag` to `C:\\Data\\domstage.txt`, replacing what was there.
 *
 * The breadcrumb, callable from both sides of this boundary. It exists because the worker thread's
 * own stages localised a death only as far as "inside rust_work", and that span contains a Rust
 * allocation, this whole file, and a layout — three candidates and three device round trips to
 * separate them by elimination. Three were spent that way.
 *
 * Its own RFs session per call, so it is safe from any thread: a file server session belongs to the
 * thread that opened it. Diagnostic, and cheap enough to leave in — one page is hundreds of
 * milliseconds and this is one file write. */
void dom_stage(const char *tag);

/* Run one primitive on the calling thread and report whether it survived.
 *
 * Written because the bridge parses all twelve probe documents on the GUI thread and none of them on
 * a worker thread, and four explanations for that were wrong in a row (a heap race, a cross-allocator
 * free, recursion depth, a per-thread heap). Each cost a device round trip and none of them was it.
 *
 * So instead of a fifth explanation, one call per layer, smallest first. `step` selects the layer;
 * the return is 0 if it completed and DOM_ERR_INTERNAL if it reported failure. A step that kills the
 * thread returns nothing at all, and the breadcrumb it left in `C:\Data\domstage.txt` names it —
 * which is the actual answer being bought here.
 *
 * The order is deliberate. Steps 0-2 are the C runtime with no NetSurf code involved, and they carry
 * a hypothesis: a thread created by a raw `RThread::Create` never went through Open C's own thread
 * setup, so it has no per-thread libc context. If step 1 or 2 dies on a worker and lives on the GUI
 * thread, that is the whole diagnosis and the fix is the threading model, not the bridge. */
#define DOM_SELF_MALLOC   0  /* malloc/free — proves the allocator, which Rust jobs already use */
#define DOM_SELF_SNPRINTF 1  /* snprintf — libc with per-thread state behind it */
#define DOM_SELF_STRTOD   2  /* strtod — locale, which is per-thread in Open C */
#define DOM_SELF_LWC      3  /* lwc_intern_string — libwapcaplet's process-global intern table */
#define DOM_SELF_PARTS    4  /* the inside of hubbub_parser_create, one call at a time — see below */
#define DOM_SELF_HUBBUB   5  /* hubbub_parser_create — the tokeniser alone, no DOM */
#define DOM_SELF_DOM      6  /* dom_hubbub_parser_create — where the worker actually dies */
/* The inside of hubbub_parser_create, one call at a time, in one job.
 *
 * The first bisect landed on `hubbub_parser_create`: everything below it — malloc, snprintf, strtod,
 * lwc_intern_string — runs on the worker, and it does not. This step walks what that function calls,
 * writing a breadcrumb before each, so the thread dying leaves the exact call behind. One job rather
 * than five, because each device round trip costs a Bluetooth push of nearly a megabyte.
 *
 * The suspect it was written for: `parserutils_inputstream_create` builds an input filter, the
 * filter is iconv, and Symbian's iconv sits on `charconv` — a converter that owns a file server
 * session, which belongs to the thread that opened it. That is the shape of a call that works on one
 * thread and not another, and it is the layer the first bisect stepped straight over. */
#define DOM_SELF_STEPS    7

int32_t dom_selftest(int32_t step);

/* ---------------------------------------------------------------------------- the select --
 *
 * The contract between the walk and the handler. Opaque on purpose: the walk must not need to know
 * what a `css_select_ctx` is, and the handler must not need to know the wire format.
 */

struct dom_select;
struct dom_node;

/* Build a select context with the UA stylesheet, plus any author sheets found in the document.
 *
 * `doc` is a `dom_document *`, passed as void so this header stays includable without libdom's. The
 * author sheets are the `<style>` blocks; linked ones are not fetched — that needs a second request
 * per sheet, which is a decision for whoever wires it, not for this layer. */
struct dom_select *dom_select_create(void *doc);
void dom_select_destroy(struct dom_select *sel);

/* The computed style for one element, or NULL.
 *
 * The result is owned by `sel` and valid until the next call for the same node, so the caller reads
 * what it needs and does not keep it. That is the cheap contract; the alternative is a
 * `css_select_results *` per node held across a whole document, which on this handset is the
 * allocation that would not fit. */
void *dom_select_style(struct dom_select *sel, void *element);

#ifdef __cplusplus
}
#endif

#endif /* DOM_BRIDGE_H */
