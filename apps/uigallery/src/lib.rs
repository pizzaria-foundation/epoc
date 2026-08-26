//! The whole component catalogue, a page at a time, with every row wearing its own name.
//!
//! # Why pages and not one screen per component
//!
//! Twenty-odd detail screens is twenty-odd chances to get the same skeleton wrong, and whoever is
//! holding the phone would spend the session pressing Back. A handful of pages puts every component
//! under one cursor: Down walks from a `Switch` to a `Stepper` to a `Slider`, which is what a person
//! actually does with a keypad — so a defect in the *assembly* shows up, not only defects in the
//! pieces.
//!
//! Every row is labelled with the type it shows. That is not decoration. "The third thing looks
//! wrong" is a report nobody can act on; "the `Stepper` row draws its arrows a pixel low" names the
//! file.
//!
//! # Why pages and not one long scrolling list
//!
//! The first version of this app was a single `ScrollList` of everything. It does not compile, and
//! what stops it is a property of the library rather than a mistake here — worth writing down.
//!
//! A list's row builder is `Fn(usize, bool) -> Node` and is handed **no slot table**. `scroll_list.rs`
//! says why: rows are built inside `draw`, so the only identity available to key a slot by would be
//! the row's *position on screen*, and state keyed that way slides one row up the list every time it
//! scrolls. So **a widget with state cannot be a list row** — `TextField`, `SearchField` and
//! `DateTime` each keep a caret or a cursor in the slot table, and none of them could appear in one.
//!
//! `view` *is* handed the table. So the gallery is a
//! [`FocusScope`](symbian_decl_ui::widgets::FocusScope) of a few rows per page, which is also what the
//! handset does: an S60 settings screen is a page of a few rows, not an infinite list.
//!
//! # The action softkey is deliberately unlabelled
//!
//! `Screen` gives the softkey bar every key **first and unconditionally**, and the bar owns
//! `Key::Select`. A screen that labels its middle slot therefore takes the centre key away from
//! whatever has the focus — so a `Switch` would never flip and a drop-down would never open. Leaving
//! it blank is what lets `Select` reach the focused control, which is the whole point of a gallery.
//!
//! That is the rule for any screen whose content answers the centre key, and nothing enforces it,
//! which is why it is written here as well as on `Select`.
//!
//! # The keys
//!
//! | key | what it does | why there |
//! |---|---|---|
//! | Up / Down | move between rows | the `FocusScope`'s own axis |
//! | Left / Right | adjust the focused control | a vertical scope declines them, so they reach the control |
//! | centre | act on the focused control | the bar leaves it alone — see above |
//! | left softkey | next page | the primary navigation, so it gets the label |
//! | right softkey | exit | on this platform it is never anything else |
//! | `#` | cycle the five palettes | both softkeys are spent, and `#` is under the thumb |
//!
//! # The instruments are read by the harness, not by the app
//!
//! [`Uigallery`] wraps the bridge and reads `measure_calls`, `type_mismatches` and
//! `unbalanced_groups` after each frame into `Cell`s the *next* frame draws. One frame late, which on
//! a still screen is exact — and it means the instruments cannot perturb the tree they measure, which
//! a row computing them inside `view` would.
//!
//! `f=0` is the one that must hold: a non-zero fault count means a slot ordinal shifted and some
//! widget is holding its neighbour's state. `m` is the measure count, and on this screen it will
//! *not* be 1 — see [`PageBody`] for why, and why that is a knowing trade rather than a defect.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use core::cell::Cell;
use core::ops::Range;

use symbian_decl_ui::app::DeclarativeApp;
use symbian_decl_ui::bridge::DeclarativeAppBridge;
use symbian_decl_ui::cmd::Cmd;
use symbian_decl_ui::layout::CrossAlign;
use symbian_decl_ui::outbox::Outbox;
use symbian_decl_ui::slot::SlotTable;
use symbian_decl_ui::spacing::{Gap, Pad};
use symbian_decl_ui::theme::FontRole;
use symbian_decl_ui::widgets::Screen as ScreenWidget;
use symbian_decl_ui::widgets::{
    Avatar, Badge, Button, Card, Checkbox, Chip, Collapsible, Column, DateTime, Divider, EmptyState,
    FieldRow, Flow, FocusScope, Icon, Ink, ListItem, Marquee, Node, Notice, NoticeTone, ProgressBar,
    DetailSheet, Dialog, Drawer, OptionMenu, Row, SearchField, SectionHeader, Slider, Spinner,
    Stack, Stepper, Switch, Tabs, Text, TextField, TitleBar,
};
use symbian_ui::calendar::Stamp;
use symbian_ui::icon::Icon as Glyph;
use symbian_ui::{Canvas, Color, Handled, Key, KeyEvent, Palette, Rect, Theme};

// ---------------------------------------------------------------- the catalogue, as data

/// What kind of row an entry needs.
///
/// A table rather than a `match` in two places: the page split and the row builder both have to agree
/// about what entry `i` is, and two answers to that is how a cursor ends up on a heading.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Kind {
    /// A heading. Added through `FocusScope::fixed`, so no cursor reaches it.
    Heading,
    Demo(Demo),
}

/// Which component a row shows. Named after the type, so the label on screen and the file to open are
/// the same word.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Demo {
    Text,
    TextDim,
    Marquee,
    Icons,
    Avatar,
    Badge,
    Divider,
    Switch,
    Checkbox,
    RadioA,
    RadioB,
    Stepper,
    Slider,
    Button,
    SearchField,
    Date,
    Time,
    RowTwoLine,
    RowIconArrow,
    RowValue,
    Flow,
    FieldRow,
    Instruments,
    Progress,
    Spinner,
    Chips,
    Notice,
    Empty,
    Card,
    Tabs,
    Collapsible,
    Error,
    OpenDialog,
    OpenMenu,
    SheetHint,
    DrawerHint,
}

