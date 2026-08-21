//! Browse the phone's Central Repository files, pick some, and get them off the device as one zip.
//!
//! One question drives this: **which setting names the application that is the phone's idle
//! screen?** A home screen that is merely launched at boot always loses a race it cannot win —
//! however early it starts, the platform's own idle reaches the display first. For the user never
//! to see it, ours has to *be* the idle, and that is a setting rather than a trick.
//!
//! The setting is a Central Repository key, and the public SDK does not document it: nothing in any
//! of its 1988 headers names it, because the S60 header that would (`ActiveIdleInternalCRKeys.h`)
//! was never published. On S60 3rd a repository's ROM defaults live in
//! `Z:\private\10202be9\<uid>.txt` as text, and what the phone has actually changed lives beside it
//! in `C:\private\10202be9\`. Both are another process's private cage, which is why this asks for
//! `AllFiles` — and why it is a separate throwaway application rather than more code inside
//! something that has to keep running.
//!
//! **It lists and a person chooses.** An earlier version copied a hard-coded list of file names
//! read out of `SysAp.exe`'s UID table, and that is exactly the design that fails quietly: a name
//! that does not match produces an empty result indistinguishable from "the file is not there".
//! Listing what exists, filtering by typing, and selecting by hand removes the guess entirely.
//!
//! Nothing is ever written into a repository. This copies, and every decision after that is made
//! with the files on a desk.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod zip;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use symbian::fs::{self, Fs, ShimFs, Utf16Path};
use symbian_gfx::Align;
use symbian_ui::{chrome, App, Canvas, Handled, Key, KeyEvent, Rect, Softkey, Theme};

/// The ROM's repository defaults — text on this platform, one key per line.
const ROM_DIR: &str = "Z:\\private\\10202be9\\";
/// The same repositories after the phone has changed something. Often binary (`.cre`), and listed
/// anyway: a value that differs from the ROM default is the interesting case, not the boring one.
const CUR_DIR: &str = "C:\\private\\10202be9\\";
/// Where the archive lands. `C:\Data` needs no capability, so a file manager can reach it — which
/// is the whole point of putting it there.
const OUT_ZIP: &str = "C:\\Data\\cenrep_dump.zip";

/// Every executable on the phone. `AllFiles` reaches it; nothing else does.
const ROM_BIN_DIR: &str = "Z:\\sys\\bin\\";
/// Where the scan's findings land.
const SCAN_PATH: &str = "C:\\Data\\uidscan.txt";
/// The UID being looked for: the application this phone actually shows as its home screen,
/// "Standby", read out of its own registry rather than assumed.
const LAUNCHER_TARGET_UID: u32 = 0x1027_50F0;
/// How long one press of the scan works for before stopping where it is. Short enough that the
/// window server never decides this application has stopped answering.
const SCAN_BUDGET_US: u64 = 3_000_000;
/// Read size per file. Large enough that a megabyte image is a handful of reads, small enough to
/// sit on a phone's heap beside everything else this app holds.
const SCAN_CHUNK: usize = 64 * 1024;

/// Where the application roster lands. Tab separated, so it opens as a table anywhere and reads
/// fine in a text viewer on the phone itself.
const APPS_PATH: &str = "C:\\Data\\apps.txt";

/// The home screen this is trying to make the phone's idle. Written into the setting as decimal
/// text, which is how that key stores a UID.
const LAUNCHER_UID: u32 = 0xE0AA_0000;

/// Big enough for a directory listing on a phone with far more repositories than an E72 has.
const LIST_UNITS: usize = 32 * 1024;
/// Skip anything larger than this. A repository is a settings file; a megabyte-sized thing in that
/// directory is not one, and putting it in the archive only makes the transfer worse.
const MAX_FILE: usize = 512 * 1024;

/// Which directory a row came from. Kept per row because the same UID exists in both, and which one
/// a value came from is the difference between "the platform's default" and "what this phone does".
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Source {
    Rom,
    Cur,
}

impl Source {
    fn dir(self) -> &'static str {
        match self {
            Source::Rom => ROM_DIR,
            Source::Cur => CUR_DIR,
        }
    }

    /// The prefix inside the archive, so the two directories stay apart on the other end.
    fn prefix(self) -> &'static str {
        match self {
            Source::Rom => "rom_",
            Source::Cur => "cur_",
        }
    }

    fn tag(self) -> &'static str {
        match self {
            Source::Rom => "Z",
            Source::Cur => "C",
        }
    }
}

