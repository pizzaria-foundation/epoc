//! A safe, isolated probe for the app-icon fetch.
//!
//! ## Why this remembers things instead of catching them
//!
//! `TRAP` catches a *Leave*, and every Leave here is already caught and shown on screen as `ErrA`
//! and friends. What closes the probe is a **panic**, and no amount of trapping reaches one: a
//! panic kills the thread outright. So the probe cannot be made not to die. It can be made not to
//! *forget*.
//!
//! Before each fetch it appends `PEND` for that app and method to a journal, and after the call it
//! appends the outcome. A line that is still `PEND` when the probe next starts is an app that took
//! the process down — recorded on the next launch as `CRASH`, durably, without anyone having to
//! watch the screen at the moment it happened. Append-only on purpose: a rewrite could itself be
//! interrupted, and the one file that has to survive a crash is this one.
//!
//! The journal is keyed by UID, not caption, which is the other half of the answer. This handset
//! has several apps sharing a name and differing only in UID, and they do not behave alike — one
//! crashes the fetch and its namesake does not. A results table keyed by name would average them
//! into nonsense.
//!
//! Fetching an installed app's icon (`RApaLsSession::GetAppIcon` → `CApaMaskedBitmap` →
//! `GetScanLine`) panics somewhere on the E72, and bisecting that inside the *resident* launcher
//! kept crashing the home screen. This is the safe place to find it: an ordinary, non-resident app.
//! You walk the installed-app list with Up/Down and press Select to fetch the icon for the app
//! under the cursor. A success shows its size and draws it; a failure shows the error. If the fetch
//! *panics* the app simply closes — reopen it, and the last line in `C:\Data\_logs\iconprobe.txt`
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

/// Candidate indices for the colour plane inside an app's icon file, for method C. MBM files index
/// their bitmaps from 0; mifconv-generated MIF files are conventionally offset, and 16384 is the
/// offset the S60 headers use. Which one a given app needs is exactly what this probe is here to
/// answer, so both are on the dial rather than one being assumed.
const BITMAP_IDS: [i32; 2] = [0, 16384];

/// Which fetch to run. They differ only in how the platform is asked for the pixels.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Method {
    /// `GetAppIcon(TSize)` into a `CApaMaskedBitmap` — the original path.
    A,
    /// `GetAppIcon(TInt)`, colour filled green — isolates the overload from the scaling.
    B,
    /// The app's icon file through `AknIconUtils` — MIF-capable, and the only one with a real mask.
    C,
}

impl Method {
    /// The suffix that tags this method in the log and on screen.
    fn tag(self) -> &'static str {
        match self {
            Method::A => "A",
            Method::B => "B",
            Method::C => "C",
        }
    }

    fn from_tag(s: &str) -> Option<Self> {
        match s {
            "A" => Some(Method::A),
            "B" => Some(Method::B),
            "C" => Some(Method::C),
            _ => None,
        }
    }
}

/// What happened last time a method was tried on an app.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// Fetched, at this size.
    Ok(i32, i32),
    /// The platform refused, cleanly.
    Err,
    /// The probe never came back — a panic. Inferred on the next start from a dangling `PEND`.
    Crash,
}

impl Outcome {
    fn tag(self) -> &'static str {
        match self {
            Outcome::Ok(..) => "ok",
            Outcome::Err => "err",
            Outcome::Crash => "CRASH",
        }
    }
}

/// The journal file, in `C:\Data` so it outlives an uninstall and can be pulled off the phone.
const JOURNAL: &str = "C:\\Data\\iconprobe.log";
/// Ceiling for the journal. 191 apps times three methods times two lines is far under this; the cap
/// exists so a stuck loop cannot fill the disk.
const JOURNAL_CAP: u64 = 128 * 1024;

