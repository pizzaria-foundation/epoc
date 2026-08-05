# symbian

Safe wrappers over the shim: owned handles that close themselves, `Result` instead of
negative integers, and the retry loops that partial reads and writes need.

[`symbian-sys`](../symbian-sys) is the raw ABI. This is the layer an app should actually
use.

## Testable without a phone

Every module here is written against a trait, with the shim as one implementation and an
in-memory fake as another. That is not architecture for its own sake.

The interesting bugs in file I/O are in the **loops**, not in the syscalls:

- `RFile::Read` is allowed to return less than you asked for — it does so at buffer
  boundaries inside the file server. Treating one call as a whole-file read gives you a
  truncated session store that parses correctly and is wrong.
- A write may take less than offered, and a loop that makes no progress and sees no error
  spins forever.
- An atomic replace is three operations, and the failure window between them is the whole
  point of the design.

All three are pure logic. Behind `trait Fs`, `ShimFs` is four syscalls and `MemFs` (in
the tests) is a `Vec` — and `MemFs` deliberately chops reads into 4-byte pieces and
writes into 3-byte pieces, because a fake that always transfers everything would pass a
broken loop.

## `fs`

```rust
let mut fs = ShimFs;
let dir  = fs::private_path(&mut fs)?;
let path = Utf16Path::join(dir.as_units(), "session.bin")?;

fs::write_atomic(&mut fs, &path, &bytes)?;

match fs::read(&mut fs, &path)? {
    Some(bytes) => restore(&bytes),
    None => first_run(),          // the file does not exist yet — not an error
}
```

### The data cage

Everything an app writes belongs under `private_path()` — `C:\private\<UID3>\`. That is
the one location an unsigned app can write to with **no capability at all**. Anywhere
else needs `WriteUserData` or more, and a capability an unsigned package declares is a
capability a stock phone will refuse to install.

`C:` specifically, not the drive the binary was installed to: a memory card can be
removed with the app's data on it.

### Why `write_atomic` exists

It writes `<path>.tmp`, closes it, then renames over the target. A battery pull at any
point leaves either the old contents or the new ones, never a mixture.

That matters more than it sounds for a session store. Of the three possible outcomes —
old data, new data, half-written data — the third is by far the worst: the file exists,
it parses as far as it goes, and every symptom afterwards points somewhere else. Losing
an update is recoverable; a half-written store sends you debugging the wrong subsystem
for a week.

The temp file is created *next to* the target rather than in a temp directory, because a
rename is only atomic within one filesystem — across drives it quietly degrades into
copy-then-delete.

`shim_file_rename` deletes the destination first, since `RFs::Rename` refuses to
overwrite. That opens a window where neither name holds the new data, but the *old* file
is intact until the rename lands, so a crash in the window loses the update rather than
corrupting it.

### Handles are scarce

The shim has eight file slots. A leaked handle is not a slow drip — it is the ninth open
failing with `Error::InUse`. So `File` closes on `Drop` rather than relying on every
early return to remember, and there is a test that opens and drops twenty files and
asserts every slot came back.

### Paths

Symbian paths are UTF-16 and capped at 256 characters (`TFileName`), so `Utf16Path` is a
fixed buffer — a heap allocation per path would buy nothing. Conversion goes through
`str::encode_utf16` rather than casting bytes, which is what makes a Cyrillic or accented
filename work; there is a test for that.

`Utf16Path::join` inserts a separator only if the directory does not already end in one.
`RFs::PrivatePath` returns its path *with* a trailing backslash and a hand-typed
directory will not have one, and a doubled separator is rejected by the file server rather
than normalised.

## `error`

Symbian returns negative integers from a flat, platform-wide list. The ones that carry a
decision get names; everything else stays `Error::Platform(code)` with the number intact.

Keeping the number matters more than it looks. On a device with no debugger and no log, a
value you can look up in `e32err.h` is often the entire diagnosis — so an unrecognised
code has to survive to the surface rather than being flattened into a generic failure.

`Error::is_missing()` covers both `KErrNotFound` and `KErrPathNotFound`, because "this
does not exist yet" is the ordinary first-run path for a settings or session file and
callers branch on it constantly.

## Coming

`net`, over the shim's TCP functions, once those exist on the C++ side. Same shape: a
trait so the retry and framing logic is host-testable, with the sockets behind it.
