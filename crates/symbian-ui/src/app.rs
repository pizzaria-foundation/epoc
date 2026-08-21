//! The contract an application implements, so the host simulator and the device
//! entry points can both drive it.
//!
//! # Why a trait at all
//!
//! Without one, every tool that runs an app names its concrete type. The simulator did
//! exactly that — `tg::App` was hardcoded in it — which meant a second app could not be
//! previewed at all, and the device glue was 120 lines of allocator and panic handler
//! copy-pasted per app.
//!
//! [`App`] is deliberately tiny: three methods, all of which the app already had. It is
//! not a framework, it is the four things a host needs to know — hand it a key, ask it
//! to draw, ask whether it wants to close, and ask what its window should be called.
//!
//! # Why it takes the theme rather than owning it
//!
//! `Theme` borrows its font atlases, so an app that owned one would be self-referential.
//! Passing it in also means the *host* chooses: the simulator can cycle palettes with a
//! keypress and the device can pick from settings, without the app knowing either
//! happened. That is the same reason the era's own toolkit resolved colours by role
//! rather than by value.
//!
//! # Why `handle_key` gets the screen rect
//!
//! Because hit-testing and scrolling need to know how much room there is, and the layout
//! is recomputed on every draw rather than retained. An app that cached its own bounds
//! would be wrong for exactly one frame after a rotation or a soft-key bar appearing —
//! which is the frame during which the user is pressing something.

use symbian_gfx::{Canvas, Rect};

use crate::clip::Clipboard;
use crate::input::{Handled, KeyEvent};
use crate::theme::Theme;

/// A platform event, before any interpretation.
///
/// The shim's own event type, handed through unchanged rather than copied into a
/// toolkit-shaped one. There used to be a separate struct here, on the reasoning that a
/// widget toolkit should not know about the ABI — and the result was two identical
/// definitions and a field-by-field conversion wherever `symbian::net` met
/// `App::handle_raw`. One type is worth the dependency.
///
/// Most apps never see one: [`App::handle_raw`] defaults to ignoring them and the host
/// translates keys into [`KeyEvent`] instead. It exists for the two cases where
/// translation is the wrong thing:
///
/// - **diagnostics**, which need the numbers the platform actually sent. The E72's
///   keyboard bug was invisible for two rounds precisely because a translated view was
///   all anyone looked at.
/// - **async completions** — a socket connecting, a timer firing — which are not keys and
///   have no `KeyEvent` to become.
pub type RawEvent = symbian_sys::ShimEvent;

/// An application the SDK can run, on a device or in the simulator.
pub trait App {
    /// A key arrived. Return [`Handled::Consumed`] if anything changed, which is what
    /// tells the host to repaint — returning `Consumed` unconditionally works and costs
    /// a redraw per keystroke, which on a 600 MHz device is worth avoiding.
    fn handle_key(&mut self, ev: KeyEvent, theme: &Theme<'_>, screen: Rect) -> Handled;

    /// Draw a whole frame. There is no partial redraw: at 320x240 the full repaint is
    /// 76,800 pixels, and tracking dirty regions costs more than it saves until the
    /// frame budget is actually exceeded.
    fn draw(&mut self, c: &mut Canvas<'_>, theme: &Theme<'_>);

    /// A platform event arrived, before translation.
    ///
    /// Return [`Handled::Consumed`] to stop it becoming a [`KeyEvent`] — which a
    /// diagnostic wants and an ordinary app does not. The default ignores everything, so
    /// implementing [`Self::handle_key`] alone is enough for a normal app.
    fn handle_raw(&mut self, _ev: &RawEvent) -> Handled {
        Handled::Ignored
    }

    /// True once the app wants to close. The host acts on it — on a device by asking the
    /// framework to exit, which an app must never do itself, since Avkon owns the loop.
    fn should_exit(&self) -> bool {
        false
    }

    /// Shown in the simulator's title bar. Unused on the device, where the caption comes
    /// from the app's registration resource.
    fn title(&self) -> &str {
        "app"
    }

    /// Hand the app a platform clipboard, so its text fields can copy and paste.
    ///
    /// A defaulted no-op: an app that holds no clipboard (or is not a bridge) simply ignores it,
    /// and paste stays the quiet no-op an empty clipboard already is. `entry!` calls this once on
    /// the device with the system clipboard, so every app gets copy-and-paste without wiring it —
    /// the SDK-wide fix for "paste does nothing". The clipboard trait lives here in `symbian-ui`,
    /// which is what lets this method exist without the toolkit depending on the shim that
    /// implements it.
    fn install_clipboard(&mut self, _clip: alloc::boxed::Box<dyn Clipboard>) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use symbian_gfx::Size;

    /// The smallest thing that satisfies the trait, which is the point: a new project
    /// should not need more than this to get a window.
    struct Minimal {
        presses: u32,
    }

    impl App for Minimal {
        fn handle_key(&mut self, _ev: KeyEvent, _t: &Theme<'_>, _s: Rect) -> Handled {
            self.presses += 1;
            Handled::Consumed
        }

        fn draw(&mut self, c: &mut Canvas<'_>, theme: &Theme<'_>) {
            c.clear(theme.palette.bg.mid());
        }
    }

    fn atlas() -> vec::Vec<u8> {
        let mut v = vec::Vec::new();
        v.extend_from_slice(b"SBF1");
        v.extend_from_slice(&12u16.to_le_bytes());
        v.extend_from_slice(&9i16.to_le_bytes());
        v.extend_from_slice(&3i16.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.push(1);
        v.push(5);
        v.extend_from_slice(&0u16.to_le_bytes());
        v.extend_from_slice(&(b'a' as u32).to_le_bytes());
        v.extend_from_slice(&0u32.to_le_bytes());
        v.extend_from_slice(&[4, 6, 5, 0]);
        v.extend_from_slice(&0i16.to_le_bytes());
        v.extend_from_slice(&6i16.to_le_bytes());
        v.extend(core::iter::repeat_n(0xFFu8, 24));
        v
    }

    #[test]
    fn a_minimal_app_can_be_driven_through_the_trait() {
        let data = atlas();
        let f = symbian_gfx::BitmapFont::new(&data).unwrap();
        let fonts = crate::theme::Fonts { body: &f, strong: &f, small: &f, title: &f };
        let theme = Theme::dark(fonts);

        let mut app = Minimal { presses: 0 };
        let mut buf = alloc::vec![0u16; 320 * 240];
        let mut c = Canvas::from_slice(&mut buf, Size::new(320, 240));

        let ev = KeyEvent {
            key: crate::input::Key::Down,
            mods: crate::input::Modifiers::default(),
            repeat: false,
        };
        assert_eq!(app.handle_key(ev, &theme, Rect::from_size(Size::new(320, 240))), Handled::Consumed);
        app.draw(&mut c, &theme);
        drop(c);

        assert_eq!(app.presses, 1);
        let bg = theme.palette.bg.mid().to_rgb565().0;
        assert!(buf.iter().all(|&p| p == bg), "draw did not fill the screen");
    }

    #[test]
    fn the_defaults_are_the_ones_a_new_app_wants() {
        // should_exit and title are defaulted so a first sketch implements two methods,
        // not four. If either default changed to something surprising, a new project
        // would exit immediately or show a blank title and the cause would be here.
        let app = Minimal { presses: 0 };
        assert!(!app.should_exit());
        assert_eq!(app.title(), "app");
    }
}
