//! Whether this handset has SQLite, and what it costs.
//!
//! # Why this exists
//!
//! `sqldb.h` is in the public SDK and `sqldb.dso` exports the whole of `RSqlDatabase` and
//! `RSqlStatement` — I checked the symbols. But the API is marked `@prototype`, no shipped
//! SDK example links it, and the S60 3.2 documentation is silent on whether a given handset
//! carries `sqldb.dll` at all. A static import of a DLL the phone does not have stops the
//! loader with no error, no log and no report file, so the question cannot be asked from
//! inside an app that matters. Hence a separate binary with its own UID: if it starts, the
//! DLL is there.
//!
//! Six things are unknown and each one changes how `symbian::sql` gets used:
//!
//! - **Is `sqldb.dll` on the handset?** Answered by this app launching.
//! - **Are bind parameters zero-based?** Symbian SQL numbers them from 0 and sqlite3's own
//!   C API from 1. Get it wrong and the first `?` stays NULL while the statement *succeeds*
//!   — data quietly missing, no error anywhere. Phase 3 binds both ways and reports which
//!   one the engine actually accepted.
//! - **Does a transaction change the cost of a batch?** On a phone the answer decides
//!   whether an import of a chat history is seconds or minutes.
//! - **Does a prepared statement pay off?** Same question for the insert loop.
//! - **Does `PRAGMA` get through the SQL server?** `symbian::sql::migrate` keeps its
//!   version in a table because I could not assume it does. Phase 8 finds out, and if the
//!   answer is yes, that table can go.
//! - **What does the engine cost in bytes?** A database file has a page size and a floor,
//!   and this device has 45 MiB of usable RAM and a report to fit beside it.
//!
//! # Why the phases resume across launches
//!
//! Nothing here is asynchronous — Symbian SQL is a blocking API — so there is no waiting to
//! interleave, and the first version of this probe ran the whole battery in one launch, one
//! phase per timer tick, flushing the report after each. The shim's rule forced the ticks:
//! `rust_step` runs on the GUI thread and a long one freezes the whole phone.
//!
//! Then the handset taught the rest of it, twice, and both lessons are in the shape of this
//! file now.
//!
//! **Run one.** The index-base phase bound a parameter at index 2 of a two-parameter
//! statement, on purpose, to find out which numbering the engine used. An out-of-range index
//! does not return an error on this platform — the SQL client asserts, and an assertion is a
//! panic, which closes the application. The report ended at the *previous* phase and said
//! nothing about why, because a flush per phase is too coarse when one call inside it can end
//! the process. So: a breadcrumb before every call that could be fatal ([`SqlProbe::mark`]
//! writes the line and flushes it *then*), and the next phase index persisted before the
//! phase runs, so a phase that closes the app is skipped on the next launch.
//!
//! **Run two.** With breadcrumbs in place the report ended at `-> step`, which named the
//! culprit exactly: `RSqlStatement::Next()` on a prepared INSERT. Stepping a statement that
//! produces no row set panics for the same reason a bad index does. `symbian::sql` now has
//! two paths — `exec` for non-SELECT, `step` for SELECT — because the platform has two, and
//! choosing wrong is not an error that comes back.
//!
//! Run two also exposed a bug of the probe's own: a `live` flag set in the `open` phase meant
//! that after a relaunch every later phase reported "the database never opened", because the
//! phase that would have set it had run in a previous process. Any state a resuming probe
//! keeps in memory is state it does not have — each phase opens the database itself now.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use symbian::fs::{self, ShimFs, Utf16Path};
use symbian::sql::{self, Db, ShimSql, Value};
use symbian_ui::{chrome, App, Canvas, Handled, KeyEvent, Point, Rect, Theme};

/// Where the report lands, first one that works. `C:\Data\` is writable with
/// `WriteUserData` and visible to File Manager and Bluetooth, which is what makes a report
/// carryable off the phone; the private directory is neither, so it is the fallback.
const REPORT_PATHS: &[&str] = &["E:\\sqlprobe.txt", "C:\\Data\\sqlprobe.txt"];

/// The database goes in the private directory, which needs no capability. A non-secure
/// database there is an ordinary file, so the dev bridge can pull it and open it on the
/// host with any SQLite tool — which is the other half of why non-secure was the right
/// choice in the shim.
const DB_NAME: &str = "sqlprobe.db";

/// Which phase runs next, remembered across launches. Written *before* the phase is
/// attempted, so a phase that takes the application down is skipped on the next launch
/// rather than trapping the probe on it forever. Same device lesson as `imgprobe`'s row
/// index, learned again here for the same reason.
const STATE_PATH: &str = "C:\\Data\\sqlprobe-next.txt";

/// Rows in the batch phases. Enough that the per-row cost is visible over the timer
/// resolution, small enough that one phase stays well inside a single `rust_step`.
const BATCH: i64 = 200;
/// Rows for the deliberately slow comparison — one implicit transaction per insert. Fewer,
/// because if each one is a disk commit this is the phase that could take seconds.
const SLOW_BATCH: i64 = 20;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Phase {
    Env,
    Open,
    Schema,
    IndexBase,
    RoundTrip,
    BatchInTransaction,
    BatchWithout,
    PreparedReuse,
    Query,
    Pragma,
    Migrate,
    Errors,
}

