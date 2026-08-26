//! Where the handset is, through the platform's Location Acquisition API.
//!
//! The shim wraps `RPositionServer` and `RPositioner`; this is the bookkeeping between asking for
//! a position and having one. See `shim/src/shim_lbs.cpp` for why every route to a fix is an event
//! and none of them blocks.
//!
//! # A fix is slow, and that is the whole design constraint
//!
//! A cold GPS start on this class of handset is measured in minutes, not milliseconds — the module
//! inventory reports its own figure, and [`Module::time_to_first_fix_ms`] is the number an app
//! should believe over its own optimism. So nothing here waits for a position: an app draws
//! whatever it can draw without one, and the fix arrives later as `SHIM_EV_GPS_FIX` like any other
//! completion.
//!
//! # Not knowing is a value
//!
//! [`Fix`] carries `Option` for everything a module may decline to report, and [`Watch::fix`]
//! returns `None` before the first completion. That is deliberate: latitude 0, longitude 0 is a
//! real place in the Gulf of Guinea, and a map that draws the user there because the API had no
//! way to say "not yet" is worse than a map that draws nothing.

use alloc::string::String;

use symbian_sys as sys;

use crate::error::{Error, Result};
use crate::net::RawEvent;

/// One position report.
///
/// Latitude and longitude are degrees, positive north and east — the same convention as every web
/// mapping service, so no sign flipping is needed anywhere downstream.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Fix {
    pub lat: f64,
    pub lon: f64,
    /// Metres above the WGS-84 ellipsoid, when the module reports it.
    pub altitude_m: Option<f64>,
    /// The radius, in metres, the module is willing to stand behind. Drawing this as a circle is
    /// the difference between a map that says where you are and one that says where you might be.
    pub accuracy_m: Option<f64>,
    pub vertical_accuracy_m: Option<f64>,
    /// Satellites contributing to this fix. `None` when satellite info was not requested, which is
    /// a different fact from zero satellites and must not be shown as one.
    pub satellites_used: Option<i32>,
    pub satellites_in_view: Option<i32>,
}

/// One positioning module the framework knows about.
///
/// Worth reading before starting anything: a handset with only a network module answers in seconds
/// with kilometres of error, and one with an integrated GPS answers in minutes with metres. Those
/// are different applications, and the difference is knowable up front.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Module {
    pub name: String,
    pub uid: i32,
    /// Whether it can be used *now* — an external Bluetooth receiver that is not connected is
    /// known to the framework and unavailable.
    pub available: bool,
    /// `TTechnologyType`, and a **bitmask** rather than an enum of values: 1 terminal (the
    /// handset's own receiver), 2 network (a cell tower, so no satellites exist to report), 4
    /// assisted (a receiver the network helps to a faster fix). A module may carry more than one.
    ///
    /// Measured on the E72: `Assisted GPS` reports 4 and `Network based` reports 2 — which is the
    /// opposite of the obvious guess, and inverting them silently picks the wrong module.
    pub technology: i32,
    /// 1 internal to the device, 2 external.
    pub device_location: i32,
    pub cost: i32,
    pub power: i32,
    pub horizontal_accuracy_mm: i32,
    pub vertical_accuracy_mm: i32,
    pub time_to_first_fix_ms: i32,
    pub time_to_next_fix_ms: i32,
}

/// What the shim can do with position. One implementation over the shim, one in memory for tests —
/// the same split as [`crate::net::Net`] and [`crate::image::Images`], and for the same reason: the
/// logic worth testing is the sequencing, not the FFI call.
pub trait Location {
    /// Subscribe. `interval_ms` 0 asks for a single fix; anything positive is a stream at that
    /// cadence. `timeout_ms` 0 lets the module take as long as it takes.
    ///
    /// `module_uid` 0 lets the framework choose; a [`Module::uid`] picks that one and no other.
    /// The modules are not interchangeable — one answers in 12 s to 200 m and another in 80 s to
    /// 10 m — so which to ask is a decision, and only one can be asked at a time.
    fn start(
        &mut self,
        interval_ms: i32,
        timeout_ms: i32,
        want_satellites: bool,
        module_uid: i32,
    ) -> Result<()>;
    fn stop(&mut self);
    /// The last completed update. `Err(Error::NotReady)` before the first one, and the platform's
    /// error when the last update failed — never a stale fix presented as a fresh one.
    fn read(&mut self) -> Result<Fix>;
    fn module_count(&mut self) -> Result<i32>;
    fn module(&mut self, index: i32) -> Result<Module>;
}