/// The catalogue, in order. Adding a component is one line here and one arm in [`demo_row`].
const ENTRIES: &[(Kind, &str)] = &[
    (Kind::Heading, "Text"),
    (Kind::Demo(Demo::Text), "Text"),
    (Kind::Demo(Demo::TextDim), "Text dim"),
    (Kind::Demo(Demo::Marquee), "Marquee"),
    (Kind::Heading, "Ink and shapes"),
    (Kind::Demo(Demo::Icons), "Icon in a Flow"),
    (Kind::Demo(Demo::Avatar), "Avatar"),
    (Kind::Demo(Demo::Badge), "Badge"),
    (Kind::Demo(Demo::Divider), "Divider"),
    (Kind::Heading, "Controls"),
    (Kind::Demo(Demo::Switch), "Switch"),
    (Kind::Demo(Demo::Checkbox), "Checkbox"),
    (Kind::Demo(Demo::RadioA), "radio A"),
    (Kind::Demo(Demo::RadioB), "radio B"),
    (Kind::Heading, "Controls II"),
    (Kind::Demo(Demo::Stepper), "Stepper"),
    (Kind::Demo(Demo::Slider), "Slider"),
    (Kind::Demo(Demo::Button), "Button"),
    (Kind::Heading, "Typing"),
    (Kind::Demo(Demo::SearchField), "SearchField"),
    (Kind::Demo(Demo::FieldRow), "FieldRow"),
    (Kind::Heading, "Date and time"),
    (Kind::Demo(Demo::Date), "DateTime::date"),
    (Kind::Demo(Demo::Time), "DateTime::time"),
    (Kind::Heading, "Rows"),
    (Kind::Demo(Demo::RowTwoLine), "two-line"),
    (Kind::Demo(Demo::RowIconArrow), "icon + arrow"),
    (Kind::Demo(Demo::RowValue), "value"),
    (Kind::Heading, "Layout"),
    (Kind::Demo(Demo::Flow), "Flow wrap"),
    (Kind::Demo(Demo::Instruments), "Instruments"),
    (Kind::Heading, "Feedback"),
    (Kind::Demo(Demo::Progress), "ProgressBar"),
    (Kind::Demo(Demo::Spinner), "Spinner"),
    (Kind::Demo(Demo::Chips), "Chip, four tones"),
    (Kind::Demo(Demo::Notice), "Notice"),
    (Kind::Heading, "Ground and space"),
    (Kind::Demo(Demo::Card), "Card"),
    (Kind::Demo(Demo::Empty), "EmptyState"),
    (Kind::Heading, "Navigation"),
    (Kind::Demo(Demo::Tabs), "Tabs"),
    (Kind::Demo(Demo::Collapsible), "Collapsible"),
    (Kind::Heading, "Overlays"),
    (Kind::Demo(Demo::OpenDialog), "Dialog"),
    (Kind::Demo(Demo::OpenMenu), "OptionMenu"),
    (Kind::Demo(Demo::SheetHint), "DetailSheet"),
    (Kind::Demo(Demo::DrawerHint), "Drawer"),
    (Kind::Heading, "Error ink"),
    (Kind::Demo(Demo::Error), "Ink::Error"),
];

/// Where each page starts and ends, as indices into [`ENTRIES`].
///
/// Cut at the headings so a page is a section, and kept to five entries because that is what fits
/// between a title bar and a softkey bar at 240 pixels. A sixth row would be laid out and
/// unreachable, which is worse than a page break.
const PAGES: &[Range<usize>] =
    &[
        0..4, 4..9, 9..14, 14..18, 18..21, 21..24, 24..28, 28..31, 31..36, 36..39, 39..42,
        42..47, 47..ENTRIES.len(),
    ];

// ---------------------------------------------------------------- the app

/// Everything the gallery knows.
///
/// Every field is a *component's value*, changed by `update` and by nothing else. That is the point of
/// a gallery on a handset: if a switch flips on screen, the message reached `update` — the half a host
/// test cannot prove about a physical keypad.
///
/// What is **not** here is the cursor. It belongs to the `FocusScope`, in the slot table, because
/// which row has focus is a consequence of having drawn this page and not something a `Cmd` is made
/// of. See `focus.rs`.
pub struct Model {
    /// Which page, as an index into [`PAGES`].
    page: usize,
    palette: usize,
    /// The phone's own theme, derived once at start-up, or `None` if there is no phone or the theme
    /// is not legible. Held in the model so `view` can name it and `update` can cycle onto it.
    ///
    /// A `Cell` because the bridge hands out `&Model` and a host harness — the contact sheets — has to
    /// be able to inject a theme it measured elsewhere. Same reason the instruments are cells.
    phone: Cell<Option<Palette>>,
    switch_on: bool,
    checkbox_on: bool,
    /// Which radio is chosen: `false` is A, `true` is B.
    radio_b: bool,
    retries: i32,
    volume: i32,
    presses: u32,
    /// The marquee's tick, advanced by a timer rather than a key — so a still screen still moves.
    ///
    /// Also the spinner's: both want "something moved since the last frame" and there is one timer.
    phase: u32,
    /// Which tab the strip is on. In the model rather than in a slot because `view` runs before
    /// dispatch, and it is `view` that picks the panel — see `tabs.rs`.
    tab: usize,
    /// Which model-driven overlay is up: 0 none, 1 dialog, 2 options. `DetailSheet` and `Drawer` are
    /// not here — they hold their own open flag in the slot table and answer a key themselves, which
    /// is the difference this page exists to show.
    overlay: u8,
    /// What the last overlay answered, so a closed overlay leaves evidence it was ever open.
    answered: Option<usize>,
    when: Stamp,
    /// Last frame's instruments. `Cell` because the harness writes them through the `&Model` the
    /// bridge hands out; see the module docs on why they are a frame late.
    measures: Cell<u32>,
    faults: Cell<u32>,
    out: Outbox<Msg>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Msg {
    /// Turn to the next page, wrapping.
    ///
    /// One direction only: both softkeys are spent, and a "previous" would have to take a key from a
    /// control. Wrapping through eight pages is a few presses at worst.
    NextPage,
    CyclePalette,
    Quit,
    Tick,
    ToggleSwitch,
    ToggleCheckbox,
    ChooseA,
    ChooseB,
    Press,
    SetRetries(i32),
    SetVolume(i32),
    SetWhen(Stamp),
    SetTab(usize),
    OpenOverlay(u8),
    CloseOverlay,
    Answered(usize),
}

/// How often the marquee moves. Four a second reads as motion without making a still screen busy.
const TICK_MS: i32 = 250;

/// The gallery's `DeclarativeApp`. [`Uigallery`] is what the host drives.
pub struct Gallery;

impl DeclarativeApp for Gallery {
    type Model = Model;
    type Message = Msg;
    type Screen = ();
    const TITLE: &'static str = "UI gallery";

    fn init() -> Model {
        Model {
            page: 0,
            palette: 0,
            phone: Cell::new(read_phone_theme()),
            switch_on: false,
            checkbox_on: true,
            radio_b: false,
            retries: 3,
            volume: 40,
            presses: 0,
            phase: 0,
            tab: 0,
            overlay: 0,
            answered: None,
            // The 31st on purpose: stepping the month to February is the case `Stamp::with_part`
            // exists to clamp, and the one worth pressing on a handset.
            when: Stamp::date(2024, 1, 31),
            measures: Cell::new(0),
            faults: Cell::new(0),
            out: Outbox::new(),
        }
    }

    /// One key, and everything else falls through.
    ///
    /// There is no `keys()` implementation: the softkeys are declared on the `Screen` widget and
    /// delivered through its outbox, so there is exactly one declaration of the bar. `on_key` claims
    /// only `#`, because every other key belongs to something in the tree — the scope's cursor, or the
    /// control that has it.
    fn on_key(_m: &Model, ev: KeyEvent) -> Option<Msg> {
        match ev.key {
            Key::Char('#') => Some(Msg::CyclePalette),
            _ => None,
        }
    }

