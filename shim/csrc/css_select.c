/* The select half of the DOM bridge: a UA stylesheet, the document's author sheets, and the
 * 36 questions libcss asks about a node.
 *
 * The three public functions are dom_bridge.h's "select" section and nothing else here is
 * visible. See that header for why a C layer exists at all.
 *
 * THE PART OF libcss THE HEADER DOES NOT MENTION
 *
 * css_select_style does not return a usable style. Its own comment says so: "In computing the
 * style, no reference is made to the parent node's style. Therefore, the resultant computed
 * style is not ready for immediate use, as some properties may be marked as inherited. Use
 * css_computed_style_compose() to obtain a fully computed style."
 *
 * So a caller that wants a *computed* style — which is what dom_select_style promises — has to
 * hold the composed style of every ancestor of the node it is asking about, because compose()
 * takes the parent's composed style as its left operand. That is O(depth), not O(nodes), and it
 * is the reason this file keeps a chain (see `chain_entry`) rather than just handing back what
 * css_select_style produced.
 *
 * The chain is rebuilt against the node's real parent pointers on every call, so it is correct
 * for any call order. It is *cheap* only for a document-order walk, where the common prefix is
 * the whole chain but for one entry and exactly one compose happens per node. dom_bridge.c walks
 * in document order, so that is the case that matters; a random-order caller still gets right
 * answers, at the cost of re-selecting the ancestors it skipped.
 *
 * COMPOSED STYLES ARE INTERNED
 *
 * css_computed_style_compose ends in css__arena_intern_style, so two nodes that compute the same
 * style share one allocation and a refcount. That is worth knowing before anyone tries to make
 * this cheaper: the O(depth) chain is already sharing with everything else on the page.
 *
 * WHAT IS NOT HERE
 *
 * No linked stylesheets (dom_bridge.h: not this layer's decision), no @import (the URL resolver
 * below is a stub for that reason), no :hover/:active/:focus/:visited (there is no interaction
 * state on this side of the bridge — dom_build is one shot), and no palette. The palette is a
 * dom_build argument, not a dom_select_create one, so this file cannot know it and the UA sheet
 * deliberately sets no colours beyond what the HTML spec's suggested rendering requires.
 */

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include <libwapcaplet/libwapcaplet.h>
#include <dom/dom.h>
#include <libcss/libcss.h>

#include "dom_bridge.h"

/* ------------------------------------------------------------------ the UA stylesheet --
 *
 * libcss ships no default stylesheet and NetSurf's is GPL-2.0, so this is written from the HTML
 * specification's suggested rendering (which is itself the CSS 2.1 sample sheet plus HTML's own
 * additions). It is trimmed rather than complete, and the trims are the point:
 *
 *  - no @page, no aural properties, no bidi: nothing downstream reads them.
 *  - no `vertical-align` table rules: this handset's layout is line-based, and the CSS table
 *    model is not in the plan for F6.
 *  - no colours except the link underline. See the palette note in the file header.
 *  - `font-size: medium` is not spelled anywhere; it comes from css_unit_ctx.font_size_default.
 *
 * The 8px body margin is the spec's, kept rather than shrunk: on a 320px viewport it is 5% of the
 * width, which is a layout judgement and not this file's to make. Whoever sizes the viewport can
 * override it with a user sheet.
 */
static const char ua_stylesheet[] =
	"html, address, blockquote, body, dd, div, dl, dt, fieldset, form,"
	" frame, frameset, h1, h2, h3, h4, h5, h6, noframes, ol, p, ul,"
	" center, dir, hr, menu, pre, article, aside, footer, header, nav,"
	" section, figure, figcaption, main { display: block }\n"
	"li { display: list-item }\n"
	"head, script, style, title, link, meta, base, param, area, template"
	" { display: none }\n"
	"table { display: table; border-spacing: 2px }\n"
	"caption { display: table-caption; text-align: center }\n"
	"thead { display: table-header-group }\n"
	"tbody { display: table-row-group }\n"
	"tfoot { display: table-footer-group }\n"
	"colgroup { display: table-column-group }\n"
	"col { display: table-column }\n"
	"tr { display: table-row }\n"
	"td { display: table-cell }\n"
	"th { display: table-cell; font-weight: bolder; text-align: center }\n"
	"body { margin: 8px }\n"
	"h1 { font-size: 2em; margin: 0.67em 0 }\n"
	"h2 { font-size: 1.5em; margin: 0.83em 0 }\n"
	"h3 { font-size: 1.17em; margin: 1em 0 }\n"
	"h4 { margin: 1.33em 0 }\n"
	"h5 { font-size: 0.83em; margin: 1.67em 0 }\n"
	"h6 { font-size: 0.67em; margin: 2.33em 0 }\n"
	"h1, h2, h3, h4, h5, h6, b, strong, th { font-weight: bolder }\n"
	"p, blockquote, figure, dl, fieldset, form, ol, ul, dir, menu, pre,"
	" hr { margin: 1em 0 }\n"
	"blockquote, figure { margin-left: 40px; margin-right: 40px }\n"
	"ol, ul, dir, menu, dd { padding-left: 40px }\n"
	"ol { list-style-type: decimal }\n"
	"ul ul, ul ol, ol ul, ol ol { margin-top: 0; margin-bottom: 0 }\n"
	"dt { font-weight: bolder }\n"
	"i, cite, em, var, address, dfn { font-style: italic }\n"
	"pre, tt, code, kbd, samp { font-family: monospace }\n"
	"pre { white-space: pre }\n"
	"nobr { white-space: nowrap }\n"
	"big { font-size: 1.17em }\n"
	"small, sub, sup { font-size: 0.83em }\n"
	"sub { vertical-align: sub }\n"
	"sup { vertical-align: super }\n"
	"u, ins { text-decoration: underline }\n"
	"s, strike, del { text-decoration: line-through }\n"
	"a:link { text-decoration: underline }\n"
	"center { text-align: center }\n"
	"hr { border: 1px inset }\n"
	"input, textarea, select, button, img, object, iframe, applet"
	" { display: inline-block }\n"
	"input, textarea, select, button { font-size: 0.9em }\n"
	/* A control's own text is never laid out — the emitter reads the label and stops — so these
	 * exist to keep the *options* of a `<select>` from being painted as prose beside the box that
	 * already shows the chosen one. That is a bug you can see on any page with a dropdown. */
	"option, optgroup { display: none }\n"
	/* A hidden input is submitted and never shown. Without this it lays out as an empty box the
	 * reader can see and cannot use. */
	"input[type=hidden] { display: none }\n"
	"table { text-align: left }\n";

/* ---------------------------------------------------------------------------- the state -- */

/* One entry per element from the document root down to the node most recently asked about. */
typedef struct chain_entry {
	void *node;                  /* borrowed dom_element * — the tree owns the reference */
	css_computed_style *style;   /* composed; this entry owns a reference */
} chain_entry;

/* Enough for every hint node_presentational_hint can emit for one element at once:
 * bgcolor, color, width, height, four margins, text-align, float, white-space. */
#define HINT_MAX 16

struct dom_select {
	dom_document *doc;           /* borrowed: dom_bridge.c owns the document */

	css_select_ctx *ctx;
	css_stylesheet *ua;
	css_stylesheet **author;
	uint32_t n_author;

	css_media media;
	css_unit_ctx units;

	chain_entry *chain;
	uint32_t n_chain;
	uint32_t chain_cap;

	void **path;                 /* scratch: root-down path to the node being selected */
	uint32_t path_cap;

	css_hint hints[HINT_MAX];

	bool holds_key;              /* this context counts towards node_data_key_users */

	/* Attribute names, interned once. Every presentational hint and every attribute selector
	 * needs a dom_string to look one up, and creating them per query would be an allocation
	 * per attribute per node. */
	dom_string *a_id;
	dom_string *a_class;
	dom_string *a_bgcolor;
	dom_string *a_text;
	dom_string *a_color;
	dom_string *a_width;
	dom_string *a_height;
	dom_string *a_align;
	dom_string *a_nowrap;
	dom_string *a_hspace;
	dom_string *a_vspace;
	dom_string *a_href;
	dom_string *a_disabled;
	dom_string *a_checked;
	dom_string *a_lang;

