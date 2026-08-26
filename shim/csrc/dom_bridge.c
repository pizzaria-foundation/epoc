/* Parse a document, ask libcss for a style per element, and emit the styled tree.
 *
 * The rationale for a C layer at all is in `dom_bridge.h`. This file is the walk; `css_select.c` is
 * the handler it asks for styles. The two meet only at the three `dom_select_*` functions.
 *
 * WHAT THIS TRANSLATES, AND WHAT IT DELIBERATELY DROPS
 *
 * libcss answers about 114 computed properties. The layout on the other side of the buffer honours
 * blocks, inline text and images in a single column, because on a 320-pixel screen a faithful CSS
 * 2.1 layout is the wrong behaviour — that is the browser's whole policy, argued in
 * `docs/plan-browser.md`. So `float`, `position`, `width`, `height`, `z-index` and the rest are read
 * and discarded here rather than being carried across a boundary to be discarded there.
 *
 * `display` is the one that must not be dropped: `display: none` is content the author removed, and
 * a browser that shows it is showing something nobody wrote.
 *
 * THE OUTPUT IS A BUFFER, NOT A TREE
 *
 * Format: `crates/symbian-layout/src/wire.rs`, which validates every index and every span on the way
 * in. That validation is not defensive tidiness — the writer is this file, so a miscount here
 * arrives there as bytes, and an index into nothing is how a tree walk loops forever.
 */

#include "dom_bridge.h"

#include <stdbool.h>
#include <stdlib.h>
#include <string.h>

#include <dom/dom.h>
#include <dom/bindings/hubbub/parser.h>
#include <libcss/libcss.h>
#include <dom/html/html_text_area_element.h>
#include <dom/html/html_select_element.h>
#include <libcss/fpmath.h>
#include <hubbub/parser.h>
#include <parserutils/charset/mibenum.h>
#include <parserutils/input/inputstream.h>
#include <iconv.h>
#include <stdio.h>

/* ------------------------------------------------------------------------- the wire format --
 *
 * Mirrors `wire.rs`. The two must agree, and the round-trip test on that side is what says so — a
 * format with one implementation has no way to be wrong out loud.
 */

#define WIRE_MAGIC_0 'S'
#define WIRE_MAGIC_1 'T'
#define WIRE_MAGIC_2 0x01
#define WIRE_MAGIC_3 0x00

#define WIRE_HEADER 16

/* One node record, written out as the sum of its fields rather than as a total.
 *
 * The total was wrong the first time — 61 against the other side's 70 — and a wrong record size does
 * not fail loudly: every node after the first is read from the wrong offset, so the tree arrives as
 * garbage that still has the right shape. Written this way the arithmetic is the same arithmetic
 * `wire.rs` does, in the same order, and the two can be compared by eye. */
#define WIRE_NODE ( \
	1 +          /* kind */ \
	4 + 4 +      /* span: offset, length */ \
	4 + 4 +      /* image: width, height */ \
	1 +          /* display */ \
	1 +          /* font role */ \
	4 +          /* colour */ \
	1 + 4 +      /* has background, background */ \
	8 +          /* margin: four int16 */ \
	8 +          /* padding: four int16 */ \
	1 + 4 + 4 +  /* marker kind, marker span */ \
	4 + 4 +      /* href span */ \
	1 +          /* rule below */ \
	1 +          /* field kind (0 = not a control) */ \
	4 + 4 +      /* field name span */ \
	2 +          /* form id */ \
	1 +          /* form method */ \
	4 + 4        /* first child, next sibling — MUST stay last: see first_child_at */ \
)

/* The other side's constant. If these ever disagree the tree decodes as garbage with the right
 * shape, which is the worst failure this file can have — so it is asserted at compile time. */
#if WIRE_NODE != 82
#error "WIRE_NODE disagrees with crates/symbian-layout/src/wire.rs"
#endif

#define KIND_ELEMENT 0
#define KIND_TEXT    1
#define KIND_IMAGE   2
#define KIND_CONTROL 3

/* Control kinds. Zero means "not a control", so these start at one and match
 * `style::FieldKind::tag` on the Rust side. */
#define FIELD_NONE     0
#define FIELD_TEXT     1
#define FIELD_PASSWORD 2
#define FIELD_BUTTON   3
#define FIELD_SUBMIT   4
#define FIELD_CHECKBOX 5
#define FIELD_RADIO    6
#define FIELD_SELECT   7
#define FIELD_TEXTAREA 8
#define FIELD_HIDDEN   9

/* No form. Matches `style::NO_FORM`. */
#define NO_FORM 0xFFFFu

#define DISPLAY_BLOCK  0
#define DISPLAY_INLINE 1
#define DISPLAY_NONE   2

/* The body atlas's height in pixels — the size every other length is judged against.
 *
 * The four roles are four fixed atlases, not a scale, so "one em" can only mean the body atlas. */
#define BODY_PX 11

#define FONT_BODY   0
#define FONT_STRONG 1
#define FONT_SMALL  2
#define FONT_TITLE  3

#define MARKER_NONE   0
#define MARKER_BULLET 1
#define MARKER_TEXT   2

#define NODE_NONE 0xFFFFFFFFu

/* A growable byte buffer. Doubling, with the caller's cap as the ceiling: a document that does not
 * fit is refused rather than grown into a heap this thread does not have. */
typedef struct buf {
	uint8_t *p;
	size_t len;
	size_t cap;
	bool failed;
} buf;

static void buf_free(buf *b)
{
	free(b->p);
	b->p = NULL;
	b->len = 0;
	b->cap = 0;
}

static bool buf_reserve(buf *b, size_t extra)
{
	if (b->failed)
		return false;
	if (b->len + extra <= b->cap)
		return true;
	size_t want = b->cap ? b->cap * 2 : 4096;
	while (want < b->len + extra)
		want *= 2;
	uint8_t *n = realloc(b->p, want);
	if (n == NULL) {
		b->failed = true;
		return false;
	}
	b->p = n;
	b->cap = want;
	return true;
}

