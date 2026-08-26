//! The packages screen: what is installed, what is available, where it comes from, what is arriving.
//!
//! ## Why this is a screen and not a fifth tab
//!
//! It was a tab on the boot manager, and four subjects were competing for one strip on a 320-pixel
//! screen with restart policy. The result is on the record: a person about to replace the binary of
//! their home screen was reading punctuation at the end of a row label, and twice in one afternoon the
//! wrong thing took over the tab. So the boot manager keeps its four tabs — all of them about *boot* —
//! and Pkgs became the door to this.
//!
//! Four sections, because there are four questions and they have different answers:
//!
//! | | |
//! |---|---|
//! | **Installed** | what we manage and believe about it |
//! | **Available** | the union of the repositories' catalogue and the `_app_install` folder |
//! | **Repos** | where to look, and what happened last time we did |
//! | **Downloads** | the queue, with a bar per item |
//!
//! ## The rows are declared; the screen around them is not
//!
//! Each row is built from `symbian-decl-ui` widgets — a `Chip` that is *measured* beside the title
//! rather than spelled into the end of it, a `ProgressBar` or a `Spinner`, and text whose colour comes
//! from [`Ground`] instead of from an `if selected` per line. Everything else on this screen — the
//! title bar, the tab strip, the softkey bar, the sheet, the menu and the prompt — stays imperative,
//! because those are already single calls into shared widgets and rewriting a working screen buys a
//! working screen with new bugs in it.
//!
//! `examples/parity.rs` is what says the rewrite moved nothing: it draws every state twice, once
//! through each painter, and compares the buffers.
//!
//! ## No I/O, and no decisions it cannot see
//!
//! Same rule as `symbian_bootctl`: this crate is handed the databases and hands back a
//! [`PkgRequest`]. Every file read, every socket and every install is `apps/bootctl`'s. That is what
//! makes the whole screen — including the download queue's states — testable on a host with no phone.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use symbian_bootcfg::catalog::{CatEntry, CatalogDb};
use symbian_bootcfg::pkg::{self, Candidate, Offer, PkgDb, Version};
use symbian_bootcfg::queue::{JobKind, JobState, Queue};
use symbian_bootcfg::repo::{LastResult, RepoDb};
use symbian_decl_ui::cache::UiCache;
use symbian_decl_ui::constraints::Constraints;
use symbian_decl_ui::layout::{self, CrossAlign};
use symbian_decl_ui::spacing::Gap;
use symbian_decl_ui::theme::FontRole;
use symbian_decl_ui::widgets::{
    Chip as ChipNode, Column, Ink, Node, ProgressBar, Row, Spinner, Text,
};
use symbian_ui::menu::{self, Menu, MenuAction};
use symbian_ui::{
    chrome, Align, Canvas, Chip, Ground, Handled, Key, KeyEvent, ListState, Meter, Rect, Sheet,
    SheetAction, SheetRow, Softkey, Tabs, TextAnswer, TextPrompt, Theme, Tone, Uniform,
};

/// Three, and it was four.
///
/// `Installed` and `Available` were the same subject split by a property of the package rather than
/// by a question the user has — and the split made "is this an update or a new install?" something
/// you answered by noticing which tab you were on. One list, one row per package, with the answer on
/// the row. See [`PkgRow`].
const TABS: [&str; 3] = ["Packages", "Repos", "Downloads"];
const TAB_PACKAGES: usize = 0;
const TAB_REPOS: usize = 1;
/// Named for the tests and for the `_` arm's reader; the routing reaches it by fallthrough.
#[allow(dead_code)]
const TAB_DOWNLOADS: usize = 2;

/// What the screen is asking the application to do.
///
/// Nothing here happens in this crate. A request is a value, which is why a test can assert what a
/// keypress *meant* instead of watching for a side effect somewhere else.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum PkgRequest {
    /// Install a package already on disk, by its candidate index.
    Install(usize),

    /// Fetch this, then install it.
    Download(CatEntry),
    /// Register `owner/repo`, or whatever the person pasted — the app parses it.
    AddRepo(String),
    RemoveRepo(u16),
    /// Ask one repository what it has now.
    Check(u16),
    /// Ask all of them.
    CheckAll,
    Retry(u16),
    Cancel(u16),
    /// Drop the finished rows from the queue.
    ClearDone,
    /// Hold this package at its installed version, or release it.
    TogglePin(u32),
    /// Step the reopen-after-install delay.
    StepReopen(u32),
    /// Leave the screen.
    Back,
}

/// Which row painter [`PkgScreen::draw_as`] uses.
///
/// Two arms so `examples/parity.rs` can render both and diff them. `Declared` is what `draw` uses
/// and what ships; `Imperative` is the row painter that shipped before the rows became widgets, kept
/// runnable so the rewrite has something to be measured against.
#[doc(hidden)]
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Rows {
    /// Built from `symbian-decl-ui` widgets and laid out by the layout pass.
    Declared,
    /// Rects and ink computed by hand, as the screen shipped.
    Imperative,
}

/// A menu item, per section. One enum so the routing has one shape.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Item {
    Install,
    Details,
    Pin,
    Reopen,
    AddRepo,
    CheckThis,
    CheckAll,
    RemoveRepo,
    RetryJob,
    CancelJob,
    ClearDone,
}

/// One line, whichever section it is in.
/// One package, whatever we know about it, from wherever we know it.
///
/// # Why this type exists
///
/// The screen used to have an Installed tab reading `pkgs.pkgs` and an Available tab reading
/// `cat.entries` and then `cands` — three collections, and a row was an index into whichever one the
/// active tab implied. That made "is this an update or a new install?" a question you answered by
/// noticing which tab you were on, and it put the same package on two screens without either saying
/// so.
///
/// A row is now a *join by UID3* over all three, built once. What was two tabs is one list where
/// every package appears exactly once, carrying both halves of its own story: what is installed, and
/// what is on offer.
struct PkgRow {
    uid3: u32,
    name: String,
    /// The version on the handset, or `None` for a package that is only on offer — and also for one
    /// that is installed but whose version nobody witnessed. Those two are different states and the
    /// row tells them apart by whether `installed_row` is set.
    installed: Option<Version>,
    /// Whether this package has a row in the database at all.
    managed: bool,
    pinned: bool,
    is_self: bool,
    /// What can be done and where the bytes are, or `None` when there is nothing on offer.
    offer: Option<(Offer, Source)>,
}

/// Where an offer's bytes are, as an index into the collection that holds them.
///
/// An index rather than a clone, because `PkgRequest` is already expressed in those terms — the
/// application takes `Install(i)` and `Download(entry)` — and translating twice is how the two
/// drift apart.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Source {
    /// A file already on the handset: `_app_install`, or one this app downloaded. Index into `cands`.
    Local(usize),
    /// A repository's catalogue. Index into `cat.entries`. Needs downloading before installing.
    Remote(usize),
}

impl PkgRow {
    /// How urgently this row wants attention, lowest first. The sort key's first half.
    ///
    /// The order is the question a person opens this screen to ask: *what do I need to do?* An
    /// update is the only thing that is both actionable and about something already relied on, so it
    /// goes first. A rebuild is the same version with different bytes — actionable, but only during
    /// development. `Unknown` is the honest "cannot tell", and it is above `ok` because it is a
    /// question rather than an answer.
    fn urgency(&self) -> u8 {
        match self.offer.map(|(o, _)| o) {
            Some(Offer::Upgrade) => 0,
            Some(Offer::New) => 1,
            Some(Offer::Rebuild) => 2,
            Some(Offer::Unknown) => 3,
            _ => 4,
        }
    }

    /// The word on the chip, and how loud it is.
    fn marker(&self) -> (&'static str, Tone) {
        if self.is_self {
            return ("self", Tone::Calm);
        }
        if self.pinned {
            return ("pinned", Tone::Calm);
        }
        match self.offer.map(|(o, _)| o) {
            Some(Offer::Upgrade) => ("update", Tone::Fresh),
            Some(Offer::New) => ("new", Tone::Fresh),
            Some(Offer::Rebuild) => ("rebuild", Tone::Warn),
            Some(Offer::Unknown) => ("?", Tone::Warn),
            _ => ("ok", Tone::Calm),
        }
    }
}

struct Line {
    text: String,
    /// The chip on the right, when the row has a state worth seeing rather than reading.
    chip: Option<(String, Tone)>,
    /// A bar under the text, for a download in flight.
    meter: Option<Meter>,
    /// The second line, for a bar's numbers or a repository's last answer.
    detail: String,
}

pub struct PkgScreen {
    tabs: Tabs,
    list: ListState,
    /// One cursor per section, because they are four different lists and a shared cursor would put
    /// somebody on row 7 of a list with two rows.
    cursor: [usize; 4],

    pkgs: PkgDb,
    cands: Vec<Candidate>,
    cat: CatalogDb,
    repos: RepoDb,
    queue: Queue,

    sheet: Option<Sheet>,
    /// Which package the open sheet is about, so its actions can be attributed.
    sheet_uid: u32,
    /// The release notes on show, or `None` when they are not.
    ///
    /// A *changelog*, and it is worth saying what this is not: it was a viewer for the application's
    /// debug log for one build, which was the wrong thing in the right place. This screen is a store.
    /// What somebody looking at 0.3.0 wants is what is in 0.3.0, not what the running copy printed
    /// to a file.
    notes: Option<String>,
    /// Whether the update scan has run yet.
    ///
    /// It is deferred by a millisecond so the screen appears before a 400 ms scan of every `.sis`
    /// on the handset — and with two tabs that was invisible, because only Available changed. With
    /// one list it is not: the rows are sorted by urgency, so the scan landing *reorders them*, and
    /// what a person sees is a list that rearranges itself under the cursor a moment after opening.
    ///
    /// So the list waits. One transition, from "looking" to the answer, instead of a list that
    /// shuffles — and the pause it costs is the same 400 ms that was already being spent.
    ///
    /// **True by default**, and the inversion is deliberate: being handed data means somebody
    /// looked. Only a caller that defers says otherwise, and there is one — everything else
    /// (previews, harnesses, tests) would have had to learn a step that has nothing to do with it.
    scanned: bool,
    /// Which of [`Self::versions_of`] the open sheet is showing, newest at 0. Left and Right step
    /// it, and Install acts on it — so the arrows are not a way to browse, they are how you choose
    /// what gets installed.
    sheet_pick: usize,
    menu: Option<Menu<Item>>,
    prompt: Option<TextPrompt>,

