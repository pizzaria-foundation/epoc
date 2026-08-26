//! Files: the data cage, whole-file read and write, and atomic replace.
//!
//! # Why a trait
//!
//! [`Fs`] exists so the loops in this module can be tested on the host. A partial
//! read, a partial write and a three-step atomic replace are where file code actually
//! goes wrong, and all three are pure logic sitting on top of four syscalls. Behind
//! the trait, [`ShimFs`] is those syscalls and `MemFs` (in the tests) is a `Vec`.
//!
//! # Paths
//!
//! Symbian paths are UTF-16 and the shim takes `(*const u16, len)`. Rust holds UTF-8,
//! so anything crossing gets converted here. `Utf16Path` does that into a fixed
//! buffer: `TFileName` is capped at 256 characters on this platform anyway, so a
//! heap allocation per path would buy nothing.
//!
//! Everything an app writes belongs under [`private_path`] — `C:\private\<UID3>\`.
//! That is the one location an unsigned app can write to with no capability at all.
//! Anywhere else needs `WriteUserData` or more, and a capability an unsigned package
//! declares is a capability a stock phone will refuse to install.

use alloc::vec::Vec;

use symbian_sys as sys;

use crate::error::{Error, Result};

/// Symbian's `TFileName` limit. A path longer than this cannot be represented on the
/// platform at all, so refusing it here is the same answer the file server would give,
/// just sooner.
pub const MAX_PATH: usize = 256;

/// A UTF-16 path in a fixed buffer, ready to hand to the shim.
///
/// `Debug` prints the path as text rather than as 256 integers — on a platform where
/// `RDebug::Print` is the only channel, a dump nobody can read is a dump nobody
/// looks at.
///
/// `Clone` and not `Copy`: it is 516 bytes, and a copy that costs that much should be
/// visible at the call site rather than happening wherever one is passed by value.
#[derive(Clone)]
pub struct Utf16Path {
    buf: [u16; MAX_PATH],
    len: usize,
}

impl core::fmt::Debug for Utf16Path {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("\"")?;
        for c in char::decode_utf16(self.as_units().iter().copied()) {
            // A lone surrogate cannot appear in a path built from &str, but a path
            // read back from the file server is not ours to vouch for.
            f.write_fmt(format_args!("{}", c.unwrap_or(char::REPLACEMENT_CHARACTER)))?;
        }
        f.write_str("\"")
    }
}

impl Utf16Path {
    pub fn new(s: &str) -> Result<Self> {
        let mut p = Utf16Path { buf: [0; MAX_PATH], len: 0 };
        p.push_str(s)?;
        Ok(p)
    }

    /// Build from a directory and a file name, inserting a separator if the directory
    /// does not already end in one.
    ///
    /// `RFs::PrivatePath` returns its path *with* a trailing backslash, but a caller
    /// who typed the directory by hand will not have one — and a path with a doubled
    /// separator is rejected by the file server rather than normalised.
    pub fn join(dir: &[u16], name: &str) -> Result<Self> {
        let mut p = Utf16Path { buf: [0; MAX_PATH], len: 0 };
        p.push_units(dir)?;
        if p.len > 0 && p.buf[p.len - 1] != b'\\' as u16 {
            p.push_units(&[b'\\' as u16])?;
        }
        p.push_str(name)?;
        Ok(p)
    }

    fn push_units(&mut self, units: &[u16]) -> Result<()> {
        if self.len + units.len() > MAX_PATH {
            return Err(Error::Overflow);
        }
        self.buf[self.len..self.len + units.len()].copy_from_slice(units);
        self.len += units.len();
        Ok(())
    }

    fn push_str(&mut self, s: &str) -> Result<()> {
        for u in s.encode_utf16() {
            if self.len >= MAX_PATH {
                return Err(Error::Overflow);
            }
            self.buf[self.len] = u;
            self.len += 1;
        }
        Ok(())
    }

    pub fn as_units(&self) -> &[u16] {
        &self.buf[..self.len]
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The same path with `suffix` appended, for building a temporary name next to
    /// the real one.
    ///
    /// Next to it on purpose: an atomic replace is a rename, and a rename can only be
    /// atomic within one filesystem. A temp file in a system temp directory would
    /// cross drives and quietly degrade into copy-then-delete.
    pub fn with_suffix(&self, suffix: &str) -> Result<Self> {
        let mut p = Utf16Path { buf: self.buf, len: self.len };
        p.push_str(suffix)?;
        Ok(p)
    }
}

/// How to open a file. Read and write are exclusive; the shim maps the rest onto
/// `RFile`'s mode flags.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum OpenMode {
    /// Must exist.
    Read,
    /// Truncate or create.
    Replace,
    /// Create if absent, then seek to the end.
    Append,
}

