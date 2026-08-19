//! An exponential-backoff cadence for a background poller.
//!
//! A home-screen daemon (signal, unread count) only needs to poll quickly while the user is looking
//! at the home. Left alone — the launcher in the background, the screen off — it should slow down so
//! an idle phone is barely woken. This tracks that: each tick the interval doubles toward a cap, and
//! an "activity" signal from the foreground resets it to the base rate. A small jitter on reset keeps
//! several daemons resetting together from realigning into one synchronized wake.
//!
//! It holds no clock and starts no timer — the caller owns the timer and asks this only *what delay
//! to arm next*, so the whole thing is pure and host-tested. Milliseconds throughout.

/// A doubling backoff between a base and a maximum interval.
#[derive(Copy, Clone, Debug)]
pub struct Backoff {
    base: i32,
    max: i32,
    /// The interval to arm on the next request, before it doubles again.
    current: i32,
}

impl Backoff {
    /// A backoff that starts at `base_ms` and doubles toward `max_ms`. `max` is held at least `base`.
    pub const fn new(base_ms: i32, max_ms: i32) -> Self {
        let max = if max_ms < base_ms { base_ms } else { max_ms };
        Self { base: base_ms, max, current: base_ms }
    }

    /// The delay to arm after a normal tick, then double toward the cap for the tick after.
    /// The sequence from base is base, 2·base, 4·base, … capped at `max`.
    pub fn on_tick(&mut self) -> i32 {
        let delay = self.current;
        self.current = self.current.saturating_mul(2).clamp(self.base, self.max);
        delay
    }

    /// The delay to arm after a foreground activity signal: back to `base` plus `jitter_ms`, with the
    /// interval *after* that doubling from `base` (the jitter staggers peers, it does not persist).
    pub fn on_reset(&mut self, jitter_ms: i32) -> i32 {
        self.current = self.base.saturating_mul(2).clamp(self.base, self.max);
        self.base.saturating_add(jitter_ms.max(0))
    }

    /// The interval that will be armed next (before doubling). For tests and diagnostics.
    pub fn current(&self) -> i32 {
        self.current
    }
}

/// A drop-in polling loop for a home-screen daemon: a [`Backoff`] wired to a one-shot timer and the
/// launcher's foreground **activity** property, so the whole "poll fast while the user is on the
/// home, back off when idle, snap back on a foreground bump" behaviour is one object instead of
/// hand-rolled glue in every daemon.
///
/// Usage — the daemon owns one, publishes once, then [`start`](Self::start)s it, and asks
/// [`poll`](Self::poll) about every raw event:
/// ```ignore
/// struct Netd { poller: BackoffPoller }
/// // new(): define keys; let mut me = ...; me.publish(); me.poller.start(); me
/// // handle_raw(ev): if self.poller.poll(ev) { self.publish(); }
/// ```
/// It arms the next timer itself; the daemon only decides *what* to publish.
pub struct BackoffPoller {
    backoff: Backoff,
    /// The pending one-shot timer handle, or `None` before `start`/off-device.
    ticker: Option<i32>,
    /// The delay the pending timer was armed with, so [`cap_next`](Self::cap_next) knows whether it
    /// would be shortening anything. Not derivable from the backoff: `current` is the interval for
    /// the tick *after* this one.
    armed_ms: i32,
    /// The launcher activity property to follow (a change resets the cadence).
    cat: u32,
    key: u32,
    jitter_ms: i32,
}

impl BackoffPoller {
    /// Poll between `base_ms` and `max_ms` (doubling), resetting to base + up-to-`jitter_ms` when the
    /// property `(activity_cat, activity_key)` changes. The jitter staggers peers sharing one signal.
    pub const fn new(base_ms: i32, max_ms: i32, jitter_ms: i32, activity_cat: u32, activity_key: u32) -> Self {
        Self {
            backoff: Backoff::new(base_ms, max_ms),
            ticker: None,
            armed_ms: 0,
            cat: activity_cat,
            key: activity_key,
            jitter_ms,
        }
    }

    /// Subscribe to the activity property and arm the first (base-rate) timer. Call once, after the
    /// daemon's initial publish.
    pub fn start(&mut self) {
        let _ = crate::prop::subscribe(self.cat, self.key);
        let delay = self.backoff.on_tick();
        self.arm(delay);
    }

    /// Feed every raw event. Returns `true` when the daemon should publish now — a backoff timer fired
    /// or the foreground bumped the activity property — having already armed the next timer.
    pub fn poll(&mut self, ev: &symbian_sys::ShimEvent) -> bool {
        if ev.kind == symbian_sys::SHIM_EV_TIMER && Some(ev.handle) == self.ticker {
            let delay = self.backoff.on_tick();
            self.arm(delay);
            true
        } else if ev.kind == symbian_sys::SHIM_EV_PROP {
            let delay = self.backoff.on_reset(self.jitter());
            self.arm(delay);
            true
        } else {
            false
        }
    }

