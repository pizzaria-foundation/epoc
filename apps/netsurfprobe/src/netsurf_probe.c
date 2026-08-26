/* The half of netsurfprobe that talks to the NetSurf libraries — the only file in this
 * repository that includes their headers, and by construction the only one that can be.
 * See inc/netsurf_probe.h for why.
 *
 * No Symbian header appears here and none may: e32base.h is C++. This file sees exactly
 * the five libraries and Open C, which is also the honest shape of the dependency — the
 * libraries know nothing about Symbian and want nothing from it beyond libc.
 *
 * What each probe is chosen to exercise is noted at the probe. The bias is towards calls
 * that allocate and free, because R1 in docs/plan-browser.md is about the C runtime, not
 * about the parsers: a library whose malloc works is a library that links.
 */

#include <stddef.h>
#include <string.h>

#include <libwapcaplet/libwapcaplet.h>
#include <parserutils/charset/mibenum.h>
#include <parserutils/input/inputstream.h>
#include <hubbub/parser.h>
#include <dom/dom.h>
#include <dom/bindings/hubbub/parser.h>
#include <libcss/libcss.h>

#include "netsurf_probe.h"

/* --------------------------------------------------------------- accumulation */

typedef struct {
	netsurf_check *out;
	int cap;
	int n;
	const char *pending_section;
} sink;

static void section(sink *s, const char *name)
{
	s->pending_section = name;
}

static void record(sink *s, const char *name, int verdict, int detail)
{
	if (s->n >= s->cap)
		return;
	s->out[s->n].section = s->pending_section;
	s->out[s->n].name = name;
	s->out[s->n].verdict = verdict;
	s->out[s->n].detail = detail;
	s->pending_section = NULL;
	s->n++;
}

static void check(sink *s, const char *name, int pass, int detail)
{
	record(s, name, pass ? 1 : 0, detail);
}

static void note(sink *s, const char *name, int value)
{
	record(s, name, -1, value);
}

/* --------------------------------------------------------------- libcss stubs */

/* css_stylesheet_create insists on a URL resolver even for a sheet that imports nothing.
 * This is the minimum one: hand the relative string back with a reference taken. Enough
 * for a sheet with no @import, and wrong for anything real — F6 owns the real one. */
static css_error resolve_url(void *pw, const char *base,
		lwc_string *rel, lwc_string **abs)
{
	(void) pw;
	(void) base;
	*abs = lwc_string_ref(rel);
	return CSS_OK;
}

/* ------------------------------------------------------------------- libwapcaplet */

/* Interning two equal strings and one different one is the cheapest possible proof that
 * the hash table, the allocator calls and the case-insensitive compare all work — and
 * pointer identity for equal content is the library's entire reason to exist, so a wrong
 * answer here is unambiguous rather than subtle. */
static void probe_wapcaplet(sink *s)
{
	lwc_string *a = NULL, *b = NULL, *c = NULL;
	lwc_error e1, e2, e3;
	bool same = false;

	section(s, "libwapcaplet");

	e1 = lwc_intern_string("display", 7, &a);
	e2 = lwc_intern_string("display", 7, &b);
	e3 = lwc_intern_string("position", 8, &c);

	check(s, "lwc_intern_string", e1 == lwc_error_ok && a != NULL, (int) e1);
	check(s, "interned once", e2 == lwc_error_ok && a == b, (int) e2);
	check(s, "distinct strings differ", e3 == lwc_error_ok && c != a, (int) e3);

	if (a != NULL)
		note(s, "length of \"display\"", (int) lwc_string_length(a));

	if (a != NULL && c != NULL) {
		lwc_error e4 = lwc_string_caseless_isequal(a, c, &same);
		check(s, "caseless compare", e4 == lwc_error_ok && !same, (int) e4);
	}

	if (a != NULL) lwc_string_unref(a);
	if (b != NULL) lwc_string_unref(b);
	if (c != NULL) lwc_string_unref(c);
}

/* ------------------------------------------------------------------ libparserutils */

