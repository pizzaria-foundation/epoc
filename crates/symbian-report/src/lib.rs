//! The text format every on-device probe writes, and the reader that merges them.
//!
//! # Why this is a crate and not a struct inside one app
//!
//! `examples/selftest` invented this format and proved it: a fixed-width verdict prefix
//! so the file can be grepped for `FAIL` and skimmed by eye, and a rewrite of the whole
//! file after every phase so that a crash leaves a report naming the phase it died in.
//!
//! `apps/devdump` needs the same thing from a dozen separate binaries at once, because a
//! probe that links an unproven library can stop the E32 loader dead and must therefore
//! live in its own image (see `docs/device-notes.md`, "An import that does not resolve
//! makes the app vanish"). Twelve copies of the format would drift; one copy with a test
//! pinning the grammar does not.
//!
//! # The grammar
//!
//! ```text
//! == BEGIN system
//!
//! == drives
//!   ok   C: present
//!   FAIL E: readable  err -18
//!   .    C: free: 41216 KB
//! == END system ok=1 fail=1
//! ```
//!
//! Four line shapes, and every one of them starts with a fixed prefix so a reader can
//! classify a line without parsing it:
//!
//! | prefix | meaning |
//! |---|---|
//! | `== `  | a section head, or one of the two sentinels below |
//! | `  ok   ` | a check that passed |
//! | `  FAIL ` | a check that failed |
//! | `  .    ` | a measurement with no verdict |
//!
//! # The sentinels, and what their absence means
//!
//! A probe opens with `== BEGIN <name>` and closes with `== END <name> ok=N fail=M`.
//! Neither is decoration. The launcher that runs the probes cannot tell "finished" from
//! "died" by watching the process — `shim_process_running` reports liveness, and a probe
//! that panics mid-write stops being alive exactly like one that completed. The END line
//! is the only thing that survives the process to say which it was. Hence three
//! distinguishable outcomes, in [`Status`]:
//!
//! - file with an END line — ran to completion,
//! - file without one — started and died partway, and the last line written says where,
//! - no file at all — the image never ran, which on this platform is what a missing
//!   import looks like.
//!
//! That third case is why the launcher writes its manifest *before* launching anything:
//! an absence is only evidence if something recorded that it was expected.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;

use symbian::fs::{self, Fs, Utf16Path};

/// Fixed-width so `FAIL` can be grepped and the names line up in a column.
const PASS_PREFIX: &str = "  ok   ";
const FAIL_PREFIX: &str = "  FAIL ";
const INFO_PREFIX: &str = "  .    ";
const HEAD_PREFIX: &str = "== ";
const BEGIN_PREFIX: &str = "== BEGIN ";
const END_PREFIX: &str = "== END ";

/// An accumulating report, rewritten to disk in full on every [`Report::flush`].
///
/// Held in memory and rewritten rather than appended because a partial line from a
/// half-completed write is worse than a shorter file: it reads as data.
pub struct Report {
    name: String,
    text: String,
    pass: u32,
    fail: u32,
    path: Option<Utf16Path>,
    path_label: String,
    dir: String,
    reachable: bool,
}

impl Report {
    /// Opens a report for a section, emitting the BEGIN sentinel immediately.
    ///
    /// Nothing is written to disk until [`Report::open_output`] has chosen a path.
    pub fn new(name: &str) -> Self {
        let mut text = String::new();
        text.push_str(BEGIN_PREFIX);
        text.push_str(name);
        text.push('\n');
        Report {
            name: String::from(name),
            text,
            pass: 0,
            fail: 0,
            path: None,
            path_label: String::new(),
            dir: String::new(),
            reachable: false,
        }
    }

