//! NetProbe.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use symbian_ui::{
    chrome, App, Canvas, Handled, Key, KeyEvent, Rect, Softkey, Theme,
};

pub struct NetProbe {
    count: i32,
    exit: bool,
}

impl NetProbe {
    pub fn new() -> Self {
        Self { count: 0, exit: false }
    }
}

impl Default for NetProbe {
    fn default() -> Self {
        Self::new()
    }
}

/// The worker's job table. One opcode for now.
pub const OP_MODPOW: i32 = 1;

/// Runs on the worker thread, not the GUI thread.
///
/// `modpow` is the right first job for this facility rather than a synthetic one: it
/// takes 0.4-0.6 s on this hardware, which is exactly the case the thread exists for,
/// and it allocates nothing — fixed-size arrays over the caller's slices — so it
/// satisfies the "nothing the job allocates may outlive it" contract by construction.
///
/// Input is three length-prefixed byte strings: modulus, base, exponent. Crude, and
/// appropriate: a job interface crossing a thread boundary with no allocator in common
/// is not the place for a serialisation format.
pub fn modpow_job(opcode: i32, input: &[u8], out: &mut [u8]) -> i32 {
    if opcode != OP_MODPOW {
        return -5; // SHIM_ERR_NOT_SUPPORTED
    }
    let mut fields: [&[u8]; 3] = [&[], &[], &[]];
    let mut rest = input;
    for f in fields.iter_mut() {
        if rest.len() < 2 {
            return -6; // SHIM_ERR_ARGUMENT
        }
        let n = u16::from_be_bytes([rest[0], rest[1]]) as usize;
        if rest.len() < 2 + n {
            return -6;
        }
        *f = &rest[2..2 + n];
        rest = &rest[2 + n..];
    }
    let Ok(m) = symbian_crypto::bignum::Modulus::new(fields[0]) else {
        return -6;
    };
    match symbian_crypto::bignum::modpow(fields[1], fields[2], &m, out) {
        Ok(()) => 0,
        Err(_) => -6,
    }
}

impl App for NetProbe {
    fn title(&self) -> &str {
        "NetProbe"
    }

    fn handle_key(&mut self, ev: KeyEvent, _theme: &Theme<'_>, _screen: Rect) -> Handled {
        match ev.key {
            Key::Up => self.count += 1,
            Key::Down => self.count -= 1,
            // The right softkey is Back on this platform, and on the first screen Back
            // means exit. Never call the framework's exit yourself — set a flag and let
            // the host do it, because Avkon owns the loop.
            Key::Softkey(Softkey::Right) | Key::End => self.exit = true,
            _ => return Handled::Ignored,
        }
        Handled::Consumed
    }

    fn should_exit(&self) -> bool {
        self.exit
    }

    fn draw(&mut self, c: &mut Canvas<'_>, theme: &Theme<'_>) {
        use symbian_ui::Align;

        let screen = Rect::from_size(c.size());
        let frame = chrome::Frame::split(screen, theme, true, true);

        chrome::clear(c, theme);
        chrome::title_bar(c, frame.title, theme, "NetProbe", None);
        chrome::softkey_bar(c, frame.softkeys, theme, [Some("Options"), None, Some("Exit")]);

        // A number and a hint. Replace this with your screen.
        let mut buf = [0u8; 16];
        let text = fmt_i32(self.count, &mut buf);
        c.draw_text_in(frame.content, text, theme.fonts.title, theme.palette.text, Align::Center);

        let hint = Rect { y0: frame.content.y1 - 20, ..frame.content };
        c.draw_text_in(hint, "up / down", theme.fonts.small, theme.palette.dim, Align::Center);
    }
}

/// Format an integer without `core::fmt`, which on this target pulls in far more code
/// than a two-line loop.
fn fmt_i32(v: i32, buf: &mut [u8; 16]) -> &str {
    let neg = v < 0;
    let mut m = v.unsigned_abs();
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (m % 10) as u8;
        m /= 10;
        if m == 0 || i == 0 {
            break;
        }
    }
    if neg && i > 0 {
        i -= 1;
        buf[i] = b'-';
    }
    core::str::from_utf8(&buf[i..]).unwrap_or("?")
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian_ui::testing;

    fn press(app: &mut NetProbe, key: Key) -> Handled {
        testing::with_theme(symbian_ui::Palette::DARK, |theme| {
            let ev = KeyEvent { key, mods: Default::default(), repeat: false };
            app.handle_key(ev, theme, testing::SCREEN)
        })
    }

    #[test]
    fn up_and_down_move_the_count() {
        let mut app = NetProbe::new();
        assert_eq!(press(&mut app, Key::Up), Handled::Consumed);
        assert_eq!(press(&mut app, Key::Up), Handled::Consumed);
        assert_eq!(press(&mut app, Key::Down), Handled::Consumed);
        assert_eq!(app.count, 1);
    }

    #[test]
    fn keys_this_app_does_not_use_are_left_alone() {
        // Returning Ignored is what lets the platform act on a key instead — and on the
        // device that includes the red End key, which is how the user gets out.
        let mut app = NetProbe::new();
        assert_eq!(press(&mut app, Key::Left), Handled::Ignored);
    }

    #[test]
    fn back_asks_to_exit_rather_than_exiting() {
        let mut app = NetProbe::new();
        assert!(!app.should_exit());
        press(&mut app, Key::Softkey(Softkey::Right));
        assert!(app.should_exit(), "Back on the first screen should ask to close");
    }

    #[test]
    fn draw_fills_the_screen() {
        // A widget that silently draws nothing passes every test about its return value.
        let mut app = NetProbe::new();
        let (_, px) = testing::with_canvas(symbian_gfx::Size::new(320, 240), |c| {
            testing::with_theme(symbian_ui::Palette::DARK, |theme| app.draw(c, theme));
        });
        assert!(px.iter().any(|&p| p != 0), "draw produced an empty frame");
    }

    #[test]
    fn formatting_covers_the_awkward_values() {
        let mut b = [0u8; 16];
        assert_eq!(fmt_i32(0, &mut b), "0");
        let mut b = [0u8; 16];
        assert_eq!(fmt_i32(-42, &mut b), "-42");
        let mut b = [0u8; 16];
        assert_eq!(fmt_i32(i32::MAX, &mut b), "2147483647");
    }
}
