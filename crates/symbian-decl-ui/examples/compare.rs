//! The same screen twice — declared, and written by hand — compared pixel for pixel.
//!
//! ```text
//! cargo run -p symbian-decl-ui --example compare     # → compare-out/*.png
//! cargo test -p symbian-decl-ui --example compare    # → fails if they differ
//! ```
//!
//! # Why this exists
//!
//! Every other test in this crate proves arithmetic: that three bands sum to 240, that a measured
//! size is inside its offer, that a hash moves when a string does. None of them can fail if the
//! declarative layer draws a *correct* screen that is not the *same* screen the imperative toolkit
//! draws, and "the same screen" is the whole claim: an application migrating to `symbian-decl-ui`
//! is told it will look identical. Nothing checked that until this file.
//!
//! So the screen below is built twice. Once as a [`Screen`] with a [`TitleBar`], a [`ScrollList`]
//! and three softkeys declared as (label, message) pairs. Once the way `symbian-ui` screens are
//! written today: [`Frame::split`] by hand, [`chrome::title_bar`], a [`ListState`] driven directly,
//! [`chrome::softkey_bar`] with an array of labels. Both render into a 320x240 RGB565 buffer with
//! the device's own font atlases, and the buffers must be byte-identical.
//!
//! # What is genuinely compared, and what is not
//!
//! Being honest about this matters more than the green tick.
//!
//! **Independently computed on both sides**, so a difference is a real difference: the background
//! fill, which bands exist and where they start, the title bar and its right-hand detail, the
//! softkey labels *and their order*, the scrollbar gutter taken off the content width, which rows
//! are on screen after the selection scrolled the list, where the selection band lands, and the
//! scrollbar thumb.
//!
//! **Shared** in scene A: the row's own drawing. [`draw_row`] is called by both sides, so scene A
//! says nothing about whether a declarative row lays out like a hand-written one — it isolates the
//! chrome, which is what Phase 2 built. Scene B asks that second question with nothing shared at
//! all: the declarative side rebuilds the row out of [`Row`] and [`Text`], the imperative side
//! keeps [`draw_row`], and any difference is the layout engine disagreeing with hand arithmetic.
//!
//! # What it found
//!
//! **Scene A is byte-identical**, drawn directly *and* down the path a real application takes —
//! `view` returning a [`Node`], the bridge owning the [`UiCache`], `draw_frame` starting the frame —
//! and identical again on a second frame drawn against the cache the first one filled. That last
//! one matters: a cache that outlives a frame is the one thing here that could make two renders
//! agree for the wrong reason, by handing out last frame's rects for ever.
//!
//! **Scene B is byte-identical too, and was not when this file was written.** A row built out of
//! [`Row`] and two [`Text`]s — padding, gap, a flexible name, a timestamp sized to itself —
//! reproduces the hand-written S60 row exactly. What it needed was cross-axis alignment: a group
//! used to leave each child at its measured height and anchor it to the top, so a 17-pixel line of
//! text in a 38-pixel list row sat at y=0..17 instead of centred at y=10..27, and every row on the
//! screen was drawn ten pixels high. The horizontal arithmetic agreed to the pixel from the start.
//! `.align(CrossAlign::Stretch)` is what closes it, and
//! [`the_row_reaches_parity_because_of_its_cross_axis_alignment_and_not_by_luck`] is there so that
//! nobody deletes that call believing it decorative.
//!
//! **Both layers draw the top scrolled row over the title bar.** 924 pixels of it, in this scene.
//! [`ListState::for_visible`] hands out a rect for the partially-visible first row that begins 23
//! pixels above the viewport — correct, that is what "partially visible" means — and nothing clips
//! it, so the text lands on the title bar. The declarative layer inherits this exactly rather than
//! introducing it, which is why scene A still passes; the fix is a `Canvas::enter` around the row
//! loop and it belongs in `symbian-ui`, not here. Pinned by
//! [`both_layers_let_a_scrolled_row_draw_over_the_title_bar`] so it cannot be lost.
//!
//! The imperative side is the reference because it is what ships today — but a reference is not a
//! proof of correctness, and both findings above are defects: one in the layout engine, now fixed,
//! and one in what ships. Every difference is
//! enumerated by band before anything is changed: making the declarative side match by adjusting it
//! until the numbers agree is the one outcome that would waste the exercise.

