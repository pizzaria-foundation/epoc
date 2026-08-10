# symbian

Safe wrappers over the shim: owned handles that close themselves, `Result` instead of
negative integers, and the retry loops that partial reads and writes need. `no_std`,
80 tests.

Files, sockets, images, timers, randomness, a disk cache, Publish & Subscribe, and the
device log - the platform, as something an application can call without writing `unsafe`.

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

## `net`

```rust
let mut net = ShimNet;

// Prompt on the first run, then pass the saved id and connect in silence.
let mut bearer = Bearer::start(&mut net, saved_iap)?;
// ... feed events until bearer.on_event returns Ok(true)
persist(bearer.iap());

let mut sock = TcpStream::open(&mut net, &bearer, 512, 256)?;
sock.connect(&mut net, Ipv4::new(192, 168, 15, 74), 7654)?;

// From App::handle_raw, with the event passed through unchanged:
match sock.on_event(&mut net, ev) {
    Progress::Connected  => { sock.write(&mut net, b"hello")?; }
    Progress::Received(_) => { let n = sock.read(&mut net, &mut buf)?; }
    Progress::Closed | Progress::Failed(_) => { /* done */ }
    _ => {}
}
```

### What the tests cover, and why each one is there

Twenty-four of them, and every one is a bug that would otherwise be found on hardware:

- **A `RECV` completion delivers what arrived, not what was asked for.** Reading a
  length-prefixed frame accumulates across several, and each completion re-issues a read
  into the *remaining* room — asking for the whole buffer again overwrites what is already
  in it.
- **Events carry a handle.** With two sockets open, one consuming another's completion is
  a bug that cannot happen with one socket and always happens with two.
- **A close while a send is queued abandons the queue.** Reporting those bytes as sent is
  a lie the caller acts on: a protocol advances believing its request went out.
- **A full receive buffer withholds the next read.** A zero-length slice comes back from
  the shim as an argument error, not as "not now", so the read has to wait for a drain.
- **`KErrEof` and `KErrDisconnected` are the peer closing, not faults.** Treating them as
  errors makes every clean shutdown look like a failure. So does a zero-length read.
- **Connecting issues a read immediately**, or a server that speaks first has its greeting
  sitting unclaimed.

### `Bearer`, and why the access point needs a type

S60 will not silently pick a bearer, so a connection has to be started before any socket
can open. The first run passes `Iap::Prompt` and lets the OS ask; `Bearer::iap()` then
reports which access point it settled on, and persisting that through
[`crate::fs::write_atomic`] is what makes every later run silent.

The part that earns a type: a saved access point **stops working**. The network it names
is gone, or the profile was deleted. An app that kept passing the stale id would simply
stop connecting, reporting an error about the access point rather than about the
situation. `Bearer` retries with a prompt exactly once, then gives up rather than looping.

### Buffers

`TcpStream` owns its send and receive buffers as `Box<[u8]>` and closes in `Drop`.

That is not tidiness. The shim holds a descriptor over the caller's memory for the
duration of a request rather than copying, so a buffer freed or moved while a request is
outstanding gets read by the socket server after the fact. A `Box`'s contents do not move
when the `Box` does, which is what lets the stream itself be moved while the shim's
pointers stay valid.

### A lookup that resolves to nothing is an error

Not `0.0.0.0`. An AAAA-only name, or a record the shim could not read as IPv4, comes back
as `Error::NotFound` — because the alternative is an address that gets attempted and fails
later as a timeout, pointing at entirely the wrong thing.

## Not here

UDP is in the shim ABI but has no wrapper yet: `TcpStream`'s state machine does not fit a
datagram socket, and a `UdpSocket` deserves its own rather than a flag.

---

Part of [epoc](../../README.md), a Rust SDK for Symbian S60 3rd Edition. MIT licensed; see
`LICENSE` at the repository root. `symbian` in this crate's name is descriptive, not a claim
on somebody else's trademark - the repository README says more. Written with AI assistance,
and every hardware claim in it was measured rather than reasoned about.
