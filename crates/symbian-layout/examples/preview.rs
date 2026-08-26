//! `cargo run -p symbian-layout --example preview` — pages rendered on the desktop.
//! `cargo test -p symbian-layout --example preview` — the same, asserted.
//!
//! Two things in one file, following the pattern `symbian-decl-ui`'s `compare.rs` set:
//!
//! - **A contact sheet.** Six hand-written pages, rendered at the handset's width and written to
//!   `preview-out/` as PNGs. Reviewed by eye, because "does this page read well on a 320-pixel
//!   screen" is not a question an assertion answers. This is the artefact that shows whether the
//!   fit-to-width policy is any good.
//!
//! - **A parity round trip**, which *is* an assertion and fails the build. Each page is painted
//!   twice: once from the live `PageIr`, once from bytes that were serialised and parsed back.
//!   `Parity` compares them pixel for pixel and panics on a difference.
//!
//! That second half is the plan's claim about the IR, tested. A frozen tab keeps the IR and throws
//! the DOM away; save-for-offline writes the IR to disk and reads it back later. Both are only safe
//! if a reloaded page is the *same* page, and "same" here means the pixels.

use symbian_gfx::{Color, Edges, Font, Rect, Size, E72_SCREEN};
use symbian_layout::block::{body, layout};
use symbian_layout::ir::{decode, encode};
use symbian_layout::paint::paint;
use symbian_layout::style::{Display, FontRole, Marker, NodeKind, Style, StyledTree};
use symbian_layout::tagsoup::parse;
use symbian_layout::FontSet;
use symbian_preview::{Atlases, Parity, Sheet};
use symbian_ui::Fonts;

/// The toolkit's four roles, as a [`FontSet`].
///
/// The whole reason layout names fonts by role: this binds them to the *preview's* atlases, which
/// are deliberately larger than the handset's. The same IR renders in both, and neither knows about
/// the other.
struct Roles<'a>(Fonts<'a>);

impl FontSet for Roles<'_> {
    fn font(&self, role: FontRole) -> &dyn Font {
        match role {
            FontRole::Body => self.0.body,
            FontRole::Strong => self.0.strong,
            FontRole::Small => self.0.small,
            FontRole::Title => self.0.title,
        }
    }
}

/// Ink and paper. Taken from the theme rather than hardcoded, so the sheets look like the device.
struct Ink {
    text: Color,
    dim: Color,
    link: Color,
    paper: Color,
}

fn styles(_theme: &symbian_ui::Theme<'_>) -> Ink {
    // The web's defaults, not the theme's.
    //
    // These sheets used the toolkit's dark palette, which made them a poor guide to the device: HTML
    // means white paper and dark ink when it says nothing, and a page rendered light-on-dark has its
    // contrast inverted rather than being neutral. The sheets now show what the handset shows.
    Ink {
        text: symbian_layout::css::INK,
        dim: symbian_layout::css::DIM,
        link: symbian_layout::css::LINK,
        paper: symbian_layout::css::PAPER,
    }
}

// ----------------------------------------------------------------------------- the pages --

/// A paragraph of body text, long enough to wrap several times.
fn prose(ink: &Ink) -> StyledTree {
    let mut t = StyledTree::new();
    let root = t.push(NodeKind::Element, Style::default());

    let h = t.intern_collapsed("Symbian");
    let head = t.push(
        NodeKind::Element,
        Style { font: FontRole::Title, color: ink.text, margin: Edges::xy(0, 4), ..body() },
    );
    let ht = t.push(NodeKind::Text(h), Style { font: FontRole::Title, color: ink.text, ..body() });
    t.append_child(head, ht);
    t.append_child(root, head);

    let p = t.intern_collapsed(
        "Symbian is a discontinued mobile operating system and computing platform designed for
         smartphones. It was originally developed as a proprietary software OS for personal digital
         assistants in 1998 by the Symbian Ltd. consortium.",
    );
    let para = t.push(NodeKind::Element, Style { color: ink.text, ..body() });
    let pt = t.push(NodeKind::Text(p), Style { color: ink.text, ..body() });
    t.append_child(para, pt);
    t.append_child(root, para);
    t
}

/// Mixed inline styles inside one paragraph — the case `Font::wrap` cannot do.
fn mixed(ink: &Ink) -> StyledTree {
    let mut t = StyledTree::new();
    let root = t.push(NodeKind::Element, Style::default());
    let para = t.push(NodeKind::Element, Style { color: ink.text, ..body() });

    let bits: [(&str, FontRole, Color); 6] = [
        ("The screen is ", FontRole::Body, ink.text),
        ("320 by 240", FontRole::Strong, ink.text),
        (" pixels, which is ", FontRole::Body, ink.text),
        ("not much", FontRole::Strong, ink.text),
        (" and is the whole reason a faithful layout is the wrong answer here. ", FontRole::Body, ink.text),
        ("Small print follows the rest of the line.", FontRole::Small, ink.dim),
    ];
    for (s, font, color) in bits {
        let span = t.intern_collapsed(s);
        let n = t.push(NodeKind::Text(span), Style { font, color, ..body() });
        t.append_child(para, n);
    }
    t.append_child(root, para);
    t
}