impl OpenMode {
    fn bits(self) -> i32 {
        match self {
            OpenMode::Read => sys::SHIM_FILE_READ,
            OpenMode::Replace => sys::SHIM_FILE_WRITE | sys::SHIM_FILE_CREATE,
            OpenMode::Append => {
                sys::SHIM_FILE_WRITE | sys::SHIM_FILE_CREATE | sys::SHIM_FILE_APPEND
            }
        }
    }
}

/// One entry's metadata, as [`Fs::stat`] reports it.
///
/// The timestamp is broken-out fields rather than an epoch offset because Symbian's epoch is
/// year 0, and every caller that wanted "a date" had to rediscover that. `month` and `day` are
/// 1-based here even though `TDateTime`'s are not — the conversion happens once, in the shim.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Stat {
    pub size: u64,
    pub year: i32,
    pub month: i32,
    pub day: i32,
    pub hour: i32,
    pub minute: i32,
    pub second: i32,
    /// `KEntryAtt*` bits, as the file server reports them.
    pub attributes: i32,
    pub is_dir: bool,
}

impl Stat {
    fn from_raw(raw: &sys::ShimFileStat) -> Self {
        Stat {
            size: raw.size(),
            year: raw.year,
            month: raw.month,
            day: raw.day,
            hour: raw.hour,
            minute: raw.minute,
            second: raw.second,
            attributes: raw.attributes,
            is_dir: raw.is_dir != 0,
        }
    }
}

/// The four operations everything else here is built from.
///
/// Deliberately the raw shape — a read that may return less than asked, a write that
/// may take less than offered — rather than something already looped. The looping is
/// what needs testing, so it belongs above this line, not below it.
pub trait Fs {
    fn open(&mut self, path: &[u16], mode: OpenMode) -> Result<i32>;
    fn read(&mut self, handle: i32, buf: &mut [u8]) -> Result<usize>;
    fn write(&mut self, handle: i32, buf: &[u8]) -> Result<usize>;
    fn size(&mut self, handle: i32) -> Result<u64>;
    fn seek(&mut self, handle: i32, pos: u64) -> Result<()>;
    fn close(&mut self, handle: i32);
    /// List the file entries (not subdirectories) of `path` into `out` as NUL-separated
    /// UTF-16 units, returning how many names were written. A directory that does not
    /// exist lists as zero.
    fn list_dir(&mut self, path: &[u16], out: &mut [u16]) -> Result<usize>;
    /// Like [`Fs::list_dir`], but including subdirectories. Each directory name is written with
    /// a trailing `\`, so a caller reading the NUL-separated buffer can tell a directory from a
    /// file without a second call — for a shell that navigates rather than reads a known dir.
    fn list_entries(&mut self, path: &[u16], out: &mut [u16]) -> Result<usize>;
    /// Size, modification time and attributes of one entry. A directory path may carry its
    /// trailing `\` or not.
    fn stat(&mut self, path: &[u16]) -> Result<Stat>;
    fn delete(&mut self, path: &[u16]) -> Result<()>;
    fn rename(&mut self, from: &[u16], to: &[u16]) -> Result<()>;
    /// Create a directory. An existing one is success, so this is safe to call blind.
    ///
    /// On the trait rather than as a free function because a caller that groups its
    /// output into a subdirectory has to be testable against [`MemFs`] — and a free
    /// function reaching straight for the shim would make every such caller
    /// device-only.
    fn mkdir(&mut self, path: &[u16]) -> Result<()>;
    /// The app's private directory, created if absent.
    fn private_path(&mut self, out: &mut [u16]) -> Result<usize>;
}

/// [`Fs`] over the shim. Zero-sized: there is one file server session and the shim
/// owns it.
#[derive(Copy, Clone, Debug, Default)]
pub struct ShimFs;

impl Fs for ShimFs {
    fn open(&mut self, path: &[u16], mode: OpenMode) -> Result<i32> {
        let mut handle = 0i32;
        // SAFETY: `path` is valid for `path.len()` units and `handle` is a live local.
        let rc = unsafe {
            sys::shim_file_open(path.as_ptr(), path.len() as i32, mode.bits(), &mut handle)
        };
        Error::check(rc)?;
        Ok(handle)
    }

    fn read(&mut self, handle: i32, buf: &mut [u8]) -> Result<usize> {
        let mut got = 0i32;
        // SAFETY: `buf` is valid for `buf.len()` bytes; the shim writes at most that.
        let rc =
            unsafe { sys::shim_file_read(handle, buf.as_mut_ptr(), buf.len() as i32, &mut got) };
        Error::check(rc)?;
        Ok(got as usize)
    }

    fn write(&mut self, handle: i32, buf: &[u8]) -> Result<usize> {
        // SAFETY: `buf` is valid for `buf.len()` bytes and only read.
        let rc = unsafe { sys::shim_file_write(handle, buf.as_ptr(), buf.len() as i32) };
        Error::check(rc)?;
        // RFile::Write is all-or-nothing: it returns an error rather than a short
        // count, so a success means the whole buffer went.
        Ok(buf.len())
    }