static void put_bytes(buf *b, const void *src, size_t n)
{
	if (!buf_reserve(b, n))
		return;
	memcpy(b->p + b->len, src, n);
	b->len += n;
}

static void put_u8(buf *b, uint8_t v)
{
	put_bytes(b, &v, 1);
}

/* Node records are written through `write_le16`/`write_le32` into a reserved slot rather than
 * appended, because a parent's child index is only known after its children exist. The append-side
 * integer helpers for i32 and i16 were unused once the walk became iterative, and are gone rather
 * than left as dead code with a plausible name. */

/* ------------------------------------------------------------------------------ the style --
 *
 * One node's appearance, in the shape the wire format wants. Built per element from the computed
 * style, inheriting what CSS says is inherited — libcss has already done the cascade, so this only
 * reads.
 */

typedef struct style {
	uint8_t display;
	uint8_t font;
	uint32_t color;
	bool has_bg;
	uint32_t bg;
	int32_t margin[4];  /* left, top, right, bottom */
	int32_t padding[4];
	uint8_t marker;
	uint32_t marker_off, marker_len;
	uint32_t href_off, href_len;
	bool rule_below;
	/* Which form this node is inside, inherited so a control several elements deep still knows.
	 * Submitting means gathering every control that shares one, and two bytes a node buys that
	 * without matching up ancestors afterwards. */
	uint16_t form;
	uint8_t method;   /* 0 GET, 1 POST */
} style;

/* libcss colours are 0xAARRGGBB already, which is what `symbian_gfx::Color` is. No conversion, and
 * that is worth stating because a swapped pair of channels is the kind of bug that looks like a
 * design choice. */
static uint32_t css_color_to_argb(css_color c)
{
	return (uint32_t) c;
}

/* A CSS length in pixels, for the units a page actually uses on a screen this size.
 *
 * `em` is resolved against a fixed body size rather than the element's own computed font size: the
 * layout has four font roles, not a continuum, so an exact `em` would be precision the other side
 * cannot spend. Percentages resolve against nothing here and come back zero — a percentage margin is
 * relative to the containing block's width, which is a layout question and not a style one.
 */
static int32_t length_px(css_fixed len, css_unit unit)
{
	/* Scaled in fixed point and rounded to an integer at the end, never the other way round.
	 *
	 * This used to take FIXTOINT(len) first and multiply the whole number. Every fractional length
	 * in the UA stylesheet then collapsed before it was used: `margin: 0.67em` truncated to 0 and
	 * came out as no margin at all, `1.67em` became 1em. The spec's paragraph spacing is written in
	 * exactly those fractions, so the effect was every block sitting closer together than any
	 * stylesheet asked for. */
	switch (unit) {
	case CSS_UNIT_PX:
		return FIXTOINT(len);
	case CSS_UNIT_EM:
	case CSS_UNIT_REM:
		return FIXTOINT(FMUL(len, INTTOFIX(BODY_PX)));
	case CSS_UNIT_EX:
		return FIXTOINT(FMUL(len, INTTOFIX(6)));
	case CSS_UNIT_PT:
		return FIXTOINT(FDIV(FMUL(len, INTTOFIX(4)), INTTOFIX(3)));
	case CSS_UNIT_PC:
		return FIXTOINT(FMUL(len, INTTOFIX(16)));
	case CSS_UNIT_MM:
		return FIXTOINT(FMUL(len, INTTOFIX(4)));
	case CSS_UNIT_CM:
		return FIXTOINT(FMUL(len, INTTOFIX(38)));
	case CSS_UNIT_IN:
		return FIXTOINT(FMUL(len, INTTOFIX(96)));
	default:
		/* Percentages, viewport units, `calc`. Zero rather than a guess: a margin invented here
		 * is a page pushed around by a number nobody wrote. */
		return 0;
	}
}

static void edge(const css_computed_style *s, int out[4])
{
	css_fixed len;
	css_unit unit;
	out[0] = out[1] = out[2] = out[3] = 0;
	if (s == NULL)
		return;
	if (css_computed_margin_left(s, &len, &unit) == CSS_MARGIN_SET)
		out[0] = length_px(len, unit);
	if (css_computed_margin_top(s, &len, &unit) == CSS_MARGIN_SET)
		out[1] = length_px(len, unit);
	if (css_computed_margin_right(s, &len, &unit) == CSS_MARGIN_SET)
		out[2] = length_px(len, unit);
	if (css_computed_margin_bottom(s, &len, &unit) == CSS_MARGIN_SET)
		out[3] = length_px(len, unit);
}

static void pad(const css_computed_style *s, int out[4])
{
	css_fixed len;
	css_unit unit;
	out[0] = out[1] = out[2] = out[3] = 0;
	if (s == NULL)
		return;
	if (css_computed_padding_left(s, &len, &unit) == CSS_PADDING_SET)
		out[0] = length_px(len, unit);
	if (css_computed_padding_top(s, &len, &unit) == CSS_PADDING_SET)
		out[1] = length_px(len, unit);
	if (css_computed_padding_right(s, &len, &unit) == CSS_PADDING_SET)
		out[2] = length_px(len, unit);
	if (css_computed_padding_bottom(s, &len, &unit) == CSS_PADDING_SET)
		out[3] = length_px(len, unit);
}

/* Which of the four font roles this element draws in.
 *
 * Roles, not sizes, because the other side has four atlases and no scaler. That is also the
 * font-size floor the plan asked for, arrived at for free: a stylesheet asking for 9 px cannot get
 * something smaller than the `small` atlas, because there is nothing smaller to give it. */
