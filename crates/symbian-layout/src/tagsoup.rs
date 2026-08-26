//! **Scaffolding.** A small, forgiving HTML reader that produces a [`StyledTree`].
//!
//! # Read this before relying on it
//!
//! This is not the HTML parser this browser ships. That one is libhubbub, behind a binding that does
//! not exist yet, and it is worth waiting for: a real HTML5 tokeniser is a state machine with sixty
//! states and a decade of accumulated error recovery, and the web is written against exactly those
//! recoveries. This file has none of them.
//!
//! It exists so the rest of the browser can be built and measured on the handset before that binding
//! lands — the fetch, the inflate, the worker, the layout, the paint and the scroll, end to end,
//! against a real page. Most of that work is not throwaway; this file is.
//!
//! Named `tagsoup` on purpose. `symbian-html` would sound like a decision.
//!
//! # What it does not do
//!
//! - **No stylesheets.** Style comes from a table of tag names, plus whatever the markup states
//!   *inline*: `style=` declarations and the presentational attributes (`bgcolor`, `color`). A page
//!   whose colours live in a `<style>` block or a linked file arrives with the default canvas. That
//!   canvas is now the web's — white paper, dark ink — rather than the handset's theme, because
//!   rendering a web written for white on a dark background does not read as neutral, it reads as a
//!   page with its contrast inverted.
//! - **No implied tags.** A real parser closes `<p>` when a `<div>` opens and rebuilds a table from
//!   fragments. This one closes what it is told to close, and a document that forgets `</p>` gets a
//!   nesting a browser would not have built.
//! - **No character encoding detection.** Input is `&str`, so someone else already decided it was
//!   UTF-8. A page in Windows-1252 that says so in a `<meta>` will arrive as mojibake.
//! - **A handful of entities**, not the 2231 in the HTML5 table.
//!
//! Every one of those is a thing libhubbub does correctly. None of them is worth doing twice.

use alloc::string::String;
use alloc::vec::Vec;

use symbian_gfx::{Color, Edges};

use crate::css;
use crate::style::{Display, FieldKind, FontRole, Marker, NodeKind, Span, Style, StyledTree};

/// How the document's colours are chosen. Layout does not know about themes, so the caller says.
#[derive(Copy, Clone, Debug)]
pub struct Palette {
    pub text: Color,
    pub dim: Color,
    pub link: Color,
}

impl Default for Palette {
    /// The web's defaults, not the phone's. See [`crate::css::PAPER`].
    fn default() -> Self {
        Palette { text: crate::css::INK, dim: crate::css::DIM, link: crate::css::LINK }
    }
}

/// A parsed document, and the canvas it asked for.
pub struct Page {
    pub tree: StyledTree,
    /// What the document said its background is, if it said.
    ///
    /// Separate from the tree because it applies to the whole viewport, not to a box: a `<body>`
    /// background has to cover the screen even where the document is shorter than it, and a `Fill`
    /// node sized to the content would leave the rest showing whatever was behind.
    pub background: Option<Color>,
}

/// What a tag means for layout. The whole of this file's "CSS".
#[derive(Copy, Clone)]
struct Tag {
    display: Display,
    font: Option<FontRole>,
    /// Vertical margin above and below, in pixels. Horizontal margins are deliberately zero: the
    /// fit-to-width policy wants the full column, and an indent per nesting level would eat it.
    space: i32,
    /// Content is dropped entirely, text included.
    skip: bool,
    rule: bool,
}

const BLOCK: Tag =
    Tag { display: Display::Block, font: None, space: 0, skip: false, rule: false };
const INLINE: Tag =
    Tag { display: Display::Inline, font: None, space: 0, skip: false, rule: false };
const DROP: Tag = Tag { display: Display::None, font: None, space: 0, skip: true, rule: false };

/// What a control shows: its `value`, or for a `<select>` the option marked selected.
///
/// A `<textarea>`'s text is its value and a `<select>`'s is one of its children, so neither is a
/// plain attribute read. Doing it here keeps the whole answer in one place instead of leaving the
/// layout to guess from a box with nothing in it.
impl Parser {
    /// What a control shows.
    ///
    /// The `value` attribute, then `alt` and `aria-label` for a button that has none — a graphical
    /// submit keeps its words there, and a button labelled with nothing is a button nobody can aim
    /// at on purpose.
    ///
    /// A `<button>`'s label is its *text content*, and this does not read it: the node is pushed on
    /// the open tag, before the content has been seen, and the skip machinery that drops a control's
    /// children is what makes a control one leaf box. The bridge — the path that actually runs —
    /// reads it properly via `dom_node_get_text_content`. Here the button still submits and is
    /// labelled by the platform, which is the fallback's job: keep the form usable, not perfect.
    fn control_value(&mut self, name: &str, attrs: &str) -> Span {
        let v = attr(attrs, "value").map(|v| entities(&v)).unwrap_or_default();
        if !v.is_empty() {
            return self.t.intern(&v);
        }
        if name == "button" || name == "input" {
            for key in ["alt", "aria-label"] {
                let a = attr(attrs, key).map(|a| entities(&a)).unwrap_or_default();
                if !a.is_empty() {
                    return self.t.intern(&a);
                }
            }
        }
        if name != "select" {
            return self.t.intern(&v);
        }
        Span::EMPTY
    }
}

/// Which control an element is, or `None` if it is not one.
///
/// `<input>` without a `type` is a text field, and so is an `input` whose type this does not know —
/// that is what HTML says an unrecognised type means, and it is what keeps a page usable when a
/// type appears that this browser has never heard of.
fn control_kind(name: &str, attrs: &str) -> Option<FieldKind> {
    match name {
        "textarea" => Some(FieldKind::TextArea),
        "select" => Some(FieldKind::Select),
        "button" => match attr(attrs, "type").as_deref().map(str::trim) {
            // A `<button>` defaults to submit, unlike an `<input>`.
            Some("button") | Some("reset") => Some(FieldKind::Button),
            _ => Some(FieldKind::Submit),
        },
        "input" => {
            let t = attr(attrs, "type").unwrap_or_default();
            Some(match t.trim().to_ascii_lowercase().as_str() {
                "password" => FieldKind::Password,
                "submit" | "image" => FieldKind::Submit,
                "button" | "reset" => FieldKind::Button,
                "checkbox" => FieldKind::Checkbox,
                "radio" => FieldKind::Radio,
                "hidden" => FieldKind::Hidden,
                _ => FieldKind::Text,
            })
        }
        _ => None,
    }
}