    fn size(&mut self, handle: i32) -> Result<u64> {
        let mut out = 0i64;
        let rc = unsafe { sys::shim_file_size(handle, &mut out) };
        Error::check(rc)?;
        Ok(out as u64)
    }

    fn seek(&mut self, handle: i32, pos: u64) -> Result<()> {
        if pos > i64::MAX as u64 {
            return Err(Error::Overflow);
        }
        Error::check(unsafe { sys::shim_file_seek(handle, pos as i64) })
    }

    fn close(&mut self, handle: i32) {
        unsafe { sys::shim_file_close(handle) }
    }

    fn list_dir(&mut self, path: &[u16], out: &mut [u16]) -> Result<usize> {
        let mut count = 0i32;
        // SAFETY: `path`/`out` are valid for their lengths; the shim writes at most
        // `out.len()` units and reports the entry count through `count`, a live local.
        let rc = unsafe {
            sys::shim_dir_list(path.as_ptr(), path.len() as i32, out.as_mut_ptr(), out.len() as i32, &mut count)
        };
        Error::check(rc)?;
        Ok(count.max(0) as usize)
    }

    fn list_entries(&mut self, path: &[u16], out: &mut [u16]) -> Result<usize> {
        let mut count = 0i32;
        // SAFETY: as `list_dir` — pointers valid for their lengths, count is a live local.
        let rc = unsafe {
            sys::shim_dir_list_all(path.as_ptr(), path.len() as i32, out.as_mut_ptr(), out.len() as i32, &mut count)
        };
        Error::check(rc)?;
        Ok(count.max(0) as usize)
    }

    fn stat(&mut self, path: &[u16]) -> Result<Stat> {
        let mut st = sys::ShimFileStat::default();
        // SAFETY: `path` is valid for its length; the shim writes `st` once and does not keep it.
        let rc = unsafe { sys::shim_file_stat(path.as_ptr(), path.len() as i32, &mut st) };
        Error::check(rc)?;
        Ok(Stat::from_raw(&st))
    }

    fn delete(&mut self, path: &[u16]) -> Result<()> {
        Error::check(unsafe { sys::shim_file_delete(path.as_ptr(), path.len() as i32) })
    }

    fn rename(&mut self, from: &[u16], to: &[u16]) -> Result<()> {
        Error::check(unsafe {
            sys::shim_file_rename(
                from.as_ptr(),
                from.len() as i32,
                to.as_ptr(),
                to.len() as i32,
            )
        })
    }

    fn mkdir(&mut self, path: &[u16]) -> Result<()> {
        // SAFETY: `path` is valid for `path.len()` units and only read.
        Error::check(unsafe { sys::shim_mkdir(path.as_ptr(), path.len() as i32) })
    }

    fn private_path(&mut self, out: &mut [u16]) -> Result<usize> {
        let mut len = 0i32;
        let rc =
            unsafe { sys::shim_private_path(out.as_mut_ptr(), out.len() as i32, &mut len) };
        Error::check(rc)?;
        Ok(len as usize)
    }
}

/// An open file that closes itself.
///
/// Handles are the shim's scarce resource — eight slots — so a leak is not a slow
/// drip, it is the ninth open failing with [`Error::InUse`]. `Drop` is what keeps that
/// from depending on every early return remembering to clean up.
pub struct File<'a, F: Fs> {
    fs: &'a mut F,
    handle: i32,
}

impl<'a, F: Fs> File<'a, F> {
    pub fn open(fs: &'a mut F, path: &Utf16Path, mode: OpenMode) -> Result<Self> {
        let handle = fs.open(path.as_units(), mode)?;
        Ok(File { fs, handle })
    }

    pub fn size(&mut self) -> Result<u64> {
        self.fs.size(self.handle)
    }

    pub fn seek(&mut self, pos: u64) -> Result<()> {
        self.fs.seek(self.handle, pos)
    }

    /// Read into `buf` until it is full or the file ends. Returns how much was read.
    ///
    /// The loop is the point. `RFile::Read` is allowed to return less than asked for —
    /// it does so at buffer boundaries inside the file server — so a single call is
    /// not a whole-file read, and treating it as one gives you a truncated session
    /// store that parses correctly and is wrong.
    pub fn read_fully(&mut self, buf: &mut [u8]) -> Result<usize> {
        let mut done = 0;
        while done < buf.len() {
            let n = self.fs.read(self.handle, &mut buf[done..])?;
            if n == 0 {
                break; // end of file
            }
            done += n;
        }
        Ok(done)
    }

    /// Write the whole buffer, looping over partial writes.
    pub fn write_all(&mut self, buf: &[u8]) -> Result<()> {
        let mut done = 0;
        while done < buf.len() {
            let n = self.fs.write(self.handle, &buf[done..])?;
            if n == 0 {
                // No progress and no error: retrying would spin forever.
                return Err(Error::Platform(sys::SHIM_ERR_GENERAL));
            }
            done += n;
        }
        Ok(())
    }
}

