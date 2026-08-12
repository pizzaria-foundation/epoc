//! What the handset offers for networking, without opening a connection.
//!
//! Links only what `examples/selftest` has already proven loads — `esock`, `insock`,
//! `commdb`, `netmeta`, `apgrfx` — and asks about everything else with
//! `RLibrary::Load`, which needs no import at all. The platform's own HTTP and TLS stacks
//! are exactly the sort of thing that would be convenient to link and catastrophic to
//! guess at.
//!
//! It deliberately does **not** dial. Bringing up a bearer on this handset can raise a
//! dialog and wait for a human, and one access point once timed out at 35013 ms in two
//! separate runs to the millisecond — "a radio failing to associate does not do that; a
//! countdown does" (`docs/device-notes.md`). A reconnaissance pass that could block on a
//! dialog is not a reconnaissance pass.

use alloc::string::String;

use symbian::fs::ShimFs;
use symbian::net;
use symbian_report::{push_i64, Report};
use symbian_sys as sys;

/// The networking DLLs whose presence decides what can be built without porting it.
const ASK: &[(&str, &str)] = &[
    ("http.dll", "the platform's own HTTP stack — would make README's HTTP todo unnecessary"),
    ("httpfilterauthentication.dll", "HTTP auth filter"),
    ("securesocket.dll", "the platform's own TLS sockets"),
    ("libssl.dll", "Open C's OpenSSL 0.9.8a — TLS, if this handset has Open C"),
    ("libcrypto.dll", "Open C's crypto: AES, RSA, bignum. No SHA-256; 0.9.8a predates it"),
    ("libc.dll", "Open C: BSD sockets and stdio"),
    ("esock.dll", "the socket server (control: selftest already links it)"),
    ("insock.dll", "IPv4 (control)"),
    ("connmon.dll", "connection monitoring"),
    ("netmeta.dll", "connection preferences"),
];

pub fn run(r: &mut Report, fs: &mut ShimFs) {
    r.entering(fs, "libraries");
    libraries(r);
    r.flush(fs);

    r.entering(fs, "access points");
    access_points(r);
}

fn libraries(r: &mut Report) {
    r.head("networking libraries");
    for (name, why) in ASK {
        let mut buf = [0u16; 64];
        let mut n = 0;
        for b in name.bytes() {
            buf[n] = b as u16;
            n += 1;
        }
        // SAFETY: `buf` is valid for `n` units and only read.
        let rc = unsafe { sys::shim_dll_present(buf.as_ptr(), n as i32) };
        let mut note = String::from(if rc == 0 { "present" } else { "ABSENT" });
        note.push_str("  [");
        push_i64(&mut note, rc as i64);
        note.push_str("]  ");
        note.push_str(why);
        r.info(name, &note);
    }
}

fn access_points(r: &mut Report) {
    r.head("access points");
    // Enumerating costs nothing and dials nothing. The count alone answers whether the
    // handset has any usable bearer configured, which is the first thing to know before
    // anything else on the network path is worth debugging.
    match net::connections_up() {
        Ok(count) => {
            r.num("RConnection count", count as i64);

            // Symbian's convention is one-based and the headers do not say so. Index 0 is
            // asked too, and the report says which answered — one run settles it, rather
            // than a guess surviving in a comment (`examples/selftest`).
            r.head("index base");
            for idx in [0u32, 1u32] {
                let mut label = String::from("index ");
                push_i64(&mut label, idx as i64);
                match net::connection_iap(idx) {
                    Ok(iap) => {
                        let mut s = String::from("answered, IAP ");
                        push_i64(&mut s, iap as i64);
                        r.info(&label, &s);
                    }
                    Err(e) => {
                        let mut s = String::from("err ");
                        push_i64(&mut s, code(e) as i64);
                        r.info(&label, &s);
                    }
                }
            }
        }
        Err(e) => {
            let mut s = String::from("err ");
            push_i64(&mut s, code(e) as i64);
            r.check_note("RConnection enumeration", false, &s);
        }
    }

    r.head("note");
    r.info(
        "not attempted",
        "no bearer is started and no socket opened: a dial can raise a dialog and wait for \
         a human, which would turn this probe's deadline into a measurement of somebody's \
         attention span",
    );
}

fn code(e: symbian::Error) -> i32 {
    match e {
        symbian::Error::Platform(c) => c,
        symbian::Error::NotFound => -1,
        symbian::Error::NotReady => -18,
        symbian::Error::AccessDenied => -46,
        _ => -2,
    }
}
