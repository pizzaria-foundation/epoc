//! Bluetooth: does the state the platform's BT server keeps answer us at all?
//!
//! This is the probe `registry.rs` has been naming as missing. `libsweep` already proved that
//! `btmanclient.dll`, `btdevice.dll`, `bluetooth.dll` and `btextnotifiers.dll` *open* on this
//! handset — that is the weaker answer, and it was always going to be. It says nothing about
//! whether a 2009 Nokia build exports the ordinals this SDK's import libraries name, and an
//! import that does not resolve stops the image loading with no error, no log and no report
//! file. So the whole point of this binary is the first line of its own section: if there is a
//! BEGIN in `C:\Data\dump-70-bt.txt`, the six imports resolved.
//!
//! # The order is the argument
//!
//! Cheapest and most certain first, so that a run which dies partway has already answered the
//! questions that decide whether the rest is worth attempting:
//!
//! ```text
//!   1  power, read        CenRep. One key, no session, no capability of consequence.
//!   2  P&S, read          six keys the BT server publishes. No session either.
//!   3  local record       RBTRegServ + RBTLocalDevice::Get — the first real session.
//!   4  paired devices     RBTRegistry::CreateView + CBTRegistryResponse.
//!   5  visibility, write  the first WRITE, and restored immediately.
//!   6  power, write       the one with two candidate routes and no documented answer.
//!   7  inquiry            slowest, least certain, and last for both reasons.
//! ```
//!
//! # What it does not do
//!
//! It does not pair, unpair, trust or rename anything. Those are writes against records the
//! user's own headsets depend on, and a reconnaissance pass that could cost somebody their
//! paired car kit is not a reconnaissance pass — the same argument `probes/net` makes for not
//! dialling. The two writes it *does* make are the two that cannot be gathered any other way,
//! and both are put back.

use alloc::string::String;

use symbian::bt;
use symbian::fs::ShimFs;
use symbian::prop;
use symbian_report::{push_hex, push_i64, Report};

/// How long the inquiry may take. A Bluetooth inquiry is specified at 1.28 s per period and the
/// stack runs about ten of them, so 15 s is one full sweep plus room; the probe's own deadline
/// in `registry.rs` is 60 s, which leaves the rest for everything above it.
const INQUIRY_BUDGET_MS: i32 = 15_000;

/// Stop after this many devices. A room with more than eight Bluetooth devices in it does not
/// tell us anything the eighth did not.
const INQUIRY_MAX: i32 = 8;

/// The read-only P&S keys, with what a value means. Named rather than looped over a range: the
/// point of asking is to find out which of them this stack actually publishes, and a key that
/// answers nothing is only interesting if the report says which key it was.
const PS_KEYS: &[(u32, &str, &str)] = &[
    (bt::PS_SCANNING_GET, "scanning", "THCIScanEnable: 0 none, 1 inquiry, 2 page, 3 both"),
    (bt::PS_LIMITED_GET, "limited discoverable", "the 60-second discoverable mode"),
    (bt::PS_DEVICE_CLASS_GET, "class of device", "24-bit CoD"),
    (bt::PS_REGISTRY_CHANGED, "registry changed", "the free refresh signal - which table last moved"),
    (bt::PS_PAIRED_ONLY_GET, "accept paired only", "1 = refuse connections from unpaired devices"),
    (bt::PS_INQUIRY_ACTIVE, "inquiry active", "1 = the stack is scanning right now"),
];

pub fn run(r: &mut Report, fs: &mut ShimFs) {
    r.entering(fs, "power (read)");
    power_read(r);
    r.flush(fs);

    r.entering(fs, "publish & subscribe");
    ps_keys(r);
    r.flush(fs);

    r.entering(fs, "local device");
    let local_ok = local(r);
    r.flush(fs);

    r.entering(fs, "paired devices");
    paired(r);
    r.flush(fs);

    r.entering(fs, "visibility (write)");
    visibility_write(r, local_ok);
    r.flush(fs);

    r.entering(fs, "power (write)");
    power_write(r);
    r.flush(fs);

    r.entering(fs, "inquiry");
    inquiry(r);
    r.flush(fs);

    r.entering(fs, "closing");
    match bt::close() {
        Ok(()) => r.check("registry session closed", true),
        Err(e) => r.check_note("registry session closed", false, &err(e)),
    }
}

/// Step 1. One CenRep key, and the one `apps/netd` already publishes for the launcher's status
/// bar — so a disagreement here would mean the status dot has been lying.
fn power_read(r: &mut Report) {
    r.head("the power key");
    let mut note = String::from("repo ");
    push_hex(&mut note, bt::POWER_REPO, 8);
    note.push_str(" key ");
    push_i64(&mut note, bt::POWER_KEY as i64);
    note.push_str("  - the same key apps/netd reads");
    r.info("KCRUidBluetoothPowerState", &note);

    match bt::power() {
        Ok(on) => r.check_note("radio powered", on, if on { "on" } else { "off" }),
        Err(e) => r.check_note("power readable", false, &err(e)),
    }
}