    fn update(m: &mut Model, msg: Msg) -> Cmd<()> {
        match msg {
            Msg::NextPage => {
                m.page = (m.page + 1) % PAGES.len();
                Cmd::None
            }
            Msg::CyclePalette => {
                // `Palette::count`, never `Palette::ALL.len()`. The phone's own palette lives outside
                // the const array, so a cycler that used the array's length would step over it for
                // ever — no compile error, no symptom but a key that appears to work.
                m.palette = (m.palette + 1) % Palette::count(m.phone.get());
                PALETTE.store(m.palette);
                PHONE.store(m.phone.get());
                Cmd::None
            }
            Msg::Tick => {
                m.phase = m.phase.wrapping_add(1);
                Cmd::None
            }
            Msg::ToggleSwitch => {
                m.switch_on = !m.switch_on;
                Cmd::None
            }
            Msg::ToggleCheckbox => {
                m.checkbox_on = !m.checkbox_on;
                Cmd::None
            }
            // "Set this", never "toggle this": a radio group whose chosen option could be pressed off
            // leaves the model with no value and the user no way back.
            Msg::ChooseA => {
                m.radio_b = false;
                Cmd::None
            }
            Msg::ChooseB => {
                m.radio_b = true;
                Cmd::None
            }
            Msg::Press => {
                m.presses += 1;
                Cmd::None
            }
            Msg::SetRetries(v) => {
                m.retries = v;
                Cmd::None
            }
            Msg::SetVolume(v) => {
                m.volume = v;
                Cmd::None
            }
            Msg::SetWhen(s) => {
                m.when = s;
                Cmd::None
            }
            Msg::OpenOverlay(n) => {
                m.overlay = n;
                Cmd::None
            }
            Msg::CloseOverlay => {
                m.overlay = 0;
                Cmd::None
            }
            Msg::Answered(n) => {
                m.answered = Some(n);
                m.overlay = 0;
                Cmd::None
            }
            Msg::SetTab(n) => {
                m.tab = n;
                Cmd::None
            }
            Msg::Quit => Cmd::Exit,
        }
    }

    /// The channel every control reports through — including the softkey bar.
    fn outbox(m: &Model) -> Option<&Outbox<Msg>> {
        Some(&m.out)
    }

    fn view(m: &Model, slots: &mut SlotTable) -> Node {
        // Keyed by page, so the cursor and every caret on it are found by *which page* rather than by
        // where the page sits among its siblings. Without the key, turning to a page of a different
        // shape hands each widget its predecessor's state — the failure `slot.rs` documents, and the
        // one thing about this screen no amount of looking would reveal.
        let page = slots.group(m.page as u64, |slots| build_page(m, slots));

        Node::leaf(
            ScreenWidget::new()
                .out(m.out.clone())
                .title_bar(TitleBar::new(Gallery::TITLE).detail(Palette::at(m.palette, m.phone.get()).0))
                .content(PageBody(page))
                .on_options(format!("Page {}/{}", m.page + 1, PAGES.len()), Msg::NextPage)
                // The action slot stays empty on purpose. See the module docs: a labelled middle slot
                // takes `Select` away from whatever has the focus.
                .on_back("Exit", Msg::Quit),
        )
    }
}

/// A `Node` wrapped so it can be handed to [`ScreenWidget::content`], which takes a `Widget`.
///
/// # This is the `Group: Widget` compatibility path, entered knowingly
///
/// `Screen::content` takes `impl Widget`, so a `Node` has to become one — and a group reached through
/// `&dyn Widget` re-measures its whole subtree every frame, because the cache it builds is born and
/// dropped inside one call. `decl-ui.md` calls it the trap, and the instruments row is what makes it
/// visible rather than invisible here: `m` counts the measures, and on this screen it is **not** 1.
///
/// It is the right trade for a gallery — a page is a dozen nodes and the reviewer is looking at
/// pixels — and it is exactly the kind of thing worth seeing a real number for rather than being told
/// about. A screen that cared would keep its content in a `Node` the bridge owns.
struct PageBody(Node);

impl symbian_decl_ui::Widget for PageBody {
    fn measure(
        &self,
        c: symbian_decl_ui::Constraints,
        theme: &Theme<'_>,
    ) -> symbian_gfx::Size {
        let mut scratch = symbian_decl_ui::UiCache::with_capacity(self.0.slot_count());
        symbian_decl_ui::layout::measure_node(&self.0, 0, c, theme, &mut scratch)
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        let mut scratch = symbian_decl_ui::UiCache::with_capacity(self.0.slot_count());
        symbian_decl_ui::layout::draw_frame(&self.0, rect, &mut scratch, c, theme);
    }

    fn handle_key(
        &self,
        ev: KeyEvent,
        rect: Rect,
        cx: &mut symbian_decl_ui::widget::KeyCtx<'_>,
    ) -> Handled {
        // Placed before dispatching, into a scratch cache, because a key must reach a widget at the
        // rect it was *drawn* at. Dispatching against an empty cache is a page whose controls answer
        // nothing at all — and it would look like a dead keypad rather than like a missing layout.
        let mut scratch = symbian_decl_ui::UiCache::with_capacity(self.0.slot_count());
        symbian_decl_ui::layout::place_frame(&self.0, rect, &mut scratch, cx.theme);
        symbian_decl_ui::layout::dispatch_key(&self.0, ev, &scratch, cx)
    }
}

/// How many pages the gallery has.
///
/// Public so the contact-sheet example can walk them without a second copy of the page table — two
/// answers to "how many pages" is how a sheet ends up missing the last one.
pub fn page_count() -> usize {
    PAGES.len()
}

/// The title of page `n`, for a filename or a caption.
pub fn page_title_of(n: usize) -> &'static str {
    page_title(n.min(PAGES.len() - 1))
}

/// The heading a page belongs to, for a filename or a caption.
///
/// Not used as the title bar's text: the page draws its own `SectionHeader`, and a title bar repeating
/// it was the first thing a contact sheet showed — "Controls" twice, one row apart. The bar carries the
/// app's name and the palette instead, which is what a reviewer needs and the page does not say.
fn page_title(page: usize) -> &'static str {
    for i in PAGES[page].clone() {
        if matches!(ENTRIES[i].0, Kind::Heading) {
            return ENTRIES[i].1;
        }
    }
    Gallery::TITLE
}