/// The running order. An array rather than a `next()` chain because the position in it is
/// what gets persisted between launches, and an index is the only form of that which
/// survives a restart.
const PHASES: &[Phase] = &[
    Phase::Env,
    Phase::Open,
    Phase::Schema,
    Phase::IndexBase,
    Phase::RoundTrip,
    Phase::BatchInTransaction,
    Phase::BatchWithout,
    Phase::PreparedReuse,
    Phase::Query,
    Phase::Pragma,
    Phase::Migrate,
    Phase::Errors,
];

impl Phase {
    /// For the breadcrumb, so a report that stops mid-phase still names which one.
    fn label(self) -> &'static str {
        match self {
            Phase::Env => "env",
            Phase::Open => "open",
            Phase::Schema => "schema",
            Phase::IndexBase => "index base",
            Phase::RoundTrip => "round trip",
            Phase::BatchInTransaction => "batch in transaction",
            Phase::BatchWithout => "batch without transaction",
            Phase::PreparedReuse => "prepared reuse",
            Phase::Query => "query",
            Phase::Pragma => "pragma",
            Phase::Migrate => "migrate",
            Phase::Errors => "errors",
        }
    }
}

pub struct SqlProbe {
    report: String,
    screen: Vec<String>,
    out: Option<Utf16Path>,
    db_path: Option<Utf16Path>,
    /// Index into [`PHASES`]. Read from [`STATE_PATH`] at start-up, so a relaunch continues
    /// past whatever killed the last one.
    at: usize,
    timer: Option<i32>,
    done: bool,
}

impl SqlProbe {
    pub fn new() -> Self {
        let mut p = SqlProbe {
            report: String::new(),
            screen: Vec::new(),
            out: None,
            db_path: None,
            at: 0,
            timer: None,
            done: false,
        };

        // The output path and the resume point come first, before anything that could fail:
        // a run that cannot say where it got to is a run that teaches nothing.
        let mut fs = ShimFs;
        p.open_output(&mut fs);
        p.at = read_next(&mut fs);
        // A finished battery leaves the index at the end of the list. Starting over here
        // rather than reporting "done" and resetting means a relaunch always *runs* something
        // — otherwise every completed run costs an extra launch that does nothing, which is
        // exactly the kind of small friction that gets a diagnostic abandoned.
        let restarted = p.at >= PHASES.len();
        if restarted {
            p.at = 0;
        }
        // Append rather than overwrite, so a relaunch adds to what the previous ones found
        // instead of erasing it.
        p.report = read_report(&mut fs, p.out.as_ref());

        if p.report.is_empty() {
            p.line("sqlprobe: does this handset have SQLite, and what does it cost");
            p.line("");
            p.line("Reaching this line at all is the first answer: the app links sqldb.dll");
            p.line("statically, so a handset without it would not have started.");
            p.line("");
            p.line("Phases resume across launches. If the app closes by itself, relaunch it:");
            p.line("the phase that closed it is skipped and the last line names the call.");
            p.line("");
        } else {
            p.line("");
            p.line("--- relaunched ---");
            let mut s = if restarted {
                String::from("previous battery finished; starting over at phase ")
            } else {
                String::from("resuming at phase ")
            };
            push_i64(&mut s, p.at as i64);
            if let Some(ph) = PHASES.get(p.at) {
                s.push_str(" (");
                s.push_str(ph.label());
                s.push(')');
            }
            p.line(&s);
            p.line("");
        }
        p.arm();
        p
    }

    /// Pick the report path: first location that accepts a write.
    ///
    /// `C:\Data\` is writable with `WriteUserData` and visible to File Manager and
    /// Bluetooth, which is what makes a report carryable off the phone. `E:\` is tried first
    /// only because a memory card is also visible over USB mass storage; the private
    /// directory is neither, so it is the last resort and the findings then have to be read
    /// off the screen.
    fn open_output(&mut self, fs: &mut ShimFs) {
        for candidate in REPORT_PATHS {
            if let Ok(p) = Utf16Path::new(candidate) {
                // Append rather than replace for the probe: an existing report from an
                // earlier launch is data, not litter.
                if fs::append_capped(fs, &p, b"", 1 << 20).is_ok() {
                    self.out = Some(p);
                    return;
                }
            }
        }
        if let Ok(dir) = fs::private_path(fs) {
            if let Ok(p) = Utf16Path::join(dir.as_units(), "sqlprobe.txt") {
                self.out = Some(p);
            }
        }
    }

    /// One phase per timer tick. 20 ms is long enough for the window server to get its
    /// turn and short enough that the whole battery finishes while someone watches.
    fn arm(&mut self) {
        self.timer = symbian::timer_after(20).ok();
    }

