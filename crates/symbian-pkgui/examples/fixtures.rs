//! The invented packages every example on this screen is rendered from.
//!
//! Shared by `preview.rs` and `parity.rs` through `#[path]`, because two copies of a fixture are two
//! fixtures: the day one grows a case the other has not got, the comparison and the picture are of
//! different screens and nothing says so. `autoexamples = false` in `Cargo.toml` is what keeps this
//! file from being mistaken for an example of its own.
//!
//! The data is invented, and has to be: `apps::installed()` reaches the phone's registry, which
//! off-device answers `NotReady`. The names and sizes are ones this project actually carries, because
//! a row is worth nothing if the widths are not the widths a real row has to fit.
#![allow(dead_code)]

use symbian_bootcfg::catalog::{CatEntry, CatalogDb};
use symbian_bootcfg::pkg::{Candidate, ManagedPkg, PkgDb, Version};
use symbian_bootcfg::queue::{Job, JobState, Queue};
use symbian_bootcfg::repo::{FailReason, LastResult, RepoDb};

pub const LAUNCHER: u32 = 0xE0AA_0000;
pub const CAL: u32 = 0xE0CA_0000;
pub const BROWSER: u32 = 0xE0DD_00F7;

pub fn pkgs() -> PkgDb {
    let mut d = PkgDb::default();
    let mut l = ManagedPkg::new(LAUNCHER, "Launcher".into());
    l.installed = Some(Version::new(0, 1, 0));
    l.stamps = true;
    l.settle_s = 5;
    d.ensure(l);

    let mut c = ManagedPkg::new(CAL, "Calendário".into());
    c.installed = Some(Version::new(0, 3, 0));
    c.stamps = true;
    d.ensure(c);

    // Installed once as `new`, so its version is on record and it still does not report one — the
    // case that rolled back a working install until `stamps` became its own fact.
    let mut b = ManagedPkg::new(BROWSER, "browser".into());
    b.installed = Some(Version::new(0, 1, 0));
    d.ensure(b);

    let mut bm = ManagedPkg::new(symbian_bootcfg::BOOTCTL_UID, "Boot manager".into());
    bm.installed = Some(Version::new(0, 1, 0));
    d.ensure(bm);
    d
}

/// What the scan of `C:\Data\_app_install\` found.
pub fn cands() -> Vec<Candidate> {
    vec![
        Candidate {
            dir: "C:\\Data\\_app_install\\".into(),
            file: "launcher-0.2.0.sisx".into(),
            uid3: LAUNCHER,
            version: Version::new(0, 2, 0),
            name: "launcher".into(),
            size: 320_484,
            sha256: None,
        },
        Candidate {
            dir: "C:\\Data\\_app_install\\".into(),
            file: "browser.sis".into(),
            uid3: BROWSER,
            version: Version::new(0, 1, 0),
            name: "browser".into(),
            size: 433_700,
            sha256: Some([0xBB; 32]),
        },
    ]
}

pub fn catalog() -> CatalogDb {
    CatalogDb {
        entries: vec![
            CatEntry {
                repo_id: 1,
                asset: "launcher.sisx".into(),
                name: "launcher".into(),
                version: Version::new(0, 3, 0),
                url: "https://github.com/pizzaria-foundation/home/releases/download/v0.3.0/launcher.sisx".into(),
                size: 331_204,
            },
            CatEntry {
                repo_id: 1,
                asset: "cal.sis".into(),
                name: "cal".into(),
                version: Version::new(0, 3, 1),
                url: "https://github.com/pizzaria-foundation/home/releases/download/v0.3.0/cal.sis".into(),
                size: 215_472,
            },
        ],
    }
}

pub fn repos() -> RepoDb {
    let mut d = RepoDb::default();
    let a = d
        .add_github("pizzaria-foundation".into(), "home".into())
        .unwrap();
    let b = d.add_github("BurntSushi".into(), "ripgrep".into()).unwrap();
    let c = d.add_github("rust-lang".into(), "rust".into()).unwrap();
    d.get_mut(a).unwrap().last = LastResult::Found(2);
    // The two answers a person has to be able to act on: wait an hour, or fix the name.
    d.get_mut(b).unwrap().last = LastResult::Failed(FailReason::NoPackages);
    d.get_mut(c).unwrap().last = LastResult::Failed(FailReason::RateLimited);
    d
}

/// A queue mid-flight: one running with a known size, one whose size the server never sent, one
/// waiting, one that failed and can be resumed.
pub fn queue() -> Queue {
    let mut q = Queue::default();
    q.push(Job::download(
        1,
        1,
        "https://github.com/p/h/releases/download/v0.3.0/launcher.sisx".into(),
        "launcher.sisx".into(),
        331_204,
    ));
    q.push(Job::download(
        2,
        1,
        "https://github.com/p/h/releases/download/v0.3.0/cal.sis".into(),
        "cal.sis".into(),
        215_472,
    ));
    q.push(Job::download(
        3,
        1,
        "https://example/telegram.sis".into(),
        "telegram.sis".into(),
        244_572,
    ));
    q.start(1);
    q.advance(1, 189_000);
    q.finish(3, JobState::Failed, -33);
    q
}

/// A phone that has just been set up: nothing managed, nothing on offer, no repositories, no queue.
///
/// Every section's empty state at once, which is the only way to render the four sentences
/// `empty_text` holds — and they are four different sentences, so one of them is not a check on the
/// other three.
pub fn nothing() -> (PkgDb, Vec<Candidate>, CatalogDb, RepoDb, Queue) {
    (
        PkgDb::default(),
        Vec::new(),
        CatalogDb {
            entries: Vec::new(),
        },
        RepoDb::default(),
        Queue::default(),
    )
}

/// More packages than fit, so the list scrolls and the top row on screen is not row zero.
///
/// A comparison at offset zero says nothing about the arithmetic that puts a row on screen; the
/// scroll offset is derived from the selection and the viewport, and it is the one number a rewrite
/// of the rows could quietly change.
pub fn many_pkgs() -> PkgDb {
    let mut d = pkgs();
    for i in 0..12u32 {
        let mut p = ManagedPkg::new(0xE100_0000 + i, format!("Package {i}"));
        p.installed = Some(Version::new(0, 1, i as u16));
        p.pinned = i % 3 == 0;
        d.ensure(p);
    }
    d
}

/// The queue with the running job's size unknown, which is a different meter and not a bar at zero.
///
/// `Content-Length` is optional, and the row that does not know its total draws the indeterminate
/// meter — a state the main fixture cannot reach, because its running job knows how big it is.
pub fn queue_unknown() -> Queue {
    let mut q = Queue::default();
    q.push(Job::download(
        1,
        1,
        "https://example/launcher.sisx".into(),
        "launcher.sisx".into(),
        0,
    ));
    q.push(Job::download(
        2,
        1,
        "https://example/cal.sis".into(),
        "cal.sis".into(),
        215_472,
    ));
    q.start(1);
    q.advance(1, 40_000);
    q
}
