//! SQL, over the platform's own SQLite.
//!
//! # Why this exists
//!
//! Everything this SDK persisted before now went through [`crate::fs::write_atomic`]: read
//! the file, change it, write the whole thing back under a temporary name, rename. That is
//! the right shape for a settings blob and the wrong one for anything that grows. A message
//! store rewritten in full on every arriving message costs the whole file per message, and
//! on a 600 MHz ARM11 with a 45 MiB ceiling that stops being viable at a few thousand rows.
//!
//! The handset already ships the answer. `sqldb.dll` is SQLite behind a client-server API,
//! with indexes, transactions and partial reads. This module is the thin part on top.
//!
//! # Indexes are zero-based
//!
//! Both bind parameters and columns, unlike sqlite3's own C API, where parameters begin at
//! one. It is the platform's choice, not ours, and it is the single easiest thing to get
//! wrong here — a first parameter bound at index 1 goes to the *second* `?`, leaving the
//! first NULL, and the statement succeeds. `examples/sqlprobe` verifies the convention on
//! the handset rather than trusting the documentation.
//!
//! # Why a trait
//!
//! The same reason as [`crate::fs::Fs`]: the logic worth testing is above the FFI, not in
//! it. Binding a slice of parameters in the right order, stepping a result set to
//! exhaustion, reading a text column that did not fit the first buffer and going back for
//! it with a bigger one, applying only the pending schema migrations — all of that is pure
//! control flow that a host test can exercise properly.
//!
//! What the host cannot check is SQL *semantics*: [`MemSql`] records calls and replays
//! programmed rows, it does not parse SQL. Whether a query is correct is a question only
//! the device answers, which is what the probe is for.

use alloc::string::String;
use alloc::vec::Vec;

use symbian_sys as sys;

use crate::error::{Error, Result};

/// How much text a column read tries to take without allocating.
///
/// A row of a message list is a name and a line of text, both comfortably under this, so
/// the common read costs one call and no allocation. Anything longer pays a second call
/// and a `Vec` — which is still cheaper than sizing every read for the worst case.
const TEXT_INLINE: usize = 128;

/// A value going into a statement.
///
/// Borrowed rather than owned so a caller can bind a `&str` out of a struct it already has
/// without copying it first. The copy that does happen is the UTF-16 conversion, which the
/// platform requires: `RSqlStatement::BindText` has no 8-bit overload.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Value<'a> {
    Null,
    Int(i64),
    Real(f64),
    Text(&'a str),
    Blob(&'a [u8]),
}

impl<'a> From<i64> for Value<'a> {
    fn from(v: i64) -> Self {
        Value::Int(v)
    }
}

impl<'a> From<i32> for Value<'a> {
    fn from(v: i32) -> Self {
        Value::Int(v as i64)
    }
}

impl<'a> From<&'a str> for Value<'a> {
    fn from(v: &'a str) -> Self {
        Value::Text(v)
    }
}

impl<'a> From<&'a [u8]> for Value<'a> {
    fn from(v: &'a [u8]) -> Self {
        Value::Blob(v)
    }
}

/// What a column holds, as the platform reports it.
///
/// `Int` covers both of the platform's integer types: it distinguishes `ESqlInt` from
/// `ESqlInt64` and the difference is a storage detail of the row buffer.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Type {
    Null,
    Int,
    Real,
    Text,
    Blob,
}

impl Type {
    fn from_code(code: i32) -> Result<Type> {
        match code {
            sys::SHIM_SQL_NULL => Ok(Type::Null),
            sys::SHIM_SQL_INT => Ok(Type::Int),
            sys::SHIM_SQL_REAL => Ok(Type::Real),
            sys::SHIM_SQL_TEXT => Ok(Type::Text),
            sys::SHIM_SQL_BLOB => Ok(Type::Blob),
            other => Err(Error::Platform(other)),
        }
    }
}

/// The operations everything else here is built from.
///
/// Deliberately the raw shape, one method per shim entry point, with one exception worth
/// naming: [`Sql::column_text`] and [`Sql::column_blob`] return the column's *full* length
/// and fill as much of the buffer as fits, rather than failing when it is short. That turns
/// "the buffer was too small" from an error into a number, and the retry loop that uses it
/// lives above this line where it can be tested.
pub trait Sql {
    fn open(&mut self, path: &[u16], create: bool) -> Result<i32>;
    fn close(&mut self, db: i32);
    fn delete(&mut self, path: &[u16]) -> Result<()>;
    /// Statements with no parameters and no rows. Returns the number of rows affected.
    fn exec(&mut self, db: i32, sql: &[u8]) -> Result<usize>;
    fn size(&mut self, db: i32) -> Result<u64>;
    /// The engine's message for the last failure, as UTF-16 units. Returns the full length.
    fn last_error(&mut self, db: i32, out: &mut [u16]) -> Result<usize>;

    fn prepare(&mut self, db: i32, sql: &[u8]) -> Result<i32>;
    fn finalize(&mut self, stmt: i32);
    fn reset(&mut self, stmt: i32) -> Result<()>;
    /// `true` when a row is ready, `false` when the statement is finished.
    ///
    /// **SELECT only** — see [`Stmt::step`] for what stepping anything else does.
    fn step(&mut self, stmt: i32) -> Result<bool>;
    /// Run a prepared non-SELECT statement to completion. Returns rows affected.
    fn exec_stmt(&mut self, stmt: i32) -> Result<usize>;
    fn bind(&mut self, stmt: i32, index: i32, value: Value<'_>) -> Result<()>;

    fn column_type(&mut self, stmt: i32, col: i32) -> Result<Type>;
    fn column_int(&mut self, stmt: i32, col: i32) -> Result<i64>;
    fn column_real(&mut self, stmt: i32, col: i32) -> Result<f64>;
    /// Fills what fits and returns the column's full length in UTF-16 units.
    fn column_text(&mut self, stmt: i32, col: i32, out: &mut [u16]) -> Result<usize>;
    /// Fills what fits and returns the column's full length in bytes.
    fn column_blob(&mut self, stmt: i32, col: i32, out: &mut [u8]) -> Result<usize>;
    fn column_index(&mut self, stmt: i32, name: &[u16]) -> Result<i32>;
}

/// [`Sql`] over the shim. Zero-sized: the shim owns the handle tables.
#[derive(Copy, Clone, Debug, Default)]
pub struct ShimSql;

impl Sql for ShimSql {
    fn open(&mut self, path: &[u16], create: bool) -> Result<i32> {
        let mut handle = 0i32;
        // SAFETY: `path` is valid for `path.len()` units, `handle` is a live local.
        let rc = unsafe {
            sys::shim_sql_open(
                path.as_ptr(),
                path.len() as i32,
                create as i32,
                &mut handle,
            )
        };
        Error::check(rc)?;
        Ok(handle)
    }

    fn close(&mut self, db: i32) {
        unsafe { sys::shim_sql_close(db) }
    }