    fn tick(&mut self) {
        let Some(&phase) = PHASES.get(self.at) else {
            self.phase_done();
            let mut fs = ShimFs;
            // Back to zero, so the next launch starts a fresh battery rather than
            // reporting "done" forever.
            write_next(&mut fs, 0);
            self.flush(&mut fs);
            self.done = true;
            return;
        };

        // Persisted *before* the phase runs. That ordering is the whole mechanism: if this
        // phase closes the application, the next launch resumes after it.
        let mut fs = ShimFs;
        self.at += 1;
        write_next(&mut fs, self.at);

        self.run(phase);
        self.flush(&mut fs);
        self.arm();
    }

    fn run(&mut self, phase: Phase) {
        // Which phase is running, on disk, before it runs. Without this a report that stops
        // mid-phase does not even say which phase that was — the first version of this probe
        // stopped after "CREATE INDEX" and left the cause to be guessed at.
        let mut s = String::from("[");
        s.push_str(phase.label());
        s.push(']');
        self.mark(&s);

        match phase {
            Phase::Env => self.phase_env(),
            Phase::Open => self.phase_open(),
            Phase::Schema => self.with_db("schema", |p, db| p.phase_schema(db)),
            Phase::IndexBase => self.with_db("index base", |p, db| p.phase_index_base(db)),
            Phase::RoundTrip => self.with_db("round trip", |p, db| p.phase_round_trip(db)),
            Phase::BatchInTransaction => {
                self.with_db("batch in transaction", |p, db| p.phase_batch_tx(db))
            }
            Phase::BatchWithout => {
                self.with_db("batch without transaction", |p, db| p.phase_batch_plain(db))
            }
            Phase::PreparedReuse => self.with_db("prepared reuse", |p, db| p.phase_prepared(db)),
            Phase::Query => self.with_db("query", |p, db| p.phase_query(db)),
            Phase::Pragma => self.with_db("pragma", |p, db| p.phase_pragma(db)),
            Phase::Migrate => self.with_db("migrate", |p, db| p.phase_migrate(db)),
            Phase::Errors => self.with_db("errors", |p, db| p.phase_errors(db)),
        }
    }

    // ------------------------------------------------------------------- phases --

    fn phase_env(&mut self) {
        let mut fs = ShimFs;

        // The path was chosen in new(), before any of this could fail; reported here so it
        // appears in the body rather than only being implied by the file existing.
        let where_to = match &self.out {
            Some(p) => utf8(p.as_units()),
            None => String::from("nowhere -- screen only"),
        };
        self.kv("report", &where_to);

        match fs::private_path(&mut fs) {
            Ok(dir) => {
                match Utf16Path::join(dir.as_units(), DB_NAME) {
                    Ok(p) => {
                        self.kv("database", &utf8(p.as_units()));
                        self.db_path = Some(p);
                    }
                    Err(e) => self.err("joining the database path", e),
                }
            }
            Err(e) => self.err("private path", e),
        }
        self.line("");
    }

    fn phase_open(&mut self) {
        let Some(path) = self.db_path.clone() else {
            self.line("  open: skipped, no path");
            return;
        };

        // Delete first, so a rerun measures a fresh database rather than one already
        // holding the last run's rows. A missing file is the normal case on a first run.
        let mut backend = ShimSql;
        match backend.open_delete(&path) {
            Ok(true) => self.line("  removed the previous database"),
            Ok(false) => {}
            Err(e) => self.err("deleting the old database", e),
        }

        let t0 = symbian::monotonic_us();
        let mut sql = ShimSql;
        match Db::open(&mut sql, &path) {
            Ok(mut db) => {
                let us = symbian::monotonic_us() - t0;
                self.kv_us("create + open", us);
                match db.size() {
                    Ok(n) => self.kv_i("size after create (bytes)", n as i64),
                    Err(e) => self.err("size", e),
                }
                self.line("  OPEN: yes -- this handset has Symbian SQL");
            }
            Err(e) => {
                self.err("open", e);
                // Not "everything below is skipped": each phase opens the database itself
                // now, so each one gets to fail on its own and say so. A phase that
                // succeeds after this one failed is a finding, not a contradiction.
                self.line("  OPEN: no. Each phase below will try again and report.");
            }
        }
        self.line("");
    }

    fn phase_schema(&mut self, db: &mut Db<'_, ShimSql>) {
        let t0 = symbian::monotonic_us();
        let r = db.execute(
            "CREATE TABLE msg (id INTEGER PRIMARY KEY, chat INTEGER NOT NULL, \
             body TEXT, ts INTEGER, blob BLOB)",
        );
        let us = symbian::monotonic_us() - t0;
        match r {
            Ok(_) => self.kv_us("CREATE TABLE", us),
            Err(e) => {
                self.err("CREATE TABLE", e);
                self.msg(db);
            }
        }

        let t1 = symbian::monotonic_us();
        match db.execute("CREATE INDEX msg_chat ON msg (chat, ts)") {
            Ok(_) => self.kv_us("CREATE INDEX", symbian::monotonic_us() - t1),
            Err(e) => {
                self.err("CREATE INDEX", e);
                self.msg(db);
            }
        }
        self.line("");
    }