impl<F: Fs> Drop for File<'_, F> {
    fn drop(&mut self) {
        self.fs.close(self.handle);
    }
}

/// The app's private directory as a UTF-16 path.
pub fn private_path<F: Fs>(fs: &mut F) -> Result<Utf16Path> {
    let mut p = Utf16Path { buf: [0; MAX_PATH], len: 0 };
    let n = fs.private_path(&mut p.buf)?;
    if n > MAX_PATH {
        return Err(Error::Overflow);
    }
    p.len = n;
    Ok(p)
}

/// Read a whole file. `Ok(None)` when it does not exist, which for a settings or
/// session file is the first run rather than a failure.
pub fn read<F: Fs>(fs: &mut F, path: &Utf16Path) -> Result<Option<Vec<u8>>> {
    let mut f = match File::open(fs, path, OpenMode::Read) {
        Ok(f) => f,
        Err(e) if e.is_missing() => return Ok(None),
        Err(e) => return Err(e),
    };
    let size = f.size()? as usize;
    let mut buf = alloc::vec![0u8; size];
    let got = f.read_fully(&mut buf)?;
    // Truncate rather than trusting Size(): another process could have shortened the
    // file between the two calls, and a buffer with a tail of zeros is worse than a
    // short one because it looks like data.
    buf.truncate(got);
    Ok(Some(buf))
}

/// Replace a file's contents atomically.
///
/// Writes `<path>.tmp`, closes it, then renames over the target. A battery pull at
/// any point leaves either the old contents or the new ones, never a mixture — which
/// is the whole reason this is not just "open for write and dump the bytes". A
/// half-written session store is the worst of the three outcomes: it exists, it
/// parses as far as it goes, and every symptom afterwards points somewhere else.
pub fn write_atomic<F: Fs>(fs: &mut F, path: &Utf16Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_suffix(".tmp")?;
    {
        let mut f = File::open(fs, &tmp, OpenMode::Replace)?;
        f.write_all(data)?;
        // The File must be dropped — and so the handle closed — before the rename.
        // Renaming a file that is still open fails with KErrInUse on this platform,
        // and the shim opens with EFileShareExclusive precisely so that it does.
    }
    fs.rename(tmp.as_units(), path.as_units())
}

/// The chunk [`copy`] moves at a time. Small on purpose: the caller is a boot daemon with a modest
/// heap copying a package that can be a third of a megabyte, and a copy that has to allocate the
/// whole file is a copy that fails on the day it is needed most.
pub const COPY_CHUNK: usize = 8 * 1024;

/// Copy `from` to `to`, atomically at the destination and without holding the file in memory.
///
/// Same temp-and-rename as [`write_atomic`] and for the same reason, one step further: a package
/// being promoted to "the version we can go back to" must never exist as a half-copy. Either the
/// whole known-good `.sis` is there or the previous one still is; a truncated rollback package is a
/// rollback that fails at the moment the phone has nothing else left.
///
/// [`Error::NotFound`] if the source is not there.
pub fn copy<F: Fs>(fs: &mut F, from: &Utf16Path, to: &Utf16Path) -> Result<u64> {
    let tmp = to.with_suffix(".tmp")?;
    // Raw handles rather than two [`File`] values: `File` borrows the filesystem for as long as it
    // lives, and a copy needs both ends open at once. The cost is that the closes are ours to get
    // right, which is what the early-return dance below is.
    let src = fs.open(from.as_units(), OpenMode::Read)?;
    let dst = match fs.open(tmp.as_units(), OpenMode::Replace) {
        Ok(h) => h,
        Err(e) => {
            fs.close(src);
            return Err(e);
        }
    };

    let mut buf = alloc::vec![0u8; COPY_CHUNK];
    let mut total: u64 = 0;
    let result = (|| -> Result<u64> {
        loop {
            let got = fs.read(src, &mut buf)?;
            if got == 0 {
                return Ok(total);
            }
            let mut sent = 0;
            while sent < got {
                let n = fs.write(dst, &buf[sent..got])?;
                if n == 0 {
                    return Err(Error::UnexpectedEof);
                }
                sent += n;
            }
            total += got as u64;
        }
    })();
    fs.close(src);
    fs.close(dst);
    let total = result?;
    fs.rename(tmp.as_units(), to.as_units())?;
    Ok(total)
}