/// One page: a focus scope over its entries, headings fixed and demos as stops.
fn build_page(m: &Model, slots: &mut SlotTable) -> Node {
    let mut scope = FocusScope::vertical(slots).gap(Gap::Hair).stretch_width();
    for i in PAGES[m.page].clone() {
        let (kind, label) = ENTRIES[i];
        match kind {
            // `fixed`, not `stop`: this is what keeps the cursor off a heading, and it is the
            // mechanism `ScrollList` has no equivalent of.
            Kind::Heading => scope = scope.fixed(Node::leaf(SectionHeader::new(label))),
            // The closure runs immediately, so it can borrow the table the scope has already finished
            // with — which is what lets a stateful widget be a row here and not in a list.
            Kind::Demo(d) => scope = scope.stop(|focused| demo_row(d, label, focused, m, slots)),
        }
    }
    let page = scope.build();
    if !PAGES[m.page].contains(&overlay_entry()) {
        return page;
    }

    // The overlays live beside the page, never inside the scope: all four measure the whole offer,
    // so a scope that held one would hand it a row's height and get a full frame back. `Stack` is
    // where something that covers the screen goes, and the order here is the z-order.
    let out = m.out.clone();
    Node::leaf(
        Stack::new(slots)
            .layer(page)
            .layer(
                Dialog::new(slots, "Delete conversation?", "This cannot be undone.")
                    .open(m.overlay == 1)
                    .choice("Delete", 1usize)
                    .choice("Keep", 0usize)
                    .out(out.clone(), Msg::Answered)
                    .on_cancel(out.clone(), Msg::CloseOverlay)
                    .build(),
            )
            .layer(
                OptionMenu::new(slots)
                    .open(m.overlay == 2)
                    .item("Mark as read", 10usize)
                    .item("Mute", 11usize)
                    .item("Delete", 12usize)
                    .out(out.clone(), Msg::Answered)
                    .on_cancel(out.clone(), Msg::CloseOverlay)
                    .build(),
            )
            .child(
                DetailSheet::new(slots, "Ana Ribeiro", "+55 71 90000-0000")
                    // Not `Select`: this layer sits over a page whose rows already answer it, and a
                    // sheet that claimed it would open on every row's press. A layer that takes a key
                    // takes it from everything under it — the same fact the tab strip demonstrates.
                    .opens_on(Key::Char('*'))
                    .row(symbian_ui::sheet::Row::pair("Mobile", "+55 71 90000-0000"))
                    .row(symbian_ui::sheet::Row::pair("Email", "ana@example.com"))
                    .action("Call")
                    .out(out.clone(), Msg::Answered),
            )
            .child(
                Drawer::new(slots, 0)
                    // The left softkey is this screen's page turn, so the drawer's default would be
                    // a key with two owners and no error.
                    .opens_on(Key::Char('0'))
                    .section(symbian_ui::drawer::Section::new("All chats").note("128"))
                    .section(symbian_ui::drawer::Section::new("Unread").note("4"))
                    .section(symbian_ui::drawer::Section::new("Groups").note("9"))
                    .out(out, Msg::Answered),
            ),
    )
}

/// The index in [`ENTRIES`] of the first overlay row, so [`build_page`] can tell which page needs
/// the layers without a second copy of the page number.
///
/// A number written twice is a number that goes stale when a page is inserted above it — and the
/// symptom would be overlays that stop opening, with nothing on screen to say why.
fn overlay_entry() -> usize {
    ENTRIES
        .iter()
        .position(|(k, _)| matches!(k, Kind::Demo(Demo::OpenDialog)))
        .expect("the dialog row is in the catalogue")
}

/// A row of this gallery: a [`ListItem`] that paints its own selection band.
///
/// The four rows that are *not* `ListItem`s — `Divider`, `SearchField`, `Flow`, `Instruments` — call
/// `Group::selection_band(sel)` themselves. Before that they were the four stops with no visible
/// cursor at all, which on a page of three rows reads as a D-pad that has stopped working.
///
/// Every row here needs the band and none of them is inside a `ScrollList` — see the module docs — so
/// it is a helper rather than `.band(true)` twenty times. A row added later cannot forget it and be
/// the one row with no visible cursor, which is a defect nobody notices until they are holding the
/// phone and wondering where they are.
fn row(label: &str, sel: bool) -> ListItem {
    ListItem::new(label).selected(sel).band(true)
}



