//! What the ROM patch actually granted — asked twice, because one answer is not enough.
//!
//! `RProcess::HasCapability` reports what the loader *stamped on this image*. That is
//! worth knowing on a patched handset: it says whether the patch lifted the ceiling or
//! merely stopped refusing the package.
//!
//! But it is not the question anyone means. "Can this process read another application's
//! data cage" is answered by trying it, and the two answers can disagree. A kernel that
//! says the capability is held while `RFs::Att` still returns `KErrPermissionDenied` means
//! something other than platform security is refusing — a fact neither column produces
//! alone.
//!
//! This binary declares **every** capability in its `app.conf`. On a stock handset that
//! would make the package uninstallable; here it is the experiment.

use alloc::string::String;

use symbian::caps;
use symbian::fs::ShimFs;
use symbian_report::{push_i64, Report};

pub fn run(r: &mut Report, fs: &mut ShimFs) {
    r.entering(fs, "granted");
    granted(r);
    r.flush(fs);

    r.entering(fs, "attempted");
    attempted(r);
}

fn granted(r: &mut Report) {
    r.head("granted (RProcess::HasCapability)");
    for cap in caps::ALL {
        match caps::has(cap.id) {
            Ok(held) => r.check_note(cap.name, held, if held { "held" } else { "NOT held" }),
            Err(e) => r.check_note(cap.name, false, &{
                let mut s = String::from("query failed, err ");
                push_i64(&mut s, code(e) as i64);
                s
            }),
        }
    }
}

fn attempted(r: &mut Report) {
    r.head("attempted (RFs::Att, which creates and destroys nothing)");
    for at in caps::ATTEMPTS {
        let mut note = String::new();
        // The kernel's answer for the capability this path depends on, so the two columns
        // sit on one line and a divergence is visible without cross-referencing.
        let held = caps::ALL
            .iter()
            .find(|c| c.name == at.cap)
            .and_then(|c| caps::has(c.id).ok());

        let outcome = caps::attempt(at.path);
        let ok = outcome.is_ok();
        note.push_str(match &outcome {
            Ok(_) => "reachable",
            Err(symbian::Error::AccessDenied) => "REFUSED by platform security",
            // Not a refusal, and must not be read as one: the path simply does not exist
            // on this handset, which says nothing about capabilities.
            Err(symbian::Error::NotFound) | Err(symbian::Error::PathNotFound) => "path absent",
            Err(_) => "error",
        });
        note.push_str("  [");
        push_i64(&mut note, outcome.map(|_| 0).unwrap_or_else(code) as i64);
        note.push_str("]  needs ");
        note.push_str(at.cap);
        note.push_str(", kernel says ");
        note.push_str(match held {
            Some(true) => "held",
            Some(false) => "NOT held",
            None => "unknown",
        });
        note.push_str("  - ");
        note.push_str(at.what);

        // The verdict column is deliberately "did it succeed", not "did it match the
        // kernel". A mismatch is the finding, and encoding an expectation here would make
        // the probe grade the handset against a guess.
        r.check_note(at.path, ok, &note);
    }

    r.head("divergence");
    let mut diverged = 0i64;
    for at in caps::ATTEMPTS {
        let held = caps::ALL.iter().find(|c| c.name == at.cap).and_then(|c| caps::has(c.id).ok());
        let refused = matches!(caps::attempt(at.path), Err(symbian::Error::AccessDenied));
        if held == Some(true) && refused {
            diverged += 1;
            r.check_note(at.path, false, "capability held, operation still refused");
        }
    }
    r.num("paths where the two answers disagree", diverged);
}

fn code(e: symbian::Error) -> i32 {
    match e {
        symbian::Error::Platform(c) => c,
        symbian::Error::NotFound => -1,
        symbian::Error::PathNotFound => -12,
        symbian::Error::AccessDenied => -46,
        symbian::Error::NotReady => -18,
        symbian::Error::Argument => -6,
        _ => -2,
    }
}