use std::process::ExitCode;

use symbian_decl_ui::app::DeclarativeApp;
use symbian_decl_ui::bridge::DeclarativeAppBridge;
use symbian_decl_ui::cmd::Cmd;
use symbian_decl_ui::constraints::Constraints;
use symbian_decl_ui::slot::SlotTable;
use symbian_decl_ui::widgets::{Node, Row, ScrollList, Screen, Text, TitleBar};
use symbian_decl_ui::Widget;
use symbian_gfx::{Align, Canvas, Color, Rect, Rgb565, Size, E72_SCREEN};
use symbian_preview::{Atlases, Sheet};
use symbian_ui::{chrome, Frame, ListState, Theme, Uniform};

/// Where the sheets land, relative to wherever this was run from.
const OUT: &str = "compare-out";

/// Far enough down the list that it has had to scroll — a screen where nothing has scrolled would
/// not compare the one number the declarative side derives and the model never sees.
const SELECTED: usize = 8;

const ROW_H: i32 = 38;

/// A chat list with the widths a real one has: names long enough to be truncated by the time
/// column, and timestamps in two different shapes.
const CHATS: [(&str, &str); 12] = [
    ("Ana Beatriz", "14:32"),
    ("Time de plantão", "14:05"),
    ("Joshua Passos", "13:58"),
    ("Carlos Eduardo Nogueira", "12:11"),
    ("Suporte", "11:47"),
    ("Mãe", "Ontem"),
    ("Grupo do prédio", "Ontem"),
    ("Rafael", "Ontem"),
    ("Financeiro — cobranças e renegociações do mês", "Seg"),
    ("Lucia", "Seg"),
    ("Entregas", "Dom"),
    ("Notificações", "Dom"),
];

#[derive(Clone, Debug, PartialEq, Eq)]
enum Msg {
    Options,
    Open,
    Back,
}

// ---------------------------------------------------------------------------- the row, by hand

/// One chat row, drawn the way a hand-written S60 screen draws it: the name on the left in the
/// emphasis font, the timestamp hugging the right edge in the small dim one, and both colours
/// swapped for the selection's own text colour when the row is highlighted.
///
/// This is the reference for both scenes. Scene A calls it from the declarative side too; scene B
/// leaves it on the imperative side only and lets the layout engine try to reproduce it.
fn draw_row(c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>, index: usize, selected: bool) {
    let (name, time) = CHATS[index];
    let pad = theme.metrics.pad;
    let inner = rect.inset_xy(pad, 0);

    let (fg, dim) = if selected {
        (theme.palette.selection_text, theme.palette.selection_text)
    } else {
        (theme.palette.text, theme.palette.dim)
    };

    let time_w = theme.fonts.small.measure(time);
    let (right, left) = inner.split_right(time_w);
    c.draw_text_in(right, time, theme.fonts.small, dim, Align::End);
    // A gap so a long name cannot butt against the timestamp — the same rule `chrome::title_bar`
    // applies to its detail, and the reason a name is truncated rather than overlapping.
    c.draw_text_in(Rect { x1: left.x1 - pad, ..left }, name, theme.fonts.strong, fg, Align::Start);
}

/// [`draw_row`] wearing the [`Widget`] contract, so a `ScrollList` can build one per visible row.
struct HandRow {
    index: usize,
    selected: bool,
}

impl Widget for HandRow {
    fn measure(&self, c: Constraints, _t: &Theme<'_>) -> Size {
        c.constrain(Size::new(c.max_w, c.max_h))
    }
    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        draw_row(c, rect, theme, self.index, self.selected);
    }
}

// ------------------------------------------------------------------------- the declarative side

fn declared(slots: &mut SlotTable, scene: Scene) -> Screen<Msg> {
    Screen::new()
        .title_bar(TitleBar::new("Recent").detail("online"))
        .content(
            ScrollList::new(slots, CHATS.len(), ROW_H)
                .selected(SELECTED)
                .scrollbar(true)
                .row(move |i, selected| match scene {
                    Scene::Chrome => Node::leaf(HandRow { index: i, selected }),
                    Scene::Rows => Node::Group(declared_row(i, selected)),
                }),
        )
        .on_options("Options", Msg::Options)
        .on_action("Open", Msg::Open)
        .on_back("Back", Msg::Back)
}

