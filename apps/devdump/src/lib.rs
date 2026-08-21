//! One install, one report: everything this handset will tell us, in a single trip.
//!
//! # Why it is shaped like this
//!
//! Testing on the phone costs a build, a transfer, an install, an open and a reading.
//! `docs/device-notes.md` counts the bill: six rounds to get a socket open, six for the
//! image decoder, three for the keyboard. `examples/selftest` was the first answer to that
//! — run forty questions unattended and leave a report — and it works, as long as every
//! question can be asked from one binary.
//!
//! It cannot. Linking an import library whose ordinals the handset does not export makes
//! the E32 loader refuse the whole image: no panic, no log, and no report file at all. One
//! unlucky import in a monolithic dump would cost every answer in it. So this is a
//! *launcher plus a fleet*: each risky subject rides its own executable, and a probe whose
//! image will not load costs its own section and nothing else.
//!
//! The launcher writes a manifest naming every probe **before** launching any of them, so
//! that an image which vanishes leaves a recorded absence rather than silence.
//!
//! # The shape on disk
//!
//! ```text
//! C:\Data\dump\00-launcher.txt   the manifest, and what happened to each probe
//!              10-system.txt     …one section per probe…
//!              99-merged.txt     all of the above concatenated, best-effort
//! ```
//!
//! # Getting it back
//!
//! `epoc sh --pull "C:\Data\dump\99-merged.txt" .` over the phone's remote shell, or the
//! whole `C:\Data\dump\` directory off over USB.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;

use symbian::fs::ShimFs;
use symbian::process::ShimProcs;
use symbian_sys;
use symbian_ui::{chrome, App, Canvas, Handled, Key, KeyEvent, Point, Rect, Softkey, Theme};

pub mod dlls;
pub mod launcher;
pub mod probes;
pub mod registry;

pub use launcher::{Launcher, Outcome, Phase, TICK_MS};

/// The launcher's screen.
///
/// Deliberately thin. It draws what the state machine already knows and owns no logic of
/// its own — the machine is in [`launcher`], where it is covered by host tests against
/// probes that refuse to load, crash and hang on demand. A screen that computed anything
/// would be computing it somewhere no test can reach.
pub struct DevDump {
    inner: Launcher,
    fs: ShimFs,
    procs: ShimProcs,
    exit: bool,
    started: bool,
    /// The repeating timer that advances the launcher.
    ///
    /// Load-bearing, and its absence was the bug that made the first two device runs look
    /// like a hung bridge. `symbian_ui::App` has no tick method: on the device Avkon owns
    /// the loop and nothing calls into an app except through an event, so a state machine
    /// that has to advance on its own must arm a timer and step from the event. Without
    /// this the launcher sat in `Phase::Start` forever, drew "running: starting", and
    /// looked exactly like a network problem.
    ticker: Option<i32>,
}

impl DevDump {
    pub fn new() -> Self {
        DevDump {
            inner: Launcher::new(),
            fs: ShimFs,
            procs: ShimProcs,
            exit: false,
            started: false,
            ticker: None,
        }
    }

    /// Advance the run. The device host calls this from its timer every [`TICK_MS`].
    pub fn tick(&mut self) {
        if !self.started || self.inner.is_done() {
            return;
        }
        self.inner.tick(&mut self.fs, &mut self.procs);
    }

    pub fn is_done(&self) -> bool {
        self.inner.is_done()
    }

    /// Called once, the moment the last probe's outcome is recorded.
    ///
    /// # Why the bridge starts here and not at the beginning
    ///
    /// Because it was asked to be the last test, and because that is the right answer. The
    /// connection ladder ends in Symbian's access-point dialog, which the shim raises after
    /// sending the application to the background — so during a run it competes for the
    /// foreground with the one screen that says which probe is executing, and an
    /// unanswered dialog leaves the launcher looking hung when it is not.
    ///
    /// Run the fleet first, flush every section, *then* go looking for a network. By the
    /// time a dialog can appear, the report is already complete on disk: the worst case is
    /// a dump that has to come off over USB, which is the case we would have had anyway.
    /// The bridge only ever buys a cheaper way to fetch a file that already exists.
    fn finish_run(&mut self) {
        // Stop the ticker: the machine is done and a timer nobody reads is a wake-up the
        // phone pays for.
        if let Some(h) = self.ticker.take() {
            symbian::timer_cancel(h);
        }
    }
}

