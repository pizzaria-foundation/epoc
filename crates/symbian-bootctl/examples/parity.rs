//! The boot manager's screens, drawn twice, compared pixel for pixel.
//!
//! ```text
//! cargo run  -p symbian-bootctl --example parity     # → parity-out/ and a report
//! cargo test -p symbian-bootctl --example parity     # the assertion
//! ```
//!
//! # What the two sides are here, and why they are not the usual two
//!
//! Everywhere else in this SDK a parity harness compares two implementations that both ship — `tg`'s
//! `chats.rs` against `chats_decl.rs`, the launcher's `draw_with_icons` against `settings_decl::view`.
//! This crate is being migrated **in place**, so there is no second live implementation to point at.
//! The left-hand side is [`symbian_bootctl::reference`]: a frozen copy of the drawing code as it
//! stood before the migration, kept as source rather than as a golden PNG because `symbian_preview`
//! writes PNGs and cannot read them. The right-hand side is what the crate actually draws today.
//!
//! This harness reported `identical` on every scene **before** a pixel was moved. That is the only
//! order in which it means anything: a comparison written after a rewrite is a comparison of the
//! rewrite with itself.
//!
//! # A scene per branch, not per screen
//!
//! Six screens, and the states inside them are where the branches are. Each scene below is aimed at
//! one:
//!
//! - the Order tab plain, and with the cursor off the first row — the selection band and `Ground::Band`
//! - move mode — the chip slot swaps for an arrow, and the arrow differs at the ends of the list
//! - the entry detail — a switch, a dropdown, a stepper and a second stepper, one of them focused
//! - the entry detail with the cursor on the *stepper*, because a focused stepper draws chevrons a
//!   calm one does not
//! - the policy dropdown open — a popup sized to its options, over rows that stay visible
//! - the Last boot report, twice: a normal boot and safe mode, which is a different first line
//! - the app picker, which owns the whole frame and leaves no tab strip behind it
//! - the removal question, which paints a scrim and its own softkey bar over everything
//! - the Settings screen, plain, with the cursor moved, and with its dropdown open
//! - and the light palette, because every colour decision above was made against one ground
//!
//! # Read a difference before removing it
//!
//! The frozen side is what shipped. That makes it the standard, not the correct answer. If the
//! migrated side differs, the question is which one is right — and if the answer is "the new one",
//! the fix is to say so here in words, not to edit `reference.rs` until the numbers agree. A commit
//! that touches both sides at once is a comparison that was bent to fit.

use std::process::ExitCode;

use symbian::apps::AppInfo;
use symbian_bootcfg::pkg::PkgDb;
use symbian_bootcfg::status::{EntryStatus, Mode, State};
use symbian_bootcfg::{BootConfig, BootStatus, Entry, Policy};
use symbian_bootctl::settings::SettingsScreen;
use symbian_bootctl::{reference, BootScreen};
use symbian_gfx::{Canvas, Rect, E72_SCREEN};
use symbian_preview::{Atlases, Parity};
use symbian_ui::{App as _, Key, KeyEvent, Softkey, Theme};

// `cargo run --example` runs with the *package* directory as the working directory, so this lands in
// `crates/symbian-bootctl/parity-out/` and not in the workspace-root `parity-out/` that other crates
// write into. Checked, because scene names like `settings` are not unique across screens and a
// shared directory would mean one crate silently overwriting another's evidence.
const OUT: &str = "parity-out";