    /// Advances once per redraw, for the indeterminate meter. The screen does not own a timer — the
    /// application already has the one that made this redraw.
    phase: u8,
    request: Option<PkgRequest>,
    back: bool,
}

impl PkgScreen {
    pub fn new(
        pkgs: PkgDb,
        cands: Vec<Candidate>,
        cat: CatalogDb,
        repos: RepoDb,
        queue: Queue,
    ) -> Self {
        Self {
            tabs: Tabs::new(),
            list: ListState::new(),
            cursor: [0; 4],
            pkgs,
            cands,
            cat,
            repos,
            queue,
            sheet: None,
            sheet_uid: 0,
            notes: None,
            scanned: true,
            sheet_pick: 0,
            menu: None,
            prompt: None,
            phase: 0,
            request: None,
            back: false,
        }
    }

    /// Hand it fresh data. Called after anything the application did on the screen's behalf.
    ///
    /// The cursor is kept on the **package** rather than on the row number. New data reorders the
    /// list — that is what sorting by urgency means — and a cursor that stayed on row 3 would be
    /// pointing at a different package than the one somebody was looking at. It has to be read
    /// before the data changes and put back after, because the row number is only meaningful
    /// against the list it came from.
    pub fn refresh(
        &mut self,
        pkgs: PkgDb,
        cands: Vec<Candidate>,
        cat: CatalogDb,
        repos: RepoDb,
        queue: Queue,
    ) {
        let anchor =
            (self.section() == TAB_PACKAGES).then(|| self.rows().get(self.list.selected).map(|r| r.uid3)).flatten();
        self.pkgs = pkgs;
        self.cands = cands;
        self.cat = cat;
        self.repos = repos;
        self.queue = queue;
        self.scanned = true;
        if let Some(uid) = anchor {
            // Gone from the list entirely — uninstalled while the screen was open — leaves the
            // cursor where it was, which is the least surprising of the wrong answers.
            if let Some(i) = self.rows().iter().position(|r| r.uid3 == uid) {
                self.list.selected = i;
            }
        }
    }

    /// The request the screen wants performed. Consumed: an action happens once.
    pub fn take_request(&mut self) -> Option<PkgRequest> {
        self.request.take()
    }

    pub fn back(&self) -> bool {
        self.back
    }

    /// How often the caller should redraw, in milliseconds, or `None` when nothing is moving.
    ///
    /// `Some` only while a download is running: an animated bar with nothing behind it is a phone
    /// waking up for no reason, and this screen has nothing to say between downloads.
    pub fn animating(&self) -> bool {
        self.queue.running().is_some()
    }

    fn section(&self) -> usize {
        self.tabs.active()
    }

    // ------------------------------------------------------------------ rows

    /// Every package this handset knows about, once each, sorted by what it wants from you.
    ///
    /// The join is by UID3 and the order of the three passes is the precedence: what is *installed*
    /// establishes the row, what is *local* claims the offer, and the catalogue fills in only what
    /// neither of those had. Preferring a local file over a catalogue entry is not a new rule — it
    /// is what `install_request` already did, moved to where the row is built so the row and the
    /// request cannot disagree about which copy they mean.
    fn rows(&self) -> Vec<PkgRow> {
        let mut out: Vec<PkgRow> = self
            .pkgs
            .pkgs
            .iter()
            .map(|p| PkgRow {
                uid3: p.uid3,
                name: p.name.clone(),
                installed: p.installed,
                managed: true,
                pinned: p.pinned,
                is_self: p.is_self(),
                // `offer_for` already refuses a pinned package and this one, so the marker and the
                // menu agree without either restating the rule.
                offer: self
                    .pkgs
                    .offer_for(p.uid3, &self.cands)
                    .and_then(|(o, c)| {
                        self.cands.iter().position(|x| x.file == c.file).map(|i| (o, Source::Local(i)))
                    }),
            })
            .collect();

        // A local file for something not in the database at all: a new install, and the row it needs
        // does not exist yet.
        for (i, c) in self.cands.iter().enumerate() {
            if out.iter().any(|r| r.uid3 == c.uid3) {
                continue;
            }
            out.push(PkgRow {
                uid3: c.uid3,
                name: c.name.clone(),
                installed: None,
                managed: false,
                pinned: false,
                is_self: false,
                offer: Some((Offer::New, Source::Local(i))),
            });
        }

        // And the catalogue, last, for a package with no copy on the handset. A row that already has
        // a local offer keeps it: bytes here beat bytes to fetch.
        //
        // # Why this one join is by name, and where it stops working
        //
        // Because a catalogue entry has no UID3 to join on. `symbian_bootcfg::github` says why in
        // its own words — *"identity comes from inside the .sis rather than from anything the
        // service says"* — so a release asset is a name, a size and a URL until its bytes arrive.
        //
        // Names usually agree: a package adopted through `start_install` is named from the `.sis`
        // itself, and that is the same string the release asset carries. They disagree when a row
        // was adopted from a *stamp* instead, because `load_pkgs` names those from the AppArc
        // caption — so `Calendário` in the database against `cal` in the catalogue, for one package.
        //
        // When that happens the screen shows two rows, and that is the honest outcome rather than a
        // defect to paper over: we do not know they are the same thing, and guessing from a prefix
        // would eventually merge two packages that are not. The second row says where it came from,
        // and installing it resolves the question the only way it can be resolved — by reading the
        // UID3 out of the bytes.
        for (i, e) in self.cat.entries.iter().enumerate() {
            // Case-insensitively, and that is not fussiness. A catalogue entry's name comes from a
            // release asset (`launcher.sisx`) and an installed row's comes from inside the package,
            // and the two agreeing on capitalisation is a coincidence rather than a rule. Matching
            // exactly put the same package on two rows, one saying `update` and one saying `new` —
            // the exact failure a join by key has, found by looking at the render.
            match out.iter_mut().find(|r| r.name.eq_ignore_ascii_case(&e.name)) {
                Some(r) if r.offer.is_none() && !r.pinned && !r.is_self => {
                    // The catalogue's version against what is installed, which is the only
                    // comparison available here — a catalogue entry carries no digest.
                    let o = match r.installed {
                        Some(v) if e.version > v => Offer::Upgrade,
                        Some(_) => continue,
                        None => Offer::New,
                    };
                    r.offer = Some((o, Source::Remote(i)));
                }
                Some(_) => {}
                None => out.push(PkgRow {
                    uid3: 0,
                    name: e.name.clone(),
                    installed: None,
                    managed: false,
                    pinned: false,
                    is_self: false,
                    offer: Some((Offer::New, Source::Remote(i))),
                }),
            }
        }

        // Urgency, then name. Stable within a group, so nothing dances between two openings that
        // found the same thing.
        out.sort_by(|a, b| a.urgency().cmp(&b.urgency()).then_with(|| a.name.cmp(&b.name)));
        out
    }

    /// One row per package, the join in [`Self::rows`] turned into what the list draws.
    ///
    /// The second line is only there when there is something to say about where an offer comes from,
    /// which is what keeps the list dense. The handset made that point once already:
    ///
    /// > *"the listing is spaced out"*. Installed rows carry no second line, so a third of the
    /// > screen was blank and four items fitted where eight do.
    fn package_lines(&self) -> Vec<Line> {
        self.rows()
            .into_iter()
            .map(|r| {
                let (word, tone) = r.marker();
                let have = match r.installed {
                    Some(v) => format!("{v}"),
                    // Two different silences, told apart. A package only on offer has no installed
                    // version because it is not installed; a managed one has none because nobody
                    // witnessed it, and saying "unknown" is the honest half of that.
                    None if r.managed => String::from("unknown"),
                    None => String::new(),
                };
                let want = match r.offer {
                    Some((_, Source::Local(i))) => self.cands.get(i).map(|c| c.version),
                    Some((_, Source::Remote(i))) => self.cat.entries.get(i).map(|e| e.version),
                    None => None,
                };
                let text = match (have.is_empty(), want) {
                    // Installed and on offer: both numbers, because the gap is the reason to act.
                    (false, Some(v)) if Some(v) != r.installed => {
                        // `>` and not `\u{2192}`: the arrow glyph is **absent from this
                        // handset's atlases**, and a missing glyph draws nothing at all — which is
                        // how the boot manager's move-mode arrows turned out to be invisible. The
                        // render is the only thing that says so, and it said so here too.
                        format!("{}  {} > {}", r.name, have, v)
                    }
                    (false, _) => format!("{}  {}", r.name, have),
                    (true, Some(v)) => format!("{}  {}", r.name, v),
                    (true, None) => r.name.clone(),
                };
                // A download already in flight outranks whatever the offer was: "queued" is
                // something happening now, and the chip is where states go. Saying `new` beside a
                // row that is already being fetched invites pressing it again.
                let chip = if self.is_queued(&r) {
                    (String::from("queued"), Tone::Busy)
                } else {
                    (String::from(word), tone)
                };
                Line { text, chip: Some(chip), meter: None, detail: self.source_note(&r) }
            })
            .collect()
    }

    /// Whether this row's offer is already being fetched.
    fn is_queued(&self, r: &PkgRow) -> bool {
        match r.offer {
            Some((_, Source::Remote(i))) => match self.cat.entries.get(i) {
                Some(e) => self.queue.jobs.iter().any(|j| j.state.pending() && j.url == e.url),
                None => false,
            },
            // A local file is not queued: it is already here, which is the whole difference.
            _ => false,
        }
    }

    /// Where an offer's bytes are, in a person's terms. Empty when there is no offer, which is what
    /// collapses the row to one line.
    fn source_note(&self, r: &PkgRow) -> String {
        match r.offer {
            Some((_, Source::Local(i))) => match self.cands.get(i) {
                Some(c) => format!("local \u{00b7} {} KB", c.size.div_ceil(1024)),
                None => String::new(),
            },
            Some((_, Source::Remote(i))) => match self.cat.entries.get(i) {
                Some(e) => {
                    let from = self
                        .repos
                        .get(e.repo_id)
                        .map(|x| x.label())
                        .unwrap_or_else(|| String::from("a repository"));
                    let queued =
                        self.queue.jobs.iter().any(|j| j.state.pending() && j.url == e.url);
                    let what = if queued { "queued" } else { &from };
                    format!("{}  \u{00b7} {} KB", what, e.size.div_ceil(1024))
                }
                None => String::new(),
            },
            None => String::new(),
        }
    }