/// Append `data`, starting the file over first if it has passed `cap` bytes.
///
/// The primitive under [`crate::applog`] and `symbian::log`. A log that grows without bound
/// on a phone eventually becomes the problem it was meant to diagnose, and the two obvious
/// alternatives are both worse than a restart: dropping the oldest half costs a full read
/// and rewrite on the GUI thread, and stopping at the cap produces a log that documents
/// everything except the bug.
///
/// The size is checked *before* appending rather than after, so `cap` is a ceiling rather
/// than a ceiling plus one line.
pub fn append_capped<F: Fs>(fs: &mut F, path: &Utf16Path, data: &[u8], cap: u64) -> Result<()> {
    let too_big = match File::open(fs, path, OpenMode::Append) {
        Ok(mut f) => f.size().unwrap_or(0) >= cap,
        // Not there yet, or not openable at all: let the append below be the real test, so
        // a caller gets one error rather than two different ones for the same problem.
        Err(_) => false,
    };
    if too_big {
        write_atomic(fs, path, b"")?;
    }

    let mut f = File::open(fs, path, OpenMode::Append)?;
    f.write_all(data)
}

// ------------------------------------------------------------------- testing --

/// An in-memory [`Fs`], with the two behaviours that make real file code go wrong: reads
/// are chopped into small pieces, and writes can be partial.
///
/// Public, and not behind `#[cfg(test)]`, because the crates above this one need it too —
/// `crate::cache` is file logic worth testing and there is no phone in a
/// `cargo test`. Same reasoning as [`crate::image::MemImages`]. It costs nothing in a device
/// build: nothing references it, and `--gc-sections` sweeps it.
pub struct MemFs {
    pub files: Vec<(Vec<u16>, Vec<u8>)>,
    pub open: Vec<Option<(usize, usize)>>, // (file index, position)
    /// Cap on a single read. 0 means unlimited.
    pub read_chunk: usize,
    /// Cap on a single write. 0 means unlimited.
    pub write_chunk: usize,
    pub private: Vec<u16>,
    /// Every path passed to [`Fs::mkdir`], in order. The fake creates no directories —
    /// see the impl — so this is the only record that a caller asked.
    pub mkdirs: Vec<Vec<u16>>,
}

impl Default for MemFs {
    fn default() -> Self {
        Self::new()
    }
}

impl MemFs {
    pub fn new() -> Self {
        MemFs {
            files: Vec::new(),
            open: Vec::new(),
            read_chunk: 0,
            write_chunk: 0,
            private: "C:\\private\\E1234569\\".encode_utf16().collect(),
            mkdirs: Vec::new(),
        }
    }

    fn find(&self, path: &[u16]) -> Option<usize> {
        self.files.iter().position(|(p, _)| p == path)
    }

    pub fn contents(&self, path: &str) -> Option<&[u8]> {
        let key: Vec<u16> = path.encode_utf16().collect();
        self.find(&key).map(|i| self.files[i].1.as_slice())
    }
}

impl Fs for MemFs {
    fn open(&mut self, path: &[u16], mode: OpenMode) -> Result<i32> {
        let idx = match (self.find(path), mode) {
            (Some(i), OpenMode::Replace) => {
                self.files[i].1.clear();
                i
            }
            (Some(i), _) => i,
            (None, OpenMode::Read) => return Err(Error::NotFound),
            (None, _) => {
                self.files.push((path.to_vec(), Vec::new()));
                self.files.len() - 1
            }
        };
        let pos = if mode == OpenMode::Append { self.files[idx].1.len() } else { 0 };
        self.open.push(Some((idx, pos)));
        Ok(self.open.len() as i32) // 1-based, so 0 stays "no handle"
    }

    fn read(&mut self, handle: i32, buf: &mut [u8]) -> Result<usize> {
        let slot = self.open.get((handle - 1) as usize).copied().flatten();
        let (idx, pos) = slot.ok_or(Error::Platform(sys::SHIM_ERR_BAD_HANDLE))?;
        let data = &self.files[idx].1;
        let mut want = buf.len().min(data.len().saturating_sub(pos));
        if self.read_chunk > 0 {
            want = want.min(self.read_chunk);
        }
        buf[..want].copy_from_slice(&data[pos..pos + want]);
        self.open[(handle - 1) as usize] = Some((idx, pos + want));
        Ok(want)
    }

    fn write(&mut self, handle: i32, buf: &[u8]) -> Result<usize> {
        let slot = self.open.get((handle - 1) as usize).copied().flatten();
        let (idx, pos) = slot.ok_or(Error::Platform(sys::SHIM_ERR_BAD_HANDLE))?;
        let n = if self.write_chunk > 0 { buf.len().min(self.write_chunk) } else { buf.len() };
        let data = &mut self.files[idx].1;
        if data.len() < pos {
            data.resize(pos, 0);
        }
        data.truncate(pos);
        data.extend_from_slice(&buf[..n]);
        self.open[(handle - 1) as usize] = Some((idx, pos + n));
        Ok(n)
    }

    fn size(&mut self, handle: i32) -> Result<u64> {
        let slot = self.open.get((handle - 1) as usize).copied().flatten();
        let (idx, _) = slot.ok_or(Error::Platform(sys::SHIM_ERR_BAD_HANDLE))?;
        Ok(self.files[idx].1.len() as u64)
    }