fn main() -> ExitCode {
    let atlases = Atlases::load();
    // `keep_matching`, so a passing run still leaves the pictures behind. A comparison whose output
    // only exists when it fails is one nobody looks at when it passes — and looking is how a
    // *shared* omission gets caught, since two sides that both fail to draw something agree
    // perfectly.
    let mut p = Parity::new(OUT).keep_matching(true);
    atlases.with_themes(|dark, light| run(&mut p, dark, light));
    println!("{}", p.report());
    if p.diffs().is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

// ------------------------------------------------------------------ the fixtures

fn app(uid3: u32, caption: &str) -> AppInfo {
    AppInfo { uid3, caption: caption.into(), hidden: false, system: false }
}

fn roster() -> Vec<AppInfo> {
    vec![
        app(0x1000_58C5, "Messaging"),
        app(0x2000_1234, "Telegram"),
        app(0x1020_7236, "Web"),
        app(0x1000_5901, "Contacts"),
        app(0x1020_7239, "Calculator"),
        app(0x1000_5905, "Clock"),
        app(0x1020_723A, "File manager"),
        app(0x1000_5907, "Settings"),
    ]
}

/// One entry of each policy, and one the supervisor disarmed itself — the state a reader most needs
/// to recognise, because it is the one nobody configured.
fn config() -> BootConfig {
    BootConfig {
        enabled: true,
        first_delay_ms: 10_000,
        max_restarts: 10,
        entries: vec![
            Entry {
                policy: Policy::Always,
                delay_ms: 2_000,
                ..Entry::new(0x2000_1234, "Telegram".into())
            },
            Entry {
                policy: Policy::Times(3),
                delay_ms: 3_000,
                ..Entry::new(0x1000_58C5, "Messaging".into())
            },
            Entry {
                policy: Policy::Never,
                delay_ms: 1_500,
                ..Entry::new(0x1000_5905, "Clock".into())
            },
            Entry {
                enabled: false,
                auto_disarmed: true,
                policy: Policy::Always,
                delay_ms: 2_000,
                ..Entry::new(0x1020_7236, "Web".into())
            },
        ],
    }
}

fn status() -> BootStatus {
    BootStatus {
        mode: Mode::Normal,
        boot_count: 0,
        restarts_used: 2,
        entries: vec![
            EntryStatus { uid3: 0x2000_1234, last_rc: 0, launch_at_s: 10, restarts: 2, state: State::Alive },
            EntryStatus { uid3: 0x1000_58C5, last_rc: 0, launch_at_s: 13, restarts: 0, state: State::Alive },
            EntryStatus { uid3: 0x1000_5905, last_rc: 0, launch_at_s: 14, restarts: 0, state: State::Dead },
            EntryStatus { uid3: 0x1020_7236, last_rc: -1, launch_at_s: 16, restarts: 3, state: State::Disarmed },
        ],
    }
}

/// The other first line of the report, and the one that matters: safe mode is what the screen exists
/// to explain, and it takes a different branch through every line of `draw_boot`.
fn safe_status() -> BootStatus {
    BootStatus {
        mode: Mode::Safe,
        boot_count: 3,
        restarts_used: 7,
        entries: vec![EntryStatus {
            uid3: 0x1020_7236,
            last_rc: -14,
            launch_at_s: 4,
            restarts: 0,
            state: State::LaunchFailed,
        }],
    }
}

// ------------------------------------------------------------------ the scenes

/// Which screen a scene is about. The two are separate types with separate entry points, so the
/// scene has to say which one it means rather than the harness guessing from the name.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Which {
    Boot,
    /// The boot screen, but reading the safe-mode report rather than the normal one.
    BootSafe,
    /// A boot list whose first entry has a name far too long for its line. The one case where
    /// "draw the text" and "draw the text as a `Text` widget" are allowed to disagree: `draw_text_in`
    /// clips at the edge, a `Text` truncates with an ellipsis, and no scene made of realistic
    /// captions can tell the two apart. Without this scene a swap that changes what happens to a
    /// long name would pass, and it would pass for the reason that is hardest to notice — nothing
    /// on screen was long enough to ask.
    BootLong,
    Settings,
}

struct Scene {
    name: &'static str,
    which: Which,
    /// How the scene is reached. Every state below is behind key presses, because that is the only
    /// way in: the screens own their cursor, their tab, their open dropdown and their modal, and not
    /// one of those is settable from outside. It is also how a person reaches them.
    keys: &'static [Key],
}

const LEFT: Key = Key::Softkey(Softkey::Left);

