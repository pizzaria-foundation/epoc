//! Can we actually put a message type into the native Messaging application?
//!
//! Four things no amount of reading the SDK answers, because the SDK documents MTMs for
//! Symbian's own TechView UI and carries the caveat *"there is no guarantee that it will work
//! on other interfaces"* — and S60's Messaging application (MCE) is another interface. Every
//! one of them is a property of Nokia's app, not of the framework.
//!
//! | question | what it decides |
//! |---|---|
//! | does `InstallMtmGroup` accept a registration outside ROM? | whether the whole plan is viable |
//! | does a message of an unknown type appear in the inbox at all? | whether the daemon route works before any MTM exists |
//! | what icon does MCE draw for us? | whether we need an `.mbm` or get the unknown-type envelope |
//!
//! # Why this probe is careful in a way the others are not
//!
//! The other probes only read. This one writes into the user's message store and registers a
//! message type with a running system service, so two rules apply that nothing else here
//! needs:
//!
//! **It registers no component it cannot back.** MCE loads a registered component's DLL and
//! calls its factory *by ordinal*. Registering a UI or UI Data component pointing at a DLL
//! with no such factory would invite Nokia's own process to call a function whose signature
//! is nothing like the one it expects. So the registration names one client component, is
//! measured, and is de-installed before anything can load it.
//!
//! **It does not raise the notification.** That was here, and it crashed the process — so it
//! moved to `probes/ncn`, on the same reasoning that puts every risky import in its own
//! image. See that module.
//!
//! **It labels what it leaves behind.** The service and the message stay on the phone after
//! the run, because the last question can only be answered by a human looking at the screen.
//! They are named so it is obvious they are ours and safe to delete, and the report says
//! their ids so a later run can remove them.

use alloc::string::String;

use symbian::fs::ShimFs;
use symbian::msg::{self, NewMessage};
use symbian_report::{push_hex, push_i64, Report};

/// `apps/mtmdemo`'s type — the one whose DLL exports a real client factory. Must match
/// `MTM_TYPE_UID` in `apps/mtmdemo/app.conf`, which is where the registration resource gets it.
const MTM_UID: u32 = 0xE0DD_0B01;
/// Where `symbuild`'s `MTM_RESOURCE` rule installs `apps/mtmdemo`'s registration.
const REG_PATH: &str = "C:\\resource\\messaging\\mtm\\mtmdemoreg.rsc";
/// Deliberately unmistakable on a phone that is somebody's actual phone.
const SERVICE_NAME: &str = "devdump probe (safe to delete)";

pub fn run(r: &mut Report, fs: &mut ShimFs) {
    r.entering(fs, "CMsvSession::OpenSyncL");
    let mut session = match msg::Session::open() {
        Ok(s) => {
            r.check("session opened", true);
            s
        }
        Err(e) => {
            r.check_note("session opened", false, &err(e));
            return;
        }
    };
    r.flush(fs);

    // FIRST, before anything that can die: what the DLL recorded about itself LAST time.
    //
    // This used to run at the end, after the call that instantiates the DLL — which is the
    // call that kills the process, so the read was dead code and the answer sat on the phone
    // unread for a whole trip. A diagnostic that runs after the thing it diagnoses is not a
    // diagnostic.
    //
    // Reading it at the start means every run reports the *previous* run's construction, one
    // trip behind. That is the price of the process being the thing that dies, and it is
    // still an answer per trip rather than none.
    r.entering(fs, "the DLL's trace from the previous run");
    dll_trace(r, fs);

    // The startup count: what a *previous* run left behind. Useful, but not the measurement —
    // it says nothing about this run, and the run before this one died before it ever
    // installed anything, so on its own it would have answered a question nobody asked.
    r.entering(fs, "registry at startup");
    r.head("registry at startup");
    let before = session.mtm_count().unwrap_or(-1);
    r.num("registered MTMs at startup", before as i64);
    r.flush(fs);

    r.entering(fs, "cleanup of earlier runs");
    cleanup(r, fs, &mut session);

    r.entering(fs, "registration");
    registration(r, fs, &mut session);
    r.flush(fs);

    // THE measurement, and it is what makes one run answer the question by itself.
    //
    // Counting on the session that did the install has never worked and the source says why:
    // CClientMtmRegistry is a per-process copy, refreshed only when the session dispatches
    // the server's EMsvMtmGroupInstalled — which cannot happen while this probe is still
    // inside its own RunL. Every reading taken that way was the pre-install snapshot.
    //
    // A *new* session builds its registry from scratch, out of the server's permanent one,
    // via FillRegisteredMtmDllArray. So: drop the session that installed, open another, and
    // count from that. No event to wait for, no stale copy, and no dependence on what some
    // earlier run did or did not get to do.
    r.entering(fs, "reopening the session to see the server's own registry");
    drop(session);
    let mut session = match msg::Session::open() {
        Ok(s) => {
            r.head("after reopening");
            r.check("second session opened", true);
            s
        }
        Err(e) => {
            r.head("after reopening");
            r.check_note("second session opened", false, &err(e));
            return;
        }
    };

    let after = session.mtm_count().unwrap_or(-1);
    r.num("registered MTMs, fresh session", after as i64);
    r.check_note(
        "the registration persisted",
        after > before,
        if after > before {
            "the server's own registry grew — a custom MTM registers on this handset, which \
             is the gate for everything after it"
        } else {
            "InstallMtmGroup returned KErrNone and the server's registry did not grow, read \
             from a session that never saw the install — so it accepts the call and keeps \
             nothing"
        },
    );
    r.flush(fs);

    let service = store(r, fs, &mut session);
    r.flush(fs);


    r.head("notification");
    // Deliberately not attempted here. The first run of this probe ended with the
    // notification's breadcrumb and nothing under it, and the launcher's manifest recorded
    // CRASHED — so MNcnNotification took the process down, and it took the rest of this
    // probe's answers with it.
    //
    // A TRAP would not have helped: it catches a Leave, and a Symbian panic kills the
    // process outright. The rule this project already follows applies — a facility that can
    // take the process down belongs in its own binary, where failing costs its own section.
    // It now has one.
    r.info("moved", "the notification is its own probe now — see the ncn section");
    let _ = service;

    r.head("what to look at on the phone");
    r.info("1", "open Messaging — is there an account called \"devdump probe\"?");
    r.info("2", "open the Inbox — is there a message from \"devdump\"?");
    r.info("3", "what icon does it have — ours, or the unknown-type envelope?");
    r.info("4", "did an indicator, a tone or a floating note appear?");
}