pub struct Iconprobe {
    roster: Vec<AppInfo>,
    sel: usize,
    status: String,
    /// The last fetched icon, drawn when present.
    icon: Option<Icon>,
    /// Index into [`BITMAP_IDS`] — which candidate method C passes next.
    id_sel: usize,
    /// What each (app, method) did last time, keyed by UID because captions repeat on this handset.
    results: Vec<(u32, Method, Outcome)>,
    /// The file the selected app's icon comes from, refreshed on every move.
    icon_file: String,
    fs: symbian::fs::ShimFs,
    exit: bool,
}

impl Iconprobe {
    pub fn new() -> Self {
        let roster = symbian::apps::installed().unwrap_or_default();
        symbian::log!("[probe] open roster={}", roster.len());
        let mut me = Self {
            roster,
            sel: 0,
            status: String::from("Up/Down: pick app. Select: fetch icon."),
            icon: None,
            id_sel: 0,
            results: Vec::new(),
            icon_file: String::new(),
            fs: symbian::fs::ShimFs,
            exit: false,
        };
        let crashed = me.replay_journal();
        me.status = if crashed > 0 {
            // The headline on reopening: something died last time, and this says what.
            format!("{crashed} crash(es) recorded. Up/Down to review.")
        } else {
            String::from("Up/Down: pick app. Select: fetch icon.")
        };
        // Land on the first app with no result yet, so reopening after a crash resumes the sweep
        // instead of starting over at the top.
        me.sel = me.first_untried();
        me.refresh_icon_file();
        me
    }

    /// Read the journal, fold it into [`Self::results`], and turn every dangling `PEND` into a
    /// recorded crash — durably, by appending the verdict so the next start does not have to infer
    /// it again. Returns how many crashes were newly discovered.
    fn replay_journal(&mut self) -> usize {
        let Ok(path) = symbian::fs::Utf16Path::new(JOURNAL) else {
            return 0;
        };
        let Ok(Some(bytes)) = symbian::fs::read(&mut self.fs, &path) else {
            return 0;
        };
        // Anything still pending at the end of the replay is an attempt that never returned.
        let mut pending: Vec<(u32, Method)> = Vec::new();
        for line in bytes.split(|&b| b == b'\n') {
            let Ok(text) = core::str::from_utf8(line) else { continue };
            let mut parts = text.split_whitespace();
            let (Some(kind), Some(tag), Some(uid_hex)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let (Some(method), Ok(uid)) = (Method::from_tag(tag), u32::from_str_radix(uid_hex, 16))
            else {
                continue;
            };
            match kind {
                "P" => {
                    if !pending.contains(&(uid, method)) {
                        pending.push((uid, method));
                    }
                }
                "O" | "E" | "C" => {
                    pending.retain(|e| *e != (uid, method));
                    let outcome = match kind {
                        "O" => parse_size(parts.next()).map(|(w, h)| Outcome::Ok(w, h)).unwrap_or(Outcome::Err),
                        "E" => Outcome::Err,
                        _ => Outcome::Crash,
                    };
                    self.record(uid, method, outcome);
                }
                _ => {}
            }
        }
        for (uid, method) in pending.clone() {
            self.record(uid, method, Outcome::Crash);
            self.append(&format!("C {} {uid:08X}\n", method.tag()));
            symbian::log!("[probe] crash inferred uid={uid:08x} method={}", method.tag());
        }
        pending.len()
    }

    /// Remember one outcome, replacing any earlier one for the same app and method.
    fn record(&mut self, uid: u32, method: Method, outcome: Outcome) {
        match self.results.iter_mut().find(|(u, m, _)| *u == uid && *m == method) {
            Some(slot) => slot.2 = outcome,
            None => self.results.push((uid, method, outcome)),
        }
    }

    fn outcome_of(&self, uid: u32, method: Method) -> Option<Outcome> {
        self.results.iter().find(|(u, m, _)| *u == uid && *m == method).map(|(_, _, o)| *o)
    }

    /// Append a journal line. Failure is ignored on purpose: losing the journal must never be a
    /// reason for the probe itself to stop working.
    fn append(&mut self, line: &str) {
        if let Ok(path) = symbian::fs::Utf16Path::new(JOURNAL) {
            let _ = symbian::fs::append_capped(&mut self.fs, &path, line.as_bytes(), JOURNAL_CAP);
        }
    }

    /// The first app nothing has been tried on yet, or 0 when every app has a result.
    fn first_untried(&self) -> usize {
        self.roster
            .iter()
            .position(|a| !self.results.iter().any(|(u, _, _)| *u == a.uid3))
            .unwrap_or(0)
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
            self.refresh_icon_file();
        }
    }

