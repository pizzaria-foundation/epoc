//! The supervisor, as a state machine with no clock, no timer and no shim call.
//!
//! Everything that decides *what happens at boot* lives here, so all of it is a plain `cargo test`
//! on the host: staged launching, death detection, restart budgets, backoff, and the auto-disarm
//! that stops a crash loop from owning the phone. `apps/bootd` is only the hands — it reads files,
//! arms one timer, asks the OS who is running, and does what [`Supervisor::step`] tells it.
//!
//! The caller's loop is:
//!
//! ```ignore
//! loop {
//!     match sup.step(now_ms, &alive) {
//!         Action::Wait(ms)              => { arm_timer(ms); break }
//!         Action::Launch(i) | Action::Restart(i) => {
//!             let rc = launch(cfg.entries[i].uid3);
//!             sup.note_launch(i, rc, now_ms);
//!         }
//!         Action::Disarm(i)             => persist_config_with_entry_disabled(i),
//!         Action::Settled | Action::GaveUp => write_status(sup.snapshot()),
//!         Action::Done                  => { exit = true; break }
//!     }
//! }
//! ```
//!
//! Every action except `Wait` and `Done` means "do this, then ask again".

use alloc::vec::Vec;
use alloc::collections::VecDeque;

use symbian::backoff::Backoff;

use crate::config::{BootConfig, Policy};
use crate::status::{BootStatus, EntryStatus, Mode, State};

/// A launched app is not eligible to be called dead until this long after its launch. A GUI app
/// takes seconds to show up in the process list, and calling it dead early means restarting an app
/// that was merely still starting.
pub const LAUNCH_GRACE_MS: u64 = 20_000;
/// Consecutive not-running observations before an entry counts as dead. One poll can catch a
/// process mid-transition; two in a row will not.
pub const DEAD_STRIKES: u8 = 2;
/// Poll cadence right after boot, and after anything changes.
pub const POLL_BASE_MS: i32 = 15_000;
/// Idle ceiling. Left alone the interval doubles to this and no further, so a quiet phone is woken
/// about once every five minutes.
pub const POLL_MAX_MS: i32 = 300_000;
/// Per-entry restart spacing: an app that crashes on startup is retried at 5 s, 10 s, 20 s … rather
/// than on every poll.
pub const ENTRY_BACKOFF_BASE_MS: i32 = 5_000;
pub const ENTRY_BACKOFF_MAX_MS: i32 = 300_000;
/// Quiet time in the supervise phase before the boot counts as settled.
pub const SETTLE_MS: u64 = 60_000;
/// A launch call that fails outright is retried this many times before the sequence moves on —
/// AppArc may not be serving yet this early in the boot.
pub const LAUNCH_TRIES: u8 = 3;
/// Delay between those retries.
pub const LAUNCH_RETRY_MS: u32 = 3_000;
/// Heartbeat once the supervisor has given up: still alive for bootctl, doing nothing.
pub const HEARTBEAT_MS: u32 = 300_000;
/// No armed delay exceeds this. `symbian::timer_after` takes an `i32` of milliseconds against a
/// shim ceiling of roughly 2000 s; five minutes is comfortably inside it.
pub const MAX_WAIT_MS: u32 = 300_000;

/// One instruction for the caller.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Action {
    /// Arm a one-shot timer for this many milliseconds and stop stepping until it fires.
    Wait(u32),
    /// Launch `cfg.entries[i]` for the first time.
    Launch(usize),
    /// Launch `cfg.entries[i]` again after it died.
    Restart(usize),
    /// `cfg.entries[i]` burned its restart budget: write the config back with it disabled, so the
    /// next boot comes up clean without anyone having to intervene.
    Disarm(usize),
    /// The boot is stable. Write the status and clear the boot counter.
    Settled,
    /// The global restart ceiling is spent. Write the status; nothing more will be restarted.
    GaveUp,
    /// Nothing left to supervise. The caller may exit — which also makes the package
    /// uninstallable, since a live executable pins `\sys\bin`.
    Done,
}

