//! The packages screen, rendered to PNG so a layout can be judged before it reaches a phone.
//!
//! ```text
//! cargo run -p symbian-pkgui --example preview     # → preview-out/
//! ```
//!
//! This exists because of a bug that cost two rounds on the handset: `Rect::split_right` answers the
//! *cut* first, so reading it as left-to-right put every label in a narrow strip on the right. No
//! content test sees that, and a person holding the phone described it as "the screen is over to the
//! right". A picture would have said so in one look.
//!
//! The data below is invented, and has to be: `apps::installed()` reaches the phone's registry, which
//! off-device answers `NotReady`. The names and sizes are ones this project actually carries, because
//! a row is worth nothing if the widths are not the widths a real row has to fit.

use symbian_gfx::{Rect, E72_SCREEN};
use symbian_pkgui::PkgScreen;
use symbian_preview::{Atlases, Sheet};
use symbian_ui::{Key, KeyEvent, Theme};

const OUT: &str = "preview-out";

#[path = "fixtures.rs"]
mod fixtures;

use fixtures::{cands, catalog, pkgs, queue, repos};

fn main() {
    let atlases = Atlases::load();
    atlases.with_themes(render);
}

fn render(dark: &Theme<'_>, light: &Theme<'_>) {
    for (theme, tag) in [(dark, "dark"), (light, "light")] {
        for (name, keys) in SCENES {
            let mut sheet = Sheet::new(E72_SCREEN);
            {
                let mut c = sheet.canvas();
                draw_scene(&mut c, theme, keys);
            }
            sheet.save(OUT, &format!("{name}-{tag}"));
        }
    }
    println!("wrote {OUT}/pkg-*.png");
}

/// The states worth a picture. `Right` steps one tab along, so a scene says which section it wants by
/// how many times it presses it.
const SCENES: [(&str, &[Key]); 6] = [
    ("pkg-installed", &[]),
    ("pkg-available", &[Key::Right]),
    ("pkg-repos", &[Key::Right, Key::Right]),
    ("pkg-downloads", &[Key::Right, Key::Right, Key::Right]),
    // The sheet: the screen the row label used to try to be.
    ("pkg-sheet", &[Key::Select]),
    // The prompt that asks for a repository, over the list it was opened from.
    (
        "pkg-addrepo",
        &[
            Key::Right,
            Key::Right,
            Key::Softkey(symbian_ui::Softkey::Left),
            Key::Select,
        ],
    ),
];

/// One scene onto a canvas — the same path the pictures are written from, so the test below is of the
/// pictures and not of something beside them.
fn draw_scene(c: &mut symbian_gfx::Canvas<'_>, theme: &Theme<'_>, keys: &[Key]) {
    let mut s = PkgScreen::new(pkgs(), cands(), catalog(), repos(), queue());
    for k in keys {
        s.handle_key(KeyEvent::new(*k), theme, Rect::from_size(E72_SCREEN));
    }
    s.draw(c, theme);
}

/// No two sheets are the same picture.
///
/// Registered as a test for a reason with a cost attached: an unregistered preview in this project
/// wrote three identical sheets under three different names for months, because the key sequence that
/// was meant to open a detail screen hit a tab strip that clamps. Nothing failed, because nothing was
/// asserting. This is the assertion that would have caught it.
#[test]
fn every_sheet_is_a_different_picture() {
    let atlases = Atlases::load();
    atlases.with_themes(|dark, light| {
        let mut shots: Vec<(String, Vec<u16>)> = Vec::new();
        for (theme, tag) in [(dark, "dark"), (light, "light")] {
            for (name, keys) in SCENES {
                let mut sheet = Sheet::new(E72_SCREEN);
                {
                    let mut c = sheet.canvas();
                    draw_scene(&mut c, theme, keys);
                }
                shots.push((format!("{name}-{tag}"), sheet.pixels().to_vec()));
            }
        }
        for (i, (a, pa)) in shots.iter().enumerate() {
            for (b, pb) in shots.iter().skip(i + 1) {
                assert_ne!(pa, pb, "{a} and {b} drew the same picture");
            }
        }
    });
}