    fn seek(&mut self, handle: i32, pos: u64) -> Result<()> {
        let slot = self.open.get((handle - 1) as usize).copied().flatten();
        let (idx, _) = slot.ok_or(Error::Platform(sys::SHIM_ERR_BAD_HANDLE))?;
        self.open[(handle - 1) as usize] = Some((idx, pos as usize));
        Ok(())
    }

    fn close(&mut self, handle: i32) {
        if let Some(s) = self.open.get_mut((handle - 1) as usize) {
            *s = None;
        }
    }

    fn list_dir(&mut self, path: &[u16], out: &mut [u16]) -> Result<usize> {
        // Immediate file children of `path`: a key that starts with the dir and whose
        // remainder holds no further separator. Names are packed NUL-separated.
        let sep = b'\\' as u16;
        let mut pos = 0usize;
        let mut n = 0usize;
        for (key, _data) in &self.files {
            if key.len() <= path.len() || key[..path.len()] != *path {
                continue;
            }
            let name = &key[path.len()..];
            if name.contains(&sep) {
                continue; // in a subdirectory, not an immediate child
            }
            if pos + name.len() + 1 > out.len() {
                break;
            }
            out[pos..pos + name.len()].copy_from_slice(name);
            pos += name.len();
            out[pos] = 0;
            pos += 1;
            n += 1;
        }
        Ok(n)
    }

    fn list_entries(&mut self, path: &[u16], out: &mut [u16]) -> Result<usize> {
        // Immediate children of `path`: files as-is, subdirectories as their first component
        // with a trailing `\`, deduplicated — the same shape the device returns.
        let sep = b'\\' as u16;
        let mut names: Vec<Vec<u16>> = Vec::new();
        for (key, _data) in &self.files {
            if key.len() <= path.len() || key[..path.len()] != *path {
                continue;
            }
            let name = &key[path.len()..];
            let entry: Vec<u16> = match name.iter().position(|&u| u == sep) {
                // A file directly in this directory.
                None => name.to_vec(),
                // Something in a subdirectory: the subdirectory itself, with a trailing sep.
                Some(i) => {
                    let mut d = name[..i].to_vec();
                    d.push(sep);
                    d
                }
            };
            if !names.contains(&entry) {
                names.push(entry);
            }
        }
        let mut pos = 0usize;
        let mut n = 0usize;
        for name in &names {
            if pos + name.len() + 1 > out.len() {
                break;
            }
            out[pos..pos + name.len()].copy_from_slice(name);
            pos += name.len();
            out[pos] = 0;
            pos += 1;
            n += 1;
        }
        Ok(n)
    }

    fn stat(&mut self, path: &[u16]) -> Result<Stat> {
        // The fake has no clock and no attributes: it answers the one field it genuinely
        // knows, so a test can assert on size without pretending to a modification time.
        match self.find(path) {
            Some(i) => Ok(Stat { size: self.files[i].1.len() as u64, ..Stat::default() }),
            None => Err(Error::NotFound),
        }
    }

    fn delete(&mut self, path: &[u16]) -> Result<()> {
        match self.find(path) {
            Some(i) => {
                self.files.remove(i);
                // Indices shift, so anything still open past the removed file
                // would now point at the wrong one. Tests never do that, and
                // panicking here would be better than silently corrupting.
                Ok(())
            }
            None => Err(Error::NotFound),
        }
    }

    fn rename(&mut self, from: &[u16], to: &[u16]) -> Result<()> {
        // Removing the destination shifts the indices, so `from` is located again
        // afterwards rather than before — the same ordering the shim uses, and for
        // the same reason.
        self.find(from).ok_or(Error::NotFound)?;
        if let Some(dst) = self.find(to) {
            self.files.remove(dst);
        }
        let src = self.find(from).ok_or(Error::NotFound)?;
        self.files[src].0 = to.to_vec();
        Ok(())
    }

    fn mkdir(&mut self, path: &[u16]) -> Result<()> {
        // The store is a flat map from full path to contents — there is no directory to
        // create, and a file may be written under any prefix. Recording the call would
        // model a rule this fake does not enforce, which is worse than not modelling it:
        // a test could then pass on a directory the device would have refused.
        //
        // `mkdirs` is kept so a test that cares can assert the call was made.
        self.mkdirs.push(path.to_vec());
        Ok(())
    }