fn scenes() -> Vec<Scene> {
    use Which::*;
    vec![
        Scene { name: "order-plain", which: Boot, keys: &[] },
        Scene { name: "order-selected", which: Boot, keys: &[Key::Down] },
        // Move mode from the *second* row, so the arrow is `↕` rather than the `↓` the first row
        // gets. Both ends of the list draw a different hint and only one of them can be the default.
        Scene { name: "order-move", which: Boot, keys: &[Key::Down, LEFT] },
        Scene { name: "order-move-first", which: Boot, keys: &[LEFT] },
        // The entry detail, reached the way a person reaches it: the centre key on a row.
        Scene { name: "entry", which: Boot, keys: &[Key::Select] },
        // The cursor on the delay stepper. A focused stepper draws chevrons a calm one does not, and
        // this row is the one whose policy hides the row above it.
        Scene { name: "entry-stepper", which: Boot, keys: &[Key::Select, Key::Down, Key::Down] },
        // A `Times(3)` entry, whose detail has a fourth row the others do not.
        Scene { name: "entry-retries", which: Boot, keys: &[Key::Down, Key::Select, Key::Down, Key::Down] },
        Scene { name: "entry-policy-open", which: Boot, keys: &[Key::Select, Key::Down, Key::Select] },
        Scene { name: "last-boot", which: Boot, keys: &[Key::Right] },
        Scene { name: "last-boot-safe", which: BootSafe, keys: &[Key::Right] },
        Scene { name: "last-boot-long-name", which: BootLong, keys: &[Key::Right] },
        Scene { name: "order-long-name", which: BootLong, keys: &[] },
        Scene { name: "picker", which: Boot, keys: &[Key::Down, Key::Down, Key::Down, Key::Down, Key::Select] },
        Scene { name: "confirm-remove", which: Boot, keys: &[Key::Backspace] },
        Scene { name: "settings", which: Settings, keys: &[] },
        Scene { name: "settings-cursor", which: Settings, keys: &[Key::Down, Key::Down] },
        Scene { name: "settings-refresh-open", which: Settings, keys: &[Key::Down, Key::Down, Key::Down, Key::Select] },
        // The stepper walked to a value it did not start at, so the row's number is not the default.
        Scene { name: "settings-stepper", which: Settings, keys: &[Key::Down, Key::Right, Key::Right] },
    ]
}

const LONG_NAME: &str = "Telegram messenger for Symbian S60 5th edition, unofficial build";

fn boot_for(scene: &Scene, theme: &Theme<'_>) -> BootScreen {
    let st = if scene.which == Which::BootSafe { safe_status() } else { status() };
    let mut cfg = config();
    if scene.which == Which::BootLong {
        cfg.entries[0].name = LONG_NAME.into();
    }
    let mut s = BootScreen::new(cfg, Some(st), roster());
    let rect = Rect::from_size(E72_SCREEN);
    for k in scene.keys {
        s.handle_key(KeyEvent::new(*k), theme, rect);
    }
    s
}

fn settings_for(scene: &Scene) -> SettingsScreen {
    let mut s = SettingsScreen::new(config(), PkgDb::default());
    for k in scene.keys {
        s.handle_key(KeyEvent::new(*k));
    }
    s
}

/// The frozen screen.
fn render_reference(c: &mut Canvas<'_>, scene: &Scene, theme: &Theme<'_>) {
    match scene.which {
        Which::Settings => reference::draw_settings(&mut settings_for(scene), c, theme),
        _ => reference::draw_boot_screen(&mut boot_for(scene, theme), c, theme),
    }
}

/// What the crate draws today.
fn render_live(c: &mut Canvas<'_>, scene: &Scene, theme: &Theme<'_>) {
    match scene.which {
        Which::Settings => settings_for(scene).draw(c, theme),
        _ => boot_for(scene, theme).draw(c, theme),
    }
}

fn run(p: &mut Parity, dark: &Theme<'_>, light: &Theme<'_>) {
    for scene in scenes() {
        p.check(
            scene.name,
            dark,
            |c| render_reference(c, &scene, dark),
            |c| render_live(c, &scene, dark),
        );
    }
    // The light palette, on the three screens the migration touches. Every colour decision in a
    // migrated row — a chip's fill, a switch's track, a caption on the selection band — was made
    // against one ground, and `Ground` exists because that choice is void on another.
    for name in ["entry", "last-boot", "settings"] {
        let scene = scenes().into_iter().find(|s| s.name == name).expect("scene");
        p.check(
            // A leaked name is a scene silently overwriting another's PNGs, so the light runs are
            // named apart rather than reusing the dark scene's name.
            Box::leak(format!("{name}-light").into_boxed_str()),
            light,
            |c| render_reference(c, &scene, light),
            |c| render_live(c, &scene, light),
        );
    }
}