    fn repo_lines(&self) -> Vec<Line> {
        if self.repos.repos.is_empty() {
            return Vec::new();
        }
        self.repos
            .repos
            .iter()
            .map(|r| {
                let (word, tone) = match r.last {
                    LastResult::Never => ("new", Tone::Calm),
                    LastResult::Found(0) => ("empty", Tone::Warn),
                    LastResult::Found(n) => {
                        return Line {
                            text: r.label(),
                            chip: Some((format!("{n}"), Tone::Fresh)),
                            meter: None,
                            detail: r.last.describe(),
                        }
                    }
                    LastResult::Failed(_) => ("failed", Tone::Warn),
                };
                Line {
                    text: r.label(),
                    chip: Some((String::from(word), tone)),
                    meter: None,
                    detail: r.last.describe(),
                }
            })
            .collect()
    }

    fn download_lines(&self) -> Vec<Line> {
        self.queue
            .jobs
            .iter()
            .map(|j| {
                let running = j.state == JobState::Running;
                let meter = running.then(|| Meter::of(j.fraction(), self.phase));
                let (word, tone) = match j.state {
                    JobState::Running => ("now", Tone::Busy),
                    JobState::Queued => ("waiting", Tone::Calm),
                    JobState::Done => ("done", Tone::Fresh),
                    JobState::Failed => ("retry?", Tone::Warn),
                    JobState::GaveUp => ("gave up", Tone::Warn),
                    JobState::Cancelled => ("stopped", Tone::Calm),
                };
                let detail = if j.kind == JobKind::Check {
                    String::from("checking the repository")
                } else if running || j.got > 0 {
                    Meter::label(j.got, j.total)
                } else {
                    String::from(j.state.describe())
                };
                Line {
                    text: j.name.clone(),
                    chip: Some((String::from(word), tone)),
                    meter,
                    detail,
                }
            })
            .collect()
    }

    fn lines(&self) -> Vec<Line> {
        match self.section() {
            TAB_PACKAGES => self.package_lines(),
            TAB_REPOS => self.repo_lines(),
            _ => self.download_lines(),
        }
    }

    /// How tall a row is in this section: two lines where there is a second line, one where there is
    /// not.
    ///
    /// It was two everywhere, on the reasoning that a constant height keeps things from moving when
    /// somebody changes section. That was wrong and the handset said so — *"the listing is spaced
    /// out"*. Installed rows carry no second line, so a third of the screen was blank and four items
    /// fitted where eight do. A tidy invariant nobody asked for is not worth a third of a 240-pixel
    /// screen.
    ///
    /// Still uniform *within* a section, which is what `Uniform` needs and what keeps scrolling
    /// arithmetic honest.
    fn row_height_for(&self, lines: &[Line], theme: &Theme<'_>) -> i32 {
        let two = lines
            .iter()
            .any(|l| !l.detail.is_empty() || l.meter.is_some());
        let mut h = theme.fonts.body.line_height() + 4;
        if two {
            h += theme.fonts.small.line_height() + 2;
        }
        h
    }

    fn row_height(&self, theme: &Theme<'_>) -> i32 {
        self.row_height_for(&self.lines(), theme)
    }