    fn private_path(&mut self, out: &mut [u16]) -> Result<usize> {
        if self.private.len() > out.len() {
            return Err(Error::Overflow);
        }
        out[..self.private.len()].copy_from_slice(&self.private);
        Ok(self.private.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;
    use alloc::vec;


    fn utf8(units: &[u16]) -> String {
        char::decode_utf16(units.iter().copied()).map(|c| c.unwrap()).collect()
    }

    #[test]
    fn join_inserts_one_separator_and_only_one() {
        let dir: Vec<u16> = "C:\\private\\E1234569\\".encode_utf16().collect();
        let p = Utf16Path::join(&dir, "session.bin").unwrap();
        assert_eq!(utf8(p.as_units()), "C:\\private\\E1234569\\session.bin");

        // Without the trailing separator the join must add one. A doubled separator
        // is rejected by the file server rather than normalised, so getting this
        // wrong fails at runtime only.
        let dir2: Vec<u16> = "C:\\data".encode_utf16().collect();
        let p2 = Utf16Path::join(&dir2, "x.txt").unwrap();
        assert_eq!(utf8(p2.as_units()), "C:\\data\\x.txt");
    }

    #[test]
    fn paths_refuse_to_overflow_tfilename() {
        let long = "a".repeat(MAX_PATH + 1);
        assert_eq!(Utf16Path::new(&long).unwrap_err(), Error::Overflow);

        let dir: Vec<u16> = "C:\\".encode_utf16().collect();
        let name = "b".repeat(MAX_PATH);
        assert_eq!(Utf16Path::join(&dir, &name).unwrap_err(), Error::Overflow);
    }

    #[test]
    fn non_ascii_paths_survive_the_round_trip() {
        // Symbian paths are UTF-16, and a Cyrillic or accented filename is entirely
        // legal. Encoding through `encode_utf16` rather than casting bytes is what
        // makes that work.
        let p = Utf16Path::new("C:\\мой файл ção.txt").unwrap();
        assert_eq!(utf8(p.as_units()), "C:\\мой файл ção.txt");
    }

    #[test]
    fn read_fully_reassembles_a_file_delivered_in_pieces() {
        let mut fs = MemFs::new();
        // Four bytes at a time, which is what makes a single read() insufficient.
        fs.read_chunk = 4;
        let path = Utf16Path::new("C:\\a.bin").unwrap();
        let data: Vec<u8> = (0..37u8).collect();
        {
            let mut f = File::open(&mut fs, &path, OpenMode::Replace).unwrap();
            f.write_all(&data).unwrap();
        }
        let got = read(&mut fs, &path).unwrap().unwrap();
        assert_eq!(got, data, "a chunked read must still produce the whole file");
    }

    #[test]
    fn write_all_finishes_when_writes_are_partial() {
        let mut fs = MemFs::new();
        fs.write_chunk = 3;
        let path = Utf16Path::new("C:\\b.bin").unwrap();
        let data: Vec<u8> = (0..50u8).collect();
        {
            let mut f = File::open(&mut fs, &path, OpenMode::Replace).unwrap();
            f.write_all(&data).unwrap();
        }
        assert_eq!(fs.contents("C:\\b.bin").unwrap(), &data[..]);
    }

    #[test]
    fn reading_a_missing_file_is_none_not_an_error() {
        // The first-run path. A settings file that does not exist yet is not a fault,
        // and forcing every caller to match on NotFound invites forgetting to.
        let mut fs = MemFs::new();
        let path = Utf16Path::new("C:\\nope.bin").unwrap();
        assert!(read(&mut fs, &path).unwrap().is_none());
    }

    #[test]
    fn copy_moves_a_file_larger_than_one_chunk_byte_for_byte() {
        let mut fs_ = MemFs::new();
        let src = Utf16Path::new("C:\\a.sis").unwrap();
        let dst = Utf16Path::new("C:\\b.sis").unwrap();
        // Deliberately not a multiple of the chunk: the last partial read is where a copy that
        // trusts `read` to fill the buffer writes the previous chunk's tail a second time.
        let data: Vec<u8> = (0..COPY_CHUNK * 2 + 37).map(|i| (i % 251) as u8).collect();
        write_atomic(&mut fs_, &src, &data).unwrap();

        assert_eq!(copy(&mut fs_, &src, &dst).unwrap(), data.len() as u64);
        assert_eq!(read(&mut fs_, &dst).unwrap().unwrap(), data);
        assert_eq!(read(&mut fs_, &src).unwrap().unwrap(), data, "the source is untouched");
    }

    #[test]
    fn copying_a_file_that_is_not_there_fails_and_leaves_the_target_alone() {
        let mut fs_ = MemFs::new();
        let dst = Utf16Path::new("C:\\b.sis").unwrap();
        write_atomic(&mut fs_, &dst, b"the known-good package").unwrap();
        assert!(copy(&mut fs_, &Utf16Path::new("C:\\gone.sis").unwrap(), &dst).is_err());
        assert_eq!(
            read(&mut fs_, &dst).unwrap().unwrap(),
            b"the known-good package",
            "a failed copy must never be the reason a rollback has nothing to install"
        );
    }

    #[test]
    fn write_atomic_leaves_no_temp_file_behind() {
        let mut fs = MemFs::new();
        let path = Utf16Path::new("C:\\s.bin").unwrap();
        write_atomic(&mut fs, &path, b"hello").unwrap();
        assert_eq!(fs.contents("C:\\s.bin").unwrap(), b"hello");
        assert!(fs.contents("C:\\s.bin.tmp").is_none(), "temp file was not renamed away");
    }

    #[test]
    fn write_atomic_replaces_existing_contents_entirely() {
        let mut fs = MemFs::new();
        let path = Utf16Path::new("C:\\s.bin").unwrap();
        write_atomic(&mut fs, &path, b"a longer original value").unwrap();
        write_atomic(&mut fs, &path, b"short").unwrap();
        // Not "starts with": a replace that left the tail of the old value would
        // produce a file that parses and is wrong, which is the exact failure this
        // function exists to prevent.
        assert_eq!(fs.contents("C:\\s.bin").unwrap(), b"short");
    }

    #[test]
    fn the_target_is_untouched_until_the_rename() {
        // The safety property: if the write fails, the old contents are still there.
        // Modelled by writing the temp file and then not renaming.
        let mut fs = MemFs::new();
        let path = Utf16Path::new("C:\\s.bin").unwrap();
        write_atomic(&mut fs, &path, b"original").unwrap();

        let tmp = path.with_suffix(".tmp").unwrap();
        {
            let mut f = File::open(&mut fs, &tmp, OpenMode::Replace).unwrap();
            f.write_all(b"interrupted").unwrap();
        }
        assert_eq!(
            fs.contents("C:\\s.bin").unwrap(),
            b"original",
            "the real file must not change before the rename lands"
        );
    }

    #[test]
    fn append_adds_rather_than_truncating() {
        let mut fs = MemFs::new();
        let path = Utf16Path::new("C:\\log.txt").unwrap();
        {
            let mut f = File::open(&mut fs, &path, OpenMode::Replace).unwrap();
            f.write_all(b"one\n").unwrap();
        }
        {
            let mut f = File::open(&mut fs, &path, OpenMode::Append).unwrap();
            f.write_all(b"two\n").unwrap();
        }
        assert_eq!(fs.contents("C:\\log.txt").unwrap(), b"one\ntwo\n");
    }

    #[test]
    fn dropping_a_file_frees_its_handle() {
        // The shim has eight slots. A leaked handle is not a slow drip — it is the
        // ninth open failing — so Drop has to be what releases it, rather than every
        // early return remembering.
        let mut fs = MemFs::new();
        let path = Utf16Path::new("C:\\h.bin").unwrap();
        for _ in 0..20 {
            let f = File::open(&mut fs, &path, OpenMode::Replace).unwrap();
            drop(f);
        }
        assert!(fs.open.iter().all(|s| s.is_none()), "handles were not released");
    }

    #[test]
    fn private_path_is_usable_as_a_directory() {
        let mut fs = MemFs::new();
        let dir = private_path(&mut fs).unwrap();
        let file = Utf16Path::join(dir.as_units(), "session.bin").unwrap();
        assert_eq!(utf8(file.as_units()), "C:\\private\\E1234569\\session.bin");
    }

    #[test]
    fn a_round_trip_through_the_private_directory_works() {
        let mut fs = MemFs::new();
        let dir = private_path(&mut fs).unwrap();
        let path = Utf16Path::join(dir.as_units(), "auth.key").unwrap();
        let key = vec![0xABu8; 256];
        write_atomic(&mut fs, &path, &key).unwrap();
        assert_eq!(read(&mut fs, &path).unwrap().unwrap(), key);
    }

    /// Split a NUL-separated UTF-16 buffer back into strings, for asserting a listing.
    fn names(buf: &[u16], count: usize) -> alloc::vec::Vec<String> {
        let mut out = alloc::vec::Vec::new();
        let mut i = 0;
        for _ in 0..count {
            let start = i;
            while i < buf.len() && buf[i] != 0 {
                i += 1;
            }
            out.push(utf8(&buf[start..i]));
            i += 1;
        }
        out
    }

    #[test]
    fn list_entries_shows_files_and_subdirs_with_a_trailing_slash() {
        let mut fs = MemFs::new();
        let units = |s: &str| -> Vec<u16> { s.encode_utf16().collect() };
        // Two files directly under Z:\, and one file inside a subdirectory.
        fs.files.push((units("Z:\\readme.txt"), b"hi".to_vec()));
        fs.files.push((units("Z:\\boot.cfg"), b"x".to_vec()));
        fs.files.push((units("Z:\\system\\apps\\a.txt"), b"y".to_vec()));

        let dir = units("Z:\\");
        let mut buf = vec![0u16; 256];
        let count = fs.list_entries(&dir, &mut buf).unwrap();
        let got = names(&buf, count);

        // The two files as-is, plus the immediate subdirectory with its trailing separator —
        // and only once, though two paths pass through it here (one, in this data).
        assert!(got.contains(&String::from("readme.txt")), "{got:?}");
        assert!(got.contains(&String::from("boot.cfg")), "{got:?}");
        assert!(got.contains(&String::from("system\\")), "{got:?}");
        assert_eq!(count, 3, "{got:?}");
    }
}