	/* Element names the hint code has to recognise. */
	lwc_string *e_center;
	lwc_string *e_body;
	lwc_string *e_font;
	lwc_string *e_basefont;
	lwc_string *e_a;
	lwc_string *e_area;
	lwc_string *e_link;
	lwc_string *e_td;
	lwc_string *e_th;
	lwc_string *e_img;
	lwc_string *e_object;
	lwc_string *e_applet;
	lwc_string *e_iframe;
	lwc_string *e_input;
	lwc_string *e_button;
	lwc_string *e_select;
	lwc_string *e_textarea;
	lwc_string *e_optgroup;
	lwc_string *e_option;
	lwc_string *e_fieldset;
	lwc_string *e_style;
};

/* The key libcss's per-node selector cache is stored under, and the one piece of state in this
 * file that cannot live in `struct dom_select`.
 *
 * The reason is the trampoline below: libdom hands a user-data handler (operation, key, data,
 * src, dst) and no client word, so a handler that has to call back into set_libcss_node_data
 * cannot be given the select context. Making the key file-static is what lets set/get ignore
 * `pw` entirely, which in turn is what lets the trampoline pass pw = NULL.
 *
 * It is refcounted against live select contexts rather than leaked, because Open C's malloc here
 * is the process heap with __UHEAP_MARKEND armed: one abandoned dom_string is a panic, not a
 * warning. libdom takes its own reference per user-data entry (_dom_node_set_user_data), so
 * dropping ours in dom_select_destroy is safe while nodes still hold data.
 */
static dom_string *node_data_key;
static uint32_t node_data_key_users;

/* ------------------------------------------------------------------------ small helpers -- */

static css_error resolve_url(void *pw, const char *base, lwc_string *rel, lwc_string **abs)
{
	(void) pw;
	(void) base;
	/* Nothing is fetched, so nothing needs resolving. A sheet with an @import records the
	 * relative URL verbatim as a pending import that no one ever collects. */
	*abs = lwc_string_ref(rel);
	return CSS_OK;
}

/* An element's name as a dom_string, or NULL. Borrowed from nowhere: caller unrefs. */
static dom_string *element_name(void *node)
{
	dom_string *name = NULL;

	if (dom_node_get_node_name((dom_node *) node, &name) != DOM_NO_ERR)
		return NULL;
	return name;
}

static bool name_is(void *node, lwc_string *want)
{
	dom_string *name = element_name(node);
	bool match;

	if (name == NULL)
		return false;
	match = dom_string_caseless_lwc_isequal(name, want);
	dom_string_unref(name);
	return match;
}

static bool is_element(void *node)
{
	dom_node_type type;

	if (node == NULL)
		return false;
	if (dom_node_get_node_type((dom_node *) node, &type) != DOM_NO_ERR)
		return false;
	return type == DOM_ELEMENT_NODE;
}

/* An attribute's value, or NULL if absent. Caller unrefs. */
static dom_string *attr(void *node, dom_string *name)
{
	dom_string *value = NULL;

	if (name == NULL)
		return NULL;
	if (dom_element_get_attribute((dom_element *) node, name, &value) != DOM_NO_ERR)
		return NULL;
	return value;
}

/* The same, for the name libcss hands us in a css_qname.
 *
 * This allocates a dom_string per query, which is the one place in this file that does. An
 * attribute selector is rare enough on a real page that a cache would cost more than it saves,
 * and libcss gives no hook to intern the selector's own names against libdom's table. */
static dom_string *attr_by_qname(void *node, const css_qname *qname)
{
	dom_string *name = NULL;
	dom_string *value;

	if (qname == NULL || qname->name == NULL)
		return NULL;
	if (dom_string_create_interned((const uint8_t *) lwc_string_data(qname->name),
			lwc_string_length(qname->name), &name) != DOM_NO_ERR)
		return NULL;

	value = attr(node, name);
	dom_string_unref(name);
	return value;
}

static bool is_ws(char c)
{
	return c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\f';
}

/* The next element sibling in either direction, borrowed.
 *
 * Every libdom sibling accessor takes a reference and libcss unrefs none of the nodes it is
 * handed, so the reference is dropped before returning. That is safe because the parent holds
 * one for the whole life of the tree — the tree is not mutated between dom_select_create and
 * dom_select_destroy. */
static void *element_sibling(void *node, bool forward)
{
	dom_node *cur = dom_node_ref((dom_node *) node);

	while (cur != NULL) {
		dom_node *next = NULL;
		dom_exception e = forward
				? dom_node_get_next_sibling(cur, &next)
				: dom_node_get_previous_sibling(cur, &next);

		dom_node_unref(cur);
		if (e != DOM_NO_ERR)
			return NULL;
		if (next == NULL)
			return NULL;
		if (is_element(next)) {
			dom_node_unref(next);
			return next;
		}
		cur = next;
	}
	return NULL;
}

/* ------------------------------------------------------------------ HTML value parsing --
 *
 * The presentational-hint attributes carry HTML's own micro-syntaxes, not CSS ones, so libcss's
 * parser is no help and these are hand-written.
 */

/* The sixteen HTML 4 colour names plus the handful that predate them and still appear.
 * Not the 140 SVG names: those belong to CSS, where libcss already parses them, and a page that
 * writes bgcolor="rebeccapurple" is not a page this handset is for. */
static const struct { const char *name; css_color value; } html_colors[] = {
	{ "black",   0xff000000 }, { "silver",  0xffc0c0c0 },
	{ "gray",    0xff808080 }, { "grey",    0xff808080 },
	{ "white",   0xffffffff }, { "maroon",  0xff800000 },
	{ "red",     0xffff0000 }, { "purple",  0xff800080 },
	{ "fuchsia", 0xffff00ff }, { "magenta", 0xffff00ff },
	{ "green",   0xff008000 }, { "lime",    0xff00ff00 },
	{ "olive",   0xff808000 }, { "yellow",  0xffffff00 },
	{ "navy",    0xff000080 }, { "blue",    0xff0000ff },
	{ "teal",    0xff008080 }, { "aqua",    0xff00ffff },
	{ "cyan",    0xff00ffff }, { "orange",  0xffffa500 }
};

static int hex_digit(char c)
{
	if (c >= '0' && c <= '9')
		return c - '0';
	if (c >= 'a' && c <= 'f')
		return c - 'a' + 10;
	if (c >= 'A' && c <= 'F')
		return c - 'A' + 10;
	return -1;
}

static bool parse_html_color(const char *p, size_t len, css_color *out)
{
	size_t i;
	int d[6];

	while (len > 0 && is_ws(*p)) { p++; len--; }
	while (len > 0 && is_ws(p[len - 1])) len--;

	if (len > 0 && *p == '#') { p++; len--; }

	if (len == 3 || len == 6) {
		for (i = 0; i < len; i++) {
			d[i] = hex_digit(p[i]);
			if (d[i] < 0)
				break;
		}
		if (i == len) {
			if (len == 3)
				*out = 0xff000000u |
					((uint32_t) (d[0] * 17) << 16) |
					((uint32_t) (d[1] * 17) << 8) |
					(uint32_t) (d[2] * 17);
			else
				*out = 0xff000000u |
					((uint32_t) (d[0] * 16 + d[1]) << 16) |
					((uint32_t) (d[2] * 16 + d[3]) << 8) |
					(uint32_t) (d[4] * 16 + d[5]);
			return true;
		}
	}

	for (i = 0; i < sizeof(html_colors) / sizeof(html_colors[0]); i++) {
		size_t n = strlen(html_colors[i].name);

		if (n == len && strncasecmp(p, html_colors[i].name, n) == 0) {
			*out = html_colors[i].value;
			return true;
		}
	}

	return false;
}

/* HTML's length: digits, or digits followed by '%'. A trailing '*' (the frameset relative
 * length) has no CSS spelling and is refused rather than guessed at. */
static bool parse_html_length(const char *p, size_t len, css_hint_length *out)
{
	int32_t value = 0;
	size_t digits = 0;

	while (len > 0 && is_ws(*p)) { p++; len--; }

	while (len > 0 && p[0] >= '0' && p[0] <= '9') {
		if (value < 100000)                       /* clamp, do not overflow css_fixed */
			value = value * 10 + (p[0] - '0');
		digits++;
		p++;
		len--;
	}
	if (digits == 0)
		return false;

	out->value = INTTOFIX(value);
	out->unit = CSS_UNIT_PX;

	if (len > 0 && p[0] == '%') {
		out->unit = CSS_UNIT_PCT;
		return true;
	}
	while (len > 0 && is_ws(*p)) { p++; len--; }
	return len == 0;                                  /* "100px" is not HTML; refuse it */
}