    fn empty_text(&self) -> &'static str {
        match self.section() {
            TAB_PACKAGES => {
                "Nothing here yet. Add a repository, or copy a .sis into _app_install."
            }
            TAB_REPOS => "No repositories. Options \u{2192} Add repository.",
            _ => "Nothing downloading.",
        }
    }

    /// Every version of one package this handset could install right now, newest first.
    ///
    /// Three places hold one: the update directories, the known-good copy `bootd` keeps for
    /// rolling back, and a repository's catalogue. The first two are files already here; the third
    /// has to be fetched.
    ///
    /// # Why the sheet has this and the list does not
    ///
    /// The list answers *what should I do*, and for that there is one answer per package — the
    /// newest thing on offer, which is what `best_for` picks. The sheet answers *what are my
    /// options*, and that is a different question with a longer answer: a package can have three
    /// files on disk and the one you want may be the old one, because the new one is what broke.
    ///
    /// Without this the older files were invisible. They were scanned, ranked, and then all but the
    /// winner were dropped — so a rollback meant deleting a file from the card to make a different
    /// one win.
    fn versions_of(&self, uid3: u32, name: &str) -> Vec<(Version, Source)> {
        let mut out: Vec<(Version, Source)> = self
            .cands
            .iter()
            .enumerate()
            .filter(|(_, c)| c.uid3 == uid3)
            .map(|(i, c)| (c.version, Source::Local(i)))
            .collect();
        for (i, e) in self.cat.entries.iter().enumerate() {
            if e.name.eq_ignore_ascii_case(name) {
                out.push((e.version, Source::Remote(i)));
            }
        }
        // Newest first, and a local copy before a remote one at the same version: bytes here beat
        // bytes to fetch, which is the rule the row already follows.
        out.sort_by(|a, b| {
            b.0.cmp(&a.0).then_with(|| match (a.1, b.1) {
                (Source::Local(_), Source::Remote(_)) => core::cmp::Ordering::Less,
                (Source::Remote(_), Source::Local(_)) => core::cmp::Ordering::Greater,
                _ => core::cmp::Ordering::Equal,
            })
        });
        out
    }

    /// The versions the open sheet can step through.
    fn sheet_versions(&self) -> Vec<(Version, Source)> {
        match self.pkgs.get(self.sheet_uid) {
            Some(p) => self.versions_of(p.uid3, &p.name),
            None => Vec::new(),
        }
    }

    /// Rebuild the sheet at the current pick, keeping it open.
    fn reopen_sheet(&mut self) {
        let uid = self.sheet_uid;
        let pick = self.sheet_pick;
        self.build_sheet(uid);
        self.sheet_pick = pick;
    }

    // ------------------------------------------------------------------ the sheet

    /// The detail view of the focused package.
    ///
    /// This is the screen the row label used to try to be. Everything that was punctuation is a line
    /// with a name.
    fn open_sheet(&mut self) {
        let i = self.list.selected;
        // Through the row, not into the database by list index. The list is a *sorted join* now, so
        // row 3 is whatever sorted third — and indexing `pkgs.pkgs[3]` would open the sheet on a
        // different package than the one under the cursor. That is the failure this shape has, and
        // it is silent: both are packages, and both draw.
        let Some(uid) = self.rows().get(i).map(|r| r.uid3) else {
            return;
        };
        // Opening always starts at the newest, which is what somebody who pressed Select wants to
        // see. Stepping is a deliberate act after that.
        self.sheet_pick = 0;
        self.build_sheet(uid);
    }

    /// The sheet itself, at whatever version [`Self::sheet_pick`] names.
    fn build_sheet(&mut self, uid: u32) {
        let versions = match self.pkgs.get(uid) {
            Some(p) => self.versions_of(p.uid3, &p.name),
            None => Vec::new(),
        };
        let Some(p) = self.pkgs.get(uid) else {
            return;
        };
        let picked = versions.get(self.sheet_pick.min(versions.len().saturating_sub(1))).copied();
        let offer = self.pkgs.offer_for(p.uid3, &self.cands);
        let mut s = Sheet::new(
            p.name.clone(),
            match p.installed {
                Some(v) => format!("{v}"),
                None => String::from("not installed"),
            },
        )
        .row(SheetRow::pair("UID", format!("{:08X}", p.uid3)))
        .row(SheetRow::pair(
            "Installed",
            p.installed
                .map(|v| format!("{v}"))
                .unwrap_or_else(|| String::from("unknown")),
        ));

        // The version on show, and how many there are to step through. `< 0.2.0 >` rather than a
        // bare number, because an arrow that does nothing is worse than no arrow — this says the
        // key works before anybody presses it.
        if let Some((v, src)) = picked {
            let n = versions.len();
            let label = if n > 1 {
                format!("\u{003c} {v} \u{003e}   {} of {n}", self.sheet_pick + 1)
            } else {
                format!("{v}")
            };
            let (where_, size) = match src {
                Source::Local(i) => match self.cands.get(i) {
                    Some(c) => (c.file.clone(), c.size),
                    None => (String::new(), 0),
                },
                Source::Remote(i) => match self.cat.entries.get(i) {
                    Some(e) => (
                        self.repos
                            .get(e.repo_id)
                            .map(|r| r.label())
                            .unwrap_or_else(|| String::from("a repository")),
                        e.size,
                    ),
                    None => (String::new(), 0),
                },
            };
            s = s
                .row(SheetRow::pair("Available", label))
                .row(SheetRow::pair("Size", format!("{} KB", size.div_ceil(1024))))
                .row(SheetRow::pair(
                    match src {
                        Source::Local(_) => "File",
                        Source::Remote(_) => "From",
                    },
                    where_,
                ));
        }

        if let Some((o, _)) = offer {
            s = s
                .row(SheetRow::chip(
                    "Offer",
                    String::from(match o {
                        Offer::Upgrade => "a newer version",
                        Offer::Rebuild => "same version, different build",
                        Offer::New => "not installed yet",
                        Offer::Unknown => "same version, cannot tell",
                        _ => "nothing",
                    }),
                    match o {
                        Offer::Upgrade | Offer::New => Tone::Fresh,
                        _ => Tone::Warn,
                    },
                ));
        }

        // On or off. It carried a number until the number was found to be describing a fallback
        // floor rather than the wait — the supervisor ends the wait when it *observes* the installer
        // close, not when a clock runs out. `settings::reopen_label` holds that argument in full.
        s = s.row(SheetRow::Gap).row(SheetRow::pair(
            "Reopen after install",
            String::from(if self.pkgs.reopen(p.uid3).is_some() { "yes" } else { "no" }),
        ));
        s = s.row(SheetRow::chip(
            "Held",
            String::from(if p.pinned { "pinned" } else { "no" }),
            if p.pinned { Tone::Warn } else { Tone::Calm },
        ));

        // The consequence, in a sentence. The one thing on this screen that is not a fact.
        s = s.row(SheetRow::Gap).row(SheetRow::note(if p.is_self() {
            String::from(
                "This is the boot manager's own package. It cannot be installed from here: there \
                 would be nobody left to finish the update.",
            )
        } else if self.pkgs.reopen(p.uid3).is_some() && p.stamps {
            String::from(
                "It must report its new version to count. If it does not, the version that was \
                 working comes back.",
            )
        } else {
            String::from(
                "It will be installed and left alone \u{2014} not reopened, and not verified.",
            )
        }));

        // On whenever there is a version to act on, and not only when `offer_for` says one is
        // *newer*. Those are different questions: the offer answers "is there something you should
        // do", and this answers "can you do it" — and rolling back to an older file is precisely the
        // case where the second is yes and the first is no.
        if picked.is_some() && !p.is_self() {
            s = s.action(match picked.map(|(_, src)| src) {
                Some(Source::Remote(_)) => "Get",
                _ => "Install",
            });
        }
        s = s.action(if p.pinned {
            "Release"
        } else {
            "Hold at this version"
        });
        s = s.action("Reopen after install");
        // Only when there is something to read. A release that published no notes, or a copy that
        // came off the card rather than from a repository, has nothing to say — and an action that
        // opens an empty page is worse than an action that is not there.
        if picked
            .and_then(|(_, src)| match src {
                Source::Remote(i) => self.cat.entries.get(i),
                Source::Local(_) => None,
            })
            .is_some_and(|e| !e.notes.trim().is_empty())
        {
            s = s.action("What\u{2019}s new");
        }

        self.sheet_uid = p.uid3;
        self.sheet = Some(s);
    }

    /// Say the scan has not run yet, so the list waits instead of drawing a half-answer.
    ///
    /// Separate from handing the candidates in, because "no candidates" and "not looked yet" are
    /// different states that produce the same empty `Vec` — and drawing "nothing here" at a handset
    /// that has not looked would be a lie with a 400 ms shelf life.
    ///
    /// [`Self::refresh`] clears it: fresh data is the answer this was waiting for.
    pub fn mark_scanning(&mut self) {
        self.scanned = false;
    }

    fn on_sheet_action(&mut self, index: usize) {
        let uid = self.sheet_uid;
        let picked = self.sheet_versions().get(self.sheet_pick).copied();
        let has_install = self
            .pkgs
            .get(uid)
            .map(|p| picked.is_some() && !p.is_self())
            .unwrap_or(false);
        // The actions were added conditionally, so the index means different things depending on
        // whether Install is there. Resolved here rather than by remembering, because the sheet is
        // rebuilt every time it opens.
        let logical = if has_install { index } else { index + 1 };
        self.request = match logical {
            // The version on show, not the one `offer_for` would have chosen. That is the whole
            // point of the arrows: what the sheet says it will install is what it installs.
            0 => match picked.map(|(_, src)| src) {
                Some(Source::Local(i)) => Some(PkgRequest::Install(i)),
                Some(Source::Remote(i)) => {
                    self.cat.entries.get(i).cloned().map(PkgRequest::Download)
                }
                None => None,
            },
            1 => Some(PkgRequest::TogglePin(uid)),
            2 => Some(PkgRequest::StepReopen(uid)),
            // The notes are already here — they arrived with the catalogue — so this asks the
            // application for nothing. The sheet stays open behind them, so closing comes back to
            // the package rather than to the list.
            _ => {
                self.notes = picked
                    .and_then(|(_, src)| match src {
                        Source::Remote(i) => self.cat.entries.get(i),
                        Source::Local(_) => None,
                    })
                    .map(|e| e.notes.clone());
                None
            }
        };
        if self.notes.is_none() {
            self.sheet = None;
        }
    }

    /// What a release said about itself, as a page you read and leave.
    ///
    /// From the **top**, and that is the difference from the debug-log viewer this replaced for one
    /// build: release notes lead with what matters, and a changelog read backwards is not a
    /// changelog. Plain lines and no list widget, because there is nothing to select and a cursor
    /// suggests pressing it does something.
    fn draw_notes(&self, c: &mut Canvas<'_>, screen: Rect, theme: &Theme<'_>, text: &str) {
        chrome::clear(c, theme);
        let name = self.pkgs.get(self.sheet_uid).map(|p| p.name.clone()).unwrap_or_default();
        let f = chrome::Frame::split(screen, theme, true, true);
        chrome::title_bar(c, f.title, theme, &name, Some("what\u{2019}s new"));
        chrome::softkey_bar(c, f.softkeys, theme, [None, None, Some("Back")]);

        if text.trim().is_empty() {
            chrome::placeholder(c, f.content, theme, "This release said nothing about itself.");
            return;
        }
        let body = theme.fonts.body;
        let step = body.line_height();
        let rows = (f.content.height() / step).max(1) as usize;
        let mut y = f.content.y0 + body.ascent();
        for l in text.lines().take(rows) {
            c.draw_text(
                symbian_gfx::Point::new(f.content.x0 + theme.metrics.pad, y),
                l,
                body,
                theme.palette.text,
            );
            y += step;
        }
    }

    // ------------------------------------------------------------------ the menu

    fn open_menu(&mut self) {
        let mut m = Menu::new();
        match self.section() {
            TAB_PACKAGES => {
                // One list, so the menu is built from what the *row* is rather than from which tab
                // it was on. A row with an offer can be acted on; a row with a database entry can be
                // held and configured; a row with both offers everything.
                if let Some(r) = self.rows().get(self.list.selected) {
                    if r.managed {
                        m = m.item("Details\u{2026}", Item::Details);
                    }
                    if !r.is_self {
                        if let Some((_, src)) = r.offer {
                            // "Get" fetches and "Install" uses bytes already here — the same
                            // distinction the two tabs used to make by being two tabs.
                            m = m.item(
                                match src {
                                    Source::Remote(_) => "Get",
                                    Source::Local(_) => "Install\u{2026}",
                                },
                                Item::Install,
                            );
                        }
                        if r.managed {
                            m = m.item(if r.pinned { "Release" } else { "Hold" }, Item::Pin);
                            m = m.item("Reopen after install", Item::Reopen);
                        }
                    }
                }
            }
            TAB_REPOS => {
                m = m.item("Add repository\u{2026}", Item::AddRepo);
                if !self.repos.repos.is_empty() {
                    m = m.item("Check this one", Item::CheckThis);
                    m = m.item("Check all", Item::CheckAll);
                    m = m.item("Remove", Item::RemoveRepo);
                }
            }
            _ => {
                if let Some(j) = self.queue.jobs.get(self.list.selected) {
                    if j.resumable() {
                        m = m.item("Retry", Item::RetryJob);
                    }
                    if j.state.pending() {
                        m = m.item("Stop", Item::CancelJob);
                    }
                }
                m = m.item("Clear finished", Item::ClearDone);
            }
        }
        self.menu = Some(m);
    }

    fn on_menu(&mut self, item: Item) {
        let i = self.list.selected;
        self.request = match item {
            Item::Details => {
                self.open_sheet();
                None
            }
            Item::Install => self.install_request(i),
            // Same reason as `open_sheet`: the index is into the sorted list, so the identity has
            // to come from the row rather than from the database's own order.
            Item::Pin => self.rows().get(i).map(|r| PkgRequest::TogglePin(r.uid3)),
            Item::Reopen => self.rows().get(i).map(|r| PkgRequest::StepReopen(r.uid3)),
            Item::AddRepo => {
                self.prompt = Some(
                    TextPrompt::new("Add repository", "owner/repo")
                        .note("A GitHub repository, or paste its address"),
                );
                None
            }
            Item::CheckThis => self.repos.repos.get(i).map(|r| PkgRequest::Check(r.id)),
            Item::CheckAll => Some(PkgRequest::CheckAll),
            Item::RemoveRepo => self
                .repos
                .repos
                .get(i)
                .map(|r| PkgRequest::RemoveRepo(r.id)),
            Item::RetryJob => self.queue.jobs.get(i).map(|j| PkgRequest::Retry(j.id)),
            Item::CancelJob => self.queue.jobs.get(i).map(|j| PkgRequest::Cancel(j.id)),
            Item::ClearDone => Some(PkgRequest::ClearDone),
        };
    }

    /// What "install" means for the focused row, which depends on where the row came from.
    ///
    /// A catalogue row has to be fetched first; a row already on disk is handed straight to the
    /// installer. Same word to the user, because from their side it is the same intention.
    ///
    /// This used to branch on the tab and re-derive which copy to use, which meant the rule
    /// "prefer what is on disk" lived here *and* in the row's own construction. It lives in
    /// [`Self::rows`] now: the row already decided, and this reads what it decided — so a row that
    /// says `update` from a local file cannot hand the installer the catalogue's copy.
    fn install_request(&self, i: usize) -> Option<PkgRequest> {
        if self.section() != TAB_PACKAGES {
            return None;
        }
        match self.rows().get(i)?.offer? {
            (_, Source::Local(n)) => Some(PkgRequest::Install(n)),
            (_, Source::Remote(n)) => self.cat.entries.get(n).cloned().map(PkgRequest::Download),
        }
    }

    // ------------------------------------------------------------------ keys

    pub fn handle_key(&mut self, ev: KeyEvent, theme: &Theme<'_>, screen: Rect) -> Handled {
        if let Some(p) = self.prompt.as_mut() {
            // A dialog that is open owns the keyboard, and the text field owns everything the dialog
            // does not need. `NoClipboard` is not right here — the app passes a real one.
            match p.handle_key(ev, &mut symbian_ui::clip::NoClipboard) {
                Some(TextAnswer::Entered(v)) => {
                    self.prompt = None;
                    self.request = Some(PkgRequest::AddRepo(v));
                }
                Some(TextAnswer::Cancelled) => self.prompt = None,
                None => {}
            }
            return Handled::Consumed;
        }
        // The log is over the sheet, which is over the list, so it answers first. Anything closes
        // it — there is nothing on it to operate, and a reader who has finished reading presses
        // whatever is under their thumb.
        if self.notes.is_some() {
            if matches!(ev.key, Key::Softkey(Softkey::Right) | Key::Select) {
                self.notes = None;
            }
            return Handled::Consumed;
        }
        if self.sheet.is_some() {
            // Before the sheet, because the sheet has no idea there is more than one version and
            // would spend Left and Right on nothing. Stepping rebuilds it, which is cheap and is
            // also what keeps every line — the size, the file, the offer — describing the version
            // now on show rather than the one it opened on.
            let n = self.sheet_versions().len();
            if n > 1 {
                match ev.key {
                    Key::Right if self.sheet_pick + 1 < n => {
                        self.sheet_pick += 1;
                        self.reopen_sheet();
                        return Handled::Consumed;
                    }
                    Key::Left if self.sheet_pick > 0 => {
                        self.sheet_pick -= 1;
                        self.reopen_sheet();
                        return Handled::Consumed;
                    }
                    // At either end the key is still swallowed: a sheet is modal, and letting Left
                    // fall through would change the tab behind it.
                    Key::Left | Key::Right => return Handled::Consumed,
                    _ => {}
                }
            }
        }
        if let Some(s) = self.sheet.as_mut() {
            let (h, action) = s.handle_key(ev);
            match action {
                SheetAction::Chose(i) => self.on_sheet_action(i),
                SheetAction::Back => self.sheet = None,
                SheetAction::None => {}
            }
            return h;
        }
        if menu::owns_keys(&self.menu) == Handled::Consumed {
            if let Some(MenuAction::Chosen(item)) = menu::route(&mut self.menu, ev) {
                self.on_menu(item);
            }
            return Handled::Consumed;
        }

        if let Key::Softkey(Softkey::Right) = ev.key {
            self.back = true;
            self.request = Some(PkgRequest::Back);
            return Handled::Consumed;
        }
        // The section is read *before* the tab strip is offered the key, because `handle_key` has
        // already changed it by the time it returns — so saving afterwards writes the cursor into the
        // slot of the tab being moved to. Which is a cursor that never comes back.
        let was = self.section();
        if self.tabs.handle_key(ev, TABS.len()) == Handled::Consumed {
            // Each section keeps its own cursor: one shared would put somebody on row 7 of a list
            // with two rows.
            self.cursor[was] = self.list.selected;
            self.list.selected = self.cursor[self.tabs.active()];
            return Handled::Consumed;
        }

        let content = Self::regions(screen, theme).1;
        let rows = Uniform {
            count: self.lines().len(),
            height: self.row_height(theme),
        };
        if self.list.handle_key(ev, &rows, content.height()) == Handled::Consumed {
            return Handled::Consumed;
        }
        match ev.key {
            Key::Softkey(Softkey::Left) => {
                self.open_menu();
                Handled::Consumed
            }
            Key::Select => {
                match self.section() {
                    // Select opens the detail sheet for a package we know about, and acts for one
                    // we only have on offer. That is not a compromise between the two old tabs: the
                    // sheet is where a managed package's whole state is, and a row with nothing
                    // installed has no state to show — only an intention.
                    TAB_PACKAGES => match self.rows().get(self.list.selected) {
                        Some(r) if r.managed => self.open_sheet(),
                        Some(_) => self.request = self.install_request(self.list.selected),
                        None => {}
                    },
                    TAB_REPOS => {
                        self.request = self
                            .repos
                            .repos
                            .get(self.list.selected)
                            .map(|r| PkgRequest::Check(r.id))
                    }
                    _ => {
                        if let Some(j) = self.queue.jobs.get(self.list.selected) {
                            if j.resumable() {
                                self.request = Some(PkgRequest::Retry(j.id));
                            }
                        }
                    }
                }
                Handled::Consumed
            }
            _ => Handled::Ignored,
        }
    }

    // ------------------------------------------------------------------ drawing

    fn regions(screen: Rect, theme: &Theme<'_>) -> (Rect, Rect, Rect, Rect) {
        let f = chrome::Frame::split(screen, theme, true, true);
        let (tabs, content) = f.content.split_top(theme.metrics.row_h);
        (f.title, content, tabs, f.softkeys)
    }

    /// The rows as they shipped: rects computed here, ink laid down here.
    ///
    /// Kept, unchanged, as the reference `examples/parity.rs` compares the declared rows against. It
    /// is the only thing that can say whether the rewrite moved a pixel, and "it looks the same" is
    /// not that thing — a second line two pixels low survives every glance and no diff.
    fn draw_rows_imperative(
        list: &mut ListState,
        c: &mut Canvas<'_>,
        content: Rect,
        theme: &Theme<'_>,
        lines: &[Line],
        rh: i32,
    ) {
        let rows = Uniform {
            count: lines.len(),
            height: rh,
        };
        let sel = list.selected;
        let p = &theme.palette;
        let pad = theme.metrics.pad;
        list.draw_visible(c, &rows, content, |c, i, row| {
            let line = &lines[i];
            if i == sel {
                chrome::selection(c, row, theme);
            }
            let fg = if i == sel { p.selection_text } else { p.text };
            let cell = row.inset_xy(pad, 1);

            // The chip is measured and its width reserved before the text is placed, which is
            // the whole reason `Chip::width` exists: a long name used to push the state off a
            // 320-pixel screen.
            let mut text_area = cell;
            if let Some((word, tone)) = &line.chip {
                let chip = Chip::new(word, *tone);
                let w = chip.width(theme);
                let (text, chip_area) = split_row(cell, w, pad);
                text_area = text;
                chip.draw_right(
                    c,
                    Rect::from_xywh(
                        chip_area.x0,
                        cell.y0,
                        chip_area.width(),
                        theme.fonts.body.line_height() + 2,
                    ),
                    theme,
                );
            }

            let (first, rest) = text_area.split_top(theme.fonts.body.line_height());
            c.draw_text_in(first, &line.text, theme.fonts.body, fg, Align::Start);

            if let Some(m) = line.meter {
                let (bar, _) = rest.split_top(symbian_ui::meter::height(theme));
                m.draw(c, bar, theme);
                let (_, under) = rest.split_top(symbian_ui::meter::height(theme) + 1);
                c.draw_text_in(under, &line.detail, theme.fonts.small, p.dim, Align::Start);
            } else if !line.detail.is_empty() {
                let dim = if i == sel { p.selection_text } else { p.dim };
                c.draw_text_in(rest, &line.detail, theme.fonts.small, dim, Align::Start);
            }
        });
    }

    /// The rows, declared: every piece is a `symbian-decl-ui` widget, measured and placed by the
    /// layout pass instead of by arithmetic written out here.
    ///
    /// # Why one row is two layouts and not one node
    ///
    /// Because the shipped row's two bands **overlap**, and the overlap is not an accident of this
    /// screen — it is what makes a chip look right beside a line of body text. The title's line is
    /// `body.line_height()` tall. The chip's box beside it is two pixels taller, because
    /// [`Chip::draw_right`] centres a pill of `chip::height` inside whatever box it is handed and the
    /// row hands it `body.line_height() + 2`. The *second* line then starts where the **title's** line
    /// ends — two pixels above where the chip's box ends.
    ///
    /// A `Column` cannot express two children anchored to the same top edge with different heights,
    /// so a single node for the whole row would have to move either the pill or the second line by a
    /// pixel. That is not a guess about how much it matters: moving the title's line down by one
    /// pixel — the negative control this harness was checked with — is about 1700 differing pixels
    /// per scene, which is a screen nobody would call changed and a diff that is unmistakable.
    ///
    /// So the bands this screen has always had are laid out separately, and what the widgets own is
    /// what is *inside* them: the chip's width against the title's, the truncation, the bar's track,
    /// and — the reason this is worth doing at all — the ink, which comes from [`Ground`] once
    /// instead of from three `if i == sel` at the call site.
    fn draw_rows_declared(
        list: &mut ListState,
        c: &mut Canvas<'_>,
        content: Rect,
        theme: &Theme<'_>,
        lines: &[Line],
        rh: i32,
    ) {
        let rows = Uniform {
            count: lines.len(),
            height: rh,
        };
        let sel = list.selected;
        let pad = theme.metrics.pad;
        let body_lh = theme.fonts.body.line_height();
        list.draw_visible(c, &rows, content, |c, i, row| {
            let line = &lines[i];
            if i == sel {
                chrome::selection(c, row, theme);
            }
            // The ground rule, applied by a row that paints outside a `ScrollList`: on the band there
            // is one legible ink and `Ink::Dim` collapses into it. That is the same answer the
            // hand-written row spelled out as two separate `if i == sel`, and it is now said once.
            let t = theme.on(if i == sel { Ground::Band } else { Ground::Page });
            let cell = row.inset_xy(pad, 1);

            // The title and the chip on one line, with the layout doing the division. The chip's
            // width is the pill's own answer to how wide it is — which is the whole reason
            // `Chip::width` exists, and why a long package name no longer pushes its state off a
            // 320-pixel screen.
            let title = Node::Group(
                Column::new()
                    // `Stretch` across, or the text is given only the width it *measures*, and a
                    // glyph whose ink runs a pixel past its advance is clipped by its own box. That
                    // was one pixel at the end of one label out of twenty-one scenes, which is
                    // exactly what a pixel comparison is for and exactly what a look would miss.
                    .align(CrossAlign::Stretch)
                    .node(Node::leaf(
                        Text::new(line.text.as_str()).font(FontRole::Body),
                    ))
                    // As tall as its text and no taller, at the top of a band the chip needs two more
                    // pixels of. This is the overlap the module note above is about.
                    .align_self(CrossAlign::Start)
                    .fill(1),
            );
            let mut head = Row::new()
                .align(CrossAlign::Stretch)
                .gap(Gap::Exact(pad))
                .node(title);
            if let Some((word, tone)) = &line.chip {
                // No `.selected(i == sel)`, and that is the shipped look rather than an oversight:
                // the hand-written row calls `Chip::draw_right`, not `draw_right_on`, so a chip on
                // the selection band keeps the fill it was given against the *page*. Measured on the
                // dark palette, a `Fresh` pill is luma 127 on a band of 94 — a delta of 33, where the
                // palette's own `check` wants 70. It is the same "ink needs a ground" defect as the
                // caption below, and the same reason it is left alone here: fixing it is a change to
                // how the screen looks, which this rewrite is not.
                head = head.child(ChipNode::new(word.as_str(), *tone));
            }
            paint(
                &Node::Group(head),
                Rect {
                    y1: cell.y0 + body_lh + 2,
                    ..cell
                },
                c,
                &t,
            );

            if line.detail.is_empty() && line.meter.is_none() {
                return;
            }

            // Where the second line ends. Asked of `Chip::width` — the same function the layout pass
            // just asked — rather than reconstructed, so the two cannot answer differently and leave
            // the second line running under the pill.
            let chip_w = line
                .chip
                .as_ref()
                .map(|(w, tone)| Chip::new(w, *tone).width(theme))
                .unwrap_or(0);
            let (text_area, _) = split_row(cell, chip_w, pad);
            let (_, rest) = text_area.split_top(body_lh);

            match line.meter {
                Some(m) => {
                    let bar_h = symbian_ui::meter::height(theme);
                    let (bar, _) = rest.split_top(bar_h);
                    // Two widgets and not one with a flag: a job that does not know its total wants
                    // the sweeping meter, and a bar stuck at 0% reads as broken rather than as
                    // unknown — the worse of the two, because the person holding the phone stops
                    // waiting. Same box either way, so the row does not twitch when a `Content-Length`
                    // arrives mid-download.
                    let node = match m {
                        Meter::Fraction(f) => Node::leaf(ProgressBar::new(f)),
                        Meter::Busy { phase } => Node::leaf(Spinner::new(phase)),
                    };
                    paint(&node, bar, c, &t);

                    // `Ground::Page` deliberately, and it is a defect kept rather than a decision:
                    // the shipped row draws this caption in the palette's `dim` even on the selection
                    // band, which is exactly the "ink needs a ground" defect `Ground` exists to
                    // close. Changing it here would move pixels on one scene and the parity harness
                    // would say so — which is the right way round, but it is a look change and not
                    // part of this rewrite. The same applies to the bar itself, drawn unselected
                    // above.
                    let (_, under) = rest.split_top(bar_h + 1);
                    paint(
                        &detail_node(&line.detail),
                        under,
                        c,
                        &theme.on(Ground::Page),
                    );
                }
                None => paint(&detail_node(&line.detail), rest, c, &t),
            }
        });
    }

    pub fn draw(&mut self, c: &mut Canvas<'_>, theme: &Theme<'_>) {
        self.draw_as(c, theme, Rows::Declared);
    }

    /// The screen, with the rows painted by whichever of the two painters is asked for.
    ///
    /// `Rows` exists for `examples/parity.rs` and for nothing else. The imperative painter is the
    /// reference the declared one has to reproduce pixel for pixel, and a reference that cannot be
    /// run is a screenshot somebody took once and then argued with. Everything *outside* the rows —
    /// the title bar, the tab strip, the softkey bar, the three overlays — is shared by both arms, so
    /// the comparison is only ever about a row. A defect in the chrome is invisible to it, and that
    /// is the honest limit of what the harness proves.
    #[doc(hidden)]
    pub fn draw_as(&mut self, c: &mut Canvas<'_>, theme: &Theme<'_>, rows: Rows) {
        let screen = Rect::from_size(c.size());
        let (title, content, tabstrip, softkeys) = Self::regions(screen, theme);

        // A sheet is a screen of its own: it takes the whole frame rather than sitting inside this
        // one, because it is a different question and a panel over a list would keep asking the old
        // one behind it.
        if let Some(text) = self.notes.as_deref() {
            self.draw_notes(c, screen, theme, text);
            return;
        }
        if let Some(s) = self.sheet.as_mut() {
            s.draw(c, screen, theme);
            return;
        }

        chrome::clear(c, theme);
        let pending = self.queue.jobs.iter().filter(|j| j.state.pending()).count();
        let detail = if pending > 0 {
            format!("{pending} in the queue")
        } else {
            String::new()
        };
        // "Store" up here and "Packages" on the tab, and the two are not a slip. The title names the
        // *place* — it is what the second menu icon opens and what a person came here for — while the
        // tab names one of three lists inside it, beside Repos and Downloads. A title that repeated
        // its own first tab would spend the widest line on the screen saying nothing.
        chrome::title_bar(
            c,
            title,
            theme,
            "Store",
            (!detail.is_empty()).then_some(detail.as_str()),
        );
        self.tabs.draw(c, tabstrip, theme, &TABS);

        if self.section() == TAB_PACKAGES && !self.scanned {
            chrome::placeholder(c, content, theme, "Looking for packages\u{2026}");
            return;
        }
        let lines = self.lines();
        if lines.is_empty() {
            chrome::placeholder(c, content, theme, self.empty_text());
        } else {
            let rh = self.row_height(theme);
            let uniform = Uniform {
                count: lines.len(),
                height: rh,
            };
            match rows {
                Rows::Imperative => {
                    Self::draw_rows_imperative(&mut self.list, c, content, theme, &lines, rh)
                }
                Rows::Declared => {
                    Self::draw_rows_declared(&mut self.list, c, content, theme, &lines, rh)
                }
            }
            chrome::scrollbar(
                c,
                content,
                theme,
                self.list.scrollbar(&uniform, content.height()),
            );
        }

        chrome::softkey_bar(
            c,
            softkeys,
            theme,
            chrome::Softkeys::new(Some("Options"), None, Some("Back")),
        );

        // Each overlay draws its own bar over the one above, because the bar above belongs to the
        // screen behind it and its labels stop being true the moment an overlay takes the keys. The
        // menu answers Select and Back; the prompt answers OK and Cancel — and it was showing
        // `Options` / `Back` while the left key committed a repository name and the right one threw
        // it away. That is `keys.rs`'s subject exactly: a label promising one thing while the key
        // does another.
        if let Some(m) = self.menu.as_ref() {
            m.draw(c, theme);
            chrome::softkey_bar(
                c,
                softkeys,
                theme,
                chrome::Softkeys::new(None, Some("Select"), Some("Cancel")),
            );
        }
        if let Some(p) = self.prompt.as_mut() {
            p.draw(c, screen, theme);
            // `TextPrompt::softkeys()` has existed all along and nothing ever called it. Asking the
            // widget rather than repeating its labels here is the point — a second copy would be the
            // thing that drifts.
            chrome::softkey_bar(c, softkeys, theme, p.softkeys());
        }
        self.phase = self.phase.wrapping_add(3);
    }
}