/// Per-entry supervision state. Entries that are disabled or refused keep a slot too, so indices
/// line up with `BootConfig::entries` and the status report can explain every row.
struct Slot {
    uid3: u32,
    policy: Policy,
    delay_ms: u32,
    /// False for a disabled entry, and for bootd/bootctl themselves.
    supervised: bool,
    launched_at: Option<u64>,
    /// Seen running at least once since the last launch. An entry that never appears alive is
    /// never called dead — otherwise an app whose process UID3 differs from its app UID3 would look
    /// permanently dead and be restarted forever.
    ever_alive: bool,
    dead_strikes: u8,
    launch_tries: u8,
    restarts: u16,
    /// No further restart will ever be issued for this entry — it is `Never` and died, or it spent
    /// its budget. Distinct from `state == Dead`, which an entry also wears while it is merely
    /// waiting out its own restart backoff and will come back.
    terminal: bool,
    /// Earliest time another restart may be issued for this entry.
    next_restart_ms: u64,
    backoff: Backoff,
    state: State,
    last_rc: i32,
    launch_at_s: u32,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Phase {
    /// Waiting before launching slot `i`.
    Delay(usize),
    /// Ready to launch slot `i`.
    Launch(usize),
    Supervise,
    GaveUp,
    Done,
}

pub struct Supervisor {
    slots: Vec<Slot>,
    phase: Phase,
    first_delay_ms: u32,
    max_restarts: u16,
    restarts_used: u16,
    launched_any: bool,
    poll: Backoff,
    queue: VecDeque<Action>,
    start_ms: u64,
    /// Last time anything changed — a launch, a death, a restart. Settling is measured from here.
    last_change_ms: u64,
    settled_reported: bool,
    /// A death was seen; the next armed poll goes back to the base rate. Held as a flag rather than
    /// applied on the spot because the round that sees the death returns a `Restart` first, and the
    /// caller comes straight back for the wait.
    reset_cadence: bool,
    boot_count: u8,
}

impl Supervisor {
    /// Plan a boot from `cfg`. `own_uid` and `ctl_uid` are bootd's and bootctl's own UID3s: both are
    /// refused, because bootd relaunching bootd forks forever and bootd relaunching bootctl would
    /// put a settings screen over whatever the user is doing every few seconds.
    pub fn new(cfg: &BootConfig, own_uid: u32, ctl_uid: u32, now_ms: u64) -> Self {
        let slots: Vec<Slot> = cfg
            .entries
            .iter()
            .map(|e| {
                let refused = e.uid3 == 0 || e.uid3 == own_uid || e.uid3 == ctl_uid;
                let supervised = e.enabled && !refused;
                Slot {
                    uid3: e.uid3,
                    policy: e.policy,
                    delay_ms: e.delay_ms,
                    supervised,
                    launched_at: None,
                    ever_alive: false,
                    dead_strikes: 0,
                    launch_tries: 0,
                    restarts: 0,
                    terminal: false,
                    next_restart_ms: 0,
                    backoff: Backoff::new(ENTRY_BACKOFF_BASE_MS, ENTRY_BACKOFF_MAX_MS),
                    state: if refused {
                        State::RefusedSelf
                    } else if !e.enabled {
                        State::Skipped
                    } else {
                        State::Pending
                    },
                    last_rc: 0,
                    launch_at_s: 0,
                }
            })
            .collect();

        let phase = match first_active(&slots, 0) {
            Some(i) => Phase::Delay(i),
            None => Phase::Supervise,
        };

        Self {
            slots,
            phase,
            first_delay_ms: cfg.first_delay_ms,
            max_restarts: cfg.max_restarts,
            restarts_used: 0,
            launched_any: false,
            poll: Backoff::new(POLL_BASE_MS, POLL_MAX_MS),
            queue: VecDeque::new(),
            start_ms: now_ms,
            last_change_ms: now_ms,
            settled_reported: false,
            reset_cadence: false,
            boot_count: 0,
        }
    }

    /// True while the staged launch sequence is still running. Nothing is supervised until it ends.
    pub fn sequencing(&self) -> bool {
        matches!(self.phase, Phase::Delay(_) | Phase::Launch(_))
    }