/* ---------------------------------------------------------------------- the 36 callbacks --
 *
 * All of them take `pw` as the struct dom_select * and `node` as a dom_element *. libcss only
 * ever passes element nodes.
 *
 * Ownership, which the header does not state and libcss's select.c does:
 *  - node_name, node_id: libcss unrefs the lwc_strings, so they are handed over with a
 *    reference (css_select__initialise_selection_state's failure path, select.c:1034).
 *  - node_classes: libcss unrefs each string but never frees the array, which is exactly what
 *    dom_element_get_classes returns — the element's own array, each entry reffed.
 *  - the node-returning callbacks: libcss unrefs nothing, so they return borrowed pointers.
 *  - node_presentational_hint: libcss never frees the array either, so it is ours and lives in
 *    the select context for the duration of the call.
 */

static css_error node_name(void *pw, void *node, css_qname *qname)
{
	dom_string *name = element_name(node);
	lwc_string *interned = NULL;

	(void) pw;

	if (name == NULL)
		return CSS_NOMEM;

	/* The name is left as libdom stores it, which for an HTML document is upper case. That
	 * is correct and not laziness: every comparison libcss makes against it goes through
	 * lwc_string's `insensitive` form (select/hash.c:347 hashes it caselessly, :361 compares
	 * the insensitive strings), so lower-casing here would buy nothing and cost an
	 * allocation per element. */
	if (dom_string_intern(name, &interned) != DOM_NO_ERR) {
		dom_string_unref(name);
		return CSS_NOMEM;
	}
	dom_string_unref(name);

	qname->ns = NULL;
	qname->name = interned;
	return CSS_OK;
}

static css_error node_classes(void *pw, void *node, lwc_string ***classes, uint32_t *n_classes)
{
	(void) pw;

	*classes = NULL;
	*n_classes = 0;

	if (dom_element_get_classes((dom_element *) node, classes, n_classes) != DOM_NO_ERR)
		return CSS_NOMEM;
	return CSS_OK;
}

static css_error node_id(void *pw, void *node, lwc_string **id)
{
	struct dom_select *sel = pw;
	dom_string *value = attr(node, sel->a_id);
	lwc_string *interned = NULL;

	*id = NULL;
	if (value == NULL)
		return CSS_OK;

	if (dom_string_intern(value, &interned) != DOM_NO_ERR) {
		dom_string_unref(value);
		return CSS_NOMEM;
	}
	dom_string_unref(value);

	*id = interned;
	return CSS_OK;
}

static css_error named_ancestor_node(void *pw, void *node, const css_qname *qname,
		void **ancestor)
{
	dom_element *found = NULL;

	(void) pw;
	*ancestor = NULL;

	if (dom_element_named_ancestor_node((dom_element *) node, qname->name,
			&found) != DOM_NO_ERR)
		return CSS_NOMEM;

	/* The declaration in dom/core/element.h says these "don't take a reference". The
	 * implementation does (src/core/element.c does dom_node_ref before returning, and its own
	 * comment says the caller must unref). The comment in the header is wrong; trust the
	 * code. */
	if (found != NULL)
		dom_node_unref(found);
	*ancestor = found;
	return CSS_OK;
}

static css_error named_parent_node(void *pw, void *node, const css_qname *qname, void **parent)
{
	dom_element *found = NULL;

	(void) pw;
	*parent = NULL;

	if (dom_element_named_parent_node((dom_element *) node, qname->name,
			&found) != DOM_NO_ERR)
		return CSS_NOMEM;
	if (found != NULL)
		dom_node_unref(found);
	*parent = found;
	return CSS_OK;
}

static css_error named_sibling_node(void *pw, void *node, const css_qname *qname, void **sibling)
{
	void *prev = element_sibling(node, false);
	dom_string *name;

	(void) pw;
	*sibling = NULL;

	if (prev == NULL)
		return CSS_OK;

	name = element_name(prev);
	if (name == NULL)
		return CSS_NOMEM;
	if (dom_string_caseless_lwc_isequal(name, qname->name))
		*sibling = prev;
	dom_string_unref(name);
	return CSS_OK;
}

static css_error named_generic_sibling_node(void *pw, void *node, const css_qname *qname,
		void **sibling)
{
	void *cur = element_sibling(node, false);

	(void) pw;
	*sibling = NULL;

	while (cur != NULL) {
		dom_string *name = element_name(cur);
		bool match;

		if (name == NULL)
			return CSS_NOMEM;
		match = dom_string_caseless_lwc_isequal(name, qname->name);
		dom_string_unref(name);
		if (match) {
			*sibling = cur;
			return CSS_OK;
		}
		cur = element_sibling(cur, false);
	}
	return CSS_OK;
}

static css_error parent_node(void *pw, void *node, void **parent)
{
	dom_element *found = NULL;

	(void) pw;
	*parent = NULL;

	if (dom_element_parent_node((dom_element *) node, &found) != DOM_NO_ERR)
		return CSS_NOMEM;
	if (found != NULL)
		dom_node_unref(found);
	*parent = found;
	return CSS_OK;
}

static css_error sibling_node(void *pw, void *node, void **sibling)
{
	(void) pw;
	*sibling = element_sibling(node, false);
	return CSS_OK;
}

static css_error node_has_name(void *pw, void *node, const css_qname *qname, bool *match)
{
	dom_string *name;

	(void) pw;

	/* The universal selector reaches here only inside :not(); libcss short-circuits it
	 * everywhere else. It is spelled as a one-character name rather than a sentinel, which
	 * is also how libcss's own bloom builder tests for it (select/hash.c). */
	if (lwc_string_length(qname->name) == 1 && lwc_string_data(qname->name)[0] == '*') {
		*match = true;
		return CSS_OK;
	}

	name = element_name(node);
	if (name == NULL)
		return CSS_NOMEM;
	*match = dom_string_caseless_lwc_isequal(name, qname->name);
	dom_string_unref(name);
	return CSS_OK;
}

static css_error node_has_class(void *pw, void *node, lwc_string *name, bool *match)
{
	(void) pw;

	/* libdom's own implementation, which consults the document's quirks mode to decide
	 * whether the comparison is caseless. Reimplementing it here would get that wrong. */
	if (dom_element_has_class((dom_element *) node, name, match) != DOM_NO_ERR)
		return CSS_NOMEM;
	return CSS_OK;
}

static css_error node_has_id(void *pw, void *node, lwc_string *name, bool *match)
{
	struct dom_select *sel = pw;
	dom_string *value = attr(node, sel->a_id);

	*match = false;
	if (value == NULL)
		return CSS_OK;
	*match = dom_string_lwc_isequal(value, name);
	dom_string_unref(value);
	return CSS_OK;
}

static css_error node_has_attribute(void *pw, void *node, const css_qname *qname, bool *match)
{
	dom_string *value = attr_by_qname(node, qname);

	(void) pw;
	*match = value != NULL;
	if (value != NULL)
		dom_string_unref(value);
	return CSS_OK;
}

static css_error node_has_attribute_equal(void *pw, void *node, const css_qname *qname,
		lwc_string *want, bool *match)
{
	dom_string *value = attr_by_qname(node, qname);

	(void) pw;
	*match = false;
	if (value == NULL)
		return CSS_OK;
	*match = dom_string_caseless_lwc_isequal(value, want);
	dom_string_unref(value);
	return CSS_OK;
}

/* |= : the whole value, or a prefix of it ending at a '-'. */
static css_error node_has_attribute_dashmatch(void *pw, void *node, const css_qname *qname,
		lwc_string *want, bool *match)
{
	dom_string *value = attr_by_qname(node, qname);
	const char *v, *w;
	size_t vlen, wlen;

	(void) pw;
	*match = false;
	if (value == NULL)
		return CSS_OK;

	v = dom_string_data(value);
	vlen = dom_string_byte_length(value);
	w = lwc_string_data(want);
	wlen = lwc_string_length(want);

	if (wlen > 0 && vlen >= wlen && strncasecmp(v, w, wlen) == 0 &&
			(vlen == wlen || v[wlen] == '-'))
		*match = true;

	dom_string_unref(value);
	return CSS_OK;
}