    fn delete(&mut self, path: &[u16]) -> Result<()> {
        Error::check(unsafe { sys::shim_sql_delete(path.as_ptr(), path.len() as i32) })
    }

    fn exec(&mut self, db: i32, sql: &[u8]) -> Result<usize> {
        let mut changed = 0i32;
        // SAFETY: `sql` is valid for its length and only read.
        let rc =
            unsafe { sys::shim_sql_exec(db, sql.as_ptr(), sql.len() as i32, &mut changed) };
        Error::check(rc)?;
        Ok(changed.max(0) as usize)
    }

    fn size(&mut self, db: i32) -> Result<u64> {
        let mut out = 0i32;
        let rc = unsafe { sys::shim_sql_size(db, &mut out) };
        Error::check(rc)?;
        Ok(out.max(0) as u64)
    }

    fn last_error(&mut self, db: i32, out: &mut [u16]) -> Result<usize> {
        let mut len = 0i32;
        // SAFETY: `out` is valid for its length; the shim writes at most that many units
        // and reports the full length through `len`.
        let rc = unsafe {
            sys::shim_sql_last_error(db, out.as_mut_ptr(), out.len() as i32, &mut len)
        };
        // Overflow is not a failure here: the length is the answer the caller needs, and
        // the shim has already written it. Every other code still propagates.
        if rc != sys::SHIM_ERR_OVERFLOW {
            Error::check(rc)?;
        }
        Ok(len.max(0) as usize)
    }

    fn prepare(&mut self, db: i32, sql: &[u8]) -> Result<i32> {
        let mut stmt = 0i32;
        let rc =
            unsafe { sys::shim_sql_prepare(db, sql.as_ptr(), sql.len() as i32, &mut stmt) };
        Error::check(rc)?;
        Ok(stmt)
    }

    fn finalize(&mut self, stmt: i32) {
        unsafe { sys::shim_sql_finalize(stmt) }
    }

    fn reset(&mut self, stmt: i32) -> Result<()> {
        Error::check(unsafe { sys::shim_sql_reset(stmt) })
    }

    fn step(&mut self, stmt: i32) -> Result<bool> {
        let rc = unsafe { sys::shim_sql_step(stmt) };
        Error::check(rc)?;
        Ok(rc == sys::SHIM_SQL_ROW)
    }

    fn exec_stmt(&mut self, stmt: i32) -> Result<usize> {
        let mut changed = 0i32;
        let rc = unsafe { sys::shim_sql_exec_stmt(stmt, &mut changed) };
        Error::check(rc)?;
        Ok(changed.max(0) as usize)
    }

    fn bind(&mut self, stmt: i32, index: i32, value: Value<'_>) -> Result<()> {
        let rc = match value {
            Value::Null => unsafe { sys::shim_sql_bind_null(stmt, index) },
            Value::Int(v) => unsafe { sys::shim_sql_bind_int(stmt, index, v) },
            Value::Real(v) => unsafe { sys::shim_sql_bind_real(stmt, index, v) },
            Value::Text(s) => {
                // The conversion the platform forces. Collected rather than written into a
                // fixed buffer because a bound value has no length limit worth imposing
                // here — a message body is exactly the case this exists for.
                let units: Vec<u16> = s.encode_utf16().collect();
                unsafe {
                    sys::shim_sql_bind_text(stmt, index, units.as_ptr(), units.len() as i32)
                }
            }
            Value::Blob(b) => unsafe {
                sys::shim_sql_bind_blob(stmt, index, b.as_ptr(), b.len() as i32)
            },
        };
        Error::check(rc)
    }

    fn column_type(&mut self, stmt: i32, col: i32) -> Result<Type> {
        let mut out = 0i32;
        let rc = unsafe { sys::shim_sql_column_type(stmt, col, &mut out) };
        Error::check(rc)?;
        Type::from_code(out)
    }

    fn column_int(&mut self, stmt: i32, col: i32) -> Result<i64> {
        let mut out = 0i64;
        let rc = unsafe { sys::shim_sql_column_int(stmt, col, &mut out) };
        Error::check(rc)?;
        Ok(out)
    }

    fn column_real(&mut self, stmt: i32, col: i32) -> Result<f64> {
        let mut out = 0f64;
        let rc = unsafe { sys::shim_sql_column_real(stmt, col, &mut out) };
        Error::check(rc)?;
        Ok(out)
    }

    fn column_text(&mut self, stmt: i32, col: i32, out: &mut [u16]) -> Result<usize> {
        let mut len = 0i32;
        let rc = unsafe {
            sys::shim_sql_column_text(stmt, col, out.as_mut_ptr(), out.len() as i32, &mut len)
        };
        // As with last_error: a short buffer is a length, not a failure. The caller decides
        // whether to come back with a bigger one.
        if rc != sys::SHIM_ERR_OVERFLOW {
            Error::check(rc)?;
        }
        Ok(len.max(0) as usize)
    }

    fn column_blob(&mut self, stmt: i32, col: i32, out: &mut [u8]) -> Result<usize> {
        let mut len = 0i32;
        let rc = unsafe {
            sys::shim_sql_column_blob(stmt, col, out.as_mut_ptr(), out.len() as i32, &mut len)
        };
        if rc != sys::SHIM_ERR_OVERFLOW {
            Error::check(rc)?;
        }
        Ok(len.max(0) as usize)
    }

    fn column_index(&mut self, stmt: i32, name: &[u16]) -> Result<i32> {
        let mut out = -1i32;
        let rc =
            unsafe { sys::shim_sql_column_index(stmt, name.as_ptr(), name.len() as i32, &mut out) };
        Error::check(rc)?;
        Ok(out)
    }
}

// ----------------------------------------------------------------------- database --

/// An open database that closes itself.
///
/// Two connections at a time is all the shim has room for, so a leak is not a slow drip —
/// it is the third open failing with [`Error::InUse`]. `Drop` is what keeps that from
/// depending on every early return remembering, exactly as with [`crate::fs::File`].
pub struct Db<'a, S: Sql> {
    sql: &'a mut S,
    handle: i32,
}

impl<'a, S: Sql> Db<'a, S> {
    /// Open `path`, creating the database if it is not there.
    pub fn open(sql: &'a mut S, path: &crate::fs::Utf16Path) -> Result<Self> {
        let handle = sql.open(path.as_units(), true)?;
        Ok(Db { sql, handle })
    }

    /// Open `path` only if it already exists. [`Error::NotFound`] otherwise.
    ///
    /// For the caller that wants to distinguish first run from later ones itself rather
    /// than having an empty database appear underneath it.
    pub fn open_existing(sql: &'a mut S, path: &crate::fs::Utf16Path) -> Result<Self> {
        let handle = sql.open(path.as_units(), false)?;
        Ok(Db { sql, handle })
    }

    /// Run one or more statements with no parameters. Returns rows affected.
    pub fn execute(&mut self, sql: &str) -> Result<usize> {
        self.sql.exec(self.handle, sql.as_bytes())
    }