/// [`Location`] over the shim.
#[derive(Copy, Clone, Debug, Default)]
pub struct ShimLocation;

impl Location for ShimLocation {
    fn start(
        &mut self,
        interval_ms: i32,
        timeout_ms: i32,
        want_satellites: bool,
        module_uid: i32,
    ) -> Result<()> {
        Error::check(unsafe {
            sys::shim_gps_start(interval_ms, timeout_ms, i32::from(want_satellites), module_uid)
        })
        .map(|_| ())
    }

    fn stop(&mut self) {
        unsafe { sys::shim_gps_stop() }
    }

    fn read(&mut self) -> Result<Fix> {
        let mut lat = 0.0f64;
        let mut lon = 0.0f64;
        let mut alt = 0.0f64;
        let mut h_acc = 0.0f64;
        let mut v_acc = 0.0f64;
        let mut sats = -1i32;
        let mut in_view = -1i32;
        Error::check(unsafe {
            sys::shim_gps_read(
                &mut lat,
                &mut lon,
                &mut alt,
                &mut h_acc,
                &mut v_acc,
                &mut sats,
                &mut in_view,
            )
        })?;
        Ok(Fix {
            lat,
            lon,
            altitude_m: finite(alt),
            accuracy_m: finite(h_acc),
            vertical_accuracy_m: finite(v_acc),
            satellites_used: (sats >= 0).then_some(sats),
            satellites_in_view: (in_view >= 0).then_some(in_view),
        })
    }

    fn module_count(&mut self) -> Result<i32> {
        let mut n = 0i32;
        Error::check(unsafe { sys::shim_gps_module_count(&mut n) })?;
        Ok(n)
    }

    fn module(&mut self, index: i32) -> Result<Module> {
        // 64 units is KPositionMaxModuleName, so this never truncates — but the overflow code is
        // still honoured below rather than assumed away, because the constant is the platform's
        // and this side does not get to decide it stayed the same.
        let mut name = [0u16; 64];
        let mut name_len = 0i32;
        let mut out = [0i32; 10];
        let rc = unsafe {
            sys::shim_gps_module_info(
                index,
                name.as_mut_ptr(),
                name.len() as i32,
                &mut name_len,
                out.as_mut_ptr(),
                out.len() as i32,
            )
        };
        // A cut name is still a module. Report it under the name that fit rather than losing the
        // whole entry over its label.
        if rc != sys::SHIM_OK && rc != sys::SHIM_ERR_OVERFLOW {
            return Err(Error::from_code(rc));
        }
        let len = name_len.clamp(0, name.len() as i32) as usize;
        Ok(Module {
            name: String::from_utf16_lossy(&name[..len]),
            uid: out[0],
            available: out[1] != 0,
            technology: out[2],
            device_location: out[3],
            cost: out[4],
            power: out[5],
            horizontal_accuracy_mm: out[6],
            vertical_accuracy_mm: out[7],
            time_to_first_fix_ms: out[8],
            time_to_next_fix_ms: out[9],
        })
    }
}

/// NaN means "the module did not report this", which is what the platform hands back for an
/// unreported `TReal32`. Infinities are refused for the same reason: neither is a distance.
fn finite(v: f64) -> Option<f64> {
    if v.is_finite() {
        Some(v)
    } else {
        None
    }
}