/* ~= : one member of a whitespace-separated list. */
static css_error node_has_attribute_includes(void *pw, void *node, const css_qname *qname,
		lwc_string *want, bool *match)
{
	dom_string *value = attr_by_qname(node, qname);
	const char *v, *w;
	size_t vlen, wlen, i;

	(void) pw;
	*match = false;
	if (value == NULL)
		return CSS_OK;

	v = dom_string_data(value);
	vlen = dom_string_byte_length(value);
	w = lwc_string_data(want);
	wlen = lwc_string_length(want);

	for (i = 0; wlen > 0 && i < vlen; ) {
		size_t start;

		while (i < vlen && is_ws(v[i]))
			i++;
		start = i;
		while (i < vlen && !is_ws(v[i]))
			i++;
		if (i - start == wlen && strncasecmp(v + start, w, wlen) == 0) {
			*match = true;
			break;
		}
	}

	dom_string_unref(value);
	return CSS_OK;
}

static css_error node_has_attribute_prefix(void *pw, void *node, const css_qname *qname,
		lwc_string *want, bool *match)
{
	dom_string *value = attr_by_qname(node, qname);
	size_t wlen;

	(void) pw;
	*match = false;
	if (value == NULL)
		return CSS_OK;

	wlen = lwc_string_length(want);
	if (wlen > 0 && dom_string_byte_length(value) >= wlen &&
			strncasecmp(dom_string_data(value), lwc_string_data(want), wlen) == 0)
		*match = true;

	dom_string_unref(value);
	return CSS_OK;
}

static css_error node_has_attribute_suffix(void *pw, void *node, const css_qname *qname,
		lwc_string *want, bool *match)
{
	dom_string *value = attr_by_qname(node, qname);
	size_t vlen, wlen;

	(void) pw;
	*match = false;
	if (value == NULL)
		return CSS_OK;

	vlen = dom_string_byte_length(value);
	wlen = lwc_string_length(want);
	if (wlen > 0 && vlen >= wlen && strncasecmp(dom_string_data(value) + vlen - wlen,
			lwc_string_data(want), wlen) == 0)
		*match = true;

	dom_string_unref(value);
	return CSS_OK;
}

static css_error node_has_attribute_substring(void *pw, void *node, const css_qname *qname,
		lwc_string *want, bool *match)
{
	dom_string *value = attr_by_qname(node, qname);
	const char *v, *w;
	size_t vlen, wlen, i;

	(void) pw;
	*match = false;
	if (value == NULL)
		return CSS_OK;

	v = dom_string_data(value);
	vlen = dom_string_byte_length(value);
	w = lwc_string_data(want);
	wlen = lwc_string_length(want);

	/* Naive, and deliberately: the alternative is a Boyer-Moore over a needle that is almost
	 * always under ten bytes, and *= appears at most a handful of times in a sheet. */
	for (i = 0; wlen > 0 && i + wlen <= vlen; i++) {
		if (strncasecmp(v + i, w, wlen) == 0) {
			*match = true;
			break;
		}
	}

	dom_string_unref(value);
	return CSS_OK;
}

static css_error node_is_root(void *pw, void *node, bool *match)
{
	void *parent = NULL;
	css_error error = parent_node(pw, node, &parent);

	if (error != CSS_OK)
		return error;
	*match = parent == NULL;
	return CSS_OK;
}

static css_error node_count_siblings(void *pw, void *node, bool same_name, bool after,
		int32_t *count)
{
	dom_string *name = NULL;
	void *cur;
	int32_t n = 0;

	(void) pw;
	*count = 0;

	if (same_name) {
		name = element_name(node);
		if (name == NULL)
			return CSS_NOMEM;
	}

	for (cur = element_sibling(node, after); cur != NULL;
			cur = element_sibling(cur, after)) {
		if (name != NULL) {
			dom_string *other = element_name(cur);
			bool eq;

			if (other == NULL) {
				dom_string_unref(name);
				return CSS_NOMEM;
			}
			eq = dom_string_caseless_isequal(name, other);
			dom_string_unref(other);
			if (!eq)
				continue;
		}
		n++;
	}

	if (name != NULL)
		dom_string_unref(name);
	*count = n;
	return CSS_OK;
}

/* :empty — no element children and no text at all, whitespace included. That is the CSS 2.1
 * definition and it is the one that matters: a `<td>\n</td>` is not empty. */
static css_error node_is_empty(void *pw, void *node, bool *match)
{
	dom_node *child = NULL;

	(void) pw;
	*match = true;

	if (dom_node_get_first_child((dom_node *) node, &child) != DOM_NO_ERR)
		return CSS_NOMEM;

	while (child != NULL) {
		dom_node_type type;
		dom_node *next = NULL;

		if (dom_node_get_node_type(child, &type) != DOM_NO_ERR) {
			dom_node_unref(child);
			return CSS_NOMEM;
		}

		if (type == DOM_ELEMENT_NODE) {
			*match = false;
		} else if (type == DOM_TEXT_NODE || type == DOM_CDATA_SECTION_NODE) {
			dom_string *text = NULL;

			if (dom_node_get_text_content(child, &text) == DOM_NO_ERR &&
					text != NULL) {
				if (dom_string_byte_length(text) > 0)
					*match = false;
				dom_string_unref(text);
			}
		}

		if (*match == false) {
			dom_node_unref(child);
			return CSS_OK;
		}

		if (dom_node_get_next_sibling(child, &next) != DOM_NO_ERR) {
			dom_node_unref(child);
			return CSS_NOMEM;
		}
		dom_node_unref(child);
		child = next;
	}

	return CSS_OK;
}

static css_error node_is_link(void *pw, void *node, bool *match)
{
	struct dom_select *sel = pw;
	dom_string *href;

	*match = false;

	if (!name_is(node, sel->e_a) && !name_is(node, sel->e_area) &&
			!name_is(node, sel->e_link))
		return CSS_OK;

	href = attr(node, sel->a_href);
	if (href != NULL) {
		*match = true;
		dom_string_unref(href);
	}
	return CSS_OK;
}

/* The five interaction pseudo-classes.
 *
 * All false, and not as a placeholder. dom_build is one shot: it is handed bytes and returns a
 * styled tree, with no pointer, no focus ring and no history on this side of the bridge. A
 * :hover rule that matched here would be wrong for the whole life of the page rather than
 * merely late. Whoever adds interaction adds the state to dom_select first. */
static css_error node_is_visited(void *pw, void *node, bool *match)
{
	(void) pw; (void) node;
	*match = false;
	return CSS_OK;
}

static css_error node_is_hover(void *pw, void *node, bool *match)
{
	(void) pw; (void) node;
	*match = false;
	return CSS_OK;
}

static css_error node_is_active(void *pw, void *node, bool *match)
{
	(void) pw; (void) node;
	*match = false;
	return CSS_OK;
}

static css_error node_is_focus(void *pw, void *node, bool *match)
{
	(void) pw; (void) node;
	*match = false;
	return CSS_OK;
}

/* :target needs the document's fragment identifier, which is part of the URL and the URL is not
 * passed across dom_build. False until it is. */
static css_error node_is_target(void *pw, void *node, bool *match)
{
	(void) pw; (void) node;
	*match = false;
	return CSS_OK;
}

/* A form control is disabled if it carries the attribute, or if any <fieldset> or <optgroup>
 * ancestor does. The ancestor walk is what makes this worth implementing at all: a page that
 * greys out a whole fieldset does it on the fieldset. */
static bool control_is_disabled(struct dom_select *sel, void *node)
{
	dom_node *cur = dom_node_ref((dom_node *) node);

	while (cur != NULL) {
		dom_node *parent = NULL;
		dom_string *value;

		if (!is_element(cur)) {
			dom_node_unref(cur);
			return false;
		}

		value = attr(cur, sel->a_disabled);
		if (value != NULL) {
			dom_string_unref(value);
			dom_node_unref(cur);
			return true;
		}

		if (dom_node_get_parent_node(cur, &parent) != DOM_NO_ERR) {
			dom_node_unref(cur);
			return false;
		}
		dom_node_unref(cur);
		cur = parent;
	}
	return false;
}

static bool is_form_control(struct dom_select *sel, void *node)
{
	return name_is(node, sel->e_input) || name_is(node, sel->e_button) ||
			name_is(node, sel->e_select) || name_is(node, sel->e_textarea) ||
			name_is(node, sel->e_optgroup) || name_is(node, sel->e_option) ||
			name_is(node, sel->e_fieldset);
}

