//! The filesystem, measured rather than assumed: the data cage, path limits, attributes,
//! and whether the atomic save this SDK relies on actually is one.
//!
//! Everything here writes only inside the app's own private directory, which needs no
//! capability and which the installer removes with the package. Nothing outside it is
//! created or deleted — the capability probe already covers *reading* elsewhere, and a
//! reconnaissance run has no business writing there.

use alloc::string::String;
use alloc::vec;

use symbian::fs::{self, Fs, OpenMode, ShimFs, Utf16Path};
use symbian_report::{push_i64, Report};

pub fn run(r: &mut Report, fs_: &mut ShimFs) {
    r.entering(fs_, "data cage");
    let dir = cage(r, fs_);
    r.flush(fs_);

    let Some(dir) = dir else {
        // Without a writable private directory nothing below can run, and saying so beats
        // a page of failures that all have the same cause.
        r.check_note("remaining checks", false, "skipped: no writable private directory");
        return;
    };

    r.entering(fs_, "read and write");
    roundtrip(r, fs_, &dir);
    r.flush(fs_);

    r.entering(fs_, "atomic save");
    atomic(r, fs_, &dir);
    r.flush(fs_);

    r.entering(fs_, "short reads");
    short_reads(r, fs_, &dir);
    r.flush(fs_);

    r.entering(fs_, "path limits");
    path_limit(r, fs_, &dir);
    r.flush(fs_);

    r.entering(fs_, "rename semantics");
    rename(r, fs_, &dir);
}

fn cage(r: &mut Report, fs_: &mut ShimFs) -> Option<Utf16Path> {
    r.head("data cage");
    match fs::private_path(fs_) {
        Ok(p) => {
            let mut s = String::new();
            for u in p.as_units() {
                if let Some(c) = char::from_u32(*u as u32) {
                    s.push(c);
                }
            }
            r.check_note("RFs::PrivatePath", true, &s);
            // The path comes back drive-relative with a trailing backslash, and the drive
            // has to be prepended by hand — C: on purpose, not the drive the binary was
            // installed to, since a memory card can be removed with the app's data on it.
            r.info("note", "drive-relative with a trailing separator; the drive is prepended by hand");
            Some(p)
        }
        Err(e) => {
            r.check_note("RFs::PrivatePath", false, &err(e));
            None
        }
    }
}

fn join(dir: &Utf16Path, name: &str) -> Option<Utf16Path> {
    Utf16Path::join(dir.as_units(), name).ok()
}

fn roundtrip(r: &mut Report, fs_: &mut ShimFs, dir: &Utf16Path) {
    r.head("read and write");
    let Some(p) = join(dir, "rt.bin") else {
        r.check("path join", false);
        return;
    };
    let data: vec::Vec<u8> = (0..4096u32).map(|i| (i * 31) as u8).collect();
    r.check_note("write 4096 bytes", fs::write_atomic(fs_, &p, &data).is_ok(), "write_atomic");
    match fs::read(fs_, &p) {
        Ok(Some(got)) => {
            r.check("read back the same length", got.len() == data.len());
            r.check("read back the same bytes", got == data);
        }
        Ok(None) => r.check_note("read back", false, "file absent immediately after writing"),
        Err(e) => r.check_note("read back", false, &err(e)),
    }
    let _ = fs_.delete(p.as_units());
}

fn atomic(r: &mut Report, fs_: &mut ShimFs, dir: &Utf16Path) {
    r.head("atomic save");
    let Some(p) = join(dir, "at.bin") else { return };
    let _ = fs::write_atomic(fs_, &p, b"first");
    let ok = fs::write_atomic(fs_, &p, b"second").is_ok();
    let got = fs::read(fs_, &p).ok().flatten();
    r.check_note("overwrite replaces rather than appends", ok && got.as_deref() == Some(b"second".as_slice()), &{
        let mut s = String::from("read back ");
        push_i64(&mut s, got.as_ref().map(|g| g.len()).unwrap_or(0) as i64);
        s.push_str(" bytes");
        s
    });
    let _ = fs_.delete(p.as_units());
}