/// A row's second line: small, quiet, and coloured by whatever ground it is painted on.
///
/// Its own function because the meter case and the plain case build the same node and differ only in
/// the ground they resolve it against — and a second copy is where the two would stop agreeing about
/// the font.
fn detail_node(text: &str) -> Node {
    Node::leaf(Text::new(text).font(FontRole::Small).ink(Ink::Dim))
}

/// Measure, place and draw one node inside `rect`.
///
/// The three passes the declarative layer runs over a whole screen, run over one band of one row. The
/// cache is per call and dies with it, which is the cost this screen chooses knowingly: a packages
/// list is a dozen rows on a 240-pixel screen, not two hundred, and a cache that outlived the call
/// would have to be keyed by something that says which row it belongs to.
fn paint(node: &Node, rect: Rect, c: &mut Canvas<'_>, theme: &Theme<'_>) {
    let mut cache = UiCache::with_capacity(node.slot_count() + 4);
    layout::measure_node(
        node,
        0,
        Constraints::tight(rect.width(), rect.height()),
        theme,
        &mut cache,
    );
    layout::layout_node(node, 0, rect, &mut cache, theme);
    layout::draw_node(node, 0, &cache, c, theme);
}

/// Split a row into where the label goes and where the chip goes.
///
/// Its own function because it is the one piece of this screen's layout that can be got wrong
/// silently, and it was: `Rect::split_right` answers **(the strip taken off the right, the rest)** —
/// the cut first, not left-to-right. Read the other way, the label lands in the narrow right-hand
/// strip and the chip paints across the wide remainder, which on the handset reads as the whole
/// screen having shifted right. Nothing in a content test sees that, so the invariant lives here:
/// **the label starts at the left edge and the chip ends at the right one.**
pub fn split_row(cell: Rect, chip_w: i32, pad: i32) -> (Rect, Rect) {
    if chip_w <= 0 {
        return (cell, Rect::from_xywh(cell.x1, cell.y0, 0, cell.height()));
    }
    let (chip, text) = cell.split_right((chip_w + pad).min(cell.width()));
    (text, chip)
}

