//! Loading our own polymorphic DLL, and calling through ordinal 1.
//!
//! The last unknown in the DLL track. `tools/e32dump.py --expect-dll` already refuses, on
//! the host, an image that is not marked as a DLL, has the wrong UID1, exports nothing, or
//! carries writable static data. What no host check can reach is whether the *handset's*
//! loader accepts the image and whether `RLibrary::Lookup(1)` hands back something
//! callable.
//!
//! It loads the DLL at runtime rather than linking it, which is what keeps this question
//! off every other binary's critical path: a DLL that will not load costs this section and
//! nothing else.

use alloc::string::String;

use symbian::fs::ShimFs;
use symbian_report::{push_hex, push_i64, Report};
use symbian_sys as sys;

/// Must match `apps/dlltest/inc/dlltest.h`.
const MAGIC: u32 = 0x5A1234A5;
/// Arbitrary, and that is the point: it is echoed back, so a callee that merely returned a
/// plausible constant would not match.
const ARG: u32 = 0xC0FFEE01;
const DLL: &str = "dlltest.dll";
/// `KDynamicLibraryUid`.
const UID1_DLL: u32 = 0x10000079;

pub fn run(r: &mut Report, fs: &mut ShimFs) {
    r.entering(fs, "RLibrary::Load");

    let mut buf = [0u16; 32];
    let mut n = 0;
    for b in DLL.bytes() {
        buf[n] = b as u16;
        n += 1;
    }
    let mut probe = sys::ShimDllProbe::default();
    // SAFETY: `buf` is valid for `n` units and only read; `probe` is a live local of the
    // layout the C side writes.
    let rc = unsafe { sys::shim_dll_call_ordinal1(buf.as_ptr(), n as i32, ARG, &mut probe) };

    r.head("dlltest.dll");
    if rc < 0 {
        r.check_note("shim_dll_call_ordinal1", false, &num("the call itself failed, err", rc as i64));
        return;
    }

    // Five separate verdicts, because they fail for different reasons and one pass/fail
    // would collapse five diagnoses into one.

    r.check_note("loads", probe.load_err == 0, &num("RLibrary::Load err", probe.load_err as i64));
    if probe.load_err != 0 {
        r.info(
            "what that means",
            "the loader refused the image — a bad header, or an import it cannot satisfy. \
             Everything below is unreachable.",
        );
        return;
    }

    let mut uids = String::new();
    push_hexn(&mut uids, probe.uid1);
    uids.push(' ');
    push_hexn(&mut uids, probe.uid2);
    uids.push(' ');
    push_hexn(&mut uids, probe.uid3);
    r.check_note("UID1 is KDynamicLibraryUid", probe.uid1 == UID1_DLL, &uids);

    r.check_note(
        "Lookup(1) returns a function",
        probe.lookup_ok == 1,
        if probe.lookup_ok == 1 {
            "non-null"
        } else {
            "NULL — the image loaded and exports nothing, which is what a DLL built \
             without EXPORT_C produces"
        },
    );
    if probe.lookup_ok != 1 {
        return;
    }

    r.check_note("the call returns KErrNone", probe.call_err == 0,
        &num("returned", probe.call_err as i64));

    // The one that actually proves our code ran. A non-null Lookup proves an export table
    // exists; only a sentinel written through the pointer we passed proves the function
    // behind that ordinal is ours and received our arguments.
    let mut got = String::new();
    push_hexn(&mut got, probe.magic);
    got.push_str(" (want ");
    push_hexn(&mut got, MAGIC);
    got.push(')');
    r.check_note("it wrote our sentinel", probe.magic == MAGIC, &got);

    let mut echo = String::new();
    push_hexn(&mut echo, probe.echo);
    echo.push_str(" (want ");
    push_hexn(&mut echo, ARG);
    echo.push(')');
    r.check_note("it received our argument", probe.echo == ARG, &echo);

    // Proves the DLL's *own* import table resolved: User::TickCount lives in euser, and a
    // DLL that exports correctly can still fail to resolve what it imports. Zero is
    // possible but vanishingly unlikely, so it is reported rather than asserted.
    r.check_note(
        "its own import of euser resolved",
        probe.ticks != 0,
        &num("User::TickCount() from inside the DLL", probe.ticks as i64),
    );

    // And the MTM's DLL, asked the safe half of the question. Last, and flushed first, so it
    // cannot cost the dlltest answers above.
    r.flush(fs);
    check_mtmdemo(r);
}

fn num(label: &str, v: i64) -> String {
    let mut s = String::from(label);
    s.push(' ');
    push_i64(&mut s, v);
    s
}

fn push_hexn(s: &mut String, v: u32) {
    s.push_str("0x");
    push_hex(s, v, 8);
}

/// Whether `apps/mtmdemo`'s DLL loads and exports ordinal 1 — asked without calling it.
///
/// This is here, in the probe that has never failed, rather than in the mtm probe, because
/// the mtm probe dies. And it is a lookup rather than a call because a call is what killed
/// it: the framework loaded that DLL, entered ordinal 1, and the process was gone before the
/// DLL's own first instruction ran. Two things explain that — the image does not really load
/// under the framework's conditions, or entering the export is itself wrong — and nothing
/// visible from outside separates them.
///
/// `RLibrary::Load` plus `Lookup` cannot fault. If both succeed here, the DLL is fine and the
/// fault is in how it is entered; if the load fails, the error code says why and the DLL is
/// the problem.
pub fn check_mtmdemo(r: &mut Report) {
    r.head("mtmdemo.dll (the MTM, loaded but not called)");

    let mut buf = [0u16; 32];
    let name = "mtmdemo.dll";
    let mut n = 0;
    for b in name.bytes() {
        buf[n] = b as u16;
        n += 1;
    }
    // SAFETY: `buf` is valid for `n` units and only read; no call is made.
    let rc = unsafe { sys::shim_dll_has_ordinal(buf.as_ptr(), n as i32, 1) };
    match rc {
        1 => {
            r.check_note("loads and exports ordinal 1", true,
                "so the image is sound and the fault is in entering it, not in the DLL");
        }
        0 => {
            r.check_note("loads and exports ordinal 1", false,
                "it LOADS but has no ordinal 1 — the export table is empty or numbered \
                 differently than the registration claims");
        }
        e => {
            let mut s = String::from("will not load, err ");
            push_i64(&mut s, e as i64);
            r.check_note("loads and exports ordinal 1", false, &s);
            r.info(
                "reading that",
                "an import it needs is not satisfiable — which the framework would hit too, \
                 and would explain a death before our first instruction",
            );
        }
    }
}