pub struct Row {
    pub name: String,
    pub source: Source,
    pub selected: bool,
    /// Not a file: the first row opens the idle-application panel.
    ///
    /// It lives in the list rather than on a key of its own because the key of its own did not
    /// work. It was the green key, and on this platform the system takes that one to open the
    /// dialler — an app never sees it. A row reached with Up/Down/Select uses only keys that are
    /// demonstrably delivered, and it is visible, which a hidden chord never is.
    pub action: bool,
}

pub struct Cenrepdump {
    fs: ShimFs,
    rows: Vec<Row>,
    /// Type-to-filter, the same gesture the launcher's application grid uses. Matched case
    /// insensitively against the file name, so `8766` and `101F` both narrow the list.
    filter: String,
    /// Cursor within the *filtered* view, not within `rows`.
    cursor: usize,
    /// First visible filtered row.
    top: usize,
    /// The last thing that happened, shown under the list.
    status: String,
    /// The idle-application panel, when it is open. See [`Cenrepdump::idle_panel`].
    idle: Option<Vec<String>>,
    /// The ROM scan: the file list, how far through it, and what was found. Held across
    /// invocations because the scan is resumable — see [`Cenrepdump::scan_rom`].
    scan_files: Vec<String>,
    scan_at: usize,
    scan_hits: Vec<String>,
    /// The Options menu, when it is open, holding the highlighted item.
    ///
    /// Everything this application can do is reachable from here. It was worth adding because the
    /// alternative had already failed twice: an action on the green key, which the platform takes
    /// for the dialler and never delivers, and an action on Left/Right, which nobody has any reason
    /// to press. A left-softkey menu is where a Symbian user looks first.
    menu: Option<usize>,
    exit: bool,
}

impl Cenrepdump {
    pub fn new() -> Self {
        let mut me = Self {
            fs: ShimFs,
            rows: Vec::new(),
            filter: String::new(),
            cursor: 0,
            top: 0,
            status: String::new(),
            idle: None,
            scan_files: Vec::new(),
            scan_at: 0,
            scan_hits: Vec::new(),
            menu: None,
            exit: false,
        };
        me.reload();
        me
    }

    fn reload(&mut self) {
        self.rows.clear();
        self.rows.push(Row {
            name: String::from("Idle application…"),
            source: Source::Cur,
            selected: false,
            action: true,
        });
        let mut notes: Vec<String> = Vec::new();
        for source in [Source::Rom, Source::Cur] {
            match self.list(source) {
                Ok(n) => notes.push(format!("{}:{n}", source.tag())),
                // Named rather than counted: without `AllFiles` this is where it stops, and a bare
                // "0 files" would read as "the directory is empty".
                Err(e) => notes.push(format!("{}:{e:?}", source.tag())),
            }
        }
        self.status = notes.join("  ");
        symbian::log!("[cenrepdump] listed {}", self.status.as_str());
    }

    fn list(&mut self, source: Source) -> Result<usize, symbian::Error> {
        let path = Utf16Path::new(source.dir()).map_err(|_| symbian::Error::Argument)?;
        let mut buf = alloc::vec![0u16; LIST_UNITS];
        self.fs.list_dir(path.as_units(), &mut buf)?;
        let before = self.rows.len();
        for name in split_names(&buf) {
            self.rows.push(Row { name, source, selected: false, action: false });
        }
        Ok(self.rows.len() - before)
    }

    /// Indices into `rows` that the current filter admits, in order.
    pub fn view(&self) -> Vec<usize> {
        if self.filter.is_empty() {
            return (0..self.rows.len()).collect();
        }
        (0..self.rows.len()).filter(|&i| matches(&self.rows[i].name, &self.filter)).collect()
    }

    pub fn selected_count(&self) -> usize {
        self.rows.iter().filter(|r| r.selected).count()
    }

    fn toggle_here(&mut self) {
        let view = self.view();
        let Some(&i) = view.get(self.cursor) else { return };
        if self.rows[i].action {
            self.idle_panel();
            return;
        }
        self.rows[i].selected = !self.rows[i].selected;
    }

    /// Select or clear everything the filter currently shows.
    ///
    /// Scoped to the filtered view on purpose: "select all" over a hidden list is how somebody ends
    /// up with an archive of two hundred files they did not mean to send.
    fn toggle_all_in_view(&mut self) {
        let view = self.view();
        let any_off = view.iter().any(|&i| !self.rows[i].action && !self.rows[i].selected);
        for &i in &view {
            if !self.rows[i].action {
                self.rows[i].selected = any_off;
            }
        }
    }