    /// The finding that decides whether `symbian::sql` is correct as written.
    ///
    /// # What the first version of this got wrong
    ///
    /// It bound a parameter at index 2 of a two-parameter statement on purpose, so that
    /// "which numbering does the engine use" could be answered by seeing which attempt was
    /// accepted. The engine answered by asserting, the assertion closed the application, and
    /// the report said nothing because the flush came at the end of the phase.
    ///
    /// So the out-of-range attempt is gone — the platform's answer to it is known now, and
    /// it is not a value this probe can carry home. What is left is the safe half: bind only
    /// indexes {0, 1}, and read the row back to see whether the values landed in the columns
    /// they were meant for. With a breadcrumb before each call, that one attempt settles it
    /// either way: if the platform were one-based, binding index 0 would be the out-of-range
    /// call, the app would close, and the last line in the file would say which bind did it.
    fn phase_index_base(&mut self, db: &mut Db<'_, ShimSql>) {
        self.line("  index base -- which number is the first bind parameter");
        self.line("  (a breadcrumb per call: if the app closes, the last line is the culprit)");

        let sql = "INSERT INTO msg (chat, body) VALUES (?, ?)";

        self.mark("    -> prepare a two-parameter INSERT");
        let placed;
        {
            let mut stmt = match db.prepare(sql) {
                Ok(s) => s,
                Err(e) => {
                    self.err("prepare", e);
                    return;
                }
            };

            self.mark("    -> bind index 0 (integer 1001)");
            if let Err(e) = stmt.bind(0, Value::Int(1001)) {
                self.err("bind at index 0", e);
                return;
            }
            self.mark("    -> bind index 1 (text)");
            if let Err(e) = stmt.bind(1, Value::Text("zero-based")) {
                self.err("bind at index 1", e);
                return;
            }
            // Exec, not step. The second run of this probe stepped here and the report ends
            // at the `-> step` breadcrumb: Next() on a statement with no row set panics and
            // closes the app. That is what this line being `exec` costs, and what it bought.
            self.mark("    -> exec (NOT step: stepping an INSERT closes the app)");
            match stmt.exec() {
                Ok(_) => placed = true,
                Err(e) => {
                    self.err("exec", e);
                    return;
                }
            }
        }
        self.kv("bind at {0,1} accepted", yn(placed));

        // Accepting a bind is not the same as putting the value where it belongs: a
        // statement whose parameters went to the wrong places still succeeds. Reading the
        // row back is what settles it — the integer has to be in `chat` and the text in
        // `body`, which only happens if index 0 meant the first `?`.
        self.mark("    -> select the row back");
        let mut chat_got = 0i64;
        let mut body_got = String::new();
        let r = db.query(
            "SELECT chat, body FROM msg WHERE chat = 1001",
            &[],
            |row| {
                chat_got = row.get_int(0)?;
                body_got = row.get_text(1)?;
                Ok(())
            },
        );
        match r {
            Ok(n) => {
                self.kv_i("rows read back", n as i64);
                let mut s = String::from("    chat=");
                push_i64(&mut s, chat_got);
                s.push_str(" body='");
                s.push_str(&body_got);
                s.push('\'');
                self.line(&s);
                if n == 1 && chat_got == 1001 && body_got == "zero-based" {
                    self.line("  VERDICT: zero-based, as symbian::sql assumes. Correct as written.");
                } else {
                    self.line("  VERDICT: the values did NOT land where they were bound.");
                    self.line("           symbian::sql::Stmt's index convention is wrong.");
                }
            }
            Err(e) => {
                self.err("reading back", e);
                self.msg(db);
            }
        }

        // And the finding the crash produced, recorded where the next reader will see it.
        self.line("");
        self.line("  NOTE: an out-of-range index is fatal here, not an error. Binding index 2");
        self.line("        of this statement closed the app on the first run of this probe.");
        self.line("        symbian::sql::Stmt::bind now refuses it in Rust instead.");
        self.line("");
    }

    fn phase_round_trip(&mut self, db: &mut Db<'_, ShimSql>) {
        // Non-ASCII on purpose. The bind path converts UTF-8 to UTF-16 and the column path
        // converts back; a cast instead of a conversion works for ASCII and mangles
        // everything else, which on a Brazilian phone is every second message.
        const TEXT: &str = "ação — não é 日本語 ✓";
        let blob: Vec<u8> = (0..300u32).map(|i| (i % 251) as u8).collect();

        let r = db.execute_with(
            "INSERT INTO msg (chat, body, ts, blob) VALUES (?, ?, ?, ?)",
            &[
                Value::Int(42),
                Value::Text(TEXT),
                Value::Int(1_700_000_000),
                Value::Blob(&blob),
            ],
        );
        if let Err(e) = r {
            self.err("insert with parameters", e);
            self.msg(db);
            return;
        }

        let mut ok_text = false;
        let mut ok_blob = false;
        let mut ok_ts = false;
        let r = db.query(
            "SELECT body, ts, blob FROM msg WHERE chat = ?",
            &[Value::Int(42)],
            |row| {
                let body = row.get_text(0)?;
                ok_text = body == TEXT;
                ok_ts = row.get_int(1)? == 1_700_000_000;
                let got = row.get_blob(2)?;
                ok_blob = got == blob;
                Ok(())
            },
        );
        match r {
            Ok(n) => self.kv_i("rows", n as i64),
            Err(e) => {
                self.err("select", e);
                self.msg(db);
            }
        }
        self.kv("text round trip (utf-16 both ways)", yn(ok_text));
        // 300 bytes is past the probe's inline buffer, so this also proves the two-pass
        // read in Stmt::get_blob works against the real engine and not only the fake.
        self.kv("blob round trip (300 bytes, two-pass)", yn(ok_blob));
        self.kv("int64 round trip", yn(ok_ts));
        self.line("");
    }