fn tag(name: &str) -> Tag {
    match name {
        // Headings. One heading font, differing by the space around them — six atlases would cost
        // more image than the distinction is worth on a 320-pixel screen.
        "h1" | "h2" => Tag { font: Some(FontRole::Title), space: 6, ..BLOCK },
        "h3" | "h4" | "h5" | "h6" => Tag { font: Some(FontRole::Strong), space: 4, ..BLOCK },

        "p" | "blockquote" | "figcaption" | "dd" | "dt" => Tag { space: 4, ..BLOCK },
        "div" | "section" | "article" | "main" | "header" | "footer" | "nav" | "aside" | "body"
        // `form` is in here rather than on an arm of its own: it lays out as any other block, and
        // the part that is special about it — the action and the method its controls submit to — is
        // read from its attributes in `open`, not from this table.
        | "html" | "ul" | "ol" | "dl" | "table" | "tbody" | "thead" | "tr" | "form" | "figure"
        | "details" => BLOCK,
        "li" | "td" => BLOCK,
        "th" => Tag { font: Some(FontRole::Strong), ..BLOCK },
        "pre" => Tag { font: Some(FontRole::Small), space: 4, ..BLOCK },
        "hr" => Tag { rule: true, space: 4, ..BLOCK },

        "strong" | "b" => Tag { font: Some(FontRole::Strong), ..INLINE },
        "em" | "i" | "cite" | "dfn" => Tag { font: Some(FontRole::Strong), ..INLINE },
        "small" | "sub" | "sup" => Tag { font: Some(FontRole::Small), ..INLINE },
        "code" | "kbd" | "samp" | "tt" => Tag { font: Some(FontRole::Small), ..INLINE },
        "a" | "span" | "label" | "abbr" | "time" | "u" | "s" | "mark" | "q" => INLINE,

        // Dropped, content and all. `<title>` matters to the chrome but is not page content, and a
        // page that rendered its own stylesheet as text is the classic tag-soup failure.
        // `noscript` is absent on purpose: its content is what a client with no scripting is meant
        // to read, and this browser has none. It is not in `is_raw_text` either, so the content
        // parses as markup — the same shape hubbub gives the bridge with scripting disabled.
        "script" | "style" | "head" | "title" | "meta" | "link" | "template"
        | "iframe" | "object" | "embed" | "svg" | "canvas" | "audio" | "video" => DROP,

        // A control's *children* are dropped and the control itself becomes one box. `<option>` is
        // dropped for the same reason: its text is the select's value, and letting it through prints
        // every option as prose beside the box that already shows the chosen one.
        "option" | "optgroup" => DROP,

        // Form controls. Block, because on a 320-pixel column a box beside text leaves neither
        // enough room — which is the same answer fit-to-width gives an image.
        "input" | "button" | "select" | "textarea" => Tag { skip: true, ..BLOCK },

        // Unknown tags are inline and transparent, which is what a browser does with them and what
        // makes an unrecognised wrapper harmless rather than a lost paragraph.
        _ => INLINE,
    }
}

/// Elements whose content is **raw text**: only the literal end tag closes them.
///
/// This is not a nicety, it is the difference between parsing a page and losing it. `<script>var x
/// = 1 < 2;</script>` contains a `<`, and a tokeniser that treats it as the start of a tag consumes
/// the `</script>` as part of a bogus tag name — so the script never closes and the entire rest of
/// the document is swallowed. Measured: it swallowed the page in the test written to prove scripts
/// were dropped.
///
/// A real tokeniser has a state per element for this. Here it is a forward scan for the closing tag.
fn is_raw_text(name: &str) -> bool {
    matches!(name, "script" | "style" | "textarea" | "title")
}

/// Elements with no end tag. Content after them belongs to the parent, not to them.
fn is_void(name: &str) -> bool {
    matches!(
        name,
        "br" | "img" | "hr" | "meta" | "link" | "input" | "area" | "base" | "col" | "embed"
            | "source" | "track" | "wbr"
    )
}

/// Parse `html` into a styled tree.
///
/// Never fails. Malformed input produces a worse tree, not an error — which is the one thing this
/// file has in common with a real HTML parser, and the reason `Result` would be the wrong shape.
pub fn parse(html: &str, palette: Palette) -> StyledTree {
    parse_page(html, palette).tree
}

/// The same, keeping what the document said about its own canvas.
pub fn parse_page(html: &str, palette: Palette) -> Page {
    let mut p = Parser {
        t: StyledTree::new(),
        stack: Vec::new(),
        lists: Vec::new(),
        next_form: 0,
        skip_depth: 0,
        palette,
        background: None,
    };
    let root = p.t.push(NodeKind::Element, Style::default());
    p.stack.push(Frame {
        node: root,
        name: String::new(),
        style: Style { color: palette.text, ..Default::default() },
    });
    p.run(html);
    Page { tree: p.t, background: p.background }
}

struct Frame {
    node: u32,
    name: String,
    /// The inherited style at this point: colour, font and href flow down.
    style: Style,
}

struct Parser {
    t: StyledTree,
    stack: Vec<Frame>,
    /// One counter per open `<ol>`, so nested lists number independently.
    lists: Vec<u32>,
    /// The next form id to hand out. Ids are per document, in source order.
    next_form: u16,
    /// How deep inside a dropped element we are. Text is discarded while non-zero.
    skip_depth: u32,
    palette: Palette,
    /// What `<body>` or `<html>` declared, if anything.
    background: Option<Color>,
}