/// A bulleted and a numbered list.
fn lists(ink: &Ink) -> StyledTree {
    let mut t = StyledTree::new();
    let root = t.push(NodeKind::Element, Style::default());

    for s in ["libhubbub tokenises", "libdom builds the tree", "libcss answers the cascade"] {
        let span = t.intern_collapsed(s);
        let li = t.push(
            NodeKind::Element,
            Style { marker: Marker::Bullet, color: ink.text, padding: Edges::xy(6, 0), ..body() },
        );
        let txt = t.push(NodeKind::Text(span), Style { color: ink.text, ..body() });
        t.append_child(li, txt);
        t.append_child(root, li);
    }

    for (i, s) in ["fetch", "inflate", "lay out", "paint"].iter().enumerate() {
        // The marker is interned by the producer — layout has no string table for generated text.
        let mark = t.intern(&format!("{}.", i + 1));
        let span = t.intern_collapsed(s);
        let li = t.push(
            NodeKind::Element,
            Style {
                marker: Marker::Text(mark),
                color: ink.text,
                padding: Edges::xy(6, 0),
                ..body()
            },
        );
        let txt = t.push(NodeKind::Text(span), Style { color: ink.text, ..body() });
        t.append_child(li, txt);
        t.append_child(root, li);
    }
    t
}

/// Links, including one long enough to wrap — which must produce a hit rectangle per line.
fn links(ink: &Ink) -> StyledTree {
    let mut t = StyledTree::new();
    let root = t.push(NodeKind::Element, Style::default());
    let para = t.push(NodeKind::Element, Style { color: ink.text, ..body() });

    let lead = t.intern_collapsed("Sources: ");
    let n = t.push(NodeKind::Text(lead), Style { color: ink.text, ..body() });
    t.append_child(para, n);

    let href = t.intern("https://www.netsurf-browser.org/projects/libcss/");
    let label = t.intern_collapsed("the libcss project page, which is a long link that has to wrap");
    let a = t.push(
        NodeKind::Text(label),
        Style { color: ink.link, href, display: Display::Inline, ..body() },
    );
    t.append_child(para, a);

    let tail = t.intern_collapsed(" and some text after it.");
    let n2 = t.push(NodeKind::Text(tail), Style { color: ink.text, ..body() });
    t.append_child(para, n2);
    t.append_child(root, para);
    t
}

/// An image far wider than the column, and one with no declared size.
fn images(ink: &Ink) -> StyledTree {
    let mut t = StyledTree::new();
    let root = t.push(NodeKind::Element, Style::default());

    let cap = t.intern_collapsed("A 640x480 photograph, scaled to the column:");
    let c1 = t.push(NodeKind::Element, Style { color: ink.dim, font: FontRole::Small, ..body() });
    let c1t = t.push(NodeKind::Text(cap), Style { color: ink.dim, font: FontRole::Small, ..body() });
    t.append_child(c1, c1t);
    t.append_child(root, c1);

    let src = t.intern("photo.jpg");
    let img = t.push(NodeKind::Image { src, w: 640, h: 480 }, body());
    t.append_child(root, img);

    let cap2 = t.intern_collapsed("And one the document forgot to measure:");
    let c2 = t.push(NodeKind::Element, Style { color: ink.dim, font: FontRole::Small, ..body() });
    let c2t = t.push(NodeKind::Text(cap2), Style { color: ink.dim, font: FontRole::Small, ..body() });
    t.append_child(c2, c2t);
    t.append_child(root, c2);

    let src2 = t.intern("mystery.gif");
    let img2 = t.push(NodeKind::Image { src: src2, w: 0, h: 0 }, body());
    t.append_child(root, img2);
    t
}

/// A table, collapsed to one column. This is the loss the policy accepts, shown rather than
/// described: the cells are readable and the columns are gone.
fn table(ink: &Ink) -> StyledTree {
    let mut t = StyledTree::new();
    let root = t.push(NodeKind::Element, Style::default());

    let rows: [(&str, &str); 4] = [
        ("Phase", "State"),
        ("F3 transport", "measured on the E72"),
        ("F4 worker", "64 KB opaque, one tick"),
        ("F5 NetSurf", "links, not yet run"),
    ];
    for (i, (a, b)) in rows.iter().enumerate() {
        let font = if i == 0 { FontRole::Strong } else { FontRole::Body };
        for cell in [a, b] {
            let span = t.intern_collapsed(cell);
            let c = t.push(
                NodeKind::Element,
                Style { font, color: ink.text, padding: Edges::new(4, 1, 0, 1), ..body() },
            );
            let ct = t.push(NodeKind::Text(span), Style { font, color: ink.text, ..body() });
            t.append_child(c, ct);
            t.append_child(root, c);
        }
        let sep = t.push(NodeKind::Element, Style { rule_below: true, color: ink.dim, ..body() });
        t.append_child(root, sep);
    }
    t
}