    fn phase_batch_tx(&mut self, db: &mut Db<'_, ShimSql>) {
        let t0 = symbian::monotonic_us();
        let r = db.transaction(|db| {
            let mut stmt = db.prepare("INSERT INTO msg (chat, body, ts) VALUES (?, ?, ?)")?;
            for i in 0..BATCH {
                stmt.run(&[Value::Int(7), Value::Text("batched"), Value::Int(i)])?;
            }
            Ok(())
        });
        let us = symbian::monotonic_us() - t0;
        match r {
            Ok(()) => {
                self.kv_i("rows (one transaction, prepared once)", BATCH);
                self.kv_us("total", us);
                self.kv_us("per row", us / BATCH as u64);
            }
            Err(e) => {
                self.err("batch in transaction", e);
                self.msg(db);
            }
        }
        self.line("");
    }

    fn phase_batch_plain(&mut self, db: &mut Db<'_, ShimSql>) {
        // No transaction and a fresh prepare per row: the naive loop, and the number that
        // says whether the transaction above is worth the code.
        let t0 = symbian::monotonic_us();
        let mut failed = None;
        for i in 0..SLOW_BATCH {
            let r = db.execute_with(
                "INSERT INTO msg (chat, body, ts) VALUES (?, ?, ?)",
                &[Value::Int(8), Value::Text("plain"), Value::Int(i)],
            );
            if let Err(e) = r {
                failed = Some(e);
                break;
            }
        }
        let us = symbian::monotonic_us() - t0;
        match failed {
            None => {
                self.kv_i("rows (implicit transaction each, prepared each)", SLOW_BATCH);
                self.kv_us("total", us);
                self.kv_us("per row", us / SLOW_BATCH as u64);
            }
            Some(e) => {
                self.err("plain batch", e);
                self.msg(db);
            }
        }
        self.line("");
    }

    fn phase_prepared(&mut self, db: &mut Db<'_, ShimSql>) {
        // Isolates the prepare cost from the commit cost: both loops are inside one
        // transaction, and only the prepare placement differs.
        let n = 50i64;

        let t0 = symbian::monotonic_us();
        let a = db.transaction(|db| {
            let mut stmt = db.prepare("INSERT INTO msg (chat, ts) VALUES (?, ?)")?;
            for i in 0..n {
                stmt.run(&[Value::Int(9), Value::Int(i)])?;
            }
            Ok(())
        });
        let reused = symbian::monotonic_us() - t0;

        let t1 = symbian::monotonic_us();
        let b = db.transaction(|db| {
            for i in 0..n {
                db.execute_with(
                    "INSERT INTO msg (chat, ts) VALUES (?, ?)",
                    &[Value::Int(10), Value::Int(i)],
                )?;
            }
            Ok(())
        });
        let fresh = symbian::monotonic_us() - t1;

        if a.is_err() || b.is_err() {
            self.line("  prepared reuse: a loop failed, see above");
            self.msg(db);
            self.line("");
            return;
        }
        self.kv_us("50 rows, prepared once", reused);
        self.kv_us("50 rows, prepared per row", fresh);
        self.kv_us("prepare cost per statement", fresh.saturating_sub(reused) / n as u64);
        self.line("");
    }

