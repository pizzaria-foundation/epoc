//! What this handset is: HAL inventory, drives and volumes, screen, clock, memory.
//!
//! Imports nothing risky — `euser`, `efsrv` and `hal`, all of which the SDK's own apps
//! have linked and run. So this probe should always produce a section, and if it does not,
//! the fault is the harness rather than the handset.
//!
//! The screen is **not** here, and not by oversight: `shim_screen_size` and friends live in
//! `shim_gfx.cpp`, which a headless build does not compile, and linking the window server
//! into a probe that never draws would add imports for nothing. The launcher is a GUI app
//! and already has them, so it reports the screen in its own section.
//!
//! Deliberately does **not** link `sysutil.dso` for the firmware string. That library is
//! unproven on this device, and one unproven import would put the whole inventory at risk
//! of vanishing. The DLL sweep asks whether it is there; if it is, a later run can afford
//! to link it.

use alloc::string::String;

use symbian::fs::ShimFs;
use symbian::{caps, hal, vol};
use symbian_report::{push_hex, push_i64, Report};
use symbian_sys as sys;

pub fn run(r: &mut Report, fs: &mut ShimFs) {
    r.entering(fs, "hal");
    hal_inventory(r);
    r.flush(fs);

    r.entering(fs, "drives");
    drives(r);
    r.flush(fs);

    r.entering(fs, "clock");
    clock(r);
    r.flush(fs);

    r.entering(fs, "memory");
    memory(r);
    r.flush(fs);

    r.entering(fs, "identity");
    identity(r);
}

fn hal_inventory(r: &mut Report) {
    r.head("HAL");
    // KErrNotSupported is recorded rather than skipped: an attribute the handset does not
    // implement is a statement about the hardware, and a sweep that dropped those would
    // describe a device with no gaps in it.
    for at in hal::INVENTORY {
        match hal::get(at.id) {
            Ok(v) => r.num(at.name, v as i64),
            Err(symbian::Error::Platform(-5)) => r.info(at.name, "not supported"),
            Err(e) => {
                let mut s = String::from("error ");
                push_i64(&mut s, code(e) as i64);
                r.info(at.name, &s);
            }
        }
    }
}

fn drives(r: &mut Report) {
    r.head("drives");
    let mask = match vol::list() {
        Ok(m) => m,
        Err(e) => {
            r.check_note("RFs::DriveList", false, &err(e));
            return;
        }
    };
    r.check("RFs::DriveList", true);

    for n in vol::present(mask) {
        let mut key = String::new();
        key.push(vol::letter(n));
        key.push(':');

        let mut line = String::new();
        match vol::drive(n) {
            Ok(d) => {
                line.push_str(vol::media::name(d.media_type));
                if d.drive_att & vol::drive_att::REMOVABLE != 0 {
                    line.push_str(", removable");
                }
                if d.drive_att & vol::drive_att::ROM != 0 {
                    line.push_str(", rom");
                }
                if d.media_att & vol::media_att::WRITE_PROTECTED != 0 {
                    line.push_str(", write-protected");
                }
                if d.media_att & vol::media_att::LOCKED != 0 {
                    line.push_str(", locked");
                }
            }
            Err(e) => {
                line.push_str("RFs::Drive ");
                line.push_str(&err(e));
            }
        }

        match vol::volume(n) {
            Ok(v) => {
                line.push_str("  free ");
                push_i64(&mut line, v.free / 1024);
                line.push_str(" KB of ");
                push_i64(&mut line, v.size / 1024);
                line.push_str(" KB  id ");
                push_hex(&mut line, v.unique_id, 8);
                let name = vol::volume_name(&v);
                if !name.is_empty() {
                    line.push_str("  \"");
                    for u in name {
                        if let Some(c) = char::from_u32(*u as u32) {
                            line.push(c);
                        }
                    }
                    line.push('"');
                }
            }
            // A present drive with nothing mounted is what an empty card slot looks like.
            // It is a finding about the handset, not a failure of the call, and rendering
            // it as "size 0" would put a number in the report the device never said.
            Err(symbian::Error::NotReady) => line.push_str("  (no volume mounted)"),
            Err(e) => {
                line.push_str("  RFs::Volume ");
                line.push_str(&err(e));
            }
        }
        r.info(&key, &line);
    }
}

fn clock(r: &mut Report) {
    r.head("clock");
    let t = symbian::unix_time();
    // Anything before 2020 means the clock is unset, which invalidates every timestamp in
    // every other section — worth a FAIL rather than a quiet number.
    r.check_note("wall clock is set", t > 1_577_836_800, &{
        let mut s = String::from("unix ");
        push_i64(&mut s, t);
        s
    });
    r.num("utc offset (s)", symbian::utc_offset() as i64);

    let a = symbian::monotonic_us();
    let b = symbian::monotonic_us();
    r.check_note("monotonic clock advances or holds", b >= a, &{
        let mut s = String::new();
        push_i64(&mut s, (b - a) as i64);
        s.push_str(" us between two reads");
        s
    });
}

fn memory(r: &mut Report) {
    r.head("memory");
    // SAFETY: no pointers.
    let free = unsafe { sys::shim_mem_free_kb() };
    let total = unsafe { sys::shim_mem_total_kb() };
    let heap = unsafe { sys::shim_heap_used_kb() };
    r.num("free RAM (KB)", free as i64);
    r.num("total RAM (KB)", total as i64);
    r.num("this process's heap (KB)", heap as i64);
}

fn identity(r: &mut Report) {
    r.head("identity");
    // SAFETY: no pointers.
    let uid3 = unsafe { sys::shim_own_uid3() };
    let mut s = String::from("0x");
    push_hex(&mut s, uid3, 8);
    r.info("this probe's UID3", &s);

    // A control for the capability probe that runs in its own binary: reading the user's
    // own directory must work everywhere, and if it does not then the instrument is at
    // fault rather than platform security.
    r.check_note("C:\\Data\\ is reachable", caps::attempt("C:\\Data\\").is_ok(), "control");
}

fn code(e: symbian::Error) -> i32 {
    match e {
        symbian::Error::Platform(c) => c,
        symbian::Error::NotFound => -1,
        symbian::Error::AccessDenied => -46,
        symbian::Error::NotReady => -18,
        _ => -2,
    }
}

fn code_note(rc: i32) -> String {
    let mut s = String::from("err ");
    push_i64(&mut s, rc as i64);
    s
}

fn err(e: symbian::Error) -> String {
    code_note(code(e))
}
