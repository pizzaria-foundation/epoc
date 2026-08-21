//! Can this handset be told to open a URL, and by which convention?
//!
//! ## Why a probe and not a line of code in the launcher
//!
//! There is no `OpenUrl` on S60. Asking a browser to open an address is done by *convention* —
//! a document name, a private command in a command-line tail, `StartDocument` — and which of them a
//! given firmware honours is not written down anywhere that applies to this phone. The launcher is
//! resident; a wrong guess there is a home screen that dies on a keypress. So the dial gets turned
//! here, in an ordinary app that can be reopened, exactly as `iconprobe` did for the icon path.
//!
//! ## What "it worked" means, and why the screen cannot tell you
//!
//! Every route reports whether **the platform accepted the launch**. None of them reports whether
//! the app then opened the URL, because AppArc has no way to say so: an app that starts and ignores
//! its command line is indistinguishable, from here, from one that honoured it. A route can
//! therefore answer `OK` and still be useless.
//!
//! That makes the instrument the handset, not this screen. The procedure is: press a route, then
//! *look at the phone*. Did the browser come up? Did it come up **on the address**, or on the home
//! page? The log records what was attempted and what the platform said; you record the rest.
//!
//! ## The journal
//!
//! Each attempt is written to `C:\Data\_logs\urlprobe.txt` **before** the call and again after it.
//! A line that has a `PEND` with no outcome is a route that took the process down — the one failure
//! mode a screen cannot show you, because the screen goes with it. Append-only: a rewrite could
//! itself be interrupted, and this is the file that has to survive.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use symbian::apps::LaunchDoc;
use symbian_ui::{chrome, App, Canvas, Handled, Key, KeyEvent, Rect, Softkey, Theme};

/// The native S60 browser's UID3 on this generation of firmware.
///
/// Hardcoded rather than discovered, because discovery is the thing under test: if a route works,
/// the launcher will need a UID to aim it at, and this is the candidate. If the app list on the
/// handset shows a different UID for "Web", the probe's own list is where you will see it — every
/// route can be aimed at whatever row the cursor is on.
const BROWSER_UID: u32 = 0x1000_8D39;

/// The address every route is asked to open.
///
/// Deliberately plain `http` and deliberately short. A URL with a query string would confound two
/// questions — whether the route works at all, and whether the tail-end route mangles a `?` — and
/// the second is only worth asking once the first has an answer.
const TARGET: &str = "http://example.com";

/// The four conventions, in the order worth trying them.
const ROUTES: [(LaunchDoc, &str); 4] = [
    (LaunchDoc::BrowserTail, "1 tail \"4 <url>\""),
    (LaunchDoc::DocumentName, "0 document name"),
    (LaunchDoc::StartDocument, "2 StartDocument at app"),
    (LaunchDoc::Resolve, "3 StartDocument resolve"),
];

/// One attempt and what came back.
struct Attempt {
    route: &'static str,
    uid: u32,
    outcome: String,
}

pub struct Urlprobe {
    /// Installed apps, so a route can be aimed somewhere other than the browser guess — the whole
    /// reason `iconprobe` is keyed by UID is that this handset has apps sharing a caption.
    apps: Vec<(u32, String)>,
    /// Which app the routes are aimed at. Starts on the browser if it is installed.
    target: usize,
    /// Which route the middle key fires.
    route: usize,
    log: Vec<Attempt>,
    note: Option<String>,
}

impl Default for Urlprobe {
    fn default() -> Self {
        Self::new()
    }
}

impl Urlprobe {
    pub fn new() -> Self {
        let mut apps: Vec<(u32, String)> = symbian::apps::installed()
            .unwrap_or_default()
            .into_iter()
            .map(|a| (a.uid3, a.caption))
            .collect();
        apps.sort_by(|a, b| a.1.cmp(&b.1));
        // Start aimed at the browser when it is there. Not an error when it is not: this handset is
        // the thing being asked, and "the UID I expected is not installed" is itself an answer.
        let target = apps.iter().position(|(uid, _)| *uid == BROWSER_UID).unwrap_or(0);
        let mut me = Self { apps, target, route: 0, log: Vec::new(), note: None };
        me.note = Some(match me.apps.len() {
            0 => "no apps listed — AppArc said nothing".to_string(),
            n => format!("{n} apps; aimed at {}", me.target_caption()),
        });
        me
    }

    fn target_uid(&self) -> u32 {
        self.apps.get(self.target).map(|(u, _)| *u).unwrap_or(BROWSER_UID)
    }

    fn target_caption(&self) -> String {
        self.apps
            .get(self.target)
            .map(|(_, c)| c.clone())
            .unwrap_or_else(|| "browser (not installed)".to_string())
    }