    fn phase_query(&mut self, db: &mut Db<'_, ShimSql>) {
        match db.query_int("SELECT count(*) FROM msg", &[]) {
            Ok(Some(n)) => self.kv_i("total rows", n),
            Ok(None) => self.line("  count returned no row, which should be impossible"),
            Err(e) => {
                self.err("count", e);
                self.msg(db);
            }
        }

        // The indexed read: msg_chat covers (chat, ts), so this is the shape a message
        // list actually uses -- newest first, one chat, a screenful.
        let t0 = symbian::monotonic_us();
        let mut rows = 0usize;
        let r = db.query(
            "SELECT id, ts FROM msg WHERE chat = ? ORDER BY ts DESC LIMIT 20",
            &[Value::Int(7)],
            |row| {
                let _ = row.get_int(0)?;
                let _ = row.get_int(1)?;
                rows += 1;
                Ok(())
            },
        );
        let us = symbian::monotonic_us() - t0;
        match r {
            Ok(_) => {
                self.kv_i("indexed select, rows", rows as i64);
                self.kv_us("indexed select (20 of 270, ORDER BY on the index)", us);
            }
            Err(e) => {
                self.err("indexed select", e);
                self.msg(db);
            }
        }

        // The same query with no index to use, for the contrast. body is unindexed, so
        // this is a table scan over everything inserted so far.
        let t1 = symbian::monotonic_us();
        let scan = db.query_int("SELECT count(*) FROM msg WHERE body = 'batched'", &[]);
        let us = symbian::monotonic_us() - t1;
        match scan {
            Ok(_) => self.kv_us("unindexed scan of the same table", us),
            Err(e) => self.err("scan", e),
        }

        match db.size() {
            Ok(n) => self.kv_i("database size now (bytes)", n as i64),
            Err(e) => self.err("size", e),
        }
        self.line("");
    }

    fn phase_pragma(&mut self, db: &mut Db<'_, ShimSql>) {
        // If this works, sql::migrate can drop its version table and use the field SQLite
        // already has. If it does not, the table was the right call.
        match db.execute("PRAGMA user_version = 7") {
            Ok(_) => {
                match db.query_int("PRAGMA user_version", &[]) {
                    Ok(Some(7)) => self.line("  PRAGMA user_version: works, reads back 7"),
                    Ok(other) => {
                        let mut s = String::from("  PRAGMA user_version: accepted but read back ");
                        match other {
                            Some(v) => push_i64(&mut s, v),
                            None => s.push_str("nothing"),
                        }
                        self.line(&s);
                    }
                    Err(e) => self.err("reading PRAGMA user_version", e),
                }
            }
            Err(e) => {
                self.err("PRAGMA user_version =", e);
                self.msg(db);
                self.line("  -- the version table in sql::migrate stays.");
            }
        }
        self.line("");
    }

    fn phase_migrate(&mut self, db: &mut Db<'_, ShimSql>) {
        let steps: &[&str] = &[
            "CREATE TABLE peer (id INTEGER PRIMARY KEY, name TEXT)",
            "ALTER TABLE peer ADD phone TEXT",
        ];
        match sql::migrate(db, steps) {
            Ok(v) => self.kv_i("migrate, version after first run", v as i64),
            Err(e) => {
                self.err("migrate", e);
                self.msg(db);
                self.line("");
                return;
            }
        }
        // Twice, because idempotence is the property that matters: the second call must
        // apply nothing and still report the same version.
        match sql::migrate(db, steps) {
            Ok(v) => self.kv_i("migrate again (must be a no-op)", v as i64),
            Err(e) => self.err("migrate, second call", e),
        }
        match db.execute_with(
            "INSERT INTO peer (name, phone) VALUES (?, ?)",
            &[Value::Text("Joshua"), Value::Text("+55")],
        ) {
            Ok(_) => self.line("  the migrated schema accepts the new column"),
            Err(e) => {
                self.err("insert into the migrated table", e);
                self.msg(db);
            }
        }
        self.line("");
    }

    fn phase_errors(&mut self, db: &mut Db<'_, ShimSql>) {
        // A deliberately wrong query, to find out whether LastErrorMessage carries the
        // reason. The error code alone says -311, which names nothing.
        match db.execute("SELECT nmae FROM msg") {
            Ok(_) => self.line("  a query on a missing column SUCCEEDED, which is wrong"),
            Err(e) => {
                let mut s = String::from("  bad column, error code ");
                push_i64(&mut s, e.code() as i64);
                self.line(&s);
                let m = db.last_error();
                if m.is_empty() {
                    self.line("  LastErrorMessage: empty -- the code is all we get");
                } else {
                    self.kv("LastErrorMessage", &m);
                }
            }
        }

        // The row-overrun read that used to be here is gone, and this note is what replaced
        // it. It read column 5 of a one-column row to find out whether the platform reports
        // an out-of-range column as an error or as NULL. The answer, established by the
        // index-base phase closing the application, is neither: it asserts. So the check
        // cost the run and could never have printed its own result.
        //
        // The guard for it lives in Rust now, in symbian::sql::Stmt, and there is nothing
        // left here worth risking a process for.
        self.line("  (no row-overrun check: an out-of-range column asserts, see index base)");

        // What is worth measuring is a wrong parameter *count*, because that is the mistake
        // a real caller makes, and it must come back as an error rather than as a closed app.
        self.mark("    -> bind one parameter too many (must be refused in Rust)");
        match db.execute_with(
            "INSERT INTO msg (chat) VALUES (?)",
            &[Value::Int(1), Value::Int(2)],
        ) {
            Ok(_) => self.line("  a two-value bind into a one-parameter statement SUCCEEDED"),
            Err(e) => {
                let mut s = String::from("  too many parameters refused in Rust, code ");
                push_i64(&mut s, e.code() as i64);
                s.push_str(" -- the platform was never reached");
                self.line(&s);
            }
        }
        self.line("");
    }

