//! What was learned about a `.sis` last time, so it is not learned again.
//!
//! # The cost this exists for
//!
//! Opening the packages screen scanned every `.sis` on the handset: open it, read the header for
//! the UID, version and name, and SHA-256 the ones whose bytes decide something. Measured on an
//! E72: **390–426 ms**, about 260 of it hashing five files — and repeated in full every time the
//! screen opened, to learn exactly what it learned before.
//!
//! None of it changes between two openings. A file at the same path, the same size and the same
//! timestamp is the same file, and what it says about itself is a fact that does not expire.
//!
//! # Why the key is a path, a size *and* a time
//!
//! A path alone is wrong the moment somebody rebuilds: same name, same place, different bytes, and
//! the cache would hand back a digest for a file that no longer exists. Size catches most rebuilds
//! and not all — two builds of one version can land on the same byte count. The timestamp is what
//! makes the trio decisive, and `Fs::stat` already returns it beside the size, so it costs nothing
//! to ask for.
//!
//! This is a cache and not a record: a miss is ordinary and costs what the scan always cost. It is
//! never *wrong* to throw the whole thing away, which is what makes it safe to be a `.db` on a
//! handset where a database can be corrupted by a flat battery.

use alloc::string::String;
use alloc::vec::Vec;

use symbian::fs::Stat;
use symbian::pkg::Version;
use symbian::sql::{Db, Sql, Value};

/// Where it lives. Beside the package database, in the directory `bootd` already owns.
pub const PATH: &str = "C:\\Data\\bootd\\scan.db";

/// One remembered file.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Known {
    pub uid3: u32,
    pub version: Version,
    pub name: String,
    /// `None` when the scan never needed one — hashing is conditional, and a cache that invented a
    /// digest to look complete would be worse than one that admits it does not have it.
    pub sha256: Option<[u8; 32]>,
}

/// The identity of a file, as far as a directory listing can tell.
///
/// A tuple rather than three arguments at four call sites, because getting the order wrong between
/// `size` and `stamp` is silent — both are integers, and both usually differ.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct FileId {
    pub size: u64,
    /// The modification time, flattened to one integer so it compares in SQL. Not a date anybody
    /// reads: it exists to be *different* when the file is.
    pub stamp: i64,
}

impl FileId {
    /// From what `Fs::stat` reports.
    ///
    /// Seconds are included: a rebuild during the same minute is exactly the case a developer hits
    /// twenty times an afternoon, and the whole point is telling those two builds apart.
    pub fn from_stat(s: &Stat) -> Self {
        let stamp = ((s.year as i64) * 10_000 + (s.month as i64) * 100 + s.day as i64) * 1_000_000
            + (s.hour as i64) * 10_000
            + (s.minute as i64) * 100
            + s.second as i64;
        Self { size: s.size, stamp }
    }
}

/// Create the table if it is not there. Safe to call on every open.
pub fn ensure<S: Sql>(db: &mut Db<'_, S>) -> symbian::Result<()> {
    db.execute(
        "CREATE TABLE IF NOT EXISTS scanned (\
           path TEXT NOT NULL PRIMARY KEY, \
           size INTEGER NOT NULL, \
           stamp INTEGER NOT NULL, \
           uid3 INTEGER NOT NULL, \
           major INTEGER NOT NULL, \
           minor INTEGER NOT NULL, \
           patch INTEGER NOT NULL, \
           name TEXT NOT NULL, \
           sha BLOB)",
    )?;
    Ok(())
}

/// What is known about this file, or `None` if it was never seen — or was seen at a different size
/// or time, which is the same thing.
///
/// The size and time are part of the *query* rather than checked afterwards, so a stale row cannot
/// be returned by a caller who forgot to compare.
pub fn get<S: Sql>(db: &mut Db<'_, S>, path: &str, id: FileId) -> Option<Known> {
    let mut found: Option<Known> = None;
    let _ = db.query(
        "SELECT uid3, major, minor, patch, name, sha FROM scanned \
         WHERE path = ?1 AND size = ?2 AND stamp = ?3",
        &[Value::Text(path), Value::Int(id.size as i64), Value::Int(id.stamp)],
        |row| {
            let uid3 = row.get_int(0).unwrap_or(0) as u32;
            let version = Version::new(
                row.get_int(1).unwrap_or(0) as u16,
                row.get_int(2).unwrap_or(0) as u16,
                row.get_int(3).unwrap_or(0) as u16,
            );
            let name = row.get_text(4).unwrap_or_default();
            let sha = row.get_blob(5).ok().and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok());
            found = Some(Known { uid3, version, name, sha256: sha });
            Ok(())
        },
    );
    found
}

/// Remember what a file turned out to be.
///
/// `INSERT OR REPLACE`, because a path that was seen at a different size is the same row with a new
/// answer — keeping both would let the older one be found first.
pub fn put<S: Sql>(
    db: &mut Db<'_, S>,
    path: &str,
    id: FileId,
    k: &Known,
) -> symbian::Result<()> {
    let sha: &[u8] = k.sha256.as_ref().map(|s| &s[..]).unwrap_or(&[]);
    db.execute_with(
        "INSERT OR REPLACE INTO scanned \
         (path, size, stamp, uid3, major, minor, patch, name, sha) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        &[
            Value::Text(path),
            Value::Int(id.size as i64),
            Value::Int(id.stamp),
            Value::Int(k.uid3 as i64),
            Value::Int(k.version.major as i64),
            Value::Int(k.version.minor as i64),
            Value::Int(k.version.patch as i64),
            Value::Text(&k.name),
            Value::Blob(sha),
        ],
    )?;
    Ok(())
}

