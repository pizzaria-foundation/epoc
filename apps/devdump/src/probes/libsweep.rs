//! Which of the SDK's import libraries actually load on this handset.
//!
//! This is the master key of the whole run. Every future "can we link that?" decision —
//! an MTM, telephony, TLS, the location framework — turns on whether the DLL behind it is
//! present, and the alternative to asking is finding out when the E32 loader silently
//! refuses an image and the app does nothing at all.
//!
//! Imports `euser` and `efsrv` and nothing else, which is not a preference: this is the
//! binary that discovers what is safe to import, so it cannot itself depend on the answer.

use alloc::string::String;

use symbian::fs::ShimFs;
use symbian_report::{push_i64, Report};
use symbian_sys as sys;

use crate::dlls;

pub fn run(r: &mut Report, fs: &mut ShimFs) {
    r.entering(fs, "controls");
    let controls_ok = controls(r);
    r.flush(fs);

    if !controls_ok {
        // Without a working control the sweep cannot distinguish a bare handset from a
        // broken query, and a page of "absent" would read as a devastating finding rather
        // than as a bug in the instrument. Say so instead of producing it.
        r.head("sweep");
        r.check_note(
            "sweep abandoned",
            false,
            "a control DLL did not load, so RLibrary::Load is not answering — every result \
             below would be meaningless",
        );
        return;
    }

    r.entering(fs, "notable");
    notable(r);
    r.flush(fs);

    r.entering(fs, "sweep");
    sweep(r, fs);
}

/// `RLibrary::Load`, not a filesystem check: a DLL can be present and still fail to load
/// through a wrong UID, its own unsatisfied imports, or a capability we do not hold — and
/// each of those breaks an import exactly as thoroughly as the file being absent.
fn present(name: &str) -> i32 {
    // Names are ASCII and short; a fixed buffer avoids an allocation per probe over
    // several hundred names.
    let mut buf = [0u16; 64];
    let mut n = 0;
    for b in name.bytes() {
        if n == buf.len() {
            return sys::SHIM_ERR_OVERFLOW;
        }
        buf[n] = b as u16;
        n += 1;
    }
    // SAFETY: `buf` is valid for `n` units and only read.
    unsafe { sys::shim_dll_present(buf.as_ptr(), n as i32) }
}

fn controls(r: &mut Report) -> bool {
    r.head("controls");
    let mut all = true;
    for name in dlls::CONTROLS {
        let rc = present(name);
        let ok = rc == 0;
        all &= ok;
        r.check_note(name, ok, &rc_note(rc));
    }
    all
}

fn notable(r: &mut Report) {
    r.head("notable");
    // The same names appear again in the full sweep below. Repeating them here, annotated
    // with the decision each one is holding up, is the difference between a reader finding
    // the answer and a reader having to already know which of three hundred lines matters.
    for (name, why) in dlls::NOTABLE {
        let rc = present(name);
        let mut note = String::from(if rc == 0 { "present" } else { "ABSENT" });
        note.push_str("  [");
        push_i64(&mut note, rc as i64);
        note.push_str("]  ");
        note.push_str(why);
        r.info(name, &note);
    }
}

fn sweep(r: &mut Report, fs: &mut ShimFs) {
    r.head("sweep");
    let mut found = 0i64;
    for (i, name) in dlls::NAMES.iter().enumerate() {
        let rc = present(name);
        if rc == 0 {
            found += 1;
        }
        r.info(name, &rc_note(rc));
        // Flushed periodically rather than per name — several hundred rewrites of a
        // growing file is the one place in this design where the flush-always discipline
        // would cost real time. Every fifty is still fine enough that a crash names a
        // neighbourhood.
        if i % 50 == 49 {
            r.flush(fs);
        }
    }
    // Names the SDK ships no import library for. Asked separately so the report says which
    // answers came from the SDK's own list and which from the gap around it — a reader
    // deciding what to link cares about that difference.
    r.head("beyond the SDK");
    r.info(
        "why",
        "these have no .dso in this SDK, so nothing could link them today - but the handset          may carry them, and RLibrary::Load is how that gets asked",
    );
    for name in dlls::EXTRA {
        let rc = present(name);
        if rc == 0 {
            found += 1;
        }
        r.info(name, &rc_note(rc));
    }

    let asked = (dlls::NAMES.len() + dlls::EXTRA.len()) as i64;
    r.head("sweep summary");
    r.num("asked", asked);
    r.num("loaded", found);
    r.num("absent", asked - found);
}

fn rc_note(rc: i32) -> String {
    let mut s = String::from(if rc == 0 { "present" } else { "absent" });
    s.push_str("  [");
    push_i64(&mut s, rc as i64);
    s.push(']');
    s
}