    /// Picks somewhere writable, most useful first, and remembers which rung won.
    ///
    /// The order is deliberate and was settled by `examples/selftest`. `C:\Data` first
    /// because it is the one location on this handset already known to be writable with no
    /// capability, reachable over both USB and Bluetooth, and where the SDK's apps already
    /// write their logs. `E:` would be nicer for a report — it appears as a mass-storage
    /// volume — but a phone with no memory card makes it a dead end.
    ///
    /// # `dir` and why it is usually empty
    ///
    /// It was not. The first version of this put every section in a `dump\` subdirectory,
    /// and the subdirectory was never created: `RFs::MkDirAll` requires a path that **ends
    /// in a separator**, because without one it treats the last component as a filename and
    /// ignores it. So every write answered `KErrPathNotFound`, the ladder fell through to
    /// the private data cage, and the report landed somewhere the file manager cannot see —
    /// with the screen cheerfully naming a path that did not exist.
    ///
    /// The fix is not only the trailing separator. It is that a subdirectory bought nothing:
    /// the filenames already sort into reading order, `C:\Data` is flat and known-good, and
    /// a directory that has to be created is one more thing between a measurement and the
    /// disk. `dir` is kept because a caller may still want one, and it now gets the
    /// separator `MkDirAll` needs.
    ///
    /// # The private cage is a last resort, and for one writer only
    ///
    /// It always works and nobody can reach it, so a report that lands there is a report
    /// nobody can carry away. Worse, the cage is **per-UID3**: a fleet of separate probe
    /// binaries each falls into its *own* cage, and no one of them can read another's
    /// without `AllFiles`. So a run that ends up here does not produce one scattered report
    /// — it produces N reports that cannot be assembled. [`Report::reachable`] is how a
    /// caller can say so out loud instead of appearing to have succeeded.
    pub fn open_output<F: Fs>(&mut self, fs: &mut F, dir: &str, filename: &str) {
        for drive in ["C:\\Data\\", "E:\\", "C:\\"] {
            let mut full = String::from(drive);
            full.push_str(dir);
            if !dir.is_empty() {
                // WITH the trailing separator. RFs::MkDirAll ignores the last component of a
                // path that lacks one, so trimming it — which this used to do — quietly
                // created the parent and not the directory asked for.
                let with_sep = if full.ends_with('\\') {
                    full.clone()
                } else {
                    let mut w = full.clone();
                    w.push('\\');
                    w
                };
                if let Ok(d) = Utf16Path::new(&with_sep) {
                    // Failure is not fatal: it may already exist, and if it genuinely
                    // cannot be made the write below fails and we move to the next rung.
                    let _ = fs.mkdir(d.as_units());
                }
            }
            let dir_len = full.len();
            full.push_str(filename);
            let Ok(p) = Utf16Path::new(&full) else { continue };
            // Probe by writing, not by opening: a rung can be listable and not writable, and
            // the only question that matters is whether the report can land there.
            if fs::write_atomic(fs, &p, self.text.as_bytes()).is_ok() {
                self.dir = String::from(&full[..dir_len]);
                self.path_label = full;
                self.path = Some(p);
                self.reachable = true;
                return;
            }
        }
        if let Ok(d) = fs::private_path(fs) {
            if let Ok(p) = Utf16Path::join(d.as_units(), filename) {
                if fs::write_atomic(fs, &p, self.text.as_bytes()).is_ok() {
                    self.path = Some(p);
                    // Deliberately prose, and deliberately NOT a usable directory: nothing
                    // may read this back and try to find its siblings there. `dir` stays
                    // empty and `reachable` stays false, which is what a caller must branch
                    // on rather than on the shape of this string.
                    self.path_label = String::from("PRIVATE CAGE - unreachable");
                    self.dir = String::new();
                }
            }
        }
    }

    /// The directory the report landed in, or empty when it fell into the private cage.
    ///
    /// Separate from [`Report::path_label`] on purpose. The label is for a human and, in the
    /// cage case, is prose — an earlier version used the label as the directory to look for
    /// sibling sections in, which meant a failed ladder produced a reader silently searching
    /// a path made of English.
    pub fn dir(&self) -> &str {
        &self.dir
    }