    /// Ask which file the selected app's icon lives in. Read on navigation rather than on fetch
    /// because it is the one question worth having answered *before* deciding what to try — and it
    /// is a plain registry lookup that touches no bitmap, so it is safe to run on every move.
    fn refresh_icon_file(&mut self) {
        let Some(uid) = self.roster.get(self.sel).map(|a| a.uid3) else {
            self.icon_file = String::new();
            return;
        };
        self.icon_file = match symbian::apps::icon_file(uid) {
            Ok(path) => path,
            Err(e) => format!("(no file: {e:?})"),
        };
    }

    /// The bitmap index method C will pass next.
    fn bitmap_id(&self) -> i32 {
        BITMAP_IDS[self.id_sel % BITMAP_IDS.len()]
    }

    /// Fetch the selected app's icon by one of the three methods, logging *before* the call so a
    /// panic leaves a trail. On the host the shim is a stub, so this reports an error rather than an
    /// icon.
    fn fetch(&mut self, method: Method) {
        self.icon = None;
        // Copied out before anything else: the journal write below needs `self` mutably, and the
        // roster entry is only wanted for these two fields.
        let Some((uid, caption)) = self.roster.get(self.sel).map(|a| (a.uid3, a.caption.clone()))
        else {
            self.status = String::from("No apps.");
            return;
        };
        let tag = method.tag();
        let id = self.bitmap_id();
        // Written BEFORE the call, and flushed, because the call may not return. If the probe dies
        // here, this line is the entire evidence — the next start turns it into a CRASH verdict.
        // The icon's source file, recorded alongside the attempt: when a fetch draws the wrong
        // picture, this is what tells the two possible causes apart afterwards.
        let file = self.icon_file.clone();
        self.append(&format!("F {tag} {uid:08X} {file}\n"));
        self.append(&format!("P {tag} {uid:08X}\n"));
        symbian::log!("[probe] fetch{tag} uid={uid:08x} id={id} cap='{caption}'");
        let result = match method {
            Method::A => symbian::apps::icon(uid, ICON_PX),
            Method::B => symbian::apps::icon_b(uid, ICON_PX),
            Method::C => symbian::apps::icon_c(uid, ICON_PX, id),
        };
        match result {
            Ok(icon) => {
                // Whether the mask is real is the whole point of method C, and it is invisible in a
                // size. A mask that is 255 everywhere is the opaque-rectangle fallback; anything
                // else means the platform gave us genuine coverage and icons will draw cut out.
                let transparent = icon.mask.iter().any(|&m| m != 255);
                symbian::log!(
                    "[probe] ok{tag} uid={uid:08x} {}x{} mask={}",
                    icon.w,
                    icon.h,
                    if transparent { "real" } else { "opaque" }
                );
                self.status = format!(
                    "OK{tag} {}x{} mask {}",
                    icon.w,
                    icon.h,
                    if transparent { "real" } else { "opaque" }
                );
                self.append(&format!("O {tag} {uid:08X} {}x{}\n", icon.w, icon.h));
                self.record(uid, method, Outcome::Ok(icon.w, icon.h));
                self.icon = Some(icon);
            }
            Err(e) => {
                symbian::log!("[probe] err{tag} uid={uid:08x} {e:?}");
                self.status = format!("Err{tag} {e:?}");
                self.append(&format!("E {tag} {uid:08X}\n"));
                self.record(uid, method, Outcome::Err);
            }
        }
    }

