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
//! `epoc db pull "C:\Data\dump\99-merged.txt" ./dump.txt`, if the dev bridge came up.
//! Otherwise USB or Bluetooth — and "the bridge did not come up" is itself the run's first
//! finding, since it has never been confirmed against a real handset
//! (`docs/spec-epocadb.md`).

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

/// Set by the `CTL rerun` verb, read by [`DevDump::tick`].
///
/// A static rather than a field because the bridge's control handler is a plain `fn` that
/// runs from inside the socket poll — it has no path to the app's data, and giving it one
/// would mean the app could be mutated halfway through a frame. A flag it can set and the
/// tick can clear is the whole of the coupling.
static mut RERUN_REQUESTED: bool = false;

/// The `CTL` verbs this app answers. Registered with the dev bridge at startup.
///
/// # Why this exists
///
/// A run costs an install, an open and a wait. Most of what makes a second run necessary is
/// not a code change — it is wanting the numbers again after moving the phone, inserting a
/// card, or turning something on. `CTL rerun` turns "build, transfer, install, open, wait,
/// pull" into "rerun, pull", and it is the first `CTL` verb defined anywhere in this
/// repository: until now `symbian-app` answered every one with `ERR no control verbs`.
///
/// It deliberately does **not** take a probe name. Running one probe out of sequence would
/// leave the other sections on disk from the previous run, with nothing in the merged file
/// saying which lines came from when — a report whose parts are from different moments,
/// presented as one observation.
pub fn control(line: &str) -> Option<alloc::string::String> {
    match line.trim() {
        "rerun" => {
            // SAFETY: single-threaded; set from the bridge poll, cleared in `tick`, both on
            // the app thread.
            unsafe { RERUN_REQUESTED = true };
            Some(alloc::string::String::from("OK rerun queued"))
        }
        "status" => Some(alloc::string::String::from("OK devdump")),
        _ => None,
    }
}

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
    #[cfg(feature = "dev-bridge")]
    bridge: bridge::Bridge,
}

/// The dev bridge: the connection ladder that actually gets online on this handset.
///
/// # The technique, and why not a shortcut
///
/// `docs/device-notes.md` records three device runs in which every other program on the
/// phone reached the network and this SDK's did not. The cause was a strategy chosen to
/// avoid the access-point dialog: open a socket with no `RConnection` at all, on the
/// reasoning that the stack would use whatever route existed. It had no dialog, no
/// negotiation and nothing that could time out — and it also could not find or create a
/// route, so it reported success unconditionally while three phases timed out beneath it.
/// **The absence of a mechanism is not a mechanism. It cannot fail, which reads as
/// working.**
///
/// So this uses the full ladder, which is the one confirmed on hardware and the one the
/// Telegram client uses:
///
/// 1. **`RConnection::Attach`** to a route that is already up — synchronous, no dialog,
///    `KErrNotFound` when there is nothing to join.
/// 2. **A saved IAP**, if a previous run recorded one. `ECommDbDialogPrefDoNotPrompt`, so
///    this is silent too.
/// 3. **The dialog**, once. Every other program on the handset offers it; refusing to is
///    what left this SDK offline for three rounds.
///
/// and then **persists whatever the OS settled on**, which is what makes every run after
/// the first silent. That last step is the difference between a ladder that works once and
/// one that stops being noticed.
///
/// # Why the dialog is safe here
///
/// It does not stall the probe run. `net_start` is asynchronous and the launcher's tick
/// loop is independent of it: the bearer progresses through events while probes are being
/// launched and polled. Nothing on the report's critical path waits for the network, so the
/// worst case of a dialog nobody answers is a run with no bridge — exactly the run we would
/// have had anyway.
///
/// The shim also sends the app to the background before prompting, because on S60 3rd
/// Edition the CommsDat dialog otherwise opens *behind* the application window. That is
/// Nokia's own fix, and it means the launcher briefly loses foreground mid-run.
#[cfg(feature = "dev-bridge")]
mod bridge {
    use symbian::fs::{self, ShimFs, Utf16Path};
    use symbian::net::{Bearer, RawEvent, ShimNet};