/// Drop rows for files that are no longer there.
///
/// Not for correctness — a row nobody asks about costs nothing to be wrong — but so a card carried
/// between phones for a year does not leave a table of names that mean nothing. Called after a full
/// scan, when `keep` is exactly what was found.
pub fn forget_missing<S: Sql>(db: &mut Db<'_, S>, keep: &[String]) -> symbian::Result<usize> {
    let mut gone: Vec<String> = Vec::new();
    let _ = db.query("SELECT path FROM scanned", &[], |row| {
        if let Ok(p) = row.get_text(0) {
            if !keep.contains(&p) {
                gone.push(p);
            }
        }
        Ok(())
    });
    let n = gone.len();
    for p in &gone {
        db.execute_with("DELETE FROM scanned WHERE path = ?1", &[Value::Text(p)])?;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian::sql::{Cell, MemSql};

    fn stat(size: u64, second: i32) -> Stat {
        Stat { size, year: 2026, month: 8, day: 26, hour: 18, minute: 4, second, attributes: 0, is_dir: false }
    }

    fn known() -> Known {
        Known {
            uid3: 0xE0AA_0000,
            version: Version::new(0, 2, 0),
            name: String::from("launcher"),
            sha256: Some([7u8; 32]),
        }
    }

    fn db(sql: &mut MemSql) -> Db<'_, MemSql> {
        Db::open(sql, &symbian::fs::Utf16Path::new("C:\\x.db").unwrap()).unwrap()
    }

    /// What a host test of this module can and cannot say.
    ///
    /// `MemSql` records calls and replays programmed rows; it does not parse SQL. So these check
    /// **control flow and the parameters bound** — that the key is three values and goes into the
    /// query rather than being compared afterwards, that a missing digest is not written as zeroes.
    /// Whether the SQL itself is right is a question only the handset answers.
    #[test]
    fn the_key_is_bound_into_the_query_and_not_checked_afterwards() {
        // The whole safety of this cache. A `SELECT` by path alone, filtered in Rust, would return
        // a stale row to any caller who forgot to compare — and forgetting is what callers do.
        let mut sql = MemSql::new();
        {
            let mut d = db(&mut sql);
            let _ = get(&mut d, "C:\\a\\x.sis", FileId::from_stat(&stat(1000, 5)));
        }
        let bound = sql.binds();
        assert_eq!(bound.len(), 3, "path, size and stamp, all three");
        assert!(matches!(&bound[0].1, Cell::Text(t) if t == "C:\\a\\x.sis"));
        assert!(matches!(bound[1].1, Cell::Int(1000)));
        assert!(matches!(bound[2].1, Cell::Int(20260826180405)));
    }

    #[test]
    fn a_digest_nobody_needed_is_bound_as_nothing_rather_than_as_zeroes() {
        // Hashing is conditional, so most rows have no digest. Binding `[0u8; 32]` would make the
        // cache answer "these bytes are all zero", which is a claim rather than an absence.
        let mut sql = MemSql::new();
        {
            let mut d = db(&mut sql);
            let k = Known { sha256: None, ..known() };
            let _ = put(&mut d, "C:\\a\\y.sis", FileId::from_stat(&stat(10, 1)), &k);
        }
        // Index 8, because `MemSql` records binds from zero while SQL numbers them from one.
        let sha = sql.binds().into_iter().find(|(i, _)| *i == 8).map(|(_, c)| c);
        assert!(
            matches!(&sha, Some(Cell::Blob(b)) if b.is_empty()),
            "an empty blob, not 32 zeroes — bound {sha:?}"
        );
    }

    #[test]
    fn a_rebuild_changes_the_key_even_within_the_same_minute() {
        // The developer's afternoon: same name, same place, same minute, different bytes. Both a
        // size change and a second change have to move the key, because either can be the only one
        // that differs — two builds of one version can land on the same byte count.
        let base = FileId::from_stat(&stat(1000, 5));
        assert_ne!(base, FileId::from_stat(&stat(1001, 5)), "size moved");
        assert_ne!(base, FileId::from_stat(&stat(1000, 6)), "the second moved");
        assert_eq!(base, FileId::from_stat(&stat(1000, 5)), "and nothing moved");
    }

    #[test]
    fn the_stamp_orders_the_way_a_clock_does() {
        // It is packed as one integer so it compares in SQL, and a packing that did not increase
        // with time would make "is this newer" answerable wrongly. Not used for that today; asserted
        // so it stays true if it ever is.
        let a = FileId::from_stat(&stat(1, 59)).stamp;
        let mut later = stat(1, 0);
        later.minute = 5;
        assert!(FileId::from_stat(&later).stamp > a, "a later minute is a bigger stamp");
    }

    #[test]
    fn creating_the_table_is_safe_to_repeat() {
        // Called on every open, so `IF NOT EXISTS` is the whole contract.
        let mut sql = MemSql::new();
        {
            let mut d = db(&mut sql);
            ensure(&mut d).unwrap();
        }
        assert!(sql.statements().iter().any(|s| s.contains("IF NOT EXISTS")));
    }
}