    /// Shorten the pending sleep to at most `ms`, without disturbing the backoff.
    ///
    /// For a daemon that has a *deadline* as well as a cadence. The calendar's reminder queue is the
    /// case this exists for: `notifd` polls the message store on a doubling interval that reaches
    /// five minutes when the phone is idle, and an idle phone is exactly when a reminder for a
    /// 14:00 meeting has to arrive at 14:00 rather than at 14:04.
    ///
    /// Only the *pending* sleep is shortened. The backoff keeps growing on its own schedule, so a
    /// daemon that caps every tick does not thereby pin itself at the base rate — the cadence is
    /// still "how often do I look", and this is "and not later than".
    ///
    /// Ignored when it would lengthen the sleep, and before [`start`](Self::start) has armed
    /// anything — a deadline is a *ceiling* on a cadence, so with no cadence yet there is nothing to
    /// cap. A non-positive `ms` arms the shortest timer the platform will take rather than being
    /// refused: "already due" is a legitimate answer from a deadline and the caller wants waking.
    ///
    /// The decision is deliberately made from `armed_ms` rather than from the timer handle, so it
    /// holds on a host where `shim_timer_after` answers `NotSupported` and there is never a handle
    /// to look at. Otherwise this method could only be tested on a phone, which for a rule about
    /// *when a reminder arrives* is the wrong place to find out it is wrong.
    pub fn cap_next(&mut self, ms: i32) {
        if self.armed_ms == 0 {
            return;
        }
        let ms = ms.max(1);
        if ms >= self.armed_ms {
            return;
        }
        self.arm(ms);
    }

    /// What the pending timer was armed with. For tests and diagnostics.
    pub fn armed(&self) -> i32 {
        self.armed_ms
    }

    /// Cancel any pending timer and arm a fresh one-shot. (One-shots free their slot when they fire,
    /// so cancelling an already-fired handle is a safe no-op.)
    fn arm(&mut self, delay: i32) {
        if let Some(t) = self.ticker.take() {
            crate::timer_cancel(t);
        }
        self.armed_ms = delay;
        self.ticker = crate::timer_after(delay).ok();
    }

    /// A small jitter from the monotonic clock; 0 when `jitter_ms` is not positive.
    fn jitter(&self) -> i32 {
        if self.jitter_ms <= 0 {
            0
        } else {
            (crate::monotonic_us() % (self.jitter_ms as u64)) as i32
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_from_base_and_caps() {
        let mut b = Backoff::new(10, 100);
        assert_eq!(b.on_tick(), 10);
        assert_eq!(b.on_tick(), 20);
        assert_eq!(b.on_tick(), 40);
        assert_eq!(b.on_tick(), 80);
        assert_eq!(b.on_tick(), 100, "capped at max");
        assert_eq!(b.on_tick(), 100, "stays at the cap");
    }

    #[test]
    fn reset_returns_to_base_plus_jitter_then_doubles_from_base() {
        let mut b = Backoff::new(10, 1000);
        // Climb a bit.
        b.on_tick();
        b.on_tick();
        b.on_tick(); // current now 80
        // A foreground reset drops back to base (+jitter), and the next tick doubles from base.
        assert_eq!(b.on_reset(3), 13, "base 10 + jitter 3");
        assert_eq!(b.on_tick(), 20, "doubling resumes from base, not from base+jitter");
        assert_eq!(b.on_tick(), 40);
    }

    #[test]
    fn negative_jitter_is_ignored() {
        let mut b = Backoff::new(10, 1000);
        assert_eq!(b.on_reset(-5), 10);
    }

    #[test]
    fn a_max_below_base_is_clamped_up() {
        let mut b = Backoff::new(50, 10);
        assert_eq!(b.on_tick(), 50);
        assert_eq!(b.on_tick(), 50, "never below base");
    }

    fn ev(kind: i32) -> symbian_sys::ShimEvent {
        symbian_sys::ShimEvent { kind, handle: 0, status: 0, a: 0, b: 0, c: 0, d: 0, native: 0 }
    }

    #[test]
    fn a_deadline_shortens_the_pending_sleep_and_never_lengthens_it() {
        // The reminder case: notifd's idle cadence reaches five minutes, and a meeting at 14:00
        // has to be announced at 14:00 rather than at 14:04.
        let mut p = BackoffPoller::new(15_000, 300_000, 0, 1, 2);
        p.start();
        assert_eq!(p.armed(), 15_000);
        p.cap_next(4_000);
        assert_eq!(p.armed(), 4_000);
        // A later deadline is not a reason to sleep longer.
        p.cap_next(60_000);
        assert_eq!(p.armed(), 4_000);
    }

    #[test]
    fn a_deadline_does_not_pin_the_cadence_at_the_base_rate() {
        // Only the pending sleep is shortened. If capping also reset the backoff, a calendar with a
        // reminder every hour would hold the daemon at its fast rate all day.
        let mut p = BackoffPoller::new(15_000, 300_000, 0, 1, 2);
        p.start();
        p.cap_next(1_000);
        // The tick after is still the one the backoff had planned.
        assert_eq!(p.backoff.current(), 30_000);
    }

    #[test]
    fn a_deadline_already_past_wakes_as_soon_as_the_platform_allows() {
        // "Already due" is a legitimate answer — the phone was off through the reminder — and the
        // daemon has to be woken to deliver it rather than told to wait.
        let mut p = BackoffPoller::new(15_000, 300_000, 0, 1, 2);
        p.start();
        p.cap_next(-5);
        assert_eq!(p.armed(), 1);
    }

    #[test]
    fn a_deadline_before_the_cadence_starts_is_ignored() {
        // A ceiling on a cadence that does not exist yet. Arming here would start a timer the
        // daemon never asked for, before its first publish.
        let mut p = BackoffPoller::new(15_000, 300_000, 0, 1, 2);
        p.cap_next(1_000);
        assert_eq!(p.armed(), 0);
    }

    #[test]
    fn poller_publishes_on_activity_and_ignores_unrelated_events() {
        // Off-device the timer is a no-op (no handle), but the reset and filter logic still runs.
        let mut p = BackoffPoller::new(10, 100, 0, 0xE0AA_0000, 100);
        p.start();
        // A property change (the launcher's activity bump) always asks for a publish.
        assert!(p.poll(&ev(symbian_sys::SHIM_EV_PROP)));
        // An unrelated event does not.
        assert!(!p.poll(&ev(12345)));
    }
}