static css_error node_is_enabled(void *pw, void *node, bool *match)
{
	struct dom_select *sel = pw;

	/* :enabled is not simply !:disabled — it applies only to elements that can be disabled,
	 * so a <div> matches neither. */
	*match = is_form_control(sel, node) && !control_is_disabled(sel, node);
	return CSS_OK;
}

static css_error node_is_disabled(void *pw, void *node, bool *match)
{
	struct dom_select *sel = pw;

	*match = is_form_control(sel, node) && control_is_disabled(sel, node);
	return CSS_OK;
}

/* The parse-time attribute only. A checkbox the user has since clicked is not visible from here
 * for the same reason :hover is not. */
static css_error node_is_checked(void *pw, void *node, bool *match)
{
	struct dom_select *sel = pw;
	dom_string *value = attr(node, sel->a_checked);

	*match = value != NULL;
	if (value != NULL)
		dom_string_unref(value);
	return CSS_OK;
}

static css_error node_is_lang(void *pw, void *node, lwc_string *lang, bool *match)
{
	struct dom_select *sel = pw;
	dom_node *cur = dom_node_ref((dom_node *) node);

	*match = false;

	/* :lang() inherits down the tree even though `lang` is a plain attribute, so this is the
	 * nearest ancestor-or-self that declares one — not the node's own attribute. */
	while (cur != NULL) {
		dom_node *parent = NULL;
		dom_string *value;

		if (!is_element(cur))
			break;

		value = attr(cur, sel->a_lang);
		if (value != NULL) {
			const char *v = dom_string_data(value);
			size_t vlen = dom_string_byte_length(value);
			size_t wlen = lwc_string_length(lang);

			if (wlen > 0 && vlen >= wlen &&
					strncasecmp(v, lwc_string_data(lang), wlen) == 0 &&
					(vlen == wlen || v[wlen] == '-'))
				*match = true;

			dom_string_unref(value);
			break;
		}

		if (dom_node_get_parent_node(cur, &parent) != DOM_NO_ERR)
			break;
		dom_node_unref(cur);
		cur = parent;
	}

	dom_node_unref(cur);
	return CSS_OK;
}

/* ------------------------------------------------------------- presentational hints --
 *
 * Where HTML's own formatting attributes become CSS. This is the callback that decides whether a
 * 1998 page looks like anything at all, so it is the one place in the handler with real
 * judgement in it rather than a DOM query.
 *
 * What is covered: bgcolor (any element), text (body), color (font/basefont), width and height,
 * align, <center>, nowrap, hspace and vspace.
 *
 * What is not, and why:
 *  - `align=middle|top|bottom` on <img> would be vertical-align, which the layout in F6 does not
 *    have a box model for yet. Emitting it would be a value nobody reads.
 *  - `align` on a block emits plain CSS_TEXT_ALIGN_CENTER, not libcss's CSS_TEXT_ALIGN_LIBCSS_*
 *    magic values. Those exist so that an align= on a block does not leak into a nested table
 *    (select/properties/text_align.c:86, CSS_TEXT_ALIGN_INHERIT_IF_NON_MAGIC), which needs a
 *    matching hint on every <table> to work. Without the table model there is nothing for the
 *    magic to protect, and the plain value is what the layout can read.
 *  - `<font size>` is a step on HTML's 1..7 scale, which is a font-size table this file would
 *    have to invent. It belongs with whoever picks the fonts.
 *  - `border`, `cellpadding`, `cellspacing`: table properties, same reason as vertical-align.
 */

static void hint_color(css_hint *hint, uint32_t prop, css_color color)
{
	hint->prop = prop;
	hint->status = CSS_BACKGROUND_COLOR_COLOR;   /* == CSS_COLOR_COLOR == 1 for both */
	hint->data.color = color;
}

static void hint_length(css_hint *hint, uint32_t prop, uint8_t status,
		const css_hint_length *length)
{
	hint->prop = prop;
	hint->status = status;
	hint->data.length = *length;
}

static css_error node_presentational_hint(void *pw, void *node, uint32_t *nhints,
		css_hint **hints)
{
	struct dom_select *sel = pw;
	css_hint *h = sel->hints;
	uint32_t n = 0;
	dom_string *value;
	css_color color;
	css_hint_length length;
	bool is_body, is_cell, is_replaced;

	*nhints = 0;
	*hints = NULL;

	is_body = name_is(node, sel->e_body);
	is_cell = name_is(node, sel->e_td) || name_is(node, sel->e_th);
	is_replaced = name_is(node, sel->e_img) || name_is(node, sel->e_object) ||
			name_is(node, sel->e_applet) || name_is(node, sel->e_iframe) ||
			name_is(node, sel->e_input);

	value = attr(node, sel->a_bgcolor);
	if (value != NULL) {
		if (parse_html_color(dom_string_data(value), dom_string_byte_length(value),
				&color))
			hint_color(&h[n++], CSS_PROP_BACKGROUND_COLOR, color);
		dom_string_unref(value);
	}

	/* body text= and font color= are the same hint on different attributes; the link=, vlink=
	 * and alink= siblings of text= are not here, because they set a colour on descendants
	 * rather than on the element, which is a stylesheet rule and not a hint. */
	value = attr(node, is_body ? sel->a_text : sel->a_color);
	if (value != NULL) {
		bool wanted = is_body || name_is(node, sel->e_font) ||
				name_is(node, sel->e_basefont);

		if (wanted && parse_html_color(dom_string_data(value),
				dom_string_byte_length(value), &color))
			hint_color(&h[n++], CSS_PROP_COLOR, color);
		dom_string_unref(value);
	}

	value = attr(node, sel->a_width);
	if (value != NULL) {
		if (parse_html_length(dom_string_data(value), dom_string_byte_length(value),
				&length))
			hint_length(&h[n++], CSS_PROP_WIDTH, CSS_WIDTH_SET, &length);
		dom_string_unref(value);
	}

	value = attr(node, sel->a_height);
	if (value != NULL) {
		if (parse_html_length(dom_string_data(value), dom_string_byte_length(value),
				&length))
			hint_length(&h[n++], CSS_PROP_HEIGHT, CSS_HEIGHT_SET, &length);
		dom_string_unref(value);
	}

	value = attr(node, sel->a_align);
	if (value != NULL) {
		const char *a = dom_string_data(value);
		size_t alen = dom_string_byte_length(value);
		bool left = alen == 4 && strncasecmp(a, "left", 4) == 0;
		bool right = alen == 5 && strncasecmp(a, "right", 5) == 0;
		bool center = (alen == 6 && strncasecmp(a, "center", 6) == 0) ||
				(alen == 6 && strncasecmp(a, "centre", 6) == 0);

		if (is_replaced && (left || right)) {
			/* On a replaced element align= floats the box. */
			h[n].prop = CSS_PROP_FLOAT;
			h[n].status = left ? CSS_FLOAT_LEFT : CSS_FLOAT_RIGHT;
			n++;
		} else if (left || right || center) {
			h[n].prop = CSS_PROP_TEXT_ALIGN;
			h[n].status = left ? CSS_TEXT_ALIGN_LEFT :
					right ? CSS_TEXT_ALIGN_RIGHT : CSS_TEXT_ALIGN_CENTER;
			n++;
		}
		dom_string_unref(value);
	}

	if (name_is(node, sel->e_center)) {
		h[n].prop = CSS_PROP_TEXT_ALIGN;
		h[n].status = CSS_TEXT_ALIGN_CENTER;
		n++;
	}

	if (is_cell) {
		value = attr(node, sel->a_nowrap);
		if (value != NULL) {
			h[n].prop = CSS_PROP_WHITE_SPACE;
			h[n].status = CSS_WHITE_SPACE_NOWRAP;
			n++;
			dom_string_unref(value);
		}
	}

	value = attr(node, sel->a_hspace);
	if (value != NULL) {
		if (is_replaced && parse_html_length(dom_string_data(value),
				dom_string_byte_length(value), &length)) {
			hint_length(&h[n++], CSS_PROP_MARGIN_LEFT, CSS_MARGIN_SET, &length);
			hint_length(&h[n++], CSS_PROP_MARGIN_RIGHT, CSS_MARGIN_SET, &length);
		}
		dom_string_unref(value);
	}

	value = attr(node, sel->a_vspace);
	if (value != NULL) {
		if (is_replaced && parse_html_length(dom_string_data(value),
				dom_string_byte_length(value), &length)) {
			hint_length(&h[n++], CSS_PROP_MARGIN_TOP, CSS_MARGIN_SET, &length);
			hint_length(&h[n++], CSS_PROP_MARGIN_BOTTOM, CSS_MARGIN_SET, &length);
		}
		dom_string_unref(value);
	}

	if (n > 0) {
		*nhints = n;
		*hints = h;
	}
	return CSS_OK;
}