    /// Whether the report landed somewhere it can actually be fetched from.
    ///
    /// False means the private cage: the file exists, and neither the file manager, USB, nor
    /// a sibling process can reach it.
    pub fn reachable(&self) -> bool {
        self.reachable
    }

    /// A section head. Blank line before it, so the file has visible structure.
    pub fn head(&mut self, s: &str) {
        self.text.push('\n');
        self.text.push_str(HEAD_PREFIX);
        self.text.push_str(s);
        self.text.push('\n');
    }

    /// A raw line, for the rare thing that fits none of the shapes above.
    pub fn line(&mut self, s: &str) {
        self.text.push_str(s);
        self.text.push('\n');
    }

    /// A check with a verdict.
    pub fn check(&mut self, name: &str, ok: bool) {
        self.verdict(ok);
        self.text.push_str(name);
        self.text.push('\n');
    }

    /// A check with a verdict and a note — an error code, a measured value, a reason.
    ///
    /// Prefer this to [`Report::check`] whenever there is a number to carry. A bare FAIL
    /// says something is wrong; a FAIL with `err -46` says what.
    pub fn check_note(&mut self, name: &str, ok: bool, note: &str) {
        self.verdict(ok);
        self.text.push_str(name);
        self.text.push_str("  ");
        self.text.push_str(note);
        self.text.push('\n');
    }

    fn verdict(&mut self, ok: bool) {
        if ok {
            self.pass += 1;
            self.text.push_str(PASS_PREFIX);
        } else {
            self.fail += 1;
            self.text.push_str(FAIL_PREFIX);
        }
    }

    /// A measurement with no verdict. Most of a reconnaissance report is these.
    pub fn info(&mut self, key: &str, value: &str) {
        self.text.push_str(INFO_PREFIX);
        self.text.push_str(key);
        self.text.push_str(": ");
        self.text.push_str(value);
        self.text.push('\n');
    }

    /// [`Report::info`] with an integer value.
    pub fn num(&mut self, key: &str, v: i64) {
        let mut s = String::new();
        push_i64(&mut s, v);
        self.info(key, &s);
    }

    /// A breadcrumb written *before* the step it names is attempted.
    ///
    /// The ordering is the whole point, and it is the lesson `examples/imgprobe` paid for:
    /// a step that wedges or takes the process down is recorded by the fact that its
    /// breadcrumb has no result under it. Written after the fact, it would record only
    /// the steps that survived.
    pub fn entering<F: Fs>(&mut self, fs: &mut F, step: &str) {
        self.text.push_str("\n-- entering ");
        self.text.push_str(step);
        self.text.push('\n');
        self.flush(fs);
    }

    /// Rewrites the whole file.
    ///
    /// Called after every phase, not once at the end. On a platform where a fault shows
    /// as the application simply closing, the last line on disk is the diagnosis.
    pub fn flush<F: Fs>(&mut self, fs: &mut F) {
        if let Some(p) = &self.path {
            let _ = fs::write_atomic(fs, p, self.text.as_bytes());
        }
    }

    /// Emits the END sentinel and flushes. After this the section is complete, and a
    /// reader can tell so.
    pub fn finish<F: Fs>(&mut self, fs: &mut F) {
        self.text.push_str(END_PREFIX);
        self.text.push_str(&self.name);
        self.text.push_str(" ok=");
        push_i64(&mut self.text, self.pass as i64);
        self.text.push_str(" fail=");
        push_i64(&mut self.text, self.fail as i64);
        self.text.push('\n');
        self.flush(fs);
    }

    pub fn passed(&self) -> u32 {
        self.pass
    }

    pub fn failed(&self) -> u32 {
        self.fail
    }