fn short_reads(r: &mut Report, fs_: &mut ShimFs, dir: &Utf16Path) {
    r.head("short reads");
    // RFile::Read may return less than asked at buffer boundaries inside the file server,
    // and treating one call as a whole-file read gives a truncated store that parses
    // correctly and is wrong. This measures whether it happens here and at what size.
    let Some(p) = join(dir, "sr.bin") else { return };
    let data = vec![0xA5u8; 64 * 1024];
    if fs::write_atomic(fs_, &p, &data).is_err() {
        r.check_note("64 KB write", false, "could not create the sample");
        return;
    }
    match fs_.open(p.as_units(), OpenMode::Read) {
        Ok(h) => {
            let mut buf = vec![0u8; data.len()];
            let first = fs_.read(h, &mut buf);
            fs_.close(h);
            match first {
                Ok(n) => {
                    r.num("first RFile::Read returned", n as i64);
                    r.check_note(
                        "a single read is not a whole-file read",
                        true,
                        if n == data.len() {
                            "it returned everything this time — which is not a guarantee"
                        } else {
                            "it returned less than asked, as the file server is allowed to"
                        },
                    );
                }
                Err(e) => r.check_note("RFile::Read", false, &err(e)),
            }
        }
        Err(e) => r.check_note("RFile::Open", false, &err(e)),
    }
    let _ = fs_.delete(p.as_units());
}

fn path_limit(r: &mut Report, fs_: &mut ShimFs, dir: &Utf16Path) {
    r.head("path limits");
    // Grow a filename until the file server refuses it. The number is worth having: a
    // cache keyed on something user-supplied will meet this limit eventually, and finding
    // it then costs a device trip.
    let mut longest = 0usize;
    let mut refused_at = 0usize;
    let mut refused_with = 0i32;
    for len in [8usize, 32, 64, 128, 200, 240, 250, 255, 256, 300] {
        let mut name = String::new();
        for _ in 0..len {
            name.push('x');
        }
        let Some(p) = join(dir, &name) else {
            refused_at = len;
            refused_with = -6;
            break;
        };
        match fs::write_atomic(fs_, &p, b"x") {
            Ok(()) => {
                longest = len;
                let _ = fs_.delete(p.as_units());
            }
            Err(e) => {
                refused_at = len;
                refused_with = code(e);
                break;
            }
        }
    }
    r.num("longest filename accepted", longest as i64);
    if refused_at > 0 {
        let mut s = String::from("err ");
        push_i64(&mut s, refused_with as i64);
        let mut key = String::from("refused at ");
        push_i64(&mut key, refused_at as i64);
        r.info(&key, &s);
    } else {
        r.info("refused at", "never, within the sizes tried");
    }
}

fn rename(r: &mut Report, fs_: &mut ShimFs, dir: &Utf16Path) {
    r.head("rename");
    let (Some(a), Some(b)) = (join(dir, "rn-a.bin"), join(dir, "rn-b.bin")) else { return };
    let _ = fs::write_atomic(fs_, &a, b"a");
    let _ = fs::write_atomic(fs_, &b, b"b");
    // RFs::Rename refuses to overwrite, which is why an atomic replace has to delete the
    // destination first — and that opens the window where neither name holds the new data.
    let refused = fs_.rename(a.as_units(), b.as_units());
    r.check_note(
        "rename over an existing file is refused",
        matches!(refused, Err(symbian::Error::AlreadyExists)),
        &match refused {
            Err(e) => err(e),
            Ok(()) => String::from("it succeeded — the atomic-save delete-first dance may be unnecessary here"),
        },
    );
    let _ = fs_.delete(a.as_units());
    let _ = fs_.delete(b.as_units());
}

fn code(e: symbian::Error) -> i32 {
    match e {
        symbian::Error::Platform(c) => c,
        symbian::Error::NotFound => -1,
        symbian::Error::PathNotFound => -12,
        symbian::Error::AlreadyExists => -11,
        symbian::Error::AccessDenied => -46,
        symbian::Error::Overflow => -9,
        symbian::Error::Argument => -6,
        symbian::Error::NotReady => -18,
        _ => -2,
    }
}

fn err(e: symbian::Error) -> String {
    let mut s = String::from("err ");
    push_i64(&mut s, code(e) as i64);
    s
}