/// The version a row shows when nothing is known. One place, because three sections needed it.
pub fn describe_version(v: Option<Version>) -> String {
    match v {
        Some(v) => format!("{v}"),
        None => String::from("unknown"),
    }
}

/// Whether this file name is one the screen would offer at all — the same rule the scanner uses.
pub fn offerable(name: &str) -> bool {
    pkg::looks_like_package(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;
    use symbian_bootcfg::pkg::ManagedPkg;
    use symbian_bootcfg::queue::Job;
    use symbian_ui::gfx::Size;
    use symbian_ui::testing::{with_canvas, with_theme, SCREEN};
    use symbian_ui::Palette;

    const LAUNCHER: u32 = 0xE0AA_0000;

    fn pkgs() -> PkgDb {
        let mut d = PkgDb::default();
        let mut l = ManagedPkg::new(LAUNCHER, "Launcher".to_string());
        l.installed = Some(Version::new(0, 1, 0));
        l.stamps = true;
        d.ensure(l);
        // A second package, installed and with nothing on offer. It is here because the join is the
        // thing under test: with one row every merge succeeds trivially, and a list of one cannot
        // show that the two halves ended up on the *same* row rather than simply on the only one.
        let mut q = ManagedPkg::new(0xE0AA_0099, "Quiet".to_string());
        q.installed = Some(Version::new(1, 0, 0));
        d.ensure(q);
        d
    }

    fn cand(uid: u32, v: (u16, u16, u16), file: &str) -> Candidate {
        Candidate {
            dir: "C:\\Data\\_app_install\\".to_string(),
            file: file.to_string(),
            uid3: uid,
            version: Version::new(v.0, v.1, v.2),
            name: "launcher".to_string(),
            size: 320_484,
            sha256: None,
        }
    }

    fn cat() -> CatalogDb {
        CatalogDb {
            entries: vec![CatEntry {
                repo_id: 1,
                asset: "launcher.sisx".to_string(),
                name: "launcher".to_string(),
                version: Version::new(0, 3, 0),
                url: "https://github.com/p/h/releases/download/v0.3.0/launcher.sisx".to_string(),
                size: 330_000,
                notes: String::new(),
            }],
        }
    }

    fn repos() -> RepoDb {
        let mut d = RepoDb::default();
        d.add_github("pizzaria-foundation".to_string(), "home".to_string())
            .unwrap();
        d
    }

    fn screen() -> PkgScreen {
        PkgScreen::new(
            pkgs(),
            vec![cand(LAUNCHER, (0, 2, 0), "launcher-0.2.0.sisx")],
            cat(),
            repos(),
            Queue::default(),
        )
    }

    fn press(s: &mut PkgScreen, k: Key) -> Handled {
        with_theme(Palette::DARK, |t| s.handle_key(KeyEvent::new(k), t, SCREEN))
    }

    fn draw(s: &mut PkgScreen) -> alloc::vec::Vec<u16> {
        with_canvas(Size::new(SCREEN.width(), SCREEN.height()), |c| {
            with_theme(Palette::DARK, |t| s.draw(c, t));
        })
        .1
    }

    fn to_tab(s: &mut PkgScreen, tab: usize) {
        while s.section() < tab {
            press(s, Key::Right);
        }
    }

    #[test]
    fn a_label_starts_at_the_left_edge_and_a_chip_ends_at_the_right_one() {
        // The handset's words: "the program is not using the screen properly, it is over to the
        // right". `Rect::split_right` answers the cut *first*, and reading it as left-to-right put
        // every label in a narrow strip on the right. No content test sees that, so this pins the
        // geometry.
        let cell = Rect::from_xywh(10, 0, 200, 20);
        let (text, chip) = split_row(cell, 40, 5);
        assert_eq!(text.x0, cell.x0, "the label starts where the row starts");
        assert_eq!(chip.x1, cell.x1, "and the chip ends where the row ends");
        assert!(
            text.width() > chip.width(),
            "the label gets the room, not the state"
        );
        assert_eq!(chip.width(), 45);

        // No chip: the label gets the whole row rather than a row minus nothing.
        let (all, none) = split_row(cell, 0, 5);
        assert_eq!(all, cell);
        assert_eq!(none.width(), 0);

        // A chip wider than the row cannot push the label off the left edge.
        let (text, _) = split_row(cell, 999, 5);
        assert_eq!(text.x0, cell.x0);
    }

    #[test]
    fn every_section_draws_and_says_something_when_empty() {
        let mut s = PkgScreen::new(
            PkgDb::default(),
            vec![],
            CatalogDb::default(),
            RepoDb::default(),
            Queue::default(),
        );
        let blank = with_canvas(Size::new(SCREEN.width(), SCREEN.height()), |_| {}).1;
        for tab in 0..TABS.len() {
            to_tab(&mut s, tab);
            assert!(
                !s.empty_text().is_empty(),
                "section {tab} has no empty message"
            );
            assert_ne!(draw(&mut s), blank, "section {tab} drew nothing");
        }
    }

    #[test]
    fn each_section_keeps_its_own_cursor() {
        // One shared cursor would put somebody on row 7 of a list with two rows.
        let mut s = screen();
        press(&mut s, Key::Down);
        assert_eq!(s.list.selected, 1);
        press(&mut s, Key::Right);
        assert_eq!(s.list.selected, 0, "Repos has its own place");
        press(&mut s, Key::Left);
        assert_eq!(s.list.selected, 1, "and Packages kept its own");
    }

    #[test]
    fn a_package_appears_once_carrying_both_halves_of_its_story() {
        // The whole point of the join. The fixture has Launcher installed *and* on offer from two
        // places; it used to be one row on Installed and another on Available, saying nothing about
        // being the same thing.
        let s = screen();
        let rows = s.rows();
        assert_eq!(
            rows.iter().filter(|r| r.name.starts_with("Launcher")).count(),
            1,
            "one row per package, not one per source"
        );
        let r = &rows[0];
        assert!(r.managed, "it is installed");
        assert!(r.offer.is_some(), "and there is something to do about it");
        assert_eq!(r.marker().0, "update");
    }

    #[test]
    fn a_row_shows_where_the_bytes_are_and_prefers_the_disk() {
        // Two offers for one package, and the rule is the one `install_request` always had: bytes
        // here beat bytes to fetch. It lives on the row now, so the label and the action cannot
        // disagree about which copy they mean.
        let s = screen();
        let lines = s.package_lines();
        assert!(lines[0].detail.starts_with("local"), "detail said {:?}", lines[0].detail);
        assert!(matches!(s.rows()[0].offer, Some((_, Source::Local(_)))));
    }

    #[test]
    fn an_install_asks_for_the_copy_the_row_named() {
        // Through the menu, because Select on a *managed* row opens the sheet — a package we know
        // about has state worth showing before acting on it, and that was true before this change
        // and stays true. Select acting directly is for a row that is only an offer.
        let mut s = screen();
        s.on_menu(Item::Install);
        assert_eq!(s.take_request(), Some(PkgRequest::Install(0)));
    }

    #[test]
    fn the_list_answers_what_needs_doing_before_what_does_not() {
        // Urgency then name. A screen you open to ask "what should I do" should not need scrolling
        // to answer, and alphabetical alone buries an update under four packages that are fine.
        let s = screen();
        let rows = s.rows();
        let urgencies: alloc::vec::Vec<u8> = rows.iter().map(|r| r.urgency()).collect();
        let mut sorted = urgencies.clone();
        sorted.sort_unstable();
        assert_eq!(urgencies, sorted, "rows are not in urgency order");
    }

    #[test]
    fn a_row_already_queued_says_so_rather_than_offering_again() {
        let mut q = Queue::default();
        q.push(Job::download(
            1,
            1,
            "https://github.com/p/h/releases/download/v0.3.0/launcher.sisx".to_string(),
            "launcher.sisx".to_string(),
            330_000,
        ));
        // No local candidate, so the only offer is the catalogue's — and it is already being
        // fetched. The chip says what is happening rather than what could be.
        let s = PkgScreen::new(pkgs(), vec![], cat(), repos(), q);
        assert_eq!(s.package_lines()[0].chip.as_ref().unwrap().0, "queued");
    }

    #[test]
    fn the_sheet_is_where_the_facts_live_now() {
        // What the row label used to try to be.
        let mut s = screen();
        press(&mut s, Key::Select);
        assert!(s.sheet.is_some());
        assert_eq!(s.sheet_uid, LAUNCHER);
        draw(&mut s);
        // The sheet opens on the *newest* version, which in this fixture is the catalogue's 0.3.0
        // against 0.2.0 on disk — so the action is Get, because the bytes are not here yet. That is
        // the label doing its job: the same key means fetch or install depending on where the
        // version on show lives.
        assert_eq!(s.sheet.as_ref().unwrap().action_label(), Some("Get"));
    }

    #[test]
    fn the_list_waits_rather_than_rearranging_itself_a_moment_later() {
        // The handset said it: "it loads badly — it comes one way and then becomes another". The
        // scan is deferred so the screen opens fast, and with two tabs that was invisible because
        // only Available changed. Sorted by urgency in one list it is not: the scan landing moves
        // every row.
        let mut s = screen();
        s.mark_scanning();
        let looking = draw(&mut s);
        s.refresh(
            pkgs(),
            vec![cand(LAUNCHER, (0, 2, 0), "launcher-0.2.0.sisx")],
            cat(),
            repos(),
            Queue::default(),
        );
        let answered = draw(&mut s);
        assert_ne!(looking, answered, "the wait draws something, and it is not the list");

        // And a screen simply handed its data never waits — being given rows means somebody looked.
        let plain = screen();
        assert!(plain.scanned);
    }

    #[test]
    fn a_refresh_keeps_the_cursor_on_the_package_and_not_on_the_row_number() {
        // New data reorders the list, which is what sorting by urgency means. A cursor that stayed
        // on row 1 would be pointing at a different package than the one being read.
        let mut s = screen();
        press(&mut s, Key::Down);
        let before = s.rows()[s.list.selected].uid3;
        s.refresh(
            pkgs(),
            vec![cand(LAUNCHER, (0, 2, 0), "launcher-0.2.0.sisx")],
            cat(),
            repos(),
            Queue::default(),
        );
        assert_eq!(s.rows()[s.list.selected].uid3, before, "the cursor followed the package");
    }

    #[test]
    fn whats_new_shows_the_release_notes_over_the_sheet() {
        // A store answers "what is in this version", and the notes travel with the catalogue entry
        // — they arrive in the same payload the version does, so nothing is fetched to read them.
        let mut s = screen();
        press(&mut s, Key::Select);
        assert_eq!(
            s.sheet.as_ref().unwrap().action_label(),
            Some("Get"),
            "the sheet opens on the catalogue's version, which is the one with notes"
        );
        // Install/Get, Hold, Reopen, What's new.
        s.on_sheet_action(3);
        assert!(s.notes.is_some(), "the notes opened");
        assert!(s.take_request().is_none(), "and asked the application for nothing");
        assert!(s.sheet.is_some(), "the sheet stays behind, so Back returns to the package");

        draw(&mut s);
        press(&mut s, Key::Select);
        assert!(s.notes.is_none(), "anything closes them");
        assert!(s.sheet.is_some(), "and leaves the sheet where it was");
    }

    #[test]
    fn a_version_with_nothing_to_say_does_not_offer_to_say_it() {
        // An action that opens an empty page is worse than an action that is not there. A local
        // file has no notes at all — it came off a card, not from a release.
        let mut s = PkgScreen::new(
            pkgs(),
            vec![cand(LAUNCHER, (0, 9, 0), "launcher-0.9.0.sisx")],
            CatalogDb::default(),
            repos(),
            Queue::default(),
        );
        press(&mut s, Key::Select);
        let sheet = s.sheet.as_ref().unwrap();
        assert_eq!(sheet.action_label(), Some("Install"), "bytes are already here");
        // Three actions, not four: Install, Hold, Reopen.
        s.on_sheet_action(3);
        assert!(s.notes.is_none(), "there was no fourth action to reach");
    }

    #[test]
    fn the_arrows_choose_which_version_gets_installed() {
        // The arrows are not browsing. What the sheet says it will install is what `Install` sends,
        // and that is the whole reason they exist: an older file on disk was scanned, ranked, and
        // then dropped, so rolling back meant deleting the newer one off the card.
        let mut s = screen();
        press(&mut s, Key::Select);
        assert_eq!(s.sheet_versions().len(), 2, "0.3.0 in the catalogue, 0.2.0 on disk");
        assert_eq!(s.sheet_pick, 0, "opens on the newest");
        assert_eq!(s.sheet.as_ref().unwrap().action_label(), Some("Get"));

        press(&mut s, Key::Right);
        assert_eq!(s.sheet_pick, 1, "stepped to the older one");
        assert_eq!(
            s.sheet.as_ref().unwrap().action_label(),
            Some("Install"),
            "and that one is already here"
        );

        // Past the end the key is swallowed rather than falling through — a sheet is modal, and a
        // Right that reached the tabs would change the section behind it.
        press(&mut s, Key::Right);
        assert_eq!(s.sheet_pick, 1);
        assert!(s.sheet.is_some(), "and the sheet is still open");

        press(&mut s, Key::Left);
        assert_eq!(s.sheet_pick, 0, "and back");
    }

    #[test]
    fn the_sheet_actions_are_attributed_even_though_install_is_conditional() {
        // The actions are added conditionally, so the index means different things. Getting this
        // wrong would pin a package when somebody asked to install it.
        let mut s = screen();
        press(&mut s, Key::Select); // sheet, with Install first
        press(&mut s, Key::Down);
        press(&mut s, Key::Select);
        assert_eq!(s.take_request(), Some(PkgRequest::TogglePin(LAUNCHER)));

        // No candidate: no Install action, so the first row is the pin.
        let mut bare = PkgScreen::new(
            pkgs(),
            vec![],
            CatalogDb::default(),
            repos(),
            Queue::default(),
        );
        press(&mut bare, Key::Select);
        assert_eq!(
            bare.sheet.as_ref().unwrap().action_label(),
            Some("Hold at this version")
        );
        press(&mut bare, Key::Select);
        assert_eq!(bare.take_request(), Some(PkgRequest::TogglePin(LAUNCHER)));
    }

    #[test]
    fn the_boot_managers_own_package_says_why_it_cannot_be_installed() {
        let mut d = pkgs();
        d.ensure(ManagedPkg::new(
            symbian_bootcfg::BOOTCTL_UID,
            "Boot manager".to_string(),
        ));
        let mut s = PkgScreen::new(d, vec![], CatalogDb::default(), repos(), Queue::default());
        press(&mut s, Key::Down);
        press(&mut s, Key::Select);
        assert!(s.sheet.is_some());
        assert_eq!(
            s.sheet.as_ref().unwrap().action_label(),
            Some("Hold at this version"),
            "and Install is not among them"
        );
    }

    #[test]
    fn adding_a_repository_asks_for_text_and_hands_back_what_was_typed() {
        let mut s = screen();
        to_tab(&mut s, TAB_REPOS);
        press(&mut s, Key::Softkey(Softkey::Left));
        press(&mut s, Key::Select); // Add repository…
        assert!(s.prompt.is_some());
        for ch in "BurntSushi/ripgrep".chars() {
            press(&mut s, Key::Char(ch));
        }
        press(&mut s, Key::Select);
        assert_eq!(
            s.take_request(),
            Some(PkgRequest::AddRepo("BurntSushi/ripgrep".to_string()))
        );
        assert!(s.prompt.is_none());
    }

    #[test]
    fn a_repository_row_carries_its_last_answer() {
        let mut d = repos();
        d.get_mut(1).unwrap().last =
            LastResult::Failed(symbian_bootcfg::repo::FailReason::RateLimited);
        let mut s = PkgScreen::new(pkgs(), vec![], CatalogDb::default(), d, Queue::default());
        to_tab(&mut s, TAB_REPOS);
        let lines = s.repo_lines();
        assert_eq!(lines[0].chip.as_ref().unwrap().0, "failed");
        assert!(lines[0].detail.contains("hourly limit"));
        draw(&mut s);
    }

    #[test]
    fn selecting_a_repository_checks_it() {
        let mut s = screen();
        to_tab(&mut s, TAB_REPOS);
        press(&mut s, Key::Select);
        assert_eq!(s.take_request(), Some(PkgRequest::Check(1)));
    }

    #[test]
    fn a_running_download_gets_a_bar_and_a_failed_one_gets_a_retry() {
        let mut q = Queue::default();
        q.push(Job::download(
            1,
            1,
            "https://x/a.sisx".to_string(),
            "a.sisx".to_string(),
            320_000,
        ));
        q.start(1);
        q.advance(1, 184_000);
        let mut s = PkgScreen::new(pkgs(), vec![], CatalogDb::default(), repos(), q);
        to_tab(&mut s, TAB_DOWNLOADS);

        let lines = s.download_lines();
        assert!(lines[0].meter.is_some(), "a running job has a bar");
        assert!(lines[0].detail.contains("KB"));
        assert_eq!(lines[0].chip.as_ref().unwrap().0, "now");
        assert!(s.animating(), "and the caller is told to keep redrawing");
        draw(&mut s);

        s.queue.fail(1, -33);
        assert_eq!(s.download_lines()[0].chip.as_ref().unwrap().0, "retry?");
        assert!(!s.animating(), "nothing is moving any more");
        press(&mut s, Key::Select);
        assert_eq!(s.take_request(), Some(PkgRequest::Retry(1)));
    }

    #[test]
    fn a_download_with_no_known_size_still_shows_movement() {
        // `Content-Length` is optional, and a bar stuck at 0% reads as broken.
        let mut q = Queue::default();
        q.push(Job::download(
            1,
            1,
            "https://x/a.sisx".to_string(),
            "a.sisx".to_string(),
            0,
        ));
        q.start(1);
        q.advance(1, 5_000);
        let mut s = PkgScreen::new(pkgs(), vec![], CatalogDb::default(), repos(), q);
        to_tab(&mut s, TAB_DOWNLOADS);
        assert_eq!(s.download_lines()[0].meter, Some(Meter::Busy { phase: 0 }));
        draw(&mut s);
    }

    #[test]
    fn the_queue_count_is_in_the_title_so_it_is_visible_from_any_section() {
        let mut q = Queue::default();
        q.push(Job::download(
            1,
            1,
            "https://x/a.sisx".to_string(),
            "a.sisx".to_string(),
            10,
        ));
        let mut s = PkgScreen::new(pkgs(), vec![], CatalogDb::default(), repos(), q);
        draw(&mut s); // Installed, and the title still says one is queued
        assert_eq!(s.queue.jobs.iter().filter(|j| j.state.pending()).count(), 1);
    }

    #[test]
    fn back_leaves_and_says_so_once() {
        let mut s = screen();
        press(&mut s, Key::Softkey(Softkey::Right));
        assert!(s.back());
        assert_eq!(s.take_request(), Some(PkgRequest::Back));
        assert_eq!(s.take_request(), None, "a request happens once");
    }

    #[test]
    fn the_menu_offers_only_what_applies_to_the_row_it_was_opened_on() {
        let mut s = screen();
        to_tab(&mut s, TAB_DOWNLOADS);
        press(&mut s, Key::Softkey(Softkey::Left));
        // Nothing in the queue, so the only thing to do is clear.
        press(&mut s, Key::Select);
        assert_eq!(s.take_request(), Some(PkgRequest::ClearDone));
    }

    #[test]
    fn the_prompt_puts_its_own_labels_on_the_bar() {
        // The defect: the add-repository prompt drew nothing over the softkey bar, so it went on
        // saying `Options` / `Back` while its left key **committed** the repository name and its
        // right key threw it away. `TextPrompt::softkeys()` had existed all along and nothing called
        // it.
        //
        // The prompt and not the menu, because the menu covers the bar's row anyway — a first
        // version of this test compared the bar's pixels with and without an overlay and passed with
        // the fix removed, since the menu's own ink was the difference it was measuring. A test that
        // cannot fail is a constant. This one compares against a bar rendered with the labels that
        // are supposed to be there.
        //
        // Real atlases, not `symbian_ui::testing`'s: that one has a single glyph, so `OK` and
        // `Options` draw the same picture and every label comparison would pass.
        let atlases = symbian_preview::Atlases::load();
        atlases.with_themes(|theme, _light| {
            // Exactly the band `Frame::split` gives the bar, on both sides. Guessing it from
            // `row_h` sampled a strip above it and compared two runs of untouched background, which
            // is a comparison that means nothing.
            let bar = chrome::Frame::split(SCREEN, theme, true, true).softkeys;
            let bar_row = |buf: &[u16]| {
                let w = SCREEN.width();
                let mut out = alloc::vec::Vec::new();
                for y in bar.y0..bar.y1 {
                    out.extend_from_slice(
                        &buf[(y * w + bar.x0) as usize..(y * w + bar.x1) as usize],
                    );
                }
                out
            };
            let mut s = screen();
            // To Repos by name rather than by counting Rights: it was two tabs away and is one now,
            // and a test that walks a distance is a test that breaks when a tab is added or removed
            // for reasons that have nothing to do with it.
            to_tab(&mut s, TAB_REPOS);
            for k in [Key::Softkey(Softkey::Left), Key::Select] {
                s.handle_key(KeyEvent::new(k), theme, SCREEN);
            }
            assert!(
                s.prompt.is_some(),
                "Options → Add repository… opens the prompt"
            );
            let expect = s.prompt.as_ref().unwrap().softkeys();
            assert_eq!(
                expect[0],
                Some("OK"),
                "the widget's own answer, not a copy of it"
            );

            let (_, drawn) = with_canvas(Size::new(SCREEN.width(), SCREEN.height()), |c| {
                s.draw(c, theme)
            });
            let (_, reference) = with_canvas(Size::new(SCREEN.width(), SCREEN.height()), |c| {
                chrome::softkey_bar(c, bar, theme, expect);
            });
            assert_eq!(
                bar_row(&drawn),
                bar_row(&reference),
                "the bar under an open prompt has to be the prompt's own"
            );
        });
    }
}