/// Scene B's row, built out of the declarative layer rather than drawn.
///
/// Padding on the left and right only, the name taking whatever the timestamp leaves, and the
/// timestamp sized to its own text — which is the arrangement [`draw_row`] writes out by hand with
/// a `split_right` and a subtraction.
fn declared_row(index: usize, selected: bool) -> symbian_decl_ui::widgets::Group {
    use symbian_decl_ui::theme::FontRole;
    use symbian_decl_ui::widgets::Ink;

    let (name, time) = CHATS[index];
    // No `if selected` here any more, and that absence is the point of this scene now. `Ink::Text`
    // and `Ink::Dim` resolve against the ground the list put them on — see `symbian_ui::Ground` — so
    // the declarative row says what it means and the highlight is handled where the highlight is
    // painted. This used to be a literal `Ink::Fixed(SELECTED_TEXT)`, a colour smuggled past the
    // theme because a row is built before it has one.
    let (fg, dim) = (Ink::Text, Ink::Dim);
    let _ = selected;
    Row::new()
        .align(symbian_decl_ui::layout::CrossAlign::Stretch)
        .padding(symbian_gfx::Edges { left: 5, right: 5, top: 0, bottom: 0 })
        .gap(5)
        .child(Text::new(name).font(FontRole::Strong).ink(fg).flex(1))
        .child(Text::new(time).font(FontRole::Small).ink(dim).align(Align::End))
}

// -------------------------------------------------------------------------- the imperative side

/// The same screen written the way `symbian-ui` screens are written today.
///
/// Nothing here is shared with the declarative path: its own [`Frame::split`], its own
/// [`ListState`], its own label array. That is the point — two independent routes to the same
/// pixels.
fn by_hand(c: &mut Canvas<'_>, theme: &Theme<'_>) {
    let screen = Rect::from_size(E72_SCREEN);

    chrome::clear(c, theme);
    let f = Frame::split(screen, theme, true, true);
    chrome::title_bar(c, f.title, theme, "Recent", Some("online"));

    let rows = Uniform { count: CHATS.len(), height: ROW_H };
    // The gutter comes off the width and never off the height, so the viewport a scroll offset is
    // computed against is still the full band. Getting that backwards is a list that scrolls a few
    // pixels short of the bottom row and never quite shows it.
    let band = Rect { x1: f.content.x1 - chrome::scrollbar_gutter(theme), ..f.content };
    let mut state = ListState::new();
    state.select(SELECTED, &rows, f.content.height());

    state.draw_visible(c, &rows, band, |c, i, r| {
        // The highlight first and full-bleed: with no pointer it is the only thing saying where you
        // are, so anything the row draws must go on top of it.
        if i == SELECTED {
            chrome::selection(c, r, theme);
        }
        draw_row(c, r, theme, i, i == SELECTED);
    });
    chrome::scrollbar(c, f.content, theme, state.scrollbar(&rows, band.height()));

    chrome::softkey_bar(
        c,
        f.softkeys,
        theme,
        chrome::Softkeys::new(Some("Options"), Some("Open"), Some("Back")),
    );
}

// ------------------------------------------------------------------------------- the comparison

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Scene {
    /// The chrome, with the row drawing shared by both sides.
    Chrome,
    /// The row rebuilt out of `Row` and `Text` on the declarative side only.
    Rows,
}

impl Scene {
    fn name(self) -> &'static str {
        match self {
            Scene::Chrome => "chrome",
            Scene::Rows => "rows",
        }
    }
}

fn render(theme: &Theme<'_>, draw: impl FnOnce(&mut Canvas<'_>, &Theme<'_>)) -> Sheet {
    let mut sheet = Sheet::new(E72_SCREEN);
    {
        let mut c = sheet.canvas();
        draw(&mut c, theme);
    }
    sheet
}

fn render_declared(theme: &Theme<'_>, scene: Scene) -> Sheet {
    let mut slots = SlotTable::new();
    slots.begin_frame();
    let screen = declared(&mut slots, scene);
    let sheet = render(theme, |c, t| screen.draw(c, Rect::from_size(E72_SCREEN), t));
    slots.end_frame();
    sheet
}