    fn phase_done(&mut self) {
        self.line("done. Carry the report off the phone and read the VERDICT lines.");
    }

    // -------------------------------------------------------------------- plumbing --

    /// Open the database, run `f`, close it. Every phase after `Open` needs a connection
    /// and none of them should hold one across a tick: a phase that dies mid-tick would
    /// otherwise leave the connection open for the rest of the run.
    ///
    /// # Why there is no "did the open phase succeed" flag any more
    ///
    /// There was one, and it turned a single crash into eight useless phases. The flag was
    /// set in the `open` phase, the crash in `index base` forced a relaunch, the relaunch
    /// resumed at `round trip` — and every phase from there reported "skipped, the database
    /// never opened" because the phase that would have set the flag had already run in a
    /// previous process.
    ///
    /// Any state a resuming probe keeps in memory is state it does not have. So this opens
    /// the database itself and reports its own failure, which costs an open per phase and
    /// cannot be wrong about a launch it was not present for.
    fn with_db<F>(&mut self, label: &str, f: F)
    where
        F: FnOnce(&mut Self, &mut Db<'_, ShimSql>),
    {
        let Some(path) = self.db_path.clone() else {
            let mut s = String::from("  ");
            s.push_str(label);
            s.push_str(": skipped, no database path");
            self.line(&s);
            return;
        };
        let mut sql = ShimSql;
        // The trailing semicolon is load-bearing: without it the match is the function's
        // tail expression, its temporaries outlive `sql`, and the borrow checker refuses.
        match Db::open(&mut sql, &path) {
            Ok(mut db) => f(self, &mut db),
            Err(e) => {
                let mut s = String::from("  ");
                s.push_str(label);
                s.push_str(": could not open the database");
                self.line(&s);
                self.err("open", e);
            }
        };
    }

    fn line(&mut self, s: &str) {
        self.report.push_str(s);
        self.report.push('\n');
        self.screen.push(String::from(s));
        if self.screen.len() > 12 {
            self.screen.remove(0);
        }
    }

    fn kv(&mut self, k: &str, v: &str) {
        let mut s = String::from("  ");
        s.push_str(k);
        s.push_str(": ");
        s.push_str(v);
        self.line(&s);
    }

    fn kv_i(&mut self, k: &str, v: i64) {
        let mut s = String::new();
        push_i64(&mut s, v);
        self.kv(k, &s);
    }

    /// Microseconds, printed as milliseconds with three decimals. A per-row insert cost is
    /// tens of microseconds and a commit is milliseconds; one unit has to show both.
    fn kv_us(&mut self, k: &str, us: u64) {
        let mut s = String::new();
        push_i64(&mut s, (us / 1000) as i64);
        s.push('.');
        let frac = us % 1000;
        if frac < 100 {
            s.push('0');
        }
        if frac < 10 {
            s.push('0');
        }
        push_i64(&mut s, frac as i64);
        s.push_str(" ms");
        self.kv(k, &s);
    }

    fn err(&mut self, what: &str, e: symbian::error::Error) {
        let mut s = String::from("  ! ");
        s.push_str(what);
        s.push_str(" failed: ");
        push_i64(&mut s, e.code() as i64);
        self.line(&s);
    }

    /// The engine's own message, when there is one. Called after a failure, because that
    /// is the only time it says anything.
    fn msg(&mut self, db: &mut Db<'_, ShimSql>) {
        let m = db.last_error();
        if !m.is_empty() {
            let mut s = String::from("    engine: ");
            s.push_str(&m);
            self.line(&s);
        }
    }

    /// A line written *and flushed now*, before the call it describes.
    ///
    /// The difference between this and [`SqlProbe::line`] is the whole lesson of the first
    /// run. `line` buffers and the phase flushes at its end, which is fine until a call
    /// inside the phase closes the application — and then the file holds everything except
    /// the one fact worth having. A `mark` costs a file write per call, which on a
    /// diagnostic is the cheapest thing in the room.
    fn mark(&mut self, s: &str) {
        self.line(s);
        let mut fs = ShimFs;
        self.flush(&mut fs);
    }

    fn flush(&mut self, fs: &mut ShimFs) {
        if let Some(p) = &self.out {
            let _ = fs::write_atomic(fs, p, self.report.as_bytes());
        }
    }
}

/// The report so far, so a relaunch appends instead of erasing.
fn read_report(fs: &mut ShimFs, path: Option<&Utf16Path>) -> String {
    let Some(p) = path else { return String::new() };
    match fs::read(fs, p) {
        Ok(Some(bytes)) => String::from_utf8_lossy(&bytes).into_owned(),
        _ => String::new(),
    }
}

/// Which phase to run next. Zero — the start — for a first run, an unreadable file, or
/// anything that does not parse: every one of those means "begin at the beginning", and
/// distinguishing them would only add ways to get stranded.
fn read_next(fs: &mut ShimFs) -> usize {
    let Ok(p) = Utf16Path::new(STATE_PATH) else { return 0 };
    match fs::read(fs, &p) {
        Ok(Some(bytes)) => {
            let mut n = 0usize;
            let mut any = false;
            for b in bytes {
                if b.is_ascii_digit() {
                    n = n * 10 + (b - b'0') as usize;
                    any = true;
                } else if any {
                    break;
                }
            }
            if any && n <= PHASES.len() {
                n
            } else {
                0
            }
        }
        _ => 0,
    }
}

fn write_next(fs: &mut ShimFs, n: usize) {
    let Ok(p) = Utf16Path::new(STATE_PATH) else { return };
    let mut s = String::new();
    push_i64(&mut s, n as i64);
    s.push('\n');
    // Atomic, so a battery pull between the write and the phase cannot leave a half-written
    // number that parses as some other phase.
    let _ = fs::write_atomic(fs, &p, s.as_bytes());
}

/// Deleting the previous database, as a method on the backend so the probe does not have to
/// name the shim directly. `Ok(false)` when there was nothing there.
trait DeleteDb {
    fn open_delete(&mut self, path: &Utf16Path) -> symbian::error::Result<bool>;
}

impl DeleteDb for ShimSql {
    fn open_delete(&mut self, path: &Utf16Path) -> symbian::error::Result<bool> {
        use symbian::sql::Sql;
        match self.delete(path.as_units()) {
            Ok(()) => Ok(true),
            Err(e) if e.is_missing() => Ok(false),
            Err(e) => Err(e),
        }
    }
}

impl Default for SqlProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl App for SqlProbe {
    fn title(&self) -> &str {
        "sqlprobe"
    }