impl Parser {
    fn run(&mut self, html: &str) {
        let b = html.as_bytes();
        let mut i = 0usize;
        let mut text_start = 0usize;

        while i < b.len() {
            if b[i] != b'<' {
                i += 1;
                continue;
            }
            // Flush the text before this tag.
            self.text(&html[text_start..i]);

            // A comment, a doctype, or a CDATA-ish thing: skip to the end and emit nothing.
            if html[i..].starts_with("<!--") {
                i = find(html, i + 4, "-->").map(|e| e + 3).unwrap_or(b.len());
                text_start = i;
                continue;
            }
            if html[i..].starts_with("<!") || html[i..].starts_with("<?") {
                i = find(html, i + 2, ">").map(|e| e + 1).unwrap_or(b.len());
                text_start = i;
                continue;
            }

            let Some(end) = find(html, i + 1, ">") else {
                // An unterminated tag at the end of the document. Everything after the `<` is not
                // text — a browser drops it, and so does this.
                text_start = b.len();
                i = b.len();
                continue;
            };
            let inner = &html[i + 1..end];
            i = end + 1;
            text_start = i;

            if let Some(name) = inner.strip_prefix('/') {
                self.close(name.trim().to_ascii_lowercase().as_str());
            } else {
                let (name, attrs) = split_name(inner);
                let name = name.to_ascii_lowercase();
                // `<br/>` and `<img ... />` both end in a slash; the slash is not part of the name.
                let self_closing = inner.trim_end().ends_with('/');
                self.open(&name, attrs, self_closing);

                // Raw text: skip to the literal end tag without tokenising what is between. See
                // `is_raw_text` — doing this by tokenising is how the rest of the page gets eaten.
                if is_raw_text(&name) && !self_closing {
                    match find_close(html, i, &name) {
                        Some((content_end, after)) => {
                            // `<title>` is dropped anyway, but `<textarea>` is not raw *content* to
                            // throw away — it is text a user typed. Fed through `text` so the skip
                            // rules decide, exactly as they would have.
                            self.text(&html[i..content_end]);
                            self.close(&name);
                            i = after;
                        }
                        None => {
                            // Unterminated. Everything to the end belongs to the element, which for
                            // a script or a style means nothing is rendered.
                            self.text(&html[i..]);
                            self.close(&name);
                            i = b.len();
                        }
                    }
                    text_start = i;
                }
            }
        }
        self.text(&html[text_start..]);
    }

    fn top(&self) -> &Frame {
        // The root frame is pushed before parsing and never popped, so this cannot be empty.
        self.stack.last().expect("the root frame is never popped")
    }

    fn text(&mut self, raw: &str) {
        if self.skip_depth > 0 || raw.is_empty() {
            return;
        }
        let decoded = entities(raw);
        // All-whitespace text between blocks is not content. Without this every newline in the
        // source becomes a space-only text node, and a page of tidily indented HTML acquires a
        // stray space at the start of most lines.
        if decoded.chars().all(|c| c.is_ascii_whitespace()) {
            return;
        }
        let span = self.t.intern_collapsed(&decoded);
        let style = self.top().style;
        let parent = self.top().node;
        let n = self.t.push(NodeKind::Text(span), style);
        self.t.append_child(parent, n);
    }

    fn open(&mut self, name: &str, attrs: &str, self_closing: bool) {
        let meta = tag(name);

        // A control is emitted here, above the skip below, and that ordering is the whole trick:
        // a control is exactly "one node, and none of its children". The tag table marks these as
        // `skip` so the children are dropped by machinery that already works and so `close`
        // decrements the depth correctly; this pushes the one node that skip would not.
        if self.skip_depth == 0 {
            if let Some(kind) = control_kind(name, attrs) {
                let inherited = self.top().style;
                let name_span =
                    self.t.intern(&entities(&attr(attrs, "name").unwrap_or_default()));
                let value_span = self.control_value(name, attrs);
                let node = self.t.push(
                    NodeKind::Control { kind, name: name_span, value: value_span },
                    Style { display: Display::Block, ..inherited },
                );
                let parent = self.top().node;
                self.t.append_child(parent, node);
            }
        }

        if meta.skip {
            // Void dropped elements (`<meta>`, `<link>`) must not open a skip that never closes.
            if !is_void(name) && !self_closing {
                self.skip_depth += 1;
                self.stack.push(Frame {
                    node: self.top().node,
                    name: String::from(name),
                    style: self.top().style,
                });
            }
            return;
        }
        if self.skip_depth > 0 {
            return;
        }

        // A hard line break is a newline in the arena. `intern`, not `intern_collapsed`: collapsing
        // would turn it into a space and the break would vanish.
        if name == "br" {
            let span = self.t.intern("\n");
            let style = self.top().style;
            let parent = self.top().node;
            let n = self.t.push(NodeKind::Text(span), style);
            self.t.append_child(parent, n);
            return;
        }

        if name == "img" {
            self.image(attrs);
            return;
        }

        let inherited = self.top().style;
        let mut style = Style {
            display: meta.display,
            font: meta.font.unwrap_or(inherited.font),
            color: inherited.color,
            background: None,
            margin: Edges::xy(0, meta.space),
            padding: Edges::ZERO,
            marker: Marker::None,
            href: inherited.href,
            rule_below: meta.rule,
            // Inherited, so a control nested inside markup still knows which form it belongs to.
            form: inherited.form,
            method: inherited.method,
        };

        if name == "a" {
            if let Some(href) = attr(attrs, "href") {
                let decoded = entities(&href);
                style.href = self.t.intern(&decoded);
                style.color = self.palette.link;
            }
        }

        // What the markup states about this element, in the order the cascade would: the
        // presentational attribute first, then `style=`, which is more specific and wins.
        if let Some(bg) = attr(attrs, "bgcolor").and_then(|v| css::color(&entities(&v))) {
            style.background = Some(bg);
        }
        if name == "font" {
            if let Some(c) = attr(attrs, "color").and_then(|v| css::color(&entities(&v))) {
                style.color = c;
            }
        }
        if let Some(decl) = attr(attrs, "style") {
            self.apply_inline(&entities(&decl), &mut style);
        }

        // A canvas colour belongs to the viewport, not to a box — a `<body>` background has to cover
        // the screen even where the document is shorter than it.
        if (name == "body" || name == "html") && style.background.is_some() {
            self.background = style.background.take();
        }

        match name {
            "ol" => self.lists.push(1),
            "ul" => self.lists.push(0),
            "li" => {
                // The counter belongs to the innermost list. A stray `<li>` outside any list gets a
                // bullet, which is what a browser shows.
                style.padding = Edges::new(6, 0, 0, 0);
                match self.lists.last_mut() {
                    Some(n) if *n > 0 => {
                        let mut buf = [0u8; 12];
                        let s = decimal(*n, &mut buf);
                        let mut label = String::from(s);
                        label.push('.');
                        style.marker = Marker::Text(self.t.intern(&label));
                        *n += 1;
                    }
                    _ => style.marker = Marker::Bullet,
                }
            }
            // A form hands its controls where to submit. The action rides in `href`, which a
            // `<form>` has no other use for — one span, two meanings, decided by the element.
            "form" => {
                style.form = self.next_form;
                style.method = match attr(attrs, "method") {
                    Some(m) if m.trim().eq_ignore_ascii_case("post") => 1,
                    _ => 0,
                };
                let action = attr(attrs, "action").map(|a| entities(&a)).unwrap_or_default();
                style.href = self.t.intern(&action);
                self.next_form = self.next_form.saturating_add(1);
            }
            "pre" | "code" => style.color = inherited.color,
            "blockquote" => style.padding = Edges::new(8, 0, 0, 0),
            "td" | "th" => style.padding = Edges::new(4, 1, 0, 1),
            _ => {}
        }

        let parent = self.top().node;
        let node = self.t.push(NodeKind::Element, style);
        self.t.append_child(parent, node);

        // A void element has no children, so it must not become the open frame — everything after
        // `<hr>` would otherwise be inside it.
        if !is_void(name) && !self_closing {
            self.stack.push(Frame { node, name: String::from(name), style });
        }
    }