/// A horizontal run of differing pixels on one scanline.
#[derive(Debug)]
struct Run {
    y: i32,
    x0: i32,
    x1: i32,
}

impl Run {
    fn width(&self) -> i32 {
        self.x1 - self.x0
    }
}

/// Every differing pixel, grouped into runs.
///
/// Runs rather than a count, because a count answers "are they different" and the useful question
/// is "different *where*" — a stripe across one scanline is a band boundary off by one, a blob at a
/// fixed x is a label in the wrong slot, and scattered single pixels are a font that resolved
/// differently. The shape of the diff names the bug.
fn diff(a: &[u16], b: &[u16], size: Size) -> Vec<Run> {
    let mut runs = Vec::new();
    for y in 0..size.h {
        let mut open: Option<i32> = None;
        for x in 0..size.w {
            let i = (y * size.w + x) as usize;
            match (a[i] != b[i], open) {
                (true, None) => open = Some(x),
                (false, Some(x0)) => {
                    runs.push(Run { y, x0, x1: x });
                    open = None;
                }
                _ => {}
            }
        }
        if let Some(x0) = open {
            runs.push(Run { y, x0, x1: size.w });
        }
    }
    runs
}

/// Which band a scanline belongs to, so a run can be reported as "in the softkey bar" rather than
/// as "at y=228".
fn band_of(y: i32, f: &Frame) -> &'static str {
    if y >= f.title.y0 && y < f.title.y1 {
        "title bar"
    } else if y >= f.softkeys.y0 && y < f.softkeys.y1 {
        "softkey bar"
    } else if y >= f.content.y0 && y < f.content.y1 {
        "content"
    } else {
        "outside every band"
    }
}

fn report(runs: &[Run], f: &Frame) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let pixels: i32 = runs.iter().map(Run::width).sum();
    let _ = writeln!(out, "{} differing pixels in {} runs:", pixels, runs.len());
    // The band summary before the detail: which band a difference is in narrows the cause faster
    // than any individual run does, and forty lines of scanlines buries it otherwise.
    for band in ["title bar", "content", "softkey bar", "outside every band"] {
        let n: i32 = runs.iter().filter(|r| band_of(r.y, f) == band).map(Run::width).sum();
        if n > 0 {
            let rows = runs.iter().filter(|r| band_of(r.y, f) == band).count();
            let _ = writeln!(out, "  {band}: {n} px in {rows} runs");
        }
    }
    for r in runs.iter().take(40) {
        let _ = writeln!(
            out,
            "  y={:3} x={:3}..{:3} ({:3} px)  {}",
            r.y,
            r.x0,
            r.x1,
            r.width(),
            band_of(r.y, f)
        );
    }
    if runs.len() > 40 {
        let _ = writeln!(out, "  ... and {} more runs", runs.len() - 40);
    }
    out
}

/// A sheet marking every differing pixel, over a dimmed copy of the declarative render.
///
/// Dimmed rather than blanked: a magenta mark floating on black says *that* something differs, and
/// a mark sitting on the screen it came from says *what*.
fn diff_sheet(declared_px: &[u16], runs: &[Run], size: Size) -> Sheet {
    let mut sheet = Sheet::new(size);
    {
        let mut c = sheet.canvas();
        for y in 0..size.h {
            for x in 0..size.w {
                let src = Rgb565(declared_px[(y * size.w + x) as usize]).to_color();
                let dim = Color::rgb(src.r() / 3, src.g() / 3, src.b() / 3);
                c.fill_rect(Rect::from_xywh(x, y, 1, 1), dim);
            }
        }
        for r in runs {
            c.fill_rect(Rect::new(r.x0, r.y, r.x1, r.y + 1), Color::hex(0xFF00FF));
        }
    }
    sheet
}

/// The chrome scene wearing the [`DeclarativeApp`] contract, so the comparison can also be run down
/// the path a migrated application actually takes.
///
/// Drawing a [`Screen`] directly is not what an app does. An app returns a [`Node`] from `view` and
/// the bridge drives [`crate::layout::draw_frame`] over a [`UiCache`] that outlives the frame — and
/// a cache that outlives the frame is the one thing in this crate that can make two renders agree
/// for the wrong reason. If a stale rect answered as current, both sides of the comparison would
/// keep painting last frame's screen and the PNGs would match beautifully while proving nothing.
struct Compared;