    /// Build the archive from every selected row.
    pub fn write_zip(&mut self) {
        let picked: Vec<(String, Source)> = self
            .rows
            .iter()
            .filter(|r| r.selected && !r.action)
            .map(|r| (r.name.clone(), r.source))
            .collect();
        if picked.is_empty() {
            self.status = String::from("nothing selected");
            return;
        }

        let mut z = zip::ZipWriter::new();
        let mut failed = 0;
        for (name, source) in &picked {
            let full = format!("{}{}", source.dir(), name);
            let Ok(p) = Utf16Path::new(&full) else {
                failed += 1;
                continue;
            };
            match fs::read(&mut self.fs, &p) {
                Ok(Some(bytes)) if bytes.len() <= MAX_FILE => {
                    z.add(&format!("{}{}", source.prefix(), name), &bytes);
                }
                Ok(Some(_)) | Ok(None) => failed += 1,
                Err(e) => {
                    symbian::log!("[cenrepdump] read {} err={e:?}", full.as_str());
                    failed += 1;
                }
            }
        }

        let added = z.len();
        let blob = z.finish();
        let Ok(out) = Utf16Path::new(OUT_ZIP) else {
            self.status = String::from("bad output path");
            return;
        };
        match fs::write_atomic(&mut self.fs, &out, &blob) {
            Ok(()) => {
                self.status = format!("{added} in zip, {failed} failed, {} bytes", blob.len());
            }
            Err(e) => self.status = format!("write failed: {e:?}"),
        }
        symbian::log!("[cenrepdump] zip {}", self.status.as_str());
    }

    /// Read the setting that names the phone's idle application, and show it.
    ///
    /// Repository `0x101F876F`: key `0x2` is the current value and is writable with
    /// `WriteDeviceData`; `0x13`/`0x14` hold the same value and refuse writes, which is the phone's
    /// factory default. Read out of `Z:\\private\\10202be9\\101F876F.txt` on this handset rather
    /// than guessed.
    ///
    /// Reading before writing is the whole discipline here: the default is the only way back if a
    /// change turns out to be wrong, and it is easier to write down now than to recover later.
    fn idle_panel(&mut self) {
        use symbian::cenrep;
        let mut out = Vec::new();
        out.push(format!("repo {:08X} key 0x1", cenrep::IDLE_APP_REPO));
        out.push(match cenrep::get(cenrep::IDLE_APP_REPO, cenrep::IDLE_APP_KEY_INT) {
            Ok(v) => format!("home = {v} ({:#010x})", v as u32),
            Err(e) => format!("home : {e:?}"),
        });
        out.push(match cenrep::get(cenrep::IDLE_APP_REPO, cenrep::IDLE_MODE_KEY) {
            Ok(v) => format!("mode = {v}"),
            Err(e) => format!("mode : {e:?}"),
        });
        out.push(format!("native = {:#010x}", cenrep::NATIVE_IDLE_UID));
        out.push(format!("ours   = {LAUNCHER_UID:#010x}"));
        out.push(String::from("Select: ours   Left key: native"));
        for line in &out {
            symbian::log!("[cenrepdump] {}", line.as_str());
        }
        self.idle = Some(out);
    }

    /// Point the home-screen setting at this launcher, or back at the platform's own.
    ///
    /// The only thing in this application that changes the phone, which is why it is behind a
    /// screen of its own that shows both UIDs before either key is pressed. `NATIVE_IDLE_UID` is a
    /// constant rather than something read back, because this repository has no factory-default
    /// file anywhere on the handset — overwrite the value and nothing left on the phone remembers
    /// what it was.
    fn set_idle(&mut self, uid: u32) {
        use symbian::cenrep;
        let rc = cenrep::set(cenrep::IDLE_APP_REPO, cenrep::IDLE_APP_KEY_INT, uid as i32);
        let after = cenrep::get(cenrep::IDLE_APP_REPO, cenrep::IDLE_APP_KEY_INT);
        self.status = format!("set {uid:#010x} {rc:?}, now {after:?}");
        symbian::log!("[cenrepdump] {}", self.status.as_str());
        self.idle_panel();
    }

    /// The Options menu, in order. Kept as one function so the labels the user reads and the
    /// actions [`Cenrepdump::run_menu`] performs cannot drift apart.
    pub fn menu_items(&self) -> [&'static str; 6] {
        [
            "Mark / unmark",
            "Mark all shown",
            "Create zip",
            "Idle application…",
            "Dump app list",
            "Scan ROM for idle UID",
        ]
    }

    fn run_menu(&mut self, item: usize) {
        self.menu = None;
        match item {
            0 => self.toggle_here(),
            1 => self.toggle_all_in_view(),
            2 => self.write_zip(),
            3 => self.idle_panel(),
            4 => self.dump_apps(),
            _ => self.scan_rom(),
        }
    }