    /// Run method C over every app with no recorded outcome yet, stopping when the roster is
    /// exhausted — or when one of them takes the process down, which is itself the finding.
    fn sweep(&mut self) {
        let mut done = 0usize;
        loop {
            let next = self
                .roster
                .iter()
                .position(|a| self.outcome_of(a.uid3, Method::C).is_none());
            let Some(i) = next else { break };
            self.sel = i;
            self.refresh_icon_file();
            self.fetch(Method::C);
            done += 1;
        }
        self.status = format!("Sweep done: {done} app(s), none left untried.");
    }

    /// One line summarising what each method has done to the selected app, e.g. `A:ok 24x24  C:CRASH`.
    /// Blank when nothing has been tried yet.
    fn history_line(&self) -> String {
        let Some(app) = self.roster.get(self.sel) else {
            return String::new();
        };
        let mut parts: Vec<String> = Vec::new();
        for method in [Method::A, Method::B, Method::C] {
            if let Some(o) = self.outcome_of(app.uid3, method) {
                parts.push(match o {
                    Outcome::Ok(w, h) => format!("{}:ok {w}x{h}", method.tag()),
                    _ => format!("{}:{}", method.tag(), o.tag()),
                });
            }
        }
        parts.join("  ")
    }
}

/// Parse a `WxH` field from the journal.
fn parse_size(field: Option<&str>) -> Option<(i32, i32)> {
    let (w, h) = field?.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
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
                self.fetch(Method::A); // GetAppIcon(TSize)
                Handled::Consumed
            }
            Key::Softkey(Softkey::Left) => {
                self.fetch(Method::B); // GetAppIcon(TInt), green fill
                Handled::Consumed
            }
            Key::Right => {
                self.fetch(Method::C); // AknIconUtils, at the currently dialled bitmap id
                Handled::Consumed
            }
            Key::Left => {
                // Cycle which bitmap index method C passes. Cheap and non-destructive, so it is
                // safe to dial before a fetch rather than rebuilding the probe per candidate.
                self.id_sel = (self.id_sel + 1) % BITMAP_IDS.len();
                self.status = format!("Method C id={}", self.bitmap_id());
                Handled::Consumed
            }
            // Sweep: run method C over every app that has no result yet, and keep going.
            //
            // This is how a hundred-odd apps get characterised without a hundred-odd key presses.
            // It is safe to run straight into a panic: each attempt is journalled before the call,
            // so the app that kills the probe is recorded, and reopening resumes at the first app
            // with no result — which skips the one that just died, because "died" is now a result.
            // Press it a few times and the list of bad UIDs converges on its own.
            Key::Char('s') | Key::Char('S') => {
                self.sweep();
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
        chrome::softkey_bar(c, frame.softkeys, theme, chrome::Softkeys::new(Some("MethodB"), Some("FetchA"), Some("Exit")));

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

        // Method C is on the D-pad rather than a softkey, so the screen has to say so.
        let hint = format!("Right: C (id {})  Left: id  S: sweep", self.bitmap_id());
        c.draw_text(Point::new(x, y + body.ascent()), &hint, body, p.dim);
        y += body.line_height();

        // What this app did before, including in a run that ended in a panic. This is the line that
        // makes same-named apps tellable apart: it is keyed by the UID in the header above.
        let history = self.history_line();
        if !history.is_empty() {
            c.draw_text(Point::new(x, y + body.ascent()), &history, body, p.accent);
        }
        y += body.line_height();

        if let Some(app) = self.roster.get(self.sel) {
            c.draw_text(Point::new(x, y + body.ascent()), &app.caption, theme.fonts.strong, p.text);
        }
        y += theme.fonts.strong.line_height();

        // Where the icon actually comes from. Only the file name — the directory is always
        // \resource\apps\ and the screen is 320 px wide, so the half that varies is the half shown.
        // The extension is the point: .mbm is a plain bitmap, .mif is scalable.
        if !self.icon_file.is_empty() {
            let tail = self.icon_file.rsplit('\\').next().unwrap_or(&self.icon_file);
            c.draw_text(Point::new(x, y + body.ascent()), tail, body, p.dim);
        }
        y += body.line_height() + 4;

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