/// A labelled row showing one component.
fn demo_row(d: Demo, label: &str, sel: bool, m: &Model, slots: &mut SlotTable) -> Node {
    let out = m.out.clone();
    match d {
        Demo::Text => {
            row(label, sel).trailing(Text::new("Handgloves 0123")).build()
        }
        Demo::TextDim => {
            row(label, sel).trailing(Text::new("quieter").dim()).build()
        }
        // A label wider than the room it gets, so the slide is visible. Unfocused it is a truncated
        // label, which is the other half of what this row shows.
        Demo::Marquee => row(label, sel)
            .trailing_node(Node::leaf(
                Marquee::new("a label far too long for the space this row can spare")
                    .focused(sel)
                    .phase(m.phase)
                    .flex(1),
            ))
            .build(),
        Demo::Icons => row(label, sel)
            .trailing_node(
                Flow::new()
                    .gap(Gap::Tight)
                    .child(Icon::new(Glyph::Check))
                    .child(Icon::new(Glyph::Warning))
                    .child(Icon::new(Glyph::Lock))
                    .child(Icon::new(Glyph::Muted))
                    .child(Icon::new(Glyph::Pinned))
                    .build(),
            )
            .build(),
        Demo::Avatar => {
            row(label, sel).leading(Avatar::new("AG", 7).size(28)).build()
        }
        Demo::Badge => row(label, sel)
            .trailing_node(match Badge::count(12, sel) {
                Some(b) => Node::leaf(b),
                None => Node::leaf(Text::new("none").dim()),
            })
            .build(),
        // The one row whose widget has no label of its own, so the label sits above it.
        Demo::Divider => Node::Group(
            Column::new()
                .align(CrossAlign::Stretch)
                .selection_band(sel)
                .padding(Pad::xy(Gap::Base, Gap::None))
                .child(Text::new(label).font(FontRole::Small).dim())
                .child(Divider::new().space(Gap::Snug)),
        ),
        Demo::Switch => row(label, sel)
            .trailing(Switch::new(m.switch_on).focused(sel).out(out, Msg::ToggleSwitch))
            .build(),
        Demo::Checkbox => row(label, sel)
            .leading(Checkbox::checked(m.checkbox_on).focused(sel).out(out, Msg::ToggleCheckbox))
            .build(),
        Demo::RadioA => row(label, sel)
            .leading(Checkbox::radio(!m.radio_b).focused(sel).out(out, Msg::ChooseA))
            .build(),
        Demo::RadioB => row(label, sel)
            .leading(Checkbox::radio(m.radio_b).focused(sel).out(out, Msg::ChooseB))
            .build(),
        Demo::Stepper => row(label, sel)
            .trailing(Stepper::new(m.retries, 0, 9).focused(sel).out(out, Msg::SetRetries))
            .build(),
        Demo::Slider => row(label, sel)
            .trailing_node(Node::leaf(
                Slider::new(m.volume, 0, 100).step(5).focused(sel).out(out, Msg::SetVolume),
            ))
            .build(),
        Demo::Button => row(label, sel)
            .trailing(Button::new(press_label(m.presses), Msg::Press).focused(sel).out(out))
            .build(),
        // The three that need the slot table, which is why this function takes one. In a `ScrollList`
        // they would be unwritable — see the module docs.
        Demo::SearchField => Node::Group(
            Column::new()
                .align(CrossAlign::Stretch)
                .selection_band(sel)
                .padding(Pad::xy(Gap::Base, Gap::None))
                .child(Text::new(label).font(FontRole::Small).dim())
                // Turbofished because a `SearchField` with no `on_change` never names its message type,
                // and this row is only showing the field rather than filtering anything.
                .child(SearchField::<Msg>::new(slots).focused(sel).placeholder("type to filter")),
        ),
        Demo::FieldRow => FieldRow::new(label)
            .hint("a hint under the control")
            .control(TextField::new(slots).focused(sel))
            .build(),
        Demo::Date => row(label, sel)
            .trailing_node(Node::leaf(
                DateTime::date(slots, m.when).focused(sel).out(out, Msg::SetWhen),
            ))
            .build(),
        Demo::Time => row(label, sel)
            .trailing_node(Node::leaf(
                DateTime::time(slots, m.when).focused(sel).out(out, Msg::SetWhen),
            ))
            .build(),
        Demo::RowTwoLine => row("Ana Ribeiro", sel)
            
            .secondary("see you at eight")
            .leading(Avatar::new("AR", 3).size(28))
            .trailing(Text::new("14:32").font(FontRole::Small).dim())
            .build(),
        Demo::RowIconArrow => {
            row(label, sel).leading_icon(Glyph::Channel).trailing_arrow().build()
        }
        Demo::RowValue => {
            row(label, sel).trailing_value("Vivo Internet").build()
        }
        Demo::Flow => Node::Group(
            Column::new()
                .align(CrossAlign::Stretch)
                .selection_band(sel)
                .padding(Pad::xy(Gap::Base, Gap::Snug))
                .child(Text::new(label).font(FontRole::Small).dim())
                .node(
                    Flow::new()
                        .gap(Gap::Snug)
                        .line_gap(Gap::Snug)
                        .stretch_width()
                        // Long enough to actually wrap. The first version used six short words, all
                        // six fitted on one 320-pixel line, and the row proved nothing about the one
                        // thing it exists to show. A demo that cannot fail is a constant.
                        .child(Text::new("unread messages").ink(Ink::Accent))
                        .child(Text::new("muted until noon").dim())
                        .child(Text::new("pinned to the top").ink(Ink::Accent))
                        .child(Text::new("failed to send").ink(Ink::Error))
                        .child(Text::new("draft saved").dim())
                        .child(Text::new("archived last week").dim())
                        .build(),
                ),
        ),
        // The bar reads `m.volume`, which the slider on page 4 sets. Two components over one value is
        // the cheapest way to show that the message reached `update` — the half a host test cannot
        // prove about a physical keypad.
        Demo::Progress => row(label, sel)
            .trailing_node(Node::leaf(ProgressBar::percent(m.volume).selected(sel).flex(1)))
            .build(),
        // `m.phase` is the marquee's timer, reused: both want "something moved since the last frame",
        // and a second timer would be a second thing to get out of step.
        Demo::Spinner => row(label, sel)
            .trailing(Spinner::new(m.phase as u8).selected(sel))
            .build(),
        // All four tones on one line, and `.selected(sel)` on each — without it a calm chip on the
        // selection band is a pill-shaped hole, which is the defect this row exists to keep visible.
        Demo::Chips => Node::Group(
            Column::new()
                .align(CrossAlign::Stretch)
                .selection_band(sel)
                .padding(Pad::xy(Gap::Base, Gap::Snug))
                .child(Text::new(label).font(FontRole::Small).dim())
                .node(
                    Flow::new()
                        .gap(Gap::Tight)
                        .line_gap(Gap::Tight)
                        .stretch_width()
                        .child(Chip::calm("calm").selected(sel))
                        .child(Chip::fresh("fresh").selected(sel))
                        .child(Chip::warn("warn").selected(sel))
                        .child(Chip::busy("busy").selected(sel))
                        .build(),
                ),
        ),
        // Both tones stacked, because `warn` is the one colour on the palette that is not derivable
        // from the accent and the only way to see that is beside it.
        Demo::Notice => Node::Group(
            Column::new()
                .align(CrossAlign::Stretch)
                .gap(Gap::Hair)
                .selection_band(sel)
                .padding(Pad::xy(Gap::None, Gap::Hair))
                .child(Notice::new("Saved to Contacts"))
                .child(Notice::new("No network").detail("retrying in 30s").tone(NoticeTone::Warn)),
        ),
        Demo::Card => Node::Group(
            Column::new()
                .align(CrossAlign::Stretch)
                .selection_band(sel)
                .padding(Pad::xy(Gap::Base, Gap::Hair))
                .child(Text::new(label).font(FontRole::Small).dim())
                .child(
                    Card::new(slots)
                        .selected(sel)
                        .group(
                            Column::new()
                                .align(CrossAlign::Stretch)
                                .gap(Gap::Hair)
                                // A card changes the ground under its children and no `Ink` role
                                // follows it there, so the ink has to be picked to match the ground
                                // the card just chose. `HIGH_CONTRAST` is where it shows: `text` and
                                // `chrome` are both white, so `Ink::Text` on a chrome card is white
                                // on white — true before this row existed, and invisible until the
                                // ground became visible. Recorded in `docs/ui-catalog.md`.
                                .child(Text::new("Vivo Internet").font(FontRole::Strong))
                                .child(Text::new("Connected \u{b7} 3G").dim()),
                        )
                        .stretch_width(),
                ),
        ),
        // `fill(1)` on purpose: this is the row that takes the leftover, and a gallery page is where
        // that is safe to show. On a real screen it is the whole content band.
        Demo::Empty => Node::leaf(
            EmptyState::new("Nothing here yet").fill(1),
        ),
        Demo::Tabs => Node::Group(
            Column::new()
                .align(CrossAlign::Stretch)
                .selection_band(sel)
                .padding(Pad::xy(Gap::None, Gap::Hair))
                .child(Text::new(label).font(FontRole::Small).dim())
                // It takes Left/Right from everything below it, which is the thing to feel on the
                // handset rather than to read about.
                .child(Tabs::new(m.tab).tab("All").tab("Unread").tab("Groups").out(out, Msg::SetTab)),
        ),
        // Head and body in one stop rather than one stop each: the head is what takes `Select`, and a
        // cursor that had to walk through a closed section's rows to leave it would be worse than a
        // section that opens and closes under one press.
        Demo::Collapsible => {
            // With children, or the chevron says "open" over nothing at all — which is what the
            // first version of this row drew, and it looked like a broken widget rather than like a
            // demo missing its content.
            let c = Collapsible::new_open(slots, label)
                .child(ListItem::new("Wi-Fi").trailing_value("Vivo-2G").build())
                .child(ListItem::new("Bluetooth").trailing_value("Off").build());
            let head = c.head(sel);
            let mut col = Column::new().align(CrossAlign::Stretch).selection_band(sel).child(head);
            for n in c.body() {
                col = col.node(n);
            }
            Node::Group(col)
        }
        // The two model-driven overlays: the row asks, `update` sets the flag, and the layer at the
        // bottom of `build_page` reads it. A `Button` and not a bare row, so `Select` has an owner.
        Demo::OpenDialog => row(label, sel)
            .trailing(Button::new("Open", Msg::OpenOverlay(1)).focused(sel).out(out))
            .build(),
        Demo::OpenMenu => row(label, sel)
            .trailing(Button::new("Open", Msg::OpenOverlay(2)).focused(sel).out(out))
            .build(),
        // These two are not rows at all — they are layers that claim a key for themselves, so the
        // row is a label saying which. `Drawer` defaults to the left softkey and this screen has
        // spent it on the page turn, which is exactly the collision `keys.rs` is about.
        Demo::SheetHint => row(label, sel).trailing_value("press *").build(),
        Demo::DrawerHint => row(label, sel).trailing_value("press 0").build(),
        Demo::Error => FieldRow::new("Phone number")
            .error("not a number this country has")
            .control(TextField::new(slots).focused(sel))
            .build(),
        Demo::Instruments => Node::Group(
            Row::new()
                .align(CrossAlign::Stretch)
                .selection_band(sel)
                .padding(Pad::xy(Gap::Base, Gap::None))
                .gap(Gap::Base)
                .child(Text::new(label).font(FontRole::Small).dim().flex(1))
                .child(
                    Text::new(instruments(m))
                        .font(FontRole::Small)
                        .ink(if m.faults.get() == 0 { Ink::Accent } else { Ink::Error }),
                ),
        ),
    }
}