    /// Apply the declarations this file understands. Everything else is ignored, deliberately:
    /// see the module note on where the real cascade lives.
    fn apply_inline(&mut self, decl: &str, style: &mut Style) {
        for (k, v) in css::declarations(decl) {
            match k {
                "color" => {
                    if let Some(c) = css::color(v) {
                        style.color = c;
                    }
                }
                // `background` is shorthand and may carry an image, a position and a repeat. Only a
                // bare colour is read; anything else leaves the background alone rather than
                // guessing which token was the colour.
                "background" | "background-color" => {
                    if let Some(c) = css::color(v) {
                        style.background = Some(c);
                    }
                }
                "font-weight" => {
                    if v == "bold" || v == "bolder" || v.starts_with('7') || v.starts_with('8')
                        || v.starts_with('9')
                    {
                        style.font = FontRole::Strong;
                    }
                }
                // The one property whose absence is visible on every page that uses it: an element
                // hidden by a stylesheet and shown by us is content the author removed.
                "display" => {
                    if v == "none" {
                        style.display = Display::None;
                    }
                }
                "visibility" if v == "hidden" => {
                    style.display = Display::None;
                }
                _ => {}
            }
        }
    }

    fn close(&mut self, name: &str) {
        if name == "ol" || name == "ul" {
            self.lists.pop();
        }
        // Find the matching frame and pop to it. An unmatched `</div>` closes nothing rather than
        // unwinding the document — dropping to the root on a stray end tag is how a tag-soup parser
        // loses the rest of the page.
        if let Some(pos) = self.stack.iter().rposition(|f| f.name == name) {
            if pos == 0 {
                return;
            }
            while self.stack.len() > pos {
                let f = self.stack.pop().expect("checked by rposition");
                if tag(&f.name).skip {
                    self.skip_depth = self.skip_depth.saturating_sub(1);
                }
            }
        }
    }

    fn image(&mut self, attrs: &str) {
        let src = attr(attrs, "src").map(|s| entities(&s)).unwrap_or_default();
        let span = if src.is_empty() { Span::EMPTY } else { self.t.intern(&src) };
        let w = attr(attrs, "width").and_then(|s| number(&s)).unwrap_or(0);
        let h = attr(attrs, "height").and_then(|s| number(&s)).unwrap_or(0);
        let style = self.top().style;
        let parent = self.top().node;
        let n = self.t.push(NodeKind::Image { src: span, w, h }, style);
        self.t.append_child(parent, n);
    }
}

// -------------------------------------------------------------------------------- helpers --

fn find(s: &str, from: usize, needle: &str) -> Option<usize> {
    s.get(from..).and_then(|t| t.find(needle)).map(|k| from + k)
}

/// Where a raw-text element's content ends, and where parsing resumes.
///
/// Matches `</name` case-insensitively followed by `>` or whitespace, which is what the HTML spec's
/// raw-text end-tag-open state accepts. A `</scriptish>` inside a script is *not* the end tag, and
/// treating it as one would truncate the script and start rendering its tail as prose.
fn find_close(s: &str, from: usize, name: &str) -> Option<(usize, usize)> {
    let hay = s.get(from..)?.to_ascii_lowercase();
    let mut at = 0usize;
    loop {
        let k = hay.get(at..)?.find("</")? + at;
        let after = k + 2;
        if hay.get(after..)?.starts_with(name) {
            let tail = after + name.len();
            let ends_here = match hay.as_bytes().get(tail) {
                Some(b'>') => true,
                Some(c) if c.is_ascii_whitespace() => true,
                _ => false,
            };
            if ends_here {
                let close = s.get(from + tail..).and_then(|t| t.find('>')).map(|d| from + tail + d);
                return Some((from + k, close.map(|c| c + 1).unwrap_or(s.len())));
            }
        }
        at = after;
    }
}

/// Split `div class="x"` into `("div", "class=\"x\"")`.
fn split_name(inner: &str) -> (&str, &str) {
    let t = inner.trim_start();
    match t.find(|c: char| c.is_ascii_whitespace()) {
        Some(k) => (&t[..k], &t[k..]),
        None => (t.trim_end_matches('/'), ""),
    }
}

