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
/// The cadence when a critical entry is armed. A home screen that dies must come back in seconds,
/// and the five-minute idle ceiling above is how a crash turns into "the phone had no home for
/// four minutes". Paid for in wakes: a `TFindProcess` walk every 5..30 s against every 15..300 s.
pub const POLL_CRITICAL_BASE_MS: i32 = 5_000;
pub const POLL_CRITICAL_MAX_MS: i32 = 30_000;
/// Per-entry restart spacing: an app that crashes on startup is retried at 5 s, 10 s, 20 s … rather
/// than on every poll.
pub const ENTRY_BACKOFF_BASE_MS: i32 = 5_000;
pub const ENTRY_BACKOFF_MAX_MS: i32 = 300_000;
/// Quiet time in the supervise phase before the boot counts as settled.
pub const SETTLE_MS: u64 = 60_000;
/// Restarts allowed for an entry that was launched and never once seen running. Distinct from the
/// policy budget, which governs an app that *died* — this governs one that never arrived. Bounded
/// so that an app whose process UID3 differs from its registered app UID3 (permanently invisible to
/// `TFindProcess`, and perfectly healthy) is retried a few times rather than forever.
pub const NEVER_UP_TRIES: u8 = 4;
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
    /// See [`crate::config::Entry::critical`]: fast cadence, and exempt from the global ceiling.
    critical: bool,
    launched_at: Option<u64>,
    /// Seen running at least once since the last launch. An entry that never appears alive is
    /// never called dead — otherwise an app whose process UID3 differs from its app UID3 would look
    /// permanently dead and be restarted forever.
    ever_alive: bool,
    dead_strikes: u8,
    /// Restarts issued for an entry that has never been observed running. Bounded separately from
    /// the policy budget: this is "it never came up", not "it died", and the two run out for
    /// different reasons.
    never_up_tries: u8,
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
    /// The Software Installer is on screen right now, so no restart may be issued.
    ///
    /// Set by the caller each round. It exists because of exactly one sequence: the user installs a
    /// new build over a running one, the installer stops the old process to replace `\sys\bin`,
    /// and this supervisor — whose whole job is to notice that death — puts the app straight back
    /// and pins the file the installer is holding open. The install then fails, and the reason the
    /// user sees is "file in use", which names the wrong culprit entirely.
    ///
    /// Deferring, not cancelling: the death is still recorded, the strike count is held at the
    /// threshold, and the restart happens on the first poll after the installer is gone.
    installing: bool,
    /// The one entry whose liveness is somebody else's business this round: the application
    /// `crate::update` is in the middle of installing and proving.
    ///
    /// Distinct from [`Supervisor::installing`] in scope and in kind, and the difference is the
    /// reason both exist. `installing` is global and temporary — *nobody* is restarted while an
    /// installer holds a file — and it still records the death. This is one entry and it records
    /// nothing, because the updater is deliberately launching that application and watching it
    /// die: a death inside a proof window is data the updater is collecting, not a fault the
    /// supervisor should be reacting to.
    ///
    /// Without it the two machines fight over the same application. The updater launches 0.2.0 and
    /// starts a 60-second proof window; the bad build dies in three seconds; the supervisor sees the
    /// death first, restarts it, and the updater then observes a *running* process for most of the
    /// window. The rollback still happens, but for the wrong reason and by luck — and meanwhile the
    /// entry has burned its restart budget and comes back `auto_disarmed`, so the home screen
    /// returns switched off.
    updating: Option<u32>,
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
                    critical: e.critical,
                    launched_at: None,
                    ever_alive: false,
                    dead_strikes: 0,
                    never_up_tries: 0,
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

        // One cadence for the whole supervisor, not one per entry: there is a single timer, and the
        // fastest thing being watched sets the rate. So the presence of any armed critical entry
        // makes every poll fast — which is the point, since the poll is what notices the death.
        let critical = slots.iter().any(|s| s.supervised && s.critical);
        let (poll_base, poll_max) = if critical {
            (POLL_CRITICAL_BASE_MS, POLL_CRITICAL_MAX_MS)
        } else {
            (POLL_BASE_MS, POLL_MAX_MS)
        };

        Self {
            slots,
            phase,
            first_delay_ms: cfg.first_delay_ms,
            max_restarts: cfg.max_restarts,
            restarts_used: 0,
            launched_any: false,
            poll: Backoff::new(poll_base, poll_max),
            queue: VecDeque::new(),
            start_ms: now_ms,
            last_change_ms: now_ms,
            settled_reported: false,
            reset_cadence: false,
            boot_count: 0,
            installing: false,
            updating: None,
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

    /// Tell the supervisor whether the Software Installer is running, before each [`step`] round.
    ///
    /// [`step`]: Supervisor::step
    pub fn set_installing(&mut self, installing: bool) {
        self.installing = installing;
    }

    /// Name the entry the updater owns this round, or `None` when no update is in flight.
    ///
    /// Set before each [`step`] round, like [`Supervisor::set_installing`]. Passing a UID3 that is
    /// not in the boot list is harmless and means nothing is exempt — the caller does not have to
    /// know whether the application being updated is also supervised.
    ///
    /// [`step`]: Supervisor::step
    pub fn set_updating(&mut self, uid3: Option<u32>) {
        self.updating = uid3;
    }

    /// The UID3s in slot order, so the caller can build the `alive` slice without holding the config.
    pub fn uids(&self) -> impl Iterator<Item = u32> + '_ {
        self.slots.iter().map(|s| s.uid3)
    }

    /// Whether slot `i` is worth asking the OS about. A caller that skips the rest saves a
    /// `TFindProcess` walk per unsupervised row.
    ///
    /// It is also where [`Supervisor::set_updating`] takes effect, and deliberately so: the
    /// supervise round opens by skipping every slot this returns `false` for, so one answer here
    /// exempts the updated entry from all of it at once — no death observed, no strike counted, no
    /// restart issued, no budget spent, no auto-disarm. Spreading the same rule across four
    /// decisions is how two of them end up disagreeing.
    pub fn probes(&self, i: usize) -> bool {
        self.slots.get(i).is_some_and(|s| {
            s.supervised && s.launched_at.is_some() && Some(s.uid3) != self.updating
        })
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
            // Not running. Only meaningful once it has had time to start.
            let grace_over = s
                .launched_at
                .is_some_and(|t| now_ms.saturating_sub(t) >= LAUNCH_GRACE_MS);
            if !grace_over {
                continue;
            }
            // Launched, past its grace, and never once observed running. Two different things look
            // like this and they need opposite treatment:
            //
            //   - an app whose process UID3 differs from its registered app UID3, which is alive
            //     and will *never* be seen — restarting it forever is the bug this guard was added
            //     for;
            //   - an app that failed to start, or crashed in its first seconds, which is the
            //     failure a boot supervisor most needs to catch.
            //
            // Refusing to act was wrong for the second, and silently: the entry stopped mattering,
            // nothing was written down, and the boot "settled" with the home screen absent.
            // Measured on the E72, where the launcher crashed on roughly two starts in three.
            //
            // So: bounded retries. A few attempts cover a crash that is not deterministic; running
            // out stops the forever-loop the guard was protecting against, and does it loudly.
            if !s.ever_alive {
                if s.never_up_tries >= NEVER_UP_TRIES {
                    if !s.terminal {
                        s.terminal = true;
                        s.state = State::Dead;
                        self.last_change_ms = now_ms;
                    }
                    continue;
                }
                if now_ms < s.next_restart_ms {
                    continue;
                }
                s.never_up_tries = s.never_up_tries.saturating_add(1);
                let spacing = s.backoff.on_tick().max(0) as u64;
                s.next_restart_ms = now_ms.saturating_add(spacing);
                s.launched_at = Some(now_ms);
                self.last_change_ms = now_ms;
                self.settled_reported = false;
                self.reset_cadence = true;
                self.queue.push_back(Action::Restart(i));
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

        // The global ceiling exists to stop a handful of flapping apps from owning the phone. A
        // critical entry is the phone — refusing to bring the home screen back because a different
        // app crashed ten times is the ceiling doing the exact damage it was added to prevent. It
        // still burns from the same counter, so everything non-critical stays stopped.
        if global_spent && !s.critical {
            s.state = State::Dead;
            s.terminal = true;
            self.phase = Phase::GaveUp;
            self.queue.push_back(Action::GaveUp);
            return;
        }

        if self.installing || now_ms < s.next_restart_ms {
            // Either its own backoff has not elapsed, or an install is in progress. Report it as
            // dead but leave it non-terminal, and hold the strike count at the threshold so the
            // very next poll reconsiders it. Neither case burns a restart from any budget: the
            // entry did not come back, so nothing was spent trying.
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
    ///
    /// An exempt entry counts as work even though nothing is being done about it. It is exempt for
    /// the duration of an update and not forever, so concluding `Done` — which makes the daemon
    /// exit — would hand the phone back with the entry unsupervised and nobody to pick it up when
    /// the update finishes.
    fn has_future_work(&self) -> bool {
        self.slots.iter().any(|s| {
            if !s.supervised {
                return false;
            }
            // Checked before the policy, and the order is the whole point. `Never` means "do not
            // restart this if it dies", which is a statement about deaths — not permission to end
            // the daemon while somebody else is mid-way through replacing the executable. A home
            // screen set to `Never` is the case that gets this wrong: the supervisor would declare
            // itself finished during the install and bootd would exit, leaving the update with
            // nobody to prove it.
            if Some(s.uid3) == self.updating {
                return true;
            }
            if s.policy == Policy::Never {
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

    fn critical(uid: u32) -> Entry {
        Entry { delay_ms: 2_000, ..Entry::home(uid, String::new()) }
    }

    /// The interval a quiet supervisor settles on: run enough healthy rounds for the backoff to
    /// reach its ceiling, and report the last armed wait. That ceiling is the honest measure of
    /// "how long can the home be dead before anyone notices", where the first wait is not — the
    /// staged boot has already stepped the ladder by the time supervision starts.
    fn idle_cadence(cfg: &BootConfig, alive: &[bool]) -> u32 {
        let mut s = Supervisor::new(cfg, OWN, CTL, 0);
        let mut t = 0;
        boot(&mut s, &mut t);
        let mut ms = 0;
        for _ in 0..12 {
            let (_, w) = run(&mut s, t, alive);
            ms = w;
            t += w as u64;
        }
        ms
    }

    #[test]
    fn an_install_defers_the_restart_instead_of_racing_the_installer_for_the_file() {
        let mut s = Supervisor::new(&cfg_of(vec![critical(1)]), OWN, CTL, 0);
        let mut t = 0;
        boot(&mut s, &mut t);

        // The installer stops the running app to replace \sys\bin. Put it back now and the
        // install fails on a file this supervisor is holding open.
        s.set_installing(true);
        let acts = kill_once(&mut s, &mut t);
        assert!(!acts.iter().any(|a| matches!(a, Action::Restart(_))), "nothing is relaunched");
        assert_eq!(s.snapshot().entries[0].state, State::Dead, "but the death is recorded");

        // Still nothing, however long the install takes.
        t += 120_000;
        let (acts, _) = run(&mut s, t, &[false]);
        assert!(!acts.iter().any(|a| matches!(a, Action::Restart(_))));

        // Installer gone: the very next poll brings it back, with no budget spent on the wait.
        s.set_installing(false);
        let (acts, _) = run(&mut s, t, &[false]);
        assert!(acts.contains(&Action::Restart(0)), "the home returns as soon as it is safe to");
    }

    #[test]
    fn a_deferred_restart_costs_no_budget() {
        // `Times(1)` held through a long install must still have its one restart afterwards.
        let e = Entry { policy: Policy::Times(1), ..critical(1) };
        let mut s = Supervisor::new(&cfg_of(vec![e]), OWN, CTL, 0);
        let mut t = 0;
        boot(&mut s, &mut t);
        s.set_installing(true);
        for _ in 0..5 {
            kill_once(&mut s, &mut t);
        }
        s.set_installing(false);
        let (acts, _) = run(&mut s, t, &[false]);
        assert!(acts.contains(&Action::Restart(0)), "five deferred rounds spent nothing");
    }

    #[test]
    fn a_critical_entry_makes_the_whole_supervisor_watch_at_the_fast_cadence() {
        let ordinary = idle_cadence(&cfg_of(vec![entry(1, Policy::Always)]), &[true]);
        let home = idle_cadence(&cfg_of(vec![critical(1)]), &[true]);
        assert_eq!(ordinary, POLL_MAX_MS as u32);
        assert_eq!(home, POLL_CRITICAL_MAX_MS as u32);
        assert!(home < ordinary, "the home is noticed dead sooner, which is the whole point");
    }

    #[test]
    fn a_disabled_critical_entry_does_not_speed_the_cadence_up() {
        // The flag is about what is *being watched*. An entry nobody is watching must not make the
        // phone wake ten times as often for nothing.
        let mut off = critical(1);
        off.enabled = false;
        let cfg = cfg_of(vec![off, entry(2, Policy::Always)]);
        assert_eq!(idle_cadence(&cfg, &[false, true]), POLL_MAX_MS as u32);
    }

    #[test]
    fn the_global_ceiling_stops_an_ordinary_entry_but_never_the_home() {
        // One flapping app burns the whole ceiling; the home dies afterwards and must still return.
        let cfg = BootConfig {
            max_restarts: 1,
            ..cfg_of(vec![entry(1, Policy::Always), critical(2)])
        };
        let mut s = Supervisor::new(&cfg, OWN, CTL, 0);
        let mut t = 0;
        boot(&mut s, &mut t);

        // Both come up, then both die. Entry 0 spends the ceiling of 1.
        run(&mut s, t, &[true, true]);
        t += LAUNCH_GRACE_MS + 1_000;
        run(&mut s, t, &[false, true]);
        t += 20_000;
        let (acts, _) = run(&mut s, t, &[false, true]);
        assert!(acts.contains(&Action::Restart(0)), "the first death is inside the ceiling");

        // Now the home dies with the ceiling already spent.
        run(&mut s, t, &[true, true]);
        t += LAUNCH_GRACE_MS + 1_000;
        run(&mut s, t, &[true, false]);
        t += 20_000;
        let (acts, _) = run(&mut s, t, &[true, false]);
        assert!(acts.contains(&Action::Restart(1)), "the home comes back past the global ceiling");
        assert!(!acts.contains(&Action::GaveUp), "and the supervisor does not declare defeat");
    }

    #[test]
    fn a_critical_entry_still_obeys_its_own_budget_and_safe_mode_above_it() {
        // Critical is an exemption from ONE limit. `Times(1)` still runs out, because a policy the
        // user chose is not something the flag gets to overrule.
        let e = Entry { policy: Policy::Times(1), ..critical(1) };
        let mut s = Supervisor::new(&cfg_of(vec![e]), OWN, CTL, 0);
        let mut t = 0;
        boot(&mut s, &mut t);
        assert!(kill_once(&mut s, &mut t).contains(&Action::Restart(0)));
        let acts = kill_once(&mut s, &mut t);
        assert!(acts.contains(&Action::Disarm(0)), "the budget runs out even for a critical entry");
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
    fn an_app_never_seen_alive_is_retried_a_bounded_number_of_times_and_then_left() {
        // Two failures look identical from here and want opposite treatment: an app whose process
        // UID3 differs from its app UID3 (alive, permanently invisible — restarting forever is the
        // bug) and an app that crashed on startup (dead, and the thing a boot supervisor exists
        // for). Retrying a bounded number of times serves the second without reopening the first.
        let cfg = cfg_of(vec![entry(1, Policy::Always)]);
        let mut s = Supervisor::new(&cfg, OWN, CTL, 0);
        let mut t = 0;
        boot(&mut s, &mut t);
        let mut restarts = 0;
        for _ in 0..40 {
            let (acts, ms) = run(&mut s, t, &[false]);
            restarts += acts.iter().filter(|a| matches!(a, Action::Restart(_))).count();
            t += ms.max(1) as u64;
        }
        assert_eq!(restarts, NEVER_UP_TRIES as usize, "tried, and then stopped trying");
        assert_eq!(
            s.snapshot().entries[0].state,
            State::Dead,
            "and says so, instead of the boot quietly settling with the app absent"
        );
    }

    #[test]
    fn an_app_that_comes_up_late_is_not_counted_against_the_never_up_budget() {
        // A slow starter must not burn the budget meant for one that never arrives.
        let cfg = cfg_of(vec![entry(1, Policy::Always)]);
        let mut s = Supervisor::new(&cfg, OWN, CTL, 0);
        let mut t = 0;
        boot(&mut s, &mut t);
        // One round where it has not appeared yet, then it does.
        t += LAUNCH_GRACE_MS + 1_000;
        run(&mut s, t, &[false]);
        t += 10_000;
        run(&mut s, t, &[true]);
        assert_eq!(s.snapshot().entries[0].state, State::Alive);
        // From here it behaves like any healthy entry: a real death is a real restart.
        assert!(kill_once(&mut s, &mut t).contains(&Action::Restart(0)));
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
    // ------------------------------------------------------- the entry an update owns

    const HOME: u32 = 0xE0AA_0000;
    const OTHER: u32 = 0x1000_0001;

    #[test]
    fn the_updated_entry_is_not_restarted_and_spends_nothing() {
        let cfg = cfg_of(vec![entry(HOME, Policy::Always)]);
        let mut sup = Supervisor::new(&cfg, OWN, CTL, 0);
        let mut t = 0;
        boot(&mut sup, &mut t);

        sup.set_updating(Some(HOME));
        let acts = kill_once(&mut sup, &mut t);
        assert!(
            acts.iter().all(|a| !matches!(a, Action::Restart(_))),
            "the updater is deliberately watching this one die; a restart underneath it turns the \
             proof window into a measurement of the supervisor"
        );
        assert_eq!(sup.snapshot().restarts_used, 0, "and nothing was spent doing it");

        // The update ends. The supervisor picks the entry straight back up, with its budget intact.
        sup.set_updating(None);
        let acts = kill_once(&mut sup, &mut t);
        assert!(acts.contains(&Action::Restart(0)), "the home comes back once the update is over");
    }

    #[test]
    fn the_rest_of_the_boot_list_stays_supervised_during_an_update() {
        // The whole reason for a per-entry exemption rather than standing the supervisor down: an
        // install can take ten minutes, and something else in the list crashing during it is real.
        let cfg = cfg_of(vec![entry(HOME, Policy::Always), entry(OTHER, Policy::Always)]);
        let mut sup = Supervisor::new(&cfg, OWN, CTL, 0);
        let mut t = 0;
        boot(&mut sup, &mut t);
        sup.set_updating(Some(HOME));

        run(&mut sup, t, &[true, true]);
        t += LAUNCH_GRACE_MS + 1_000;
        run(&mut sup, t, &[false, false]);
        t += 20_000;
        let (acts, _) = run(&mut sup, t, &[false, false]);
        assert!(acts.contains(&Action::Restart(1)), "the other app is still watched");
        assert!(!acts.contains(&Action::Restart(0)), "the updated one is not");
    }

    #[test]
    fn an_exempt_entry_is_never_auto_disarmed_by_a_crash_loop_the_updater_caused() {
        // Without the exemption this is the expensive failure: the bad build dies five times inside
        // the proof window, the budget runs out, and the home screen comes back switched off — so
        // the rollback restores a version the boot list no longer launches.
        let cfg = cfg_of(vec![entry(HOME, Policy::Times(2))]);
        let mut sup = Supervisor::new(&cfg, OWN, CTL, 0);
        let mut t = 0;
        boot(&mut sup, &mut t);
        sup.set_updating(Some(HOME));

        for _ in 0..5 {
            let acts = kill_once(&mut sup, &mut t);
            assert!(!acts.contains(&Action::Disarm(0)));
        }
        sup.set_updating(None);
        let acts = kill_once(&mut sup, &mut t);
        assert!(acts.contains(&Action::Restart(0)), "its two restarts were never spent");
    }

    #[test]
    fn an_update_in_flight_stops_the_supervisor_declaring_itself_finished() {
        // `Done` makes the daemon exit. Exiting mid-update leaves nobody to prove the new version,
        // roll it back, or resume supervising the entry when the update ends.
        let cfg = cfg_of(vec![entry(HOME, Policy::Never)]);
        let mut sup = Supervisor::new(&cfg, OWN, CTL, 0);
        let mut t = 0;
        boot(&mut sup, &mut t);
        sup.set_updating(Some(HOME));

        t += SETTLE_MS + 1_000;
        let (acts, _) = run(&mut sup, t, &[true]);
        assert!(
            !acts.contains(&Action::Done),
            "a `Never` entry would normally end the supervisor here"
        );

        sup.set_updating(None);
        t += SETTLE_MS + 1_000;
        let (acts, _) = run(&mut sup, t, &[true]);
        assert!(acts.contains(&Action::Done), "and it ends as soon as the update is over");
    }

    #[test]
    fn naming_an_application_that_is_not_in_the_boot_list_exempts_nothing() {
        // bootd does not have to know whether the package it is updating is also supervised.
        let cfg = cfg_of(vec![entry(HOME, Policy::Always)]);
        let mut sup = Supervisor::new(&cfg, OWN, CTL, 0);
        let mut t = 0;
        boot(&mut sup, &mut t);
        sup.set_updating(Some(0xDEAD_BEEF));
        assert!(kill_once(&mut sup, &mut t).contains(&Action::Restart(0)));
    }
}
