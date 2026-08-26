//! The packages screen, drawn twice, compared pixel for pixel — in every state that has a state.
//!
//! ```text
//! cargo run  -p symbian-pkgui --example parity     # → parity-out/ and a report
//! cargo test -p symbian-pkgui --example parity     # the assertion
//! ```
//!
//! [`PkgScreen::draw_as`] takes which row painter to use. `Rows::Imperative` is the screen as it
//! shipped — rects computed by hand, ink laid down by hand. `Rows::Declared` builds each row from
//! `symbian-decl-ui` widgets and runs the layout pass over it. Both are handed the same screen, in
//! the same section, with the same cursor, and the buffers must match.
//!
//! # What this can and cannot see
//!
//! Only the rows differ between the two arms; the title bar, the tab strip, the softkey bar and the
//! three overlays are one piece of code called by both. So a defect in the chrome is invisible here,
//! and so is a defect in [`PkgScreen::lines`] — both sides read the same model. What it sees is the
//! one thing being rewritten: where a row's text, chip, bar and second line land.
//!
//! Two scenes are deliberately blind and kept anyway: `pkg-sheet` returns before a row is drawn, so
//! it cannot fail. It is here because it is a state a person can be in and because a rewrite that
//! *started* drawing rows under the sheet would show up as a diff rather than as a mystery on the
//! handset.
//!
//! # Why the scenes are what they are
//!
//! Not one per look — one per **branch of the row painter**. It has four: a chip or none, a second
//! line or none, a determinate meter, an indeterminate one. Times four sections, because each builds
//! its rows differently, plus the selection (which changes every colour in the row), plus a list long
//! enough to scroll (which is the arithmetic that decides which rows are drawn at all), plus the
//! empty state (which is the branch that draws no rows), plus the light palette.
//!
//! The count matters: the first parity harness in this SDK reported `identical` on one scene, and the
//! states nobody rendered were hiding a real divergence.

use std::process::ExitCode;

use symbian_bootcfg::catalog::CatalogDb;
use symbian_bootcfg::pkg::{Candidate, PkgDb};
use symbian_bootcfg::queue::Queue;
use symbian_bootcfg::repo::RepoDb;
use symbian_gfx::{Canvas, Rect, E72_SCREEN};
use symbian_pkgui::{PkgScreen, Rows};
use symbian_preview::{Atlases, Parity};
use symbian_ui::{Key, KeyEvent, Softkey, Theme};

#[path = "fixtures.rs"]
mod fixtures;

const OUT: &str = "parity-out";