    /// Fire the selected route at the selected app.
    ///
    /// The two log lines around the call are the point of this function. Between them the process
    /// may simply cease to exist — that is what an unresolved import or a bad descriptor does here,
    /// with no panic dialog and no unwinding — and the `PEND` line is the only thing that would say
    /// which route did it.
    fn fire(&mut self) {
        let (route, label) = ROUTES[self.route];
        let uid = self.target_uid();
        symbian::log!("PEND route={} uid={:#010x} url={}", label, uid, TARGET);

        let outcome = match symbian::apps::launch_doc(uid, TARGET, route) {
            Ok(()) => "accepted".to_string(),
            Err(e) => format!("{e:?}"),
        };

        symbian::log!("DONE route={} uid={:#010x} -> {}", label, uid, outcome);
        self.note = Some(format!("{label}: {outcome}"));
        self.log.push(Attempt { route: label, uid, outcome });
        // Newest first: the answer you are looking for is the one you just asked for, and on a
        // 320x240 screen only about six lines fit.
        self.log.rotate_right(1);
    }
}

impl App for Urlprobe {
    fn title(&self) -> &str {
        "urlprobe"
    }

    fn handle_key(&mut self, ev: KeyEvent, _theme: &Theme<'_>, _screen: Rect) -> Handled {
        match ev.key {
            // Up/Down pick the app the routes are aimed at.
            Key::Up if self.target > 0 => {
                self.target -= 1;
                self.note = Some(format!("aimed at {}", self.target_caption()));
                Handled::Consumed
            }
            Key::Down if self.target + 1 < self.apps.len() => {
                self.target += 1;
                self.note = Some(format!("aimed at {}", self.target_caption()));
                Handled::Consumed
            }
            // Left/Right pick the route, so every combination is reachable without a menu.
            Key::Left => {
                self.route = (self.route + ROUTES.len() - 1) % ROUTES.len();
                self.note = Some(format!("route: {}", ROUTES[self.route].1));
                Handled::Consumed
            }
            Key::Right => {
                self.route = (self.route + 1) % ROUTES.len();
                self.note = Some(format!("route: {}", ROUTES[self.route].1));
                Handled::Consumed
            }
            // The D-pad centre acts, per the SDK's softkey convention.
            Key::Select => {
                self.fire();
                Handled::Consumed
            }
            Key::Softkey(Softkey::Right) => Handled::Ignored,
            _ => Handled::Ignored,
        }
    }

    fn draw(&mut self, c: &mut Canvas<'_>, theme: &Theme<'_>) {
        let screen = Rect::from_size(c.size());
        let f = chrome::Frame::split(screen, theme, true, true);
        chrome::clear(c, theme);
        chrome::title_bar(c, f.title, theme, "urlprobe", self.note.as_deref());

        let p = &theme.palette;
        let m = &theme.metrics;
        let mut y = f.content.y0 + m.pad;
        let line = theme.fonts.body.line_height() + 1;
        let put = |c: &mut Canvas<'_>, y: &mut i32, s: &str, col| {
            let r = Rect::new(f.content.x0 + m.pad, *y, f.content.x1 - m.pad, *y + line);
            c.draw_text_in(r, s, theme.fonts.body, col, symbian_ui::Align::Start);
            *y += line;
        };

        put(c, &mut y, &format!("app:   {}", self.target_caption()), p.text);
        put(c, &mut y, &format!("uid:   {:#010x}", self.target_uid()), p.dim);
        put(c, &mut y, &format!("route: {}", ROUTES[self.route].1), p.accent);
        put(c, &mut y, TARGET, p.dim);
        y += m.pad;

        if self.log.is_empty() {
            put(c, &mut y, "Select fires. Then LOOK AT THE PHONE:", p.dim);
            put(c, &mut y, "did the browser open ON the address?", p.dim);
        } else {
            for a in self.log.iter().take(5) {
                let col = if a.outcome == "accepted" { p.text } else { p.unread };
                put(c, &mut y, &format!("{} {:#06x} {}", a.route, a.uid & 0xFFFF, a.outcome), col);
            }
        }

        chrome::softkey_bar(
            c,
            f.softkeys,
            theme,
            chrome::Softkeys::new(Some("Route"), Some("Fire"), Some("Exit")),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_route_is_reachable_by_turning_the_dial() {
        // The probe is worthless if a route cannot be selected, and a modular index is exactly the
        // kind of arithmetic that quietly skips one.
        let mut p = Urlprobe { apps: Vec::new(), target: 0, route: 0, log: Vec::new(), note: None };
        let mut seen = Vec::new();
        for _ in 0..ROUTES.len() {
            seen.push(p.route);
            p.route = (p.route + 1) % ROUTES.len();
        }
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), ROUTES.len(), "the dial must reach every route");
        assert_eq!(p.route, 0, "and come back round");
    }

    #[test]
    fn the_routes_are_distinct_conventions() {
        // A copy-paste in the table would make two rows fire the same call and read as agreement
        // between two conventions that were never both tried.
        let mut codes: Vec<i32> = ROUTES.iter().map(|(r, _)| *r as i32).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), ROUTES.len());
    }

    #[test]
    fn with_no_apps_listed_it_still_names_a_target() {
        // AppArc answering nothing is a real outcome on a probe, and it must not leave the screen
        // showing an empty string where a UID belongs.
        let p = Urlprobe { apps: Vec::new(), target: 0, route: 0, log: Vec::new(), note: None };
        assert_eq!(p.target_uid(), BROWSER_UID);
        assert!(!p.target_caption().is_empty());
    }
}