    /// Recorded in the status report so bootctl can say "this is the third boot that never settled".
    pub fn set_boot_count(&mut self, n: u8) {
        self.boot_count = n;
    }

    /// The UID3s in slot order, so the caller can build the `alive` slice without holding the config.
    pub fn uids(&self) -> impl Iterator<Item = u32> + '_ {
        self.slots.iter().map(|s| s.uid3)
    }

    /// Whether slot `i` is worth asking the OS about. A caller that skips the rest saves a
    /// `TFindProcess` walk per unsupervised row.
    pub fn probes(&self, i: usize) -> bool {
        self.slots.get(i).is_some_and(|s| s.supervised && s.launched_at.is_some())
    }

    /// One instruction. `alive[i]` is whether `cfg.entries[i]`'s process is currently running; it is
    /// only consulted in the supervise phase, so a caller may pass an empty slice before then.
    pub fn step(&mut self, now_ms: u64, alive: &[bool]) -> Action {
        if let Some(a) = self.queue.pop_front() {
            return a;
        }
        match self.phase {
            Phase::Delay(i) => {
                self.phase = Phase::Launch(i);
                let ms = if self.slots[i].launch_tries > 0 {
                    LAUNCH_RETRY_MS
                } else if self.launched_any {
                    self.slots[i].delay_ms
                } else {
                    self.first_delay_ms
                };
                Action::Wait(ms.min(MAX_WAIT_MS))
            }
            Phase::Launch(i) => {
                self.launched_any = true;
                let restart = self.slots[i].launched_at.is_some();
                self.phase = match first_active(&self.slots, i + 1) {
                    Some(j) => Phase::Delay(j),
                    None => Phase::Supervise,
                };
                if restart {
                    Action::Restart(i)
                } else {
                    Action::Launch(i)
                }
            }
            Phase::Supervise => self.supervise(now_ms, alive),
            Phase::GaveUp => Action::Wait(HEARTBEAT_MS),
            Phase::Done => Action::Done,
        }
    }

    /// Report the outcome of the launch the caller just performed. `rc` is 0 for success.
    pub fn note_launch(&mut self, i: usize, rc: i32, now_ms: u64) {
        let Some(s) = self.slots.get_mut(i) else { return };
        s.last_rc = rc;
        self.last_change_ms = now_ms;
        if rc == 0 {
            s.launched_at = Some(now_ms);
            s.launch_at_s = ((now_ms.saturating_sub(self.start_ms)) / 1000) as u32;
            s.ever_alive = false;
            s.dead_strikes = 0;
            s.launch_tries = 0;
            s.state = State::Launched;
            return;
        }
        s.launch_tries = s.launch_tries.saturating_add(1);
        s.state = State::LaunchFailed;
        // A launch that fails this early is usually AppArc not serving yet, not a bad UID. Retry
        // this same entry before moving down the list.
        if s.launch_tries < LAUNCH_TRIES {
            self.phase = Phase::Delay(i);
        }
    }

    /// The report bootd writes for bootctl to read.
    pub fn snapshot(&self) -> BootStatus {
        BootStatus {
            mode: Mode::Normal,
            boot_count: self.boot_count,
            restarts_used: self.restarts_used,
            entries: self
                .slots
                .iter()
                .map(|s| EntryStatus {
                    uid3: s.uid3,
                    last_rc: s.last_rc,
                    launch_at_s: s.launch_at_s,
                    restarts: s.restarts,
                    state: s.state,
                })
                .collect(),
        }
    }

