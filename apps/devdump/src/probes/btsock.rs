//! RFCOMM: can an unsigned app on this handset be a Bluetooth serial *server*?
//!
//! This is the probe the remote-shell agent (apps/rshell) is blocked on. `libsweep` proved the
//! Bluetooth DLLs open and `bt` proved the registry side answers, but neither says whether this
//! ROM lets us open RFCOMM, claim a server channel, write an SDP service record and `Listen` —
//! the exact sequence a serial-port server is, and the one an unsigned app is most likely to be
//! refused for want of `LocalServices`. It also carries the one import nothing here has linked
//! before, `sdpdatabase`, so a BEGIN in `C:\Data\dump-71-btsock.txt` is itself the first
//! finding: the `esock`/`bluetooth`/`sdpdatabase` set resolved on a 2009 build.
//!
//! # One synchronous call, reported step by step
//!
//! Everything up to and including `Listen` is synchronous, so the whole question is answered by
//! `symbian::bt::rfcomm_probe`, which brings the socket up, registers and deletes a throwaway
//! SPP record, and tears it all down before returning — reporting one Symbian error code per
//! step. `Accept` and the reads and writes after it are asynchronous and are the agent's job,
//! built once this probe says the ground holds. Nothing here is left advertised or open: a
//! probe that changed what it measured would be no probe.

use alloc::string::String;

use symbian::bt::{self, RfcommProbe};
use symbian::fs::ShimFs;
use symbian_report::{push_i64, Report};

pub fn run(r: &mut Report, fs: &mut ShimFs) {
    // Breadcrumb before the risky call, not after: if the image is refused for the SDP import
    // or the process dies inside the bring-up, the last line on disk is the diagnosis.
    r.entering(fs, "rfcomm bring-up");
    r.head("RFCOMM server socket + SDP record");
    r.info(
        "what",
        "open RFCOMM, claim a server channel, bind, register an SPP record, listen",
    );

    match bt::rfcomm_probe() {
        Ok(p) => report_steps(r, &p),
        Err(e) => {
            // The whole sequence could not run — most likely the build carries no USE_BTSOCK,
            // or a leave escaped the shim's TRAP. Either way it is a single finding.
            r.check_note("rfcomm bring-up ran", false, &err(e));
        }
    }
    r.flush(fs);
}

/// Turn the per-step error codes into report lines. Reading order matters: a failure early
/// explains every `skipped` after it, so they are listed in the order the shim attempts them.
fn report_steps(r: &mut Report, p: &RfcommProbe) {
    step(r, "socket server (RSocketServ::Connect)", p.serv_err);
    step(r, "open RFCOMM socket", p.open_err);
    step(r, "claim a server channel", p.channel_err);
    if p.channel >= 0 {
        let mut note = String::from("channel ");
        push_i64(&mut note, p.channel as i64);
        r.info("assigned", &note);
    }
    step(r, "bind to the channel", p.bind_err);
    step(r, "open SDP database", p.sdp_open_err);
    step(r, "register + delete SPP record", p.sdp_reg_err);
    step(r, "listen (the LocalServices gate)", p.listen_err);
}

/// One step: `OK` is a pass, `SKIPPED` says the sequence stopped before reaching it (not a
/// failure of *this* step), anything else is the Symbian error it returned.
fn step(r: &mut Report, name: &str, code: i32) {
    if code == bt::RFCOMM_STEP_OK {
        r.check(name, true);
    } else if code == bt::RFCOMM_STEP_SKIPPED {
        r.check_note(name, false, "skipped - an earlier step stopped the sequence");
    } else {
        let mut note = String::from("err ");
        push_i64(&mut note, code as i64);
        r.check_note(name, false, &note);
    }
}

fn err(e: symbian::Error) -> String {
    let code = match e {
        symbian::Error::Platform(c) => c,
        symbian::Error::NotFound => -1,
        symbian::Error::NotReady => -18,
        symbian::Error::AccessDenied => -46,
        _ => -2,
    };
    let mut s = String::from("err ");
    push_i64(&mut s, code as i64);
    s
}