fn main() -> ExitCode {
    let atlases = Atlases::load();
    // `keep_matching`, so a passing run still leaves the pictures behind: a comparison whose output
    // only exists when it fails is one nobody looks at when it passes, and looking is how a *shared*
    // omission gets caught — two sides that both fail to draw something agree perfectly.
    let mut p = Parity::new(OUT).keep_matching(true);
    let atlases_ref = &atlases;
    atlases_ref.with_themes(|dark, light| {
        for scene in scenes() {
            let theme = if scene.light { light } else { dark };
            p.check(
                scene.name,
                theme,
                |c| render(c, &scene, theme, Rows::Imperative),
                |c| render(c, &scene, theme, Rows::Declared),
            );
        }
    });
    println!("{}", p.report());
    if p.diffs().is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Which invented phone a scene is of.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Data {
    /// Four managed packages, two candidates, a catalogue, three repositories, a queue mid-flight.
    Full,
    /// A phone that has just been set up. Every section is empty.
    Nothing,
    /// Sixteen managed packages, which is more than fits.
    Many,
    /// The queue's running job with no `Content-Length`, so its meter is the indeterminate one.
    Unknown,
}

impl Data {
    fn build(self) -> PkgScreen {
        let (pkgs, cands, cat, repos, queue): (PkgDb, Vec<Candidate>, CatalogDb, RepoDb, Queue) =
            match self {
                Data::Full => (
                    fixtures::pkgs(),
                    fixtures::cands(),
                    fixtures::catalog(),
                    fixtures::repos(),
                    fixtures::queue(),
                ),
                Data::Nothing => fixtures::nothing(),
                Data::Many => (
                    fixtures::many_pkgs(),
                    fixtures::cands(),
                    fixtures::catalog(),
                    fixtures::repos(),
                    fixtures::queue(),
                ),
                Data::Unknown => (
                    fixtures::pkgs(),
                    fixtures::cands(),
                    fixtures::catalog(),
                    fixtures::repos(),
                    fixtures::queue_unknown(),
                ),
            };
        PkgScreen::new(pkgs, cands, cat, repos, queue)
    }
}

struct Scene {
    name: &'static str,
    data: Data,
    /// How the screen is got into this state — the same way a person gets into it. The cursor, the
    /// section and the open overlay are all the screen's own and none of them is settable.
    keys: &'static [Key],
    light: bool,
}

const OPTIONS: Key = Key::Softkey(Softkey::Left);

fn scenes() -> Vec<Scene> {
    use Key::{Down, Right, Select};
    let dark = |name, data, keys| Scene {
        name,
        data,
        keys,
        light: false,
    };
    let light = |name, data, keys| Scene {
        name,
        data,
        keys,
        light: true,
    };
    vec![
        // The four sections. Each builds its rows from a different model, so each is its own check.
        dark("pkg-installed", Data::Full, &[]),
        dark("pkg-installed-selected", Data::Full, &[Down, Down]),
        dark("pkg-available", Data::Full, &[Right]),
        dark("pkg-available-selected", Data::Full, &[Right, Down]),
        dark("pkg-repos", Data::Full, &[Right, Right]),
        dark(
            "pkg-repos-selected",
            Data::Full,
            &[Right, Right, Down, Down],
        ),
        // The meters. The running row is row 0, so `downloads` has the bar unselected and
        // `downloads-selected` has it on the band — where the imperative painter draws the bar and
        // its numbers in the *page's* colours, and the declared one has to do the same to match.
        dark("pkg-downloads", Data::Full, &[Right, Right, Right, Down]),
        dark("pkg-downloads-selected", Data::Full, &[Right, Right, Right]),
        dark(
            "pkg-downloads-unknown",
            Data::Unknown,
            &[Right, Right, Right],
        ),
        // A list longer than the viewport, with the cursor driven to the bottom of it: the scroll
        // offset is derived, and a partially visible row is clipped by the list rather than by the
        // row.
        dark(
            "pkg-scrolled",
            Data::Many,
            &[
                Down, Down, Down, Down, Down, Down, Down, Down, Down, Down, Down, Down,
            ],
        ),
        // The branch that draws no rows at all, in all four sections — four different sentences.
        dark("pkg-empty-installed", Data::Nothing, &[]),
        dark("pkg-empty-available", Data::Nothing, &[Right]),
        dark("pkg-empty-repos", Data::Nothing, &[Right, Right]),
        dark("pkg-empty-downloads", Data::Nothing, &[Right, Right, Right]),
        // The overlays, over the rows they were opened from.
        dark("pkg-sheet", Data::Full, &[Select]),
        dark("pkg-menu-installed", Data::Full, &[OPTIONS]),
        dark(
            "pkg-menu-downloads",
            Data::Full,
            &[Right, Right, Right, OPTIONS],
        ),
        dark("pkg-addrepo", Data::Full, &[Right, Right, OPTIONS, Select]),
        // The light palette, where `dim` and the selection band are different colours from the dark
        // one's — which is where an ink chosen against the wrong ground shows.
        light("pkg-installed-light", Data::Full, &[]),
        light("pkg-installed-light-selected", Data::Full, &[Down]),
        light("pkg-downloads-light", Data::Full, &[Right, Right, Right]),
    ]
}

/// One scene, rendered with the row painter asked for.
fn render(c: &mut Canvas<'_>, scene: &Scene, theme: &Theme<'_>, rows: Rows) {
    let mut s = scene.data.build();
    let rect = Rect::from_size(E72_SCREEN);
    for k in scene.keys {
        s.handle_key(KeyEvent::new(*k), theme, rect);
    }
    s.draw_as(c, theme, rows);
}

/// The pixels one scene produces, through the reference painter.
#[cfg(test)]
fn shot(scene: &Scene, theme: &Theme<'_>) -> Vec<u16> {
    let mut sheet = symbian_preview::Sheet::new(E72_SCREEN);
    {
        let mut c = sheet.canvas();
        render(&mut c, scene, theme, Rows::Imperative);
    }
    sheet.pixels().to_vec()
}

#[test]
fn the_declared_rows_are_the_rows_that_shipped() {
    let atlases = Atlases::load();
    let mut p = Parity::new(OUT).keep_matching(false);
    atlases.with_themes(|dark, light| {
        for scene in scenes() {
            let theme = if scene.light { light } else { dark };
            p.check(
                scene.name,
                theme,
                |c| render(c, &scene, theme, Rows::Imperative),
                |c| render(c, &scene, theme, Rows::Declared),
            );
        }
    });
    // The count is asserted as well as the diffs: a refactor that stops building scenes would
    // otherwise turn this into a green light for nothing.
    assert_eq!(
        p.checked(),
        scenes().len(),
        "a scene stopped being compared"
    );
    assert_eq!(
        p.checked(),
        21,
        "a scene was added or removed without the reader being told"
    );
    p.finish();
}

/// No two scenes draw the same picture.
///
/// The instrument checking that it is pointed at something. A scene whose keys never reach the
/// screen renders the previous scene's picture under a new name, passes, and proves nothing — which
/// is not hypothetical in this project: a preview left unchecked rendered three identical sheets
/// under three different names for months.
///
/// Pairwise rather than against a base, because that is the shape the failure took.
#[test]
fn every_scene_draws_a_different_picture() {
    let atlases = Atlases::load();
    atlases.with_themes(|dark, light| {
        let shots: Vec<(&str, Vec<u16>)> = scenes()
            .iter()
            .map(|s| (s.name, shot(s, if s.light { light } else { dark })))
            .collect();
        for (i, (a, pa)) in shots.iter().enumerate() {
            for (b, pb) in shots.iter().skip(i + 1) {
                assert_ne!(pa, pb, "{a} and {b} drew the same pixels");
            }
        }
    });
}
