//! Which probes exist, what each is called, and how long it is allowed to take.
//!
//! One table, read by three things that must agree: the launcher (which runs them), the
//! merge (which orders the sections) and the build (which packages the binaries). Three
//! copies of this list would drift, and the way it would drift is a probe that is
//! installed and never run — an absence indistinguishable from a probe that failed to
//! load, which is the one distinction the whole design turns on.

/// A probe: one binary, one section file, one subject.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Probe {
    /// Short name, used in the manifest and in the section's BEGIN/END sentinels.
    pub name: &'static str,
    /// Filename prefix. Two digits so a lexical sort is reading order, and so a gap in
    /// the listing is visible to a human skimming the directory.
    pub order: u8,
    /// The executable's basename in `\sys\bin`.
    pub exe: &'static str,
    /// UID3, used to poll liveness. Must match the probe's own `app.conf`.
    pub uid3: u32,
    /// How long to wait before killing it and recording a timeout.
    ///
    /// Per-probe rather than global because the spread is real: a HAL sweep is
    /// instantaneous, and opening a Message Server session on a handset that is
    /// rebuilding its index is not. A single global deadline would have to be the
    /// slowest, which would turn a hung fast probe into a two-minute stall.
    pub deadline_ms: i32,
    /// What it answers, one line, printed in the manifest so the report explains itself.
    pub about: &'static str,
}

const fn p(
    order: u8,
    name: &'static str,
    exe: &'static str,
    uid3: u32,
    deadline_ms: i32,
    about: &'static str,
) -> Probe {
    Probe { name, order, exe, uid3, deadline_ms, about }
}

/// Every probe, in the order the launcher runs them.
///
/// The order is not arbitrary. `libsweep` is early because it is the master key — which of
/// the SDK's 345 import libraries actually load decides what every future binary may link —
/// and a run that dies partway should have answered that before it did. The isolated,
/// riskiest probes (`msg`, `etel`, and the rest of the hardware set) come after the ones
/// that cannot fail, so a catastrophic hang costs the least.
pub const PROBES: &[Probe] = &[
    p(10, "system", "ddsystem.exe", 0xE0DD0010, 15_000,
      "HAL inventory, drives and volumes, screen, clock, RAM"),
    p(20, "libsweep", "ddlibs.exe", 0xE0DD0020, 90_000,
      "which of the SDK's import libraries actually load on this handset"),
    p(30, "caps", "ddcaps.exe", 0xE0DD0030, 15_000,
      "what the ROM patch granted: HasCapability against the operation itself"),
    p(40, "dll", "dddll.exe", 0xE0DD0040, 15_000,
      "loading our own polymorphic DLL and calling ordinal 1"),
    p(50, "net", "ddnet.exe", 0xE0DD0050, 60_000,
      "IAPs and bearers, and which networking DLLs the handset has"),
    p(80, "fs", "ddfs.exe", 0xE0DD0080, 30_000,
      "data cage, path limits, file attributes, atomic save"),
    // The only probe that imports a library the handset may not satisfy. Alone in its
    // image precisely so that failing to load costs its own section and nothing else.
    p(60, "msg", "ddmsg.exe", 0xE0DD0060, 45_000,
      "Message Server: registered MTMs and folder counts (imports msgs.dso)"),
    // The only probe that WRITES. It registers a message type, creates a service and one
    // message, and raises a notification — so it runs last, after everything that only
    // observes has already been recorded.
    p(61, "mtm", "ddmtm.exe", 0xE0DD0061, 60_000,
      "can we put a message type into the native Messaging app? (writes; leaves entries)"),
    // Alone in its image because it is *known* to kill the process: an earlier run recorded
    // `mtm CRASHED` with this call's breadcrumb as the last line on disk. A TRAP does not
    // help — it catches a Leave, and a Symbian panic kills the process outright — so the only
    // containment is the one every risky import here gets. Last, so it can cost nothing else.
    p(62, "ncn", "ddncn.exe", 0xE0DD0062, 20_000,
      "the platform's new-message notification (known to crash; contained here)"),
];

// NOT IN THE TABLE, AND DELIBERATELY SO
//
// ETel, the sensor framework, Bluetooth, the location framework, central repository and
// DBMS each want a probe of their own, on the same isolation argument as `msg`. None is
// listed here because none has been built: there is no shim for any of them yet.
//
// Listing them anyway would be worse than leaving them out. The launcher would fail to
// start a binary that does not exist and record `REFUSED`, which in this report means "the
// loader would not accept the image" — a statement about the *handset*. It would be
// reporting a fact nobody established, in a run whose entire purpose is to establish facts,
// and it would look exactly like the finding that matters most.
//
// Their first question is answered anyway, and for free: the `libsweep` probe asks
// `RLibrary::Load` about `etel.dll`, `etelmm.dll`, `sensrvclient.dll`, `btdevice.dll`,
// `lbs.dll`, `centralrepository.dll` and `edbms.dll` along with every other library the
// SDK ships. That is the weaker answer — it proves the DLL opens, not that the ordinals we
// would call exist — but it is the one that decides whether building the stronger probe is
// worth the trip, which is exactly the trip-2 argument in the plan.

/// The launcher's own section, which is not a probe: it is written before anything runs.
pub const MANIFEST_ORDER: u8 = 0;
pub const MANIFEST_NAME: &str = "launcher";

/// The merged file, written last and best-effort.
pub const MERGED_ORDER: u8 = 99;
pub const MERGED_NAME: &str = "merged";