/* ------------------------------------------------------------------- the UA defaults --
 *
 * Only three properties reach here: libcss calls this from css__initial_color,
 * css__initial_font_family and css__initial_quotes and nowhere else. Anything else is a libcss
 * change and should fail loudly rather than return a plausible zero.
 */
static css_error ua_default_for_property(void *pw, uint32_t property, css_hint *hint)
{
	(void) pw;

	switch (property) {
	case CSS_PROP_COLOR:
		hint->prop = property;
		hint->status = CSS_COLOR_COLOR;
		/* Black. Not the palette's `text`: the palette is a dom_build argument and this
		 * context is built before it is known, so dom_bridge.c substitutes. */
		hint->data.color = 0xff000000;
		return CSS_OK;

	case CSS_PROP_FONT_FAMILY:
		hint->prop = property;
		/* Sans-serif, and no named list: this handset has one usable proportional face
		 * and one monospace one, so a family list would be answered by the same font.
		 * data.strings NULL is the documented "no names" case
		 * (css__set_font_family_from_hint tolerates it). */
		hint->status = CSS_FONT_FAMILY_SANS_SERIF;
		hint->data.strings = NULL;
		return CSS_OK;

	case CSS_PROP_QUOTES:
		hint->prop = property;
		hint->status = CSS_QUOTES_NONE;
		hint->data.strings = NULL;
		return CSS_OK;

	default:
		break;
	}

	return CSS_INVALID;
}

/* ----------------------------------------------------- libcss's per-node selector cache --
 *
 * These two are not optional and they are not a cache we own: libcss stores the node's bloom
 * filter, its pseudo-class flags and its shareable partial style behind them, and a select that
 * cannot store them still works but re-derives the ancestor bloom for every node.
 *
 * Getting them wrong leaks one struct per element. On this platform Open C's malloc is the
 * process heap and __UHEAP_MARKEND is armed in the probe, so that leak is a panic rather than a
 * number in a report — hence the user-data handler below rather than a list of our own to walk
 * at teardown. libdom fires it from _dom_node_finalise for every node it destroys, so the data
 * dies with the document whichever of the two outlives the other.
 */
static css_select_handler select_handler;      /* defined below; the trampoline needs its address */

static void node_data_trampoline(dom_node_operation op, dom_string *key, void *data,
		struct dom_node *src, struct dom_node *dst)
{
	(void) key;
	(void) dst;

	if (data == NULL)
		return;

	switch (op) {
	case DOM_NODE_DELETED:
		/* Frees the node data. Does not call back into set_libcss_node_data, which is why
		 * this path needs neither the key nor a client word. */
		css_libcss_node_data_handler(&select_handler, CSS_NODE_DELETED,
				NULL, NULL, NULL, data);
		break;

	case DOM_NODE_RENAMED:
		/* The node's name changed, so its selector match is stale. libcss frees the data
		 * and asks us to drop our reference to it, which is what pw = NULL is safe for:
		 * set_libcss_node_data below never reads pw. */
		css_libcss_node_data_handler(&select_handler, CSS_NODE_MODIFIED,
				NULL, src, NULL, data);
		break;

	case DOM_NODE_ADOPTED:
	case DOM_NODE_IMPORTED:
		/* The subtree moved, so every descendant's ancestor bloom is wrong. */
		css_libcss_node_data_handler(&select_handler, CSS_NODE_ANCESTORS_MODIFIED,
				NULL, src, NULL, data);
		break;

	case DOM_NODE_CLONED:
		/* A no-op inside libcss, deliberately: the clone's ancestors are different, so
		 * the cached data would be wrong. It matters only that the clone does not end up
		 * pointing at the original's data, and libdom does not copy user data unless a
		 * handler does it — so doing nothing here is the whole fix. */
		css_libcss_node_data_handler(&select_handler, CSS_NODE_CLONED,
				NULL, src, dst, data);
		break;

	default:
		break;
	}
}

static css_error set_libcss_node_data(void *pw, void *node, void *libcss_node_data)
{
	void *previous = NULL;

	(void) pw;

	if (node_data_key == NULL)
		return CSS_NOMEM;

	/* _dom_node_set_user_data dereferences `result` unconditionally, including on the
	 * remove path, so it may not be NULL. */
	if (dom_node_set_user_data((dom_node *) node, node_data_key, libcss_node_data,
			node_data_trampoline, &previous) != DOM_NO_ERR)
		return CSS_NOMEM;

	/* libcss replaces data by calling here with the new pointer, and expects the old one to
	 * be released rather than handed back. */
	if (previous != NULL && previous != libcss_node_data)
		css_libcss_node_data_handler(&select_handler, CSS_NODE_DELETED,
				NULL, NULL, NULL, previous);

	return CSS_OK;
}

static css_error get_libcss_node_data(void *pw, void *node, void **libcss_node_data)
{
	(void) pw;

	*libcss_node_data = NULL;
	if (node_data_key == NULL)
		return CSS_OK;

	if (dom_node_get_user_data((dom_node *) node, node_data_key,
			libcss_node_data) != DOM_NO_ERR) {
		*libcss_node_data = NULL;
		return CSS_NOMEM;
	}
	return CSS_OK;
}

/* -------------------------------------------------------------------- the handler table -- */

static css_select_handler select_handler = {
	CSS_SELECT_HANDLER_VERSION_1,

	node_name,
	node_classes,
	node_id,

	named_ancestor_node,
	named_parent_node,
	named_sibling_node,
	named_generic_sibling_node,

	parent_node,
	sibling_node,

	node_has_name,
	node_has_class,
	node_has_id,
	node_has_attribute,
	node_has_attribute_equal,
	node_has_attribute_dashmatch,
	node_has_attribute_includes,
	node_has_attribute_prefix,
	node_has_attribute_suffix,
	node_has_attribute_substring,

	node_is_root,
	node_count_siblings,
	node_is_empty,

	node_is_link,
	node_is_visited,
	node_is_hover,
	node_is_active,
	node_is_focus,

	node_is_enabled,
	node_is_disabled,
	node_is_checked,

	node_is_target,
	node_is_lang,

	node_presentational_hint,

	ua_default_for_property,

	set_libcss_node_data,
	get_libcss_node_data
};

/* ------------------------------------------------------------------------- construction -- */

static bool intern_dom(const char *s, dom_string **out)
{
	return dom_string_create_interned((const uint8_t *) s, strlen(s), out) == DOM_NO_ERR;
}

static bool intern_lwc(const char *s, lwc_string **out)
{
	return lwc_intern_string(s, strlen(s), out) == lwc_error_ok;
}

static void release_strings(struct dom_select *sel)
{
	dom_string **d[] = {
		&sel->a_id, &sel->a_class, &sel->a_bgcolor, &sel->a_text, &sel->a_color,
		&sel->a_width, &sel->a_height, &sel->a_align, &sel->a_nowrap, &sel->a_hspace,
		&sel->a_vspace, &sel->a_href, &sel->a_disabled, &sel->a_checked, &sel->a_lang
	};
	lwc_string **l[] = {
		&sel->e_center, &sel->e_body, &sel->e_font, &sel->e_basefont, &sel->e_a,
		&sel->e_area, &sel->e_link, &sel->e_td, &sel->e_th, &sel->e_img,
		&sel->e_object, &sel->e_applet, &sel->e_iframe, &sel->e_input, &sel->e_button,
		&sel->e_select, &sel->e_textarea, &sel->e_optgroup, &sel->e_option,
		&sel->e_fieldset, &sel->e_style
	};
	size_t i;

	for (i = 0; i < sizeof(d) / sizeof(d[0]); i++) {
		if (*d[i] != NULL) {
			dom_string_unref(*d[i]);
			*d[i] = NULL;
		}
	}
	for (i = 0; i < sizeof(l) / sizeof(l[0]); i++) {
		if (*l[i] != NULL) {
			lwc_string_unref(*l[i]);
			*l[i] = NULL;
		}
	}
}