/// The button's label, carrying its own press count so a press is visible without a second row.
fn press_label(n: u32) -> String {
    if n == 0 {
        String::from("Press")
    } else {
        format!("{n}")
    }
}

fn instruments(m: &Model) -> String {
    format!("m={} f={}", m.measures.get(), m.faults.get())
}

// ---------------------------------------------------------------- the palette, and why it is static

/// Which palette is showing, as the entry point's `palette =` expression reads it.
///
/// `entry!` evaluates that expression on every step and *outside* the app — before `draw`, with no
/// `&Model` in scope — so this is the one piece of state here that genuinely cannot live in the model.
/// Writable static data is unrestricted in an EXE; `symbian-app` says so where it uses eight of them,
/// and elf2e32 refuses it only in a DLL.
pub fn palette() -> Palette {
    let phone = PHONE.load();
    Palette::at(PALETTE.load() % Palette::count(phone), phone).1
}

/// The E72's own theme, from the seeds `skinprobe` measured — for a host that has no skin server.
///
/// The measurement is in `docs/reference/skinprobe.txt`, and the numbers are repeated here rather
/// than read because the host cannot read them. That is not a duplicate of the device path: this is a
/// *record* of one phone's theme, and the device path is whatever theme is on the phone right now.
/// The contact sheets use this so the sixth palette can be reviewed without carrying a handset.
pub fn phone_theme_from_measured_seeds() -> Option<Palette> {
    Palette::from_device_seeds(
        Color::hex(0x030510),
        Color::hex(0x4b5879),
        Color::hex(0x0099cc),
        Color::hex(0x751001),
    )
}

/// The phone's own theme, read once.
///
/// Four colours, at the indices `skinprobe` measured on the E72 — `docs/reference/skinprobe.txt`. They
/// are cited as measurements rather than as header names on purpose: `AknsConstants.h` comments every
/// index, and two of these four sit *past* the last index it documents, so a role picked from the
/// header would have found greys.
///
/// `None` when there is no skin (the host, a daemon) or when the derived palette fails
/// [`Palette::check`] — an unreadable theme must not become an unreadable application.
fn read_phone_theme() -> Option<Palette> {
    use symbian::skin::{self, Table};

    // Logged unconditionally, and that is a correction rather than a flourish. The first version
    // logged only on failure, so a silent run meant either "the theme was fine" or "the app never
    // started" and there was no way to tell them apart from the host. An instrument that only speaks
    // when things go wrong cannot confirm that they went right.
    let raw = |t, i| match skin::color(t, i) {
        Ok(c) => {
            symbian::log!("theme: {:?}[{}] = {:#08x}", t, i, c);
            Some(Color::hex(c))
        }
        Err(e) => {
            symbian::log!("theme: {:?}[{}] refused {}", t, i, e.code());
            None
        }
    };

    let seeds = (
        raw(Table::Component, 18),
        raw(Table::Text, 62),
        raw(Table::Other, 8),
        raw(Table::Component, 24),
    );
    let (Some(page), Some(chrome), Some(accent), Some(warn)) = seeds else {
        symbian::log!("theme: no skin here — the built-ins are the whole offer");
        return None;
    };

    match Palette::from_device_seeds(page, chrome, accent, warn) {
        Some(p) => {
            symbian::log!(
                "theme: accepted, accent {:02x}{:02x}{:02x}",
                p.accent.r(),
                p.accent.g(),
                p.accent.b()
            );
            Some(p)
        }
        None => {
            symbian::log!("theme: derived but NOT legible; falling back to the built-ins");
            None
        }
    }
}

static PALETTE: PaletteCell = PaletteCell::new();

/// The phone's palette, where the entry point's `palette =` expression can see it.
///
/// Same reason as [`PALETTE`]: `entry!` evaluates that expression outside the app with no `&Model` in
/// scope, so anything it needs has to be reachable from a static. `Palette` is `Copy` and about 140
/// bytes, which is unremarkable writable data in an EXE.
static PHONE: PhoneCell = PhoneCell::new();

struct PhoneCell(core::cell::UnsafeCell<Option<Palette>>);

// SAFETY: single-threaded by construction, exactly as `PaletteCell` below — every access is from the
// GUI thread.
unsafe impl Sync for PhoneCell {}

impl PhoneCell {
    const fn new() -> Self {
        Self(core::cell::UnsafeCell::new(None))
    }
    fn load(&self) -> Option<Palette> {
        // SAFETY: see the `Sync` impl.
        unsafe { *self.0.get() }
    }
    fn store(&self, v: Option<Palette>) {
        // SAFETY: see the `Sync` impl.
        unsafe { *self.0.get() = v }
    }
}

/// A `usize` the step function reads and `update` writes.
///
/// Its own type so the `unsafe` is in one place with a reason on it. `AtomicUsize` would be tidier and
/// is not available: this target has no atomics.
struct PaletteCell(core::cell::UnsafeCell<usize>);

// SAFETY: single-threaded by construction. Every access is from the GUI thread — `rust_step` and the
// `update` it calls — because that is the only thread that touches it. The worker thread `entry!` can
// start never sees this.
unsafe impl Sync for PaletteCell {}

impl PaletteCell {
    const fn new() -> Self {
        Self(core::cell::UnsafeCell::new(0))
    }
    fn load(&self) -> usize {
        // SAFETY: see the `Sync` impl.
        unsafe { *self.0.get() }
    }
    fn store(&self, v: usize) {
        // SAFETY: see the `Sync` impl.
        unsafe { *self.0.get() = v }
    }
}

// ---------------------------------------------------------------- the harness

/// What the host drives: the bridge, one timer, and the instruments.
///
/// # Why a wrapper rather than the bridge itself
///
/// Two things the bridge cannot do, both of which belong to whoever owns the platform.
///
/// **A `Cmd::SetTimer` is a request, not an action.** `take_effects` hands the commands back and
/// something has to arm a real timer and route its completion. That is deliberate — it is what keeps
/// `update` unable to reach the platform — and it means every app needs this much glue.
///
/// **The instruments have to be read from outside.** `measure_calls` is the bridge's, and a row that
/// computed it inside `view` would be measuring a tree it was part of.
pub struct Uigallery {
    bridge: DeclarativeAppBridge<Gallery>,
    /// The handle of the tick we are waiting on, so its completion can be told from any other timer in
    /// the process.
    ticker: Option<i32>,
}

