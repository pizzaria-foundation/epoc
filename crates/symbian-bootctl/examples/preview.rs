//! The boot manager's screens, rendered to PNG so a change can be judged before it reaches a phone.
//!
//! ```text
//! cargo run -p symbian-bootctl --example preview     # → preview-out/
//! ```
//!
//! The machinery — the pixel buffer, the atlases chained the way the device chains them, the PNG
//! writer — is `symbian-preview`. What lives here is only the scenes.
//!
//! The roster and the boot list below are invented, and have to be: `apps::installed()` reaches the
//! phone's application registry, which off-device answers `NotReady`. The captions are ones an E72
//! actually carries, because a sheet is worth nothing if the widths are not the widths a real row
//! has to fit.

// draw/handle_key are trait methods; the trait must be in scope to call them.
use symbian_ui::App as _;

use symbian::apps::AppInfo;
use symbian_bootcfg::status::{EntryStatus, Mode, State};
use symbian_bootcfg::{BootConfig, BootStatus, Entry, Policy};
use symbian_bootctl::BootScreen;
use symbian_gfx::{Rect, E72_SCREEN};
use symbian_preview::{Atlases, Sheet};
use symbian_ui::{Key, KeyEvent, Theme};

const OUT: &str = "preview-out";

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

/// A boot list with one of each policy, and one entry the supervisor switched off itself — the
/// state a reader most needs to recognise, because it is the one nobody configured.
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

/// What bootd wrote after a boot that mostly worked.
fn status() -> BootStatus {
    BootStatus {
        mode: Mode::Normal,
        boot_count: 0,
        restarts_used: 2,
        entries: vec![
            EntryStatus {
                uid3: 0x2000_1234,
                last_rc: 0,
                launch_at_s: 10,
                restarts: 2,
                state: State::Alive,
            },
            EntryStatus {
                uid3: 0x1000_58C5,
                last_rc: 0,
                launch_at_s: 13,
                restarts: 0,
                state: State::Alive,
            },
            EntryStatus {
                uid3: 0x1000_5905,
                last_rc: 0,
                launch_at_s: 14,
                restarts: 0,
                state: State::Dead,
            },
            EntryStatus {
                uid3: 0x1020_7236,
                last_rc: -1,
                launch_at_s: 16,
                restarts: 3,
                state: State::Disarmed,
            },
        ],
    }
}

fn main() {
    let atlases = Atlases::load();
    atlases.with_themes(render);
}

fn render(dark: &Theme<'_>, light: &Theme<'_>) {
    let rect = Rect::from_size(E72_SCREEN);

    let sheet = |name: &str, draw: &mut dyn FnMut(&mut symbian_gfx::Canvas<'_>)| {
        let mut s = Sheet::new(E72_SCREEN);
        {
            let mut c = s.canvas();
            draw(&mut c);
        }
        s.save(OUT, name);
    };

    // Right steps one tab along, so `keys` is how a scene says which tab it wants.
    let screen = |theme: &Theme<'_>, keys: &[Key]| {
        let mut s = BootScreen::new(config(), Some(status()), roster());
        for k in keys {
            s.handle_key(KeyEvent::new(*k), theme, rect);
        }
        s
    };

    let mut s = screen(dark, &[]);
    sheet("50-boot-list", &mut |c| s.draw(c, dark));

    // Move mode: the title changes, the softkey becomes Done, and the row carries an arrow.
    let mut s = screen(dark, &[Key::Softkey(symbian_ui::Softkey::Left)]);
    sheet("51-boot-move", &mut |c| s.draw(c, dark));

    let mut s = screen(dark, &[Key::Right]);
    sheet("52-boot-entry", &mut |c| s.draw(c, dark));

    // The policy dropdown open over the rows beneath it.
    let mut s = screen(dark, &[Key::Right, Key::Down, Key::Select]);
    sheet("53-boot-policy", &mut |c| s.draw(c, dark));

    let mut s = screen(dark, &[Key::Right, Key::Right]);
    sheet("54-boot-setup", &mut |c| s.draw(c, dark));

    let mut s = screen(dark, &[Key::Right, Key::Right, Key::Right]);
    sheet("55-boot-status", &mut |c| s.draw(c, dark));

    // The picker that adds an entry, over the list.
    let mut s = screen(dark, &[Key::Down, Key::Down, Key::Down, Key::Down, Key::Select]);
    sheet("56-boot-picker", &mut |c| s.draw(c, dark));

    let mut s = screen(light, &[]);
    sheet("57-boot-list-light", &mut |c| s.draw(c, light));
}