/* The alias lookup goes through a 52 KB generated table (build/make-aliases.pl over
 * build/Aliases), so this doubles as proof that the generated file survived vendoring —
 * a build that silently lost it would still link and would answer 0 to everything. */
static void probe_parserutils(sink *s)
{
	uint16_t utf8, latin1, junk;
	const char *back;
	parserutils_inputstream *stream = NULL;
	parserutils_error e;

	section(s, "libparserutils");

	utf8 = parserutils_charset_mibenum_from_name("UTF-8", 5);
	latin1 = parserutils_charset_mibenum_from_name("iso-8859-1", 10);
	junk = parserutils_charset_mibenum_from_name("not-a-charset", 13);

	check(s, "mibenum UTF-8", utf8 != 0, (int) utf8);
	check(s, "mibenum iso-8859-1", latin1 != 0, (int) latin1);
	check(s, "unknown charset is 0", junk == 0, (int) junk);

	back = parserutils_charset_mibenum_to_name(utf8);
	check(s, "mibenum round trip", back != NULL && strcmp(back, "UTF-8") == 0,
			back != NULL ? (int) strlen(back) : -1);

	/* The input stream is the object libhubbub feeds bytes through, and the one place
	 * iconv can be reached. Asking for UTF-8 means the built-in codec answers and
	 * iconv is not called — which is on purpose: whether the handset's iconv has
	 * conversion tables is a separate question and not this probe's. */
	e = parserutils_inputstream_create("UTF-8", 0, NULL, &stream);
	check(s, "inputstream_create", e == PARSERUTILS_OK && stream != NULL, (int) e);
	if (stream != NULL) {
		e = parserutils_inputstream_append(stream, (const uint8_t *) "<p>hi", 5);
		check(s, "inputstream_append", e == PARSERUTILS_OK, (int) e);
		parserutils_inputstream_destroy(stream);
	}
}

/* ---------------------------------------------------------------------- libhubbub */

/* Create is the interesting half: it allocates the treebuilder's insertion-mode stack
 * and the owners of both generated tables (the gperf element-name hash and the 333 KB
 * named-entity trie). Destroy is what says the allocator round-tripped. */
static void probe_hubbub(sink *s)
{
	hubbub_parser *parser = NULL;
	hubbub_error e;

	section(s, "libhubbub");

	e = hubbub_parser_create("UTF-8", false, &parser);
	check(s, "hubbub_parser_create", e == HUBBUB_OK && parser != NULL, (int) e);
	if (parser != NULL) {
		e = hubbub_parser_destroy(parser);
		check(s, "hubbub_parser_destroy", e == HUBBUB_OK, (int) e);
	}
}

/* ------------------------------------------------------------------------- libdom */

/* The only probe that runs two libraries against each other: the libhubbub binding is
 * the whole HTML path in four calls. The tree walk stops at the root element's name on
 * purpose — F5 is a link test, and a real tree dump belongs to F6, which owns the
 * traversal anyway. */
