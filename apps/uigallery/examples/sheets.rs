//! `cargo run -p uigallery --example sheets` — every page, in every palette, as PNGs.
//!
//! # Why this exists next to the simulator
//!
//! `--example sim` opens a window, which is the right tool for pressing keys and the wrong one for
//! *reviewing*: it shows one page in one palette at a time, and nothing it shows can be attached to a
//! message. Eight pages across five palettes is forty screens, and the only way to look at forty
//! screens is to have forty files.
//!
//! It is also the honest way to check a palette decision before anyone carries a phone anywhere. What
//! it cannot do — and the reason the handset is still owed a session — is tell you whether a 30x18
//! switch reads as a switch on a TN panel in daylight.
//!
//! The app is driven the way the device drives it: `symbian_ui::App::draw` into a real 320x240 RGB565
//! buffer, with the device's own font atlases chained through `WithFallback`. Nothing here reaches
//! into the app's internals except to turn the page, which is what a keypress would do.

use symbian_gfx::E72_SCREEN;
use symbian_preview::{Atlases, Sheet};
use symbian_ui::App as _;

/// Where the sheets land, relative to wherever this was run from.
const OUT: &str = "gallery-out";

fn main() {
    let atlases = Atlases::load();
    let mut written = 0;

    // The phone's own theme, from the four colours `skinprobe` measured on the E72 — see
    // `docs/reference/skinprobe.txt`. The host has no skin server, but it does not need one: the seeds
    // are data now, so the sixth palette can be rendered here like any other. That is the whole
    // argument for deriving from seeds rather than reading 35 colours off a device.
    let phone = uigallery::phone_theme_from_measured_seeds();
    let offer = symbian_ui::Palette::count(phone);

    for index in 0..offer {
        let name = symbian_ui::Palette::at(index, phone).0;
        atlases.with_fonts(|fonts| {
            for page in 0..uigallery::page_count() {
                // A fresh app per page rather than one driven through eight page turns: a sheet that
                // inherited the previous page's cursor would be showing state no reviewer chose, and
                // the point of a contact sheet is that every frame on it is reproducible from its
                // filename alone.
                let mut app = uigallery::Uigallery::new();
                app.set_phone_theme(phone);
                app.goto_palette(index);
                app.goto_page(page);

                // The theme is built from what the *app* now thinks the palette is, not from what this
                // loop picked. The first version did the opposite and produced a sheet of the light
                // palette captioned "Dark": the caption read the model and the pixels read the loop.
                // Reading both from one place is the same rule the app follows for its softkeys.
                let theme = symbian_ui::Theme::new(uigallery::palette(), fonts);

                let mut sheet = Sheet::new(E72_SCREEN);
                {
                    let mut c = sheet.canvas();
                    app.draw(&mut c, &theme);
                }
                // Lower-cased and hyphenated so the files sort by palette and then by page, which is
                // the order anyone reviewing them wants to read.
                let slug = name.to_lowercase().replace(' ', "-");
                sheet.save(OUT, &format!("{slug}-{page}-{}", uigallery::page_title_of(page)));
                written += 1;
            }
        });
    }

    println!(
        "{written} sheets in {OUT}/  ({} pages x {offer} palettes, the phone's own included)",
        uigallery::page_count()
    );
    println!("what to look for:");
    println!("  the Switch row      — does a 30x18 pill read as a switch?");
    println!("  the Slider row      — is the fill distinguishable from the track?");
    println!("  the Stepper row     — are the chevrons on the text's baseline?");
    println!("  the radio rows      — is the chosen one obvious at a glance?");
    println!("  every palette       — does `dim` text survive, or vanish?");
    println!("  the Instruments row — f= must be 0");
    println!();
    println!("note: the Instruments row reads m=0 f=0 on a sheet and that is not a finding.");
    println!("      The harness reads the counters *after* the draw, so one frame per file means the");
    println!("      numbers land in a frame nobody renders. On the handset they are live. `f` is the");
    println!("      one that matters and the host asserts it: `cargo test -p uigallery`.");
}