    /// Prepare `sql`, bind `params` positionally, run it, and report rows affected.
    ///
    /// **For statements that return no rows** — INSERT, UPDATE, DELETE, DDL. Parameters go
    /// to indexes 0, 1, 2… in the order given, which is the platform's numbering and not
    /// sqlite3's. For a SELECT, use [`Db::query`].
    ///
    /// # Why the split is not a matter of taste
    ///
    /// The first version of this stepped the statement instead of executing it, with a
    /// comment claiming that one code path served both shapes and that a statement which
    /// unexpectedly returned rows would merely be stepped to exhaustion. On this platform
    /// that is fatal in the other direction: stepping a statement with no row set panics
    /// inside the SQL client and closes the process. `examples/sqlprobe` proved it on the
    /// handset — its report ends at the breadcrumb `-> step` on a prepared INSERT.
    ///
    /// So there are two paths because the platform has two, and choosing the wrong one is
    /// not an error that comes back.
    pub fn execute_with(&mut self, sql: &str, params: &[Value<'_>]) -> Result<usize> {
        let mut stmt = self.prepare(sql)?;
        stmt.bind_all(params)?;
        stmt.exec()
    }

    /// Prepare a statement for repeated use.
    ///
    /// The reason SQLite is worth having: parsing the SQL once and rebinding per row is
    /// most of the difference between a fast insert loop and a slow one.
    pub fn prepare(&mut self, sql: &str) -> Result<Stmt<'_, S>> {
        // Counted here, from the SQL, because the platform offers no way to ask a prepared
        // statement how many parameters it has — and an out-of-range bind is fatal, not an
        // error. See [`Stmt::bind`].
        let params = count_placeholders(sql);
        let handle = self.sql.prepare(self.handle, sql.as_bytes())?;
        Ok(Stmt { sql: &mut *self.sql, handle, params })
    }