static void probe_dom(sink *s)
{
	static const char html[] = "<html><body><p id=\"x\">hello</p></body></html>";
	dom_string *str = NULL;
	dom_exception de;
	dom_hubbub_parser_params params;
	dom_hubbub_parser *parser = NULL;
	dom_document *doc = NULL;
	dom_hubbub_error he;

	section(s, "libdom");

	de = dom_string_create((const uint8_t *) "html", 4, &str);
	check(s, "dom_string_create", de == DOM_NO_ERR && str != NULL, (int) de);
	if (str != NULL) {
		note(s, "dom_string length", (int) dom_string_length(str));
		dom_string_unref(str);
	}

	memset(&params, 0, sizeof(params));
	params.enc = "UTF-8";
	params.fix_enc = true;
	params.enable_script = false;
	params.script = NULL;
	params.msg = NULL;
	params.ctx = NULL;
	params.daf = NULL;

	he = dom_hubbub_parser_create(&params, &parser, &doc);
	check(s, "dom_hubbub_parser_create",
			he == DOM_HUBBUB_OK && parser != NULL && doc != NULL, (int) he);

	if (parser != NULL) {
		he = dom_hubbub_parser_parse_chunk(parser,
				(const uint8_t *) html, sizeof(html) - 1);
		check(s, "parse_chunk", he == DOM_HUBBUB_OK, (int) he);
		he = dom_hubbub_parser_completed(parser);
		check(s, "parse completed", he == DOM_HUBBUB_OK, (int) he);
		dom_hubbub_parser_destroy(parser);
	}

	if (doc != NULL) {
		dom_element *root = NULL;
		de = dom_document_get_document_element(doc, &root);
		check(s, "document_element", de == DOM_NO_ERR && root != NULL, (int) de);
		if (root != NULL) {
			dom_string *name = NULL;
			de = dom_node_get_node_name(root, &name);
			/* "HTML" — four characters, upper-cased by the HTML parser. A
			 * length of 4 is therefore a real assertion about the tokeniser
			 * and not just about the pointer being non-NULL. */
			check(s, "root name is 4 chars",
					de == DOM_NO_ERR && name != NULL &&
					dom_string_length(name) == 4,
					name != NULL ? (int) dom_string_length(name) : -1);
			if (name != NULL)
				dom_string_unref(name);
			dom_node_unref(root);
		}
		dom_node_unref(doc);
	}
}

/* ------------------------------------------------------------------------- libcss */

/* One rule with two declarations, which runs the tokeniser, the dispatch table over the
 * 119 generated property parsers, and libcss's interning against libwapcaplet. */
static void probe_css(sink *s)
{
	static const char sheet_src[] = "p { color: #ff0000; display: block; }";
	css_stylesheet_params params;
	css_stylesheet *sheet = NULL;
	css_error e;

	section(s, "libcss");

	memset(&params, 0, sizeof(params));
	params.params_version = CSS_STYLESHEET_PARAMS_VERSION_1;
	params.level = CSS_LEVEL_21;
	params.charset = "UTF-8";
	params.url = "http://localhost/";
	params.title = NULL;
	params.allow_quirks = false;
	params.inline_style = false;
	params.resolve = resolve_url;

	e = css_stylesheet_create(&params, &sheet);
	check(s, "css_stylesheet_create", e == CSS_OK && sheet != NULL, (int) e);

	if (sheet != NULL) {
		size_t size = 0;

		e = css_stylesheet_append_data(sheet,
				(const uint8_t *) sheet_src, sizeof(sheet_src) - 1);
		/* CSS_NEEDDATA is the documented success reply to append: the parser has
		 * consumed what it was given and wants more. Reading it as failure is the
		 * mistake this comment exists to stop. */
		check(s, "append_data", e == CSS_OK || e == CSS_NEEDDATA, (int) e);

		e = css_stylesheet_data_done(sheet);
		check(s, "data_done", e == CSS_OK, (int) e);

		e = css_stylesheet_size(sheet, &size);
		check(s, "stylesheet_size", e == CSS_OK, (int) e);
		/* The number worth having on paper: how many bytes of heap one 36-byte
		 * rule costs. R3 is about RAM, and this is the first datum for it. */
		note(s, "heap for one rule", (int) size);

		e = css_stylesheet_destroy(sheet);
		check(s, "css_stylesheet_destroy", e == CSS_OK, (int) e);
	}
}

/* ---------------------------------------------------------------------------- entry */

int netsurf_probe_run(netsurf_check *out, int cap)
{
	sink s;

	if (out == NULL || cap < 8)
		return -1;

	s.out = out;
	s.cap = cap;
	s.n = 0;
	s.pending_section = NULL;

	/* Dependency order, so a failure lands on the lowest library that has one:
	 * libcss failing is uninformative if libwapcaplet already did. */
	probe_wapcaplet(&s);
	probe_parserutils(&s);
	probe_hubbub(&s);
	probe_dom(&s);
	probe_css(&s);

	return s.n;
}