static uint8_t font_role(const css_computed_style *s)
{
	if (s == NULL)
		return FONT_BODY;

	css_fixed size;
	css_unit unit;
	int px = 0;
	if (css_computed_font_size(s, &size, &unit) == CSS_FONT_SIZE_DIMENSION)
		px = length_px(size, unit);

	bool bold = false;
	switch (css_computed_font_weight(s)) {
	case CSS_FONT_WEIGHT_BOLD:
	case CSS_FONT_WEIGHT_BOLDER:
	case CSS_FONT_WEIGHT_700:
	case CSS_FONT_WEIGHT_800:
	case CSS_FONT_WEIGHT_900:
		bold = true;
		break;
	default:
		break;
	}

	/* A heading is bold and larger; body-sized bold is emphasis. The threshold is the body atlas
	 * plus a little, so a stylesheet nudging 11 px to 12 px does not turn a paragraph into a
	 * heading. */
	if (px >= BODY_PX + 4)
		return FONT_TITLE;
	if (bold)
		return FONT_STRONG;
	if (px > 0 && px <= 9)
		return FONT_SMALL;
	return FONT_BODY;
}

static uint8_t display_of(const css_computed_style *s, bool root)
{
	if (s == NULL)
		return DISPLAY_INLINE;
	switch (css_computed_display(s, root)) {
	case CSS_DISPLAY_NONE:
		return DISPLAY_NONE;
	case CSS_DISPLAY_INLINE:
		return DISPLAY_INLINE;
	default:
		/* Everything else — block, list-item, table, flex, grid, inline-block — collapses to a
		 * block. Not a shortcut: they are all ways of placing content across a width this screen
		 * does not have, and the fit-to-width policy exists to refuse them. */
		return DISPLAY_BLOCK;
	}
}

/* ------------------------------------------------------------------------------- the walk --
 *
 * Depth first, in document order, emitting nodes as they are entered. Indices are assigned in the
 * order nodes are written, so a parent's `first_child` is known only after its children exist —
 * which is why the node records are patched rather than written once. The alternative is two passes
 * over the DOM, and a second pass over a 6000-element document costs more than the patching.
 *
 * ITERATIVE, WITH AN EXPLICIT STACK
 *
 * It was recursive, and it died on the handset: the breadcrumb in `C:\Data\workstage.txt` said
 * `pre_rust_work` and stopped, meaning the worker thread entered the job and never came out. The
 * worker's stack is 64 KB and a page from a template engine is forty-odd nested `<div>`s, each frame
 * here carrying a `style` of sixty bytes plus whatever the compiler keeps.
 *
 * The scaffolding parser this replaces has a test for exactly this — `deep_nesting_is_survivable`,
 * five hundred levels — and it passes because it is not recursive. That lesson was already written
 * down in this repo and not applied on this side.
 *
 * So: an explicit stack, heap-allocated and bounded. Depth beyond the bound is truncated rather than
 * a crash, because a document nested a thousand deep is a generated document and the part a reader
 * wants is above the bound anyway.
 */

/* Deepest nesting the walk will follow. Real documents are tens deep; a thousand is a generator. */
#define MAX_DEPTH 256

/* One level of the walk. The `style` is the level's inherited style, which is why this is not just a
 * node pointer: a child's colour and href come from here. */
typedef struct level {
	dom_node *node;      /* the parent whose children this level is walking */
	dom_nodelist *kids;
	uint32_t count;
	uint32_t at;
	uint32_t self_index; /* the parent's own record, to patch its first child into */
	uint32_t first;      /* first child emitted, or NODE_NONE */
	uint32_t last;       /* last child emitted, for chaining siblings */
	style st;            /* the parent's style, inherited by its children */
} level;

typedef struct emit {
	buf nodes;   /* fixed-width node records */
	buf text;    /* the string arena */
	struct dom_select *sel;
	dom_palette pal;
	bool failed;
	/* The next form id to hand out, in source order. */
	uint16_t next_form;
} emit;

/* Intern into the arena, returning the span. Not deduplicated: a hash map per document to catch the
 * repeated `href`s in a navigation bar is worth measuring before paying for. */
static void intern(emit *e, const char *p, size_t n, uint32_t *off, uint32_t *len)
{
	*off = (uint32_t) e->text.len;
	put_bytes(&e->text, p, n);
	*len = (uint32_t) n;
}

/* Intern a dom_string, collapsing runs of whitespace to one space.
 *
 * Collapsing here rather than on the Rust side is forced, not chosen: the spans the layout emits are
 * byte ranges into this arena, and "three spaces render as one" cannot be expressed as a range over
 * text that still has three.
 */