#[test]
fn the_migrated_screens_draw_what_the_frozen_ones_drew() {
    let atlases = Atlases::load();
    let mut p = Parity::new(OUT).keep_matching(true);
    atlases.with_themes(|dark, light| run(&mut p, dark, light));
    // The count is asserted as well as the diffs. A refactor that stops building scenes otherwise
    // turns this into a green light for nothing, which is exactly what `checked()` is for.
    assert_eq!(p.checked(), 21, "a scene stopped being compared");
    p.finish();
}

/// The scenes are distinct, which is what makes nineteen of them worth more than one.
///
/// This is the instrument checking that it is pointed at something. A key sequence that does not
/// reach the state it names produces two identical pictures under two names, both passing, both
/// proving one thing — and that is not hypothetical here: `examples/preview.rs` shipped three scenes
/// under three names that were one picture, for months, because a `Right` meant to open a detail hit
/// a tab strip that clamps.
#[test]
fn every_scene_renders_something_different() {
    let atlases = Atlases::load();
    atlases.with_themes(|dark, _light| {
        let mut seen: Vec<(&str, Vec<u16>)> = Vec::new();
        for scene in scenes() {
            let mut sheet = symbian_preview::Sheet::new(E72_SCREEN);
            render_live(&mut sheet.canvas(), &scene, dark);
            let px = sheet.pixels().to_vec();
            for (name, other) in &seen {
                assert_ne!(&px, other, "{} drew the same pixels as {name}", scene.name);
            }
            seen.push((scene.name, px));
        }
    });
}

/// The light palette is a different palette, not the same one under another name.
///
/// Cheap, and it stands in front of a real way to write a useless scene: passing `dark` to a check
/// named `-light` compares two screens that agree for the wrong reason.
#[test]
fn the_two_palettes_do_not_draw_the_same_screen() {
    let atlases = Atlases::load();
    atlases.with_themes(|dark, light| {
        let scene = Scene { name: "settings", which: Which::Settings, keys: &[] };
        let shot = |theme: &Theme<'_>| {
            let mut sheet = symbian_preview::Sheet::new(E72_SCREEN);
            render_live(&mut sheet.canvas(), &scene, theme);
            sheet.pixels().to_vec()
        };
        assert_ne!(shot(dark), shot(light), "the light palette drew the dark screen");
    });
}

/// The Order tab's chip is handed less room than it asked for, and the pill is clipped.
///
/// # This test asserts the wrong number on purpose
///
/// Found by looking at `preview-out/50-boot-list.png` and then measuring, because the sheet showed
/// `always` reading as `alway` and `crash loop` as `rash loop` — and a 2x PNG is not evidence.
/// The measurement, on the selected first row of a 320-pixel screen:
///
/// ```text
/// slot 3 (the title): x  18..275  (w 257)
/// slot 4 (the chip):  x 275..310  (w  35)
/// Chip::fresh("always").width(theme) = 41
/// ```
///
/// `Chip::draw` right-aligns its pill against `rect.x1`, so a 41-pixel pill is painted from 269 and
/// then clipped to the 35-pixel rect it was given — `layout` clips every leaf to its own rect. Six
/// pixels come off the *left*, which is the first character of the word.
///
/// The defect is in the layout of a `Row` with one flexed child and one fixed one, not in this
/// crate: the fixed child is short by exactly one `Gap::Base`, which is the gap the row reserves.
/// `symbian-bootctl` only supplies the two children. Fixing it belongs in `symbian-decl-ui` and it
/// moves pixels on every screen in this SDK that puts a chip on a row, which is why it is recorded
/// here rather than done here.
///
/// So this asserts `35` — today's wrong answer. **When it fails, the layout was fixed**: delete the
/// pin, regenerate the frozen `reference` for the Order scenes, and read the new sheets. A test that
/// merely said "less than 41" would keep passing through the fix and tell nobody.
#[test]
fn the_order_tabs_chip_is_placed_narrower_than_it_asked_for() {
    use symbian_decl_ui::cache::UiCache;
    use symbian_decl_ui::constraints::Constraints;
    use symbian_decl_ui::layout;

    let atlases = Atlases::load();
    atlases.with_themes(|dark, _light| {
        let scene = Scene { name: "order-plain", which: Which::Boot, keys: &[] };
        let screen = boot_for(&scene, dark);
        let node = screen.row_node_for_test(0);
        // The band `draw_list` hands a row: the content width less the scrollbar gutter.
        let band = Rect::new(0, 56, 316, 94);
        let theme = dark.on(symbian_ui::Ground::Band);
        let mut cache = UiCache::with_capacity(node.slot_count() + 4);
        layout::measure_node(
            &node,
            0,
            Constraints::tight(band.width(), band.height()),
            &theme,
            &mut cache,
        );
        layout::layout_node(&node, 0, band, &mut cache, &theme);

        let wanted = symbian_ui::Chip::fresh("always").width(dark);
        let got = cache.rect(4).expect("the chip is the fifth slot of a row with a leading index");
        assert_eq!(wanted, 41, "the chip's own width changed; re-measure before trusting the rest");
        // **It was fixed, and this pin is what announced it.** The assertion below used to read 35,
        // with the message "if it is now 41, the layout was fixed — read the doc comment". It is now
        // 41, and the doc comment above records what it was.
        //
        // The cause was one word in `list_item.rs`: `line()` wrapped the title and the trailing
        // widget in a `Row` that was **not** flexed, so it was measured against the whole line and
        // reported the whole line back — while placement gave it the line *less the leading widget
        // and its gap*, and clamped the shortfall out of the last child. The trailing widget paid,
        // always, and exactly one `Gap::Base` of it.
        assert_eq!(
            got.width(),
            wanted,
            "the trailing widget must get the width it measured"
        );
        // The slot index is positional, so this pins that slot 4 really is the thing against the
        // row's right edge and not, say, the title. Without it a renumbered tree would keep the test
        // green while measuring the wrong box.
        assert_eq!(
            (got.x0, got.x1),
            (269, 310),
            "slot 4 is no longer the trailing chip"
        );
    });
}