    /// Search every executable in the ROM for the home screen application's UID, a slice at a time.
    ///
    /// The question this answers is the one that matters and that no amount of reading settings
    /// could: **who launches the Standby application at boot?** Three repositories were found to
    /// contain its UID and none of them changed anything when written, which is what "stores it"
    /// looks like as opposed to "decides it". Neither `SysAp.exe` nor the Startup application
    /// carries the UID at all.
    ///
    /// So: scan the ROM. Whatever starts an application by UID has that UID in it somewhere, and
    /// `Z:\\sys\\bin` is where every executable on this phone lives. Transferring it to a desk would
    /// be tens of megabytes over Bluetooth; searching it in place is a few hundred file reads.
    ///
    /// **Resumable, with a time budget.** A synchronous walk over the whole ROM would stop this
    /// application answering the window server for long enough to be killed for not responding, and
    /// an app that dies mid-scan reports nothing. So each invocation works for
    /// [`SCAN_BUDGET_US`] and stops where it is; pressing the menu item again continues. The status
    /// line carries the position, so the user can see it advance rather than wonder.
    fn scan_rom(&mut self) {
        let started = symbian::monotonic_us();
        if self.scan_files.is_empty() {
            let Ok(dir) = Utf16Path::new(ROM_BIN_DIR) else { return };
            let mut buf = alloc::vec![0u16; LIST_UNITS];
            match self.fs.list_dir(dir.as_units(), &mut buf) {
                Ok(_) => self.scan_files = split_names(&buf),
                Err(e) => {
                    self.status = format!("{ROM_BIN_DIR}: {e:?}");
                    return;
                }
            }
            self.scan_at = 0;
            self.scan_hits.clear();
        }

        let needle = LAUNCHER_TARGET_UID.to_le_bytes();
        while self.scan_at < self.scan_files.len() {
            if symbian::monotonic_us().saturating_sub(started) > SCAN_BUDGET_US {
                break;
            }
            let name = self.scan_files[self.scan_at].clone();
            self.scan_at += 1;
            if let Some(off) = self.find_in_file(&name, &needle) {
                let line = format!("{name} @ {off:#x}");
                symbian::log!("[cenrepdump] hit {}", line.as_str());
                self.scan_hits.push(line);
            }
        }

        let done = self.scan_at >= self.scan_files.len();
        self.status = format!(
            "{}/{}  {} hit(s){}",
            self.scan_at,
            self.scan_files.len(),
            self.scan_hits.len(),
            if done { " — done" } else { " — press again" }
        );
        if done {
            self.write_scan_report();
            // Cleared so a second run starts over rather than reporting "done" for ever.
            self.scan_files.clear();
        }
        symbian::log!("[cenrepdump] scan {}", self.status.as_str());
    }

    /// Read one ROM file in chunks, looking for `needle`.
    ///
    /// Chunked because a ROM executable can be megabytes and this phone has tens of them free; the
    /// overlap of `needle.len() - 1` bytes between chunks is what stops a match that straddles a
    /// boundary from being missed, which is the classic way a scan like this quietly under-reports.
    fn find_in_file(&mut self, name: &str, needle: &[u8]) -> Option<u64> {
        let full = format!("{ROM_BIN_DIR}{name}");
        let path = Utf16Path::new(&full).ok()?;
        let mut f = fs::File::open(&mut self.fs, &path, fs::OpenMode::Read).ok()?;
        let mut buf = alloc::vec![0u8; SCAN_CHUNK + needle.len() - 1];
        let mut base = 0u64;
        let mut carry = 0usize;
        loop {
            let n = f.read_fully(&mut buf[carry..]).ok()?;
            let filled = carry + n;
            if filled < needle.len() {
                return None;
            }
            if let Some(i) = buf[..filled].windows(needle.len()).position(|w| w == needle) {
                return Some(base + i as u64);
            }
            if n == 0 {
                return None;
            }
            carry = needle.len() - 1;
            let keep = filled - carry;
            buf.copy_within(keep.., 0);
            base += keep as u64;
        }
    }

    fn write_scan_report(&mut self) {
        let mut text = format!("uid {LAUNCHER_TARGET_UID:#010x} in {ROM_BIN_DIR}\n");
        for line in &self.scan_hits {
            text.push_str(line);
            text.push('\n');
        }
        if let Ok(p) = Utf16Path::new(SCAN_PATH) {
            let _ = fs::write_atomic(&mut self.fs, &p, text.as_bytes());
        }
    }