/// A page built from **HTML**, through `tagsoup`, rather than by hand.
///
/// The others test the layout engine. This one tests the whole chain a browser runs: markup in,
/// pixels out — which is the thing that will be pointed at a real URL on the handset.
fn from_html(ink: &Ink) -> StyledTree {
    let html = "\
<!DOCTYPE html>
<html><head><title>ignored</title>
<style>body { font-family: whatever }</style>
<script>var t = 1 < 2;</script>
</head>
<body>
  <h1>The E72 browser</h1>
  <p>This page was <strong>parsed from HTML</strong> and laid out for a
     320&nbsp;pixel column. The declared width of anything is ignored &mdash;
     that is the whole policy.</p>
  <ul>
    <li>fetch over the platform stack</li>
    <li>inflate through a sliding window</li>
    <li>lay out off the UI thread</li>
  </ul>
  <hr>
  <p><small>See <a href=\"https://www.netsurf-browser.org/\">NetSurf</a> for the
     libraries this borrows.</small></p>
</body></html>";
    parse(html, symbian_layout::tagsoup::Palette { text: ink.text, dim: ink.dim, link: ink.link })
}

fn main() {
    let atlases = Atlases::load();
    let mut parity = Parity::new(symbian_preview::parity::default_out_dir());

    atlases.with_themes(|dark, _light| {
        let fonts = Roles(dark.fonts);
        let ink = styles(dark);

        let pages: [(&str, StyledTree); 7] = [
            ("page-prose", prose(&ink)),
            ("page-mixed", mixed(&ink)),
            ("page-lists", lists(&ink)),
            ("page-links", links(&ink)),
            ("page-images", images(&ink)),
            ("page-table", table(&ink)),
            ("page-html", from_html(&ink)),
        ];

        for (name, tree) in pages {
            let ir = layout(&tree, E72_SCREEN.w, &fonts);

            // The contact sheet.
            let mut sheet = Sheet::new(E72_SCREEN);
            {
                let mut c = sheet.canvas();
                c.clear(ink.paper);
                paint(&mut c, Rect::from_size(E72_SCREEN), &ir, 0, &fonts);
            }
            sheet.save("preview-out", name);
            println!(
                "{name}: {} nodes, document {} px tall",
                ir.nodes().len(),
                ir.height()
            );

            // The assertion: the same page, from bytes.
            let bytes = encode(&ir);
            let reloaded = decode(&bytes).expect("a page we just wrote must parse");
            parity.check(
                name,
                dark,
                |c| {
                    c.clear(ink.paper);
                    paint(c, Rect::from_size(E72_SCREEN), &ir, 0, &fonts);
                },
                |c| {
                    c.clear(ink.paper);
                    paint(c, Rect::from_size(E72_SCREEN), &reloaded, 0, &fonts);
                },
            );
        }

        // A scrolled view too: culling is where a round trip could differ and a first screenful
        // would never show it.
        let tree = prose(&ink);
        let ir = layout(&tree, E72_SCREEN.w, &fonts);
        let reloaded = decode(&encode(&ir)).unwrap();
        let scroll = symbian_layout::max_scroll(&ir, E72_SCREEN.h);
        parity.check(
            "page-prose-scrolled",
            dark,
            |c| {
                c.clear(ink.paper);
                paint(c, Rect::from_size(E72_SCREEN), &ir, scroll, &fonts);
            },
            |c| {
                c.clear(ink.paper);
                paint(c, Rect::from_size(E72_SCREEN), &reloaded, scroll, &fonts);
            },
        );

        let mut sheet = Sheet::new(Size::new(E72_SCREEN.w, E72_SCREEN.h));
        {
            let mut c = sheet.canvas();
            c.clear(ink.paper);
            paint(&mut c, Rect::from_size(E72_SCREEN), &ir, scroll, &fonts);
        }
        sheet.save("preview-out", "page-prose-scrolled");
    });

    // Panics if any page differed. `checked()` is asserted too: a suite that quietly ran one page
    // reads exactly like a suite that ran seven.
    assert_eq!(parity.checked(), 8, "every page must be checked, not just the ones that ran");
    parity.finish();
}

#[test]
fn pages_survive_a_round_trip_through_the_ir() {
    main();
}