/// Step 2. Six keys, no session, no capability of consequence — and between them the whole of
/// the settings surface a Bluetooth screen needs that is not in the registry.
fn ps_keys(r: &mut Report) {
    r.head("the BT server's P&S keys");
    let mut cat = String::from("category ");
    push_hex(&mut cat, bt::PS_CATEGORY, 8);
    cat.push_str("  (KUidSystemCategory - the platform's, not ours)");
    r.info("where", &cat);

    for (key, name, why) in PS_KEYS {
        let mut label = String::from(*name);
        label.push_str(" (");
        push_hex(&mut label, *key, 8);
        label.push(')');

        let mut note = String::new();
        match prop::get(bt::PS_CATEGORY, *key) {
            Ok(v) => {
                note.push_str("= ");
                push_i64(&mut note, v as i64);
            }
            Err(e) => {
                note.push_str("unreadable ");
                note.push_str(&err(e));
            }
        }
        note.push_str("   ");
        note.push_str(why);
        r.info(&label, &note);
    }
}

/// Step 3. The first real session — RBTRegServ plus a subsession — and therefore the first
/// step whose failure means the registry route is closed to us rather than merely unhelpful.
///
/// Returns the visibility the record held, so step 5 can put it back. `None` means either the
/// read failed or the field was never set, and step 5 refuses to write in both cases: guessing
/// a value to restore is how a probe leaves a phone worse than it found it.
fn local(r: &mut Report) -> Option<bt::Visibility> {
    r.head("this handset's own record");
    match bt::local() {
        Ok(l) => {
            r.check("RBTLocalDevice::Get", true);
            r.info("name", if l.name.is_empty() { "(empty)" } else { &l.name });

            let mut addr = String::new();
            for (i, b) in l.addr.iter().enumerate() {
                if i > 0 {
                    addr.push(':');
                }
                push_hex(&mut addr, *b as u32, 2);
            }
            r.info("address", &addr);

            let mut cod = String::new();
            push_hex(&mut cod, l.device_class, 6);
            r.info("class of device", &cod);

            r.info("visibility", &describe_visibility(l.visibility));
            r.info("limited discoverable", &describe_flag(l.limited_discoverable));
            r.info("accept paired only", &describe_flag(l.accept_paired_only));
            match l.power_setting {
                Some(p) => {
                    let mut s = String::new();
                    push_i64(&mut s, p as i64);
                    s.push_str("  (the registry's stored setting, not the live radio)");
                    r.info("power setting", &s);
                }
                None => r.info("power setting", "unset"),
            }
            l.visibility
        }
        Err(e) => {
            r.check_note("RBTLocalDevice::Get", false, &err(e));
            None
        }
    }
}

/// Step 4. The list a paired-devices screen is made of, and the one measurement that says
/// whether `CBTRegistryResponse` works here at all.
fn paired(r: &mut Report) {
    r.head("paired devices");
    match bt::paired() {
        Ok((devices, total)) => {
            r.num("registry count", total as i64);
            if total > devices.len() {
                let mut s = String::from("showing ");
                push_i64(&mut s, devices.len() as i64);
                s.push_str(" of ");
                push_i64(&mut s, total as i64);
                s.push_str(" - the shim's cache holds 32");
                r.info("truncated", &s);
            }
            if devices.is_empty() {
                r.info(
                    "empty",
                    "no paired devices. Not a failure: the view answered, it was just empty. \
                     Pair a headset and re-run to see the other half of this.",
                );
            }
            for d in &devices {
                let mut note = String::new();
                note.push_str(&d.addr_string());
                note.push_str("  class ");
                push_hex(&mut note, d.device_class, 6);
                note.push_str("  major ");
                push_i64(&mut note, d.major_class() as i64);
                if d.trusted {
                    note.push_str("  TRUSTED");
                }
                if d.blocked {
                    note.push_str("  BLOCKED");
                }
                if d.encrypted {
                    note.push_str("  encrypted");
                }
                note.push_str(if d.friendly_name { "  (friendly name)" } else { "  (device name)" });
                let label = if d.name.is_empty() { String::from("(no name)") } else { d.name.clone() };
                r.info(&label, &note);
            }
        }
        Err(e) => r.check_note("CreateView(FindBonded)", false, &err(e)),
    }
}