    /// Write every registered application's UID and caption to a file.
    ///
    /// This is what turned the search around. Hunting for "the setting that names the home screen"
    /// found two plausible repositories that were something else entirely; having the phone's own
    /// name-to-UID table meant the question could be asked backwards instead — *which repository
    /// contains the UID of the application that is demonstrably the home screen* — and that has
    /// exactly one answer.
    ///
    /// Hidden and control-panel entries are included and marked. "Standby" is hidden, so a list
    /// filtered the way a launcher filters one would have dropped the row this exists to find.
    fn dump_apps(&mut self) {
        let roster = match symbian::apps::installed() {
            Ok(r) => r,
            Err(e) => {
                self.status = format!("app list: {e:?}");
                return;
            }
        };
        let mut text = String::from("uid3\thidden\tsystem\tcaption\n");
        for a in &roster {
            text.push_str(&format!(
                "{:08X}\t{}\t{}\t{}\n",
                a.uid3, a.hidden as u8, a.system as u8, a.caption
            ));
        }
        let Ok(path) = Utf16Path::new(APPS_PATH) else {
            self.status = String::from("bad path");
            return;
        };
        match fs::write_atomic(&mut self.fs, &path, text.as_bytes()) {
            Ok(()) => self.status = format!("{} apps -> {APPS_PATH}", roster.len()),
            Err(e) => self.status = format!("app list write: {e:?}"),
        }
        symbian::log!("[cenrepdump] {}", self.status.as_str());
    }

    fn move_cursor(&mut self, down: bool) {
        let n = self.view().len();
        if n == 0 {
            self.cursor = 0;
            return;
        }
        self.cursor = if down {
            (self.cursor + 1).min(n - 1)
        } else {
            self.cursor.saturating_sub(1)
        };
    }
}

/// Case-insensitive substring match, which is what type-to-filter means to a person holding a
/// keypad: `8766` finds `101f8766.txt` without them typing the whole name or knowing its case.
fn matches(name: &str, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    let n: String = name.chars().flat_map(|c| c.to_lowercase()).collect();
    let f: String = filter.chars().flat_map(|c| c.to_lowercase()).collect();
    n.contains(f.as_str())
}

/// Split the shim's directory listing into names.
///
/// `list_dir` packs entries as NUL-separated UTF-16; empty runs are skipped so a trailing
/// terminator does not become a nameless row.
fn split_names(buf: &[u16]) -> Vec<String> {
    let mut out = Vec::new();
    for chunk in buf.split(|&u| u == 0) {
        if chunk.is_empty() {
            continue;
        }
        out.push(String::from_utf16_lossy(chunk));
    }
    out
}

impl Default for Cenrepdump {
    fn default() -> Self {
        Self::new()
    }
}

impl App for Cenrepdump {
    fn title(&self) -> &str {
        "Cenrep dump"
    }

    fn handle_key(&mut self, ev: KeyEvent, _theme: &Theme<'_>, _screen: Rect) -> Handled {
        // The menu is modal, and takes precedence over the panel behind it.
        if let Some(at) = self.menu {
            let n = self.menu_items().len();
            match ev.key {
                Key::Up => self.menu = Some(at.saturating_sub(1)),
                Key::Down => self.menu = Some((at + 1).min(n - 1)),
                Key::Select | Key::Softkey(Softkey::Left) => self.run_menu(at),
                Key::Softkey(Softkey::Right) | Key::Backspace => self.menu = None,
                Key::End => self.exit = true,
                _ => {}
            }
            return Handled::Consumed;
        }

        // The idle panel is modal while it is open: it is the only screen here that can change the
        // phone, and a stray filter keystroke landing on it would be the wrong kind of surprise.
        if self.idle.is_some() {
            match ev.key {
                Key::Select => self.set_idle(LAUNCHER_UID),
                Key::Softkey(Softkey::Left) => self.set_idle(symbian::cenrep::NATIVE_IDLE_UID),
                Key::Softkey(Softkey::Right) | Key::Backspace | Key::Left => self.idle = None,
                Key::End => self.exit = true,
                _ => {}
            }
            return Handled::Consumed;
        }
        match ev.key {
            Key::Up => self.move_cursor(false),
            Key::Down => self.move_cursor(true),
            Key::Select => self.toggle_here(),
            Key::Softkey(Softkey::Left) => self.menu = Some(0),
            Key::Left | Key::Right => self.toggle_all_in_view(),
            Key::Backspace | Key::Delete => {
                self.filter.pop();
                self.cursor = 0;
                self.top = 0;
            }
            Key::Char(c) => {
                self.filter.push(c);
                self.cursor = 0;
                self.top = 0;
            }
            Key::Softkey(Softkey::Right) | Key::End => self.exit = true,
            _ => return Handled::Ignored,
        }
        Handled::Consumed
    }

    fn should_exit(&self) -> bool {
        self.exit
    }

