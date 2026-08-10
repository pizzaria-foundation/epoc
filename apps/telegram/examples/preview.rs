//! The client's screens, rendered to PNG so a change can be judged before it reaches a phone.
//!
//! ```text
//! cargo run -p tg --example preview      # → preview-out/
//! ```
//!
//! The machinery — the pixel buffer, the atlases chained the way the device chains them, the
//! PNG writer — is `symbian-preview`. What lives here is only the scenes: which screen, in
//! which theme, after which keys. They belong next to the code they document, which is why
//! they are not in the SDK.

// handle_key and draw are trait methods; the trait must be in scope to call them.
use symbian_ui::App as _;

use symbian_gfx::{Rect, E72_SCREEN};
use symbian_preview::{Atlases, Sheet};

/// Where the sheets land, relative to wherever this was run from.
const OUT: &str = "preview-out";

fn main() {
    let atlases = Atlases::load(&symbian_preview::sdk_root());
    atlases.with_themes(render);
}

fn render(dark: &symbian_ui::Theme<'_>, light: &symbian_ui::Theme<'_>) {
    let rect = Rect::from_size(E72_SCREEN);

    use symbian_ui::{Key, KeyEvent, Softkey};

    let shot = |name: &str, theme: &symbian_ui::Theme<'_>, keys: &[Key]| {
        let mut app = tg::App::mock();
        for k in keys {
            app.handle_key(KeyEvent::new(*k), theme, rect);
        }
        let mut s = Sheet::new(E72_SCREEN);
        {
            let mut c = s.canvas();
            app.draw(&mut c, theme);
        }
        s.save(OUT, name);
    };

    shot("10-chats", dark, &[]);
    shot("11-chats-scrolled", dark, &[Key::Down, Key::Down, Key::Down, Key::Down, Key::Down]);
    shot("12-conversation", dark, &[Key::Select]);
    shot("13-transcript-focus", dark, &[Key::Select, Key::Up, Key::Up, Key::Up]);

    // Typing into the composer, to check the caret and the Send softkey appearing.
    let typed: Vec<Key> = "beleza, vou testar".chars().map(Key::Char).collect();
    let mut keys = vec![Key::Down, Key::Down, Key::Select];
    keys.extend(typed);
    shot("14-composing", dark, &keys);

    shot("15-cyrillic", dark, &[Key::Down, Key::Down, Key::Select]);
    shot("16-chats-light", light, &[]);
    let _ = Softkey::Left;

    // Login screens.
    {
        use tg::login::{Login, Screen as Ls};
        let shot_login = |name: &str, theme: &symbian_ui::Theme<'_>, screen: Ls| {
            let mut login = Login::for_preview(screen);
            let mut s = Sheet::new(E72_SCREEN);
            {
                let mut c = s.canvas();
                login.draw(&mut c, theme);
            }
            s.save(OUT, name);
        };
        shot_login("17-login-phone", dark, Ls::Phone {
            field: symbian_ui::TextField::with_limit(16),
            error: None,
        });
        shot_login("18-login-code", dark, Ls::Code {
            field: symbian_ui::TextField::with_limit(8),
            length: Some(5),
            error: None,
        });
        shot_login("19-login-password", dark, Ls::Password {
            field: {
                let mut f = symbian_ui::TextField::with_limit(128);
                f.set_masked(true);
                f
            },
            hint: String::from("dica do usuário"),
            error: None,
        });
    }
}