    /// Which output rung won, for drawing on screen. Empty if none did.
    pub fn path_label(&self) -> &str {
        &self.path_label
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

/// What reading a section file back says about how its probe ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// An END sentinel is present: the probe ran to completion with these counts.
    Complete { pass: u32, fail: u32 },
    /// A BEGIN but no END: the probe started and died partway. The last line written
    /// names where.
    Truncated,
    /// Not a report at all — no BEGIN line. A stale file, or a truncated write.
    Malformed,
}

/// Classifies a section file's contents.
///
/// Deliberately tolerant about everything except the two sentinels: probes are free to
/// write whatever they like in between, and a reader that insisted on the rest of the
/// grammar would turn a probe's formatting slip into a lost result.
pub fn status(text: &str) -> Status {
    let mut seen_begin = false;
    let mut result = Status::Malformed;
    for line in text.lines() {
        if line.starts_with(BEGIN_PREFIX) {
            seen_begin = true;
            result = Status::Truncated;
        } else if let Some(rest) = line.strip_prefix(END_PREFIX) {
            if !seen_begin {
                continue;
            }
            // "<name> ok=N fail=M" — the name may contain spaces, so scan for the keys
            // rather than splitting positionally.
            let pass = field(rest, "ok=");
            let fail = field(rest, "fail=");
            if let (Some(pass), Some(fail)) = (pass, fail) {
                result = Status::Complete { pass, fail };
            }
        }
    }
    result
}

/// The name a section declares in its BEGIN line, if it has one.
pub fn section_name(text: &str) -> Option<&str> {
    text.lines().find_map(|l| l.strip_prefix(BEGIN_PREFIX)).map(str::trim)
}

fn field(s: &str, key: &str) -> Option<u32> {
    let at = s.find(key)? + key.len();
    let rest = &s[at..];
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    rest[..end].parse().ok()
}

/// Decimal formatting without `core::fmt`.
///
/// `format!` on this target drags in the whole formatting machinery for what is a dozen
/// divisions; the report writes hundreds of these.
pub fn push_i64(s: &mut String, mut v: i64) {
    if v < 0 {
        s.push('-');
        // Negated as i64 after the sign is emitted, so i64::MIN does not overflow.
        let mut d = [0u8; 20];
        let mut n = 0;
        let mut u = (v as i128).unsigned_abs();
        loop {
            d[n] = b'0' + (u % 10) as u8;
            n += 1;
            u /= 10;
            if u == 0 {
                break;
            }
        }
        for i in (0..n).rev() {
            s.push(d[i] as char);
        }
        return;
    }
    let mut d = [0u8; 20];
    let mut n = 0;
    loop {
        d[n] = b'0' + (v % 10) as u8;
        n += 1;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    for i in (0..n).rev() {
        s.push(d[i] as char);
    }
}

/// Hex, lower case, no prefix.
pub fn push_hex(s: &mut String, v: u32, digits: usize) {
    for i in (0..digits).rev() {
        let nib = (v >> (i * 4)) & 0xf;
        s.push(char::from_digit(nib, 16).unwrap_or('?'));
    }
}

#[cfg(test)]
mod tests {
    /// Report text is read on the handset, in a viewer that mangles anything outside ASCII —
    /// an em dash came back as three bytes of noise in the middle of a line. Every string this
    /// crate emits of its own must stay in ASCII; a probe's own text is its business, but the
    /// scaffolding around it is read on every report.
    #[test]
    fn the_scaffolding_is_ascii_only() {
        let mut r = super::Report::new("x");
        r.head("h");
        r.line("l");
        r.check("c", true);
        r.check_note("n", false, "why");
        r.info("k", "v");
        r.num("i", 42);
        for (i, ch) in r.text().chars().enumerate() {
            assert!(ch.is_ascii(), "non-ASCII {ch:?} at {i} in report scaffolding");
        }
    }

    use super::*;
    use symbian::fs::MemFs;

    /// The grammar is an interface: `grep FAIL` and the merge reader both depend on these
    /// exact prefixes. Pinned here so a tidy-up cannot quietly change them.
    #[test]
    fn the_line_shapes_are_fixed() {
        let mut r = Report::new("demo");
        r.head("drives");
        r.check("C: present", true);
        r.check_note("E: readable", false, "err -18");
        r.info("C: free", "41216 KB");
        r.num("count", -7);
        assert_eq!(
            r.text(),
            "== BEGIN demo\n\
             \n== drives\n\
             \x20 ok   C: present\n\
             \x20 FAIL E: readable  err -18\n\
             \x20 .    C: free: 41216 KB\n\
             \x20 .    count: -7\n"
        );
    }

    #[test]
    fn finish_emits_the_sentinel_with_counts() {
        let mut fs = MemFs::new();
        let mut r = Report::new("demo");
        r.check("a", true);
        r.check("b", true);
        r.check("c", false);
        r.finish(&mut fs);
        assert!(r.text().ends_with("== END demo ok=2 fail=1\n"), "{}", r.text());
    }

    #[test]
    fn a_finished_section_reads_back_as_complete() {
        let mut fs = MemFs::new();
        let mut r = Report::new("system");
        r.check("x", true);
        r.finish(&mut fs);
        assert_eq!(status(r.text()), Status::Complete { pass: 1, fail: 0 });
        assert_eq!(section_name(r.text()), Some("system"));
    }

    /// The case the launcher's manifest exists to distinguish: the probe ran, wrote, and
    /// died before finishing. Its breadcrumb is the diagnosis.
    #[test]
    fn an_unfinished_section_reads_back_as_truncated() {
        let mut r = Report::new("msg");
        r.entering(&mut MemFs::new(), "CMsvSession::OpenSyncL");
        assert_eq!(status(r.text()), Status::Truncated);
        assert!(r.text().trim_end().ends_with("-- entering CMsvSession::OpenSyncL"));
    }

    #[test]
    fn a_file_that_is_not_a_report_is_malformed() {
        assert_eq!(status(""), Status::Malformed);
        assert_eq!(status("hello\nworld\n"), Status::Malformed);
        // An END with no BEGIN is not a section either — most likely a stale tail.
        assert_eq!(status("== END msg ok=1 fail=0\n"), Status::Malformed);
    }

    /// Section names are written by hand and will eventually contain a space. Parsing the
    /// counts positionally would break silently on that, so it scans for the keys.
    #[test]
    fn counts_survive_a_name_with_spaces() {
        assert_eq!(
            status("== BEGIN a b\n== END a b ok=12 fail=345\n"),
            Status::Complete { pass: 12, fail: 345 }
        );
    }

    #[test]
    fn open_output_prefers_c_data_and_reports_which_rung_won() {
        let mut fs = MemFs::new();
        let mut r = Report::new("system");
        r.open_output(&mut fs, "dump\\", "10-system.txt");
        assert_eq!(r.path_label(), "C:\\Data\\dump\\10-system.txt");
    }

    /// The BEGIN line has to reach disk before the first phase runs, or a probe that dies
    /// immediately is indistinguishable from one whose image never loaded.
    #[test]
    fn open_output_writes_immediately() {
        let mut fs = MemFs::new();
        let mut r = Report::new("system");
        r.open_output(&mut fs, "", "s.txt");
        let p = Utf16Path::new("C:\\Data\\s.txt").unwrap();
        let got = fs::read(&mut fs, &p).unwrap().unwrap();
        assert_eq!(core::str::from_utf8(&got).unwrap(), "== BEGIN system\n");
    }

    #[test]
    fn push_i64_handles_the_extremes() {
        let mut s = String::new();
        push_i64(&mut s, 0);
        push_i64(&mut s, i64::MAX);
        push_i64(&mut s, i64::MIN);
        assert_eq!(s, "09223372036854775807-9223372036854775808");
    }

    #[test]
    fn push_hex_pads_to_width() {
        let mut s = String::new();
        push_hex(&mut s, 0xe07d11e5, 8);
        s.push(' ');
        push_hex(&mut s, 0xa5, 2);
        assert_eq!(s, "e07d11e5 a5");
    }
}