impl Default for DevDump {
    fn default() -> Self {
        Self::new()
    }
}

impl App for DevDump {
    fn title(&self) -> &str {
        "devdump"
    }

    fn handle_key(&mut self, ev: KeyEvent, _theme: &Theme<'_>, _screen: Rect) -> Handled {
        match ev.key {
            // One key starts the run. Not automatic on launch: a run takes minutes and
            // kills processes, and an app that began doing that the instant it was opened
            // would be impossible to inspect before it committed.
            Key::Select if !self.started => {
                self.started = true;
                // Everything the run does happens off this timer. See the `ticker` field.
                self.ticker = symbian::timer_every(TICK_MS).ok();
                // The bridge is deliberately NOT started here — see `connect_bridge_when_done`.
                Handled::Consumed
            }
            // Mid-run there is deliberately no way out: leaving would orphan the probe the
            // launcher is waiting on and freeze the report with that probe still marked
            // pending — an outcome indistinguishable from the launcher having crashed.
            Key::Softkey(Softkey::Right) if self.inner.is_done() || !self.started => {
                self.exit = true;
                Handled::Consumed
            }
            _ => Handled::Ignored,
        }
    }

    /// Every platform event, before translation.
    ///
    /// The bridge's sockets are its own and independent of anything the launcher does, so
    /// this forwards unconditionally and ignores the result for key translation — a
    /// diagnostic channel must not be able to swallow a keypress.
    fn handle_raw(&mut self, ev: &symbian_ui::RawEvent) -> Handled {
        // The tick. Matched on our own handle, so another timer in the process cannot drive
        // the state machine and ours cannot be mistaken for one.
        if ev.kind == symbian_sys::SHIM_EV_TIMER && Some(ev.handle) == self.ticker {
            let was_done = self.inner.is_done();
            self.tick();
            if !was_done && self.inner.is_done() {
                self.finish_run();
            }
            // Consumed: it repaints, and it must not fall through to key translation.
            return Handled::Consumed;
        }
        Handled::Ignored
    }

    fn should_exit(&self) -> bool {
        self.exit
    }