impl DeclarativeApp for Compared {
    type Model = ();
    type Message = Msg;
    type Screen = ();
    const TITLE: &'static str = "Recent";

    fn init() -> Self::Model {}

    fn update(_model: &mut (), _msg: Msg) -> Cmd<()> {
        Cmd::None
    }

    fn view(_model: &(), slots: &mut SlotTable) -> Node {
        Node::leaf(declared(slots, Scene::Chrome))
    }
}

/// Draw `frames` consecutive frames through the bridge into one sheet, so the last one is what
/// comes back.
///
/// More than one frame on purpose: the first fills the cache and the second reads it. A cache whose
/// generation never advanced would hand out the first frame's rects on the second, and the only way
/// to see that from outside is to render twice and compare.
fn render_through_bridge(theme: &Theme<'_>, frames: usize) -> Sheet {
    use symbian_ui::App as _;

    let mut bridge = DeclarativeAppBridge::<Compared>::new();
    let mut sheet = Sheet::new(E72_SCREEN);
    for _ in 0..frames {
        let mut c = sheet.canvas();
        bridge.draw(&mut c, theme);
    }
    sheet
}

/// Render both sides of one scene and diff them. Returns the runs and, for a caller that wants to
/// write files, both sheets.
fn compare(theme: &Theme<'_>, scene: Scene) -> (Sheet, Sheet, Vec<Run>) {
    let a = render_declared(theme, scene);
    let b = render(theme, by_hand);
    let runs = diff(a.pixels(), b.pixels(), E72_SCREEN);
    (a, b, runs)
}

fn main() -> ExitCode {
    let atlases = Atlases::load();
    let mut worst = 0usize;

    atlases.with_themes(|dark, _light| {
        let f = Frame::split(Rect::from_size(E72_SCREEN), dark, true, true);
        for scene in [Scene::Chrome, Scene::Rows] {
            let (declared_sheet, hand_sheet, runs) = compare(dark, scene);
            declared_sheet.save(OUT, &format!("{}-declared", scene.name()));
            hand_sheet.save(OUT, &format!("{}-by-hand", scene.name()));

            if runs.is_empty() {
                println!("scene {}: identical", scene.name());
                if scene == Scene::Chrome {
                    // The same scene down the path a migrated application takes: `view` returning a
                    // `Node`, the bridge owning the cache, and a second frame drawn against it.
                    // Saved as well as asserted, because "the app path renders the same screen" is
                    // the claim a human should be able to check by looking.
                    let bridged = render_through_bridge(dark, 2);
                    let runs = diff(bridged.pixels(), declared_sheet.pixels(), E72_SCREEN);
                    bridged.save(OUT, "chrome-via-bridge");
                    println!(
                        "  via the bridge, second frame: {}",
                        if runs.is_empty() { "identical" } else { "DIFFERS" }
                    );
                    if !runs.is_empty() {
                        print!("{}", report(&runs, &f));
                        worst = worst.max(runs.len());
                    }
                }
                println!();
            } else {
                diff_sheet(declared_sheet.pixels(), &runs, E72_SCREEN)
                    .save(OUT, &format!("{}-diff", scene.name()));
                println!("scene {}: DIFFERS", scene.name());
                print!("{}", report(&runs, &f));
                println!();
                worst = worst.max(runs.len());
            }
        }
    });

    // `main` reports and does not judge: the assertions live in the tests, so a developer can look
    // at the PNGs even while something is red. Both scenes are expected to be identical now; the
    // one difference this file found and could not fix is the row that bleeds over the title bar,
    // and it bleeds identically on both sides, so it does not show up here at all.
    if worst > 0 {
        println!("see {OUT}/*-diff.png");
    }
    ExitCode::SUCCESS
}


#[cfg(test)]
mod tests {
    use super::*;
    use symbian_decl_ui::cache::UiCache;
    use symbian_decl_ui::layout::{layout_tree, measure_tree, CrossAlign};

