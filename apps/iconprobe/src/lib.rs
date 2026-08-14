//! A safe, isolated probe for the app-icon fetch.
//!
//! Fetching an installed app's icon (`RApaLsSession::GetAppIcon` → `CApaMaskedBitmap` →
//! `GetScanLine`) panics somewhere on the E72, and bisecting that inside the *resident* launcher
//! kept crashing the home screen. This is the safe place to find it: an ordinary, non-resident app.
//! You walk the installed-app list with Up/Down and press Select to fetch the icon for the app
//! under the cursor. A success shows its size and draws it; a failure shows the error. If the fetch
//! *panics* the app simply closes — reopen it, and the last line in `C:\Data\logs_iconprobe.txt`
//! (written before every fetch) names the app that did it. So the culprit UID is found by which app
//! makes the probe vanish, with no risk to anything else — the exact opposite of bisecting in the
//! home.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use symbian::apps::{AppInfo, Icon};
use symbian_ui::gfx::Size;
use symbian_ui::{chrome, App, Canvas, Handled, Key, KeyEvent, Point, Rect, Softkey, Theme};

/// The size we ask GetAppIcon to scale to — the same value the launcher uses, so the probe
/// exercises the identical path.
const ICON_PX: u16 = 44;

pub struct Iconprobe {
    roster: Vec<AppInfo>,
    sel: usize,
    status: String,
    /// The last fetched icon, drawn when present.
    icon: Option<Icon>,
    exit: bool,
}

impl Iconprobe {
    pub fn new() -> Self {
        let roster = symbian::apps::installed().unwrap_or_default();
        symbian::log!("[probe] open roster={}", roster.len());
        Self {
            roster,
            sel: 0,
            status: String::from("Up/Down: pick app. Select: fetch icon."),
            icon: None,
            exit: false,
        }
    }

    fn move_sel(&mut self, delta: isize) {
        if self.roster.is_empty() {
            return;
        }
        let last = self.roster.len() - 1;
        let target = (self.sel as isize + delta).clamp(0, last as isize) as usize;
        if target != self.sel {
            self.sel = target;
            self.icon = None;
            self.status = String::from("Select: fetch icon.");
        }
    }

    /// Fetch the selected app's icon, logging *before* the call so a panic leaves a trail. `variant_b`
    /// picks the shim's TInt-overload path (green fill) instead of the default TSize one, to compare
    /// which overload survives on MIF-icon apps. On the host the shim is a stub, so this reports an
    /// error rather than an icon.
    fn fetch(&mut self, variant_b: bool) {
        self.icon = None;
        let Some(app) = self.roster.get(self.sel) else {
            self.status = String::from("No apps.");
            return;
        };
        let uid = app.uid3;
        let tag = if variant_b { "B" } else { "A" };
        // Last line before a possible panic: it names the app and method that crashed the probe.
        symbian::log!("[probe] fetch{tag} uid={uid:08x} cap='{}'", app.caption);
        let result = if variant_b {
            symbian::apps::icon_b(uid, ICON_PX)
        } else {
            symbian::apps::icon(uid, ICON_PX)
        };
        match result {
            Ok(icon) => {
                symbian::log!("[probe] ok{tag} uid={uid:08x} {}x{}", icon.w, icon.h);
                self.status = format!("OK{tag} {}x{}", icon.w, icon.h);
                self.icon = Some(icon);
            }
            Err(e) => {
                symbian::log!("[probe] err{tag} uid={uid:08x} {e:?}");
                self.status = format!("Err{tag} {e:?}");
            }
        }
    }
}

impl Default for Iconprobe {
    fn default() -> Self {
        Self::new()
    }
}

impl App for Iconprobe {
    fn title(&self) -> &str {
        "Icon Probe"
    }

    fn handle_key(&mut self, ev: KeyEvent, _theme: &Theme<'_>, _screen: Rect) -> Handled {
        match ev.key {
            Key::Up => {
                self.move_sel(-1);
                Handled::Consumed
            }
            Key::Down => {
                self.move_sel(1);
                Handled::Consumed
            }
            Key::Select => {
                self.fetch(false); // method A: GetAppIcon(TSize)
                Handled::Consumed
            }
            Key::Softkey(Softkey::Left) => {
                self.fetch(true); // method B: GetAppIcon(TInt), green fill
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
        chrome::title_bar(c, frame.title, theme, "Icon Probe", None);
        chrome::softkey_bar(c, frame.softkeys, theme, [Some("MethodB"), Some("FetchA"), Some("Exit")]);

        let body = theme.fonts.body;
        let p = &theme.palette;
        let x = frame.content.x0 + 6;
        let mut y = frame.content.y0 + 4;

        // Which app is under the cursor.
        let header = match self.roster.get(self.sel) {
            Some(app) => format!("{}/{}  {:08X}", self.sel + 1, self.roster.len(), app.uid3),
            None => String::from("no apps"),
        };
        c.draw_text(Point::new(x, y + body.ascent()), &header, body, p.dim);
        y += body.line_height();

        if let Some(app) = self.roster.get(self.sel) {
            c.draw_text(Point::new(x, y + body.ascent()), &app.caption, theme.fonts.strong, p.text);
        }
        y += theme.fonts.strong.line_height() + 4;

        // The fetched icon, if any, drawn in a fixed box so its real size is visible.
        if let Some(icon) = &self.icon {
            let box_rect = Rect::from_xywh(x, y, ICON_PX as i32, ICON_PX as i32);
            c.stroke_rect(box_rect, p.dim);
            c.blit_icon(box_rect, &icon.pixels, &icon.mask, Size::new(icon.w, icon.h), icon.w as usize);
            y += ICON_PX as i32 + 4;
        }

        c.draw_text(Point::new(x, y + body.ascent()), &self.status, body, p.text);
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
    fn constructs_and_draws() {
        let mut app = Iconprobe::new();
        let (_, px) = testing::with_canvas(symbian_ui::gfx::Size::new(320, 240), |c| {
            testing::with_theme(Palette::DARK, |t| app.draw(c, t));
        });
        assert!(px.iter().any(|&p| p != 0));
    }

    #[test]
    fn fetch_and_move_do_not_panic_on_host() {
        // The host shim is a stub, so the roster is empty and fetch reports "no apps" — the point
        // is only that the key paths run without panicking.
        let mut app = Iconprobe::new();
        testing::with_theme(Palette::DARK, |t| {
            app.handle_key(KeyEvent::new(Key::Down), t, testing::SCREEN);
            app.handle_key(KeyEvent::new(Key::Select), t, testing::SCREEN);
        });
        assert!(!app.status.is_empty());
    }

    #[test]
    fn right_softkey_exits() {
        let mut app = Iconprobe::new();
        testing::with_theme(Palette::DARK, |t| {
            app.handle_key(KeyEvent::new(Key::Softkey(Softkey::Right)), t, testing::SCREEN)
        });
        assert!(app.should_exit());
    }
}