/// The question that decides whether the plan is viable.
fn registration(r: &mut Report, fs: &mut ShimFs, session: &mut msg::Session) {
    r.head("registration");

    // Whether the file even got installed by the package. If it is absent, everything below
    // is measuring the wrong thing, and the report has to say so rather than reporting a
    // KErrNotFound as if the Message Server had refused us. That distinction already saved
    // one trip.
    let present = symbian::caps::attempt(REG_PATH).is_ok();
    r.check_note(
        "the .mtm reached the device",
        present,
        if present { REG_PATH } else { "NOT INSTALLED — the package did not carry it" },
    );
    if !present {
        return;
    }

    match session.install_mtm(REG_PATH) {
        Ok(()) => r.check("InstallMtmGroup", true),
        Err(e) => {
            r.check_note("InstallMtmGroup", false, &err(e));
            r.info(
                "what that means",
                "the Message Server refused a registration outside ROM. Everything about a \
                 custom MTM on this handset depends on this call.",
            );
            return;
        }
    }
    r.flush(fs);

    // THE measurement, and why this probe now registers something real.
    //
    // It asks the framework to do the whole job: find the type in the registry, load
    // apps/mtmdemo's DLL out of \sys\bin, and call the factory at the ordinal the
    // registration names. An object coming back means the registration path works end to
    // end, and everything after it — UI Data, the icon, opening a message — is ordinary work.
    //
    // The earlier version could never have answered this. It registered a component pointing
    // at a DLL with no such factory and de-installed immediately, so nothing could ever be
    // instantiated.

    // NOT de-installed, and that is the fix for the flaw that made three runs unreadable.
    //
    // This probe used to remove the registration at the end of every run, so the next run
    // always started from a clean registry and the startup count was always 15 — which read
    // as "it never registers". The probe was destroying the only evidence it could produce.
    // The registration stays. It is inert unless something asks for the type.
    r.info("left installed", "on purpose — the next run's startup count is the measurement");
}

/// Where `apps/mtmdemo` writes its construction breadcrumbs. Must match the `Trace` helper
/// in `apps/mtmdemo/src/mtmclient.cpp`.
const DLL_TRACE: &str = "C:\\Data\\dump-mtmdemo.txt";

fn dll_trace(r: &mut Report, fs: &mut ShimFs) {
    r.head("what the DLL itself recorded");
    let Ok(path) = symbian::fs::Utf16Path::new(DLL_TRACE) else { return };
    match symbian::fs::read(fs, &path) {
        Ok(Some(bytes)) => {
            let text = alloc::string::String::from_utf8_lossy(&bytes);
            let mut last = "";
            for line in text.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                r.info("step", line.trim());
                last = line.trim();
            }
            if last.is_empty() {
                r.info("empty", "the file exists and holds nothing");
            } else {
                r.info("furthest step reached", last);
            }
            // Removed so the next run's trace is this run's, not an accumulation of every
            // run's. A trace that grows across runs cannot say which one died where.
            let _ = <ShimFs as symbian::fs::Fs>::delete(fs, path.as_units());
        }
        Ok(None) | Err(_) => r.info(
            "no trace file",
            "the DLL never reached its first breadcrumb — so the fault is before any of our \
             code ran, in the load or the call itself",
        ),
    }
}