    /// One supervise round: read liveness, decide restarts, then say how long to sleep.
    fn supervise(&mut self, now_ms: u64, alive: &[bool]) -> Action {
        for i in 0..self.slots.len() {
            if !self.probes(i) {
                continue;
            }
            let s = &mut self.slots[i];
            if s.terminal {
                continue;
            }
            if alive.get(i).copied().unwrap_or(false) {
                s.ever_alive = true;
                s.dead_strikes = 0;
                if s.state != State::Alive {
                    s.state = State::Alive;
                    self.last_change_ms = now_ms;
                }
                continue;
            }
            // Not running. Only meaningful once it has had time to start AND has been seen alive.
            let grace_over = s
                .launched_at
                .is_some_and(|t| now_ms.saturating_sub(t) >= LAUNCH_GRACE_MS);
            if !grace_over || !s.ever_alive {
                continue;
            }
            s.dead_strikes = s.dead_strikes.saturating_add(1);
            if s.dead_strikes < DEAD_STRIKES {
                continue;
            }
            self.last_change_ms = now_ms;
            self.settled_reported = false;
            self.reset_cadence = true;
            self.consider_restart(i, now_ms);
        }

        if let Some(a) = self.queue.pop_front() {
            return a;
        }

        let quiet = now_ms.saturating_sub(self.last_change_ms) >= SETTLE_MS;
        if quiet && !self.settled_reported {
            self.settled_reported = true;
            if !self.has_future_work() {
                self.phase = Phase::Done;
            }
            return Action::Settled;
        }
        if self.settled_reported && !self.has_future_work() {
            self.phase = Phase::Done;
            return Action::Done;
        }

        let ms = if core::mem::take(&mut self.reset_cadence) {
            self.poll.on_reset(0)
        } else {
            self.poll.on_tick()
        };
        Action::Wait((ms.max(0) as u32).min(MAX_WAIT_MS))
    }

    /// Decide what a confirmed death means for slot `i`, queueing the resulting action.
    fn consider_restart(&mut self, i: usize, now_ms: u64) {
        let global_spent = self.restarts_used >= self.max_restarts;
        let s = &mut self.slots[i];

        let allowed = match s.policy.budget() {
            Some(n) => s.restarts < n,
            None => true,
        };

        if !allowed {
            // Out of its own budget. `Never` dying is the expected outcome and not a fault; any
            // other policy running out means the app kept crashing, so switch it off for good.
            s.terminal = true;
            if s.policy == Policy::Never {
                s.state = State::Dead;
            } else {
                s.state = State::Disarmed;
                self.queue.push_back(Action::Disarm(i));
            }
            return;
        }

        if global_spent {
            s.state = State::Dead;
            s.terminal = true;
            self.phase = Phase::GaveUp;
            self.queue.push_back(Action::GaveUp);
            return;
        }

        if now_ms < s.next_restart_ms {
            // Its own backoff has not elapsed. Report it as dead but leave it non-terminal, and
            // hold the strike count at the threshold so the very next poll reconsiders it.
            s.state = State::Dead;
            s.dead_strikes = DEAD_STRIKES;
            return;
        }

        s.restarts = s.restarts.saturating_add(1);
        s.dead_strikes = 0;
        let spacing = s.backoff.on_tick().max(0) as u64;
        s.next_restart_ms = now_ms.saturating_add(spacing);
        self.restarts_used = self.restarts_used.saturating_add(1);
        self.queue.push_back(Action::Restart(i));
    }

    /// Whether any supervised entry could still produce work. An entry that is alive under `Never`
    /// will never be restarted, so watching it forever buys nothing.
    fn has_future_work(&self) -> bool {
        self.slots.iter().any(|s| {
            if !s.supervised || s.policy == Policy::Never {
                return false;
            }
            !s.terminal
        })
    }
}

