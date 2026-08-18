//! The escape hatch for the resident launcher.
//!
//! `apps/launcher`, when resident, captures the Menu key and refuses to close on End — which is
//! what makes it a home screen, and also what makes it impossible to stop from its own UI. This
//! app is the way out: it reports whether the launcher is running and stops it on a keypress.
//!
//! # Why the kill is on a keypress, not on launch
//!
//! An earlier version killed in `new()`, during construction — before this app had drawn
//! anything. If the kill faults (killing a process the caller has no right to is a platsec panic
//! that takes down *this* app, not the target), it would vanish before showing why. So `new()`
//! only *reads* whether the launcher is up, and Select does the kill with the result on screen. A
//! reader learns the state before the dangerous call, and the outcome after it.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::format;
use alloc::string::String;

use symbian_ui::{chrome, App, Canvas, Handled, Key, KeyEvent, Point, Rect, Softkey, Theme};

/// The launcher's UID3, from `apps/launcher/app.conf`. Hard-coded rather than shared through a
/// crate because it is a device identity, not a Rust value — the same reason the launcher's own
/// UID lives only in its `app.conf` and `_reg.rss`.
const LAUNCHER_UID: u32 = 0xE0AA_0000;

pub struct KillHome {
    status: String,
    exit: bool,
}

impl KillHome {
    pub fn new() -> Self {
        let running = symbian::process::is_running(LAUNCHER_UID);
        symbian::log!("[kill] open running={running}");
        Self {
            status: if running {
                String::from("Home is running. Select to stop.")
            } else {
                String::from("Home is not running.")
            },
            exit: false,
        }
    }

    fn attempt_kill(&mut self) {
        let before = symbian::process::is_running(LAUNCHER_UID);
        symbian::log!("[kill] attempt before_running={before}");
        // Window-server KillTask first — the way one app ends another without owning it. If the
        // launcher is still up, fall back to RProcess::Kill (needs PowerMgmt).
        let via_ws = symbian::apps::kill(LAUNCHER_UID);
        let mut method = "ws";
        if symbian::process::is_running(LAUNCHER_UID) {
            method = "proc";
            let _ = symbian::process::kill(LAUNCHER_UID);
        }
        let after = symbian::process::is_running(LAUNCHER_UID);
        symbian::log!("[kill] done ws_ok={} method={method} after_running={after}", via_ws.is_ok());
        self.status = if !after {
            String::from("Home stopped.")
        } else if !before {
            String::from("Home was not running.")
        } else {
            format!("Still up (ws={via_ws:?}).")
        };
    }
}

impl Default for KillHome {
    fn default() -> Self {
        Self::new()
    }
}

impl App for KillHome {
    fn title(&self) -> &str {
        "Kill Home"
    }

    fn handle_key(&mut self, ev: KeyEvent, _theme: &Theme<'_>, _screen: Rect) -> Handled {
        match ev.key {
            Key::Select => {
                self.attempt_kill();
                Handled::Consumed
            }
            Key::Softkey(Softkey::Right) => {
                self.exit = true;
                Handled::Consumed
            }
            _ => Handled::Ignored,
        }
    }

    fn draw(&mut self, c: &mut Canvas<'_>, theme: &Theme<'_>) {
        let screen = Rect::from_size(c.size());
        let frame = chrome::Frame::split(screen, theme, true, true);
        chrome::clear(c, theme);
        chrome::title_bar(c, frame.title, theme, "Kill Home", None);
        // The action is on the D-pad centre (see `handle_key`), so its label goes in the middle
        // slot. It sat on the left for a while, which told the user to press a key that did
        // nothing — the label is a promise about which key acts.
        chrome::softkey_bar(c, frame.softkeys, theme, chrome::Softkeys::action("Stop", "Exit"));

        let body = theme.fonts.body;
        let mut y = frame.content.y0 + 6;
        for line in [self.status.as_str(), "", "Select stops Home;", "Exit closes this."] {
            c.draw_text(Point::new(frame.content.x0 + 6, y + body.ascent()), line, body, theme.palette.text);
            y += body.line_height();
        }
    }

    fn should_exit(&self) -> bool {
        self.exit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian_ui::{testing, Palette};

    #[test]
    fn it_constructs_and_draws() {
        let mut app = KillHome::new();
        assert!(!app.status.is_empty());
        let (_, px) = testing::with_canvas(symbian_ui::gfx::Size::new(320, 240), |c| {
            testing::with_theme(Palette::DARK, |t| app.draw(c, t));
        });
        assert!(px.iter().any(|&p| p != 0));
    }

    #[test]
    fn select_attempts_a_kill_without_panicking_on_host() {
        let mut app = KillHome::new();
        testing::with_theme(Palette::DARK, |t| {
            app.handle_key(KeyEvent::new(Key::Select), t, testing::SCREEN)
        });
        // On the host the shim is a stub, so the status reflects a failed/absent kill rather than
        // a phone action — the point is only that Select runs the path without panicking.
        assert!(!app.status.is_empty());
    }

    #[test]
    fn the_right_softkey_exits() {
        let mut app = KillHome::new();
        testing::with_theme(Palette::DARK, |t| {
            app.handle_key(KeyEvent::new(Key::Softkey(Softkey::Right)), t, testing::SCREEN)
        });
        assert!(app.should_exit());
    }
}
