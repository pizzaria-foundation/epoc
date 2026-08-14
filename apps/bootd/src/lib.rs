//! The boot supervisor: the one thing registered in the platform's start-up list.
//!
//! S60 can register an executable to run at boot and nothing else — `STARTUP_ITEM_INFO` has no
//! order field, and its `recovery` field has exactly one legal value, "do nothing". So the start-up
//! list holds this daemon and only this daemon, and everything the platform lacks happens here:
//! read the boot list, launch its apps in order with a delay between them, watch them, and restart
//! them according to their policy.
//!
//! All the deciding is in `symbian_bootcfg::supervise`, which is pure and host-tested. This file is
//! the hands: files, one timer, and asking the OS who is running.
//!
//! Two things it deliberately never does. It never *kills* anything — it only creates, which
//! deletes the whole class of "the supervisor took the phone down". And it never supervises itself
//! or `bootctl`: relaunching itself forks forever, and relaunching the editor would put a settings
//! screen over the user every few seconds.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use symbian::fs::{self, Fs, ShimFs, Utf16Path};
use symbian::{apps, process};
use symbian_bootcfg::status::Mode;
use symbian_bootcfg::supervise::{Action, Supervisor};
use symbian_bootcfg::{
    BootConfig, BOOTCTL_UID, BOOTD_UID, CONFIG_PATH, COUNT_PATH, DATA_DIR, SAFE_MODE_STRIKES,
    STATUS_PATH,
};

/// A ceiling on actions performed in one wake. The supervisor is a finite machine and cannot
/// legitimately need this many, so hitting it means a bug — and a bug that spins here would spin
/// the CPU at boot, which is the failure this SDK has already paid for once.
const MAX_ACTIONS_PER_WAKE: u32 = 64;

pub struct Bootd {
    fs: ShimFs,
    cfg: BootConfig,
    sup: Option<Supervisor>,
    /// The pending one-shot. One at a time, ever: the shim has eight timer slots and this needs one.
    ticker: Option<i32>,
    exit: bool,
    mode: Mode,
    boot_count: u8,
    /// `monotonic_us` at start, so every timestamp is a duration and never depends on the phone's
    /// clock — which at boot may be wrong, and on this handset has been.
    start_us: u64,
}

impl Bootd {
    pub fn new() -> Self {
        let mut me = Self {
            fs: ShimFs,
            cfg: BootConfig::default(),
            sup: None,
            ticker: None,
            exit: false,
            mode: Mode::Normal,
            boot_count: 0,
            start_us: symbian::monotonic_us(),
        };
        me.arm_boot();
        me
    }

    /// Decide what this boot is: safe mode, a refused config, disabled, or a real run.
    fn arm_boot(&mut self) {
        if let Some(dir) = path_of(DATA_DIR) {
            let _ = self.fs.mkdir(dir.as_units());
        }

        let strikes = self.read_count();
        symbian::log!("[bootd] start strikes={strikes}");

        // Three boots in a row that never settled. Something in the list is taking the phone down,
        // so launch nothing and leave the reason on disk. bootctl's Reset clears the counter — an
        // explicit acknowledgement, which is the point.
        if strikes >= SAFE_MODE_STRIKES {
            symbian::log!("[bootd] SAFE MODE: {strikes} unsettled boots, launching nothing");
            self.mode = Mode::Safe;
            self.boot_count = strikes;
            self.write_status_bare();
            self.exit = true;
            return;
        }
        self.boot_count = strikes.saturating_add(1);
        self.write_count(self.boot_count);

        let raw = fs::read(&mut self.fs, &match path_of(CONFIG_PATH) {
            Some(p) => p,
            None => return,
        });
        let bytes = match raw {
            Ok(Some(b)) => b,
            Ok(None) => {
                symbian::log!("[bootd] no config yet; nothing to do");
                self.clear_count();
                self.exit = true;
                return;
            }
            Err(e) => {
                symbian::log!("[bootd] config read err={e:?}");
                self.mode = Mode::ConfigError;
                self.write_status_bare();
                self.exit = true;
                return;
            }
        };

        self.cfg = match BootConfig::decode(&bytes) {
            Ok(c) => c,
            Err(e) => {
                // A config that will not decode is never guessed at. Launching a half-read boot
                // list is how a phone ends up in a loop nobody can explain.
                symbian::log!("[bootd] config refused: {e:?}");
                self.mode = Mode::ConfigError;
                self.write_status_bare();
                self.exit = true;
                return;
            }
        };

        if !self.cfg.enabled {
            symbian::log!("[bootd] master switch off");
            self.mode = Mode::Disabled;
            self.write_status_bare();
            self.clear_count();
            self.exit = true;
            return;
        }

        let now = self.now_ms();
        let mut sup = Supervisor::new(&self.cfg, BOOTD_UID, BOOTCTL_UID, now);
        sup.set_boot_count(self.boot_count);
        symbian::log!(
            "[bootd] {} entries, first delay {} ms, ceiling {}",
            self.cfg.entries.len(),
            self.cfg.first_delay_ms,
            self.cfg.max_restarts
        );
        self.sup = Some(sup);
        self.pump();
    }

    /// Milliseconds since this daemon started.
    fn now_ms(&self) -> u64 {
        symbian::monotonic_us().saturating_sub(self.start_us) / 1_000
    }