impl Uigallery {
    pub fn new() -> Self {
        let me = Self { bridge: DeclarativeAppBridge::new(), ticker: symbian::timer_after(TICK_MS).ok() };
        // The entry point reads the palette from statics *before* the first draw, so the model's
        // answers have to be there already — otherwise the first frame renders against whatever the
        // statics happened to hold.
        //
        // Both, and that matters more than it looks: a static outlives the app that wrote it, so a
        // host harness making one app per sheet inherits the previous app's palette index. That is
        // exactly what happened — the contact sheet named "phone theme" rendered in high contrast,
        // because the loop's previous iteration had left a 4 in this cell. On the device there is one
        // app and the bug is unreachable; on the host it is the first thing that goes wrong.
        PALETTE.store(me.bridge.model().palette);
        PHONE.store(me.bridge.model().phone.get());
        me
    }

    /// Act on whatever `update` asked the world for.
    ///
    /// Draining is not optional even when nothing is asked for: a queue nobody empties is a command
    /// that arrives on whichever later event happens to drain it.
    fn drain_effects(&mut self) {
        for cmd in self.bridge.take_effects() {
            if let Cmd::SetTimer { ms, .. } = cmd {
                self.ticker = symbian::timer_after(ms as i32).ok();
            }
        }
    }

    /// Turn to page `n`, for a harness rendering one page per file.
    ///
    /// Goes through `send` rather than reaching into the model, so it takes the same path a keypress
    /// does — including the `Cmd` the message returns. A setter that wrote the field directly would be
    /// a second way to change the model, which is the one thing this architecture is for.
    pub fn goto_page(&mut self, n: usize) {
        for _ in 0..n % page_count() {
            self.bridge.send(Msg::NextPage);
        }
        self.drain_effects();
    }

    /// Hand the app a phone theme it could not read for itself, for a host harness.
    pub fn set_phone_theme(&mut self, phone: Option<Palette>) {
        self.bridge.model().phone.set(phone);
        PHONE.store(phone);
    }

    /// Switch to palette `n`, for a harness rendering one file per palette.
    ///
    /// Through `send`, like [`goto_page`](Self::goto_page), so the model's index and the static the
    /// entry point reads are both written — by the one code path that writes them. A harness that set
    /// the theme itself and left the model alone produced a sheet of the light palette captioned
    /// "Dark", which is the same two-sources-of-truth mistake this file goes out of its way to avoid,
    /// arriving in the tooling instead of in the app.
    pub fn goto_palette(&mut self, n: usize) {
        // `Palette::count`, not `Palette::ALL.len()` — with five built-ins and a phone theme, `n = 5`
        // under the array's length is `5 % 5 = 0` and the harness silently renders the first palette
        // under the sixth one's name.
        let offer = Palette::count(self.bridge.model().phone.get());
        for _ in 0..n % offer {
            self.bridge.send(Msg::CyclePalette);
        }
        self.drain_effects();
    }

    /// Copy this frame's counters into the model, for the next frame to draw.
    fn read_instruments(&self) {
        let m = self.bridge.model();
        m.measures.set(self.bridge.measure_calls());
        let slots = self.bridge.slots();
        m.faults.set(slots.type_mismatches() + slots.unbalanced_groups());
    }
}

impl Default for Uigallery {
    fn default() -> Self {
        Self::new()
    }
}

impl symbian_ui::App for Uigallery {
    fn title(&self) -> &str {
        Gallery::TITLE
    }

    fn handle_key(&mut self, ev: KeyEvent, theme: &Theme<'_>, screen: Rect) -> Handled {
        let handled = symbian_ui::App::handle_key(&mut self.bridge, ev, theme, screen);
        self.drain_effects();
        handled
    }

    /// The tick, and nothing else.
    ///
    /// Matched on the handle rather than on the event kind alone: any other timer in the process
    /// completes here too, and a marquee advancing on somebody else's timer would be a phase counter
    /// driven by whatever the platform happened to be doing.
    fn handle_raw(&mut self, ev: &symbian_ui::RawEvent) -> Handled {
        if ev.kind == symbian_sys::SHIM_EV_TIMER && Some(ev.handle) == self.ticker {
            self.bridge.send(Msg::Tick);
            // Re-armed here rather than through a `Cmd`, because the tick is the harness's own
            // heartbeat: an app that stopped asking would stop being woken, and this one never stops.
            self.ticker = symbian::timer_after(TICK_MS).ok();
            self.drain_effects();
            return Handled::Consumed;
        }
        symbian_ui::App::handle_raw(&mut self.bridge, ev)
    }

    fn draw(&mut self, c: &mut Canvas<'_>, theme: &Theme<'_>) {
        symbian_ui::App::draw(&mut self.bridge, c, theme);
        self.read_instruments();
    }

    fn should_exit(&self) -> bool {
        symbian_ui::App::should_exit(&self.bridge)
    }