/// The move-mode arrows are not in the phone's fonts, so the row in the air says nothing.
///
/// # Found by looking at a sheet, confirmed by counting ink
///
/// `preview-out/51-boot-move.png` shows the selected row in move mode with an empty right-hand end.
/// `BootScreen::move_hint` returns `↑`, `↓` or `↕` there, and `row_node` gives up the chip slot to
/// show it — its doc comment argues at length that a single `↕` lies at both ends of the list, which
/// is correct and which none of the reader can see, because **all three glyphs are absent from the
/// atlases the E72 links**.
///
/// Measuring the string does not reveal this: `Font::measure` answers `3` for `↕`, for `i`, for a
/// space, for a private-use codepoint and for a CJK ideograph alike — a missing glyph gets the
/// notdef advance and no complaint. What reveals it is counting pixels: `↕` puts **0** on the
/// canvas and `i` puts 16.
///
/// Not fixed here, for the reason the chip pin above gives: it moves pixels, and this pass has a
/// parity mandate. The fix is a choice between a glyph `symbian_ui::icon` draws itself — which is
/// what that module exists for, and where `Icon` already lives for twenty others — and a word.
///
/// **When this test fails, the arrows became visible.** Delete the pin and look at the sheet.
#[test]
fn the_move_mode_arrows_put_no_ink_on_the_real_atlas() {
    use symbian_gfx::{Color, Rect as GRect, Size};

    let atlases = Atlases::load();
    atlases.with_themes(|dark, _light| {
        let ink = |text: &str| {
            let mut sheet = symbian_preview::Sheet::new(Size::new(40, 20));
            {
                let mut c = sheet.canvas();
                c.clear(Color::hex(0x000000));
                c.draw_text_in(
                    GRect::new(0, 0, 40, 20),
                    text,
                    dark.fonts.body,
                    Color::hex(0xffffff),
                    symbian_ui::Align::Start,
                );
            }
            sheet.pixels().iter().filter(|p| **p != 0).count()
        };

        // The negative control, and it is the whole reason this test is worth anything: an
        // instrument that reported "no ink" for every string would agree with the arrows perfectly.
        assert!(ink("i") > 0, "the atlas draws nothing at all — the instrument is broken");

        for arrow in ["\u{2191}", "\u{2193}", "\u{2195}"] {
            assert_eq!(ink(arrow), 0, "{arrow:?} now draws — read the doc comment on this test");
        }
    });
}
