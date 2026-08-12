//! The Message Server, read-only — and the one probe most likely to leave no section at
//! all.
//!
//! This is the only binary in the fleet that links `msgs.dso` and `mtur.dso`. The E72's
//! messaging DLLs are a 2009 Nokia build and this SDK's import libraries need not be the
//! same ones; an ordinal we call that the handset does not export makes the E32 loader
//! refuse the image, with no panic, no log and no file. That is precisely why it is alone
//! in here: the absence of `60-msg.txt`, recorded against the launcher's manifest, *is*
//! the finding.
//!
//! Everything it does is a read. Opening a session, enumerating the MTM registry and
//! counting folder entries is enough to learn what the platform's messaging stack contains
//! before deciding whether to build on it, and it puts none of the user's messages at risk.

use alloc::string::String;

use symbian::fs::{self, Fs, ShimFs, Utf16Path};
use symbian::msg;
use symbian_report::{push_hex, push_i64, Report};

/// Where the Message Server keeps its MTM registration resources. Listing it needs no
/// messaging API at all — so it answers even if the session below never opens.
const MTM_DIR: &str = "C:\\resource\\messaging\\mtm\\";
const MTM_DIR_ROM: &str = "Z:\\resource\\messaging\\mtm\\";

pub fn run(r: &mut Report, fs_: &mut ShimFs) {
    // Deliberately first. It uses only the file server, so if the session below takes the
    // process down, the registry directory listing is already on disk — and that listing
    // alone says how many MTMs the handset has.
    r.entering(fs_, "mtm registry directory");
    registry_dir(r, fs_);
    r.flush(fs_);

    r.entering(fs_, "CMsvSession::OpenSyncL");
    let started = symbian::monotonic_us();
    let session = msg::Session::open();
    let elapsed = symbian::monotonic_us().saturating_sub(started);

    r.head("session");
    match session {
        Ok(mut s) => {
            r.check("CMsvSession::OpenSyncL", true);
            r.num("open took (us)", elapsed as i64);
            r.flush(fs_);

            r.entering(fs_, "registered MTMs");
            mtms(r, &mut s);
            r.flush(fs_);

            r.entering(fs_, "folders");
            folders(r, &mut s);
        }
        Err(e) => {
            r.check_note("CMsvSession::OpenSyncL", false, &err(e));
            r.num("failed after (us)", elapsed as i64);
            r.info(
                "what that means",
                "the image loaded, so msgs.dll is present and its ordinals resolved — the \
                 Message Server itself refused or is not running",
            );
        }
    }
}

fn registry_dir(r: &mut Report, fs_: &mut ShimFs) {
    r.head("mtm registration resources");
    for dir in [MTM_DIR, MTM_DIR_ROM] {
        let Ok(p) = Utf16Path::new(dir) else { continue };
        let mut buf = [0u16; 4096];
        match fs_.list_dir(p.as_units(), &mut buf) {
            Ok(n) => {
                let mut key = String::from(dir);
                let mut note = String::new();
                push_i64(&mut note, n as i64);
                note.push_str(" entries");
                r.info(&key, &note);
                key.clear();
                // Names are packed NUL-separated.
                let mut start = 0usize;
                for i in 0..buf.len() {
                    if buf[i] == 0 {
                        if i > start {
                            let mut name = String::new();
                            for u in &buf[start..i] {
                                if let Some(c) = char::from_u32(*u as u32) {
                                    name.push(c);
                                }
                            }
                            r.info("  file", &name);
                        }
                        start = i + 1;
                        if start >= buf.len() {
                            break;
                        }
                    }
                }
            }
            Err(e) => r.info(dir, &err(e)),
        }
    }
}

fn mtms(r: &mut Report, s: &mut msg::Session) {
    r.head("registered MTMs");
    let count = match s.mtm_count() {
        Ok(n) => n,
        Err(e) => {
            r.check_note("CClientMtmRegistry", false, &err(e));
            return;
        }
    };
    r.check("CClientMtmRegistry", true);
    r.num("registered", count as i64);

    for i in 0..count {
        match s.mtm_info(i) {
            Ok(info) => {
                let mut key = String::from("0x");
                push_hex(&mut key, info.type_uid, 8);

                let mut note = String::new();
                for u in msg::info_name(&info) {
                    if let Some(c) = char::from_u32(*u as u32) {
                        note.push(c);
                    }
                }
                // An MTM this table does not recognise is printed as its raw UID and
                // nothing else. Guessing a name would invent the very fact the
                // enumeration exists to discover.
                if let Some(known) = msg::mtm_name(info.type_uid) {
                    note.push_str("  (");
                    note.push_str(known);
                    note.push(')');
                }
                note.push_str("  tech 0x");
                push_hex(&mut note, info.technology_uid, 8);
                r.info(&key, &note);
            }
            Err(e) => {
                let mut key = String::from("index ");
                push_i64(&mut key, i as i64);
                r.info(&key, &err(e));
            }
        }
    }
}

fn folders(r: &mut Report, s: &mut msg::Session) {
    r.head("standard folders");
    for (id, name) in msg::FOLDERS {
        match s.folder_count(*id) {
            Ok(n) => r.num(name, n as i64),
            Err(e) => r.info(name, &err(e)),
        }
    }
}

fn err(e: symbian::Error) -> String {
    let code = match e {
        symbian::Error::Platform(c) => c,
        symbian::Error::NotFound => -1,
        symbian::Error::PathNotFound => -12,
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