    fn handle_key(&mut self, _ev: KeyEvent, _t: &Theme<'_>, _s: Rect) -> Handled {
        Handled::Ignored
    }

    fn handle_raw(&mut self, ev: &symbian_ui::RawEvent) -> Handled {
        if ev.kind == symbian_sys::SHIM_EV_TIMER && Some(ev.handle) == self.timer {
            self.timer = None;
            self.tick();
            return Handled::Consumed;
        }
        Handled::Ignored
    }

    fn draw(&mut self, c: &mut Canvas<'_>, theme: &Theme<'_>) {
        let screen = Rect::from_size(c.size());
        let frame = symbian_ui::Frame::split(screen, theme, true, true);
        chrome::clear(c, theme);
        chrome::title_bar(c, frame.title, theme, "sqlprobe", None);
        chrome::softkey_bar(c, frame.softkeys, theme, [None, None, Some("Sair")]);

        let small = theme.fonts.small;
        let mut y = frame.content.y0 + 2;
        for l in &self.screen {
            c.draw_text(
                Point::new(frame.content.x0 + 2, y + small.ascent()),
                l,
                small,
                theme.palette.text,
            );
            y += small.line_height();
        }
    }
}

fn yn(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "NO"
    }
}

fn utf8(units: &[u16]) -> String {
    char::decode_utf16(units.iter().copied())
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

/// Integer to text without `core::fmt`.
///
/// `format!` pulls the formatting machinery into the image for what is a division loop;
/// the other probes do the same and for the same reason.
fn push_i64(out: &mut String, mut v: i64) {
    if v < 0 {
        out.push('-');
        // Negated as i128 would be cleaner; this avoids the widening for the one value
        // that cannot be negated in place.
        if v == i64::MIN {
            out.push_str("9223372036854775808");
            return;
        }
        v = -v;
    }
    let mut digits = [0u8; 20];
    let mut n = 0;
    loop {
        digits[n] = b'0' + (v % 10) as u8;
        v /= 10;
        n += 1;
        if v == 0 {
            break;
        }
    }
    while n > 0 {
        n -= 1;
        out.push(digits[n] as char);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_phase_list_is_complete_and_labelled() {
        // Every phase in the running order needs a label, because the label is the
        // breadcrumb: a report that stops mid-phase says which one only through this.
        assert_eq!(PHASES.len(), 12, "the phase list changed; update the count deliberately");
        for p in PHASES {
            assert!(!p.label().is_empty());
        }
        // No duplicates: a repeated phase would run twice and the resume index would point
        // at the wrong one after a crash.
        for (i, a) in PHASES.iter().enumerate() {
            for b in &PHASES[i + 1..] {
                assert_ne!(a, b, "duplicate phase {a:?}");
            }
        }
    }

    #[test]
    fn a_resume_index_past_the_end_starts_over() {
        // The resume file is written before the phase runs, so the last write of a completed
        // battery is PHASES.len(). One past that must be treated as "start again" rather
        // than indexing off the end.
        assert!(PHASES.get(PHASES.len()).is_none());
    }

    #[test]
    fn integers_print_without_core_fmt() {
        let mut s = String::new();
        push_i64(&mut s, 0);
        push_i64(&mut s, -42);
        push_i64(&mut s, 1_700_000_000);
        assert_eq!(s, "0-421700000000");

        let mut s = String::new();
        push_i64(&mut s, i64::MIN);
        assert_eq!(s, "-9223372036854775808");
    }
}