/// The serving cell tower, as the modem names it.
///
/// Four numbers that a public tower database turns into a place. Not a position by themselves and
/// not a secret either — they identify an antenna that serves a neighbourhood, which is exactly the
/// resolution a map needs to open in the right city.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Cell {
    /// Mobile country code. 724 is Brazil.
    pub mcc: i32,
    /// Mobile network code — the operator within that country.
    pub mnc: i32,
    pub lac: i32,
    pub cell_id: i32,
    /// The modem's own flag. When false, `lac` and `cell_id` are whatever was in the struct and
    /// mean nothing — a lookup built from them would be a confident answer about the wrong place.
    pub area_known: bool,
}

/// Reading the serving cell. One implementation over the shim, one in memory for tests — the same
/// split as every other trait here.
pub trait Cells {
    /// Ask. The completion arrives as `SHIM_EV_CELL`; nothing blocks.
    fn read(&mut self) -> Result<()>;
    /// The last completed read. `Err(Error::NotReady)` before the first one.
    fn get(&mut self) -> Result<Cell>;
    fn stop(&mut self);
}

/// [`Cells`] over the shim.
#[derive(Copy, Clone, Debug, Default)]
pub struct ShimCells;

impl Cells for ShimCells {
    fn read(&mut self) -> Result<()> {
        Error::check(unsafe { sys::shim_cell_read() }).map(|_| ())
    }

    fn get(&mut self) -> Result<Cell> {
        let mut mcc = 0i32;
        let mut mnc = 0i32;
        let mut lac = 0i32;
        let mut cid = 0i32;
        let mut ak = 0i32;
        Error::check(unsafe {
            sys::shim_cell_get(&mut mcc, &mut mnc, &mut lac, &mut cid, &mut ak)
        })?;
        Ok(Cell { mcc, mnc, lac, cell_id: cid, area_known: ak != 0 })
    }

    fn stop(&mut self) {
        unsafe { sys::shim_cell_stop() }
    }
}

/// A scripted [`Cells`] for tests.
pub struct MemCells {
    pub queued: alloc::vec::Vec<Result<Cell>>,
    pub reads: u32,
}

impl MemCells {
    pub fn new(queued: alloc::vec::Vec<Result<Cell>>) -> Self {
        Self { queued, reads: 0 }
    }
}

impl Cells for MemCells {
    fn read(&mut self) -> Result<()> {
        self.reads += 1;
        Ok(())
    }

    fn get(&mut self) -> Result<Cell> {
        if self.queued.is_empty() {
            return Err(Error::NotReady);
        }
        self.queued.remove(0)
    }

    fn stop(&mut self) {}
}

/// A subscription, and the last thing it said.
///
/// The whole point is that [`Self::fix`] is `None` until a real fix arrives, so an app cannot
/// accidentally draw a position it does not have. Feed every event to [`Self::on_event`]; it
/// ignores the ones that are not its own, so it composes with the app's other subsystems the way
/// [`crate::net::TcpStream`] does.
pub struct Watch<L: Location> {
    location: L,
    running: bool,
    /// The module the live subscription was opened on. What lets `switch_to` tell "change to the
    /// precise receiver" from "ask again for the one already running".
    module: i32,
    fix: Option<Fix>,
    /// The status of the most recent completion, `None` before any. Kept separately from the fix
    /// because "we had a fix and then the tunnel" is a state a status bar should be able to show,
    /// and it is not the same as "we never had one".
    last_status: Option<i32>,
}

impl<L: Location> Watch<L> {
    /// Not started. Nothing happens until [`Self::start`] — a constructor that powered up the GPS
    /// would make an app pay for a subsystem it may only offer as a menu item.
    pub fn new(location: L) -> Self {
        Self { location, running: false, module: 0, fix: None, last_status: None }
    }

    pub fn start(
        &mut self,
        interval_ms: i32,
        timeout_ms: i32,
        want_satellites: bool,
        module_uid: i32,
    ) -> Result<()> {
        if self.running {
            return Ok(());
        }
        self.location.start(interval_ms, timeout_ms, want_satellites, module_uid)?;
        self.running = true;
        self.module = module_uid;
        Ok(())
    }

