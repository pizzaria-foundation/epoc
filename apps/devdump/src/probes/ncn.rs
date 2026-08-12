//! The platform's new-message notification, in a binary of its own because it kills one.
//!
//! # Why this is not part of the mtm probe any more
//!
//! It was, and the launcher's manifest recorded `mtm CRASHED` with the notification's
//! breadcrumb as the last line on disk. So `MNcnNotification` takes the process down, and
//! while it was sharing a binary it took the rest of that probe's answers with it.
//!
//! A `TRAP` does not help. It catches a Leave; a Symbian panic kills the process outright,
//! and no amount of care on this side of the call changes that. The only containment
//! available is the one this project already uses for every risky import: give it its own
//! image, where failing costs its own section and nothing else.
//!
//! # There is no presence check any more, and removing it is the point
//!
//! A version of this probe asked `RLibrary::Load("ncnnotification.dll")` first, on the
//! reasoning that a load-and-close cannot crash and would separate "the plugin is absent"
//! from "the plugin is present and faults". It answered `present [0]` — and then the probe
//! died in `CMsvSession::OpenSyncL`, a call the mtm probe makes in the same run without
//! trouble.
//!
//! An ECom plugin is not an ordinary library. Its lifetime belongs to `REComSession`, and
//! loading and unloading one behind ECom's back leaves the process in a state the next
//! Message Server session does not survive. The check was safe reasoning applied to the
//! wrong kind of DLL, and it broke the thing it was added to protect.
//!
//! It is also unnecessary: the plugin's presence is already measured. So the probe now does
//! the least it can — session, service, notify — and each step is flushed before the next is
//! attempted, so wherever it stops, the last line written names what killed it.
//!
//! # And it still died, before reaching the notification at all
//!
//! With the check gone, the section ended at the *session and service* step — which the mtm
//! probe performs in the same run without trouble. So the fault is a property of this binary
//! rather than of those calls, and only two differences are left: this one links `ecom.dso`,
//! and it held fewer capabilities.
//!
//! The capabilities are now identical to the mtm probe's, which costs nothing and removes one
//! of the two. And the step is split so the next run says whether `OpenSyncL` or
//! `create_service` is the one that dies. Neither is a theory: both are one line, and the
//! alternative is guessing at a crash on somebody else's phone.
//!
//! # What it notifies about, and why that changed
//!
//! The first version passed the **inbox folder's** id, on the reasoning that a
//! platform-owned folder removes a variable. It crashed. But the parameter is named
//! `aMailBox`, and a folder is not a mailbox — the plugin plausibly looks the id up as a
//! *service* and faults when it is not one. So this version creates a service entry of its
//! own and notifies about that, which is both the documented shape of the argument and what
//! a real integration would pass.
//!
//! Both ids are tried, service first, with a flush between them. If the section ends after
//! the service attempt, the argument was never the problem and the route is unusable; if the
//! service attempt returns and the folder attempt kills it, the answer is that the id has to
//! be a service — which is a working notification rather than a dead end.

use alloc::string::String;

use symbian::fs::ShimFs;
use symbian::msg::{self, ncn};
use symbian_report::{push_i64, Report};
use symbian_sys as sys;

pub fn run(r: &mut Report, fs: &mut ShimFs) {
    // Split into two flushed steps, because the previous run died somewhere in "session and
    // service" and that step was wide enough to hide which. The mtm probe makes both of these
    // calls in the same run and survives them, so whatever kills this binary is a property of
    // the binary, not of the calls — and the only two candidates left are that this one links
    // ecom.dso and that it holds fewer capabilities. Narrowing the step is what turns those
    // from a guess into a next question.
    // Cleanup first, and flushed, so that a death later in this probe still leaves the
    // record that the account list was tidied — which is the one thing here that changes
    // the user's phone for the better and should not be lost with everything else.
    r.entering(fs, "cleanup of earlier runs");
    if let Ok(mut s) = msg::Session::open() {
        r.head("cleanup");
        match s.delete_services(SERVICE_MTM) {
            Ok(n) => r.num("services removed", n as i64),
            Err(e) => r.info("cleanup", &code_of(e)),
        }
    }
    r.flush(fs);

    r.entering(fs, "CMsvSession::OpenSyncL");
    let session = msg::Session::open();
    r.head("session");
    match &session {
        Ok(_) => r.check("OpenSyncL", true),
        Err(e) => r.check_note("OpenSyncL", false, &code_of(*e)),
    }
    r.flush(fs);

    r.entering(fs, "create_service");
    let service = session.and_then(|mut s| s.create_service(SERVICE_MTM, SERVICE_NAME));
    r.head("service");
    let service = match service {
        Ok(id) => {
            let mut t = String::from("id ");
            push_i64(&mut t, id as i64);
            r.check_note("create_service", true, &t);
            Some(id)
        }
        Err(e) => {
            r.check_note("create_service", false, &code_of(e));
            None
        }
    };
    r.flush(fs);

    // A service id first: `aMailBox` is what the parameter is called, and the run before last
    // passed a folder id. Flushed before each call, so the section ending says which attempt
    // died rather than merely that one did.
    if let Some(service) = service {
        r.entering(fs, "NewMessages with a SERVICE id");
        attempt(r, fs, "service", service);
    }

    // The folder id, which is what died two runs ago. Reached only if the service attempt
    // survived — in which case this line separates "the id must be a service", which is a
    // working notification, from "the route is unusable", which is a dead end.
    r.entering(fs, "NewMessages with the INBOX FOLDER id");
    attempt(r, fs, "inbox folder", sys::SHIM_MSV_INBOX);
}

/// The account name left in the Messaging list.
const SERVICE_NAME: &str = "ncn probe (safe to delete)";

/// Must be a type the platform does not know; the point is the id, not the type.
const SERVICE_MTM: u32 = 0xE0DD_0A02;

fn attempt(r: &mut Report, fs: &mut ShimFs, what: &str, id: i32) {
    r.head(what);
    r.info("about to call", "if the section ends here, this attempt killed the process");
    r.flush(fs);

    match ncn::notify(id, ncn::NORMAL) {
        Ok(()) => {
            r.check_note("NewMessages returned", true, what);
            r.info(
                "expect on screen",
                "an indicator, a tone and a floating note — the triple an arriving SMS makes",
            );
        }
        Err(e) => {
            r.check_note("NewMessages returned", false, &code_of(e));
            r.info(
                "reading that",
                "it resolved and refused rather than faulting, which is the good kind of no: \
                 the interface is documented for EMAIL plugins, so a refusal is an answer",
            );
        }
    }
}

fn code_of(e: symbian::Error) -> String {
    let code = match e {
        symbian::Error::Platform(c) => c,
        symbian::Error::NotFound => -1,
        symbian::Error::NotReady => -18,
        symbian::Error::AccessDenied => -46,
        symbian::Error::InUse => -14,
        _ => -2,
    };
    let mut s = String::from("err ");
    push_i64(&mut s, code as i64);
    s
}