/// One attribute's value. Quoted or bare; the first match wins.
fn attr(attrs: &str, want: &str) -> Option<String> {
    let lower = attrs.to_ascii_lowercase();
    let mut from = 0usize;
    while let Some(k) = lower.get(from..)?.find(want) {
        let at = from + k;
        // Must be preceded by whitespace or the start, so `href` does not match inside `data-href`.
        let ok_before = at == 0 || lower.as_bytes()[at - 1].is_ascii_whitespace();
        let after = at + want.len();
        let rest = lower.get(after..).unwrap_or("").trim_start();
        if ok_before && rest.starts_with('=') {
            let eq = attrs.get(after..)?.find('=')? + after + 1;
            let v = attrs.get(eq..)?.trim_start();
            let quote = v.as_bytes().first().copied();
            return match quote {
                Some(q @ (b'"' | b'\'')) => {
                    let body = &v[1..];
                    let end = body.find(q as char).unwrap_or(body.len());
                    Some(String::from(&body[..end]))
                }
                _ => {
                    let end = v
                        .find(|c: char| c.is_ascii_whitespace() || c == '>')
                        .unwrap_or(v.len());
                    Some(String::from(v[..end].trim_end_matches('/')))
                }
            };
        }
        from = at + want.len();
    }
    None
}

fn number(s: &str) -> Option<i32> {
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

fn decimal(mut v: u32, buf: &mut [u8; 12]) -> &str {
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 || i == 0 {
            break;
        }
    }
    core::str::from_utf8(&buf[i..]).unwrap_or("")
}

/// The handful of entities that actually matter, plus numeric ones.
///
/// Not the 2231-name HTML5 table. An unknown entity is left as written, which is what a reader sees
/// on a page today when something goes wrong — visible and obviously an escaping bug, rather than
/// silently deleted text.
fn entities(s: &str) -> String {
    if !s.contains('&') {
        return String::from(s);
    }
    let mut out = String::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0usize;
    while i < s.len() {
        if b[i] != b'&' {
            let ch = s[i..].chars().next().unwrap_or('&');
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        let Some(semi) = s.get(i..).and_then(|t| t.find(';')).map(|k| i + k) else {
            out.push('&');
            i += 1;
            continue;
        };
        // A "&" with a lot of text before the next ";" is punctuation, not an entity.
        if semi - i > 12 {
            out.push('&');
            i += 1;
            continue;
        }
        let name = &s[i + 1..semi];
        let ch = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            "mdash" => Some('—'),
            "ndash" => Some('–'),
            "hellip" => Some('…'),
            "copy" => Some('©'),
            "raquo" => Some('»'),
            "laquo" => Some('«'),
            _ => numeric(name),
        };
        match ch {
            Some(c) => {
                out.push(c);
                i = semi + 1;
            }
            None => {
                out.push('&');
                i += 1;
            }
        }
    }
    out
}

