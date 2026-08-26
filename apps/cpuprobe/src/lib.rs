//! Does this handset account for thread CPU time?
//!
//! That is the whole question. `RThread::GetCpuTime` is declared in `e32std.h` and exported from
//! `euser.dso`, so it links — but on Symbian 9.x the kernel-side accounting is a build option, and
//! where it is off the call answers `KErrNotSupported`. The header has no doc comment either way.
//! Nothing in this SDK should draw a CPU figure until the phone has answered, and this is what
//! asks.
//!
//! It samples twice a second and shows, live: the whole phone's load, the busiest processes, and —
//! most importantly — the raw return code, because `KErrNotSupported` is the finding that matters
//! most. Everything is journalled to `C:\Data\_logs\cpuprobe.txt` so the answer survives the app
//! being closed, or panicking.
//!
//! Isolated and non-resident, like `iconprobe`: it links `USE_CPUTIME`, which is a facility no
//! other binary has, and if enumerating threads upsets this platform the cost is one probe.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use symbian::cpu::{self, Sample};
use symbian_ui::{chrome, App, Canvas, Handled, Key, KeyEvent, Point, Rect, Softkey, Theme};

/// How often to sample. Half a second is long enough that the difference is meaningful and short
/// enough to watch something react to being used.
const TICK_MS: i32 = 500;

/// One process being watched.
struct Watch {
    /// The executable name, as the kernel spells it.
    name: String,
    prev: Option<Sample>,
    /// Latest load, as a percentage of one processor.
    load: Option<i32>,
}

pub struct Cpuprobe {
    /// Whole-phone samples, for the headline figure.
    all_prev: Option<Sample>,
    /// The kernel idle thread, which is what turns the total into a load — see `symbian::cpu`.
    idle_prev: Option<Sample>,
    all_load: Option<i32>,
    /// What the idle thread itself is doing, shown because the headline is derived from it and a
    /// derived number nobody can check is a number nobody should trust.
    idle_load: Option<i32>,
    /// The per-process watches, rebuilt when the process list changes.
    watches: Vec<Watch>,
    /// The first line: what the platform said when asked. This is the actual result of the probe.
    verdict: String,
    /// Scroll offset into `watches`.
    top: usize,
    ticker: Option<i32>,
    /// Whether the verdict has been written to the log yet — once, not every tick.
    logged: bool,
    exit: bool,
}

impl Cpuprobe {
    pub fn new() -> Self {
        let mut me = Self {
            all_prev: None,
            idle_prev: None,
            all_load: None,
            idle_load: None,
            watches: Vec::new(),
            verdict: String::from("Sampling…"),
            top: 0,
            ticker: symbian::timer_after(TICK_MS).ok(),
            logged: false,
            exit: false,
        };
        me.rescan();
        me
    }

    /// Rebuild the watch list from the running processes.
    ///
    /// Deliberately not incremental: the list is short, processes come and go, and a probe that
    /// carried stale entries would report load for something that had exited.
    fn rescan(&mut self) {
        let names = cpu::processes();
        symbian::log!("[cpuprobe] processes={}", names.len());
        // Our own process is in that list, and it is always the busiest thing on the phone while
        // the probe runs — it would sit at the top and mislead. Matched by UID rather than by name:
        // the first run filtered on the string "cpuprobe" and the row appeared anyway, because the
        // name the kernel reports is not the one the source spells.
        let own = alloc::format!("[{:08x}]", symbian::own_uid3());
        self.watches = names
            .iter()
            .filter(|full| !full.to_lowercase().contains(&own))
            .map(|full| Watch {
                name: String::from(cpu::short_name(full)),
                prev: None,
                load: None,
            })
            .collect();
    }

    fn tick(&mut self) {
        // The headline first: the whole phone. It also answers the only question that matters if
        // per-process turns out to be unsupported.
        match cpu::sample_all() {
            Ok(now) => {
                // Busy = everything minus the idle thread. The totals alone are always 100% of the
                // wall clock, which is exactly what the first device run showed.
                let idle_now = cpu::sample_idle().ok();
                if let (Some(prev), Some(iprev), Some(inow)) =
                    (self.all_prev, self.idle_prev, idle_now)
                {
                    self.all_load = cpu::busy_percent((&prev, &now), (&iprev, &inow));
                    self.idle_load = iprev.load_percent(&inow);
                }
                self.idle_prev = idle_now;
                if !self.logged {
                    self.verdict = format!("SUPPORTED — {} threads answered", now.threads);
                    symbian::log!("[cpuprobe] supported threads={}", now.threads);
                    self.logged = true;
                }
                self.all_prev = Some(now);
            }
            Err(e) => {
                // The finding. Written once, in the words the platform used.
                if !self.logged {
                    self.verdict = format!("NOT AVAILABLE — {e:?}");
                    symbian::log!("[cpuprobe] unsupported {e:?}");
                    self.logged = true;
                }
            }
        }

        // Then each process. A pattern that matches nothing (the process exited between the scan
        // and now) is an ordinary error and leaves that row blank rather than dropping it.
        for w in &mut self.watches {
            match cpu::sample(&cpu::of_process(&w.name)) {
                Ok(now) => {
                    if let Some(prev) = w.prev {
                        w.load = prev.load_percent(&now);
                    }
                    w.prev = Some(now);
                }
                Err(_) => w.load = None,
            }
        }

        // Busiest first — that is what a task manager is for. Unknown sorts last rather than as
        // zero, so "we could not measure this" never masquerades as "this is idle".
        self.watches.sort_by_key(|w| core::cmp::Reverse(w.load.unwrap_or(-1)));
        self.ticker = symbian::timer_after(TICK_MS).ok();
    }
}