    /// Where the chosen access point is remembered between runs.
    ///
    /// The app's own private directory: it needs no capability, and it is removed with the
    /// package so an uninstall does not leave a stale id behind for the next install to
    /// trust.
    const IAP_FILE: &str = "iap.txt";

    #[derive(Copy, Clone, Debug, PartialEq, Eq)]
    pub enum State {
        Idle,
        /// A bearer request is outstanding — attach, a saved id, or the dialog.
        Connecting,
        Up,
        /// The ladder ran out. Recorded, not retried: a second pass would raise a second
        /// dialog for a channel that is optional by design.
        Unavailable,
    }

    pub struct Bridge {
        pub state: State,
        net: ShimNet,
        fs: ShimFs,
        bearer: Option<Bearer>,
        /// Read at startup, written once the OS says which IAP it settled on.
        saved: Option<u32>,
    }

    impl Bridge {
        pub fn new() -> Self {
            let mut fs = ShimFs;
            let saved = load_iap(&mut fs);
            Bridge { state: State::Idle, net: ShimNet, fs, bearer: None, saved }
        }

        /// Start the ladder. Called when the run starts.
        pub fn connect(&mut self) {
            if self.state != State::Idle {
                return;
            }
            // Attach first, always. It is synchronous and free, and on a phone that is
            // already online it is the whole of the work — no dialog, nothing to wait for.
            //
            // `Bearer::attach` does *not* give up if there is nothing to join: it falls
            // through to the dialog on the first failed event, which is the behaviour that
            // got this SDK online.
            //
            // The saved id is not consulted here on purpose. Attach is cheaper and quieter
            // than starting a named IAP, so it is worth trying even when an id is known —
            // and if it finds nothing, `Bearer` prompts once and we record the answer.
            // Reading `self.saved` before attaching would trade a free success for a
            // guaranteed connection attempt.
            match Bearer::attach(&mut self.net) {
                Ok(b) => {
                    self.bearer = Some(b);
                    self.state = State::Connecting;
                }
                Err(_) => self.state = State::Unavailable,
            }
        }

        /// Feed a platform event. Returns true if the host asked the app to exit.
        pub fn on_event(&mut self, ev: &RawEvent) -> bool {
            if let Some(b) = &mut self.bearer {
                match b.on_event(&mut self.net, ev) {
                    Ok(true) => {
                        // The ordering that matters: a socket opened on a connection that
                        // has not started panics esock rather than failing, so the bridge
                        // connects only once the bearer says it is up.
                        symbian_app::devbridge::connect(Some(b.handle()));
                        self.state = State::Up;
                        // Persist what the OS actually settled on, which may differ from
                        // what was asked for. This is the step that makes the *next* run
                        // silent, and the one whose absence would leave a dialog in front
                        // of every future run.
                        if let Some(iap) = b.iap() {
                            if self.saved != Some(iap) {
                                save_iap(&mut self.fs, iap);
                                self.saved = Some(iap);
                            }
                        }
                    }
                    Ok(false) => {}
                    Err(_) => self.state = State::Unavailable,
                }
            }
            symbian_app::devbridge::on_event(ev)
        }

        pub fn label(&self) -> &'static str {
            match self.state {
                State::Idle => "bridge: not tried",
                State::Connecting => "bridge: connecting",
                State::Up => "bridge: up",
                State::Unavailable => "bridge: offline (report is in C:\\Data\\dump)",
            }
        }
    }

    fn iap_path(fs: &mut ShimFs) -> Option<Utf16Path> {
        let dir = fs::private_path(fs).ok()?;
        Utf16Path::join(dir.as_units(), IAP_FILE).ok()
    }

    fn load_iap(fs: &mut ShimFs) -> Option<u32> {
        let p = iap_path(fs)?;
        let bytes = fs::read(fs, &p).ok()??;
        let text = core::str::from_utf8(&bytes).ok()?;
        // A zero id is not worth trusting: the shim writes zero when it could not read the
        // IAP back, and passing it to net_start would ask for access point number nothing.
        text.trim().parse::<u32>().ok().filter(|v| *v > 0)
    }

    fn save_iap(fs: &mut ShimFs, iap: u32) {
        let Some(p) = iap_path(fs) else { return };
        let mut s = alloc::string::String::new();
        symbian_report::push_i64(&mut s, iap as i64);
        // Best-effort: a bridge that came up and could not record the fact is still a
        // bridge that came up, and the only cost is one more dialog next time.
        let _ = fs::write_atomic(fs, &p, s.as_bytes());
        let _ = fs;
    }
}