fn numeric(name: &str) -> Option<char> {
    let digits = name.strip_prefix('#')?;
    let v = match digits.strip_prefix('x').or_else(|| digits.strip_prefix('X')) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => digits.parse::<u32>().ok()?,
    };
    char::from_u32(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::layout;
    use crate::inline::FontSet;
    use crate::ir::Node;
    use symbian_gfx::{BitmapFont, Font};

    struct Fixed(BitmapFont<'static>);

    fn atlas() -> &'static [u8] {
        alloc::boxed::Box::leak(symbian_ui::testing::atlas().into_boxed_slice())
    }

    impl Fixed {
        fn new() -> Self {
            Fixed(BitmapFont::new(atlas()).unwrap())
        }
    }

    impl FontSet for Fixed {
        fn font(&self, _r: FontRole) -> &dyn Font {
            &self.0
        }
    }

    fn pal() -> Palette {
        Palette { text: Color::WHITE, dim: Color::rgb(0x80, 0x80, 0x80), link: Color::rgb(0, 0, 0xFF) }
    }

    /// Every text run the document produced, in order. The observable behaviour of the parser.
    fn runs(html: &str) -> Vec<String> {
        let t = parse(html, pal());
        let f = Fixed::new();
        let ir = layout(&t, 10_000, &f);
        ir.text_runs().map(|(s, _)| String::from(ir.str(s))).collect()
    }

    #[test]
    fn text_survives_the_tags_around_it() {
        assert_eq!(runs("<p>hello</p>"), ["hello"]);
        assert_eq!(runs("<div><p>a</p><p>b</p></div>"), ["a", "b"]);
    }

    /// Whitespace between tags is formatting, not content. Without this, tidily indented HTML
    /// acquires a stray space at the start of most lines.
    #[test]
    fn whitespace_between_blocks_is_not_content() {
        assert_eq!(runs("<div>\n  <p>a</p>\n  <p>b</p>\n</div>"), ["a", "b"]);
    }

    /// A stylesheet or a script rendered as text is the classic tag-soup failure.
    #[test]
    fn script_and_style_content_is_dropped() {
        assert_eq!(runs("<style>p { color: red }</style><p>visible</p>"), ["visible"]);
        assert_eq!(runs("<script>var x = 1 < 2;</script><p>visible</p>"), ["visible"]);
        assert_eq!(runs("<head><title>Tab name</title></head><body><p>page</p></body>"), ["page"]);
    }

    /// A dropped element must not swallow the rest of the document.
    #[test]
    fn the_page_survives_a_dropped_element() {
        let out = runs("<p>before</p><script>junk</script><p>after</p>");
        assert_eq!(out, ["before", "after"]);
    }

    /// `<meta>` and `<link>` are void *and* dropped — opening a skip for them would never close and
    /// the whole page after `<head>` would vanish.
    #[test]
    fn void_dropped_elements_do_not_open_a_skip() {
        let out = runs("<meta charset=\"utf-8\"><link rel=\"x\" href=\"y\"><p>still here</p>");
        assert_eq!(out, ["still here"]);
    }

    #[test]
    fn entities_are_decoded() {
        assert_eq!(runs("<p>a &amp; b &lt;c&gt; &quot;d&quot;</p>"), ["a & b <c> \"d\""]);
        assert_eq!(runs("<p>&#65;&#x42;</p>"), ["AB"]);
        assert_eq!(runs("<p>caf&eacute;</p>"), ["caf&eacute;"], "an unknown entity stays visible");
    }

    /// A bare `&` in prose is not an entity and must not eat the text after it.
    #[test]
    fn a_bare_ampersand_is_left_alone() {
        assert_eq!(runs("<p>Tom & Jerry; later</p>"), ["Tom & Jerry; later"]);
    }

    #[test]
    fn comments_and_doctypes_produce_nothing() {
        assert_eq!(runs("<!DOCTYPE html><!-- a note --><p>text</p>"), ["text"]);
        assert_eq!(runs("<!-- <p>commented out</p> --><p>real</p>"), ["real"]);
    }

    /// A link's href reaches the tree and its descendants inherit it.
    #[test]
    fn a_link_carries_its_href_into_the_layout() {
        let t = parse("<p>see <a href=\"https://e.com/x\">this <b>page</b></a> now</p>", pal());
        let f = Fixed::new();
        let ir = layout(&t, 10_000, &f);
        let hrefs: Vec<String> = ir
            .nodes()
            .iter()
            .filter_map(|n| match n {
                Node::Link { href, .. } => Some(String::from(ir.str(*href))),
                _ => None,
            })
            .collect();
        assert!(!hrefs.is_empty(), "a link must produce a hit rectangle");
        assert!(hrefs.iter().all(|h| h == "https://e.com/x"), "got {hrefs:?}");
        // And the bold word inside the link is still part of it.
        assert!(hrefs.len() >= 2, "both the plain and the bold part of the link: {hrefs:?}");
    }

    /// Attribute parsing has to survive the shapes real HTML uses.
    #[test]
    fn attributes_are_read_in_every_quoting_style() {
        assert_eq!(attr("href=\"a b\"", "href").as_deref(), Some("a b"));
        assert_eq!(attr("href='a b'", "href").as_deref(), Some("a b"));
        assert_eq!(attr("href=plain", "href").as_deref(), Some("plain"));
        assert_eq!(attr("class=\"x\" href=\"y\"", "href").as_deref(), Some("y"));
        assert_eq!(attr("HREF=\"upper\"", "href").as_deref(), Some("upper"));
        assert_eq!(attr("src=\"i.png\"/", "src").as_deref(), Some("i.png"));
    }

    /// `data-href` must not be read as `href`. This is the bug a naive `find` produces.
    #[test]
    fn a_prefixed_attribute_is_not_the_one_asked_for() {
        assert_eq!(attr("data-href=\"no\"", "href"), None);
        assert_eq!(attr("data-href=\"no\" href=\"yes\"", "href").as_deref(), Some("yes"));
    }

    #[test]
    fn an_image_keeps_its_source_and_size() {
        let t = parse("<img src=\"cat.png\" width=\"640\" height=\"480\">", pal());
        let f = Fixed::new();
        let ir = layout(&t, 320, &f);
        match ir.nodes().iter().find(|n| matches!(n, Node::Image { .. })).expect("an image") {
            Node::Image { rect, src, .. } => {
                assert_eq!(ir.str(*src), "cat.png");
                assert_eq!(rect.width(), 320, "scaled to the column");
                assert_eq!(rect.height(), 240);
            }
            _ => unreachable!(),
        }
    }

    /// A void element must not become the open frame, or everything after it lands inside it.
    #[test]
    fn content_after_a_void_element_is_a_sibling_not_a_child() {
        assert_eq!(runs("<p>a</p><hr><p>b</p>"), ["a", "b"]);
        assert_eq!(runs("<img src=\"x\"><p>after</p>"), ["after"]);
    }

    /// `<br>` is a break the author asked for, so it must survive whitespace collapsing.
    #[test]
    fn br_breaks_the_line() {
        let t = parse("<p>one<br>two</p>", pal());
        let f = Fixed::new();
        let ir = layout(&t, 10_000, &f);
        let ys: Vec<i32> = ir.text_runs().map(|(_, y)| y).collect();
        assert_eq!(ys.len(), 2);
        assert!(ys[0] < ys[1], "br must put the second half on its own line: {ys:?}");
    }

    #[test]
    fn ordered_lists_number_and_unordered_ones_do_not() {
        assert_eq!(runs("<ol><li>a</li><li>b</li><li>c</li></ol>"), ["1.", "a", "2.", "b", "3.", "c"]);
        assert_eq!(runs("<ul><li>a</li><li>b</li></ul>"), ["a", "b"]);
    }

    /// Nested lists number independently, and the outer one resumes.
    #[test]
    fn nested_ordered_lists_keep_their_own_counters() {
        let out = runs("<ol><li>a<ol><li>x</li><li>y</li></ol></li><li>b</li></ol>");
        assert_eq!(out, ["1.", "a", "1.", "x", "2.", "y", "2.", "b"]);
    }

    /// An unmatched end tag closes nothing. Unwinding to the root on a stray `</div>` is how a
    /// tag-soup parser loses the rest of the page.
    #[test]
    fn a_stray_end_tag_does_not_unwind_the_document() {
        assert_eq!(runs("<div><p>a</p></span><p>b</p></div>"), ["a", "b"]);
        assert_eq!(runs("</p></div></body><p>still parsed</p>"), ["still parsed"]);
    }

    /// An unterminated tag at the end of the document is dropped, not shown as text.
    #[test]
    fn an_unterminated_tag_is_not_rendered_as_text() {
        assert_eq!(runs("<p>text</p><div class=\"unclosed"), ["text"]);
    }

    #[test]
    fn empty_and_degenerate_input_is_a_tree_with_nothing_in_it() {
        assert!(runs("").is_empty());
        assert!(runs("<>").is_empty());
        // Garbage in, garbage out — but no panic, and no lost document. A browser renders
        // something here too; what matters is that the parser survives it.
        let _ = runs("<<<>>>");
        assert!(runs("<p></p>").is_empty());
    }

    /// The parser must not lose text just because a tag it does not know wraps it.
    #[test]
    fn an_unknown_tag_is_transparent() {
        assert_eq!(runs("<p>a <weird>b</weird> c</p>"), ["a b c"]);
        assert_eq!(runs("<custom-element>content</custom-element>"), ["content"]);
    }

    /// Self-closing syntax appears in the wild on non-void elements too.
    #[test]
    fn self_closing_syntax_does_not_open_a_frame() {
        assert_eq!(runs("<div/><p>after</p>"), ["after"]);
        assert_eq!(runs("<br/>text"), ["text"]);
    }

    /// A `<` inside a script must not be read as a tag. This ate an entire document.
    #[test]
    fn what_a_page_wrote_for_a_client_without_scripting_is_read() {
        // The half of a modern page that is actually addressed to this browser.
        assert_eq!(
            runs("<noscript><p>Search without JavaScript</p></noscript>"),
            ["Search without JavaScript"]
        );
    }

    #[test]
    fn a_script_is_still_dropped_and_noscript_did_not_loosen_that() {
        assert_eq!(runs("<script>var x = 1 < 2;</script><noscript>ok</noscript>"), ["ok"]);
    }

    #[test]
    fn a_less_than_inside_a_script_does_not_eat_the_page() {
        assert_eq!(runs("<script>var x = 1 < 2;</script><p>after</p>"), ["after"]);
        assert_eq!(runs("<script>if (a<b && c>d) {}</script><p>after</p>"), ["after"]);
        assert_eq!(runs("<style>a[href^=\"<\"] { color: red }</style><p>after</p>"), ["after"]);
    }

    /// A tag whose name merely starts with the element's name is not the end tag.
    #[test]
    fn a_similar_looking_tag_does_not_close_a_script() {
        assert_eq!(runs("<script></scriptish></script><p>after</p>"), ["after"]);
    }

    /// An unterminated script renders nothing rather than dumping its source as prose.
    #[test]
    fn an_unterminated_script_is_not_shown_as_text() {
        assert!(runs("<p>a</p><script>never closed").contains(&String::from("a")));
        assert!(!runs("<p>a</p><script>never closed").iter().any(|r| r.contains("never")));
    }

    /// Inline declarations reach the layout. This is what "use the page's own colours" means
    /// without a stylesheet.
    #[test]
    fn inline_style_sets_the_colour() {
        let t = parse("<p style=\"color: #ff0000\">red</p>", pal());
        let f = Fixed::new();
        let ir = layout(&t, 320, &f);
        let colours: Vec<Color> = ir
            .nodes()
            .iter()
            .filter_map(|n| match n {
                Node::Text { color, .. } => Some(*color),
                _ => None,
            })
            .collect();
        assert_eq!(colours, alloc::vec![Color::rgb(0xFF, 0, 0)]);
    }

    /// An element hidden by a declaration must stay hidden. Showing it is showing content the
    /// author removed.
    #[test]
    fn display_none_from_a_style_attribute_is_honoured() {
        assert!(runs("<p style=\"display:none\">gone</p>").is_empty());
        assert!(runs("<p style=\"visibility: hidden\">gone</p>").is_empty());
        assert_eq!(runs("<p style=\"color:red\">shown</p>"), ["shown"]);
    }

    #[test]
    fn font_weight_bold_reaches_the_font_role() {
        let t = parse("<span style=\"font-weight: bold\">b</span>", pal());
        let f = Fixed::new();
        let ir = layout(&t, 320, &f);
        let fonts: Vec<FontRole> = ir
            .nodes()
            .iter()
            .filter_map(|n| match n {
                Node::Text { font, .. } => Some(*font),
                _ => None,
            })
            .collect();
        assert_eq!(fonts, alloc::vec![FontRole::Strong]);
    }

    /// The page's own background reaches the caller separately, because it covers the viewport and
    /// not just the box it was declared on.
    #[test]
    fn a_body_background_is_reported_as_the_canvas() {
        let p = parse_page("<body bgcolor=\"#112233\"><p>x</p></body>", pal());
        assert_eq!(p.background, Some(Color::rgb(0x11, 0x22, 0x33)));

        let p2 = parse_page("<body style=\"background: white\"><p>x</p></body>", pal());
        assert_eq!(p2.background, Some(Color::rgb(255, 255, 255)));

        let p3 = parse_page("<p>no body</p>", pal());
        assert_eq!(p3.background, None, "a page that says nothing gets the default canvas");
    }

    /// `style=` is more specific than the presentational attribute, and must win.
    #[test]
    fn a_style_attribute_beats_the_presentational_one() {
        let p = parse_page(
            "<body bgcolor=\"red\" style=\"background: #00ff00\"><p>x</p></body>",
            pal(),
        );
        assert_eq!(p.background, Some(Color::rgb(0, 0xFF, 0)));
    }

    /// A declaration this file does not understand must leave the element alone rather than reset
    /// it — an unparsed colour is an inherited colour, not a default one.
    #[test]
    fn an_unknown_value_leaves_the_element_alone() {
        let t = parse("<p style=\"color: var(--brand)\">text</p>", pal());
        let f = Fixed::new();
        let ir = layout(&t, 320, &f);
        match ir.nodes().iter().find(|n| matches!(n, Node::Text { .. })).unwrap() {
            Node::Text { color, .. } => assert_eq!(*color, pal().text, "kept the inherited ink"),
            _ => unreachable!(),
        }
    }

    /// A whole small document, to check the pieces compose rather than each working alone.
    #[test]
    fn a_small_page_comes_out_whole() {
        let html = "\
<!DOCTYPE html>
<html>
  <head><title>Ignored</title><style>p{}</style></head>
  <body>
    <h1>Title</h1>
    <p>Some <strong>bold</strong> text with a <a href=\"/x\">link</a>.</p>
    <ul><li>first</li><li>second</li></ul>
    <hr>
    <p>After the rule &amp; the entity.</p>
  </body>
</html>";
        assert_eq!(
            runs(html),
            [
                "Title",
                "Some ",
                "bold",
                " text with a ",
                "link",
                ".",
                "first",
                "second",
                "After the rule & the entity."
            ]
        );
    }

    /// Deep nesting must not blow the stack — a real page from a template engine is often 40 levels
    /// of `<div>`, and this recursion-free parser should not care.
    #[test]
    fn deep_nesting_is_survivable() {
        let mut html = String::new();
        for _ in 0..500 {
            html.push_str("<div>");
        }
        html.push_str("deep");
        for _ in 0..500 {
            html.push_str("</div>");
        }
        assert_eq!(runs(&html), ["deep"]);
    }
}