static bool intern_strings(struct dom_select *sel)
{
	return intern_dom("id", &sel->a_id) &&
		intern_dom("class", &sel->a_class) &&
		intern_dom("bgcolor", &sel->a_bgcolor) &&
		intern_dom("text", &sel->a_text) &&
		intern_dom("color", &sel->a_color) &&
		intern_dom("width", &sel->a_width) &&
		intern_dom("height", &sel->a_height) &&
		intern_dom("align", &sel->a_align) &&
		intern_dom("nowrap", &sel->a_nowrap) &&
		intern_dom("hspace", &sel->a_hspace) &&
		intern_dom("vspace", &sel->a_vspace) &&
		intern_dom("href", &sel->a_href) &&
		intern_dom("disabled", &sel->a_disabled) &&
		intern_dom("checked", &sel->a_checked) &&
		intern_dom("lang", &sel->a_lang) &&
		intern_lwc("center", &sel->e_center) &&
		intern_lwc("body", &sel->e_body) &&
		intern_lwc("font", &sel->e_font) &&
		intern_lwc("basefont", &sel->e_basefont) &&
		intern_lwc("a", &sel->e_a) &&
		intern_lwc("area", &sel->e_area) &&
		intern_lwc("link", &sel->e_link) &&
		intern_lwc("td", &sel->e_td) &&
		intern_lwc("th", &sel->e_th) &&
		intern_lwc("img", &sel->e_img) &&
		intern_lwc("object", &sel->e_object) &&
		intern_lwc("applet", &sel->e_applet) &&
		intern_lwc("iframe", &sel->e_iframe) &&
		intern_lwc("input", &sel->e_input) &&
		intern_lwc("button", &sel->e_button) &&
		intern_lwc("select", &sel->e_select) &&
		intern_lwc("textarea", &sel->e_textarea) &&
		intern_lwc("optgroup", &sel->e_optgroup) &&
		intern_lwc("option", &sel->e_option) &&
		intern_lwc("fieldset", &sel->e_fieldset) &&
		intern_lwc("style", &sel->e_style);
}

/* The E72's panel is 320x240 with a physical diagonal of 2.36", which is about 169 dpi.
 * device_dpi is nonetheless 96, i.e. one CSS pixel to one device pixel. At the true dpi a
 * spec-default 16px font computes to 28 device pixels and a 240px-wide column fits about eight
 * characters, which is not a browser. Scaling belongs to whoever picks the font, not to the unit
 * conversion, and 96 is the identity that keeps it there. */
static void init_media(struct dom_select *sel)
{
	memset(&sel->media, 0, sizeof(sel->media));
	sel->media.type = CSS_MEDIA_SCREEN;
	sel->media.width = INTTOFIX(320);
	sel->media.height = INTTOFIX(240);
	sel->media.aspect_ratio = FDIV(INTTOFIX(320), INTTOFIX(240));
	sel->media.orientation = CSS_MEDIA_ORIENTATION_LANDSCAPE;
	sel->media.scan = CSS_MEDIA_SCAN_PROGRESSIVE;
	sel->media.grid = 0;
	sel->media.update = CSS_MEDIA_UPDATE_FREQUENCY_NORMAL;
	sel->media.overflow_block = CSS_MEDIA_OVERFLOW_BLOCK_SCROLL;
	sel->media.overflow_inline = CSS_MEDIA_OVERFLOW_INLINE_NONE;
	/* Bits per colour component: the panel is 16-bit 565, so five. `monochrome` stays 0,
	 * which is what a colour screen reports. `resolution` is left zero because this libcss
	 * never reads css_media.resolution — nothing in src/select consults it. */
	sel->media.color = INTTOFIX(5);
	sel->media.color_index = 0;
	sel->media.monochrome = 0;
	sel->media.inverted_colors = 0;
	sel->media.prefers_color_scheme = NULL;
	/* A five-way key and a keypad: no pointer at all, and hover is a state the hardware
	 * cannot report. This is what makes `@media (hover: none)` rules apply, which is the
	 * closest a 2009 handset gets to being served the mobile layout. */
	sel->media.pointer = CSS_MEDIA_POINTER_NONE;
	sel->media.any_pointer = CSS_MEDIA_POINTER_NONE;
	sel->media.hover = CSS_MEDIA_HOVER_NONE;
	sel->media.any_hover = CSS_MEDIA_HOVER_NONE;
	sel->media.light_level = CSS_MEDIA_LIGHT_LEVEL_NORMAL;
	sel->media.scripting = CSS_MEDIA_SCRIPTING_NONE;

	memset(&sel->units, 0, sizeof(sel->units));
	sel->units.viewport_width = INTTOFIX(320);
	sel->units.viewport_height = INTTOFIX(240);
	/* The body atlas, not the CSS `medium` of 16 px.
	 *
	 * 16 is what a desktop browser means by "medium", and inheriting it here made *every* element
	 * without an explicit font-size compute to 16 px — which the role classifier reads as a heading,
	 * because 16 is well past the body atlas. The whole web rendered in the title face. Reported by
	 * eye ("os textos estão bold o tempo todo"), which is the only way a bug like this shows up: it
	 * breaks nothing, it just makes every page wrong in the same way.
	 *
	 * The number has to be the body atlas's height, because that is what one em can mean when there
	 * are four atlases and no scaler. */
	sel->units.font_size_default = INTTOFIX(11);
	/* Zero, which libcss documents as "no minimum". A floor here would silently override
	 * every author's small print, and that is a rendering policy rather than a unit. */
	sel->units.font_size_minimum = 0;
	sel->units.device_dpi = F_96;
	sel->units.root_style = NULL;      /* filled in once the root has been composed */
	sel->units.pw = NULL;
	/* No measure callback: libcss then derives ex and ch from em by a fixed ratio, which is
	 * the right answer here because the font is not chosen on this side of the bridge. */
}

static css_error make_sheet(const char *url, const uint8_t *data, size_t len,
		css_stylesheet **out)
{
	css_stylesheet_params params;
	css_stylesheet *sheet = NULL;
	css_error error;

	memset(&params, 0, sizeof(params));
	params.params_version = CSS_STYLESHEET_PARAMS_VERSION_1;
	params.level = CSS_LEVEL_DEFAULT;
	params.charset = "UTF-8";
	params.url = url;
	params.title = NULL;
	params.allow_quirks = false;
	params.inline_style = false;
	params.resolve = resolve_url;

	error = css_stylesheet_create(&params, &sheet);
	if (error != CSS_OK)
		return error;

	error = css_stylesheet_append_data(sheet, data, len);
	/* CSS_NEEDDATA is append's success reply: it consumed what it was given and wants more.
	 * Reading it as failure is the mistake this comment exists to stop. */
	if (error != CSS_OK && error != CSS_NEEDDATA) {
		css_stylesheet_destroy(sheet);
		return error;
	}

	error = css_stylesheet_data_done(sheet);
	if (error != CSS_OK) {
		css_stylesheet_destroy(sheet);
		return error;
	}

	*out = sheet;
	return CSS_OK;
}

/* The next node in document order, borrowed in and a new reference out. Iterative because the
 * only bound on a document's nesting depth is the tokeniser's, and this runs on a worker thread
 * whose stack is not the main thread's. */
static dom_exception preorder_next(dom_node *node, dom_node **out)
{
	dom_node *child = NULL;
	dom_node *up;

	*out = NULL;

	if (dom_node_get_first_child(node, &child) != DOM_NO_ERR)
		return DOM_NO_MEM_ERR;
	if (child != NULL) {
		*out = child;
		return DOM_NO_ERR;
	}

	up = dom_node_ref(node);
	while (up != NULL) {
		dom_node *sib = NULL;
		dom_node *parent = NULL;

		if (dom_node_get_next_sibling(up, &sib) != DOM_NO_ERR) {
			dom_node_unref(up);
			return DOM_NO_MEM_ERR;
		}
		if (sib != NULL) {
			dom_node_unref(up);
			*out = sib;
			return DOM_NO_ERR;
		}
		if (dom_node_get_parent_node(up, &parent) != DOM_NO_ERR) {
			dom_node_unref(up);
			return DOM_NO_MEM_ERR;
		}
		dom_node_unref(up);
		up = parent;
	}

	return DOM_NO_ERR;
}