/// Step 5. The first write, and the smallest one available: set the scan-enable to what it
/// already is.
///
/// A no-op value on purpose. What is being measured is whether `RBTLocalDevice::Modify` is
/// permitted and honoured, and that question does not need the phone's visibility to actually
/// change — a probe that left a handset discoverable to a room would have answered the same
/// question and charged the user for it.
fn visibility_write(r: &mut Report, current: Option<bt::Visibility>) {
    r.head("can we write the local record?");
    let Some(v) = current else {
        r.info(
            "not attempted",
            "the record did not say what the visibility is, and writing a guessed value is how \
             a probe leaves a phone worse than it found it",
        );
        return;
    };

    let mut note = String::from("wrote back ");
    note.push_str(&describe_visibility(Some(v)));
    match bt::set_visibility(v) {
        Ok(()) => r.check_note("RBTLocalDevice::Modify", true, &note),
        Err(e) => r.check_note("RBTLocalDevice::Modify", false, &err(e)),
    }
}

/// Step 6. The question the whole trip was for.
///
/// Turning the radio on has two candidate routes and no documented answer as to which this
/// handset honours: the S60 notifier (which raises a query and needs a person) or a direct
/// CenRep write (silent, and undocumented as a write). The shim tries them in that order and
/// reports which one answered; the app that comes after this uses whichever worked.
///
/// It only ever turns the radio ON, and only when it is off. Turning somebody's Bluetooth off
/// would drop a headset mid-call, and there is nothing to learn from it that turning it on does
/// not already say.
fn power_write(r: &mut Report) {
    r.head("can we turn the radio on?");

    let before = match bt::power() {
        Ok(on) => on,
        Err(e) => {
            r.check_note("power readable", false, &err(e));
            return;
        }
    };

    if before {
        r.info(
            "not attempted",
            "the radio is already on. Switch Bluetooth off in the native settings and re-run \
             to measure this - it is the one answer this probe cannot get for itself.",
        );
        return;
    }

    match bt::set_power(true) {
        Ok(route) => {
            let via = match route {
                bt::PowerRoute::Notifier => {
                    "RNotifier 0x100059E2 - the documented route, and it raised the platform's \
                     own query"
                }
                bt::PowerRoute::CenRep => {
                    "a direct CenRep write - the notifier did not do it, the key did"
                }
            };
            r.check_note("radio turned on", true, via);
        }
        Err(e) => {
            let mut note = String::from(&err(e));
            note.push_str("  - NEITHER route worked. The app that follows ships read-only for \
                           power and the user toggles it in the native screen.");
            r.check_note("radio turned on", false, &note);
        }
    }
}

/// Step 7. Slowest and least certain, and last for both reasons.
///
/// A blocking inquiry, which is only acceptable because this is a headless daemon with nothing
/// but a deadline to answer to. It is also the one step that needs something outside the phone:
/// with no discoverable device in the room, a working inquiry and a broken one both report zero.
fn inquiry(r: &mut Report) {
    r.head("RHostResolver over KBTLinkManager");
    let mut budget = String::new();
    push_i64(&mut budget, INQUIRY_BUDGET_MS as i64);
    budget.push_str(" ms, at most ");
    push_i64(&mut budget, INQUIRY_MAX as i64);
    budget.push_str(" devices");
    r.info("bounded at", &budget);

    match bt::inquiry(INQUIRY_BUDGET_MS, INQUIRY_MAX) {
        Ok(n) => {
            r.check("inquiry ran", true);
            r.num("devices found", n as i64);
            if n == 0 {
                r.info(
                    "zero",
                    "a working inquiry in an empty room and a broken one look identical here. \
                     Make a laptop discoverable and re-run before believing either.",
                );
            }
            for d in bt::found() {
                let mut note = String::new();
                note.push_str(&d.addr_string());
                note.push_str("  class ");
                push_hex(&mut note, d.device_class, 6);
                let label = if d.name.is_empty() { String::from("(no name)") } else { d.name.clone() };
                r.info(&label, &note);
            }
        }
        Err(e) => {
            // A timeout is its own finding and not the same as a refusal: it means the resolver
            // opened and answered nothing, which is what an empty room looks like from here too.
            let found = bt::found();
            let mut note = String::from(&err(e));
            note.push_str("  (collected ");
            push_i64(&mut note, found.len() as i64);
            note.push_str(" before it ended)");
            r.check_note("inquiry ran", false, &note);
        }
    }
}

fn describe_visibility(v: Option<bt::Visibility>) -> String {
    String::from(match v {
        Some(bt::Visibility::Hidden) => "0 hidden - neither found nor connectable",
        Some(bt::Visibility::InquiryOnly) => "1 inquiry only - found, not connectable",
        Some(bt::Visibility::PageOnly) => "2 page only - connectable, not discoverable",
        Some(bt::Visibility::Visible) => "3 visible - discoverable and connectable",
        None => "unset - the record does not say, which is not the same as hidden",
    })
}

fn describe_flag(f: Option<bool>) -> String {
    String::from(match f {
        Some(true) => "on",
        Some(false) => "off",
        None => "unset",
    })
}

/// The platform code, as text. Same shape as `probes/net`'s helper and for the same reason: a
/// report that says "err" without the number has not reported anything.
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