    fn install_clipboard(&mut self, clip: alloc::boxed::Box<dyn symbian_ui::Clipboard>) {
        symbian_ui::App::install_clipboard(&mut self.bridge, clip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pages_cover_every_entry_exactly_once() {
        // Two tables that have to agree. A component added to `ENTRIES` and forgotten in `PAGES` is a
        // component nobody can reach, and nothing about the screen would say so.
        let mut seen = alloc::vec![0u32; ENTRIES.len()];
        for p in PAGES {
            for i in p.clone() {
                seen[i] += 1;
            }
        }
        assert!(seen.iter().all(|&n| n == 1), "coverage is {seen:?}");
    }

    #[test]
    fn every_page_has_at_least_one_stop() {
        // A page of nothing but headings is a page the cursor cannot enter, and the D-pad would look
        // broken on it.
        for (n, p) in PAGES.iter().enumerate() {
            let stops = p.clone().filter(|&i| !matches!(ENTRIES[i].0, Kind::Heading)).count();
            assert!(stops > 0, "page {n} has no stops");
        }
    }

    #[test]
    fn no_page_holds_more_rows_than_the_screen_can_show() {
        // 240 pixels, less an 18-pixel title bar and a 17-pixel softkey bar, is 205 — five rows of 38
        // and no more. A sixth would be laid out and unreachable.
        for (n, p) in PAGES.iter().enumerate() {
            assert!(p.len() <= 5, "page {n} holds {} entries", p.len());
        }
    }

    #[test]
    fn a_page_title_names_its_section() {
        assert_eq!(page_title(0), "Text");
        assert_eq!(page_title(2), "Controls");
    }

    #[test]
    fn the_only_key_the_model_claims_is_the_palette_one() {
        // Everything else has to fall through to the tree, or the scope's cursor and the focused
        // control never see a key — and the softkey bar declared on the `Screen` widget never gets its
        // own, which is the path this app exists to exercise.
        let m = Gallery::init();
        assert_eq!(Gallery::on_key(&m, KeyEvent::new(Key::Char('#'))), Some(Msg::CyclePalette));
        for key in [
            Key::Up,
            Key::Down,
            Key::Left,
            Key::Right,
            Key::Select,
            Key::Backspace,
            Key::Softkey(symbian_ui::Softkey::Left),
            Key::Softkey(symbian_ui::Softkey::Right),
        ] {
            assert!(Gallery::on_key(&m, KeyEvent::new(key)).is_none(), "{key:?} was claimed");
        }
    }

    #[test]
    fn turning_the_pages_comes_back_round() {
        let mut m = Gallery::init();
        for _ in 0..PAGES.len() {
            Gallery::update(&mut m, Msg::NextPage);
        }
        assert_eq!(m.page, 0);
    }

    #[test]
    fn cycling_the_palette_comes_back_round_and_the_entry_point_sees_it() {
        let mut m = Gallery::init();
        for _ in 0..Palette::count(m.phone.get()) {
            Gallery::update(&mut m, Msg::CyclePalette);
        }
        assert_eq!(m.palette, 0);
        assert_eq!(PALETTE.load(), 0);
    }

    #[test]
    fn the_cycler_would_reach_the_phones_own_palette() {
        // The trap, asserted at the call site as well as in `symbian-ui`: stepping with
        // `Palette::ALL.len()` instead of `Palette::count` makes the sixth palette unreachable, and
        // nothing about the screen would say so.
        let mut m = Gallery::init();
        // The host has no skin, so a phone palette is injected to stand in for one.
        m.phone.set(Some(Palette::LIGHT));
        let mut names = alloc::vec::Vec::new();
        for _ in 0..Palette::count(m.phone.get()) {
            names.push(Palette::at(m.palette, m.phone.get()).0);
            Gallery::update(&mut m, Msg::CyclePalette);
        }
        assert!(names.contains(&"Phone theme"), "cycled {names:?} and never reached it");
        assert_eq!(m.palette, 0, "and still came back round");
    }

    #[test]
    fn with_no_phone_theme_the_gallery_offers_the_built_ins_and_nothing_else() {
        // The host, and a device whose theme failed `check`. The `#` key must still work rather than
        // stepping onto a palette that is not there.
        let m = Gallery::init();
        assert!(m.phone.get().is_none(), "the host has no skin");
        assert_eq!(Palette::count(m.phone.get()), Palette::ALL.len());
    }

    #[test]
    fn a_radio_choice_never_leaves_the_model_without_one() {
        // "Set this", not "toggle this". Pressing the chosen option again is a no-op rather than a
        // hole the user cannot climb out of.
        let mut m = Gallery::init();
        Gallery::update(&mut m, Msg::ChooseA);
        assert!(!m.radio_b);
        Gallery::update(&mut m, Msg::ChooseA);
        assert!(!m.radio_b);
        Gallery::update(&mut m, Msg::ChooseB);
        assert!(m.radio_b);
    }

    #[test]
    fn stepping_the_month_off_the_thirty_first_clamps_the_day() {
        // Why `init` starts on 31 January: it is the case `Stamp::with_part` exists for, and the one
        // worth pressing on a handset. Asserted here so a regression fails on the host first.
        use symbian_ui::calendar::Part;
        let m = Gallery::init();
        assert_eq!(m.when.with_part(Part::Month, 2).part(Part::Day), 29, "2024 is a leap year");
        assert_eq!(m.when.with_part(Part::Month, 3).part(Part::Day), 31);
    }

    #[test]
    fn every_page_builds_and_draws_without_a_slot_fault() {
        // The whole screen, every page, on the host — so a panic or a shifted slot ordinal fails here
        // rather than as a dialog with a number in it on the handset. `f=0` is what the instruments row
        // shows, and this is the same assertion made eight times before anyone has to read it.
        use symbian_ui::{testing, Palette};
        let mut m = Gallery::init();
        let mut slots = SlotTable::new();
        for page in 0..PAGES.len() {
            m.page = page;
            for _ in 0..3 {
                slots.begin_frame();
                let root = Gallery::view(&m, &mut slots);
                testing::with_canvas(symbian_gfx::Size::new(320, 240), |c| {
                    testing::with_theme(Palette::DARK, |theme| {
                        let mut cache =
                            symbian_decl_ui::UiCache::with_capacity(root.slot_count() + 8);
                        symbian_decl_ui::layout::draw_frame(
                            &root,
                            symbian_gfx::Rect::from_xywh(0, 0, 320, 240),
                            &mut cache,
                            c,
                            theme,
                        );
                    });
                });
            }
            assert_eq!(slots.type_mismatches(), 0, "page {page} shifted a slot ordinal");
            assert_eq!(slots.unbalanced_groups(), 0, "page {page} left a group open");
        }
    }

    #[test]
    fn every_row_that_offers_typing_actually_takes_a_character() {
        // A person on the handset found this, twice, and the host had nothing to say: `FieldRow`
        // hands `focused` to the *row* and its control has to be told separately —
        // `.control(TextField::new(slots).focused(here)).focused(here)` — which `field_row.rs`
        // documents and calls a duplication in its own module docs. Both typing rows here forgot the
        // second half, so the caption lit up in the accent, the caret drew, and no key ever arrived.
        //
        // The lesson is not "remember the second call". It is that a focused-looking field that
        // takes nothing is indistinguishable from a dead keypad, so it needs a test that presses a
        // key rather than one that inspects a tree.
        use symbian_ui::{testing, Handled, Key, KeyEvent, Palette};

        let typed = |page: usize, downs: usize| {
            let mut m = Gallery::init();
            m.page = page;
            let mut slots = SlotTable::new();
            let rect = symbian_gfx::Rect::from_xywh(0, 0, 320, 240);
            let mut out = Handled::Ignored;
            // Twice: the first frame is what places the rects a key is matched against.
            for _ in 0..2 {
                slots.begin_frame();
                let root = Gallery::view(&m, &mut slots);
                testing::with_canvas(symbian_gfx::Size::new(320, 240), |c| {
                    testing::with_theme(Palette::DARK, |theme| {
                        let mut cache =
                            symbian_decl_ui::UiCache::with_capacity(root.slot_count() + 8);
                        symbian_decl_ui::layout::draw_frame(&root, rect, &mut cache, c, theme);
                        let mut clip = symbian_ui::NoClipboard;
                        let mut cx = symbian_decl_ui::widget::KeyCtx::new(theme, &mut clip);
                        for _ in 0..downs {
                            symbian_decl_ui::layout::dispatch_key(
                                &root,
                                KeyEvent::new(Key::Down),
                                &cache,
                                &mut cx,
                            );
                        }
                        out = symbian_decl_ui::layout::dispatch_key(
                            &root,
                            KeyEvent::new(Key::Char('a')),
                            &cache,
                            &mut cx,
                        );
                    });
                });
            }
            out
        };

        // Every stop built from a typing widget, found from the catalogue so a page inserted above
        // one cannot quietly stop testing it.
        for (page, range) in PAGES.iter().enumerate() {
            let mut stop = 0;
            for i in range.clone() {
                let Kind::Demo(d) = ENTRIES[i].0 else { continue };
                if matches!(d, Demo::SearchField | Demo::FieldRow | Demo::Error) {
                    assert_eq!(
                        typed(page, stop),
                        Handled::Consumed,
                        "page {} stop {stop} ({}) looks like a field and takes nothing",
                        page + 1,
                        ENTRIES[i].1
                    );
                }
                stop += 1;
            }
        }
    }
}