impl Default for Cpuprobe {
    fn default() -> Self {
        Self::new()
    }
}

impl App for Cpuprobe {
    fn title(&self) -> &str {
        "CPU Probe"
    }

    fn handle_key(&mut self, ev: KeyEvent, _theme: &Theme<'_>, _screen: Rect) -> Handled {
        match ev.key {
            Key::Down => {
                self.top = (self.top + 1).min(self.watches.len().saturating_sub(1));
                Handled::Consumed
            }
            Key::Up => {
                self.top = self.top.saturating_sub(1);
                Handled::Consumed
            }
            Key::Select => {
                self.rescan();
                Handled::Consumed
            }
            Key::Softkey(Softkey::Right) => {
                self.exit = true;
                Handled::Consumed
            }
            _ => Handled::Ignored,
        }
    }

    fn handle_raw(&mut self, ev: &symbian_ui::RawEvent) -> Handled {
        if ev.kind == symbian_sys::SHIM_EV_TIMER && Some(ev.handle) == self.ticker {
            self.tick();
            return Handled::Consumed;
        }
        Handled::Ignored
    }

    fn draw(&mut self, c: &mut Canvas<'_>, theme: &Theme<'_>) {
        let screen = Rect::from_size(c.size());
        let frame = chrome::Frame::split(screen, theme, true, true);
        chrome::clear(c, theme);
        chrome::title_bar(c, frame.title, theme, "CPU Probe", None);
        chrome::softkey_bar(c, frame.softkeys, theme, chrome::Softkeys::action("Rescan", "Exit"));

        let body = theme.fonts.body;
        let p = &theme.palette;
        let x = frame.content.x0 + 4;
        let mut y = frame.content.y0 + 2;

        // The verdict, which is the actual output of this probe.
        c.draw_text(Point::new(x, y + body.ascent()), &self.verdict, theme.fonts.strong, p.text);
        y += theme.fonts.strong.line_height();

        // Busy and idle side by side, because the first is derived from the second and a derived
        // number is worth nothing if the working is hidden.
        let total = match (self.all_load, self.idle_load) {
            (Some(b), Some(i)) => format!("Busy {b}%   idle {i}%"),
            (Some(b), None) => format!("Busy {b}%"),
            _ => String::from("Busy —"),
        };
        c.draw_text(Point::new(x, y + body.ascent()), &total, body, p.accent);
        y += body.line_height() + 2;

        // One row per process from the scroll position down, busiest first.
        let rows = ((frame.content.y1 - y) / body.line_height()).max(0) as usize;
        for w in self.watches.iter().skip(self.top).take(rows) {
            let load = match w.load {
                Some(l) => format!("{l}%"),
                None => String::from("—"),
            };
            c.draw_text(Point::new(x, y + body.ascent()), &w.name, body, p.dim);
            let tw = body.measure(&load);
            c.draw_text(Point::new(frame.content.x1 - tw - 4, y + body.ascent()), &load, body, p.text);
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
    fn constructs_and_draws_on_the_host() {
        // The host shim stubs the CPU calls, so this is the "nothing is measurable" rendering —
        // which must still be a readable screen rather than a blank one.
        let mut app = Cpuprobe::new();
        let (_, px) = testing::with_canvas(symbian_ui::gfx::Size::new(320, 240), |c| {
            testing::with_theme(Palette::DARK, |t| app.draw(c, t));
        });
        assert!(px.iter().any(|&p| p != 0));
    }

    #[test]
    fn a_tick_off_device_records_the_refusal_once() {
        let mut app = Cpuprobe::new();
        app.tick();
        assert!(app.logged, "the verdict is the point of the probe");
        let first = app.verdict.clone();
        app.tick();
        assert_eq!(app.verdict, first, "the verdict is written once, not per tick");
    }

    #[test]
    fn keys_do_not_panic_with_an_empty_list() {
        let mut app = Cpuprobe::new();
        testing::with_theme(Palette::DARK, |t| {
            app.handle_key(KeyEvent::new(Key::Down), t, testing::SCREEN);
            app.handle_key(KeyEvent::new(Key::Up), t, testing::SCREEN);
            app.handle_key(KeyEvent::new(Key::Select), t, testing::SCREEN);
            app.handle_key(KeyEvent::new(Key::Softkey(Softkey::Right)), t, testing::SCREEN);
        });
        assert!(app.should_exit());
    }
}