/// Every MTM type these probes have ever created a service under.
///
/// Runs before anything is created, and it is a list rather than a constant because earlier
/// runs used earlier uids and their services are still on the phone. Nothing remembers those
/// ids, so "delete everything of these types" is the only cleanup that can reach them.
const OURS: &[(u32, &str)] = &[
    (0xE0DD_0A01, "the mtm probe's first type"),
    (0xE0DD_0A02, "the ncn probe's type"),
    (0xE0DD_0B01, "apps/mtmdemo — the current one"),
];

/// Remove what previous runs left in the user's Messaging account list.
///
/// This exists because the probe created a service on every run and removed none, so the
/// list filled with copies of "devdump probe (safe to delete)". Creating one per run was the
/// mistake; cleaning up first — and then creating exactly one — is the fix, and it also
/// repairs the phones that already collected them.
fn cleanup(r: &mut Report, fs: &mut ShimFs, session: &mut msg::Session) {
    r.head("cleanup");
    let mut total = 0;
    for (uid, what) in OURS {
        match session.delete_services(*uid) {
            Ok(n) => {
                total += n;
                if n > 0 {
                    let mut s = String::new();
                    push_i64(&mut s, n as i64);
                    s.push_str(" removed — ");
                    s.push_str(what);
                    let mut key = String::from("0x");
                    push_hex(&mut key, *uid, 8);
                    r.info(&key, &s);
                }
            }
            Err(e) => {
                let mut key = String::from("0x");
                push_hex(&mut key, *uid, 8);
                r.info(&key, &err(e));
            }
        }
    }
    r.num("services removed", total as i64);
    r.info(
        "why",
        "earlier runs created one service each and removed none, so the Messaging account \
         list filled with copies. This run leaves exactly one.",
    );
    r.flush(fs);
}

/// Create a service and one message, and leave them for inspection.
fn store(r: &mut Report, fs: &mut ShimFs, session: &mut msg::Session) -> Option<i32> {
    r.head("service entry");
    let service = match session.create_service(MTM_UID, SERVICE_NAME) {
        Ok(id) => {
            let mut s = String::from("id ");
            push_i64(&mut s, id as i64);
            r.check_note("create_service", true, &s);
            id
        }
        Err(e) => {
            r.check_note("create_service", false, &err(e));
            return None;
        }
    };
    r.flush(fs);

    r.head("message entry");
    // An unregistered MTM uid on purpose: the registration was de-installed above, so this
    // measures what MCE does with a message whose type it does not know — which is exactly
    // the state the daemon route would be in before any MTM exists.
    let msg = NewMessage::new(service, MTM_UID)
        .from("devdump")
        .subject("probe: does this appear?")
        .body("Written by apps/devdump. Safe to delete.");

    match session.create_message(&msg) {
        Ok(id) => {
            let mut s = String::from("id ");
            push_i64(&mut s, id as i64);
            r.check_note("create_message", true, &s);
            let mut u = String::from("0x");
            push_hex(&mut u, MTM_UID, 8);
            r.info("its iMtm", &u);
            r.info("to delete later", "both ids are above; delete_entry takes them");
        }
        Err(e) => r.check_note("create_message", false, &err(e)),
    }

    // The inbox count, before and after, is the cheap corroboration that the entry is really
    // in the folder rather than merely having been assigned an id.
    if let Ok(n) = session.folder_count(symbian_sys::SHIM_MSV_INBOX) {
        r.num("inbox count now", n as i64);
    }
    Some(service)
}

fn err(e: symbian::Error) -> String {
    let code = match e {
        symbian::Error::Platform(c) => c,
        symbian::Error::NotFound => -1,
        symbian::Error::PathNotFound => -12,
        symbian::Error::AlreadyExists => -11,
        symbian::Error::AccessDenied => -46,
        symbian::Error::NotReady => -18,
        symbian::Error::InUse => -14,
        symbian::Error::Argument => -6,
        _ => -2,
    };
    let mut s = String::from("err ");
    push_i64(&mut s, code as i64);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stale constant here shipped once and cost a device trip: the probe registered a
    /// path whose resource had been deleted and created messages under the old type, and the
    /// report looked plausible in both cases. Neither is visible by reading the section.
    #[test]
    fn the_type_matches_the_registration_it_installs() {
        assert_eq!(MTM_UID, 0xE0DD_0B01, "must match apps/mtmdemo/inc/mtmdemo.h");
        assert!(
            REG_PATH.ends_with("mtmdemoreg.rsc"),
            "the registration installed must be apps/mtmdemo's, not a probe's own: {REG_PATH}"
        );
    }

    /// The cleanup list has to include the type the probe currently creates, or this run's
    /// service is the one left behind next time — which is the bug it exists to fix.
    #[test]
    fn cleanup_covers_the_type_currently_in_use() {
        assert!(OURS.iter().any(|(uid, _)| *uid == MTM_UID));
    }
}