/// Where every section lands: `C:\Data\` itself, flat, no subdirectory.
///
/// It was `dump\`, and that cost a device trip. `RFs::MkDirAll` ignores the last component
/// of a path that does not end in a separator, so the directory was never created, every
/// write answered `KErrPathNotFound`, and the whole report fell through to the private data
/// cage — where the file manager cannot see it and, because the cage is per-UID3, each probe
/// landed in a different one.
///
/// Flat is simply better here. The filenames already sort into reading order, `C:\Data\` is
/// the one directory known to be writable with no capability and reachable over both USB and
/// Bluetooth, and there is no directory left to fail to create.
pub const DIR: &str = "";

/// Prefix on every section file, so the run's output is identifiable among the user's own
/// files in `C:\Data\` — the same reason `symbian::log` writes `logs_<app>.txt` there.
pub const PREFIX: &str = "dump-";

/// `"<order:02>-<name>.txt"`, e.g. `"10-system.txt"`.
pub fn filename(order: u8, name: &str) -> alloc::string::String {
    let mut s = alloc::string::String::from(PREFIX);
    s.push((b'0' + order / 10) as char);
    s.push((b'0' + order % 10) as char);
    s.push('-');
    s.push_str(name);
    s.push_str(".txt");
    s
}

/// The full device path of a probe executable.
///
/// `\sys\bin` on C: — where `symbuild`'s package puts every executable, and the only
/// directory a platform-secured device will load code from.
pub fn exe_path(exe: &str) -> alloc::string::String {
    let mut s = alloc::string::String::from("C:\\sys\\bin\\");
    s.push_str(exe);
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeSet;
    use alloc::vec::Vec;

    /// Two probes sharing a UID3 would make the launcher poll the wrong process for
    /// liveness — and it would look like the second probe finishing instantly.
    #[test]
    fn uid3s_are_unique() {
        let mut seen = BTreeSet::new();
        for pr in PROBES {
            assert!(seen.insert(pr.uid3), "duplicate UID3 {:#x} on {}", pr.uid3, pr.name);
        }
    }

    /// Two probes sharing an order would write to the same section file, and the second
    /// would silently erase the first.
    #[test]
    fn orders_are_unique_and_do_not_collide_with_the_reserved_ones() {
        let mut seen = BTreeSet::new();
        for pr in PROBES {
            assert!(seen.insert(pr.order), "duplicate order {} on {}", pr.order, pr.name);
            assert_ne!(pr.order, MANIFEST_ORDER, "{} collides with the manifest", pr.name);
            assert_ne!(pr.order, MERGED_ORDER, "{} collides with the merge", pr.name);
        }
    }

    #[test]
    fn names_and_executables_are_unique() {
        let names: BTreeSet<_> = PROBES.iter().map(|p| p.name).collect();
        assert_eq!(names.len(), PROBES.len());
        let exes: BTreeSet<_> = PROBES.iter().map(|p| p.exe).collect();
        assert_eq!(exes.len(), PROBES.len());
    }

    /// A probe in the development UID range keeps the package installable on a stock
    /// handset and cannot collide with a real application's identity — which, since UID3
    /// is what the installer and the data cage key on, would silently overwrite it.
    #[test]
    fn uid3s_are_in_the_development_range() {
        for pr in PROBES {
            assert!(pr.uid3 >= 0xE000_0000, "{} has UID3 {:#x}", pr.name, pr.uid3);
        }
    }

    /// A zero or absent deadline is how a hung probe takes the whole run with it.
    #[test]
    fn every_probe_has_a_deadline_and_a_description() {
        for pr in PROBES {
            assert!(pr.deadline_ms > 0, "{} has no deadline", pr.name);
            assert!(pr.deadline_ms <= 120_000, "{} would stall the run", pr.name);
            assert!(!pr.about.is_empty(), "{} explains nothing", pr.name);
        }
    }

    /// The whole run has to fit in the time somebody will stand there holding the phone.
    /// If the sum of the deadlines exceeds that, the design is wrong rather than the
    /// operator being impatient.
    #[test]
    fn the_worst_case_run_is_bounded() {
        let total: i32 = PROBES.iter().map(|p| p.deadline_ms).sum();
        assert!(total <= 10 * 60_000, "worst case is {total} ms, over ten minutes");
    }

    /// The subdirectory that cost a trip. Pinned so it cannot come back without a test
    /// failing first.
    #[test]
    fn output_goes_flat_into_c_data() {
        assert_eq!(DIR, "", "a subdirectory has to be created, and creating it went wrong");
        assert!(filename(10, "system").starts_with(PREFIX));
    }

    #[test]
    fn filenames_sort_into_reading_order() {
        let mut got: Vec<_> = PROBES.iter().map(|p| filename(p.order, p.name)).collect();
        let unsorted = got.clone();
        got.sort();
        assert_eq!(got.first().unwrap(), "dump-10-system.txt");
        // Sorting by name must agree with sorting by order, or the merged report reads
        // in a different sequence than the directory listing.
        let mut by_order: Vec<_> = PROBES.iter().collect();
        by_order.sort_by_key(|p| p.order);
        let want: Vec<_> = by_order.iter().map(|p| filename(p.order, p.name)).collect();
        assert_eq!(got, want);
        assert_ne!(unsorted, want, "the table is already in order; the test proves nothing");
    }

    #[test]
    fn the_manifest_sorts_first_and_the_merge_last() {
        let manifest = filename(MANIFEST_ORDER, MANIFEST_NAME);
        let merged = filename(MERGED_ORDER, MERGED_NAME);
        for pr in PROBES {
            let f = filename(pr.order, pr.name);
            assert!(manifest < f, "{manifest} should precede {f}");
            assert!(merged > f, "{merged} should follow {f}");
        }
    }

    #[test]
    fn executables_are_looked_for_in_sys_bin() {
        assert_eq!(exe_path("ddsystem.exe"), "C:\\sys\\bin\\ddsystem.exe");
    }
}