/// The first supervised slot at or after `from`.
fn first_active(slots: &[Slot], from: usize) -> Option<usize> {
    (from..slots.len()).find(|&i| slots[i].supervised)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Entry;
    use alloc::string::String;
    use alloc::vec;

    const OWN: u32 = 0xE0AA_0011;
    const CTL: u32 = 0xE0AA_0010;

    fn cfg_of(entries: Vec<Entry>) -> BootConfig {
        BootConfig { enabled: true, first_delay_ms: 8_000, max_restarts: 10, entries }
    }

    fn entry(uid: u32, policy: Policy) -> Entry {
        Entry { policy, delay_ms: 2_000, ..Entry::new(uid, String::new()) }
    }

    /// One timer round, driven the way bootd drives it: perform every action until the machine asks
    /// to wait. `alive` is the liveness answer for every slot this round.
    fn run(sup: &mut Supervisor, now: u64, alive: &[bool]) -> (Vec<Action>, u32) {
        let mut seen = Vec::new();
        loop {
            let a = sup.step(now, alive);
            match a {
                Action::Wait(ms) => return (seen, ms),
                Action::Done => {
                    seen.push(a);
                    return (seen, 0);
                }
                Action::Launch(i) | Action::Restart(i) => {
                    seen.push(a);
                    sup.note_launch(i, 0, now);
                }
                other => seen.push(other),
            }
        }
    }

    /// Run rounds until the staged launch sequence is done, advancing `t` by each armed wait.
    /// Returns every action the sequence produced.
    fn boot(sup: &mut Supervisor, t: &mut u64) -> Vec<Action> {
        let mut all = Vec::new();
        let mut guard = 0;
        while sup.sequencing() && guard < 64 {
            let (acts, ms) = run(sup, *t, &[]);
            all.extend(acts);
            *t += ms as u64;
            guard += 1;
        }
        all
    }

    /// Let entry 0 be seen alive, then die twice so the supervisor acts on it. Returns the actions
    /// of the round that made the decision.
    fn kill_once(sup: &mut Supervisor, t: &mut u64) -> Vec<Action> {
        run(sup, *t, &[true]);
        *t += LAUNCH_GRACE_MS + 1_000;
        run(sup, *t, &[false]);
        *t += 20_000;
        let (acts, _) = run(sup, *t, &[false]);
        acts
    }

    #[test]
    fn the_first_wait_is_the_global_delay_and_the_rest_are_per_entry() {
        let cfg = cfg_of(vec![entry(1, Policy::Never), entry(2, Policy::Never)]);
        let mut s = Supervisor::new(&cfg, OWN, CTL, 0);
        assert_eq!(s.step(0, &[]), Action::Wait(8_000));
        assert_eq!(s.step(8_000, &[]), Action::Launch(0));
        s.note_launch(0, 0, 8_000);
        assert_eq!(s.step(8_000, &[]), Action::Wait(2_000));
        assert_eq!(s.step(10_000, &[]), Action::Launch(1));
    }

    #[test]
    fn disabled_and_self_entries_are_never_launched() {
        let mut disabled = entry(1, Policy::Always);
        disabled.enabled = false;
        let cfg = cfg_of(vec![disabled, entry(OWN, Policy::Always), entry(CTL, Policy::Always), entry(4, Policy::Never)]);
        let mut s = Supervisor::new(&cfg, OWN, CTL, 0);
        let mut t = 0;
        let acts = boot(&mut s, &mut t);
        let launches: Vec<Action> =
            acts.iter().filter(|a| matches!(a, Action::Launch(_))).copied().collect();
        assert_eq!(launches, vec![Action::Launch(3)], "only the one legitimate entry runs");
        let snap = s.snapshot();
        assert_eq!(snap.entries[0].state, State::Skipped);
        assert_eq!(snap.entries[1].state, State::RefusedSelf);
        assert_eq!(snap.entries[2].state, State::RefusedSelf);
    }

    #[test]
    fn a_failed_launch_is_retried_then_given_up_on() {
        let cfg = cfg_of(vec![entry(1, Policy::Never)]);
        let mut s = Supervisor::new(&cfg, OWN, CTL, 0);
        assert_eq!(s.step(0, &[]), Action::Wait(8_000));
        for _ in 0..LAUNCH_TRIES {
            assert!(matches!(s.step(8_000, &[]), Action::Launch(0)));
            s.note_launch(0, -1, 8_000);
            // Between tries the machine goes back to waiting on this same entry.
            if let Action::Wait(ms) = s.step(8_000, &[]) {
                assert!(ms == LAUNCH_RETRY_MS || ms == POLL_BASE_MS as u32);
            }
        }
        assert_eq!(s.snapshot().entries[0].state, State::LaunchFailed);
        assert_eq!(s.snapshot().entries[0].last_rc, -1);
    }

    #[test]
    fn a_death_inside_the_grace_window_is_not_a_death() {
        let cfg = cfg_of(vec![entry(1, Policy::Always)]);
        let mut s = Supervisor::new(&cfg, OWN, CTL, 0);
        let mut t = 0;
        boot(&mut s, &mut t);
        // Not running, but only a second after the launch: still starting, not dead.
        let (acts, _) = run(&mut s, 9_000, &[false]);
        assert!(
            acts.iter().all(|a| !matches!(a, Action::Restart(_))),
            "no restart during the grace window, got {acts:?}"
        );
    }

    #[test]
    fn an_app_never_seen_alive_is_never_restarted() {
        // The UID-mismatch case: the process list never shows it, so it always reads as not
        // running. Restarting on that would loop forever.
        let cfg = cfg_of(vec![entry(1, Policy::Always)]);
        let mut s = Supervisor::new(&cfg, OWN, CTL, 0);
        let mut t = 0;
        boot(&mut s, &mut t);
        for _ in 0..10 {
            let (acts, ms) = run(&mut s, t, &[false]);
            assert!(acts.iter().all(|a| !matches!(a, Action::Restart(_))));
            t += ms as u64;
        }
    }

    #[test]
    fn two_strikes_are_needed_before_a_restart() {
        let cfg = cfg_of(vec![entry(1, Policy::Always)]);
        let mut s = Supervisor::new(&cfg, OWN, CTL, 0);
        let mut t = 0;
        boot(&mut s, &mut t);
        run(&mut s, t, &[true]); // seen alive
        t += LAUNCH_GRACE_MS + 1_000;
        let (first, _) = run(&mut s, t, &[false]);
        assert!(first.is_empty(), "one missed observation is not a death");
        let (second, _) = run(&mut s, t + 20_000, &[false]);
        assert_eq!(second, vec![Action::Restart(0)]);
    }

    #[test]
    fn never_does_not_restart_and_does_not_disarm() {
        let cfg = cfg_of(vec![entry(1, Policy::Never)]);
        let mut s = Supervisor::new(&cfg, OWN, CTL, 0);
        let mut t = 0;
        boot(&mut s, &mut t);
        let acts = kill_once(&mut s, &mut t);
        assert!(acts.iter().all(|a| !matches!(a, Action::Restart(_) | Action::Disarm(_))));
        assert_eq!(s.snapshot().entries[0].state, State::Dead);
    }

    #[test]
    fn times_restarts_exactly_n_then_disarms() {
        let cfg = cfg_of(vec![entry(1, Policy::Times(2))]);
        let mut s = Supervisor::new(&cfg, OWN, CTL, 0);
        let mut t = 0;
        boot(&mut s, &mut t);
        let mut restarts = 0;
        let mut disarmed = false;
        for _ in 0..6 {
            let acts = kill_once(&mut s, &mut t);
            restarts += acts.iter().filter(|a| matches!(a, Action::Restart(_))).count();
            disarmed |= acts.contains(&Action::Disarm(0));
            if disarmed {
                break;
            }
        }
        assert_eq!(restarts, 2, "Times(2) restarts exactly twice");
        assert!(disarmed, "then switches itself off in the config");
        assert_eq!(s.snapshot().entries[0].state, State::Disarmed);
        assert_eq!(s.snapshot().restarts_used, 2);
    }

    #[test]
    fn always_runs_until_the_global_ceiling_then_gives_up() {
        let mut cfg = cfg_of(vec![entry(1, Policy::Always)]);
        cfg.max_restarts = 3;
        let mut s = Supervisor::new(&cfg, OWN, CTL, 0);
        let mut t = 0;
        boot(&mut s, &mut t);
        let mut restarts = 0;
        let mut gave_up = false;
        for _ in 0..8 {
            let acts = kill_once(&mut s, &mut t);
            restarts += acts.iter().filter(|a| matches!(a, Action::Restart(_))).count();
            gave_up |= acts.contains(&Action::GaveUp);
            if gave_up {
                break;
            }
        }
        assert_eq!(restarts, 3, "bounded by max_restarts, not by the policy");
        assert!(gave_up);
        // Past the ceiling it only heartbeats.
        assert_eq!(s.step(t, &[false]), Action::Wait(HEARTBEAT_MS));
    }

    #[test]
    fn a_restart_is_spaced_by_the_entrys_own_backoff() {
        let cfg = cfg_of(vec![entry(1, Policy::Always)]);
        let mut s = Supervisor::new(&cfg, OWN, CTL, 0);
        let mut t = 0;
        boot(&mut s, &mut t);
        let acts = kill_once(&mut s, &mut t);
        assert_eq!(acts, vec![Action::Restart(0)]);
        // It is seen alive, then dies again immediately. The entry's own backoff has not elapsed,
        // so the second death does not buy a second restart yet.
        run(&mut s, t, &[true]);
        let (again, _) = run(&mut s, t + 1, &[false]);
        assert!(again.iter().all(|a| !matches!(a, Action::Restart(_))));
    }

    #[test]
    fn the_poll_interval_doubles_while_healthy_and_snaps_back_on_a_death() {
        let cfg = cfg_of(vec![entry(1, Policy::Always)]);
        let mut s = Supervisor::new(&cfg, OWN, CTL, 0);
        let mut t = 0;
        boot(&mut s, &mut t);
        let (_, a) = run(&mut s, t, &[true]);
        t += a as u64;
        let (_, b) = run(&mut s, t, &[true]);
        assert!(b > a, "doubling while everything is healthy: {a} then {b}");
        t += LAUNCH_GRACE_MS + 1_000;
        run(&mut s, t, &[false]);
        let (_, after) = run(&mut s, t + 20_000, &[false]);
        assert_eq!(after, POLL_BASE_MS as u32, "a death snaps the cadence back to the base rate");
    }

    #[test]
    fn a_quiet_boot_settles_once() {
        let cfg = cfg_of(vec![entry(1, Policy::Always)]);
        let mut s = Supervisor::new(&cfg, OWN, CTL, 0);
        let mut t = 0;
        boot(&mut s, &mut t);
        let (early, _) = run(&mut s, t, &[true]);
        assert!(!early.contains(&Action::Settled), "the app only just came up");
        let (late, _) = run(&mut s, t + SETTLE_MS, &[true]);
        assert!(late.contains(&Action::Settled));
        let (again, _) = run(&mut s, t + SETTLE_MS * 3, &[true]);
        assert!(!again.contains(&Action::Settled), "settling is reported once, not every poll");
    }

    #[test]
    fn nothing_left_to_watch_means_done() {
        // A single Never entry, alive: it will never be restarted, so there is nothing to supervise.
        let cfg = cfg_of(vec![entry(1, Policy::Never)]);
        let mut s = Supervisor::new(&cfg, OWN, CTL, 0);
        let mut t = 0;
        boot(&mut s, &mut t);
        run(&mut s, t, &[true]);
        let (acts, _) = run(&mut s, t + SETTLE_MS, &[true]);
        assert!(acts.contains(&Action::Settled));
        assert!(acts.contains(&Action::Done));
    }

    #[test]
    fn an_empty_config_settles_and_finishes_without_launching() {
        let cfg = cfg_of(vec![]);
        let mut s = Supervisor::new(&cfg, OWN, CTL, 0);
        let (acts, _) = run(&mut s, SETTLE_MS, &[]);
        assert!(acts.iter().all(|a| !matches!(a, Action::Launch(_))));
        assert!(acts.contains(&Action::Done));
    }

    #[test]
    fn a_zero_ceiling_disables_restarting_wholesale() {
        let mut cfg = cfg_of(vec![entry(1, Policy::Always)]);
        cfg.max_restarts = 0;
        let mut s = Supervisor::new(&cfg, OWN, CTL, 0);
        let mut t = 0;
        boot(&mut s, &mut t);
        let acts = kill_once(&mut s, &mut t);
        assert!(acts.contains(&Action::GaveUp));
        assert!(acts.iter().all(|a| !matches!(a, Action::Restart(_))));
    }

    #[test]
    fn every_armed_wait_is_inside_the_shim_timer_ceiling() {
        let mut cfg = cfg_of(vec![entry(1, Policy::Always)]);
        cfg.first_delay_ms = u32::MAX;
        let mut s = Supervisor::new(&cfg, OWN, CTL, 0);
        match s.step(0, &[]) {
            Action::Wait(ms) => assert_eq!(ms, MAX_WAIT_MS),
            other => panic!("expected a wait, got {other:?}"),
        }
    }
}