    /// Stop whatever is running and subscribe to a different module.
    ///
    /// Its own method because the framework allows one subscription at a time, so "switch to the
    /// precise one now that the map is open" is a stop and a start — and a caller that wrote those
    /// two lines itself would sooner or later write only the second and get `AlreadyExists`.
    ///
    /// # Asking for what is already running does nothing, and that is the point
    ///
    /// A stop cancels the outstanding `NotifyPositionUpdate`, and a receiver part-way through a
    /// cold start loses the wait it had already served. Measured on the E72: a user who pressed
    /// "follow" four times because nothing seemed to be happening restarted the request four
    /// times, which is the one action guaranteed to make nothing happen. So a switch to the module
    /// already subscribed is a no-op rather than a restart.
    ///
    /// The comparison is on the module alone. Changing the cadence or the timeout of a live
    /// subscription is not something this type can do without a restart, and pretending otherwise
    /// would be worse than the restart.
    pub fn switch_to(
        &mut self,
        interval_ms: i32,
        timeout_ms: i32,
        want_satellites: bool,
        module_uid: i32,
    ) -> Result<()> {
        if self.running && self.module == module_uid {
            return Ok(());
        }
        self.stop();
        self.start(interval_ms, timeout_ms, want_satellites, module_uid)
    }

    /// The module the live subscription is on, or 0 when nothing is running.
    pub fn module(&self) -> i32 {
        if self.running {
            self.module
        } else {
            0
        }
    }

    pub fn stop(&mut self) {
        if self.running {
            self.location.stop();
            self.running = false;
            self.module = 0;
        }
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    /// The underlying [`Location`], for the calls that are not about a subscription — the module
    /// inventory, in practice. Exposed rather than mirrored here because a `Watch` that grew a
    /// `module_count` would be pretending the inventory has something to do with watching.
    pub fn location_mut(&mut self) -> &mut L {
        &mut self.location
    }

    /// The last good fix, which outlives the error that followed it. A map keeps drawing the last
    /// known position while the sky is gone; it just stops calling it current.
    pub fn fix(&self) -> Option<&Fix> {
        self.fix.as_ref()
    }

    /// The completion code of the most recent update: `Some(0)` after a fix, `Some(err)` after a
    /// failure, `None` before anything has completed.
    pub fn last_status(&self) -> Option<i32> {
        self.last_status
    }

    /// Feed a platform event. Returns true when this was a position update — whether or not it
    /// carried a fix — which is what tells a caller to repaint.
    pub fn on_event(&mut self, ev: &RawEvent) -> bool {
        if ev.kind != sys::SHIM_EV_GPS_FIX {
            return false;
        }
        self.last_status = Some(ev.status);
        if ev.status == sys::SHIM_OK {
            if let Ok(fix) = self.location.read() {
                self.fix = Some(fix);
            }
        }
        true
    }
}

impl<L: Location> Drop for Watch<L> {
    /// A subscription that outlives its owner keeps the GPS powered on the server's side. The shim
    /// cleans up at process exit too; this is for the app that opens a map screen and closes it.
    fn drop(&mut self) {
        self.stop();
    }
}

impl Default for Watch<ShimLocation> {
    fn default() -> Self {
        Self::new(ShimLocation)
    }
}

/// A scripted [`Location`] for tests: a queue of results, handed out one per `read`.
pub struct MemLocation {
    pub queued: alloc::vec::Vec<Result<Fix>>,
    pub modules: alloc::vec::Vec<Module>,
    pub started: bool,
    /// How many times `start` was called. What lets a test assert that a live subscription was
    /// left alone rather than torn down and rebuilt.
    pub starts: u32,
    /// The module the last `start` asked for. What lets a test assert that the coarse module was
    /// chosen for the coarse question.
    pub last_module: i32,
}

impl MemLocation {
    pub fn new(queued: alloc::vec::Vec<Result<Fix>>) -> Self {
        Self { queued, modules: alloc::vec::Vec::new(), started: false, starts: 0, last_module: 0 }
    }
}

impl Location for MemLocation {
    fn start(
        &mut self,
        _interval_ms: i32,
        _timeout_ms: i32,
        _want_satellites: bool,
        module_uid: i32,
    ) -> Result<()> {
        self.started = true;
        self.starts += 1;
        self.last_module = module_uid;
        Ok(())
    }