    /// The device's real atlases, not the one-glyph test font. A parity test against a font where
    /// every character has the same advance would agree about text that is not laid out the same
    /// way at all.
    fn with_real_theme<R>(f: impl FnOnce(&Theme<'_>) -> R) -> R {
        let atlases = Atlases::load();
        atlases.with_fonts(|fonts| f(&symbian_ui::Theme::dark(fonts)))
    }

    fn frame(theme: &Theme<'_>) -> Frame {
        Frame::split(Rect::from_size(E72_SCREEN), theme, true, true)
    }

    // ---- the acceptance criterion -------------------------------------------------------------

    #[test]
    fn the_declared_chrome_is_the_hand_written_chrome_pixel_for_pixel() {
        // Phase 2's acceptance criterion, and the only thing that can prove it. If this goes red,
        // read the PNGs before touching either side: `cargo run -p symbian-decl-ui --example
        // compare` writes them, including a magenta diff.
        with_real_theme(|theme| {
            let (_, _, runs) = compare(theme, Scene::Chrome);
            assert!(runs.is_empty(), "\n{}", report(&runs, &frame(theme)));
        });
    }

    #[test]
    fn the_bridge_draws_the_same_screen_and_a_warm_cache_does_not_go_stale() {
        // The same chrome scene down the path an application actually takes: `view` returns a
        // `Node`, the bridge owns the `UiCache` and the `SlotTable`, and `draw_frame` starts the
        // frame. Three claims, in order of what they would hide if they failed.
        with_real_theme(|theme| {
            let f = frame(theme);
            let direct = render_declared(theme, Scene::Chrome);

            // One frame through the bridge is the same pixels as drawing the screen by hand. If it
            // were not, everything above this test would be measuring a path nobody runs.
            let once = render_through_bridge(theme, 1);
            let runs = diff(once.pixels(), direct.pixels(), E72_SCREEN);
            assert!(runs.is_empty(), "bridge vs direct:\n{}", report(&runs, &f));

            // And the second frame, drawn against a cache the first one filled, is the same again.
            // This is the failure the cache can produce and no arithmetic test can see: a frame
            // that was never begun leaves every rect from last frame answering as current, so the
            // screen keeps painting where it used to be and looks perfectly correct until a branch
            // leaves the tree. Two identical frames is not proof on its own — a screen that drew
            // nothing at all would pass it too — which is why it is asserted against `direct`
            // rather than against the first frame.
            let twice = render_through_bridge(theme, 2);
            let runs = diff(twice.pixels(), direct.pixels(), E72_SCREEN);
            assert!(runs.is_empty(), "second frame vs direct:\n{}", report(&runs, &f));

            // The belt-and-braces half of that: the screen has ink on it, so "identical" is not two
            // blank buffers agreeing.
            assert!(
                twice.pixels().iter().any(|&p| p != 0),
                "the bridge rendered an empty screen and every comparison above passed anyway"
            );
        });
    }

    #[test]
    fn the_comparison_would_notice_if_it_were_lied_to() {
        // A parity test that cannot fail is worse than none: it reads as a proof and is a constant.
        // One deliberately wrong screen, to show the machinery has teeth — and the wrongness is the
        // exact defect `keys.rs` exists to prevent, a middle and a right label transposed, which no
        // arithmetic test in this crate would catch.
        with_real_theme(|theme| {
            let declared = render_declared(theme, Scene::Chrome);
            let wrong = render(theme, |c, t| {
                let screen = Rect::from_size(E72_SCREEN);
                chrome::clear(c, t);
                let f = Frame::split(screen, t, true, true);
                chrome::title_bar(c, f.title, t, "Recent", Some("online"));
                chrome::softkey_bar(
                    c,
                    f.softkeys,
                    t,
                    chrome::Softkeys::new(Some("Options"), Some("Back"), Some("Open")),
                );
            });
            let runs = diff(declared.pixels(), wrong.pixels(), E72_SCREEN);
            assert!(!runs.is_empty(), "the comparison passed two different screens");
            let bar = frame(theme).softkeys;
            assert!(
                runs.iter().any(|r| r.y >= bar.y0 && r.y < bar.y1),
                "a transposed softkey label must show up in the softkey band"
            );
        });
    }

    // ---- what the comparison found ------------------------------------------------------------

    #[test]
    fn a_row_built_out_of_widgets_is_the_hand_written_row_pixel_for_pixel() {
        // Scene B, and the claim that actually lets a real list be migrated: nothing is shared
        // between the two sides here. The declarative row is a `Row` with padding, a gap, a
        // flexible name and a timestamp sized to its own text; the hand-written one is a
        // `split_right` and a subtraction. They agree on every pixel, selection colours and
        // truncating ellipsis included.
        with_real_theme(|theme| {
            let (_, _, runs) = compare(theme, Scene::Rows);
            assert!(runs.is_empty(), "\n{}", report(&runs, &frame(theme)));
        });
    }

    #[test]
    fn the_row_reaches_parity_because_of_its_cross_axis_alignment_and_not_by_luck() {
        // The finding this file was written to produce, kept as the reason the test above passes.
        //
        // A group used to leave every child at its measured cross-axis size, anchored to the start
        // of the line. In a 38-pixel list row that puts a 17-pixel line of text at y=0..17 rather
        // than centred at y=10..27 — every row on the screen drawn ten pixels high, which is
        // obvious once rendered and invisible in any arithmetic test. `CrossAlign` is the fix, and
        // `.align(CrossAlign::Stretch)` in `declared_row` is load-bearing rather than decorative.
        //
        // Both halves are asserted: that the default still anchors to the start (so this test is
        // describing the engine and not a wish), and that stretching is what produces the row.
        // Deleting the `.align` call turns the second assertion red before the pixels ever get a
        // chance to disagree, which is a much easier failure to read.
        with_real_theme(|theme| {
            let band_w = E72_SCREEN.w - chrome::scrollbar_gutter(theme);
            let row_rect = Rect::from_xywh(0, frame(theme).content.y0, band_w, ROW_H);
            let line_h = theme.fonts.strong.line_height();
            assert!(row_rect.height() > line_h, "a row must be taller than a line or this is moot");

            let name_rect = |g: symbian_decl_ui::widgets::Group| {
                let node = Node::Group(g);
                let mut cache = UiCache::new();
                // Written out rather than reached through `layout::draw_frame`, because this wants
                // the rects and not the pixels and there is no canvas here. `begin_frame` is
                // exactly the line `draw_frame` exists to stop anyone forgetting: without it the
                // generation never advances and last frame's rects keep answering as current.
                cache.begin_frame();
                measure_tree(
                    &node,
                    Constraints::tight(row_rect.width(), row_rect.height()),
                    theme,
                    &mut cache,
                );
                layout_tree(&node, row_rect, &mut cache, theme);
                cache.rect(1).expect("the name child was not placed")
            };

            let stretched = name_rect(declared_row(0, false));
            assert_eq!(
                (stretched.y0, stretched.height()),
                (row_rect.y0, row_rect.height()),
                "the name must get the whole row height so `draw_text_in` can centre in it"
            );

            let anchored = name_rect(declared_row(0, false).align(CrossAlign::Start));
            assert_eq!(
                (anchored.y0, anchored.height()),
                (row_rect.y0, line_h),
                "the default is still start-anchored; if it is not, `declared_row` no longer needs \
                 its `.align` call and this test is describing something that changed"
            );
        });
    }

    #[test]
    fn neither_layer_lets_a_scrolled_row_draw_over_the_title_bar() {
        // A defect in what shipped, found by rendering it rather than by reading it, and since
        // fixed. Kept as the test that would notice it coming back.
        //
        // `ListState::for_visible` reports the partially-visible first row with a rect that starts
        // above the viewport — which is right, that is what partial means. What was missing was
        // anyone trimming it: neither the toolkit's three row loops, nor `ScrollList`, nor
        // `bootctl`'s two clipped to the band, so the top row's text landed on the title bar. 924
        // pixels of it in this scene, on a dark bar under dark text, which is how it survived years
        // of use.
        //
        // The fix is `ListState::draw_visible` — the same walk with the canvas passed through and
        // the clip applied — and it is in `symbian-ui` rather than in either drawing layer, because
        // the count at the time was eight loops that had forgotten and two in the Telegram client
        // that had each hand-rolled the same `clip_to`. A defect with a known workaround is not a
        // fixed defect.
        with_real_theme(|theme| {
            let f = frame(theme);
            let rows = Uniform { count: CHATS.len(), height: ROW_H };
            let mut state = ListState::new();
            state.select(SELECTED, &rows, f.content.height());
            assert!(state.scroll > 0, "the scene must scroll or there is no partial row");
            assert!(
                state.scroll % ROW_H != 0,
                "and the scroll must land mid-row, or the top row is not partial"
            );

            // The same screen with the list left out: everything above the content band in the
            // full render that is not in this one is row ink that escaped.
            let chrome_only = render(theme, |c, t| {
                chrome::clear(c, t);
                let f = Frame::split(Rect::from_size(E72_SCREEN), t, true, true);
                chrome::title_bar(c, f.title, t, "Recent", Some("online"));
                chrome::softkey_bar(
                    c,
                    f.softkeys,
                    t,
                    chrome::Softkeys::new(Some("Options"), Some("Open"), Some("Back")),
                );
            });

            let bleed = |sheet: &Sheet| -> i32 {
                diff(sheet.pixels(), chrome_only.pixels(), E72_SCREEN)
                    .iter()
                    .filter(|r| r.y < f.title.y1)
                    .map(Run::width)
                    .sum()
            };

            // The negative control, and the reason `for_visible` still exists and is still public:
            // the same scene drawn with the unclipped walk bleeds, so this test is measuring the
            // clip and not measuring a scene that never overflowed. Without this the whole test
            // would keep passing if the row loop stopped drawing anything at all.
            let unclipped = render(theme, |c, t| {
                let f = Frame::split(Rect::from_size(E72_SCREEN), t, true, true);
                let band = Rect { x1: f.content.x1 - chrome::scrollbar_gutter(t), ..f.content };
                let mut st = ListState::new();
                st.select(SELECTED, &rows, f.content.height());
                chrome::clear(c, t);
                chrome::title_bar(c, f.title, t, "Recent", Some("online"));
                st.for_visible(&rows, band, |i, r| draw_row(c, r, t, i, i == SELECTED));
            });
            assert!(
                bleed(&unclipped) > 0,
                "the scene must actually overflow the band, or this test proves nothing"
            );

            assert_eq!(bleed(&render(theme, by_hand)), 0, "the hand-written screen bleeds");
            assert_eq!(bleed(&render_declared(theme, Scene::Chrome)), 0, "the declared screen bleeds");
        });
    }

    // ---- the scene is worth rendering ---------------------------------------------------------

    #[test]
    fn the_scene_scrolled_so_the_list_geometry_is_actually_exercised() {
        // A comparison of two screens that both show rows 0..5 would never test the offset the
        // declarative side derives and the model never names. If this stops holding, the parity
        // test above quietly stops covering scrolling.
        with_real_theme(|theme| {
            let f = frame(theme);
            let rows = Uniform { count: CHATS.len(), height: ROW_H };
            let mut state = ListState::new();
            state.select(SELECTED, &rows, f.content.height());
            assert!(state.scroll > 0, "the scene must scroll or it proves less than it looks");
            assert!(
                state.scrollbar(&rows, f.content.height()).is_some(),
                "and it must have a scrollbar thumb, or the gutter is untested"
            );
        });
    }

    #[test]
    fn a_name_in_the_scene_is_long_enough_to_be_truncated() {
        // The scene is only worth rendering if it has the widths a real list has. A name that fits
        // easily never exercises the ellipsis, and the ellipsis is chosen by the font rather than
        // by either layer — which makes it exactly the thing a shared-leaf comparison could hide.
        with_real_theme(|theme| {
            // The room a name actually gets: the band without the scrollbar gutter, without the
            // padding on both sides, without the timestamp, without the gap before it.
            let band_w = E72_SCREEN.w - chrome::scrollbar_gutter(theme);
            let pad = theme.metrics.pad;
            let (name, time) = CHATS[SELECTED];
            let room = band_w - pad * 3 - theme.fonts.small.measure(time);
            let wanted = theme.fonts.strong.measure(name);
            assert!(
                wanted > room,
                "the selected row's name wants {wanted}px and has {room}px; nothing is truncated \
                 and the scene never renders an ellipsis"
            );
        });
    }
}