#[cfg(test)]
mod form_tests {
    use super::*;
    use crate::style::NO_FORM;

    fn parse(html: &str) -> Page {
        parse_page(html, Palette::default())
    }

    fn controls(p: &Page) -> Vec<(FieldKind, String, String, u16)> {
        let t = &p.tree;
        (0..t.len() as u32)
            .filter_map(|i| match t.node(i).kind {
                NodeKind::Control { kind, name, value } => Some((
                    kind,
                    String::from(t.str(name)),
                    String::from(t.str(value)),
                    t.node(i).style.form,
                )),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_search_form_becomes_two_controls() {
        // The shape of every search box on the web, and the reason this feature exists.
        let p = parse(
            r#"<form action="/search"><input name="q" value="cats"><input type="submit" value="Go"></form>"#,
        );
        let c = controls(&p);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0], (FieldKind::Text, String::from("q"), String::from("cats"), 0));
        assert_eq!(c[1].0, FieldKind::Submit);
        assert_eq!(c[1].2, "Go", "the button's label is its value");
    }

    #[test]
    fn an_input_with_no_type_or_an_unknown_one_is_a_text_field() {
        // What HTML says, and what keeps a page usable when a new input type appears.
        for html in [r#"<input name="a">"#, r#"<input type="color" name="a">"#] {
            let c = controls(&parse(html));
            assert_eq!(c[0].0, FieldKind::Text, "{html}");
        }
    }

    #[test]
    fn a_button_defaults_to_submit_and_an_input_button_does_not() {
        assert_eq!(controls(&parse("<button>Send</button>"))[0].0, FieldKind::Submit);
        assert_eq!(
            controls(&parse(r#"<input type="button" value="x">"#))[0].0,
            FieldKind::Button
        );
    }

    #[test]
    fn a_controls_children_are_not_laid_out_beside_it() {
        // The bug this shape prevents: a `<button>`'s text printed both inside the box and after it.
        let p = parse("<button>Send</button>");
        let texts: Vec<&str> = (0..p.tree.len() as u32)
            .filter_map(|i| match p.tree.node(i).kind {
                NodeKind::Text(s) => Some(p.tree.str(s)),
                _ => None,
            })
            .collect();
        assert!(texts.iter().all(|t| t.trim().is_empty()), "got prose: {texts:?}");
    }

    #[test]
    fn options_do_not_leak_out_of_a_select() {
        // Today's visible bug: every option printed as prose next to the box.
        let p = parse("<select name=s><option>One</option><option>Two</option></select>");
        let texts: Vec<&str> = (0..p.tree.len() as u32)
            .filter_map(|i| match p.tree.node(i).kind {
                NodeKind::Text(s) => Some(p.tree.str(s)),
                _ => None,
            })
            .collect();
        assert!(texts.iter().all(|t| t.trim().is_empty()), "got prose: {texts:?}");
        assert_eq!(controls(&p)[0].0, FieldKind::Select);
    }

    #[test]
    fn each_form_gets_its_own_id_and_a_stray_control_has_none() {
        let p = parse(
            "<input name=loose><form><input name=a></form><form><input name=b></form>",
        );
        let c = controls(&p);
        assert_eq!(c[0].3, NO_FORM, "a control outside every form still has an answer");
        assert_eq!(c[1].3, 0);
        assert_eq!(c[2].3, 1, "ids are per document, in source order");
    }

    #[test]
    fn a_control_inside_nested_markup_still_knows_its_form() {
        // Why the id is inherited rather than matched up by ancestor afterwards.
        let p = parse("<form><div><p><input name=deep></p></div></form>");
        assert_eq!(controls(&p)[0].3, 0);
    }

    #[test]
    fn a_form_inside_noscript_is_a_working_form() {
        // The reason this matters more than the prose: a page whose real search box is built by a
        // script usually leaves a plain one in here, and dropping it left nothing to type into.
        let c = controls(&parse(
            r#"<noscript><form action="/s"><input name=q><input type=submit value=Go></form></noscript>"#,
        ));
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].0, FieldKind::Text);
        assert_eq!(c[0].1, "q");
        assert_eq!(c[1].0, FieldKind::Submit);
    }

    #[test]
    fn a_graphical_submit_is_labelled_by_its_alt_text() {
        // `<input type=image>` has no value by construction, and this browser cannot draw the
        // picture inside a control anyway — the alt text is the whole label there is.
        let c = controls(&parse(r#"<form><input type=image name=go alt="Search" src="/s.png"></form>"#));
        assert_eq!(c[0].0, FieldKind::Submit);
        assert_eq!(c[0].2, "Search");
    }

    #[test]
    fn a_value_wins_over_alt_because_it_is_the_one_that_is_submitted() {
        let c = controls(&parse(r#"<form><input type=submit value="Go" alt="Search"></form>"#));
        assert_eq!(c[0].2, "Go");
    }

    #[test]
    fn the_method_and_action_ride_on_the_form() {
        let p = parse(r#"<form action="/login" method="POST"><input name=u></form>"#);
        let t = &p.tree;
        let form = (0..t.len() as u32)
            .find(|&i| t.node(i).style.method == 1)
            .expect("a POST form");
        assert_eq!(t.str(t.node(form).style.href), "/login");
    }

    #[test]
    fn a_hidden_input_is_carried_but_not_focusable() {
        // It must reach the submission and never take a cursor.
        let c = controls(&parse(r#"<input type="hidden" name="csrf" value="tok">"#));
        assert_eq!(c[0].0, FieldKind::Hidden);
        assert!(!FieldKind::Hidden.focusable());
        assert_eq!(c[0].2, "tok");
    }
}