    fn stop(&mut self) {
        self.started = false;
    }

    fn read(&mut self) -> Result<Fix> {
        if self.queued.is_empty() {
            return Err(Error::NotReady);
        }
        self.queued.remove(0)
    }

    fn module_count(&mut self) -> Result<i32> {
        Ok(self.modules.len() as i32)
    }

    fn module(&mut self, index: i32) -> Result<Module> {
        self.modules.get(index as usize).cloned().ok_or(Error::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn fix_at(lat: f64, lon: f64) -> Fix {
        Fix { lat, lon, accuracy_m: Some(12.0), ..Fix::default() }
    }

    fn ev(status: i32) -> RawEvent {
        RawEvent { kind: sys::SHIM_EV_GPS_FIX, status, ..Default::default() }
    }

    #[test]
    fn no_fix_until_one_arrives() {
        let w = Watch::new(MemLocation::new(vec![]));
        assert!(w.fix().is_none());
        assert!(w.last_status().is_none());
    }

    #[test]
    fn a_fix_is_kept_and_an_error_does_not_erase_it() {
        let mut w = Watch::new(MemLocation::new(vec![Ok(fix_at(-8.05, -34.9))]));
        assert!(w.on_event(&ev(sys::SHIM_OK)));
        assert_eq!(w.fix().map(|f| f.lat), Some(-8.05));

        // The tunnel. The position is stale, not gone — and the status says which.
        assert!(w.on_event(&ev(sys::SHIM_ERR_TIMED_OUT)));
        assert_eq!(w.fix().map(|f| f.lat), Some(-8.05));
        assert_eq!(w.last_status(), Some(sys::SHIM_ERR_TIMED_OUT));
    }

    #[test]
    fn other_events_are_not_ours() {
        let mut w = Watch::new(MemLocation::new(vec![Ok(fix_at(0.0, 0.0))]));
        let other = RawEvent { kind: sys::SHIM_EV_TIMER, ..Default::default() };
        assert!(!w.on_event(&other));
        assert!(w.fix().is_none());
    }


    #[test]
    fn asking_again_for_the_running_module_does_not_restart_it() {
        let mut w = Watch::new(MemLocation::new(vec![Ok(fix_at(0.0, 0.0))]));
        w.start(1000, 0, true, 0x101f_e98a).unwrap();
        w.location_mut().starts = 0;

        // The four presses that made the E72 restart a cold start four times.
        for _ in 0..4 {
            w.switch_to(1000, 0, true, 0x101f_e98a).unwrap();
        }
        assert_eq!(w.location_mut().starts, 0, "a live subscription was restarted");
    }

    #[test]
    fn switching_to_a_different_module_does_restart() {
        let mut w = Watch::new(MemLocation::new(vec![Ok(fix_at(0.0, 0.0))]));
        w.start(0, 0, false, 0x1020_6915).unwrap();
        w.location_mut().starts = 0;
        w.switch_to(1000, 0, true, 0x101f_e98a).unwrap();
        assert_eq!(w.location_mut().starts, 1);
        assert_eq!(w.module(), 0x101f_e98a);
    }

    #[test]
    fn unreported_fields_are_none_not_zero() {
        assert_eq!(finite(f64::NAN), None);
        assert_eq!(finite(f64::INFINITY), None);
        assert_eq!(finite(0.0), Some(0.0));
    }
}