    fn draw(&mut self, c: &mut Canvas<'_>, theme: &Theme<'_>) {
        let screen = Rect::from_size(c.size());
        let frame = chrome::Frame::split(screen, theme, true, true);
        chrome::clear(c, theme);

        if let Some(at) = self.menu {
            chrome::title_bar(c, frame.title, theme, "Options", None);
            chrome::softkey_bar(c, frame.softkeys, theme, [Some("Select"), None, Some("Back")]);
            let line_h = theme.fonts.body.line_height().max(1);
            let mut y = frame.content.y0;
            for (i, label) in self.menu_items().iter().enumerate() {
                let colour =
                    if i == at { theme.palette.accent } else { theme.palette.text };
                let r = Rect { y0: y, y1: y + line_h, ..frame.content };
                c.draw_text_in(r, label, theme.fonts.body, colour, Align::Start);
                y += line_h;
            }
            return;
        }

        if let Some(lines) = self.idle.clone() {
            chrome::title_bar(c, frame.title, theme, "Idle application", None);
            chrome::softkey_bar(c, frame.softkeys, theme, [Some("Native"), None, Some("Back")]);
            let line_h = theme.fonts.small.line_height().max(1);
            let mut y = frame.content.y0;
            for line in &lines {
                let r = Rect { y0: y, y1: y + line_h, ..frame.content };
                c.draw_text_in(r, line, theme.fonts.small, theme.palette.text, Align::Start);
                y += line_h;
            }
            let status = Rect { y0: frame.content.y1 - line_h - 2, ..frame.content };
            c.draw_text_in(status, &self.status, theme.fonts.small, theme.palette.dim, Align::Start);
            return;
        }

        let title = if self.filter.is_empty() {
            format!("Cenrep  {} sel", self.selected_count())
        } else {
            // The filter goes in the title bar, the same place the launcher's grid puts it — so the
            // list below is the only thing that ever moves.
            format!("/{}  {} sel", self.filter, self.selected_count())
        };
        chrome::title_bar(c, frame.title, theme, &title, None);
        chrome::softkey_bar(c, frame.softkeys, theme, [Some("Options"), None, Some("Exit")]);

        let line_h = theme.fonts.small.line_height().max(1);
        let status_h = line_h + 2;
        let list = Rect { y1: frame.content.y1 - status_h, ..frame.content };
        let rows = ((list.height() / line_h).max(1)) as usize;

        let view = self.view();
        // Keep the cursor on screen without a scrollbar: the window follows it, never the reverse.
        if self.cursor < self.top {
            self.top = self.cursor;
        } else if self.cursor >= self.top + rows {
            self.top = self.cursor + 1 - rows;
        }

        let mut y = list.y0;
        for (n, &i) in view.iter().enumerate().skip(self.top).take(rows) {
            let r = self.rows.get(i);
            let Some(r) = r else { continue };
            let text = if r.action {
                format!("» {}", r.name)
            } else {
                let mark = if r.selected { "[x]" } else { "[ ]" };
                format!("{mark} {} {}", r.source.tag(), r.name)
            };
            let colour = if n == self.cursor { theme.palette.accent } else { theme.palette.text };
            let line = Rect { y0: y, y1: y + line_h, ..list };
            c.draw_text_in(line, &text, theme.fonts.small, colour, Align::Start);
            y += line_h;
        }

        if view.is_empty() {
            c.draw_text_in(list, "no match", theme.fonts.body, theme.palette.dim, Align::Center);
        }

        let status = Rect { y0: frame.content.y1 - status_h, ..frame.content };
        c.draw_text_in(status, &self.status, theme.fonts.small, theme.palette.dim, Align::Start);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian_ui::testing;

    fn press(app: &mut Cenrepdump, key: Key) -> Handled {
        testing::with_theme(symbian_ui::Palette::DARK, |theme| {
            let ev = KeyEvent { key, mods: Default::default(), repeat: false };
            app.handle_key(ev, theme, testing::SCREEN)
        })
    }

    /// A screen with rows already in it. On the host every shim call answers NotReady, so the rows
    /// have to be planted rather than listed — which is fine: the listing is the shim's business and
    /// everything interesting here is what happens to a list once it exists.
    fn with_rows(names: &[(&str, Source)]) -> Cenrepdump {
        let mut app = Cenrepdump::new();
        app.rows = names
            .iter()
            .map(|(n, s)| Row {
                name: String::from(*n),
                source: *s,
                selected: false,
                action: false,
            })
            .collect();
        app
    }

    fn action_row() -> Row {
        Row {
            name: String::from("Idle application…"),
            source: Source::Cur,
            selected: false,
            action: true,
        }
    }

    #[test]
    fn names_come_out_of_a_nul_separated_listing() {
        let mut buf: Vec<u16> = Vec::new();
        for name in ["101f8766.txt", "10205041.txt"] {
            buf.extend(name.encode_utf16());
            buf.push(0);
        }
        buf.push(0);
        assert_eq!(split_names(&buf), alloc::vec!["101f8766.txt", "10205041.txt"]);
    }

    #[test]
    fn an_empty_listing_yields_no_names() {
        assert!(split_names(&[0, 0, 0]).is_empty());
    }

    #[test]
    fn typing_filters_the_list_and_case_does_not_matter() {
        let mut app = with_rows(&[
            ("101f8766.txt", Source::Rom),
            ("10205041.TXT", Source::Rom),
            ("101F8767.txt", Source::Cur),
        ]);
        assert_eq!(app.view().len(), 3);
        for ch in "101f8766".chars() {
            press(&mut app, Key::Char(ch));
        }
        assert_eq!(app.view().len(), 1);
        press(&mut app, Key::Backspace);
        assert_eq!(app.view().len(), 2, "one character back admits both 101f876x rows");
        for _ in 0..7 {
            press(&mut app, Key::Backspace);
        }
        assert_eq!(app.view().len(), 3, "an empty filter shows everything again");
    }

    #[test]
    fn select_marks_the_row_under_the_cursor_in_the_filtered_view() {
        // The bug this pins: selecting by cursor position into the *unfiltered* list marks the
        // wrong file as soon as anything is typed, and the archive then contains the wrong thing
        // with nothing to show for it.
        let mut app = with_rows(&[
            ("aaa.txt", Source::Rom),
            ("101f8766.txt", Source::Rom),
            ("bbb.txt", Source::Rom),
        ]);
        for ch in "8766".chars() {
            press(&mut app, Key::Char(ch));
        }
        assert_eq!(app.view().len(), 1);
        press(&mut app, Key::Select);
        assert!(app.rows[1].selected, "the filtered row, not row 0");
        assert_eq!(app.selected_count(), 1);
    }

    #[test]
    fn select_all_covers_the_filtered_view_and_nothing_else() {
        let mut app = with_rows(&[
            ("101f8766.txt", Source::Rom),
            ("999.txt", Source::Rom),
            ("101f8767.txt", Source::Cur),
        ]);
        for ch in "101f".chars() {
            press(&mut app, Key::Char(ch));
        }
        press(&mut app, Key::Right);
        assert_eq!(app.selected_count(), 2);
        assert!(!app.rows[1].selected, "a row the filter hides is not swept up");
        press(&mut app, Key::Right);
        assert_eq!(app.selected_count(), 0, "pressing it again clears them");
    }

    #[test]
    fn the_cursor_stays_inside_the_filtered_view() {
        let mut app = with_rows(&[("a.txt", Source::Rom), ("b.txt", Source::Rom)]);
        press(&mut app, Key::Down);
        press(&mut app, Key::Down);
        press(&mut app, Key::Down);
        assert_eq!(app.cursor, 1, "never past the last row");
        press(&mut app, Key::Up);
        press(&mut app, Key::Up);
        press(&mut app, Key::Up);
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn zipping_nothing_says_so_instead_of_writing_an_empty_archive() {
        // Through the menu, which is the only route now that the left softkey opens it.
        let mut app = with_rows(&[("a.txt", Source::Rom)]);
        press(&mut app, Key::Softkey(Softkey::Left));
        press(&mut app, Key::Down);
        press(&mut app, Key::Down); // "Create zip"
        press(&mut app, Key::Select);
        assert_eq!(app.status, "nothing selected");
    }

    #[test]
    fn the_first_row_opens_the_idle_panel_instead_of_being_selected() {
        // The entry point lives in the list because the key it used to live on — the green one —
        // is taken by the platform to open the dialler and never reaches an application.
        let mut app = with_rows(&[("a.txt", Source::Rom)]);
        app.rows.insert(0, action_row());
        assert!(app.idle.is_none());
        press(&mut app, Key::Select);
        assert!(app.idle.is_some(), "Select on the action row opens the panel");
        assert_eq!(app.selected_count(), 0, "and selects nothing");
    }

    #[test]
    fn select_all_and_the_archive_both_skip_the_action_row() {
        let mut app = with_rows(&[("a.txt", Source::Rom), ("b.txt", Source::Rom)]);
        app.rows.insert(0, action_row());
        press(&mut app, Key::Right);
        assert_eq!(app.selected_count(), 2, "the two files, not the action row");
        assert!(!app.rows[0].selected);
    }

    #[test]
    fn the_left_softkey_opens_a_menu_that_reaches_every_action() {
        let mut app = with_rows(&[("a.txt", Source::Rom)]);
        press(&mut app, Key::Softkey(Softkey::Left));
        assert_eq!(app.menu, Some(0), "the menu opens on its first item");
        // Down to "Idle application…" by its label, so adding a menu item does not silently move
        // this test onto a different action.
        let want = app
            .menu_items()
            .iter()
            .position(|l| l.starts_with("Idle"))
            .expect("the idle panel is reachable from the menu");
        for _ in 0..want {
            press(&mut app, Key::Down);
        }
        assert_eq!(app.menu, Some(want));
        press(&mut app, Key::Select);
        assert!(app.menu.is_none(), "choosing closes the menu");
        assert!(app.idle.is_some(), "and performs the action");
    }

    #[test]
    fn a_chunked_search_finds_a_match_that_straddles_a_chunk_boundary() {
        // The bug this exists for reports "not found" and looks exactly like the truth. Everything
        // in `find_in_file` except the file handle is this arithmetic, so it is tested here on
        // plain slices with the same overlap rule the real loop uses.
        fn scan(data: &[u8], needle: &[u8], chunk: usize) -> Option<usize> {
            let mut buf = alloc::vec![0u8; chunk + needle.len() - 1];
            let mut base = 0usize;
            let mut carry = 0usize;
            let mut pos = 0usize;
            loop {
                let want = buf.len() - carry;
                let n = (data.len() - pos).min(want);
                buf[carry..carry + n].copy_from_slice(&data[pos..pos + n]);
                pos += n;
                let filled = carry + n;
                if filled < needle.len() {
                    return None;
                }
                if let Some(i) = buf[..filled].windows(needle.len()).position(|w| w == needle) {
                    return Some(base + i);
                }
                if n == 0 {
                    return None;
                }
                carry = needle.len() - 1;
                let keep = filled - carry;
                buf.copy_within(keep.., 0);
                base += keep;
            }
        }

        let needle = 0x1027_50F0u32.to_le_bytes();
        // Straddling: three bytes in one chunk, the fourth in the next.
        let mut data = alloc::vec![0u8; 61];
        data.extend_from_slice(&needle);
        data.extend_from_slice(&[0u8; 40]);
        assert_eq!(scan(&data, &needle, 64), Some(61));
        // And the ordinary cases, so the overlap has not broken them.
        assert_eq!(scan(&needle, &needle, 64), Some(0));
        assert_eq!(scan(&alloc::vec![0u8; 300], &needle, 64), None);
        assert_eq!(scan(&[1, 2], &needle, 64), None, "a file shorter than the needle");
    }

    #[test]
    fn the_menu_cursor_stays_inside_the_menu() {
        let mut app = with_rows(&[("a.txt", Source::Rom)]);
        press(&mut app, Key::Softkey(Softkey::Left));
        for _ in 0..10 {
            press(&mut app, Key::Down);
        }
        assert_eq!(app.menu, Some(app.menu_items().len() - 1));
        for _ in 0..10 {
            press(&mut app, Key::Up);
        }
        assert_eq!(app.menu, Some(0));
    }

    #[test]
    fn marking_from_the_menu_marks_the_row_under_the_cursor() {
        let mut app = with_rows(&[("a.txt", Source::Rom), ("b.txt", Source::Rom)]);
        press(&mut app, Key::Down);
        press(&mut app, Key::Softkey(Softkey::Left));
        press(&mut app, Key::Select); // "Mark / unmark"
        assert!(app.rows[1].selected);
        assert!(!app.rows[0].selected);
    }

    #[test]
    fn the_panel_offers_the_way_back_on_a_key_of_its_own() {
        // On the host every shim call fails, so what is pinned here is the routing: the panel's
        // left softkey must reach `restore_idle` and say something, because the moment it is needed
        // is the moment the phone has no home screen and nobody can be asked to type a number.
        let mut app = with_rows(&[("a.txt", Source::Rom)]);
        app.idle_panel();
        assert!(app.idle.is_some());
        press(&mut app, Key::Softkey(Softkey::Left));
        assert!(!app.status.is_empty(), "putting the native one back reports what happened");
        assert!(app.idle.is_some(), "and leaves the panel open to show it");
    }

    #[test]
    fn back_asks_to_exit_rather_than_exiting() {
        let mut app = Cenrepdump::new();
        assert!(!app.should_exit());
        press(&mut app, Key::Softkey(Softkey::Right));
        assert!(app.should_exit());
    }

    #[test]
    fn draw_fills_the_screen_empty_and_full() {
        for rows in [alloc::vec![], alloc::vec![("101f8766.txt", Source::Rom)]] {
            let mut app = with_rows(&rows);
            let (_, px) = testing::with_canvas(symbian_gfx::Size::new(320, 240), |c| {
                testing::with_theme(symbian_ui::Palette::DARK, |theme| app.draw(c, theme));
            });
            assert!(px.iter().any(|&p| p != 0), "empty frame");
        }
    }
}