impl DevDump {
    pub fn new() -> Self {
        // Registered here rather than in the entry point so the simulator gets it too, and
        // so there is one place that knows this app answers CTL at all.
        #[cfg(feature = "dev-bridge")]
        symbian_app::devbridge::set_control_handler(control);
        DevDump {
            inner: Launcher::new(),
            fs: ShimFs,
            procs: ShimProcs,
            exit: false,
            started: false,
            ticker: None,
            #[cfg(feature = "dev-bridge")]
            bridge: bridge::Bridge::new(),
        }
    }

    /// Advance the run. The device host calls this from its timer every [`TICK_MS`].
    pub fn tick(&mut self) {
        // SAFETY: single-threaded; see RERUN_REQUESTED.
        if unsafe { core::mem::replace(&mut *core::ptr::addr_of_mut!(RERUN_REQUESTED), false) } {
            // A fresh launcher, not a reset one: the previous run's outcomes and its
            // manifest belong to the previous run, and carrying either forward would
            // produce a report whose lines came from two different moments.
            self.inner = Launcher::new();
            self.started = true;
        }
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
        #[cfg(feature = "dev-bridge")]
        self.bridge.connect();
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
        #[cfg(feature = "dev-bridge")]
        if self.bridge.on_event(ev) {
            // The host asked us to quit. Honoured, because a run nobody is watching is a
            // run whose report is already on disk.
            self.exit = true;
        }

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

        #[cfg(feature = "dev-bridge")]
        if y + small.line_height() <= frame.content.y1 {
            c.draw_text(
                Point::new(6, y + small.ascent()),
                self.bridge.label(),
                small,
                theme.palette.dim,
            );
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
        let _g = control_tests::LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: single-threaded under LOCK.
        unsafe { RERUN_REQUESTED = false };
        let mut app = DevDump::new();
        app.tick();
        assert_eq!(app.inner.phase(), Phase::Start);
    }
}

#[cfg(test)]
mod control_tests {
    use super::*;

    /// `RERUN_REQUESTED` is a process-wide static — deliberately, because on the device the
    /// bridge handler is a plain `fn` with no path to the app. Under `cargo test` that makes
    /// every test touching it share one flag, and they run in parallel by default: without
    /// this lock, `ticking_before_start_does_nothing` occasionally saw a `rerun` queued by a
    /// test in another thread and failed for a reason that had nothing to do with it.
    ///
    /// Serialising them is the honest fix. Clearing the flag per test would hide the sharing
    /// rather than account for it, and a test that passes because of timing is worse than
    /// one that fails.
    pub(super) static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn clear() {
        // SAFETY: single-threaded under LOCK.
        unsafe { RERUN_REQUESTED = false };
    }

    #[test]
    fn rerun_is_acknowledged_and_queues_a_run() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        assert!(control("rerun").is_some());
        let mut app = DevDump::new();
        assert!(!app.started);
        app.tick();
        // A run that was never started is started by the verb, which is the point: the
        // phone need not be touched between one report and the next.
        assert!(app.started);
    }

    /// A finished run must start over rather than resume, or the merged report would carry
    /// sections from two different moments as if they were one observation.
    #[test]
    fn rerun_after_a_finished_run_starts_a_new_one() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        control("rerun");
        let mut app = DevDump::new();
        app.tick();
        let before = app.inner.phase();
        control("rerun");
        app.tick();
        assert_eq!(app.inner.phase(), before, "the second run did not begin from the start");
    }

    /// An unknown verb must be refused, not swallowed: the bridge turns `None` into an
    /// error reply, and a host waiting on a dropped line cannot tell a slow device from a
    /// wrong verb.
    #[test]
    fn an_unknown_verb_is_refused() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        assert_eq!(control("wat"), None);
        assert_eq!(control(""), None);
    }

    #[test]
    fn whitespace_around_a_verb_is_tolerated() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        assert!(control("  rerun \n").is_some());
    }
}