    fn draw(&mut self, c: &mut Canvas<'_>, theme: &Theme<'_>) {
        let screen = Rect::from_size(c.size());
        let frame = chrome::Frame::split(screen, theme, true, true);
        chrome::clear(c, theme);
        chrome::title_bar(c, frame.title, theme, "device dump", None);
        chrome::softkey_bar(
            c,
            frame.softkeys,
            theme,
            [
                Some(if self.started { "" } else { "Run" }),
                None,
                Some(if self.started && !self.inner.is_done() { "" } else { "Exit" }),
            ],
        );

        let body = theme.fonts.body;
        let small = theme.fonts.small;
        let mut y = frame.content.y0 + 4;

        if !self.started {
            for text in [
                "Press Select to run.",
                "",
                "Runs every probe in turn and",
                "writes one report each to",
                "C:\\Data\\dump\\.",
                "",
                "A probe that will not load is",
                "recorded, not skipped.",
            ] {
                c.draw_text(Point::new(6, y + body.ascent()), text, body, theme.palette.text);
                y += body.line_height();
            }
            return;
        }

        let mut head = String::from("running: ");
        head.push_str(&self.inner.status());
        // A run whose ticker never armed cannot advance, and that has to say so on the
        // screen rather than presenting as a phase that is taking a long time. The first
        // two device runs were exactly this, and were read as a hung bridge.
        if self.ticker.is_none() && !self.inner.is_done() {
            head.push_str("  NO TICKER");
        }
        c.draw_text(Point::new(6, y + body.ascent()), &head, body, theme.palette.accent);
        y += body.line_height() + 4;

        // One row per probe, so a run that stalls shows which probe it stalled on and what
        // every earlier one answered — without waiting for the report to come off the
        // phone. On a platform whose usual failure is the app simply closing, what is on
        // screen when it closes is a diagnosis.
        for (pr, outcome) in registry::PROBES.iter().zip(self.inner.outcomes()) {
            if y + small.line_height() > frame.content.y1 {
                break;
            }
            let colour = if outcome.is_notable() {
                theme.palette.unread
            } else {
                theme.palette.dim
            };
            let mut row = String::from(pr.name);
            row.push_str("  ");
            row.push_str(outcome.label());
            c.draw_text(Point::new(6, y + small.ascent()), &row, small, colour);
            y += small.line_height();
        }

        if self.inner.is_done() && y + small.line_height() <= frame.content.y1 {
            let label = self.inner.output_label();
            // An empty label means every rung of the output ladder refused. That has to be
            // said loudly: the run happened and went nowhere.
            let (text, colour) = if label.is_empty() {
                ("NOWHERE WRITABLE", theme.palette.unread)
            } else {
                (label, theme.palette.text)
            };
            c.draw_text(Point::new(6, y + small.ascent()), text, small, colour);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian_ui::{testing, Palette};

    fn key(app: &mut DevDump, k: Key) -> Handled {
        testing::with_theme(Palette::DARK, |t| app.handle_key(KeyEvent::new(k), t, testing::SCREEN))
    }

    #[test]
    fn it_waits_to_be_told_to_start() {
        // A run kills processes and takes minutes; beginning that the instant the app is
        // opened would take the decision away from whoever opened it.
        let app = DevDump::new();
        assert!(!app.started);
        assert_eq!(app.inner.phase(), Phase::Start);
    }

    #[test]
    fn select_starts_the_run() {
        let mut app = DevDump::new();
        assert_eq!(key(&mut app, Key::Select), Handled::Consumed);
        assert!(app.started);
    }

    #[test]
    fn the_right_softkey_exits_before_a_run_and_after_one() {
        let mut app = DevDump::new();
        key(&mut app, Key::Softkey(Softkey::Right));
        assert!(app.should_exit());
    }

    /// Mid-run there is no exit, because leaving would orphan the probe the launcher is
    /// waiting on and freeze the report with it still marked pending.
    #[test]
    fn there_is_no_exit_while_probes_are_running() {
        let mut app = DevDump::new();
        app.started = true;
        assert_eq!(key(&mut app, Key::Softkey(Softkey::Right)), Handled::Ignored);
        assert!(!app.should_exit());
    }

    #[test]
    fn it_draws_something_before_and_during_a_run() {
        for started in [false, true] {
            let mut app = DevDump::new();
            app.started = started;
            let (_, px) = testing::with_canvas(symbian_gfx::Size::new(320, 240), |c| {
                testing::with_theme(Palette::DARK, |t| app.draw(c, t));
            });
            assert!(px.iter().any(|&p| p != 0), "nothing drawn with started={started}");
        }
    }

    /// The regression test for the bug that cost two device trips.
    ///
    /// `symbian_ui::App` has no tick method, so a state machine that must advance on its
    /// own has to arm a timer and step from the event. Nothing did, so the launcher sat in
    /// `Phase::Start` forever, drew "running: starting", and read on the phone as a hung
    /// network — the one part of the screen that mentions a mechanism at all.
    ///
    /// This asserts the wiring, not the effect: that Select arms a ticker, and that a timer
    /// event carrying that handle drives the machine. It cannot assert what the machine then
    /// does, because on the host every shim call behind it is a stub — that half is covered
    /// by `launcher::tests` against fakes.
    #[test]
    fn select_arms_a_ticker_and_the_timer_event_drives_the_run() {
        let mut app = DevDump::new();
        assert!(app.ticker.is_none(), "a ticker before Select would start the run by itself");
        key(&mut app, Key::Select);

        // On the host `timer_every` is a stub, so the handle is None — which is exactly why
        // the assertion below is about the *match*, not about a value. Pretend the shim
        // handed us one.
        app.ticker = Some(7);

        let mut ev = symbian_ui::RawEvent::default();
        ev.kind = symbian_sys::SHIM_EV_TIMER;
        ev.handle = 7;
        assert_eq!(app.handle_raw(&ev), Handled::Consumed, "the tick must repaint");

        // A timer belonging to something else must not drive the machine: another timer in
        // the process would otherwise advance the run at its own rate.
        let mut other = symbian_ui::RawEvent::default();
        other.kind = symbian_sys::SHIM_EV_TIMER;
        other.handle = 99;
        assert_eq!(app.handle_raw(&other), Handled::Ignored);
    }

    /// The run is driven by a timer, not by the draw. A tick before Select must do nothing,
    /// or opening the app would start the run by accident.
    #[test]
    fn ticking_before_start_does_nothing() {
        let mut app = DevDump::new();
        app.tick();
        assert_eq!(app.inner.phase(), Phase::Start);
    }
}