static void intern_collapsed(emit *e, dom_string *str, uint32_t *off, uint32_t *len)
{
	*off = (uint32_t) e->text.len;
	*len = 0;
	if (str == NULL)
		return;

	const char *p = dom_string_data(str);
	size_t n = dom_string_byte_length(str);
	bool last_space = false;
	size_t written = 0;
	for (size_t i = 0; i < n; i++) {
		char c = p[i];
		bool ws = (c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\f' || c == '\v');
		if (ws) {
			if (!last_space) {
				put_u8(&e->text, ' ');
				written++;
				last_space = true;
			}
		} else {
			put_u8(&e->text, (uint8_t) c);
			written++;
			last_space = false;
		}
	}
	*len = (uint32_t) written;
}

/* Reserve a node record and return its index. Written blank and patched: see the note above. */
static uint32_t node_reserve(emit *e)
{
	uint32_t index = (uint32_t) (e->nodes.len / WIRE_NODE);
	if (!buf_reserve(&e->nodes, WIRE_NODE)) {
		e->failed = true;
		return NODE_NONE;
	}
	memset(e->nodes.p + e->nodes.len, 0, WIRE_NODE);
	e->nodes.len += WIRE_NODE;
	return index;
}

static void write_le32(uint8_t *at, uint32_t v)
{
	at[0] = (uint8_t) (v & 0xFF);
	at[1] = (uint8_t) ((v >> 8) & 0xFF);
	at[2] = (uint8_t) ((v >> 16) & 0xFF);
	at[3] = (uint8_t) ((v >> 24) & 0xFF);
}

static void write_le16(uint8_t *at, int32_t v)
{
	if (v > 32767)
		v = 32767;
	if (v < -32768)
		v = -32768;
	at[0] = (uint8_t) (v & 0xFF);
	at[1] = (uint8_t) (((uint32_t) v >> 8) & 0xFF);
}

/* Fill in a reserved record. The field order is `wire.rs`'s and nothing else may reorder it. */
static void node_write_full(emit *e, uint32_t index, uint8_t kind, uint32_t span_off,
		uint32_t span_len, int32_t w, int32_t h, const style *st, uint8_t field,
		uint32_t field_name_off, uint32_t field_name_len, uint32_t first, uint32_t next)
{
	if (index == NODE_NONE || e->failed)
		return;
	uint8_t *n = e->nodes.p + (size_t) index * WIRE_NODE;
	size_t o = 0;

	n[o++] = kind;
	write_le32(n + o, span_off); o += 4;
	write_le32(n + o, span_len); o += 4;
	write_le32(n + o, (uint32_t) w); o += 4;
	write_le32(n + o, (uint32_t) h); o += 4;

	n[o++] = st->display;
	n[o++] = st->font;
	write_le32(n + o, st->color); o += 4;
	n[o++] = st->has_bg ? 1 : 0;
	write_le32(n + o, st->bg); o += 4;

	for (int i = 0; i < 4; i++) { write_le16(n + o, st->margin[i]); o += 2; }
	for (int i = 0; i < 4; i++) { write_le16(n + o, st->padding[i]); o += 2; }

	n[o++] = st->marker;
	write_le32(n + o, st->marker_off); o += 4;
	write_le32(n + o, st->marker_len); o += 4;

	write_le32(n + o, st->href_off); o += 4;
	write_le32(n + o, st->href_len); o += 4;
	n[o++] = st->rule_below ? 1 : 0;

	n[o++] = field;
	write_le32(n + o, field_name_off); o += 4;
	write_le32(n + o, field_name_len); o += 4;
	n[o++] = (uint8_t) (st->form & 0xFF);
	n[o++] = (uint8_t) ((st->form >> 8) & 0xFF);
	n[o++] = st->method;

	/* Last, and the two patch helpers below depend on it. */
	write_le32(n + o, first); o += 4;
	write_le32(n + o, next); o += 4;
}

/* The common case: a node that is not a control.
 *
 * Kept as a wrapper so the twenty existing call sites do not each have to pass three zeros, and so
 * that adding a field to the record means touching one writer rather than twenty callers. */
static void node_write(emit *e, uint32_t index, uint8_t kind, uint32_t span_off, uint32_t span_len,
		int32_t w, int32_t h, const style *st, uint32_t first, uint32_t next)
{
	node_write_full(e, index, kind, span_off, span_len, w, h, st, FIELD_NONE, 0, 0, first, next);
}

/* Lower-case ASCII compare of a dom_string against a literal. Element names come back upper-cased
 * from the parser, so every comparison has to be case-insensitive or none of them match. */
static bool name_is(dom_string *name, const char *want)
{
	if (name == NULL)
		return false;
	const char *p = dom_string_data(name);
	size_t n = dom_string_byte_length(name);
	size_t w = strlen(want);
	if (n != w)
		return false;
	for (size_t i = 0; i < n; i++) {
		char c = p[i];
		if (c >= 'A' && c <= 'Z')
			c = (char) (c - 'A' + 'a');
		if (c != want[i])
			return false;
	}
	return true;
}

/* An attribute's value, interned. Zero length when absent. */
static void attr_interned(emit *e, dom_element *el, const char *name, uint32_t *off, uint32_t *len)
{
	*off = 0;
	*len = 0;
	dom_string *key = NULL;
	if (dom_string_create((const uint8_t *) name, strlen(name), &key) != DOM_NO_ERR)
		return;
	dom_string *value = NULL;
	if (dom_element_get_attribute(el, key, &value) == DOM_NO_ERR && value != NULL) {
		intern(e, dom_string_data(value), dom_string_byte_length(value), off, len);
		dom_string_unref(value);
	}
	dom_string_unref(key);
}

/* An integer attribute, or 0 when absent or not a number. */
static int32_t attr_int(dom_element *el, const char *name)
{
	dom_string *key = NULL;
	if (dom_string_create((const uint8_t *) name, strlen(name), &key) != DOM_NO_ERR)
		return 0;
	dom_string *value = NULL;
	int32_t out = 0;
	if (dom_element_get_attribute(el, key, &value) == DOM_NO_ERR && value != NULL) {
		const char *p = dom_string_data(value);
		size_t n = dom_string_byte_length(value);
		for (size_t i = 0; i < n; i++) {
			if (p[i] < '0' || p[i] > '9')
				break;
			out = out * 10 + (p[i] - '0');
			if (out > 100000) /* a document lying about a size; the layout would refuse it anyway */
				break;
		}
		dom_string_unref(value);
	}
	dom_string_unref(key);
	return out;
}

/* Whether an attribute equals a literal, ignoring case and surrounding blanks.
 *
 * For `method="POST"` and `type="submit"`, which real pages write in every casing there is. */
static bool attr_is(emit *e, dom_element *el, const char *name, const char *want)
{
	(void) e;
	bool hit = false;
	dom_string *key = NULL;
	if (dom_string_create((const uint8_t *) name, strlen(name), &key) != DOM_NO_ERR)
		return false;
	dom_string *value = NULL;
	if (dom_element_get_attribute(el, key, &value) == DOM_NO_ERR && value != NULL) {
		const char *p = dom_string_data(value);
		size_t n = dom_string_byte_length(value);
		while (n > 0 && (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r')) { p++; n--; }
		while (n > 0) {
			char c = p[n - 1];
			if (c != ' ' && c != '\t' && c != '\n' && c != '\r')
				break;
			n--;
		}
		size_t w = strlen(want);
		if (n == w) {
			hit = true;
			for (size_t i = 0; i < n; i++) {
				char a = p[i];
				if (a >= 'A' && a <= 'Z')
					a = (char) (a - 'A' + 'a');
				if (a != want[i]) { hit = false; break; }
			}
		}
		dom_string_unref(value);
	}
	dom_string_unref(key);
	return hit;
}

/* Which control this element is, or FIELD_NONE.
 *
 * An `<input>` with no `type`, or one this does not recognise, is a text field — that is what HTML
 * says an unknown type means, and it is what keeps a page usable when a type appears that this
 * browser has never heard of. */
static uint8_t field_kind_of(emit *e, dom_node *node, dom_string *name)
{
	dom_element *el = (dom_element *) node;
	if (name_is(name, "textarea"))
		return FIELD_TEXTAREA;
	if (name_is(name, "select"))
		return FIELD_SELECT;
	if (name_is(name, "button")) {
		/* A `<button>` defaults to submit, unlike an `<input>`. */
		if (attr_is(e, el, "type", "button") || attr_is(e, el, "type", "reset"))
			return FIELD_BUTTON;
		return FIELD_SUBMIT;
	}
	if (!name_is(name, "input"))
		return FIELD_NONE;

	if (attr_is(e, el, "type", "password")) return FIELD_PASSWORD;
	if (attr_is(e, el, "type", "submit"))   return FIELD_SUBMIT;
	if (attr_is(e, el, "type", "image"))    return FIELD_SUBMIT;
	if (attr_is(e, el, "type", "button"))   return FIELD_BUTTON;
	if (attr_is(e, el, "type", "reset"))    return FIELD_BUTTON;
	if (attr_is(e, el, "type", "checkbox")) return FIELD_CHECKBOX;
	if (attr_is(e, el, "type", "radio"))    return FIELD_RADIO;
	if (attr_is(e, el, "type", "hidden"))   return FIELD_HIDDEN;
	return FIELD_TEXT;
}

/* What a control shows, interned.
 *
 * Mostly the `value` attribute. Three are not:
 *
 *   - a `<textarea>`'s value is its text content, and a `<select>`'s is one of its options, so
 *     those use libdom's own accessors, which do that walk. It is the one place where the typed
 *     interface buys something the attribute read cannot;
 *   - a `<button>`'s label is its *content* — `<button>Search</button>` has no value attribute at
 *     all, and reading one gets an empty string. Measured on Google: the buttons came out labelled
 *     with their `name`, because that was the only non-empty string the browser had for them, and a
 *     name is an identifier for the server rather than a word for a reader.
 *
 * `is_button_el` distinguishes `<button>` from `<input type=submit>`, which share a field kind and
 * do not share where their label lives.
 *
 * For a button that still has nothing, `alt` and then `aria-label` are tried. Those are where a
 * graphical submit keeps its words — `<input type=image>` has no value by construction — and they
 * are written for exactly this purpose: to be read when the picture is not.
 */
static void control_value(emit *e, dom_node *node, uint8_t field, bool is_button_el,
		uint32_t *off, uint32_t *len)
{
	*off = 0;
	*len = 0;
	if (is_button_el) {
		dom_string *v = NULL;
		if (dom_node_get_text_content(node, &v) == DOM_NO_ERR && v != NULL) {
			intern_collapsed(e, v, off, len);
			dom_string_unref(v);
		}
		if (*len > 0)
			return;
	}
	if (field == FIELD_TEXTAREA) {
		dom_string *v = NULL;
		if (dom_html_text_area_element_get_value((dom_html_text_area_element *) node, &v)
				== DOM_NO_ERR && v != NULL) {
			intern_collapsed(e, v, off, len);
			dom_string_unref(v);
		}
		return;
	}
	if (field == FIELD_SELECT) {
		dom_string *v = NULL;
		if (dom_html_select_element_get_value((dom_html_select_element *) node, &v)
				== DOM_NO_ERR && v != NULL) {
			intern_collapsed(e, v, off, len);
			dom_string_unref(v);
		}
		return;
	}
	attr_interned(e, (dom_element *) node, "value", off, len);
	if (*len > 0 || (field != FIELD_SUBMIT && field != FIELD_BUTTON))
		return;
	attr_interned(e, (dom_element *) node, "alt", off, len);
	if (*len > 0)
		return;
	attr_interned(e, (dom_element *) node, "aria-label", off, len);
}


/* Walk `node`, appending its subtree. Returns the index of the first node written, or NODE_NONE when
 * the subtree contributed nothing — which is what `display: none` and a dropped element look like
 * from the caller's side.
 *
 * `inherited` is the style a child starts from. libcss has already applied CSS inheritance, so this
 * is only for what is ours: the link colour and the `href` itself, which flow to descendants so that
 * a link wrapping a bold word is still one link.
 */
/* Emit one node without descending. Returns its index, or NODE_NONE when it contributes nothing.
 *
 * `*descend` comes back true for an element whose children must be walked; the caller owns the
 * looping so this function cannot recurse.
 */
static uint32_t emit_one(emit *e, dom_node *node, const style *inherited, bool root,
		style *out_style, bool *descend)
{
	*descend = false;
	if (e->failed)
		return NODE_NONE;

	dom_node_type type = DOM_NODE_TYPE_COUNT;
	if (dom_node_get_node_type(node, &type) != DOM_NO_ERR)
		return NODE_NONE;

	if (type == DOM_TEXT_NODE) {
		dom_string *content = NULL;
		if (dom_node_get_text_content(node, &content) != DOM_NO_ERR || content == NULL)
			return NODE_NONE;
		uint32_t off, len;
		intern_collapsed(e, content, &off, &len);
		dom_string_unref(content);

		/* Whitespace-only text between blocks is formatting, not content. Without this every
		 * newline in a tidily indented document becomes a space on screen, which on a 320-pixel
		 * column is most of the column. */
		bool blank = true;
		for (uint32_t i = 0; i < len; i++) {
			if (e->text.p[off + i] != ' ') {
				blank = false;
				break;
			}
		}
		if (len == 0 || blank) {
			e->text.len = off; /* unwind the arena; nothing else has been written since */
			return NODE_NONE;
		}

		uint32_t index = node_reserve(e);
		node_write(e, index, KIND_TEXT, off, len, 0, 0, inherited, NODE_NONE, NODE_NONE);
		return index;
	}

	if (type != DOM_ELEMENT_NODE)
		return NODE_NONE;

	dom_string *name = NULL;
	if (dom_node_get_node_name(node, &name) != DOM_NO_ERR)
		return NODE_NONE;

	/* Dropped whole, content included. A stylesheet or a script rendered as prose is the classic
	 * failure of a browser that walks a DOM without knowing what it is looking at.
	 *
	 * `<noscript>` is deliberately NOT in this list, and used to be. Its content is written for a
	 * client that cannot run scripts, which is precisely what this is — the form that works without
	 * JavaScript, the list of links behind a menu that would have been built at runtime. Dropping it
	 * threw away the one part of a modern page meant for us.
	 *
	 * It is safe to render because of what hubbub does with `enable_scripting` false, which is what
	 * this bridge leaves it at: the treebuilder inserts `<noscript>` as an ordinary element and
	 * parses its content as *markup* (in_head.c, IN_HEAD_NOSCRIPT). With scripting on it would
	 * instead be raw text, and rendering it would spray tags across the page as prose. So the two
	 * settings have to agree, and this comment is the link between them.
	 *
	 * `<template>` and `<iframe>` stay: template content is inert by definition, and an iframe is a
	 * nested document this browser has no second window to put. */
	if (name_is(name, "script") || name_is(name, "style") || name_is(name, "head")
			|| name_is(name, "title")
			|| name_is(name, "template") || name_is(name, "iframe")) {
		dom_string_unref(name);
		return NODE_NONE;
	}

	const css_computed_style *cs =
			(const css_computed_style *) dom_select_style(e->sel, node);

	style st = *inherited;
	st.display = display_of(cs, root);
	if (st.display == DISPLAY_NONE) {
		dom_string_unref(name);
		return NODE_NONE;
	}
	st.font = font_role(cs);
	st.rule_below = false;
	st.marker = MARKER_NONE;
	st.marker_off = st.marker_len = 0;

	css_color c;
	if (cs != NULL && css_computed_color(cs, &c) == CSS_COLOR_COLOR)
		st.color = css_color_to_argb(c);
	st.has_bg = false;
	st.bg = 0;
	if (cs != NULL && css_computed_background_color(cs, &c) == CSS_BACKGROUND_COLOR_COLOR) {
		/* A fully transparent background is not a background. Emitting one would make every
		 * element a fill node, and a page of 6000 of them is 6000 rectangles painted for nothing. */
		if ((css_color_to_argb(c) >> 24) != 0) {
			st.has_bg = true;
			st.bg = css_color_to_argb(c);
		}
	}
	edge(cs, st.margin);
	pad(cs, st.padding);

	if (name_is(name, "a")) {
		attr_interned(e, (dom_element *) node, "href", &st.href_off, &st.href_len);
		if (st.href_len > 0)
			st.color = e->pal.link;
	}
	if (name_is(name, "hr"))
		st.rule_below = true;
	if (name_is(name, "li"))
		st.marker = MARKER_BULLET;

	/* A form hands its controls where to submit, and the id is inherited from here down. The
	 * action rides in `href`, which a `<form>` has no other use for — one span, two meanings,
	 * decided by the element that carries it. */
	if (name_is(name, "form")) {
		st.form = e->next_form;
		if (e->next_form != NO_FORM)
			e->next_form++;
		attr_interned(e, (dom_element *) node, "action", &st.href_off, &st.href_len);
		st.method = attr_is(e, (dom_element *) node, "method", "post") ? 1 : 0;
	}

	const uint8_t field = field_kind_of(e, node, name);
	bool is_img = name_is(name, "img");
	/* Read before `name` is released below, which is the only reason it is a variable. */
	bool is_button_el = name_is(name, "button");
	uint32_t index = node_reserve(e);
	dom_string_unref(name);
	if (index == NODE_NONE)
		return NODE_NONE;

	/* A control is one leaf box.
	 *
	 * Never descended into, and that is the point: a `<button>Send</button>` has a text child and a
	 * `<select>` has its options, and laying those out would print them as prose *beside* the box
	 * that already shows them — which is exactly what a `<select>` does today. The label is read
	 * here and the walk stops. */
	if (field != FIELD_NONE) {
		uint32_t name_off = 0, name_len = 0;
		attr_interned(e, (dom_element *) node, "name", &name_off, &name_len);
		uint32_t val_off = 0, val_len = 0;
		control_value(e, node, field, is_button_el, &val_off, &val_len);
		node_write_full(e, index, KIND_CONTROL, val_off, val_len, 0, 0, &st, field,
				name_off, name_len, NODE_NONE, NODE_NONE);
		return index;
	}

	/* An image is a leaf with a size the document may or may not have stated. */
	if (is_img) {
		uint32_t src_off, src_len;
		attr_interned(e, (dom_element *) node, "src", &src_off, &src_len);
		int32_t w = attr_int((dom_element *) node, "width");
		int32_t h = attr_int((dom_element *) node, "height");
		node_write(e, index, KIND_IMAGE, src_off, src_len, w, h, &st, NODE_NONE, NODE_NONE);
		return index;
	}

	/* Written now with no children; the caller patches the first child in once they exist. */
	node_write(e, index, KIND_ELEMENT, 0, 0, 0, 0, &st, NODE_NONE, NODE_NONE);
	*out_style = st;
	*descend = true;
	return index;
}

/* Where a node record's `first_child` and `next_sibling` live, for patching. */
static uint8_t *first_child_at(emit *e, uint32_t index)
{
	return e->nodes.p + (size_t) index * WIRE_NODE + WIRE_NODE - 8;
}

static uint8_t *next_sibling_at(emit *e, uint32_t index)
{
	return e->nodes.p + (size_t) index * WIRE_NODE + WIRE_NODE - 4;
}

/* Walk `root`'s subtree. Returns the index of the node it emitted for `root`, or NODE_NONE. */
static uint32_t walk(emit *e, dom_node *root, const style *inherited, bool is_root)
{
	style st;
	bool descend = false;
	uint32_t index = emit_one(e, root, inherited, is_root, &st, &descend);
	if (index == NODE_NONE || !descend)
		return index;

	level *stack = calloc(MAX_DEPTH, sizeof(level));
	if (stack == NULL) {
		e->failed = true;
		return index;
	}

	int top = 0;
	stack[0].node = root;
	stack[0].kids = NULL;
	stack[0].count = 0;
	stack[0].at = 0;
	stack[0].self_index = index;
	stack[0].first = NODE_NONE;
	stack[0].last = NODE_NONE;
	stack[0].st = st;
	if (dom_node_get_child_nodes(root, &stack[0].kids) != DOM_NO_ERR)
		stack[0].kids = NULL;
	if (stack[0].kids != NULL)
		dom_nodelist_get_length(stack[0].kids, &stack[0].count);

	while (top >= 0 && !e->failed) {
		level *lv = &stack[top];

		if (lv->at >= lv->count) {
			/* This level is done: link its children in and pop. */
			if (lv->first != NODE_NONE)
				write_le32(first_child_at(e, lv->self_index), lv->first);
			if (lv->kids != NULL)
				dom_nodelist_unref(lv->kids);
			top--;
			continue;
		}

		dom_node *kid = NULL;
		uint32_t i = lv->at++;
		if (dom_nodelist_item(lv->kids, i, &kid) != DOM_NO_ERR || kid == NULL)
			continue;

		style child_st;
		bool child_descend = false;
		uint32_t ci = emit_one(e, kid, &lv->st, false, &child_st, &child_descend);
		if (ci == NODE_NONE) {
			dom_node_unref(kid);
			continue;
		}

		/* Chain it onto this level's sibling list. */
		if (lv->first == NODE_NONE)
			lv->first = ci;
		else
			write_le32(next_sibling_at(e, lv->last), ci);
		lv->last = ci;

		if (!child_descend) {
			dom_node_unref(kid);
			continue;
		}

		if (top + 1 >= MAX_DEPTH) {
			/* Truncated, not crashed. A document nested this deep is generated, and what a reader
			 * wants from it is above the bound. */
			dom_node_unref(kid);
			continue;
		}

		top++;
		level *next = &stack[top];
		next->node = kid;
		next->kids = NULL;
		next->count = 0;
		next->at = 0;
		next->self_index = ci;
		next->first = NODE_NONE;
		next->last = NODE_NONE;
		next->st = child_st;
		if (dom_node_get_child_nodes(kid, &next->kids) != DOM_NO_ERR)
			next->kids = NULL;
		if (next->kids != NULL)
			dom_nodelist_get_length(next->kids, &next->count);
		/* The reference taken by `dom_nodelist_item` is held for as long as this level is on the
		 * stack, and released when it pops — the node list alone does not keep the node alive. */
	}

	/* Unwind anything left holding a list, which happens when `e->failed` broke the loop. */
	while (top >= 0) {
		if (stack[top].kids != NULL)
			dom_nodelist_unref(stack[top].kids);
		top--;
	}
	free(stack);
	return index;
}

/* --------------------------------------------------------------------------- the entry point -- */

/* ------------------------------------------------------------------------------- selftest --
 *
 * One primitive per call, so a thread that dies names the layer it died in instead of a span. The
 * breadcrumb is written *before* each call and again after: a step that never returns leaves the
 * "before" tag behind, which is the measurement. See dom_bridge.h for why this exists.
 */
int32_t dom_selftest(int32_t step)
{
	switch (step) {
	case DOM_SELF_MALLOC: {
		dom_stage("self_malloc");
		/* 8 KB, not 8 bytes: a size below the allocator's small-block path proves less. */
		void *p = malloc(8 * 1024);
		if (p == NULL)
			return DOM_ERR_NO_MEMORY;
		memset(p, 0x5a, 8 * 1024);
		free(p);
		dom_stage("self_malloc_ok");
		return 0;
	}
	case DOM_SELF_SNPRINTF: {
		dom_stage("self_snprintf");
		char buf[64];
		int n = snprintf(buf, sizeof(buf), "%d %s %ld", 42, "x", (long) sizeof(buf));
		dom_stage("self_snprintf_ok");
		return (n > 0) ? 0 : DOM_ERR_INTERNAL;
	}
	case DOM_SELF_STRTOD: {
		dom_stage("self_strtod");
		/* strtod reads the decimal point from the locale, and a locale is per-thread state in
		 * Open C — which is the thing being tested, not the arithmetic. */
		char *end = NULL;
		double d = strtod("1.5", &end);
		dom_stage("self_strtod_ok");
		return (d > 1.0 && d < 2.0) ? 0 : DOM_ERR_INTERNAL;
	}
	case DOM_SELF_LWC: {
		dom_stage("self_lwc");
		lwc_string *str = NULL;
		lwc_error err = lwc_intern_string("div", 3, &str);
		if (err != lwc_error_ok)
			return DOM_ERR_INTERNAL;
		lwc_string_unref(str);
		dom_stage("self_lwc_ok");
		return 0;
	}
	case DOM_SELF_HUBBUB: {
		dom_stage("self_hubbub");
		hubbub_parser *parser = NULL;
		hubbub_error err = hubbub_parser_create("UTF-8", true, &parser);
		if (err != HUBBUB_OK)
			return DOM_ERR_PARSE;
		hubbub_parser_destroy(parser);
		dom_stage("self_hubbub_ok");
		return 0;
	}
	case DOM_SELF_DOM: {
		dom_stage("self_dom");
		dom_hubbub_parser_params params;
		memset(&params, 0, sizeof(params));
		params.enc = "UTF-8";
		params.fix_enc = true;

		dom_hubbub_parser *parser = NULL;
		dom_document *doc = NULL;
		if (dom_hubbub_parser_create(&params, &parser, &doc) != DOM_HUBBUB_OK)
			return DOM_ERR_PARSE;
		dom_hubbub_parser_destroy(parser);
		if (doc != NULL)
			dom_node_unref(doc);
		dom_stage("self_dom_ok");
		return 0;
	}
	case DOM_SELF_PARTS: {
		/* A breadcrumb before every call, because the thread dying is the likely outcome and a
		 * breadcrumb is all it leaves. */
		dom_stage("part_iconv");
		iconv_t cd = iconv_open("UTF-8", "UTF-8");
		if (cd == (iconv_t) -1)
			return DOM_ERR_INTERNAL;
		iconv_close(cd);

		dom_stage("part_mibenum");
		uint16_t mib = parserutils_charset_mibenum_from_name("UTF-8", 5);
		if (mib == 0)
			return DOM_ERR_INTERNAL;

		dom_stage("part_stream");
		parserutils_inputstream *stream = NULL;
		if (parserutils_inputstream_create("UTF-8", 1, NULL, &stream) != PARSERUTILS_OK)
			return DOM_ERR_INTERNAL;
		parserutils_inputstream_destroy(stream);

		dom_stage("part_ok");
		return 0;
	}
	default:
		return DOM_ERR_ARGUMENT;
	}
}

int32_t dom_build(const uint8_t *html, int32_t html_len, int32_t width,
		const dom_palette *palette, uint8_t *out, int32_t out_cap)
{
	(void) width; /* the column is the layout's business; nothing here depends on it */

	dom_stage("dom_enter");
	if (html == NULL || html_len <= 0 || palette == NULL || out == NULL || out_cap <= 0)
		return DOM_ERR_ARGUMENT;

	dom_hubbub_parser_params params;
	memset(&params, 0, sizeof(params));

	/* No encoding declared here, so the *document* decides. This used to say `params.enc =
	 * "UTF-8"`, and that is not a hint — libhubbub reads a non-NULL encoding as
	 * `HUBBUB_CHARSET_CONFIDENT`, meaning "I know what this is, do not look". So the parser never
	 * consulted the page's own `<meta charset>` or its BOM.
	 *
	 * On a page that really is UTF-8 it made no difference, which is why it survived: English
	 * pages and Wikipedia both render. On a page in ISO-8859-1 or Windows-1252 — which is most of
	 * the older Brazilian web — every accented byte is invalid UTF-8, so `ç` and `ã` simply
	 * vanished. Reported by eye, on a page with a cedilla in it.
	 *
	 * NULL makes the stream `HUBBUB_CHARSET_UNKNOWN` and lets `hubbub_charset_extract` — passed in
	 * either way, and never called before — find the declaration. That is the order a browser
	 * uses, minus one step we cannot take yet: the HTTP `Content-Type` charset outranks the
	 * document and `shim_http.cpp` does not report it. A page whose server declares one encoding
	 * and whose markup declares another will follow the markup here, which is the wrong authority
	 * and a rarer problem than the one this fixes.
	 *
	 * `fix_enc` stays on: it repairs the aliases real pages carry, and it applies to whatever the
	 * document turned out to say. */
	params.fix_enc = true;

	dom_hubbub_parser *parser = NULL;
	dom_document *doc = NULL;
	dom_stage("parser_create");
	if (dom_hubbub_parser_create(&params, &parser, &doc) != DOM_HUBBUB_OK)
		return DOM_ERR_PARSE;

	int32_t rc = DOM_ERR_PARSE;
	emit e;
	memset(&e, 0, sizeof(e));
	e.pal = *palette;

	dom_stage("parse_chunk");
	if (dom_hubbub_parser_parse_chunk(parser, html, (size_t) html_len) != DOM_HUBBUB_OK)
		goto done;
	dom_stage("parse_done");
	if (dom_hubbub_parser_completed(parser) != DOM_HUBBUB_OK)
		goto done;

	dom_stage("select_create");
	e.sel = dom_select_create(doc);
	if (e.sel == NULL) {
		rc = DOM_ERR_CSS;
		goto done;
	}

	dom_stage("root");
	dom_element *root = NULL;
	if (dom_document_get_document_element(doc, &root) != DOM_NO_ERR || root == NULL) {
		rc = DOM_ERR_PARSE;
		goto done;
	}

	{
		/* Node 0 is a container the layout can hang everything from, so an empty document is still
		 * a tree with a root rather than a special case on the other side. */
		style base;
		memset(&base, 0, sizeof(base));
		base.display = DISPLAY_BLOCK;
		base.font = FONT_BODY;
		base.color = palette->text;
		base.form = NO_FORM;

		dom_stage("walk");
		uint32_t container = node_reserve(&e);
		uint32_t child = walk(&e, (dom_node *) root, &base, true);
		dom_stage("walked");
		node_write(&e, container, KIND_ELEMENT, 0, 0, 0, 0, &base, child, NODE_NONE);
	}
	dom_node_unref(root);

	if (e.failed || e.nodes.failed || e.text.failed) {
		rc = DOM_ERR_NO_MEMORY;
		goto done;
	}

	{
		size_t count = e.nodes.len / WIRE_NODE;
		size_t total = WIRE_HEADER + e.nodes.len + e.text.len;
		if (total > (size_t) out_cap) {
			/* Refused whole. A prefix decodes as nothing on the other side anyway, and "nothing"
			 * reported as success is the worse of the two failures. */
			rc = DOM_ERR_OVERFLOW;
			goto done;
		}
		uint8_t *o = out;
		*o++ = WIRE_MAGIC_0;
		*o++ = WIRE_MAGIC_1;
		*o++ = WIRE_MAGIC_2;
		*o++ = WIRE_MAGIC_3;
		write_le32(o, (uint32_t) count); o += 4;
		write_le32(o, (uint32_t) e.text.len); o += 4;
		write_le32(o, (uint32_t) total); o += 4;
		memcpy(o, e.nodes.p, e.nodes.len); o += e.nodes.len;
		memcpy(o, e.text.p, e.text.len);
		dom_stage("emitted");
		rc = (int32_t) total;
	}

done:
	if (e.sel != NULL)
		dom_select_destroy(e.sel);
	buf_free(&e.nodes);
	buf_free(&e.text);
	dom_hubbub_parser_destroy(parser);
	if (doc != NULL)
		dom_node_unref(doc);
	return rc;
}