    /// Run a query, calling `f` once per row.
    ///
    /// A callback rather than an iterator: a row borrows the statement it came from, and an
    /// iterator handing those out would either fight the borrow checker or copy every row
    /// into a `Vec` before the caller had asked for any of it. Returns the row count.
    pub fn query<F>(&mut self, sql: &str, params: &[Value<'_>], mut f: F) -> Result<usize>
    where
        F: FnMut(&mut Stmt<'_, S>) -> Result<()>,
    {
        let mut stmt = self.prepare(sql)?;
        stmt.bind_all(params)?;
        let mut rows = 0usize;
        while stmt.step()? {
            f(&mut stmt)?;
            rows += 1;
        }
        Ok(rows)
    }

    /// The first column of the first row, as an integer. `None` when the query returned no
    /// rows — the shape of every `SELECT COUNT(*)` and `SELECT max(id)`.
    pub fn query_int(&mut self, sql: &str, params: &[Value<'_>]) -> Result<Option<i64>> {
        let mut out = None;
        self.query(sql, params, |row| {
            if out.is_none() {
                out = Some(row.get_int(0)?);
            }
            Ok(())
        })?;
        Ok(out)
    }

    /// The database file's size in bytes.
    pub fn size(&mut self) -> Result<u64> {
        self.sql.size(self.handle)
    }

    /// The engine's message for the last failure — `no such column: nmae`, where the error
    /// code only said `-311`. Worth logging whenever a statement is rejected.
    pub fn last_error(&mut self) -> String {
        let mut buf = [0u16; 256];
        let n = match self.sql.last_error(self.handle, &mut buf) {
            Ok(n) => n.min(buf.len()),
            Err(_) => return String::new(),
        };
        char::decode_utf16(buf[..n].iter().copied())
            .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect()
    }

    /// Run `f` inside a transaction, committing if it succeeds and rolling back if it does
    /// not.
    ///
    /// This is the other half of why SQLite beats a file. A batch of inserts inside one
    /// transaction is a single commit rather than one per row, and a failure halfway
    /// through leaves the store as it was instead of half updated.
    pub fn transaction<F, T>(&mut self, mut f: F) -> Result<T>
    where
        F: FnMut(&mut Self) -> Result<T>,
    {
        self.execute("BEGIN")?;
        match f(self) {
            Ok(v) => {
                self.execute("COMMIT")?;
                Ok(v)
            }
            Err(e) => {
                // The rollback's own result is dropped deliberately: the caller's error is
                // the one that explains what happened, and replacing it with a failure
                // from the cleanup would hide it.
                let _ = self.execute("ROLLBACK");
                Err(e)
            }
        }
    }
}

impl<S: Sql> Drop for Db<'_, S> {
    fn drop(&mut self) {
        self.sql.close(self.handle);
    }
}

// ---------------------------------------------------------------------- statement --

/// A prepared statement, which is also the row cursor.
///
/// One type rather than a separate `Row`, because that is what the platform has: the
/// column getters read out of the statement's own row buffer, and a row that outlived the
/// statement would be reading freed server-side state. sqlite3's C API draws the same
/// line.
pub struct Stmt<'a, S: Sql> {
    sql: &'a mut S,
    handle: i32,
    /// How many `?` the SQL had, or `None` when it could not be counted — a statement with
    /// named parameters, where the count is not the number of markers. `None` means the
    /// guard in [`Stmt::bind`] cannot run, not that there is nothing to guard.
    params: Option<usize>,
}

impl<S: Sql> Stmt<'_, S> {
    /// Bind one parameter. Indexes are zero-based.
    ///
    /// # Why this checks the index itself
    ///
    /// An out-of-range parameter index does not come back as an error on this platform: the
    /// SQL client asserts, and an assertion is a *panic*, which takes the process down. A
    /// TRAP does not catch it — a panic is not a Leave — so there is no barrier anywhere
    /// below this line that can turn it into a return value. `examples/sqlprobe` found that
    /// the hard way, by binding index 2 of a two-parameter statement on purpose and killing
    /// the app.
    ///
    /// So the check happens here, from a `?` count taken off the SQL at prepare time. It
    /// cannot be exhaustive — named parameters make the marker count meaningless, and then
    /// the guard steps aside rather than refusing something legal — but it catches the
    /// mistake that is actually made, which is a parameter list that does not match the
    /// statement.
    pub fn bind(&mut self, index: i32, value: Value<'_>) -> Result<()> {
        if index < 0 {
            return Err(Error::Argument);
        }
        if let Some(n) = self.params {
            if index as usize >= n {
                // Deliberately an error and not a panic of our own: a caller that got the
                // count wrong deserves a diagnosis, and on a phone the diagnosis has to
                // survive long enough to be written down.
                return Err(Error::Argument);
            }
        }
        self.sql.bind(self.handle, index, value)
    }

    /// Bind a whole parameter list to indexes 0, 1, 2… in order.
    ///
    /// Refuses, without binding anything, when the list is longer than the statement's `?`
    /// count — see [`Stmt::bind`] for why that is worth a check rather than a crash. A list
    /// *shorter* than the count is allowed: the parameters left alone stay NULL, which is
    /// occasionally what a caller means.
    pub fn bind_all(&mut self, params: &[Value<'_>]) -> Result<()> {
        if let Some(n) = self.params {
            if params.len() > n {
                return Err(Error::Argument);
            }
        }
        for (i, v) in params.iter().enumerate() {
            self.bind(i as i32, *v)?;
        }
        Ok(())
    }

    /// Advance to the next row. `false` when there are none left.
    ///
    /// # SELECT only
    ///
    /// Stepping a statement that produces no row set — an INSERT, an UPDATE, a CREATE —
    /// panics inside the SQL client, which closes the application. It is not an error that
    /// comes back and no TRAP catches it, because a panic is not a Leave. Use [`Stmt::exec`]
    /// for those.
    ///
    /// There is no guard for this one, unlike the index check in [`Stmt::bind`]: telling a
    /// SELECT from a non-SELECT means parsing SQL, and a guess that refused a legal `WITH …
    /// SELECT` or a `PRAGMA` would be worse than the documentation.
    pub fn step(&mut self) -> Result<bool> {
        self.sql.step(self.handle)
    }

    /// Run a non-SELECT statement to completion. Returns rows affected.
    ///
    /// The counterpart of [`Stmt::step`], and the only safe way to run a bound INSERT,
    /// UPDATE or DELETE.
    pub fn exec(&mut self) -> Result<usize> {
        self.sql.exec_stmt(self.handle)
    }

    /// Rewind so the statement can run again. Bindings survive, which is what makes
    /// reusing a prepared statement worthwhile — rebind only what changed.
    pub fn reset(&mut self) -> Result<()> {
        self.sql.reset(self.handle)
    }

    /// Bind a fresh parameter list and run a non-SELECT statement. Rows affected.
    ///
    /// The insert-loop body: reset, rebind, exec, without reparsing the SQL. For a SELECT
    /// being re-run, reset and bind, then step — see [`Stmt::step`] for why the two cannot
    /// share one path.
    pub fn run(&mut self, params: &[Value<'_>]) -> Result<usize> {
        self.reset()?;
        self.bind_all(params)?;
        self.exec()
    }

    pub fn column_type(&mut self, col: i32) -> Result<Type> {
        self.sql.column_type(self.handle, col)
    }

    /// The position of a named column, for a SELECT whose shape is not fixed at the call
    /// site.
    pub fn column_index(&mut self, name: &str) -> Result<i32> {
        let units: Vec<u16> = name.encode_utf16().collect();
        self.sql.column_index(self.handle, &units)
    }

    pub fn get_int(&mut self, col: i32) -> Result<i64> {
        self.sql.column_int(self.handle, col)
    }

    pub fn get_real(&mut self, col: i32) -> Result<f64> {
        self.sql.column_real(self.handle, col)
    }

    /// A text column as a `String`.
    ///
    /// Two passes when it has to be: the first read uses a stack buffer, and only a column
    /// longer than [`TEXT_INLINE`] costs a heap allocation and a second call. The loop is
    /// the part worth having tested — a version that trusted the first read would silently
    /// truncate every long message body, and the result parses fine.
    pub fn get_text(&mut self, col: i32) -> Result<String> {
        let mut inline = [0u16; TEXT_INLINE];
        let len = self.sql.column_text(self.handle, col, &mut inline)?;
        if len <= TEXT_INLINE {
            return Ok(decode(&inline[..len]));
        }
        let mut heap = alloc::vec![0u16; len];
        let got = self.sql.column_text(self.handle, col, &mut heap)?;
        // Trust the second read's own count rather than the first one's: it is the
        // authority on what it wrote, and a shrinking column between the two calls would
        // otherwise leave a tail of zeros that looks like data.
        Ok(decode(&heap[..got.min(heap.len())]))
    }

    /// A blob column as bytes. Same two-pass shape as [`Stmt::get_text`].
    pub fn get_blob(&mut self, col: i32) -> Result<Vec<u8>> {
        let mut probe = [0u8; TEXT_INLINE];
        let len = self.sql.column_blob(self.handle, col, &mut probe)?;
        if len <= probe.len() {
            return Ok(probe[..len].to_vec());
        }
        let mut heap = alloc::vec![0u8; len];
        let got = self.sql.column_blob(self.handle, col, &mut heap)?;
        heap.truncate(got.min(len));
        Ok(heap)
    }

    /// Whether the column is NULL — the one question the typed getters cannot answer,
    /// since a NULL integer column reads as 0 and so does a stored 0.
    pub fn is_null(&mut self, col: i32) -> Result<bool> {
        Ok(self.column_type(col)? == Type::Null)
    }
}

impl<S: Sql> Drop for Stmt<'_, S> {
    fn drop(&mut self) {
        self.sql.finalize(self.handle);
    }
}

/// How many `?` markers the statement has, or `None` when the answer would be misleading.
///
/// String literals are skipped, because `SELECT '?'` has no parameter and a naive count says
/// it has one — which would make [`Stmt::bind`] refuse a legal bind. A named parameter
/// (`:name`, `@name`, `$name`) gives up entirely and returns `None`: with names, the marker
/// count is not the parameter count, since one name may appear twice and still be one
/// parameter.
///
/// The mangled cases are deliberately resolved towards `None` rather than towards a number.
/// A wrong count here refuses a correct program; no count only means the platform's own
/// behaviour is what the caller gets, which is where it started.
fn count_placeholders(sql: &str) -> Option<usize> {
    let b = sql.as_bytes();
    let mut i = 0usize;
    let mut n = 0usize;
    while i < b.len() {
        match b[i] {
            // Both quote styles, and the SQL doubling convention for an embedded quote:
            // inside 'it''s' the pair is not a close followed by an open.
            q @ (b'\'' | b'"') => {
                i += 1;
                while i < b.len() {
                    if b[i] == q {
                        if i + 1 < b.len() && b[i + 1] == q {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                i += 1;
            }
            b'-' if i + 1 < b.len() && b[i + 1] == b'-' => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'?' => {
                n += 1;
                i += 1;
            }
            b':' | b'@' | b'$' => {
                let next = b.get(i + 1).copied().unwrap_or(0);
                if next.is_ascii_alphanumeric() || next == b'_' {
                    return None;
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    Some(n)
}

fn decode(units: &[u16]) -> String {
    char::decode_utf16(units.iter().copied())
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

// ---------------------------------------------------------------------- migration --

/// The table [`migrate`] keeps its version in.
const VERSION_TABLE: &str = "shim_schema_version";

/// Apply the schema steps that have not run yet, and return the version now in force.
///
/// `steps[0]` takes the database from version 0 to 1, `steps[1]` from 1 to 2, and so on, so
/// appending a step is how the schema changes and no step is ever edited after it has
/// shipped.
///
/// # Why a table and not `PRAGMA user_version`
///
/// SQLite has a field for exactly this, and it is reached through a PRAGMA. Symbian SQL is
/// the engine behind a server that filters what it will run, and nothing in the public SDK
/// promises that PRAGMA gets through — `examples/sqlprobe` asks the handset directly. A
/// one-row table costs a page and works whatever the answer turns out to be.
///
/// Every step, and the version bump, run inside one transaction: a migration interrupted
/// halfway is the worst outcome available, because the schema then matches no version the
/// code knows about.
pub fn migrate<S: Sql>(db: &mut Db<'_, S>, steps: &[&str]) -> Result<u32> {
    db.execute(&alloc::format!(
        "CREATE TABLE IF NOT EXISTS {VERSION_TABLE} (version INTEGER NOT NULL)"
    ))?;

    let current = db
        .query_int(&alloc::format!("SELECT version FROM {VERSION_TABLE}"), &[])?
        .unwrap_or(-1);

    // -1 means the table was just created and holds no row yet, which is distinct from a
    // stored 0: the first needs an INSERT, the second an UPDATE.
    let first_run = current < 0;
    let mut version = current.max(0) as usize;

    if version >= steps.len() {
        // Nothing to do — including the case of a database written by a *newer* build than
        // this one. Reporting the version it found rather than refusing is deliberate: an
        // older binary that cannot understand the schema will fail on the query that needs
        // the new column, which names the actual problem.
        if first_run {
            db.execute_with(
                &alloc::format!("INSERT INTO {VERSION_TABLE} (version) VALUES (?)"),
                &[Value::Int(version as i64)],
            )?;
        }
        return Ok(version as u32);
    }

    db.transaction(|db| {
        for step in &steps[version..] {
            db.execute(step)?;
            version += 1;
        }
        let sql = if first_run {
            alloc::format!("INSERT INTO {VERSION_TABLE} (version) VALUES (?)")
        } else {
            alloc::format!("UPDATE {VERSION_TABLE} SET version = ?")
        };
        db.execute_with(&sql, &[Value::Int(version as i64)])?;
        Ok(())
    })?;

    Ok(version as u32)
}

// ------------------------------------------------------------------------ testing --

/// One owned cell, for [`MemSql`]'s programmed rows.
#[derive(Clone, Debug, PartialEq)]
pub enum Cell {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl Cell {
    fn kind(&self) -> Type {
        match self {
            Cell::Null => Type::Null,
            Cell::Int(_) => Type::Int,
            Cell::Real(_) => Type::Real,
            Cell::Text(_) => Type::Text,
            Cell::Blob(_) => Type::Blob,
        }
    }
}

/// What [`MemSql`] recorded. The assertion surface: tests check that parameters were bound
/// to the indexes and in the order the wrapper claims.
#[derive(Clone, Debug, PartialEq)]
pub enum Call {
    Open(String, bool),
    Close,
    Exec(String),
    Prepare(String),
    Bind(i32, Cell),
    Step,
    /// `RSqlStatement::Exec` — the non-SELECT path. Distinct from [`Call::Step`] in the
    /// recording because the device made the distinction load-bearing: stepping a
    /// non-SELECT closes the process, so a test asserting "this ran as an exec, not as a
    /// step" is asserting something that actually matters.
    ExecStmt,
    Reset,
    Finalize,
}

/// An in-memory [`Sql`] that records calls and replays programmed rows.
///
/// Public, and not behind `#[cfg(test)]`, for the same reason as [`crate::fs::MemFs`]: the
/// crates above this one need it too, and it costs a device build nothing because nothing
/// references it and `--gc-sections` sweeps it.
///
/// **It does not parse SQL.** It cannot tell you whether a query is right; it tells you
/// whether this module drove the platform correctly. Rows come from [`MemSql::rows`], set
/// before the query that should return them.
pub struct MemSql {
    pub calls: Vec<Call>,
    /// Rows the next query returns, outermost is rows and innermost is columns.
    pub rows: Vec<Vec<Cell>>,
    /// Which row the cursor is on: `None` before the first step.
    pos: Option<usize>,
    next_handle: i32,
    open_stmts: Vec<i32>,
    open_dbs: Vec<i32>,
}

impl MemSql {
    pub fn new() -> Self {
        MemSql {
            calls: Vec::new(),
            rows: Vec::new(),
            pos: None,
            next_handle: 1,
            open_stmts: Vec::new(),
            open_dbs: Vec::new(),
        }
    }

    /// Program the rows the next query yields.
    pub fn set_rows(&mut self, rows: Vec<Vec<Cell>>) {
        self.rows = rows;
        self.pos = None;
    }

    /// Every SQL string that was executed or prepared, in order. The readable form of
    /// [`MemSql::calls`] for a test that only cares about the statements.
    pub fn statements(&self) -> Vec<&str> {
        self.calls
            .iter()
            .filter_map(|c| match c {
                Call::Exec(s) | Call::Prepare(s) => Some(s.as_str()),
                _ => None,
            })
            .collect()
    }

    /// The parameters bound since the last prepare, in the order the wrapper sent them.
    pub fn binds(&self) -> Vec<(i32, Cell)> {
        self.calls
            .iter()
            .filter_map(|c| match c {
                Call::Bind(i, v) => Some((*i, v.clone())),
                _ => None,
            })
            .collect()
    }

    fn current(&self) -> Option<&Vec<Cell>> {
        self.pos.and_then(|p| self.rows.get(p))
    }

    fn cell(&self, col: i32) -> Result<&Cell> {
        self.current()
            .and_then(|r| r.get(col as usize))
            .ok_or(Error::Argument)
    }

    fn handle(&mut self) -> i32 {
        let h = self.next_handle;
        self.next_handle += 1;
        h
    }
}

impl Default for MemSql {
    fn default() -> Self {
        Self::new()
    }
}

impl Sql for MemSql {
    fn open(&mut self, path: &[u16], create: bool) -> Result<i32> {
        self.calls.push(Call::Open(decode(path), create));
        let h = self.handle();
        self.open_dbs.push(h);
        Ok(h)
    }

    fn close(&mut self, db: i32) {
        self.calls.push(Call::Close);
        self.open_dbs.retain(|&h| h != db);
    }

    fn delete(&mut self, _path: &[u16]) -> Result<()> {
        Ok(())
    }

    fn exec(&mut self, _db: i32, sql: &[u8]) -> Result<usize> {
        self.calls
            .push(Call::Exec(String::from_utf8_lossy(sql).into_owned()));
        Ok(0)
    }

    fn size(&mut self, _db: i32) -> Result<u64> {
        Ok(0)
    }

    fn last_error(&mut self, _db: i32, _out: &mut [u16]) -> Result<usize> {
        Ok(0)
    }

    fn prepare(&mut self, _db: i32, sql: &[u8]) -> Result<i32> {
        self.calls
            .push(Call::Prepare(String::from_utf8_lossy(sql).into_owned()));
        self.pos = None;
        let h = self.handle();
        self.open_stmts.push(h);
        Ok(h)
    }

    fn finalize(&mut self, stmt: i32) {
        self.calls.push(Call::Finalize);
        self.open_stmts.retain(|&h| h != stmt);
    }

    fn reset(&mut self, _stmt: i32) -> Result<()> {
        self.calls.push(Call::Reset);
        self.pos = None;
        Ok(())
    }

    fn step(&mut self, _stmt: i32) -> Result<bool> {
        self.calls.push(Call::Step);
        let next = match self.pos {
            None => 0,
            Some(p) => p + 1,
        };
        if next < self.rows.len() {
            self.pos = Some(next);
            Ok(true)
        } else {
            // Park past the end so a further step stays false rather than wrapping.
            self.pos = Some(self.rows.len());
            Ok(false)
        }
    }

    fn exec_stmt(&mut self, _stmt: i32) -> Result<usize> {
        self.calls.push(Call::ExecStmt);
        // The programmed rows belong to a query; an exec reports rows *affected*, which the
        // fake has no way to know. One is the honest answer for the INSERT this stands in
        // for, and a test that cares asserts on the recorded call rather than the count.
        Ok(1)
    }

    fn bind(&mut self, _stmt: i32, index: i32, value: Value<'_>) -> Result<()> {
        let cell = match value {
            Value::Null => Cell::Null,
            Value::Int(v) => Cell::Int(v),
            Value::Real(v) => Cell::Real(v),
            Value::Text(s) => Cell::Text(String::from(s)),
            Value::Blob(b) => Cell::Blob(b.to_vec()),
        };
        self.calls.push(Call::Bind(index, cell));
        Ok(())
    }

    fn column_type(&mut self, _stmt: i32, col: i32) -> Result<Type> {
        Ok(self.cell(col)?.kind())
    }

    fn column_int(&mut self, _stmt: i32, col: i32) -> Result<i64> {
        match self.cell(col)? {
            Cell::Int(v) => Ok(*v),
            Cell::Real(v) => Ok(*v as i64),
            // The platform coerces rather than failing, and a fake that refused would
            // make tests pass that the device would not.
            _ => Ok(0),
        }
    }

    fn column_real(&mut self, _stmt: i32, col: i32) -> Result<f64> {
        match self.cell(col)? {
            Cell::Real(v) => Ok(*v),
            Cell::Int(v) => Ok(*v as f64),
            _ => Ok(0.0),
        }
    }

    fn column_text(&mut self, _stmt: i32, col: i32, out: &mut [u16]) -> Result<usize> {
        let units: Vec<u16> = match self.cell(col)? {
            Cell::Text(s) => s.encode_utf16().collect(),
            Cell::Null => Vec::new(),
            _ => return Ok(0),
        };
        let n = units.len().min(out.len());
        out[..n].copy_from_slice(&units[..n]);
        // The full length, not what fitted: that is the trait's contract and the whole
        // reason the two-pass read above can work.
        Ok(units.len())
    }

    fn column_blob(&mut self, _stmt: i32, col: i32, out: &mut [u8]) -> Result<usize> {
        let bytes: Vec<u8> = match self.cell(col)? {
            Cell::Blob(b) => b.clone(),
            Cell::Null => Vec::new(),
            _ => return Ok(0),
        };
        let n = bytes.len().min(out.len());
        out[..n].copy_from_slice(&bytes[..n]);
        Ok(bytes.len())
    }

    fn column_index(&mut self, _stmt: i32, _name: &[u16]) -> Result<i32> {
        Err(Error::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::Utf16Path;
    use alloc::vec;

    fn path() -> Utf16Path {
        Utf16Path::new("C:\\private\\E1234569\\store.db").unwrap()
    }

    #[test]
    fn parameters_are_bound_from_zero_in_order() {
        // The whole point of this test: Symbian SQL numbers parameters from 0, sqlite3's
        // own C API from 1. Binding the first value at 1 leaves the first `?` NULL and the
        // statement still succeeds, which is the kind of bug that surfaces as missing data
        // a week later.
        let mut sql = MemSql::new();
        let mut db = Db::open(&mut sql, &path()).unwrap();
        db.execute_with(
            "INSERT INTO msg (chat, body, ts) VALUES (?, ?, ?)",
            &[Value::Int(7), Value::Text("hello"), Value::Int(1_700_000_000)],
        )
        .unwrap();
        drop(db);

        assert_eq!(
            sql.binds(),
            vec![
                (0, Cell::Int(7)),
                (1, Cell::Text(String::from("hello"))),
                (2, Cell::Int(1_700_000_000)),
            ]
        );
    }

    #[test]
    fn a_query_visits_every_row_exactly_once() {
        let mut sql = MemSql::new();
        sql.set_rows(vec![
            vec![Cell::Int(1), Cell::Text(String::from("a"))],
            vec![Cell::Int(2), Cell::Text(String::from("b"))],
            vec![Cell::Int(3), Cell::Text(String::from("c"))],
        ]);
        let mut db = Db::open(&mut sql, &path()).unwrap();

        let mut seen = Vec::new();
        let n = db
            .query("SELECT id, name FROM chat", &[], |row| {
                seen.push((row.get_int(0)?, row.get_text(1)?));
                Ok(())
            })
            .unwrap();

        assert_eq!(n, 3);
        assert_eq!(
            seen,
            vec![
                (1, String::from("a")),
                (2, String::from("b")),
                (3, String::from("c"))
            ]
        );
    }

    #[test]
    fn a_text_column_longer_than_the_stack_buffer_survives_whole() {
        // The two-pass read. A version that trusted the first buffer would truncate at
        // exactly TEXT_INLINE units and produce a message body that looks fine.
        let long = "x".repeat(TEXT_INLINE * 3 + 7);
        let mut sql = MemSql::new();
        sql.set_rows(vec![vec![Cell::Text(long.clone())]]);
        let mut db = Db::open(&mut sql, &path()).unwrap();

        let mut got = String::new();
        db.query("SELECT body FROM msg", &[], |row| {
            got = row.get_text(0)?;
            Ok(())
        })
        .unwrap();

        assert_eq!(got.len(), long.len());
        assert_eq!(got, long);
    }

    #[test]
    fn a_text_column_exactly_the_buffer_length_takes_one_pass() {
        // The boundary. `len <= TEXT_INLINE` rather than `<`, or a value of exactly the
        // buffer size would allocate needlessly — and, worse, a `<` written the other way
        // round would read past the end of the inline array.
        let exact = "y".repeat(TEXT_INLINE);
        let mut sql = MemSql::new();
        sql.set_rows(vec![vec![Cell::Text(exact.clone())]]);
        let mut db = Db::open(&mut sql, &path()).unwrap();

        let mut got = String::new();
        db.query("SELECT body FROM msg", &[], |row| {
            got = row.get_text(0)?;
            Ok(())
        })
        .unwrap();
        assert_eq!(got, exact);
    }

    #[test]
    fn non_ascii_text_round_trips_through_utf16() {
        // Bind converts UTF-8 to UTF-16 and the column read converts back. A cast instead
        // of a conversion would work for ASCII and mangle everything else, which on a
        // Brazilian phone means every second message.
        let mut sql = MemSql::new();
        sql.set_rows(vec![vec![Cell::Text(String::from("ação — não é 日本"))]]);
        let mut db = Db::open(&mut sql, &path()).unwrap();

        db.execute_with("INSERT INTO t VALUES (?)", &[Value::Text("ação — não")])
            .unwrap();

        let mut got = String::new();
        db.query("SELECT v FROM t", &[], |row| {
            got = row.get_text(0)?;
            Ok(())
        })
        .unwrap();
        drop(db);

        assert_eq!(got, "ação — não é 日本");
        assert_eq!(sql.binds(), vec![(0, Cell::Text(String::from("ação — não")))]);
    }

    #[test]
    fn a_blob_longer_than_the_probe_buffer_survives_whole() {
        let blob: Vec<u8> = (0..(TEXT_INLINE * 2 + 5)).map(|i| (i % 251) as u8).collect();
        let mut sql = MemSql::new();
        sql.set_rows(vec![vec![Cell::Blob(blob.clone())]]);
        let mut db = Db::open(&mut sql, &path()).unwrap();

        let mut got = Vec::new();
        db.query("SELECT data FROM blobs", &[], |row| {
            got = row.get_blob(0)?;
            Ok(())
        })
        .unwrap();
        assert_eq!(got, blob);
    }

    #[test]
    fn statements_and_databases_are_released_on_drop() {
        // The shim has two database slots and eight statement slots. A leak is the third
        // open failing, not a slow drip, so Drop has to be what releases them.
        let mut sql = MemSql::new();
        for _ in 0..20 {
            let mut db = Db::open(&mut sql, &path()).unwrap();
            let _stmt = db.prepare("SELECT 1").unwrap();
        }
        assert!(sql.open_stmts.is_empty(), "statements were not finalised");
        assert!(sql.open_dbs.is_empty(), "databases were not closed");
    }

    #[test]
    fn a_prepared_statement_resets_before_rebinding() {
        // The insert loop. Without the reset the second run binds into a statement that is
        // already past its row, and the platform answers KSqlErrStmtExpired.
        let mut sql = MemSql::new();
        let mut db = Db::open(&mut sql, &path()).unwrap();
        {
            let mut stmt = db.prepare("INSERT INTO t VALUES (?)").unwrap();
            for i in 0..3i64 {
                stmt.run(&[Value::Int(i)]).unwrap();
            }
        }
        drop(db);

        // One prepare for three runs is the whole point of preparing.
        assert_eq!(
            sql.calls.iter().filter(|c| matches!(c, Call::Prepare(_))).count(),
            1
        );
        assert_eq!(sql.calls.iter().filter(|c| **c == Call::Reset).count(), 3);
        assert_eq!(sql.binds(), vec![(0, Cell::Int(0)), (0, Cell::Int(1)), (0, Cell::Int(2))]);
    }

    #[test]
    fn a_transaction_commits_on_success_and_rolls_back_on_failure() {
        let mut sql = MemSql::new();
        {
            let mut db = Db::open(&mut sql, &path()).unwrap();
            db.transaction(|db| db.execute("INSERT INTO t VALUES (1)").map(|_| ()))
                .unwrap();
        }
        let s = sql.statements();
        assert!(s.contains(&"BEGIN") && s.contains(&"COMMIT"));
        assert!(!s.contains(&"ROLLBACK"));

        let mut sql = MemSql::new();
        {
            let mut db = Db::open(&mut sql, &path()).unwrap();
            let e = db
                .transaction(|_db| Err::<(), _>(Error::Platform(-311)))
                .unwrap_err();
            // The caller's error survives, not the rollback's. Replacing it would hide
            // the only description of what actually went wrong.
            assert_eq!(e, Error::Platform(-311));
        }
        let s = sql.statements();
        assert!(s.contains(&"ROLLBACK"));
        assert!(!s.contains(&"COMMIT"));
    }

    #[test]
    fn migrate_applies_every_step_on_a_fresh_database() {
        let mut sql = MemSql::new();
        // No row in the version table: the first-run path, which must INSERT rather than
        // UPDATE — an UPDATE against an empty table succeeds and changes nothing, so the
        // next run would replay every step.
        let mut db = Db::open(&mut sql, &path()).unwrap();
        let v = migrate(
            &mut db,
            &["CREATE TABLE chat (id INTEGER PRIMARY KEY)", "ALTER TABLE chat ADD title TEXT"],
        )
        .unwrap();
        drop(db);

        assert_eq!(v, 2);
        let s = sql.statements();
        assert!(s.iter().any(|q| q.contains("CREATE TABLE chat")));
        assert!(s.iter().any(|q| q.contains("ALTER TABLE chat")));
        assert!(
            s.iter().any(|q| q.starts_with("INSERT INTO shim_schema_version")),
            "a fresh database must insert its version row, not update a row that is not there"
        );
    }

    #[test]
    fn migrate_applies_only_the_pending_steps() {
        let mut sql = MemSql::new();
        // Stored version 1: step 0 has run, step 1 has not.
        sql.set_rows(vec![vec![Cell::Int(1)]]);
        let mut db = Db::open(&mut sql, &path()).unwrap();
        let v = migrate(&mut db, &["step zero", "step one"]).unwrap();
        drop(db);

        assert_eq!(v, 2);
        let s = sql.statements();
        assert!(!s.contains(&"step zero"), "an applied step must not run twice");
        assert!(s.contains(&"step one"));
        assert!(s.iter().any(|q| q.starts_with("UPDATE shim_schema_version")));
    }

    #[test]
    fn migrate_is_a_no_op_when_the_schema_is_current() {
        let mut sql = MemSql::new();
        sql.set_rows(vec![vec![Cell::Int(2)]]);
        let mut db = Db::open(&mut sql, &path()).unwrap();
        let v = migrate(&mut db, &["step zero", "step one"]).unwrap();
        drop(db);

        assert_eq!(v, 2);
        let s = sql.statements();
        assert!(!s.contains(&"step zero") && !s.contains(&"step one"));
        assert!(!s.contains(&"BEGIN"), "nothing to do must not open a transaction");
    }

    #[test]
    fn a_database_from_a_newer_build_reports_its_own_version() {
        // Downgrade. The old binary cannot know what version 5 means, and refusing here
        // would replace a specific failure ("no such column") with a vague one.
        let mut sql = MemSql::new();
        sql.set_rows(vec![vec![Cell::Int(5)]]);
        let mut db = Db::open(&mut sql, &path()).unwrap();
        assert_eq!(migrate(&mut db, &["a", "b"]).unwrap(), 5);
    }

    #[test]
    fn query_int_returns_none_for_an_empty_result() {
        // SELECT COUNT(*) always has a row; SELECT max(id) FROM an empty table has one
        // holding NULL; a WHERE that matches nothing has none at all. The third is the
        // case a caller must not read as zero.
        let mut sql = MemSql::new();
        {
            let mut db = Db::open(&mut sql, &path()).unwrap();
            assert_eq!(db.query_int("SELECT id FROM chat WHERE id = 9", &[]).unwrap(), None);
        }

        sql.set_rows(vec![vec![Cell::Int(42)]]);
        let mut db = Db::open(&mut sql, &path()).unwrap();
        assert_eq!(db.query_int("SELECT count(*) FROM chat", &[]).unwrap(), Some(42));
    }

    #[test]
    fn a_non_select_runs_through_exec_and_is_never_stepped() {
        // The device lesson as a regression guard. Stepping a statement with no row set
        // panics inside the SQL client and closes the application — it is not an error that
        // comes back, so nothing but this test stands between a refactor and a phone that
        // shuts the app when a message is saved.
        let mut sql = MemSql::new();
        {
            let mut db = Db::open(&mut sql, &path()).unwrap();
            db.execute_with("INSERT INTO t (a) VALUES (?)", &[Value::Int(1)]).unwrap();
            let mut stmt = db.prepare("UPDATE t SET a = ?").unwrap();
            stmt.run(&[Value::Int(2)]).unwrap();
        }
        assert!(
            sql.calls.contains(&Call::ExecStmt),
            "a non-SELECT must go through RSqlStatement::Exec"
        );
        assert!(
            !sql.calls.contains(&Call::Step),
            "a non-SELECT must never be stepped: on the device that closes the app"
        );
    }

    #[test]
    fn a_query_is_stepped_and_never_execed() {
        // The other direction. Exec on a SELECT is the mirror mistake, and the platform's
        // answer to it is not documented anywhere we can rely on.
        let mut sql = MemSql::new();
        sql.set_rows(vec![vec![Cell::Int(1)]]);
        {
            let mut db = Db::open(&mut sql, &path()).unwrap();
            db.query("SELECT a FROM t", &[], |_row| Ok(())).unwrap();
        }
        assert!(sql.calls.contains(&Call::Step));
        assert!(!sql.calls.contains(&Call::ExecStmt));
    }

    #[test]
    fn placeholders_are_counted_outside_literals_and_comments() {
        assert_eq!(count_placeholders("INSERT INTO t VALUES (?, ?, ?)"), Some(3));
        assert_eq!(count_placeholders("SELECT 1"), Some(0));

        // A `?` inside a literal is text, not a parameter. Counting it would make bind()
        // refuse a legal bind, which is worse than not guarding at all.
        assert_eq!(count_placeholders("SELECT '?' , ?"), Some(1));
        assert_eq!(count_placeholders("SELECT \"a?b\" FROM t WHERE x = ?"), Some(1));
        // The SQL doubling convention: 'it''s' is one literal, not two with a ? between.
        assert_eq!(count_placeholders("SELECT 'it''s ?' , ?"), Some(1));
        assert_eq!(count_placeholders("SELECT ? -- and ? in a comment\n"), Some(1));
    }

    #[test]
    fn named_parameters_disable_the_guard_rather_than_misinform_it() {
        // With names, the marker count is not the parameter count — one name may appear
        // twice and still be one parameter. None means "cannot say", and bind() then lets
        // the platform decide, which is where it started.
        assert_eq!(count_placeholders("SELECT * FROM t WHERE a = :id AND b = :id"), None);
        assert_eq!(count_placeholders("SELECT @x"), None);
        assert_eq!(count_placeholders("SELECT $1"), None);
        // A bare colon is not a named parameter: `a::b` and `t:` appear in ordinary SQL.
        assert_eq!(count_placeholders("SELECT a, ? FROM t WHERE x = ':'"), Some(1));
    }

    #[test]
    fn binding_past_the_last_parameter_is_an_error_not_a_crash() {
        // The device lesson, held in place by a test. An out-of-range index does not come
        // back as an error from Symbian SQL: the client asserts and the process dies. So the
        // refusal has to happen here, above the FFI, or there is no refusal at all.
        let mut sql = MemSql::new();
        let mut db = Db::open(&mut sql, &path()).unwrap();
        let mut stmt = db.prepare("INSERT INTO msg (chat, body) VALUES (?, ?)").unwrap();

        assert!(stmt.bind(0, Value::Int(1)).is_ok());
        assert!(stmt.bind(1, Value::Int(2)).is_ok());
        assert_eq!(stmt.bind(2, Value::Int(3)).unwrap_err(), Error::Argument);
        assert_eq!(stmt.bind(-1, Value::Int(3)).unwrap_err(), Error::Argument);
    }

    #[test]
    fn a_too_long_parameter_list_is_refused_before_anything_is_bound() {
        // All-or-nothing on purpose: half a bound statement that then errors would leave the
        // caller holding a statement in a state it did not ask for, and the obvious retry
        // would run it with stale parameters.
        let mut sql = MemSql::new();
        {
            let mut db = Db::open(&mut sql, &path()).unwrap();
            let e = db
                .execute_with("INSERT INTO t (a) VALUES (?)", &[Value::Int(1), Value::Int(2)])
                .unwrap_err();
            assert_eq!(e, Error::Argument);
        }
        assert!(sql.binds().is_empty(), "nothing may be bound when the list is refused");
    }

    #[test]
    fn a_shorter_parameter_list_is_allowed() {
        // The parameters left alone stay NULL, which is occasionally what a caller means —
        // and refusing it would be this guard inventing a rule the platform does not have.
        let mut sql = MemSql::new();
        let mut db = Db::open(&mut sql, &path()).unwrap();
        let mut stmt = db.prepare("INSERT INTO t (a, b) VALUES (?, ?)").unwrap();
        assert!(stmt.bind_all(&[Value::Int(1)]).is_ok());
    }

    #[test]
    fn a_named_parameter_statement_still_binds() {
        // The guard stands aside when it cannot count. If it did not, this legal bind would
        // be refused and the caller would have no way to make it work.
        let mut sql = MemSql::new();
        let mut db = Db::open(&mut sql, &path()).unwrap();
        let mut stmt = db.prepare("UPDATE t SET a = :v WHERE id = :id").unwrap();
        assert!(stmt.bind(0, Value::Int(1)).is_ok());
        assert!(stmt.bind(1, Value::Int(2)).is_ok());
    }

    #[test]
    fn null_is_distinguishable_from_zero() {
        let mut sql = MemSql::new();
        sql.set_rows(vec![vec![Cell::Null, Cell::Int(0)]]);
        let mut db = Db::open(&mut sql, &path()).unwrap();
        db.query("SELECT a, b FROM t", &[], |row| {
            assert!(row.is_null(0)?);
            assert!(!row.is_null(1)?);
            // Both read as 0 through the typed getter, which is why is_null exists.
            assert_eq!(row.get_int(0)?, 0);
            assert_eq!(row.get_int(1)?, 0);
            Ok(())
        })
        .unwrap();
    }
}