/* Every <style> block in the document, in document order, appended as CSS_ORIGIN_AUTHOR.
 *
 * The tree is walked rather than queried with get_elements_by_tag_name because that call
 * compares node names exactly, and an HTML document stores them upper-cased — so the obvious
 * "style" query returns nothing and the obvious "STYLE" query is a bet on the parser's casing.
 * A caseless walk is the same amount of code and does not have to be right about that.
 *
 * A failure to parse one block is not a failure of the document: a page with a broken <style>
 * and three good ones renders with three. Only running out of memory stops the walk. */
static css_error collect_author_sheets(struct dom_select *sel)
{
	dom_node *node = NULL;
	css_error result = CSS_OK;
	uint32_t cap = 0;

	if (dom_document_get_document_element(sel->doc, &node) != DOM_NO_ERR)
		return CSS_OK;

	while (node != NULL) {
		dom_node *next = NULL;

		if (is_element(node) && name_is(node, sel->e_style)) {
			dom_string *text = NULL;

			if (dom_node_get_text_content(node, &text) == DOM_NO_ERR &&
					text != NULL) {
				if (dom_string_byte_length(text) > 0) {
					css_stylesheet *sheet = NULL;
					css_error e = make_sheet("about:inline",
							(const uint8_t *) dom_string_data(text),
							dom_string_byte_length(text), &sheet);

					if (e == CSS_NOMEM) {
						dom_string_unref(text);
						result = CSS_NOMEM;
						break;
					}
					if (e == CSS_OK) {
						if (sel->n_author == cap) {
							uint32_t grown = cap == 0 ? 4 : cap * 2;
							css_stylesheet **t = realloc(sel->author,
									grown * sizeof(*t));

							if (t == NULL) {
								css_stylesheet_destroy(sheet);
								dom_string_unref(text);
								result = CSS_NOMEM;
								break;
							}
							sel->author = t;
							cap = grown;
						}
						sel->author[sel->n_author++] = sheet;

						e = css_select_ctx_append_sheet(sel->ctx, sheet,
								CSS_ORIGIN_AUTHOR, NULL);
						if (e != CSS_OK) {
							dom_string_unref(text);
							result = e;
							break;
						}
					}
				}
				dom_string_unref(text);
			}
		}

		if (preorder_next(node, &next) != DOM_NO_ERR) {
			dom_node_unref(node);
			return CSS_NOMEM;
		}
		dom_node_unref(node);
		node = next;
	}

	if (node != NULL)
		dom_node_unref(node);
	return result;
}

struct dom_select *dom_select_create(void *doc)
{
	struct dom_select *sel;

	if (doc == NULL)
		return NULL;

	sel = calloc(1, sizeof(*sel));
	if (sel == NULL)
		return NULL;

	sel->doc = doc;
	init_media(sel);

	if (!intern_strings(sel))
		goto fail;

	if (node_data_key == NULL &&
			!intern_dom("libcss-node-data", &node_data_key))
		goto fail;
	node_data_key_users++;
	sel->holds_key = true;

	if (css_select_ctx_create(&sel->ctx) != CSS_OK)
		goto fail;

	if (make_sheet("about:ua", (const uint8_t *) ua_stylesheet,
			sizeof(ua_stylesheet) - 1, &sel->ua) != CSS_OK)
		goto fail;

	/* The UA sheet must be sheet 0: css_select_style walks ctx->sheets in order and uses the
	 * first sheet's origin to seed its revert bookkeeping. */
	if (css_select_ctx_append_sheet(sel->ctx, sel->ua, CSS_ORIGIN_UA, NULL) != CSS_OK)
		goto fail;

	if (collect_author_sheets(sel) != CSS_OK)
		goto fail;

	return sel;

fail:
	dom_select_destroy(sel);
	return NULL;
}

void dom_select_destroy(struct dom_select *sel)
{
	uint32_t i;

	if (sel == NULL)
		return;

	for (i = 0; i < sel->n_chain; i++)
		css_computed_style_destroy(sel->chain[i].style);
	free(sel->chain);
	free(sel->path);

	if (sel->ctx != NULL)
		css_select_ctx_destroy(sel->ctx);

	/* After the context, which holds borrowed pointers to all of them. */
	for (i = 0; i < sel->n_author; i++)
		css_stylesheet_destroy(sel->author[i]);
	free(sel->author);
	if (sel->ua != NULL)
		css_stylesheet_destroy(sel->ua);

	release_strings(sel);

	if (sel->holds_key && --node_data_key_users == 0 && node_data_key != NULL) {
		dom_string_unref(node_data_key);
		node_data_key = NULL;
	}

	free(sel);
}

/* --------------------------------------------------------------------------- selection -- */

static bool chain_reserve(struct dom_select *sel, uint32_t want)
{
	chain_entry *grown;

	if (want <= sel->chain_cap)
		return true;

	grown = realloc(sel->chain, want * sizeof(*grown));
	if (grown == NULL)
		return false;
	sel->chain = grown;
	sel->chain_cap = want;
	return true;
}

static bool path_reserve(struct dom_select *sel, uint32_t want)
{
	void **grown;

	if (want <= sel->path_cap)
		return true;

	grown = realloc(sel->path, want * sizeof(*grown));
	if (grown == NULL)
		return false;
	sel->path = grown;
	sel->path_cap = want;
	return true;
}

static void chain_truncate(struct dom_select *sel, uint32_t keep)
{
	while (sel->n_chain > keep) {
		sel->n_chain--;
		css_computed_style_destroy(sel->chain[sel->n_chain].style);
		sel->chain[sel->n_chain].style = NULL;
		sel->chain[sel->n_chain].node = NULL;
	}
	if (keep == 0)
		sel->units.root_style = NULL;
}

/* Select `node` and compose it onto `parent_style`, pushing the result onto the chain. */
static css_error chain_push(struct dom_select *sel, void *node,
		const css_computed_style *parent_style)
{
	css_select_results *results = NULL;
	css_computed_style *composed = NULL;
	css_error error;

	error = css_select_style(sel->ctx, node, &sel->units, &sel->media, NULL,
			&select_handler, sel, &results);
	if (error != CSS_OK)
		return error;

	/* The partial style is consumed here and freed here. The header's contract — one
	 * css_select_results at a time — is met by never letting a second one exist rather than
	 * by remembering to free the previous one. */
	error = css_computed_style_compose(parent_style,
			results->styles[CSS_PSEUDO_ELEMENT_NONE], &sel->units, &composed);
	css_select_results_destroy(results);
	if (error != CSS_OK)
		return error;

	if (!chain_reserve(sel, sel->n_chain + 1)) {
		css_computed_style_destroy(composed);
		return CSS_NOMEM;
	}

	sel->chain[sel->n_chain].node = node;
	sel->chain[sel->n_chain].style = composed;
	sel->n_chain++;

	/* rem resolves against the root element's style, so it exists only after the root has
	 * been composed. Setting it any earlier would make rem mean em. */
	if (sel->n_chain == 1)
		sel->units.root_style = composed;

	return CSS_OK;
}

void *dom_select_style(struct dom_select *sel, void *element)
{
	uint32_t depth = 0;
	uint32_t common = 0;
	uint32_t i;
	void *cur;

	if (sel == NULL || element == NULL || !is_element(element))
		return NULL;

	/* The root-down path to `element`, built by walking up. dom_element_parent_node returns
	 * a reference; parent_node() drops it, and the tree keeps the node alive. */
	cur = element;
	while (cur != NULL) {
		void *parent = NULL;

		if (!path_reserve(sel, depth + 1))
			return NULL;
		sel->path[depth++] = cur;

		if (parent_node(sel, cur, &parent) != CSS_OK)
			return NULL;
		cur = parent;
	}

	/* path is deepest-first; reverse it in place so index 0 is the root. */
	for (i = 0; i < depth / 2; i++) {
		void *swap = sel->path[i];

		sel->path[i] = sel->path[depth - 1 - i];
		sel->path[depth - 1 - i] = swap;
	}

	while (common < depth && common < sel->n_chain &&
			sel->chain[common].node == sel->path[common])
		common++;

	chain_truncate(sel, common);

	for (i = common; i < depth; i++) {
		const css_computed_style *parent_style =
				sel->n_chain > 0 ? sel->chain[sel->n_chain - 1].style : NULL;

		if (chain_push(sel, sel->path[i], parent_style) != CSS_OK) {
			/* A partial chain is not a usable chain: the next call would compose a
			 * child onto an ancestor that was never selected. */
			chain_truncate(sel, 0);
			return NULL;
		}
	}

	return sel->chain[sel->n_chain - 1].style;
}