    /// Run the state machine until it asks to wait.
    fn pump(&mut self) {
        let Some(mut sup) = self.sup.take() else { return };
        let now = self.now_ms();
        let alive = self.probe(&sup);

        for _ in 0..MAX_ACTIONS_PER_WAKE {
            match sup.step(now, &alive) {
                Action::Wait(ms) => {
                    self.arm(ms as i32);
                    self.sup = Some(sup);
                    return;
                }
                Action::Launch(i) | Action::Restart(i) => {
                    let uid = self.cfg.entries.get(i).map(|e| e.uid3).unwrap_or(0);
                    let rc = match apps::launch(uid) {
                        Ok(()) => 0,
                        Err(e) => e.code(),
                    };
                    symbian::log!("[bootd] launch uid={uid:08X} rc={rc}");
                    sup.note_launch(i, rc, now);
                }
                Action::Disarm(i) => {
                    if let Some(e) = self.cfg.entries.get_mut(i) {
                        e.enabled = false;
                        e.auto_disarmed = true;
                        symbian::log!("[bootd] auto-disarm uid={:08X}: crash loop", e.uid3);
                    }
                    self.write_config();
                }
                Action::Settled => {
                    // The boot is stable, so this one does not count against safe mode.
                    self.clear_count();
                    self.write_status(&sup);
                    symbian::log!("[bootd] settled");
                }
                Action::GaveUp => {
                    symbian::log!("[bootd] restart ceiling reached; supervising no further");
                    self.write_status(&sup);
                }
                Action::Done => {
                    self.write_status(&sup);
                    symbian::log!("[bootd] nothing left to supervise; exiting");
                    self.exit = true;
                    self.sup = Some(sup);
                    return;
                }
            }
        }

        symbian::log!("[bootd] action ceiling hit — stopping to avoid a spin");
        self.exit = true;
        self.sup = Some(sup);
    }

    /// Ask the OS which supervised entries are running.
    ///
    /// `process::is_running` and NOT `apps::running()`: the latter goes through `CCoeEnv::Static()`
    /// (see `shim/src/shim_apparc.cpp`), which a headless daemon does not have, so it would answer
    /// `NotReady` on every call, forever. `TFindProcess` needs no window server.
    fn probe(&self, sup: &Supervisor) -> alloc::vec::Vec<bool> {
        self.cfg
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| sup.probes(i) && process::is_running(e.uid3))
            .collect()
    }

    /// Cancel any pending timer and arm a fresh one-shot. A one-shot frees its slot when it fires,
    /// so cancelling an already-fired handle is a safe no-op.
    fn arm(&mut self, ms: i32) {
        if let Some(t) = self.ticker.take() {
            symbian::timer_cancel(t);
        }
        self.ticker = symbian::timer_after(ms).ok();
    }

    fn write_config(&mut self) {
        let Some(p) = path_of(CONFIG_PATH) else { return };
        let bytes = self.cfg.encode();
        if let Err(e) = fs::write_atomic(&mut self.fs, &p, &bytes) {
            // bootctl may hold the file open. Losing the disarm costs one more crash loop before
            // the next boot re-runs the same arithmetic, so it is worth reporting and not retrying.
            symbian::log!("[bootd] config write err={e:?}");
        }
    }

    fn write_status(&mut self, sup: &Supervisor) {
        let mut st = sup.snapshot();
        st.mode = self.mode;
        st.boot_count = self.boot_count;
        let Some(p) = path_of(STATUS_PATH) else { return };
        let _ = fs::write_atomic(&mut self.fs, &p, &st.encode());
    }

    /// A status file for a boot that never built a supervisor — safe mode, a refused config, or the
    /// master switch off. Without this, "bootd deliberately did nothing" and "bootd never ran" look
    /// identical from bootctl.
    fn write_status_bare(&mut self) {
        let st = symbian_bootcfg::BootStatus {
            mode: self.mode,
            boot_count: self.boot_count,
            restarts_used: 0,
            entries: alloc::vec::Vec::new(),
        };
        let Some(p) = path_of(STATUS_PATH) else { return };
        let _ = fs::write_atomic(&mut self.fs, &p, &st.encode());
    }

    fn read_count(&mut self) -> u8 {
        let Some(p) = path_of(COUNT_PATH) else { return 0 };
        match fs::read(&mut self.fs, &p) {
            Ok(Some(b)) => b.first().copied().unwrap_or(0),
            _ => 0,
        }
    }

    fn write_count(&mut self, n: u8) {
        let Some(p) = path_of(COUNT_PATH) else { return };
        let _ = fs::write_atomic(&mut self.fs, &p, &[n]);
    }

    fn clear_count(&mut self) {
        self.boot_count = 0;
        self.write_count(0);
    }
}

/// A device path, or `None` if it will not fit the shim's 256-unit buffer. Every caller treats
/// `None` as "skip this write" rather than panicking — a boot supervisor must not panic.
fn path_of(s: &str) -> Option<Utf16Path> {
    Utf16Path::new(s).ok()
}

impl Default for Bootd {
    fn default() -> Self {
        Self::new()
    }
}

impl symbian_app::DaemonApp for Bootd {
    fn handle_raw(&mut self, ev: &symbian_sys::ShimEvent) {
        if ev.kind == symbian_sys::SHIM_EV_TIMER && Some(ev.handle) == self.ticker {
            self.ticker = None;
            self.pump();
        }
    }

    fn should_exit(&self) -> bool {
        self.exit
    }
}
